//! Instance, physical/logical device, queue and command pool: one per process.

use std::ffi::CStr;

use anyhow::{anyhow, Context, Result};
use ash::{ext, khr, vk};

pub struct Gpu {
    pub entry: ash::Entry,
    pub instance: ash::Instance,
    pub surface_loader: khr::surface::Instance,
    pub wayland_loader: khr::wayland_surface::Instance,
    pub physical: vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue_family: u32,
    pub queue: vk::Queue,
    pub swapchain_loader: khr::swapchain::Device,
    pub command_pool: vk::CommandPool,
    pub limits: vk::PhysicalDeviceLimits,
    pub mem_props: vk::PhysicalDeviceMemoryProperties,
    pub device_name: String,
    /// Whether this device can share images with the compositor as dmabufs, which is what a
    /// screen capture needs to arrive in the output's own colours rather than flattened to SDR.
    pub dmabuf: bool,
}

impl Gpu {
    /// Creates the instance and picks the device against `first_surface` (a `wl_surface*`), so
    /// the chosen queue family is known to present to Wayland. The `VkSurfaceKHR` created for
    /// that first surface is returned so the caller can reuse it instead of creating a second.
    ///
    /// `validation_env`: name of the environment variable that enables the Khronos validation
    /// layer (each app uses its own, e.g. `VANTALOCK_VK_VALIDATION`).
    ///
    /// # Safety
    /// `display` and `first_surface` must be live libwayland pointers that outlive the `Gpu`.
    pub unsafe fn new(
        app_name: &CStr,
        display: *mut std::ffi::c_void,
        first_surface: *mut std::ffi::c_void,
        validation_env: &str,
    ) -> Result<(Self, vk::SurfaceKHR)> {
        let entry = ash::Entry::load().context("load Vulkan loader")?;
        let app_info = vk::ApplicationInfo::default()
            .application_name(app_name)
            .api_version(vk::API_VERSION_1_1);
        // VK_EXT_swapchain_colorspace is what exposes EXTENDED_SRGB_LINEAR_EXT at all. Without
        // it the surface never offers that colour space and every output is drawn in SDR.
        let mut instance_exts = vec![khr::surface::NAME.as_ptr(), khr::wayland_surface::NAME.as_ptr()];
        let available = entry.enumerate_instance_extension_properties(None).unwrap_or_default();
        if available.iter().any(|e| e.extension_name_as_c_str() == Ok(ext::swapchain_colorspace::NAME)) {
            instance_exts.push(ext::swapchain_colorspace::NAME.as_ptr());
        } else {
            eprintln!("vanta-gpu: {} is missing; HDR output is off", ext::swapchain_colorspace::NAME.to_string_lossy());
        }
        let validation = c"VK_LAYER_KHRONOS_validation";
        let layers: Vec<*const i8> = if std::env::var_os(validation_env).is_some() {
            vec![validation.as_ptr()]
        } else {
            Vec::new()
        };
        let instance = entry
            .create_instance(
                &vk::InstanceCreateInfo::default()
                    .application_info(&app_info)
                    .enabled_extension_names(&instance_exts)
                    .enabled_layer_names(&layers),
                None,
            )
            .context("create Vulkan instance")?;
        let surface_loader = khr::surface::Instance::new(&entry, &instance);
        let wayland_loader = khr::wayland_surface::Instance::new(&entry, &instance);
        let surface = wayland_loader
            .create_wayland_surface(
                &vk::WaylandSurfaceCreateInfoKHR::default()
                    .display(display.cast())
                    .surface(first_surface.cast()),
                None,
            )
            .context("create Wayland Vulkan surface")?;

        // Prefer a discrete GPU with a graphics queue that can present to this surface, unless
        // VANTA_VK_DEVICE names another (any part of its name, case-insensitive): on a laptop
        // whose integrated GPU drives the screen, that one saves waking the discrete one.
        let wanted = std::env::var("VANTA_VK_DEVICE").ok().filter(|w| !w.is_empty()).map(|w| w.to_lowercase());
        let mut best: Option<(vk::PhysicalDevice, u32, i32)> = None;
        for pd in instance.enumerate_physical_devices()? {
            let props = instance.get_physical_device_properties(pd);
            let name = CStr::from_ptr(props.device_name.as_ptr()).to_string_lossy().to_lowercase();
            let score = match props.device_type {
                _ if wanted.as_deref().is_some_and(|w| name.contains(w)) => 3,
                vk::PhysicalDeviceType::DISCRETE_GPU => 2,
                vk::PhysicalDeviceType::INTEGRATED_GPU => 1,
                _ => 0,
            };
            for (i, qf) in instance
                .get_physical_device_queue_family_properties(pd)
                .iter()
                .enumerate()
            {
                if qf.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                    // A card that cannot answer is passed over, not a reason to give up on all.
                    && surface_loader.get_physical_device_surface_support(pd, i as u32, surface).unwrap_or(false)
                {
                    if best.map_or(true, |(_, _, s)| score > s) {
                        best = Some((pd, i as u32, score));
                    }
                    break;
                }
            }
        }
        let (physical, queue_family, _) =
            best.ok_or_else(|| anyhow!("no GPU can present to a Wayland surface"))?;
        let props = instance.get_physical_device_properties(physical);
        let device_name = CStr::from_ptr(props.device_name.as_ptr())
            .to_string_lossy()
            .into_owned();
        let mem_props = instance.get_physical_device_memory_properties(physical);

        let prio = [1.0f32];
        let qinfo = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&prio)];
        // The swapchain always; the dmabuf set when the driver has it, so an image can be handed
        // to the compositor and read back here without going through shared memory.
        let have: Vec<String> = instance
            .enumerate_device_extension_properties(physical)?
            .iter()
            .map(|e| CStr::from_ptr(e.extension_name.as_ptr()).to_string_lossy().into_owned())
            .collect();
        let shared = [
            khr::external_memory_fd::NAME,
            ext::external_memory_dma_buf::NAME,
            ext::image_drm_format_modifier::NAME,
            // Required by image_drm_format_modifier on a Vulkan 1.1 device.
            khr::image_format_list::NAME,
            ext::queue_family_foreign::NAME,
        ];
        let dmabuf = shared.iter().all(|n| have.iter().any(|h| h.as_str() == n.to_string_lossy()));
        let mut dev_exts = vec![khr::swapchain::NAME.as_ptr()];
        if dmabuf {
            dev_exts.extend(shared.iter().map(|n| n.as_ptr()));
        }
        let device = instance
            .create_device(
                physical,
                &vk::DeviceCreateInfo::default()
                    .queue_create_infos(&qinfo)
                    .enabled_extension_names(&dev_exts),
                None,
            )
            .context("create logical device")?;
        let queue = device.get_device_queue(queue_family, 0);
        let swapchain_loader = khr::swapchain::Device::new(&instance, &device);
        let command_pool = device.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(queue_family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )?;
        Ok((
            Gpu {
                entry,
                instance,
                surface_loader,
                wayland_loader,
                physical,
                device,
                queue_family,
                queue,
                swapchain_loader,
                command_pool,
                limits: props.limits,
                mem_props,
                device_name,
                dmabuf,
            },
            surface,
        ))
    }

    /// Wraps another `wl_surface*` as a `VkSurfaceKHR` on the same display.
    ///
    /// # Safety
    /// Same contract as [`Gpu::new`].
    pub unsafe fn create_surface(
        &self,
        display: *mut std::ffi::c_void,
        wl_surface: *mut std::ffi::c_void,
    ) -> Result<vk::SurfaceKHR> {
        Ok(self.wayland_loader.create_wayland_surface(
            &vk::WaylandSurfaceCreateInfoKHR::default()
                .display(display.cast())
                .surface(wl_surface.cast()),
            None,
        )?)
    }

    pub fn surface_formats(&self, surface: vk::SurfaceKHR) -> Result<Vec<vk::SurfaceFormatKHR>> {
        Ok(unsafe {
            self.surface_loader
                .get_physical_device_surface_formats(self.physical, surface)?
        })
    }

    /// scRGB fp16 when `want_hdr` and offered, else the plainest sRGB format available.
    /// `VANTA_HDR=off` keeps every surface in SDR, for a driver whose HDR output comes out wrong.
    pub fn choose_format(&self, surface: vk::SurfaceKHR, want_hdr: bool) -> Result<vk::SurfaceFormatKHR> {
        let formats = self.surface_formats(surface)?;
        let hdr_off = std::env::var("VANTA_HDR").is_ok_and(|v| v == "off" || v == "0");
        if want_hdr && !hdr_off {
            if let Some(f) = formats.iter().find(|f| is_scrgb(f)) {
                return Ok(*f);
            }
        }
        formats
            .iter()
            .find(|f| {
                f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
                    && f.format == vk::Format::B8G8R8A8_UNORM
            })
            // Any UNORM one next: an _SRGB format would encode a second time what the shaders
            // already encode.
            .or_else(|| formats.iter().find(|f| f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR && format!("{:?}", f.format).ends_with("_UNORM")))
            .or_else(|| {
                formats
                    .iter()
                    .find(|f| f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR)
            })
            .copied()
            .ok_or_else(|| anyhow!("no sRGB surface format offered"))
    }

    pub fn find_memory_type(&self, type_bits: u32, props: vk::MemoryPropertyFlags) -> Result<u32> {
        (0..self.mem_props.memory_type_count)
            .find(|&i| {
                type_bits & (1 << i) != 0
                    && self.mem_props.memory_types[i as usize]
                        .property_flags
                        .contains(props)
            })
            .ok_or_else(|| anyhow!("no memory type with {props:?}"))
    }

    /// Records `f` into a one-time command buffer, submits it and waits (bounded 5 s: an
    /// upload that takes longer than that is a wedged driver, not a slow upload).
    ///
    /// # Safety
    /// Vulkan.
    pub unsafe fn one_shot(&self, f: impl FnOnce(vk::CommandBuffer)) -> Result<()> {
        let cmd = self.device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(self.command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )?[0];
        self.device.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;
        f(cmd);
        self.device.end_command_buffer(cmd)?;
        let fence = self.device.create_fence(&vk::FenceCreateInfo::default(), None)?;
        let cmds = [cmd];
        self.device
            .queue_submit(self.queue, &[vk::SubmitInfo::default().command_buffers(&cmds)], fence)?;
        if self.device.wait_for_fences(&[fence], true, 5_000_000_000).is_err() {
            // Still running, or the device is lost. Either way the GPU may yet read or write
            // what the commands name, so nothing is freed: the fence and command buffer are left
            // to leak, and the caller is told to leave its buffers alone too ([`StillRunning`]).
            return Err(anyhow::Error::new(StillRunning));
        }
        self.device.destroy_fence(fence, None);
        self.device.free_command_buffers(self.command_pool, &cmds);
        Ok(())
    }

    pub fn format_supports_linear_blit(&self, format: vk::Format) -> bool {
        let p = unsafe {
            self.instance
                .get_physical_device_format_properties(self.physical, format)
        };
        p.optimal_tiling_features.contains(
            vk::FormatFeatureFlags::BLIT_SRC
                | vk::FormatFeatureFlags::BLIT_DST
                | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR,
        )
    }

    /// Destroys the device and instance. Every surface/chain/texture must be gone first.
    ///
    /// # Safety
    /// Vulkan.
    pub unsafe fn destroy(self) {
        self.device.destroy_command_pool(self.command_pool, None);
        self.device.destroy_device(None);
        self.instance.destroy_instance(None);
    }
}

pub fn is_scrgb(f: &vk::SurfaceFormatKHR) -> bool {
    f.format == vk::Format::R16G16B16A16_SFLOAT
        && f.color_space == vk::ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT
}

/// A one-shot command that did not finish in time. What it reads and writes may still be in use
/// by the GPU, so a caller must not free those buffers: leaking them is the safe choice on a GPU
/// that has stopped answering.
#[derive(Debug)]
pub struct StillRunning;

impl std::fmt::Display for StillRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("one-shot command did not finish within 5 s")
    }
}

impl std::error::Error for StillRunning {}

/// Whether `e` is (or wraps) [`StillRunning`].
pub fn still_running(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<StillRunning>())
}
