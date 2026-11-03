use super::*;

/// Every Metal buffer can be bound or copied any way, so every one is made
/// with every use.
pub(super) const BUFFER_USAGE: vk::BufferUsageFlags = vk::BufferUsageFlags::from_raw(
    vk::BufferUsageFlags::STORAGE_BUFFER.as_raw()
        | vk::BufferUsageFlags::UNIFORM_BUFFER.as_raw()
        | vk::BufferUsageFlags::VERTEX_BUFFER.as_raw()
        | vk::BufferUsageFlags::INDEX_BUFFER.as_raw()
        | vk::BufferUsageFlags::INDIRECT_BUFFER.as_raw()
        | vk::BufferUsageFlags::UNIFORM_TEXEL_BUFFER.as_raw()
        | vk::BufferUsageFlags::STORAGE_TEXEL_BUFFER.as_raw()
        | vk::BufferUsageFlags::TRANSFER_SRC.as_raw()
        | vk::BufferUsageFlags::TRANSFER_DST.as_raw(),
);

impl Gpu {
    pub(super) fn memory_type(&self, bits: u32, wanted: vk::MemoryPropertyFlags) -> Option<u32> {
        (0..self.memory.memory_type_count).find(|index| {
            bits & (1 << index) != 0
                && self.memory.memory_types[*index as usize]
                    .property_flags
                    .contains(wanted)
        })
    }

    /// Allocate memory for `requirements`, preferring `wanted` and falling back
    /// to `accepted` when the device has no such heap.
    fn allocate(
        &self,
        requirements: vk::MemoryRequirements,
        wanted: vk::MemoryPropertyFlags,
        accepted: vk::MemoryPropertyFlags,
    ) -> Result<vk::DeviceMemory, String> {
        let index = self
            .memory_type(requirements.memory_type_bits, wanted)
            .or_else(|| self.memory_type(requirements.memory_type_bits, accepted))
            .ok_or("no Vulkan memory type satisfies the allocation")?;
        let info = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(index);
        // SAFETY: a well-formed allocation on this live device.
        unsafe { self.device.allocate_memory(&info, None) }
            .map_err(|error| format!("vkAllocateMemory failed with {error:?}"))
    }

    /// A Metal buffer: host visible so `-contents` can hand the guest a
    /// pointer, and usable by every stage Metal allows a buffer in.
    ///
    /// The memory is ordinary host memory, cached where the device offers it.
    /// A heap which is host-visible *and* device-local looks like the better
    /// answer and is the worse one: on a discrete adapter that is the PCI
    /// aperture, a few hundred megabytes wide and uncached, so a game which
    /// streams its world in overruns it and every byte the guest reads back
    /// through `-contents` crosses the bus. `MTLStorageModeShared` on a Mac
    /// with a discrete GPU is system memory too.
    pub fn create_buffer(&self, length: u64) -> Result<GpuBuffer, String> {
        self.create_buffer_with(length, true, false)
    }

    /// An `MTLStorageModePrivate` buffer, in device memory.
    ///
    /// Only the device reads and writes it, and only the device should hold
    /// it: a compute pass over host memory crosses the bus for every access,
    /// which made an FFT over a few megabytes take forty milliseconds a
    /// dispatch. Its `-contents` is null, as Metal's is.
    pub fn create_private_buffer(&self, length: u64) -> Result<GpuBuffer, String> {
        self.create_buffer_with(length, true, true)
    }

