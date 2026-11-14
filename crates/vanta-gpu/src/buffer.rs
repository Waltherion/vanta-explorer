//! Host-visible, coherent, permanently mapped buffers for uniforms and small vertex data.

use anyhow::Result;
use ash::vk;

use crate::gpu::Gpu;

pub struct MappedBuffer {
    pub buffer: vk::Buffer,
    pub memory: vk::DeviceMemory,
    pub ptr: *mut u8,
    pub size: u64,
}

impl MappedBuffer {
    /// # Safety
    /// Vulkan.
    pub unsafe fn new(gpu: &Gpu, size: u64, usage: vk::BufferUsageFlags) -> Result<Self> {
        let buffer = gpu.device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(size)
                .usage(usage)
                .sharing_mode(vk::SharingMode::EXCLUSIVE),
            None,
        )?;
        let req = gpu.device.get_buffer_memory_requirements(buffer);
        let memory = gpu.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(gpu.find_memory_type(
                    req.memory_type_bits,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )?),
            None,
        )?;
        gpu.device.bind_buffer_memory(buffer, memory, 0)?;
        let ptr = gpu
            .device
            .map_memory(memory, 0, size, vk::MemoryMapFlags::empty())?
            .cast();
        Ok(MappedBuffer { buffer, memory, ptr, size })
    }

    /// Copies `bytes` to `offset`. Panics if it does not fit; the sizes are compile-time facts.
    pub fn write(&self, offset: usize, bytes: &[u8]) {
        assert!(offset + bytes.len() <= self.size as usize, "MappedBuffer::write out of range");
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr.add(offset), bytes.len()) };
    }

    pub fn write_f32(&self, offset: usize, values: &[f32]) {
        let bytes = unsafe { std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), values.len() * 4) };
        self.write(offset, bytes);
    }

    /// # Safety
    /// Vulkan; the buffer must not be in use by pending GPU work.
    pub unsafe fn destroy(self, gpu: &Gpu) {
        gpu.device.unmap_memory(self.memory);
        gpu.device.destroy_buffer(self.buffer, None);
        gpu.device.free_memory(self.memory, None);
    }
}
