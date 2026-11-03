use super::*;

pub(super) unsafe extern "C" fn new_command_queue(receiver: Id, _sel: Sel) -> Id {
    let Some(gpu) = gpu_of(receiver) else {
        return std::ptr::null_mut();
    };
    encoder::new_command_queue(gpu, receiver)
}

pub(super) unsafe extern "C" fn new_command_queue_with_count(
    receiver: Id,
    sel: Sel,
    _count: u64,
) -> Id {
    // SAFETY: forwards to the unbounded form; nothing here bounds the number
    // of command buffers in flight.
    unsafe { new_command_queue(receiver, sel) }
}

pub(super) unsafe extern "C" fn new_buffer(
    receiver: Id,
    _sel: Sel,
    length: usize,
    options: u64,
) -> Id {
    let Some(gpu) = gpu_of(receiver) else {
        return std::ptr::null_mut();
    };
    resource::new_buffer(gpu, length, options)
}

pub(super) unsafe extern "C" fn new_buffer_with_bytes(
    receiver: Id,
    _sel: Sel,
    bytes: *const u8,
    length: usize,
    options: u64,
) -> Id {
    let Some(gpu) = gpu_of(receiver) else {
        return std::ptr::null_mut();
    };
    let buffer = resource::new_buffer(gpu, length, options);
    if !buffer.is_null() && !bytes.is_null() {
        // SAFETY: a buffer this call just made.
        let inner = unsafe { &object::body::<resource::Buffer>(buffer).inner };
        if inner.contents.is_null() {
            // A private buffer: the device copies the bytes in.
            // SAFETY: `length` readable bytes the guest promised.
            let bytes = unsafe { std::slice::from_raw_parts(bytes, length) };
            if let Err(reason) = gpu.upload_to_private(inner, bytes) {
                gpu::trace(format!("newBufferWithBytes: upload failed: {reason}"));
            }
        } else {
            // SAFETY: the buffer is mapped over `length` bytes, and `length`
            // readable bytes the guest promised.
            unsafe { std::ptr::copy_nonoverlapping(bytes, inner.contents, length) };
        }
    }
    buffer
}

pub(super) unsafe extern "C" fn new_texture(receiver: Id, _sel: Sel, descriptor: Id) -> Id {
    let Some(gpu) = gpu_of(receiver) else {
        return std::ptr::null_mut();
    };
    if descriptor.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `MTLTextureDescriptor` is this crate's class.
    let descriptor = *unsafe { object::body::<resource::TextureDescriptor>(descriptor) };
    resource::new_texture(gpu, descriptor)
}

pub(super) unsafe extern "C" fn new_sampler_state(receiver: Id, _sel: Sel, descriptor: Id) -> Id {
    let Some(gpu) = gpu_of(receiver) else {
        return std::ptr::null_mut();
    };
    if descriptor.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `MTLSamplerDescriptor` is this crate's class.
    let descriptor = *unsafe { object::body::<resource::SamplerDescriptor>(descriptor) };
    resource::new_sampler(gpu, &descriptor)
}

pub(super) unsafe extern "C" fn new_heap(receiver: Id, _sel: Sel, descriptor: Id) -> Id {
    let Some(gpu) = gpu_of(receiver) else {
        return std::ptr::null_mut();
    };
    resource::new_heap(gpu, receiver, descriptor)
}

pub(super) unsafe extern "C" fn new_depth_stencil_state(
    _receiver: Id,
    _sel: Sel,
    descriptor: Id,
) -> Id {
    if descriptor.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `MTLDepthStencilDescriptor` is this crate's class.
    let state = *unsafe { object::body::<resource::DepthStencilState>(descriptor) };
    resource::new_depth_stencil_state(state)
}

pub(super) unsafe extern "C" fn new_compute_pipeline(
    receiver: Id,
    _sel: Sel,
    function: Id,
    error: *mut Id,
) -> Id {
    if !error.is_null() {
        // SAFETY: the guest passed a writable `NSError **`.
        unsafe { *error = std::ptr::null_mut() };
    }
    let fail = |message: &str| -> Id {
        if !error.is_null() {
            unsafe {
                *error = foundation::error::make("MTLComputePipelineErrorDomain", 3, message)
            };
        }
        gpu::trace(message);
        std::ptr::null_mut()
    };
    if function.is_null() || !object::is_kind(function, c"RosemaryMTLFunction") {
        return fail("newComputePipelineStateWithFunction:error: requires an MTLFunction");
    }
    let Some(gpu) = gpu_of(receiver) else {
        return fail("Metal device has no Vulkan device");
    };
    // SAFETY: an `RosemaryMTLFunction`, checked above.
    let entry = unsafe { object::body::<Function>(function) };
    // SAFETY: a function holds the library which vended it.
    let Some(artifact) = (unsafe { entry.artifact() }) else {
        return fail("the MTLFunction has no translated entry point");
    };
    match pipeline::ComputePipeline::new(gpu, receiver, artifact) {
        Ok(state) => {
            objc::arc::retain(receiver);
            object::make(c"RosemaryMTLComputePipelineState", state)
        }
        Err(reason) => fail(&reason),
    }
}

