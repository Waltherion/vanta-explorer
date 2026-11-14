//! The Vulkan renderer: the shared vanta-gpu device and swapchain, and the two passes that are
//! this app's own.
//!
//! wgpu does not expose VK_COLOR_SPACE_EXTENDED_SRGB_LINEAR_EXT, which is why this is ash.
//!
//! Frame layout:
//!   pass 1 (ui_pass)      egui -> offscreen RGBA16F, LINEAR premultiplied alpha
//!   pass 2 (the chain's)  clear to background, the HDR preview, then the UI layer with ui_scale
//!
//! vanta-gpu owns instance, device, swapchain, frame pacing and every bounded GPU wait: a hidden
//! window drops frames instead of blocking the event loop (Hyprland shows "wait or terminate"
//! when the xdg ping goes unanswered), rebuilds hand over the old swapchain, and MAILBOX keeps
//! acquire from waiting on Hyprland's on-demand composition (FIFO froze ~500 ms at every
//! idle-to-input transition).

use anyhow::{anyhow, Context, Result};
use ash::vk;
use vanta_gpu::{util, Gpu, SurfaceChain, Texture, GPU_WAIT_NS};

/// The UI layer is always fp16 linear so darks do not band before the scale is applied.
const UI_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;
pub const FRAMES_IN_FLIGHT: usize = vanta_gpu::chain::FRAMES_IN_FLIGHT;

const COMPOSITE_VERT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/composite.vert.spv"));
const COMPOSITE_FRAG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/composite.frag.spv"));
const HDR_VERT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/hdr_image.vert.spv"));
const HDR_FRAG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/hdr_image.frag.spv"));

/// windows-scRGB reference: content white sits at 203 nits, 1.0 = 80 nits.
/// Deliberately NOT the chrome's matchDesktop scale, see hdr_image.frag.
pub const CONTENT_SCALE: f32 = 203.0 / 80.0;