    fn create_buffer_with(
        &self,
        length: u64,
        zeroed: bool,
        private: bool,
    ) -> Result<GpuBuffer, String> {
        let info = vk::BufferCreateInfo::default()
            .size(length.max(4))
            .usage(BUFFER_USAGE)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: a well-formed create info on this live device.
        let buffer = unsafe { self.device.create_buffer(&info, None) }
            .map_err(|error| format!("vkCreateBuffer failed with {error:?}"))?;
        // Nothing is bound to the buffer until the guard is taken, so every
        // failure below destroys it on the way out.
        let buffer = Guard::new(buffer, |buffer| {
            // SAFETY: a buffer of this device's which was never bound.
            unsafe { self.device.destroy_buffer(buffer, None) };
        });
        // SAFETY: the buffer was just created by this device.
        let requirements = unsafe { self.device.get_buffer_memory_requirements(*buffer) };
        let heap = match private {
            true => self.private_buffer_heap(requirements.memory_type_bits)?,
            false => self.buffer_heap(requirements.memory_type_bits)?,
        };
        let allocation = heap.allocate(&self.device, requirements.size, requirements.alignment)?;
        let allocation = Guard::new(allocation, |allocation| {
            heap.free(&self.device, allocation);
        });
        // SAFETY: a fresh, unbound buffer and a range nothing else holds.
        unsafe {
            self.device
                .bind_buffer_memory(*buffer, allocation.memory, allocation.offset())
        }
        .map_err(|error| format!("vkBindBufferMemory failed with {error:?}"))?;
        let (buffer, allocation) = (buffer.take(), allocation.take());
        // A guest which reads a fresh buffer before writing it must not see the
        // bytes of a buffer this block last held. Memory the driver has just
        // handed over is already zero, and during a streaming burst - which is
        // where this matters, because it is gigabytes a second - almost all of
        // it is, so only a reused range is written over.
        let gpu_buffer = GpuBuffer {
            buffer,
            allocation,
            contents: allocation.contents,
            length,
            private,
            placement: None,
        };
        if zeroed && !allocation.pristine {
            if allocation.contents.is_null() {
                // Unmapped, so the device zeroes it ahead of any work which
                // could read it.
                let filled = self.defer(|gpu, batch| {
                    batch.touch_buffer(buffer);
                    // SAFETY: the batch is recording, and the buffer lives
                    // until it has run: see `destroy_buffer`.
                    unsafe {
                        gpu.device.cmd_fill_buffer(
                            batch.command_buffer(),
                            buffer,
                            0,
                            vk::WHOLE_SIZE,
                            0,
                        )
                    };
                    Ok(())
                });
                if let Err(reason) = filled {
                    self.destroy_buffer(gpu_buffer);
                    return Err(reason);
                }
            } else {
                // SAFETY: the mapping covers this whole range.
                crate::stats::timed(crate::stats::Counter::BufferZeroMicros, || unsafe {
                    std::ptr::write_bytes(allocation.contents, 0, length as usize)
                });
            }
        }
        Ok(gpu_buffer)
    }

    /// Copy `bytes` into the start of a buffer the host cannot map, ahead of
    /// whatever is submitted next.
    pub fn upload_to_private(&self, buffer: &GpuBuffer, bytes: &[u8]) -> Result<(), String> {
        let length = (bytes.len() as u64).min(buffer.length);
        if length == 0 {
            return Ok(());
        }
        let target = buffer.buffer;
        self.defer(|gpu, batch| {
            let (staging, offset) = batch.stage(gpu, &bytes[..length as usize], 4)?;
            batch.touch_buffer(target);
            let region = vk::BufferCopy::default()
                .src_offset(offset)
                .dst_offset(0)
                .size(length);
            // SAFETY: the batch is recording; both buffers outlive it.
            unsafe {
                gpu.device
                    .cmd_copy_buffer(batch.command_buffer(), staging, target, &[region])
            };
            Ok(())
        })
    }

    /// A host-visible buffer whose bytes the caller overwrites in full before
    /// the device reads them, so a reused range is not zeroed first.
    pub fn create_staging_buffer(&self, length: u64) -> Result<GpuBuffer, String> {
        self.create_buffer_with(length, false, false)
    }

    /// Destroy a buffer this device made and return its range to the heap.
    ///
    /// The caller must have waited for every submission which referred to it.
    ///
    /// A private buffer a deferred transfer still names is kept until the
    /// device has run that transfer.
    pub fn destroy_buffer(&self, buffer: GpuBuffer) {
        let buffer = match buffer.private {
            true => match self.defer_buffer_destruction(buffer) {
                Some(buffer) => buffer,
                None => return,
            },
            false => buffer,
        };
        self.destroy_buffer_now(buffer);
    }

    pub(super) fn destroy_buffer_now(&self, buffer: GpuBuffer) {
        // SAFETY: the caller guarantees the buffer is idle.
        unsafe { self.device.destroy_buffer(buffer.buffer, None) };
        let heap = match buffer.private {
            true => self.private_buffers.get(),
            false => self.buffers.get(),
        };
        if let Some(heap) = heap {
            heap.free(&self.device, buffer.allocation);
        }
        // Only now that the buffer is gone may its heap's memory go.
        drop(buffer.placement);
    }