pub(super) unsafe extern "C" fn new_render_pipeline(
    receiver: Id,
    _sel: Sel,
    descriptor: Id,
    error: *mut Id,
) -> Id {
    if !error.is_null() {
        // SAFETY: the guest passed a writable `NSError **`.
        unsafe { *error = std::ptr::null_mut() };
    }
    let fail = |message: &str| -> Id {
        if !error.is_null() {
            unsafe { *error = foundation::error::make("MTLRenderPipelineErrorDomain", 3, message) };
        }
        gpu::trace(message);
        std::ptr::null_mut()
    };
    if descriptor.is_null() {
        return fail("newRenderPipelineStateWithDescriptor:error: requires a descriptor");
    }
    let Some(gpu) = gpu_of(receiver) else {
        return fail("Metal device has no Vulkan device");
    };
    // SAFETY: `MTLRenderPipelineDescriptor` is this crate's class.
    let described = unsafe { object::body::<pipeline::RenderPipelineDescriptor>(descriptor) };
    let Some(vertex) = (unsafe { artifact_of(described.vertex_function) }) else {
        return fail("render pipeline has no translated vertex function");
    };
    let Some(fragment) = (unsafe { artifact_of(described.fragment_function) }) else {
        return fail("render pipeline has no translated fragment function");
    };
    let (layouts, attributes) = if described.vertex_descriptor.is_null() {
        (
            [resource::VertexLayout::default(); resource::VERTEX_SLOTS],
            [resource::VertexAttribute::default(); resource::VERTEX_SLOTS],
        )
    } else {
        // SAFETY: `MTLVertexDescriptor` is this crate's class.
        let vertex_descriptor =
            unsafe { object::body::<resource::VertexDescriptor>(described.vertex_descriptor) };
        (vertex_descriptor.layouts, vertex_descriptor.attributes)
    };
    match pipeline::RenderPipeline::new(gpu, vertex, fragment, described, &layouts, &attributes) {
        Ok(state) => object::make(c"RosemaryMTLRenderPipelineState", state),
        Err(reason) => fail(&reason),
    }
}

/// The translated entry point behind an `MTLFunction`.
///
/// # Safety
///
/// `function` must be null or an `RosemaryMTLFunction`.
pub(super) unsafe fn artifact_of(function: Id) -> Option<&'static metal_translate::EntryArtifact> {
    if function.is_null() || !object::is_kind(function, c"RosemaryMTLFunction") {
        return None;
    }
    // SAFETY: the caller guarantees the class.
    let entry = unsafe { object::body::<Function>(function) };
    // SAFETY: a function holds the library which vended it, and a library
    // outlives every function it made.
    unsafe { entry.artifact() }
}

pub(super) unsafe extern "C" fn current_allocated_size(_receiver: Id, _sel: Sel) -> u64 {
    ALLOCATED_SIZE.load(Ordering::Relaxed)
}

pub(super) unsafe extern "C" fn name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `RosemaryMTLDevice`.
    unsafe { object::body::<Device>(receiver) }.name
}

pub(super) unsafe extern "C" fn registry_id(receiver: Id, _sel: Sel) -> u64 {
    unsafe { object::body::<Device>(receiver) }.ordinal
}

pub(super) unsafe extern "C" fn no(_receiver: Id, _sel: Sel) -> bool {
    false
}

pub(super) unsafe extern "C" fn recommended_working_set(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: an `RosemaryMTLDevice`.
    unsafe { object::body::<Device>(receiver) }.recommended_working_set
}

pub(super) unsafe extern "C" fn max_buffer_length(receiver: Id, _sel: Sel) -> u64 {
    gpu_of(receiver).map_or(256 << 20, |gpu| {
        u64::from(gpu.limits.max_storage_buffer_range)
    })
}

pub(super) unsafe extern "C" fn max_threadgroup_memory(receiver: Id, _sel: Sel) -> u64 {
    gpu_of(receiver).map_or(32 << 10, |gpu| {
        u64::from(gpu.limits.max_compute_shared_memory_size)
    })
}

/// `-supportsFamily:`.
///
/// A Vulkan device on a desktop driver is the Mac family, never an Apple
/// silicon one: the Apple families promise tile memory, imageblocks and
/// programmable blending, which this translator does not lower. A game which
/// asks and is told "no family at all" concludes the device is unusable.
pub(super) unsafe extern "C" fn supports_family(_receiver: Id, _sel: Sel, family: u64) -> bool {
    matches!(
        family,
        MTL_GPU_FAMILY_MAC_1 | MTL_GPU_FAMILY_MAC_2
        // MTLGPUFamilyCommon1 to Common3.
        | MTL_GPU_FAMILY_COMMON_1
            ..=MTL_GPU_FAMILY_COMMON_3
        // MTLGPUFamilyMetal3.
        | MTL_GPU_FAMILY_METAL_3
    )
}

/// `-supportsFeatureSet:`, the older spelling of the same question. Only the
/// macOS feature sets are answered; iOS and tvOS sets belong to other devices.
pub(super) unsafe extern "C" fn supports_feature_set(
    _receiver: Id,
    _sel: Sel,
    feature_set: u64,
) -> bool {
    matches!(
        feature_set,
        MTL_FEATURE_SET_MACOS_GPU_FAMILY_1_V1..=MTL_FEATURE_SET_MACOS_GPU_FAMILY_2_V1
    )
}

