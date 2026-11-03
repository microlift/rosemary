//! Placement heaps: one block of device memory the guest lays resources out
//! in itself, at offsets it chooses and may overlap.

use std::sync::Arc;

use super::memory::BUFFER_USAGE;
use super::*;

/// A placement heap's memory. Every resource placed in it holds a reference,
/// so the memory outlives the heap until the last of them is destroyed -
/// which can be after the guest released it, behind work still in flight.
pub struct Placement {
    gpu: &'static Gpu,
    pub(super) memory: vk::DeviceMemory,
    size: u64,
    memory_type: u32,
}

impl Placement {
    /// Device memory of `size` bytes that buffers and images can both be
    /// bound into, as a Metal heap holds either.
    pub fn new(gpu: &'static Gpu, size: u64) -> Result<Arc<Self>, String> {
        let bits = gpu.buffer_memory_bits()? & gpu.image_memory_bits()?;
        let memory_type = gpu
            .memory_type(bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)
            .ok_or("no device-local Vulkan memory type holds both buffers and images")?;
        let info = vk::MemoryAllocateInfo::default()
            .allocation_size(size)
            .memory_type_index(memory_type);
        // SAFETY: a well-formed allocation on this live device.
        let memory = unsafe { gpu.device.allocate_memory(&info, None) }
            .map_err(|error| format!("vkAllocateMemory failed with {error:?}"))?;
        Ok(Arc::new(Self {
            gpu,
            memory,
            size,
            memory_type,
        }))
    }

    /// Whether a resource with these requirements can be bound at `offset`.
    pub(super) fn admits(
        &self,
        requirements: &vk::MemoryRequirements,
        offset: u64,
    ) -> Result<(), String> {
        if requirements.memory_type_bits & (1 << self.memory_type) == 0 {
            return Err("the resource cannot live in the heap's Vulkan memory type".into());
        }
        if !offset.is_multiple_of(requirements.alignment.max(1)) {
            return Err(format!(
                "offset {offset} is not aligned to {}",
                requirements.alignment
            ));
        }
        if offset
            .checked_add(requirements.size)
            .is_none_or(|end| end > self.size)
        {
            return Err(format!(
                "{} bytes at offset {offset} do not fit a heap of {}",
                requirements.size, self.size
            ));
        }
        Ok(())
    }
}

impl Drop for Placement {
    fn drop(&mut self) {
        // SAFETY: the heap and every resource bound into it are gone, and
        // each waited for the device before letting its reference go.
        unsafe { self.gpu.device.free_memory(self.memory, None) };
    }
}

impl Gpu {
    /// The memory types a Metal buffer can be bound to.
    fn buffer_memory_bits(&self) -> Result<u32, String> {
        Ok(self.buffer_requirements(1)?.memory_type_bits)
    }

    /// The memory types an ordinary colour texture can be bound to; other
    /// textures are checked as they are placed.
    fn image_memory_bits(&self) -> Result<u32, String> {
        let description = ImageDescription {
            image_type: vk::ImageType::TYPE_2D,
            view_type: vk::ImageViewType::TYPE_2D,
            format: vk::Format::R8G8B8A8_UNORM,
            width: 1,
            height: 1,
            depth: 1,
            levels: 1,
            layers: 1,
            samples: 1,
            usage: USAGE_SHADER_READ | USAGE_RENDER_TARGET,
            cube: false,
            components: vk::ComponentMapping::default(),
        };
        Ok(self.image_requirements(&description)?.memory_type_bits)
    }

    /// The memory a Metal buffer of `length` bytes needs.
    pub fn buffer_requirements(&self, length: u64) -> Result<vk::MemoryRequirements, String> {
        let buffer = self.new_buffer(length)?;
        // SAFETY: the buffer was just created by this device.
        let requirements = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        // SAFETY: never bound or used.
        unsafe { self.device.destroy_buffer(buffer, None) };
        Ok(requirements)
    }

    fn new_buffer(&self, length: u64) -> Result<vk::Buffer, String> {
        let info = vk::BufferCreateInfo::default()
            .size(length.max(4))
            .usage(BUFFER_USAGE)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: a well-formed create info on this live device.
        unsafe { self.device.create_buffer(&info, None) }
            .map_err(|error| format!("vkCreateBuffer failed with {error:?}"))
    }

    /// A private buffer bound at `offset` into a placement heap's memory.
    ///
    /// Its bytes are whatever the heap held there, as Metal's are.
    pub fn create_placed_buffer(
        &self,
        length: u64,
        placement: Arc<Placement>,
        offset: u64,
    ) -> Result<GpuBuffer, String> {
        let buffer = self.new_buffer(length)?;
        let buffer = Guard::new(buffer, |buffer| {
            // SAFETY: a buffer of this device's which was never bound.
            unsafe { self.device.destroy_buffer(buffer, None) };
        });
        // SAFETY: the buffer was just created by this device.
        let requirements = unsafe { self.device.get_buffer_memory_requirements(*buffer) };
        placement.admits(&requirements, offset)?;
        // SAFETY: a fresh, unbound buffer, and a range `admits` checked.
        unsafe {
            self.device
                .bind_buffer_memory(*buffer, placement.memory, offset)
        }
        .map_err(|error| format!("vkBindBufferMemory failed with {error:?}"))?;
        Ok(GpuBuffer {
            buffer: buffer.take(),
            length,
            private: true,
            placement: Some(placement),
            ..GpuBuffer::none()
        })
    }
}