    /// What the buffer heaps hold, for `ROSEMARY_METAL_STATS`: blocks, their
    /// size and the free part of it, or `None` while both are empty. Held
    /// minus free is what live buffers occupy, so a heap that grows while
    /// little of it is free is a guest holding buffers, and one that grows
    /// while much is free is fragmentation.
    pub fn heap_report(&self) -> Option<String> {
        let shared = self.buffers.get().map_or((0, 0, 0), Heap::usage);
        let private = self.private_buffers.get().map_or((0, 0, 0), Heap::usage);
        let imported = self
            .host_imported
            .load(std::sync::atomic::Ordering::Relaxed);
        if shared.0 + private.0 == 0 && imported == 0 {
            return None;
        }
        let usage = |(blocks, bytes, free): (usize, u64, u64)| {
            format!("{blocks}/{}MiB/{}MiB", bytes >> 20, free >> 20)
        };
        Some(format!(
            "shared_heap(blocks/held/free)={} private_heap={} host_imported={}MiB",
            usage(shared),
            usage(private),
            imported >> 20,
        ))
    }

    /// The heap every Metal buffer is cut from.
    ///
    /// One heap, not one per requirement mask: every buffer this crate makes is
    /// created with the same usage flags, so every one of them reports the same
    /// admissible memory types.
    fn buffer_heap(&self, bits: u32) -> Result<&Heap, String> {
        if let Some(heap) = self.buffers.get() {
            return Ok(heap);
        }
        let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let index = self
            .memory_type(bits, host | vk::MemoryPropertyFlags::HOST_CACHED)
            .or_else(|| self.memory_type(bits, host))
            .ok_or("no Vulkan memory type is host visible and coherent")?;
        Ok(self.buffers.get_or_init(|| Heap::new(index, true)))
    }

    /// The heap private buffers are cut from: device-local memory, preferring
    /// a type the host cannot see, which on a discrete adapter is video
    /// memory rather than the small mappable window into it. Where there is
    /// none - a device with no device-local type admitting buffers - private
    /// buffers share the ordinary heap's memory type, unmapped.
    fn private_buffer_heap(&self, bits: u32) -> Result<&Heap, String> {
        if let Some(heap) = self.private_buffers.get() {
            return Ok(heap);
        }
        let local = vk::MemoryPropertyFlags::DEVICE_LOCAL;
        let index = (0..self.memory.memory_type_count)
            .find(|index| {
                let flags = self.memory.memory_types[*index as usize].property_flags;
                bits & (1 << index) != 0
                    && flags.contains(local)
                    && !flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
            })
            .or_else(|| self.memory_type(bits, local))
            .or_else(|| self.memory_type(bits, vk::MemoryPropertyFlags::empty()))
            .ok_or("no Vulkan memory type admits a private buffer")?;
        Ok(self.private_buffers.get_or_init(|| Heap::new(index, false)))
    }

    pub fn create_image(&self, description: &ImageDescription) -> Result<GpuImage, String> {
        self.create_image_in(description, None)
    }

    /// The memory an image of this description needs, which is what a heap
    /// quotes for it.
    pub fn image_requirements(
        &self,
        description: &ImageDescription,
    ) -> Result<vk::MemoryRequirements, String> {
        let (image, _) = self.new_image(description)?;
        // SAFETY: the image was just created by this device.
        let requirements = unsafe { self.device.get_image_memory_requirements(image) };
        // SAFETY: never bound or used.
        unsafe { self.device.destroy_image(image, None) };
        Ok(requirements)
    }