pub(super) unsafe extern "C" fn yes(_receiver: Id, _sel: Sel) -> bool {
    true
}

/// `-supportsBCTextureCompression`, which the pixel-format table needs in
/// order to answer a BC format at all.
pub(super) unsafe extern "C" fn supports_bc(receiver: Id, _sel: Sel) -> bool {
    gpu_of(receiver).is_some()
}

/// `-argumentBuffersSupport`: `MTLArgumentBuffersTier1`, which is the tier
/// whose argument buffers this runtime could lower to descriptor sets.
pub(super) unsafe extern "C" fn argument_buffers_tier(_receiver: Id, _sel: Sel) -> u64 {
    MTL_ARGUMENT_BUFFERS_TIER_1
}

/// `-readWriteTextureSupport`: tier 1, the single-channel 32-bit formats
/// every Vulkan device can read and write as storage images.
pub(super) unsafe extern "C" fn read_write_texture_tier(_receiver: Id, _sel: Sel) -> u64 {
    MTL_READ_WRITE_TEXTURE_TIER_1
}

pub(super) unsafe extern "C" fn max_threads_per_threadgroup(
    receiver: Id,
    _sel: Sel,
) -> encoder::MtlSize {
    let [width, height, depth] =
        gpu_of(receiver).map_or([0; 3], |gpu| gpu.limits.max_compute_work_group_size);
    encoder::MtlSize {
        width: u64::from(width),
        height: u64::from(height),
        depth: u64::from(depth),
    }
}

/// `-maxArgumentBufferSamplerCount`: the number of distinct sampler objects
/// the device can hold at once.
pub(super) unsafe extern "C" fn max_argument_buffer_samplers(receiver: Id, _sel: Sel) -> u64 {
    gpu_of(receiver).map_or(0, |gpu| u64::from(gpu.limits.max_sampler_allocation_count))
}

/// `-minimumTextureBufferAlignmentForPixelFormat:` and its linear-texture
/// sibling: a texture over buffer memory is a Vulkan texel buffer.
pub(super) unsafe extern "C" fn texel_buffer_alignment(
    receiver: Id,
    _sel: Sel,
    _format: u64,
) -> u64 {
    gpu_of(receiver).map_or(0, |gpu| gpu.limits.min_texel_buffer_offset_alignment)
}

/// `-counterSets`: no GPU counters are exposed, so there is no set to sample.
pub(super) unsafe extern "C" fn counter_sets(_receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an empty array.
    unsafe {
        corefoundation::api::CFAutorelease(corefoundation::collections::new_array(
            &[],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

/// `-location`: Vulkan does not say how an adapter is attached.
pub(super) unsafe extern "C" fn location(_receiver: Id, _sel: Sel) -> u64 {
    MTL_DEVICE_LOCATION_UNSPECIFIED
}

/// `-sampleTimestamps:gpuTimestamp:`: the CPU stamp in `mach_absolute_time`
/// nanoseconds, the GPU stamp in the device's own ticks.
pub(super) unsafe extern "C" fn sample_timestamps(
    receiver: Id,
    _sel: Sel,
    cpu_timestamp: *mut u64,
    gpu_timestamp: *mut u64,
) {
    let Some((cpu, gpu)) = gpu_of(receiver).and_then(Gpu::sample_timestamps) else {
        // FIXME (ACCURACY): without calibrated timestamps the device clock
        // cannot be read on demand, so both stamps are zero, which callers
        // take as "no sample".
        // SAFETY: Metal writes both through the guest's pointers.
        unsafe { (cpu_timestamp.write(0), gpu_timestamp.write(0)) };
        return;
    };
    // SAFETY: Metal writes both through the guest's pointers.
    unsafe { (cpu_timestamp.write(cpu), gpu_timestamp.write(gpu)) };
}

pub(super) unsafe extern "C" fn supports_counter_sampling(
    _receiver: Id,
    _sel: Sel,
    _sampling_point: u64,
) -> bool {
    false
}

pub(super) unsafe extern "C" fn supports_sample_count(receiver: Id, _sel: Sel, count: u64) -> bool {
    let Some(gpu) = gpu_of(receiver) else {
        return false;
    };
    let Some(samples) = gpu::sample_count(count) else {
        return false;
    };
    gpu.limits.framebuffer_color_sample_counts.contains(samples)
}

pub(super) unsafe extern "C" fn new_event(receiver: Id, _sel: Sel) -> Id {
    let Some(gpu) = gpu_of(receiver) else {
        return std::ptr::null_mut();
    };
    event::new_event(gpu, receiver, c"RosemaryMTLEvent")
}

pub(super) unsafe extern "C" fn new_shared_event(receiver: Id, _sel: Sel) -> Id {
    let Some(gpu) = gpu_of(receiver) else {
        return std::ptr::null_mut();
    };
    event::new_event(gpu, receiver, c"RosemaryMTLSharedEvent")
}
