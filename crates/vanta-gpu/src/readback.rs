//! Reading an image the program rendered back into memory.

use anyhow::Result;
use ash::vk;

use crate::buffer::MappedBuffer;
use crate::gpu::Gpu;
use crate::util::color_range;

/// Copies a whole image, just rendered, into memory: tightly packed rows of `bpp` bytes a pixel.
///
/// `layout` is the layout the render left it in; it is put back afterwards. The barriers are the
/// point of this function: the render's writes are made visible to the copy, and the copy's
/// writes to the processor, before a byte is read. Blocks until the copy is done.
///
/// # Safety
/// Vulkan. `image` must be a colour image of `extent` in `layout`, not in use by the GPU.
pub unsafe fn read_image(gpu: &Gpu, image: vk::Image, layout: vk::ImageLayout, extent: vk::Extent2D, bpp: u32) -> Result<Vec<u8>> {
    let size = extent.width as u64 * extent.height as u64 * bpp as u64;
    let buf = MappedBuffer::new(gpu, size, vk::BufferUsageFlags::TRANSFER_DST)?;
    let dev = &gpu.device;
    let image_barrier = |old: vk::ImageLayout, new: vk::ImageLayout, src: vk::AccessFlags, dst: vk::AccessFlags| {
        vk::ImageMemoryBarrier::default()
            .image(image)
            .subresource_range(color_range())
            .old_layout(old)
            .new_layout(new)
            .src_access_mask(src)
            .dst_access_mask(dst)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
    };
    let copied = gpu.one_shot(|cmd| {
        dev.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[image_barrier(layout, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::AccessFlags::COLOR_ATTACHMENT_WRITE, vk::AccessFlags::TRANSFER_READ)],
        );
        dev.cmd_copy_image_to_buffer(
            cmd,
            image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            buf.buffer,
            &[vk::BufferImageCopy::default()
                .buffer_row_length(extent.width)
                .buffer_image_height(extent.height)
                .image_subresource(vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1))
                .image_extent(vk::Extent3D { width: extent.width, height: extent.height, depth: 1 })],
        );
        let to_host = vk::BufferMemoryBarrier::default()
            .buffer(buf.buffer)
            .size(vk::WHOLE_SIZE)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::HOST_READ)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED);
        dev.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::HOST | vk::PipelineStageFlags::ALL_COMMANDS,
            vk::DependencyFlags::empty(),
            &[],
            &[to_host],
            &[image_barrier(vk::ImageLayout::TRANSFER_SRC_OPTIMAL, layout, vk::AccessFlags::TRANSFER_READ, vk::AccessFlags::empty())],
        );
    });
    if let Err(e) = copied {
        if crate::gpu::still_running(&e) {
            // The copy may still land in it; leaked rather than freed under the GPU.
            std::mem::forget(buf);
        } else {
            buf.destroy(gpu);
        }
        return Err(e.context("read image back"));
    }
    let out = std::slice::from_raw_parts(buf.ptr.cast::<u8>(), size as usize).to_vec();
    buf.destroy(gpu);
    Ok(out)
}