    /// An unbound image for `description`, and the usage it was made with.
    fn new_image(
        &self,
        description: &ImageDescription,
    ) -> Result<(vk::Image, vk::ImageUsageFlags), String> {
        let samples = sample_count(description.samples).ok_or_else(|| {
            format!(
                "texture has invalid Metal sample count {}",
                description.samples
            )
        })?;
        let mut usage = vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST;
        if description.usage & USAGE_SHADER_READ != 0 {
            usage |= vk::ImageUsageFlags::SAMPLED;
        }
        if description.usage & USAGE_SHADER_WRITE != 0 {
            usage |= vk::ImageUsageFlags::STORAGE;
        }
        if description.usage & USAGE_RENDER_TARGET != 0 || description.usage == 0 {
            usage |= match aspect_of(description.format) {
                aspect if aspect.contains(vk::ImageAspectFlags::DEPTH) => {
                    vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
                }
                _ => vk::ImageUsageFlags::COLOR_ATTACHMENT,
            };
        }
        if description.usage == 0 {
            usage |= vk::ImageUsageFlags::SAMPLED;
        }
        let mut flags = vk::ImageCreateFlags::empty();
        if description.cube {
            flags |= vk::ImageCreateFlags::CUBE_COMPATIBLE;
        }
        // A texture view of another format needs a mutable-format image.
        // `PixelFormatView` asks for any compatible format; without it Metal
        // still allows the sRGB twin, which a format list names so the driver
        // can keep its compression.
        let twin = [
            description.format,
            srgb_twin(description.format).unwrap_or_default(),
        ];
        let mut format_list = vk::ImageFormatListCreateInfo::default().view_formats(&twin);
        let any_format = description.usage & USAGE_PIXEL_FORMAT_VIEW != 0;
        if any_format || twin[1] != vk::Format::UNDEFINED {
            flags |= vk::ImageCreateFlags::MUTABLE_FORMAT;
        }
        // A storage image whose twin is sRGB needs the extended usage flag:
        // the sRGB format is never a storage format.
        if usage.contains(vk::ImageUsageFlags::STORAGE)
            && (any_format || is_srgb(twin[1]) || is_srgb(twin[0]))
        {
            flags |= vk::ImageCreateFlags::EXTENDED_USAGE;
        }
        let mut info = vk::ImageCreateInfo::default()
            .flags(flags)
            .image_type(description.image_type)
            .format(description.format)
            .extent(vk::Extent3D {
                width: description.width.max(1),
                height: description.height.max(1),
                depth: description.depth.max(1),
            })
            .mip_levels(description.levels.max(1))
            .array_layers(description.layers.max(1))
            .samples(samples)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        if !any_format && twin[1] != vk::Format::UNDEFINED {
            info = info.push_next(&mut format_list);
        }
        // SAFETY: a well-formed create info on this live device.
        let image = unsafe { self.device.create_image(&info, None) }
            .map_err(|error| format!("vkCreateImage failed with {error:?}"))?;
        Ok((image, usage))
    }