/// One HDR image to draw beneath the UI, in physical pixels.
#[derive(Clone, Copy, Debug)]
pub struct HdrDraw {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// 0 = BT.709, 1 = BT.2020, 2 = Display-P3.
    pub primaries: u32,
    pub exposure: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct HdrPush {
    rect: [f32; 4],
    params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CompositePush {
    ui_scale: f32,
    ui_sat: f32,
    sdr: f32,
    _pad: f32,
}

/// The offscreen UI layer, sized to the swapchain.
struct UiTarget {
    tex: Texture,
    fb: vk::Framebuffer,
    extent: vk::Extent2D,
}

pub struct Renderer {
    /// `Some` for the renderer's whole life; taken only in `drop`, which has to hand it over
    /// by value to destroy it.
    gpu: Option<Gpu>,
    chain: Option<SurfaceChain>,
    // What egui-ash-renderer is built from and uploads through.
    pub physical_device: vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub command_pool: vk::CommandPool,
    /// True when we actually got a non-SDR swapchain format. ANDed with the monitor's HDR
    /// state to decide the shader branch, both must hold.
    pub hdr_active: bool,
    /// True when the swapchain negotiated premultiplied alpha, i.e. a translucent background
    /// will actually show the wallpaper through.
    pub alpha_supported: bool,

    pub ui_pass: vk::RenderPass,
    ui: Option<UiTarget>,
    ui_sampler: vk::Sampler,

    desc_layout: vk::DescriptorSetLayout,
    desc_pool: vk::DescriptorPool,
    desc_set: vk::DescriptorSet,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,

    // --- HDR content layer ---
    hdr_pipeline_layout: vk::PipelineLayout,
    hdr_pipeline: vk::Pipeline,
    hdr_desc_set: vk::DescriptorSet,
    hdr_sampler: vk::Sampler,
    hdr_texture: Option<Texture>,
    /// The chain render pass the two pipelines were built against; they are rebuilt when an
    /// HDR/SDR switch gives the chain a new one.
    pipelines_for: vk::RenderPass,
}

/// `VANTAEXPLORER_FORMAT=sdr|hdr` overrides the monitor probe (`VANTA_HDR=off` is honoured by
/// vanta-gpu itself).
fn want_hdr_override(want_hdr: bool) -> bool {
    match std::env::var("VANTAEXPLORER_FORMAT").as_deref() {
        Ok("sdr") => false,
        Ok("hdr") => true,
        _ => want_hdr,
    }
}

impl Renderer {
    /// # Safety
    /// `display_ptr`/`surface_ptr` must be live Wayland handles outliving the renderer.
    pub unsafe fn new(
        display_ptr: *mut std::ffi::c_void,
        surface_ptr: *mut std::ffi::c_void,
        width: u32,
        height: u32,
        want_hdr: bool,
    ) -> Result<Self> {
        let (gpu, surface) = Gpu::new(c"vantaexplorer", display_ptr, surface_ptr, "VANTAEXPLORER_VK_VALIDATION")?;
        log::info!("GPU {}", gpu.device_name);
        let chain = SurfaceChain::new(&gpu, "vantaexplorer", surface, want_hdr_override(want_hdr), width, height, true, true)?;
        let device = gpu.device.clone();

        let ui_pass = util::create_render_pass(&device, UI_FORMAT, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)?;
        let sampler = |mips: bool| {
            let mut info = vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR)
                .min_filter(vk::Filter::LINEAR)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
            if mips {
                // The preview is a big picture drawn small: trilinear over its mip chain, or
                // fine detail shimmers.
                info = info.mipmap_mode(vk::SamplerMipmapMode::LINEAR).max_lod(vk::LOD_CLAMP_NONE);
            }
            device.create_sampler(&info, None)
        };
        let ui_sampler = sampler(false)?;
        let hdr_sampler = sampler(true)?;

        let bindings = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
        let desc_layout = device.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)?;
        // Two sets: the UI layer and the HDR content layer.
        let pool_sizes = [vk::DescriptorPoolSize::default().ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER).descriptor_count(2)];
        let desc_pool = device.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().pool_sizes(&pool_sizes).max_sets(2), None)?;
        let layouts = [desc_layout, desc_layout];
        let sets = device.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(desc_pool).set_layouts(&layouts))?;
        let (desc_set, hdr_desc_set) = (sets[0], sets[1]);

        let push_ranges = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)
            .offset(0)
            .size(std::mem::size_of::<CompositePush>() as u32)];
        let pipeline_layout = device.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts[..1]).push_constant_ranges(&push_ranges),
            None,
        )?;
        let hdr_push = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .offset(0)
            .size(std::mem::size_of::<HdrPush>() as u32)];
        let hdr_pipeline_layout = device.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts[..1]).push_constant_ranges(&hdr_push),
            None,
        )?;
        let pipeline = create_composite_pipeline(&device, pipeline_layout, chain.render_pass)?;
        let hdr_pipeline = create_hdr_pipeline(&device, hdr_pipeline_layout, chain.render_pass)?;

        let mut r = Self {
            physical_device: gpu.physical,
            device,
            queue: gpu.queue,
            command_pool: gpu.command_pool,
            hdr_active: chain.hdr,
            alpha_supported: chain.alpha,
            pipelines_for: chain.render_pass,
            gpu: Some(gpu),
            chain: Some(chain),
            ui_pass,
            ui: None,
            ui_sampler,
            desc_layout,
            desc_pool,
            desc_set,
            pipeline_layout,
            pipeline,
            hdr_pipeline_layout,
            hdr_pipeline,
            hdr_desc_set,
            hdr_sampler,
            hdr_texture: None,
        };
        r.ensure_ui_target()?;
        let f = r.chain().format;
        log::info!(
            "swapchain: {:?} / {:?} {:?} -> HDR {}",
            f.format,
            f.color_space,
            r.chain().present_mode,
            if r.hdr_active { "ACTIVE" } else { "unavailable (SDR fallback)" }
        );
        Ok(r)
    }

    fn gpu(&self) -> &Gpu {
        self.gpu.as_ref().expect("renderer in use after drop")
    }

    fn chain(&self) -> &SurfaceChain {
        self.chain.as_ref().expect("renderer in use after drop")
    }

    /// Waits (bounded) for every frame in flight, so resources they use can go. False when the
    /// GPU did not finish in time; the caller then leaks rather than frees under it.
    fn idle(&self) -> bool {
        self.chain().wait_pending(self.gpu(), GPU_WAIT_NS)
    }

    /// Upload a decoded image as the HDR content texture, replacing any previous one.
    ///
    /// fp16 goes straight to the GPU with no conversion: the decode crate already produced
    /// exactly the format the shader expects (linear, 1.0 = 203 nits, native primaries).
    pub fn upload_hdr(&mut self, w: u32, h: u32, rgba16f: &[half::f16]) -> Result<()> {
        if w == 0 || h == 0 || rgba16f.len() < (w as usize * h as usize * 4) {
            return Err(anyhow!("bad HDR image dimensions"));
        }
        let bytes = unsafe { std::slice::from_raw_parts(rgba16f.as_ptr().cast::<u8>(), w as usize * h as usize * 8) };
        let tex = unsafe { Texture::upload(self.gpu(), w, h, UI_FORMAT, bytes) }.context("upload HDR preview")?;
        self.drop_hdr_texture();
        let info = [vk::DescriptorImageInfo::default()
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .image_view(tex.view)
            .sampler(self.hdr_sampler)];
        unsafe {
            self.device.update_descriptor_sets(
                &[vk::WriteDescriptorSet::default()
                    .dst_set(self.hdr_desc_set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&info)],
                &[],
            );
        }
        self.hdr_texture = Some(tex);
        Ok(())
    }

    pub fn clear_hdr(&mut self) {
        self.drop_hdr_texture();
    }

    pub fn has_hdr_texture(&self) -> bool {
        self.hdr_texture.is_some()
    }

    /// The current preview texture goes once no frame in flight still samples it.
    fn drop_hdr_texture(&mut self) {
        let Some(t) = self.hdr_texture.take() else { return };
        if self.idle() {
            unsafe { t.destroy(self.gpu()) };
        } else {
            log::warn!("GPU busy past the wait; the old preview texture is left allocated");
        }
    }

    /// `vantaexplorer --probe`: dump every surface format, mirroring vantalock's `--probe`.
    pub fn report_formats(&self) -> Result<()> {
        let formats = self.gpu().surface_formats(self.chain().surface)?;
        println!("vantaexplorer: {} surface formats", formats.len());
        let mut scrgb = false;
        for f in &formats {
            let marker = if vanta_gpu::gpu::is_scrgb(f) {
                scrgb = true;
                "  <-- windows-scRGB"
            } else {
                ""
            };
            println!("  {:?} / {:?}{}", f.format, f.color_space, marker);
        }
        println!("\nscRGB HDR: {}", if scrgb { "AVAILABLE" } else { "NOT AVAILABLE on this output" });
        let f = self.chain().format;
        println!("selected:  {:?} / {:?}", f.format, f.color_space);
        Ok(())
    }

    /// The UI layer follows the swapchain's size; rebuilt when that changed.
    fn ensure_ui_target(&mut self) -> Result<()> {
        let extent = self.chain().extent;
        if extent.width == 0 || extent.height == 0 || self.ui.as_ref().is_some_and(|u| u.extent == extent) {
            return Ok(());
        }
        if let Some(old) = self.ui.take() {
            if self.idle() {
                unsafe {
                    self.device.destroy_framebuffer(old.fb, None);
                    old.tex.destroy(self.gpu());
                }
            } else {
                log::warn!("GPU busy past the wait; the old UI layer is left allocated");
            }
        }
        unsafe {
            let tex = Texture::new(self.gpu(), extent.width, extent.height, UI_FORMAT, 1, vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED)?;
            let attachments = [tex.view];
            let fb = self.device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(self.ui_pass)
                    .attachments(&attachments)
                    .width(extent.width)
                    .height(extent.height)
                    .layers(1),
                None,
            )?;
            let info = [vk::DescriptorImageInfo::default()
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .image_view(tex.view)
                .sampler(self.ui_sampler)];
            self.device.update_descriptor_sets(
                &[vk::WriteDescriptorSet::default()
                    .dst_set(self.desc_set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&info)],
                &[],
            );
            self.ui = Some(UiTarget { tex, fb, extent });
        }
        Ok(())
    }

    /// The pipelines follow the chain's render pass, which changes with the swapchain format.
    fn ensure_pipelines(&mut self) -> Result<()> {
        let pass = self.chain().render_pass;
        if pass == self.pipelines_for {
            return Ok(());
        }
        unsafe {
            let pipeline = create_composite_pipeline(&self.device, self.pipeline_layout, pass)?;
            let hdr_pipeline = create_hdr_pipeline(&self.device, self.hdr_pipeline_layout, pass)?;
            if self.idle() {
                self.device.destroy_pipeline(self.pipeline, None);
                self.device.destroy_pipeline(self.hdr_pipeline, None);
            }
            self.pipeline = pipeline;
            self.hdr_pipeline = hdr_pipeline;
        }
        self.pipelines_for = pass;
        Ok(())
    }

    /// Live HDR<->SDR switching. The NVIDIA Vulkan WSI owns the wp-color-management tag for
    /// the surface and derives it from the swapchain colorspace, so switching mode = recreating
    /// the swapchain with the other format. Manual tagging is a fatal protocol error here
    /// (`surface_exists`), do not reintroduce it.
    pub fn set_hdr(&mut self, want_hdr: bool) -> Result<bool> {
        let extent = self.chain().extent;
        let gpu = self.gpu.as_ref().expect("renderer in use after drop");
        let chain = self.chain.as_mut().expect("renderer in use after drop");
        let changed = unsafe { chain.set_hdr(gpu, want_hdr_override(want_hdr), extent.width, extent.height)? };
        if !changed {
            return Ok(false);
        }
        self.hdr_active = self.chain().hdr;
        self.alpha_supported = self.chain().alpha;
        self.ensure_pipelines()?;
        self.ensure_ui_target()?;
        let f = self.chain().format;
        log::info!("swapchain recreated: {:?} / {:?} -> HDR {}", f.format, f.color_space, if self.hdr_active { "ACTIVE" } else { "off" });
        Ok(true)
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let gpu = self.gpu.as_ref().expect("renderer in use after drop");
        let chain = self.chain.as_mut().expect("renderer in use after drop");
        unsafe { chain.rebuild(gpu, width, height)? };
        self.alpha_supported = self.chain().alpha;
        self.ensure_ui_target()
    }

    /// Records and submits one frame. `draw_ui` records egui into the offscreen pass.
    ///
    /// `bg` is the background colour in the swapchain's own encoding, already prepared by the
    /// caller (linear*scale for scRGB, sRGB-encoded for SDR).
    ///
    /// Layer order inside the present pass: background clear, then any HDR content, then the
    /// egui layer composited on top. The UI leaves the preview rect transparent, so the HDR
    /// image shows through it.
    pub fn render<F>(&mut self, bg: [f32; 4], ui_scale: f32, ui_sat: f32, hdr: Option<HdrDraw>, mut draw_ui: F) -> Result<()>
    where
        F: FnMut(vk::CommandBuffer, vk::Extent2D),
    {
        // The chain rebuilds itself on OUT_OF_DATE; the layer and pipelines follow here.
        self.ensure_ui_target()?;
        self.ensure_pipelines()?;
        let Some(ui) = self.ui.as_ref() else { return Ok(()) };
        let (ui_fb, extent) = (ui.fb, ui.extent);
        let gpu = self.gpu.as_ref().expect("renderer in use after drop");
        let chain = self.chain.as_mut().expect("renderer in use after drop");
        if chain.extent != extent {
            return Ok(());
        }
        unsafe {
            let Some(frame) = chain.begin_frame(gpu)? else { return Ok(()) };
            let cmd = frame.cmd;
            let dev = &gpu.device;

            // --- pass 1: egui into the linear fp16 UI layer ---
            let ui_clear = [vk::ClearValue { color: vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 0.0] } }];
            dev.cmd_begin_render_pass(
                cmd,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(self.ui_pass)
                    .framebuffer(ui_fb)
                    .render_area(vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent })
                    .clear_values(&ui_clear),
                vk::SubpassContents::INLINE,
            );
            draw_ui(cmd, extent);
            dev.cmd_end_render_pass(cmd);

            // --- pass 2: background, HDR content, UI composite ---
            chain.begin_render_pass(gpu, &frame, bg);
            dev.cmd_set_viewport(
                cmd,
                0,
                &[vk::Viewport { x: 0.0, y: 0.0, width: extent.width as f32, height: extent.height as f32, min_depth: 0.0, max_depth: 1.0 }],
            );
            dev.cmd_set_scissor(cmd, 0, &[vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent }]);
            let sdr = if chain.hdr { 0.0 } else { 1.0 };
            if let (Some(d), true) = (hdr, self.hdr_texture.is_some()) {
                let (fw, fh) = (extent.width as f32, extent.height as f32);
                // Pixels -> NDC.
                let to_ndc = |x: f32, y: f32| [x / fw * 2.0 - 1.0, y / fh * 2.0 - 1.0];
                let [x0, y0] = to_ndc(d.x, d.y);
                let [x1, y1] = to_ndc(d.x + d.w, d.y + d.h);
                let push = HdrPush { rect: [x0, y0, x1, y1], params: [CONTENT_SCALE, sdr, d.primaries as f32, d.exposure] };
                dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.hdr_pipeline);
                dev.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::GRAPHICS, self.hdr_pipeline_layout, 0, &[self.hdr_desc_set], &[]);
                dev.cmd_push_constants(
                    cmd,
                    self.hdr_pipeline_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    std::slice::from_raw_parts((&push as *const HdrPush).cast::<u8>(), std::mem::size_of::<HdrPush>()),
                );
                dev.cmd_draw(cmd, 6, 1, 0, 0);
            }
            dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            dev.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline_layout, 0, &[self.desc_set], &[]);
            let push = CompositePush { ui_scale, ui_sat, sdr, _pad: 0.0 };
            dev.cmd_push_constants(
                cmd,
                self.pipeline_layout,
                vk::ShaderStageFlags::FRAGMENT,
                0,
                std::slice::from_raw_parts((&push as *const CompositePush).cast::<u8>(), std::mem::size_of::<CompositePush>()),
            );
            dev.cmd_draw(cmd, 3, 1, 0, 0);
            dev.cmd_end_render_pass(cmd);
            chain.end_frame(gpu, frame)?;
        }
        Ok(())
    }

    /// egui-ash-renderer needs the instance to query memory properties.
    pub fn instance_ref(&self) -> &ash::Instance {
        &self.gpu().instance
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        let Some(gpu) = self.gpu.take() else { return };
        unsafe {
            let finished = self.chain.as_ref().is_none_or(|c| c.wait_pending(&gpu, 500_000_000));
            if !finished {
                // Tearing down under a GPU that has not finished would fault; the process is
                // ending anyway.
                log::warn!("GPU busy at exit; Vulkan objects left to the driver");
                return;
            }
            let dev = &gpu.device;
            if let Some(ui) = self.ui.take() {
                dev.destroy_framebuffer(ui.fb, None);
                ui.tex.destroy(&gpu);
            }
            if let Some(t) = self.hdr_texture.take() {
                t.destroy(&gpu);
            }
            dev.destroy_pipeline(self.hdr_pipeline, None);
            dev.destroy_pipeline_layout(self.hdr_pipeline_layout, None);
            dev.destroy_sampler(self.hdr_sampler, None);
            dev.destroy_pipeline(self.pipeline, None);
            dev.destroy_pipeline_layout(self.pipeline_layout, None);
            dev.destroy_descriptor_pool(self.desc_pool, None);
            dev.destroy_descriptor_set_layout(self.desc_layout, None);
            dev.destroy_sampler(self.ui_sampler, None);
            dev.destroy_render_pass(self.ui_pass, None);
            if let Some(chain) = self.chain.take() {
                chain.destroy(&gpu);
            }
            gpu.destroy();
        }
    }
}

