//! Images the compositor can write into and this side can read: a Vulkan image laid out by a DRM
//! format modifier, with its memory exported as a dmabuf.
//!
//! A screen capture through shared memory arrives flattened to SDR, because the compositor has to
//! convert it for a buffer that carries no colour of its own. A dmabuf is the output's own kind of
//! buffer, so this is the route a capture has to take if the light in it is to survive. The image
//! is tiled the way the graphics card wants (compositors generally offer no linear layout), so it is read back
//! with an ordinary copy on the device rather than by mapping it.

use anyhow::{anyhow, Context, Result};
use ash::{ext, khr, vk};

use crate::gpu::Gpu;

/// An image shared with the compositor, and what the protocol needs to know about it.
pub struct Shared {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub w: u32,
    pub h: u32,
    pub format: vk::Format,
    /// The layout the compositor was told about.
    pub modifier: u64,
    pub offset: u64,
    pub stride: u64,
    /// The file the buffer is passed over as. Closed with the image.
    pub fd: std::os::fd::OwnedFd,
}

/// Which layouts this device can put `format` in, as DRM format modifiers.
///
/// # Safety
/// Vulkan.
pub unsafe fn modifiers(gpu: &Gpu, format: vk::Format) -> Vec<u64> {
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
    let mut props = vk::FormatProperties2::default().push_next(&mut list);
    gpu.instance.get_physical_device_format_properties2(gpu.physical, format, &mut props);
    let count = list.drm_format_modifier_count as usize;
    if count == 0 {
        return Vec::new();
    }
    let mut store = vec![vk::DrmFormatModifierPropertiesEXT::default(); count];
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default().drm_format_modifier_properties(&mut store);
    let mut props = vk::FormatProperties2::default().push_next(&mut list);
    gpu.instance.get_physical_device_format_properties2(gpu.physical, format, &mut props);
    store.iter().map(|p| p.drm_format_modifier).collect()
}

impl Shared {
    /// Makes an image the compositor can render into, in one of `allowed` layouts, and hands back
    /// everything the dmabuf protocol asks for. `usage` is what this side will do with it, which
    /// is at least TRANSFER_SRC, since the point is to read it.
    ///
    /// # Safety
    /// Vulkan.
    pub unsafe fn new(gpu: &Gpu, w: u32, h: u32, format: vk::Format, allowed: &[u64], usage: vk::ImageUsageFlags) -> Result<Shared> {
        if !gpu.dmabuf {
            return Err(anyhow!("this device cannot share images with the compositor"));
        }
        if allowed.is_empty() {
            return Err(anyhow!("no layout both sides know"));
        }
        let mut external = vk::ExternalMemoryImageCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let mut modifiers = vk::ImageDrmFormatModifierListCreateInfoEXT::default().drm_format_modifiers(allowed);
        let image = gpu
            .device
            .create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(format)
                    .extent(vk::Extent3D { width: w, height: h, depth: 1 })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED)
                    .push_next(&mut external)
                    .push_next(&mut modifiers),
                None,
            )
            .context("create the shared image")?;

        let req = gpu.device.get_image_memory_requirements(image);
        let mut export = vk::ExportMemoryAllocateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let memory = gpu
            .device
            .allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(gpu.find_memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?)
                    .push_next(&mut export)
                    .push_next(&mut dedicated),
                None,
            )
            .context("memory for the shared image")?;
        gpu.device.bind_image_memory(image, memory, 0).context("bind the shared image")?;

        // Which layout the driver actually chose, and where the pixels sit in it.
        let modifier_dev = ext::image_drm_format_modifier::Device::new(&gpu.instance, &gpu.device);
        let mut chosen = vk::ImageDrmFormatModifierPropertiesEXT::default();
        modifier_dev.get_image_drm_format_modifier_properties(image, &mut chosen).context("ask which layout the image got")?;
        let plane = vk::ImageSubresource::default().aspect_mask(vk::ImageAspectFlags::MEMORY_PLANE_0_EXT);
        let layout = gpu.device.get_image_subresource_layout(image, plane);

        let fd_dev = khr::external_memory_fd::Device::new(&gpu.instance, &gpu.device);
        let fd = fd_dev
            .get_memory_fd(&vk::MemoryGetFdInfoKHR::default().memory(memory).handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT))
            .context("export the image as a file")?;
        // SAFETY: the driver just handed this over and nothing else holds it.
        let fd = <std::os::fd::OwnedFd as From<_>>::from(std::os::fd::OwnedFd::from_raw_fd(fd));

        Ok(Shared {
            image,
            memory,
            w,
            h,
            format,
            modifier: chosen.drm_format_modifier,
            offset: layout.offset,
            stride: layout.row_pitch,
            fd,
        })
    }

    /// # Safety
    /// Vulkan. Nothing may still be using the image.
    pub unsafe fn destroy(self, gpu: &Gpu) {
        gpu.device.destroy_image(self.image, None);
        gpu.device.free_memory(self.memory, None);
    }
}

use std::os::fd::FromRawFd;