    /// An image in memory of its own, or `placed` at an offset into a
    /// placement heap's.
    pub fn create_image_in(
        &self,
        description: &ImageDescription,
        placed: Option<(Arc<Placement>, u64)>,
    ) -> Result<GpuImage, String> {
        let samples = sample_count(description.samples).ok_or_else(|| {
            format!(
                "texture has invalid Metal sample count {}",
                description.samples
            )
        })?;
        let (image, usage) = self.new_image(description)?;
        // Everything below can fail; the guards unwind what has been made so
        // far, and only the last statement of the function takes them.
        let image = Guard::new(image, |image| {
            // SAFETY: an image of this device's, referred to by nothing.
            unsafe { self.device.destroy_image(image, None) };
        });
        // SAFETY: the image was just created by this device.
        let requirements = unsafe { self.device.get_image_memory_requirements(*image) };
        let (memory, placement) = match placed {
            None => {
                let memory = self.allocate(
                    requirements,
                    vk::MemoryPropertyFlags::DEVICE_LOCAL,
                    vk::MemoryPropertyFlags::empty(),
                )?;
                let memory = Guard::new(memory, |memory| {
                    // SAFETY: memory of this device's, bound to nothing which
                    // survives.
                    unsafe { self.device.free_memory(memory, None) };
                });
                // SAFETY: fresh, unbound image and memory from this device.
                unsafe { self.device.bind_image_memory(*image, *memory, 0) }
                    .map_err(|error| format!("vkBindImageMemory failed with {error:?}"))?;
                (Some(memory), None)
            }
            Some((placement, offset)) => {
                placement.admits(&requirements, offset)?;
                // SAFETY: a fresh, unbound image, and a range of the heap's
                // memory `admits` checked.
                unsafe {
                    self.device
                        .bind_image_memory(*image, placement.memory, offset)
                }
                .map_err(|error| format!("vkBindImageMemory failed with {error:?}"))?;
                (None, Some(placement))
            }
        };
        let aspect = aspect_of(description.format);
        let levels = description.levels.max(1);
        let layers = description.layers.max(1);
        let view = self.create_view(*image, description, usage)?;
        let view = Guard::new(view, |view| {
            // SAFETY: a view of this device's which nothing has used.
            unsafe { self.device.destroy_image_view(view, None) };
        });
        // Every texture stays in `GENERAL` from creation: see `render_pass`.
        // Nothing below may fail once the transition is recorded, because the
        // batch then names the image.
        self.defer(|gpu, batch| {
            batch.touch(*image);
            let barrier = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(*image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: aspect,
                    base_mip_level: 0,
                    level_count: levels,
                    base_array_layer: 0,
                    layer_count: layers,
                })
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE);
            // SAFETY: recording into the batch's command buffer, which the
            // batch lock gives this thread alone.
            unsafe {
                gpu.device.cmd_pipeline_barrier(
                    batch.command_buffer(),
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[barrier],
                )
            };
            Ok(())
        })?;
        let view = view.take();
        Ok(GpuImage {
            image: image.take(),
            memory: memory.map(Guard::take),
            placement,
            view,
            format: description.format,
            aspect: aspect_of(description.format),
            width: description.width.max(1),
            height: description.height.max(1),
            depth: description.depth.max(1),
            levels: description.levels.max(1),
            layers: description.layers.max(1),
            samples,
            base_level: 0,
            base_layer: 0,
            usage,
        })
    }

    fn create_view(
        &self,
        image: vk::Image,
        description: &ImageDescription,
        usage: vk::ImageUsageFlags,
    ) -> Result<vk::ImageView, String> {
        let view_type = description.view_type;
        let info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(view_type)
            .format(description.format)
            .components(view_components(description.components, usage))
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: single_aspect(aspect_of(description.format)),
                base_mip_level: 0,
                level_count: description.levels.max(1),
                base_array_layer: 0,
                layer_count: description.layers.max(1),
            });
        // SAFETY: the image belongs to this device and outlives the view.
        unsafe { self.device.create_image_view(&info, None) }
            .map_err(|error| format!("vkCreateImageView failed with {error:?}"))
    }

    /// A texture view: `source`'s image seen through another format, view
    /// type, or range of its levels and layers. The image is still `source`'s,
    /// and so is its memory; only the view is this image's own.
    #[allow(clippy::too_many_arguments)]
    pub fn create_texture_view(
        &self,
        source: &GpuImage,
        format: vk::Format,
        view_type: vk::ImageViewType,
        levels: std::ops::Range<u32>,
        layers: std::ops::Range<u32>,
        components: vk::ComponentMapping,
        view_aspect: vk::ImageAspectFlags,
    ) -> Result<GpuImage, String> {
        let aspect = aspect_of(format);
        // An sRGB format is never a storage format, so a view of one cannot
        // carry the storage usage its image may have.
        let mut usage = source.usage;
        if is_srgb(format) {
            usage &= !vk::ImageUsageFlags::STORAGE;
        }
        let mut view_usage = vk::ImageViewUsageCreateInfo::default().usage(usage);
        let info = vk::ImageViewCreateInfo::default()
            .image(source.image)
            .view_type(view_type)
            .format(format)
            .components(view_components(components, usage))
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: view_aspect,
                base_mip_level: levels.start,
                level_count: levels.len().max(1) as u32,
                base_array_layer: layers.start,
                layer_count: layers.len().max(1) as u32,
            })
            .push_next(&mut view_usage);
        // SAFETY: the image belongs to this device and outlives the view,
        // because the view texture retains the texture that owns it.
        let view = unsafe { self.device.create_image_view(&info, None) }
            .map_err(|error| format!("vkCreateImageView failed with {error:?}"))?;
        Ok(GpuImage {
            image: source.image,
            memory: None,
            placement: None,
            view,
            format,
            aspect,
            width: source.width,
            height: source.height,
            depth: source.depth,
            levels: levels.len().max(1) as u32,
            layers: layers.len().max(1) as u32,
            samples: source.samples,
            base_level: levels.start,
            base_layer: layers.start,
            usage,
        })
    }

    /// Destroy only the view of a texture view; the image is its parent's.
    pub fn destroy_view(&self, image: GpuImage) {
        let image = GpuImage {
            image: vk::Image::null(),
            memory: None,
            placement: None,
            ..image
        };
        if let Some(image) = self.defer_destruction(image) {
            self.destroy_image_now(image);
        }
    }

    /// A view of one mip level of one array slice, which is what a framebuffer
    /// attachment must be.
    ///
    /// A texture's own view covers every level and every layer, and is what a
    /// shader samples. Metal names a level and a slice on a render pass
    /// attachment, and a game which renders a blur pyramid names a different
    /// one in every pass.
    pub fn create_attachment_view(
        &self,
        image: &GpuImage,
        level: u32,
        slice: u32,
    ) -> Result<vk::ImageView, String> {
        let info = vk::ImageViewCreateInfo::default()
            .image(image.image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(image.format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: image.aspect,
                base_mip_level: level
                    .clamp(image.base_level, image.base_level + image.levels.max(1) - 1),
                level_count: 1,
                base_array_layer: slice
                    .clamp(image.base_layer, image.base_layer + image.layers.max(1) - 1),
                layer_count: 1,
            });
        // SAFETY: the image belongs to this device and outlives the view.
        unsafe { self.device.create_image_view(&info, None) }
            .map_err(|error| format!("vkCreateImageView failed with {error:?}"))
    }

    /// Destroy an image this device made. The caller must have waited for
    /// every submission which referred to it; a deferred transfer which
    /// still names it keeps it until the device has run that transfer.
    pub fn destroy_image(&self, image: GpuImage) {
        if let Some(image) = self.defer_destruction(image) {
            self.destroy_image_now(image);
        }
    }

    pub(super) fn destroy_image_now(&self, image: GpuImage) {
        // SAFETY: the caller guarantees the image is idle.
        unsafe {
            self.device.destroy_image_view(image.view, None);
            self.device.destroy_image(image.image, None);
            if let Some(memory) = image.memory {
                self.device.free_memory(memory, None);
            }
        }
        // Only now that the image is gone may its heap's memory go.
        drop(image.placement);
    }

    pub fn create_shader_module(
        &self,
        words: &[u32],
        entry: &str,
    ) -> Result<vk::ShaderModule, String> {
        dump_spirv(entry, words);
        let info = vk::ShaderModuleCreateInfo::default().code(words);
        // SAFETY: `words` lives across the call and is SPIR-V this process emitted.
        unsafe { self.device.create_shader_module(&info, None) }
            .map_err(|error| format!("vkCreateShaderModule failed with {error:?}"))
    }

    /// One descriptor-set layout per distinct binding shape, shared across the
    /// pipelines which declare it.
    pub fn descriptor_set_layout(
        &self,
        bindings: &[(u32, vk::DescriptorType)],
        stages: vk::ShaderStageFlags,
        push: bool,
    ) -> Result<vk::DescriptorSetLayout, String> {
        let mut key = bindings.to_vec();
        key.sort_unstable();
        key.dedup();
        let mut cache = self.layouts.lock().expect("descriptor layout cache");
        let key = (key, stages, push);
        if let Some(layout) = cache.get(&key) {
            return Ok(*layout);
        }
        let entries = key
            .0
            .iter()
            .map(|(binding, kind)| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(*binding)
                    .descriptor_type(*kind)
                    .descriptor_count(1)
                    .stage_flags(stages)
            })
            .collect::<Vec<_>>();
        let flags = if push {
            vk::DescriptorSetLayoutCreateFlags::PUSH_DESCRIPTOR_KHR
        } else {
            vk::DescriptorSetLayoutCreateFlags::empty()
        };
        let info = vk::DescriptorSetLayoutCreateInfo::default()
            .flags(flags)
            .bindings(&entries);
        // SAFETY: the bindings live across the call.
        let layout = unsafe { self.device.create_descriptor_set_layout(&info, None) }
            .map_err(|error| format!("vkCreateDescriptorSetLayout failed with {error:?}"))?;
        cache.insert(key, layout);
        Ok(layout)
    }

    /// Every attachment is used in `GENERAL` layout.
    ///
    /// Metal has no image layouts: a texture is a texture, and a game rebinds
    /// one as an attachment, a sampled texture and a blit source in any order.
    /// Reconstructing Vulkan's optimal layouts would need per-subresource
    /// tracking across command buffers and threads. `GENERAL` is valid for
    /// every use and costs bandwidth on some hardware, not correctness.
    ///
    /// Using the optimal layout inside the pass and `GENERAL` outside it, which
    /// needs no tracking at all, was measured and is much worse: the home page
    /// went from 6.65 ms of device time a frame to 12.0 ms on a GTX 1660 SUPER,
    /// because each of the twenty-five passes in a frame then decompresses its
    /// attachments on the way out.
    pub fn render_pass(&self, key: &RenderPassKey) -> Result<vk::RenderPass, String> {
        let mut cache = self.render_passes.lock().expect("render pass cache");
        if let Some(pass) = cache.get(key) {
            return Ok(*pass);
        }
        let mut attachments = Vec::new();
        let mut colors = Vec::new();
        for (format, load, store) in &key.colors {
            colors.push(
                vk::AttachmentReference::default()
                    .attachment(attachments.len() as u32)
                    .layout(vk::ImageLayout::GENERAL),
            );
            attachments.push(
                vk::AttachmentDescription::default()
                    .format(*format)
                    .samples(key.samples)
                    .load_op(*load)
                    .store_op(*store)
                    .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                    .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                    .initial_layout(vk::ImageLayout::GENERAL)
                    .final_layout(vk::ImageLayout::GENERAL),
            );
        }
        let depth = key.depth.map(|(format, load, store)| {
            let (stencil_load, stencil_store) = key.stencil.unwrap_or((
                vk::AttachmentLoadOp::DONT_CARE,
                vk::AttachmentStoreOp::DONT_CARE,
            ));
            let reference = vk::AttachmentReference::default()
                .attachment(attachments.len() as u32)
                .layout(vk::ImageLayout::GENERAL);
            attachments.push(
                vk::AttachmentDescription::default()
                    .format(format)
                    .samples(key.samples)
                    .load_op(load)
                    .store_op(store)
                    .stencil_load_op(stencil_load)
                    .stencil_store_op(stencil_store)
                    .initial_layout(vk::ImageLayout::GENERAL)
                    .final_layout(vk::ImageLayout::GENERAL),
            );
            reference
        });
        // Metal resolves a multisampled attachment by naming the destination
        // on the pass descriptor; Vulkan does it with a second attachment the
        // subpass names positionally, so a colour without a resolve still needs
        // an entry. The resolve attachments follow the depth one, which is the
        // order `begin_render_pass` builds the framebuffer's views in.
        let mut resolves = Vec::new();
        if key.color_resolves.iter().any(Option::is_some) {
            for format in &key.color_resolves {
                let Some(format) = format else {
                    resolves
                        .push(vk::AttachmentReference::default().attachment(vk::ATTACHMENT_UNUSED));
                    continue;
                };
                resolves.push(
                    vk::AttachmentReference::default()
                        .attachment(attachments.len() as u32)
                        .layout(vk::ImageLayout::GENERAL),
                );
                attachments.push(
                    vk::AttachmentDescription::default()
                        .format(*format)
                        .samples(vk::SampleCountFlags::TYPE_1)
                        .load_op(vk::AttachmentLoadOp::DONT_CARE)
                        .store_op(vk::AttachmentStoreOp::STORE)
                        .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                        .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                        .initial_layout(vk::ImageLayout::GENERAL)
                        .final_layout(vk::ImageLayout::GENERAL),
                );
            }
        }
        // Vulkan's depth/stencil resolve destination is one further
        // attachment, after the colour resolves, and the subpass names it
        // through an extension structure `vkCreateRenderPass` cannot carry.
        let depth_resolve = key.depth_resolve.map(|resolve| {
            let reference = vk::AttachmentReference::default()
                .attachment(attachments.len() as u32)
                .layout(vk::ImageLayout::GENERAL);
            attachments.push(
                vk::AttachmentDescription::default()
                    .format(resolve.format)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .load_op(vk::AttachmentLoadOp::DONT_CARE)
                    .store_op(vk::AttachmentStoreOp::STORE)
                    .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                    .stencil_store_op(vk::AttachmentStoreOp::STORE)
                    .initial_layout(vk::ImageLayout::GENERAL)
                    .final_layout(vk::ImageLayout::GENERAL),
            );
            (resolve, reference)
        });
        let pass = match depth_resolve {
            Some((resolve, reference)) => self.create_depth_resolving_pass(
                &attachments,
                &colors,
                &resolves,
                depth,
                resolve,
                reference,
            )?,
            None => {
                let mut subpass = vk::SubpassDescription::default()
                    .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
                    .color_attachments(&colors);
                if !resolves.is_empty() {
                    subpass = subpass.resolve_attachments(&resolves);
                }
                if let Some(depth) = &depth {
                    subpass = subpass.depth_stencil_attachment(depth);
                }
                let subpasses = [subpass];
                let info = vk::RenderPassCreateInfo::default()
                    .attachments(&attachments)
                    .subpasses(&subpasses);
                // SAFETY: every borrowed slice lives across the call.
                unsafe { self.device.create_render_pass(&info, None) }
                    .map_err(|error| format!("vkCreateRenderPass failed with {error:?}"))?
            }
        };
        cache.insert(key.clone(), pass);
        Ok(pass)
    }

    /// The same subpass through `vkCreateRenderPass2`, which is the only form
    /// that can name a depth/stencil resolve attachment.
    ///
    /// The version-two structures carry exactly the version-one fields plus a
    /// `pNext`, so the descriptions and references are converted rather than
    /// described twice: one shape of render pass, whichever call creates it.
    fn create_depth_resolving_pass(
        &self,
        attachments: &[vk::AttachmentDescription],
        colors: &[vk::AttachmentReference],
        resolves: &[vk::AttachmentReference],
        depth: Option<vk::AttachmentReference>,
        resolve: DepthResolve,
        reference: vk::AttachmentReference,
    ) -> Result<vk::RenderPass, String> {
        let creation = self
            .depth_stencil_resolve
            .as_ref()
            .ok_or("a render pass resolves depth on a device without depth/stencil resolve")?;
        let attachments = attachments.iter().map(description2).collect::<Vec<_>>();
        let colors = colors.iter().map(reference2).collect::<Vec<_>>();
        let resolves = resolves.iter().map(reference2).collect::<Vec<_>>();
        let depth = depth.map(|depth| reference2(&depth));
        let destination = reference2(&reference);
        let mut resolve_state = vk::SubpassDescriptionDepthStencilResolve::default()
            .depth_resolve_mode(resolve.depth_mode)
            .stencil_resolve_mode(resolve.stencil_mode)
            .depth_stencil_resolve_attachment(&destination);
        let mut subpass = vk::SubpassDescription2::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&colors);
        if !resolves.is_empty() {
            subpass = subpass.resolve_attachments(&resolves);
        }
        if let Some(depth) = &depth {
            subpass = subpass.depth_stencil_attachment(depth);
        }
        subpass = subpass.push_next(&mut resolve_state);
        let subpasses = [subpass];
        let info = vk::RenderPassCreateInfo2::default()
            .attachments(&attachments)
            .subpasses(&subpasses);
        // SAFETY: every borrowed slice and the extension structure live across
        // the call, and the entry point is the device's own or the one loaded
        // for `VK_KHR_create_renderpass2`.
        unsafe {
            match &creation.renderpass2 {
                Some(api) => api.create_render_pass2(&info, None),
                None => self.device.create_render_pass2(&info, None),
            }
        }
        .map_err(|error| format!("vkCreateRenderPass2 failed with {error:?}"))
    }

    pub fn create_framebuffer(
        &self,
        pass: vk::RenderPass,
        views: &[vk::ImageView],
        width: u32,
        height: u32,
        layers: u32,
    ) -> Result<vk::Framebuffer, String> {
        let info = vk::FramebufferCreateInfo::default()
            .render_pass(pass)
            .attachments(views)
            .width(width.max(1))
            .height(height.max(1))
            .layers(layers.max(1));
        // SAFETY: the views outlive the framebuffer's use in this frame.
        unsafe { self.device.create_framebuffer(&info, None) }
            .map_err(|error| format!("vkCreateFramebuffer failed with {error:?}"))
    }
}

