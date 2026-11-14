//! Sampled images with optional mip chains: uploaded (wallpapers) or rendered-to (UI panels).

use anyhow::Result;
use ash::vk;

use crate::gpu::Gpu;
use crate::util::{color_range, mip_levels, mip_range, transition};

pub struct Texture {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
    pub format: vk::Format,
    pub w: u32,
    pub h: u32,
    pub mips: u32,
}

impl Texture {
    /// Creates an optimal-tiling 2D image with `mips` levels (device-local, no contents yet).
    ///
    /// # Safety
    /// Vulkan.
    pub unsafe fn new(
        gpu: &Gpu,
        w: u32,
        h: u32,
        format: vk::Format,
        mips: u32,
        usage: vk::ImageUsageFlags,
    ) -> Result<Self> {
        let image = gpu.device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(format)
                .extent(vk::Extent3D { width: w, height: h, depth: 1 })
                .mip_levels(mips)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(usage)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            None,
        )?;
        let req = gpu.device.get_image_memory_requirements(image);
        let memory = gpu.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(
                    gpu.find_memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?,
                ),
            None,
        )?;
        gpu.device.bind_image_memory(image, memory, 0)?;
        let view = gpu.device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .subresource_range(color_range()),
            None,
        )?;
        Ok(Texture { image, memory, view, format, w, h, mips })
    }

    /// Uploads `bytes` (tightly packed rows) into mip 0, generates the remaining mips if the
    /// format can be blitted linearly, and leaves the whole image SHADER_READ_ONLY.
    ///
    /// # Safety
    /// Vulkan.
    pub unsafe fn upload(gpu: &Gpu, w: u32, h: u32, format: vk::Format, bytes: &[u8]) -> Result<Self> {
        let mips = if gpu.format_supports_linear_blit(format) { mip_levels(w, h) } else { 1 };
        let tex = Texture::new(
            gpu,
            w,
            h,
            format,
            mips,
            vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;

        let staging = gpu.device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(bytes.len() as u64)
                .usage(vk::BufferUsageFlags::TRANSFER_SRC),
            None,
        )?;
        let req = gpu.device.get_buffer_memory_requirements(staging);
        let staging_mem = gpu.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(gpu.find_memory_type(
                    req.memory_type_bits,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )?),
            None,
        )?;
        gpu.device.bind_buffer_memory(staging, staging_mem, 0)?;
        let ptr = gpu
            .device
            .map_memory(staging_mem, 0, bytes.len() as u64, vk::MemoryMapFlags::empty())?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast::<u8>(), bytes.len());
        gpu.device.unmap_memory(staging_mem);

        let result = gpu.one_shot(|cmd| {
            transition(
                &gpu.device,
                cmd,
                tex.image,
                color_range(),
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::empty(),
                vk::AccessFlags::TRANSFER_WRITE,
            );
            gpu.device.cmd_copy_buffer_to_image(
                cmd,
                staging,
                tex.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::BufferImageCopy::default()
                    .image_subresource(
                        vk::ImageSubresourceLayers::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .layer_count(1),
                    )
                    .image_extent(vk::Extent3D { width: w, height: h, depth: 1 })],
            );
            tex.record_generate_mips(gpu, cmd, vk::ImageLayout::TRANSFER_DST_OPTIMAL);
        });
        if let Err(e) = result {
            if !crate::gpu::still_running(&e) {
                gpu.device.destroy_buffer(staging, None);
                gpu.device.free_memory(staging_mem, None);
            }
            return Err(e.context("upload texture"));
        }
        gpu.device.destroy_buffer(staging, None);
        gpu.device.free_memory(staging_mem, None);
        Ok(tex)
    }

    /// Records the mip chain generation. Precondition: mip 0 is in `mip0_layout` (written);
    /// the other levels may be in any layout (they are overwritten). Postcondition: every
    /// level is SHADER_READ_ONLY_OPTIMAL. With `mips == 1` this only does the final transition.
    ///
    /// # Safety
    /// Vulkan.
    pub unsafe fn record_generate_mips(&self, gpu: &Gpu, cmd: vk::CommandBuffer, mip0_layout: vk::ImageLayout) {
        let dev = &gpu.device;
        transition(
            dev,
            cmd,
            self.image,
            mip_range(0),
            mip0_layout,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::AccessFlags::MEMORY_WRITE,
            vk::AccessFlags::TRANSFER_READ,
        );
        let (mut w, mut h) = (self.w as i32, self.h as i32);
        for level in 1..self.mips {
            let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
            transition(
                dev,
                cmd,
                self.image,
                mip_range(level),
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::empty(),
                vk::AccessFlags::TRANSFER_WRITE,
            );
            let blit = vk::ImageBlit::default()
                .src_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .mip_level(level - 1)
                        .layer_count(1),
                )
                .src_offsets([vk::Offset3D::default(), vk::Offset3D { x: w, y: h, z: 1 }])
                .dst_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .mip_level(level)
                        .layer_count(1),
                )
                .dst_offsets([vk::Offset3D::default(), vk::Offset3D { x: nw, y: nh, z: 1 }]);
            dev.cmd_blit_image(
                cmd,
                self.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[blit],
                vk::Filter::LINEAR,
            );
            transition(
                dev,
                cmd,
                self.image,
                mip_range(level),
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::TRANSFER_READ,
            );
            w = nw;
            h = nh;
        }
        transition(
            dev,
            cmd,
            self.image,
            color_range(),
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::AccessFlags::TRANSFER_READ,
            vk::AccessFlags::SHADER_READ,
        );
    }

    /// # Safety
    /// Vulkan; no pending GPU work may still sample the texture.
    pub unsafe fn destroy(self, gpu: &Gpu) {
        gpu.device.destroy_image_view(self.view, None);
        gpu.device.destroy_image(self.image, None);
        gpu.device.free_memory(self.memory, None);
    }
}

/// A single-level view of mip 0, for use as a framebuffer attachment.
///
/// # Safety
/// Vulkan.
pub unsafe fn mip0_view(gpu: &Gpu, tex: &Texture) -> Result<vk::ImageView> {
    Ok(gpu.device.create_image_view(
        &vk::ImageViewCreateInfo::default()
            .image(tex.image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(tex.format)
            .subresource_range(mip_range(0)),
        None,
    )?)
}