fn create_hdr_pipeline(
    device: &ash::Device,
    layout: vk::PipelineLayout,
    render_pass: vk::RenderPass,
) -> Result<vk::Pipeline> {
    unsafe {
        let vert = create_shader_module(device, HDR_VERT)?;
        let frag = create_shader_module(device, HDR_FRAG)?;
        let entry = c"main";

        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vert)
                .name(entry),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(frag)
                .name(entry),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let blend_attachments = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)];
        let blending =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&blending)
            .dynamic_state(&dynamic)
            .layout(layout)
            .render_pass(render_pass)
            .subpass(0);
        let pipeline = device
            .create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)
            .map_err(|(_, e)| anyhow!("hdr pipeline: {e}"))?[0];
        device.destroy_shader_module(vert, None);
        device.destroy_shader_module(frag, None);
        Ok(pipeline)
    }
}

fn create_composite_pipeline(
    device: &ash::Device,
    layout: vk::PipelineLayout,
    render_pass: vk::RenderPass,
) -> Result<vk::Pipeline> {
    unsafe {
        let vert = create_shader_module(device, COMPOSITE_VERT)?;
        let frag = create_shader_module(device, COMPOSITE_FRAG)?;
        let entry = c"main";

        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vert)
                .name(entry),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(frag)
                .name(entry),
        ];

        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // Premultiplied alpha, matching what egui-ash-renderer writes into the UI layer.
        let blend_attachments = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)];
        let blending =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&blending)
            .dynamic_state(&dynamic)
            .layout(layout)
            .render_pass(render_pass)
            .subpass(0);

        let pipeline = device
            .create_graphics_pipelines(vk::PipelineCache::null(), &[info], None)
            .map_err(|(_, e)| anyhow!("composite pipeline: {e}"))?[0];

        device.destroy_shader_module(vert, None);
        device.destroy_shader_module(frag, None);
        Ok(pipeline)
    }
}

unsafe fn create_shader_module(device: &ash::Device, spv: &[u8]) -> Result<vk::ShaderModule> {
    let mut cursor = std::io::Cursor::new(spv);
    let code = ash::util::read_spv(&mut cursor).context("read SPIR-V")?;
    Ok(device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None)?)
}