/// One attachment description in the form `vkCreateRenderPass2` takes.
fn description2(description: &vk::AttachmentDescription) -> vk::AttachmentDescription2<'static> {
    vk::AttachmentDescription2::default()
        .flags(description.flags)
        .format(description.format)
        .samples(description.samples)
        .load_op(description.load_op)
        .store_op(description.store_op)
        .stencil_load_op(description.stencil_load_op)
        .stencil_store_op(description.stencil_store_op)
        .initial_layout(description.initial_layout)
        .final_layout(description.final_layout)
}

/// One attachment reference in the same form. `aspectMask` is read only for an
/// input attachment, which this runtime never declares, so it stays empty.
fn reference2(reference: &vk::AttachmentReference) -> vk::AttachmentReference2<'static> {
    vk::AttachmentReference2::default()
        .attachment(reference.attachment)
        .layout(reference.layout)
}

/// A storage or attachment view must not swizzle, so an image used that way
/// keeps the identity and a shader writing it sees the stored channel.
fn view_components(
    components: vk::ComponentMapping,
    usage: vk::ImageUsageFlags,
) -> vk::ComponentMapping {
    let identity = vk::ComponentMapping::default();
    let swizzled = [components.r, components.g, components.b, components.a]
        .iter()
        .any(|swizzle| *swizzle != vk::ComponentSwizzle::IDENTITY);
    if swizzled && usage.contains(vk::ImageUsageFlags::STORAGE) {
        super::trace_once("an alpha-only texture is also a storage image, so it is not swizzled");
        return identity;
    }
    components
}

/// The aspect of a texture's own view, which is what a shader reads: depth
/// alone of a depth/stencil image, because a sampled view may name only one.
fn single_aspect(aspect: vk::ImageAspectFlags) -> vk::ImageAspectFlags {
    if aspect.contains(vk::ImageAspectFlags::DEPTH) {
        vk::ImageAspectFlags::DEPTH
    } else {
        aspect
    }
}
