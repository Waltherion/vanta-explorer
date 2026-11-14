//! One swapchain per Wayland surface with bounded acquire/fence waits and an own render pass.

use anyhow::Result;
use ash::vk;

use crate::gpu::{is_scrgb, Gpu};
use crate::util::{create_render_pass, mip_range};
use crate::GPU_WAIT_NS;

pub const FRAMES_IN_FLIGHT: usize = 2;

/// A frame in progress: the command buffer is begun and the render pass is NOT (the caller
/// may record other passes first). Finish with [`SurfaceChain::end_frame`].
pub struct Frame {
    pub cmd: vk::CommandBuffer,
    pub image_index: u32,
    pub slot: usize,
}

pub struct SurfaceChain {
    pub tag: String,
    pub surface: vk::SurfaceKHR,
    pub format: vk::SurfaceFormatKHR,
    pub hdr: bool,
    pub render_pass: vk::RenderPass,
    pub swapchain: vk::SwapchainKHR,
    pub extent: vk::Extent2D,
    pub present_mode: vk::PresentModeKHR,
    /// Premultiplied-alpha compositing was granted (translucent overlays); false = opaque.
    pub alpha: bool,
    views: Vec<vk::ImageView>,
    pub framebuffers: Vec<vk::Framebuffer>,
    /// Per swapchain image: present's wait is not fence-covered, so a per-slot semaphore could
    /// be re-signalled while an old present still owns it (validation VUID-00067).
    render_finished: Vec<vk::Semaphore>,
    image_available: Vec<vk::Semaphore>,
    fences: Vec<vk::Fence>,
    cmds: Vec<vk::CommandBuffer>,
    slot: usize,
    pub stalled: bool,
    pub presented: u64,
    pub dropped: u64,
    /// Old chains whose presents may still be in flight. Present has no fence, so they are
    /// destroyed only after enough further frames have gone through (or at teardown).
    retired: Vec<Retired>,
}

struct Retired {
    swapchain: vk::SwapchainKHR,
    views: Vec<vk::ImageView>,
    framebuffers: Vec<vk::Framebuffer>,
    render_finished: Vec<vk::Semaphore>,
    /// Set when the colour mode changed: the framebuffers above still reference it.
    render_pass: Option<vk::RenderPass>,
    retired_at: u64,
}

impl SurfaceChain {
    /// `prefer_mailbox`: interactive overlays want MAILBOX (Hyprland composes on demand, and
    /// FIFO then stalls ~500 ms at every idle->input transition). Lock screens want FIFO: at
    /// DPMS-off the blocking present parks only the GPU work and the bounded acquire keeps
    /// the loop alive, with zero GPU load while the screen is off.
    ///
    /// # Safety
    /// Vulkan; `surface` must belong to `gpu`.
    pub unsafe fn new(
        gpu: &Gpu,
        tag: impl Into<String>,
        surface: vk::SurfaceKHR,
        want_hdr: bool,
        width: u32,
        height: u32,
        prefer_mailbox: bool,
        want_alpha: bool,
    ) -> Result<Self> {
        let format = gpu.choose_format(surface, want_hdr)?;
        let hdr = is_scrgb(&format);
        let render_pass = create_render_pass(&gpu.device, format.format, vk::ImageLayout::PRESENT_SRC_KHR)?;
        let cmds = gpu.device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(gpu.command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(FRAMES_IN_FLIGHT as u32),
        )?;
        let mut image_available = Vec::new();
        let mut fences = Vec::new();
        for _ in 0..FRAMES_IN_FLIGHT {
            image_available.push(gpu.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?);
            fences.push(gpu.device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?);
        }
        let modes = gpu
            .surface_loader
            .get_physical_device_surface_present_modes(gpu.physical, surface)
            .unwrap_or_default();
        let present_mode = if prefer_mailbox && modes.contains(&vk::PresentModeKHR::MAILBOX) {
            vk::PresentModeKHR::MAILBOX
        } else {
            vk::PresentModeKHR::FIFO
        };
        let mut chain = SurfaceChain {
            tag: tag.into(),
            surface,
            format,
            hdr,
            render_pass,
            swapchain: vk::SwapchainKHR::null(),
            extent: vk::Extent2D::default(),
            present_mode,
            alpha: want_alpha,
            views: Vec::new(),
            framebuffers: Vec::new(),
            render_finished: Vec::new(),
            image_available,
            fences,
            cmds,
            slot: 0,
            stalled: false,
            presented: 0,
            dropped: 0,
            retired: Vec::new(),
        };
        chain.rebuild(gpu, width, height)?;
        Ok(chain)
    }

    /// (Re)creates the swapchain at `width`x`height` buffer pixels, handing any existing chain
    /// over as `old_swapchain`.
    ///
    /// # Safety
    /// Vulkan.
    pub unsafe fn rebuild(&mut self, gpu: &Gpu, width: u32, height: u32) -> Result<()> {
        let caps = gpu
            .surface_loader
            .get_physical_device_surface_capabilities(gpu.physical, self.surface)?;
        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: width.clamp(caps.min_image_extent.width.max(1), caps.max_image_extent.width),
                height: height.clamp(caps.min_image_extent.height.max(1), caps.max_image_extent.height),
            }
        };
        let mut count = caps.min_image_count + 1;
        if caps.max_image_count > 0 && count > caps.max_image_count {
            count = caps.max_image_count;
        }
        let composite_alpha = if self.alpha && caps.supported_composite_alpha.contains(vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED) {
            vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED
        } else {
            vk::CompositeAlphaFlagsKHR::OPAQUE
        };
        self.alpha = composite_alpha == vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED;
        let old = self.swapchain;
        let swapchain = gpu.swapchain_loader.create_swapchain(
            &vk::SwapchainCreateInfoKHR::default()
                .surface(self.surface)
                .min_image_count(count)
                .image_format(self.format.format)
                .image_color_space(self.format.color_space)
                .image_extent(extent)
                .image_array_layers(1)
                .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC)
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                .pre_transform(caps.current_transform)
                .composite_alpha(composite_alpha)
                .present_mode(self.present_mode)
                .clipped(true)
                .old_swapchain(old),
            None,
        )?;
        // The old chain's images/semaphores may still be owned by a present in flight, and
        // present is not fence-covered: park them and destroy later (see `reap_retired`).
        if old != vk::SwapchainKHR::null() {
            self.retired.push(Retired {
                swapchain: old,
                views: std::mem::take(&mut self.views),
                framebuffers: std::mem::take(&mut self.framebuffers),
                render_finished: std::mem::take(&mut self.render_finished),
                render_pass: None,
                retired_at: self.presented,
            });
        }
        self.swapchain = swapchain;
        self.extent = extent;
        let images = gpu.swapchain_loader.get_swapchain_images(swapchain)?;
        for &img in &images {
            let view = gpu.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(img)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(self.format.format)
                    .subresource_range(mip_range(0)),
                None,
            )?;
            let att = [view];
            let fb = gpu.device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(self.render_pass)
                    .attachments(&att)
                    .width(extent.width)
                    .height(extent.height)
                    .layers(1),
                None,
            )?;
            self.views.push(view);
            self.framebuffers.push(fb);
            self.render_finished
                .push(gpu.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?);
        }
        eprintln!(
            "{}: swapchain {}x{} {:?}/{:?} {} images {:?} ({})",
            self.tag,
            extent.width,
            extent.height,
            self.format.format,
            self.format.color_space,
            images.len(),
            self.present_mode,
            if self.hdr { "HDR scRGB" } else { "SDR" }
        );
        Ok(())
    }

    unsafe fn destroy_per_image(&mut self, gpu: &Gpu) {
        for fb in self.framebuffers.drain(..) {
            gpu.device.destroy_framebuffer(fb, None);
        }
        for v in self.views.drain(..) {
            gpu.device.destroy_image_view(v, None);
        }
        for s in self.render_finished.drain(..) {
            gpu.device.destroy_semaphore(s, None);
        }
    }

    unsafe fn destroy_retired(gpu: &Gpu, r: Retired) {
        for fb in r.framebuffers {
            gpu.device.destroy_framebuffer(fb, None);
        }
        for v in r.views {
            gpu.device.destroy_image_view(v, None);
        }
        for s in r.render_finished {
            gpu.device.destroy_semaphore(s, None);
        }
        gpu.swapchain_loader.destroy_swapchain(r.swapchain, None);
        if let Some(pass) = r.render_pass {
            gpu.device.destroy_render_pass(pass, None);
        }
    }

    /// Destroys retired chains once `presented` has moved far enough past their retirement
    /// that every present which could reference them has completed.
    unsafe fn reap_retired(&mut self, gpu: &Gpu) {
        const GRACE: u64 = 8;
        let presented = self.presented;
        let (done, keep): (Vec<_>, Vec<_>) = self.retired.drain(..).partition(|r| presented >= r.retired_at + GRACE);
        self.retired = keep;
        for r in done {
            Self::destroy_retired(gpu, r);
        }
    }

    /// Switches the surface between HDR (scRGB fp16) and SDR. `Ok(true)` = the format changed,
    /// and the caller must pick up the new `render_pass` (pipelines are keyed on the format).
    ///
    /// The swapchain is recreated on the SAME `VkSurfaceKHR`: a second surface for one
    /// wl_surface makes NVIDIA's WSI request a second `wp_fifo_v1` for it, which the compositor
    /// answers by killing the connection ("Protocol error 0 on object @0").
    ///
    /// # Safety
    /// Vulkan.
    pub unsafe fn set_hdr(&mut self, gpu: &Gpu, want_hdr: bool, width: u32, height: u32) -> Result<bool> {
        let format = gpu.choose_format(self.surface, want_hdr)?;
        if format.format == self.format.format && format.color_space == self.format.color_space {
            return Ok(false);
        }
        self.format = format;
        self.hdr = is_scrgb(&format);
        let pass = create_render_pass(&gpu.device, format.format, vk::ImageLayout::PRESENT_SRC_KHR)?;
        let old_pass = std::mem::replace(&mut self.render_pass, pass);
        self.rebuild(gpu, width, height)?;
        // rebuild() parked the framebuffers that still reference the old pass; it goes with them.
        match self.retired.last_mut() {
            Some(r) => r.render_pass = Some(old_pass),
            None => gpu.device.destroy_render_pass(old_pass, None),
        }
        Ok(true)
    }

    /// Waits (bounded) for the slot's fence and acquires an image. `Ok(None)` = frame dropped
    /// (compositor holds every image, e.g. DPMS-off); the edges of a stall are logged once.
    ///
    /// # Safety
    /// Vulkan.
    pub unsafe fn begin_frame(&mut self, gpu: &Gpu) -> Result<Option<Frame>> {
        if self.swapchain == vk::SwapchainKHR::null() {
            return Ok(None);
        }
        self.reap_retired(gpu);
        let slot = self.slot;
        match gpu.device.wait_for_fences(&[self.fences[slot]], true, GPU_WAIT_NS) {
            Ok(()) => {}
            Err(vk::Result::TIMEOUT) => {
                self.stall("previous frame still in flight");
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        }
        let image_index = match gpu.swapchain_loader.acquire_next_image(
            self.swapchain,
            GPU_WAIT_NS,
            self.image_available[slot],
            vk::Fence::null(),
        ) {
            Ok((i, _)) => i,
            Err(vk::Result::TIMEOUT | vk::Result::NOT_READY) => {
                self.stall("no swapchain image free");
                return Ok(None);
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                let (w, h) = (self.extent.width, self.extent.height);
                self.rebuild(gpu, w, h)?;
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };
        if self.stalled {
            self.stalled = false;
            eprintln!("{}: rendering resumed", self.tag);
        }
        // The slot's fence is reset only right before the submit that signals it again (in
        // `end_frame`): reset here, a failure in between would leave it unsignalled for good,
        // and every later frame on this slot would time out waiting for it.
        let cmd = self.cmds[slot];
        gpu.device
            .reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())?;
        gpu.device.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;
        Ok(Some(Frame { cmd, image_index, slot }))
    }

    fn stall(&mut self, why: &str) {
        self.dropped += 1;
        if !self.stalled {
            self.stalled = true;
            eprintln!("{}: dropping frames ({why}); input stays responsive", self.tag);
        }
    }

    /// Begins this chain's render pass on the frame's image with the given clear colour.
    ///
    /// # Safety
    /// Vulkan.
    pub unsafe fn begin_render_pass(&self, gpu: &Gpu, frame: &Frame, clear: [f32; 4]) {
        let clear_values = [vk::ClearValue { color: vk::ClearColorValue { float32: clear } }];
        gpu.device.cmd_begin_render_pass(
            frame.cmd,
            &vk::RenderPassBeginInfo::default()
                .render_pass(self.render_pass)
                .framebuffer(self.framebuffers[frame.image_index as usize])
                .render_area(vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: self.extent })
                .clear_values(&clear_values),
            vk::SubpassContents::INLINE,
        );
    }

    /// Ends the command buffer, submits and presents. A rebuild is triggered on OUT_OF_DATE.
    ///
    /// # Safety
    /// Vulkan; the caller must have ended any render pass it began.
    pub unsafe fn end_frame(&mut self, gpu: &Gpu, frame: Frame) -> Result<()> {
        gpu.device.end_command_buffer(frame.cmd)?;
        let wait = [self.image_available[frame.slot]];
        let stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let signal = [self.render_finished[frame.image_index as usize]];
        let cmds = [frame.cmd];
        let submit = [vk::SubmitInfo::default()
            .wait_semaphores(&wait)
            .wait_dst_stage_mask(&stages)
            .command_buffers(&cmds)
            .signal_semaphores(&signal)];
        gpu.device.reset_fences(&[self.fences[frame.slot]])?;
        gpu.device.queue_submit(gpu.queue, &submit, self.fences[frame.slot])?;
        let chains = [self.swapchain];
        let indices = [frame.image_index];
        let present = vk::PresentInfoKHR::default()
            .wait_semaphores(&signal)
            .swapchains(&chains)
            .image_indices(&indices);
        match gpu.swapchain_loader.queue_present(gpu.queue, &present) {
            Ok(_) => {}
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) | Err(vk::Result::SUBOPTIMAL_KHR) => {
                let (w, h) = (self.extent.width, self.extent.height);
                self.rebuild(gpu, w, h)?;
            }
            Err(e) => return Err(e.into()),
        }
        self.slot = (frame.slot + 1) % FRAMES_IN_FLIGHT;
        self.presented += 1;
        Ok(())
    }

    /// Bounded wait for every frame this chain has in flight. `false` = timed out.
    pub fn wait_pending(&self, gpu: &Gpu, timeout_ns: u64) -> bool {
        unsafe { gpu.device.wait_for_fences(&self.fences, true, timeout_ns).is_ok() }
    }

    /// # Safety
    /// Vulkan.
    pub unsafe fn destroy(mut self, gpu: &Gpu) {
        let _ = gpu.device.wait_for_fences(&self.fences, true, 500_000_000);
        for r in std::mem::take(&mut self.retired) {
            Self::destroy_retired(gpu, r);
        }
        self.destroy_per_image(gpu);
        if self.swapchain != vk::SwapchainKHR::null() {
            gpu.swapchain_loader.destroy_swapchain(self.swapchain, None);
        }
        for s in self.image_available.drain(..) {
            gpu.device.destroy_semaphore(s, None);
        }
        for f in self.fences.drain(..) {
            gpu.device.destroy_fence(f, None);
        }
        gpu.device.free_command_buffers(gpu.command_pool, &self.cmds);
        gpu.device.destroy_render_pass(self.render_pass, None);
        gpu.surface_loader.destroy_surface(self.surface, None);
    }
}
