use super::*;

/// Do not execute independent Vulkan/Metal integration fixtures against a
/// shared process-global Metal runtime concurrently. Libtest may choose
/// different OS threads for each test, and the fake device uses global
/// resources. Keep the assertions real, one test per child process.
static GPU_TEST_SUBPROCESS: std::sync::Mutex<()> = std::sync::Mutex::new(());

macro_rules! isolate_metal_fixture {
    () => {
        let here = {
            // A child that fails poisons this; the next must still run.
            let _serial = GPU_TEST_SUBPROCESS
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            sdl3::isolated::run_here()
        };
        if !here {
            return;
        }
    };
}

fn device_for_tests() -> Id {
    bootstrap();
    let target = Box::leak(Box::new(metal_translate::TargetProfile::vulkan_1_3()));
    object::make(
        c"RosemaryMTLDevice",
        Device {
            ordinal: 1,
            name: foundation::string::from_str("Test Device"),
            recommended_working_set: 1 << 30,
            target,
            gpu: None,
        },
    )
}

/// A device backed by a real Vulkan adapter, or `None` where there is none.
fn gpu_device() -> Option<Id> {
    bootstrap();
    // SAFETY: called through the same entry point a guest uses.
    let device = unsafe { MTLCreateSystemDefaultDevice() };
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_some() {
        assert!(
            !device.is_null(),
            "the configured runtime must provide a Metal device"
        );
        assert!(
            gpu_of(device).is_some(),
            "the configured runtime must create a GPU"
        );
    }
    (!device.is_null() && gpu_of(device).is_some()).then_some(device)
}

const TEST_KERNEL: &str =
    "kernel void clear(device uint* output [[buffer(0)]]) { output[0] = 42; }";

fn source_library(device: Id, source: &str) -> Id {
    let mut error = std::ptr::null_mut();
    // SAFETY: the same call the guest makes.
    unsafe {
        new_library_with_source(
            device,
            std::ptr::null(),
            foundation::string::from_str(source),
            std::ptr::null_mut(),
            &mut error,
        )
    }
}

#[test]
fn every_export_resolves_with_both_spellings() {
    isolate_metal_fixture!();
    for name in names() {
        assert_ne!(symbol(&name), None);
        assert_eq!(symbol(&format!("_{name}")), symbol(&name));
    }
}

#[test]
fn device_registers_the_metal_object_graph() {
    isolate_metal_fixture!();
    bootstrap();
    for (class, selector) in [
        (c"RosemaryMTLDevice", "newLibraryWithSource:options:error:"),
        (c"RosemaryMTLDevice", "newDefaultLibrary"),
        (
            c"RosemaryMTLDevice",
            "newRenderPipelineStateWithDescriptor:error:",
        ),
        (
            c"RosemaryMTLCommandEncoder",
            "drawPrimitives:vertexStart:vertexCount:instanceCount:",
        ),
        (c"RosemaryMTLCommandBuffer", "GPUStartTime"),
        (c"RosemaryMTLCommandBuffer", "GPUEndTime"),
        (c"MTLRenderPassDescriptor", "colorAttachments"),
        (c"MTLRenderPipelineDescriptor", "setVertexFunction:"),
        (c"MTLSamplerDescriptor", "setMinFilter:"),
        (c"MTLDepthStencilDescriptor", "setDepthWriteEnabled:"),
        (c"MTLDepthStencilDescriptor", "backFaceStencil"),
        (c"RosemaryCAMetalDrawable", "texture"),
        (
            c"RosemaryMTLLibrary",
            "newFunctionWithName:constantValues:error:",
        ),
        (
            c"MTLFunctionConstantValues",
            "setConstantValue:type:atIndex:",
        ),
        (
            c"MTLFunctionConstantValues",
            "setConstantValues:type:withRange:",
        ),
    ] {
        let class = objc::class::lookup(class).expect("Metal is bootstrapped");
        assert!(
            class.method(objc::sel::get(selector)).is_some(),
            "{selector} is not registered"
        );
    }
}

/// A device which answers "no" to every family and feature set reads as
/// unusable: a game asks before it decides which renderer to run.
#[test]
fn the_device_reports_the_mac_gpu_families() {
    isolate_metal_fixture!();
    bootstrap();
    // SAFETY: the same calls the guest makes; neither reads the receiver.
    unsafe {
        assert!(supports_family(
            std::ptr::null_mut(),
            std::ptr::null(),
            2002
        ));
        assert!(supports_family(
            std::ptr::null_mut(),
            std::ptr::null(),
            3001
        ));
        // An Apple silicon family promises tile memory and imageblocks.
        assert!(!supports_family(
            std::ptr::null_mut(),
            std::ptr::null(),
            1001
        ));
        assert!(supports_feature_set(
            std::ptr::null_mut(),
            std::ptr::null(),
            10000
        ));
        // An iOS feature set belongs to a different device.
        assert!(!supports_feature_set(
            std::ptr::null_mut(),
            std::ptr::null(),
            0
        ));
    }
}

#[test]
fn sample_counts_follow_the_vulkan_device() {
    isolate_metal_fixture!();
    for (count, sample) in [
        (1, ash::vk::SampleCountFlags::TYPE_1),
        (2, ash::vk::SampleCountFlags::TYPE_2),
        (4, ash::vk::SampleCountFlags::TYPE_4),
        (8, ash::vk::SampleCountFlags::TYPE_8),
    ] {
        assert_eq!(gpu::sample_count(count), Some(sample));
    }
    for count in [0, 3, 16, 64] {
        assert_eq!(gpu::sample_count(count), None);
    }

    let Some(device) = gpu_device() else {
        return;
    };
    let gpu = gpu_of(device).expect("a Vulkan device");
    for (count, sample) in [
        (1, ash::vk::SampleCountFlags::TYPE_1),
        (2, ash::vk::SampleCountFlags::TYPE_2),
        (4, ash::vk::SampleCountFlags::TYPE_4),
        (8, ash::vk::SampleCountFlags::TYPE_8),
    ] {
        // SAFETY: the same query a guest makes on this live device.
        let reported = unsafe { supports_sample_count(device, std::ptr::null(), count) };
        assert_eq!(
            reported,
            gpu.limits.framebuffer_color_sample_counts.contains(sample)
        );
    }
    // SAFETY: the same query with values outside Metal's supported set.
    unsafe {
        assert!(!supports_sample_count(device, std::ptr::null(), 0));
        assert!(!supports_sample_count(device, std::ptr::null(), 16));
    }
}

#[test]
fn the_read_write_tier_follows_storage_image_support() {
    for format in READ_WRITE_TIER_1.iter().chain(&READ_WRITE_TIER_2) {
        assert_ne!(
            gpu::pixel_format(*format),
            ash::vk::Format::UNDEFINED,
            "pixel format {format}"
        );
    }
    let offline = device_for_tests();
    // SAFETY: the guest's query on a device with no Vulkan adapter.
    let tier = unsafe { read_write_texture_tier(offline, std::ptr::null()) };
    assert_eq!(tier, MTL_READ_WRITE_TEXTURE_TIER_NONE);

    let Some(device) = gpu_device() else {
        return;
    };
    let gpu = gpu_of(device).expect("a Vulkan device");
    let instance = vulkan::instance().expect("the instance behind the device");
    let storage = |format: u64| {
        // SAFETY: the physical device belongs to this instance.
        let properties = unsafe {
            instance.get_physical_device_format_properties(gpu.physical, gpu::pixel_format(format))
        };
        properties
            .optimal_tiling_features
            .contains(ash::vk::FormatFeatureFlags::STORAGE_IMAGE)
    };
    // SAFETY: the same query on the live device.
    let tier = unsafe { read_write_texture_tier(device, std::ptr::null()) };
    if tier >= MTL_READ_WRITE_TEXTURE_TIER_1 {
        assert!(READ_WRITE_TIER_1.iter().all(|&format| storage(format)));
    }
    if tier == MTL_READ_WRITE_TEXTURE_TIER_2 {
        assert!(READ_WRITE_TIER_2.iter().all(|&format| storage(format)));
    }
    assert!(tier <= MTL_READ_WRITE_TEXTURE_TIER_2);
}

#[test]
fn source_library_creation_reports_invalid_source() {
    isolate_metal_fixture!();
    let device = device_for_tests();
    let mut error = std::ptr::null_mut();
    // SAFETY: the same call the guest makes.
    let library = unsafe {
        new_library_with_source(
            device,
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut error,
        )
    };
    assert!(library.is_null());
    assert!(!error.is_null());
}

#[test]
fn a_translated_library_exposes_its_entry_points() {
    isolate_metal_fixture!();
    let device = device_for_tests();
    let library = source_library(device, TEST_KERNEL);
    assert!(!library.is_null());
    // SAFETY: a library this test made.
    let names = unsafe { library_function_names(library, std::ptr::null()) };
    // SAFETY: the array `functionNames` just returned.
    assert_eq!(
        unsafe { corefoundation::api::CFArrayGetCount(names as CFTypeRef) },
        1
    );
    let name = unsafe { corefoundation::api::CFArrayGetValueAtIndex(names as CFTypeRef, 0) as Id };
    // SAFETY: a library and a name it just reported.
    let function = unsafe { new_function_with_name(library, std::ptr::null(), name) };
    assert!(!function.is_null());
    // SAFETY: a function this test made.
    assert_eq!(
        unsafe { foundation::string::bytes(function_name(function, std::ptr::null())) },
        b"clear"
    );
}

fn constant_values() -> Id {
    bootstrap();
    object::make(
        c"MTLFunctionConstantValues",
        ConstantValues {
            values: metal_translate::FunctionConstantValues::new(),
        },
    )
}

#[test]
fn function_constant_values_are_kept_by_index_name_and_range() {
    isolate_metal_fixture!();
    let values = constant_values();
    let flag = 1u8;
    let lanes = [7u32, 11];
    // SAFETY: the calls a guest makes, with values of the named types.
    unsafe {
        set_constant_at_index(values, std::ptr::null(), &flag, 53, 2);
        set_constant_with_name(
            values,
            std::ptr::null(),
            lanes.as_ptr().cast(),
            34,
            foundation::string::from_str("lanes"),
        );
        set_constants_in_range(values, std::ptr::null(), lanes.as_ptr().cast(), 33, 4, 2);
    }
    let mut expected = metal_translate::FunctionConstantValues::new();
    expected.set_index(2, 53, &[1]).unwrap();
    expected
        .set_name("lanes", 34, &[7, 0, 0, 0, 11, 0, 0, 0])
        .unwrap();
    expected.set_index(4, 33, &7u32.to_le_bytes()).unwrap();
    expected.set_index(5, 33, &11u32.to_le_bytes()).unwrap();
    // SAFETY: an `MTLFunctionConstantValues` this test made.
    assert_eq!(
        unsafe { object::body::<ConstantValues>(values) }.values,
        expected
    );
}

/// A library which declares no function constants has nothing to specialize,
/// so Metal hands back the function the values would not have changed.
#[test]
fn a_library_without_function_constants_ignores_the_values() {
    isolate_metal_fixture!();
    let device = device_for_tests();
    let library = source_library(device, TEST_KERNEL);
    let values = constant_values();
    let mut error = std::ptr::null_mut();
    // SAFETY: the calls a guest makes.
    let function = unsafe {
        new_function_with_constants(
            library,
            std::ptr::null(),
            foundation::string::from_str("clear"),
            values,
            &mut error,
        )
    };
    assert!(!function.is_null());
    assert!(error.is_null());
    // SAFETY: a function this test made.
    assert!(
        unsafe { object::body::<Function>(function) }
            .specialized
            .is_none()
    );

    let missing = unsafe {
        new_function_with_constants(
            library,
            std::ptr::null(),
            foundation::string::from_str("absent"),
            values,
            &mut error,
        )
    };
    assert!(missing.is_null());
    assert!(!error.is_null(), "a missing function is an error");
}

/// A device without a Vulkan device must refuse resource creation rather than
/// answer an object which cannot back any GPU work.
#[test]
fn a_device_without_vulkan_refuses_resources() {
    isolate_metal_fixture!();
    let device = device_for_tests();
    // SAFETY: the same calls the guest makes.
    unsafe {
        assert!(new_buffer(device, std::ptr::null(), 256, 0).is_null());
        assert!(new_command_queue(device, std::ptr::null()).is_null());
    }
}

/// The whole compute path on a real device: translate, create a pipeline,
/// encode a dispatch against a Vulkan buffer, and read the result back.
#[test]
fn a_translated_kernel_runs_on_the_gpu() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let library = source_library(device, TEST_KERNEL);
    assert!(!library.is_null(), "the test kernel must translate");
    // SAFETY: a library this test made.
    let function = unsafe {
        new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("clear"),
        )
    };
    assert!(!function.is_null());
    let mut error = std::ptr::null_mut();
    // SAFETY: a device and a function this test made.
    let pipeline = unsafe { new_compute_pipeline(device, std::ptr::null(), function, &mut error) };
    assert!(!pipeline.is_null(), "compute pipeline creation failed");
    assert!(error.is_null());

    // SAFETY: the same calls the guest makes.
    let (buffer, queue) = unsafe {
        (
            new_buffer(device, std::ptr::null(), 16, 0),
            new_command_queue(device, std::ptr::null()),
        )
    };
    assert!(!buffer.is_null() && !queue.is_null());
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", pipeline);
        set_buffer_at(encoder, buffer, 0, 0);
        dispatch(encoder, 1);
        send_void(encoder, "endEncoding");
        assert_eq!(send_double(command, "GPUStartTime"), 0.0);
        assert_eq!(send_double(command, "GPUEndTime"), 0.0);
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");

        let contents = object::body::<resource::Buffer>(buffer).inner.contents;
        assert_eq!(std::ptr::read(contents as *const u32), 42);
        let start = send_double(command, "GPUStartTime");
        let end = send_double(command, "GPUEndTime");
        assert!(start > 0.0);
        assert!(end >= start);
    }
}

/// An encoder reuses the descriptor set it last wrote while the bindings a
/// dispatch needs are unchanged, so a rebind between dispatches must still
/// reach the shader - including a rebind back to a buffer whose set was
/// already replaced, and one to a `-setBytes:` copy in between.
#[test]
fn rebinding_between_dispatches_reaches_each_buffer() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let library = source_library(device, TEST_KERNEL);
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let function = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("clear"),
        );
        let mut error = std::ptr::null_mut();
        let pipeline = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        assert!(!pipeline.is_null());
        let buffers = [
            new_buffer(device, std::ptr::null(), 16, 0),
            new_buffer(device, std::ptr::null(), 16, 0),
            new_buffer(device, std::ptr::null(), 16, 0),
        ];
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", pipeline);
        set_buffer_at(encoder, buffers[0], 0, 0);
        dispatch(encoder, 1);
        dispatch(encoder, 1);
        set_buffer_at(encoder, buffers[1], 0, 0);
        dispatch(encoder, 1);
        set_bytes_named(encoder, "setBytes:length:atIndex:", &[0; 16], 0);
        dispatch(encoder, 1);
        set_buffer_at(encoder, buffers[2], 0, 0);
        dispatch(encoder, 1);
        send_void(encoder, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        for buffer in buffers {
            let contents = object::body::<resource::Buffer>(buffer).inner.contents;
            assert_eq!(std::ptr::read(contents as *const u32), 42);
        }
    }
}

/// A private buffer lives where the host cannot map it, so its `-contents`
/// is null; the bytes it was made with, the device's writes and a blit out of
/// it must still all arrive, and a reused range must read back as zeros.
#[test]
fn a_private_buffer_is_filled_written_and_copied_by_the_device() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const PRIVATE: u64 = 2 << 4;
    let library = source_library(device, TEST_KERNEL);
    let words: [u32; 4] = [1, 2, 3, 4];
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let function = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("clear"),
        );
        let mut error = std::ptr::null_mut();
        let pipeline = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        assert!(!pipeline.is_null());
        let private =
            new_buffer_with_bytes(device, std::ptr::null(), words.as_ptr().cast(), 16, PRIVATE);
        assert!(!private.is_null());
        assert!(
            object::body::<resource::Buffer>(private)
                .inner
                .contents
                .is_null()
        );
        assert_eq!(send_number_result(private, "storageMode"), 2);
        let shared = new_buffer(device, std::ptr::null(), 16, 0);
        let queue = new_command_queue(device, std::ptr::null());

        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", pipeline);
        set_buffer_at(encoder, private, 0, 0);
        dispatch(encoder, 1);
        send_void(encoder, "endEncoding");
        let blit = send(command, "blitCommandEncoder");
        copy_buffer(blit, private, shared, 16);
        send_void(blit, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        let contents = object::body::<resource::Buffer>(shared).inner.contents as *const u32;
        assert_eq!(std::slice::from_raw_parts(contents, 4), [42, 2, 3, 4]);

        // The range the first buffer held is handed to the next one.
        objc::arc::release(private);
        let reused = new_buffer(device, std::ptr::null(), 16, PRIVATE);
        let command = send(queue, "commandBuffer");
        let blit = send(command, "blitCommandEncoder");
        copy_buffer(blit, reused, shared, 16);
        send_void(blit, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        assert_eq!(std::slice::from_raw_parts(contents, 4), [0, 0, 0, 0]);
    }
}

/// A large shared buffer is host pages the device imports as it uses them: a
/// copy imports only the range it names and a binding the whole buffer, over
/// pages a copy already imported. Each must see what the host wrote, and the
/// host what each wrote.
#[test]
fn a_large_shared_buffer_is_seen_through_its_host_pages() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const LENGTH: usize = 4 << 20;
    // Inside a page, so a copy's import starts below it.
    const OFFSET: usize = (3 << 20) + 100;
    let library = source_library(device, TEST_KERNEL);
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let function = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("clear"),
        );
        let mut error = std::ptr::null_mut();
        let pipeline = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        assert!(!pipeline.is_null());
        let large = new_buffer(device, std::ptr::null(), LENGTH, 0);
        let other = new_buffer(device, std::ptr::null(), LENGTH, 0);
        let small = new_buffer(device, std::ptr::null(), 16, 0);
        let contents = object::body::<resource::Buffer>(large).inner.contents;
        std::ptr::copy_nonoverlapping([1u32, 2, 3, 4].as_ptr(), contents.add(OFFSET).cast(), 4);
        let queue = new_command_queue(device, std::ptr::null());

        let command = send(queue, "commandBuffer");
        let blit = send(command, "blitCommandEncoder");
        copy_buffer_range(blit, large, OFFSET as u64, small, 0, 16);
        send_void(blit, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        let small_contents = object::body::<resource::Buffer>(small).inner.contents;
        assert_eq!(
            std::slice::from_raw_parts(small_contents as *const u32, 4),
            [1, 2, 3, 4]
        );

        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", pipeline);
        set_buffer_at(encoder, large, 0, 0);
        dispatch(encoder, 1);
        send_void(encoder, "endEncoding");
        let blit = send(command, "blitCommandEncoder");
        copy_buffer_range(blit, small, 0, other, (LENGTH - 16) as u64, 16);
        send_void(blit, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        assert_eq!(std::ptr::read(contents as *const u32), 42);
        assert_eq!(
            std::slice::from_raw_parts(contents.add(OFFSET) as *const u32, 4),
            [1, 2, 3, 4]
        );
        let other_contents = object::body::<resource::Buffer>(other).inner.contents;
        assert_eq!(
            std::slice::from_raw_parts(other_contents.add(LENGTH - 16) as *const u32, 4),
            [1, 2, 3, 4]
        );
        for buffer in [large, other, small] {
            objc::arc::release(buffer);
        }
    }
}

/// # Safety
///
/// `blit` must be a blit encoder of this crate's, and both buffers its own.
unsafe fn copy_buffer(blit: Id, from: Id, to: Id, size: u64) {
    // SAFETY: as the caller promised.
    unsafe { copy_buffer_range(blit, from, 0, to, 0, size) }
}

/// # Safety
///
/// As for [`copy_buffer`], with both ranges inside their buffers.
unsafe fn copy_buffer_range(
    blit: Id,
    from: Id,
    from_offset: u64,
    to: Id,
    to_offset: u64,
    size: u64,
) {
    let sel = objc::sel::get("copyFromBuffer:sourceOffset:toBuffer:destinationOffset:size:");
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(blit) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the registered signature.
    let call: unsafe extern "C" fn(Id, Sel, Id, u64, Id, u64, u64) =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { call(blit, sel, from, from_offset, to, to_offset, size) };
}

/// `MTLSamplerAddressModeMirrorClampToEdge` is the one Metal wrap mode Vulkan
/// makes conditional on a feature.
#[test]
fn a_mirror_clamp_sampler_follows_the_device_feature() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let gpu = gpu_of(device).expect("a Vulkan device");
    let descriptor = resource::SamplerDescriptor {
        address: [1, 0, 0],
        ..resource::SamplerDescriptor::default()
    };
    // Mirror-clamp is created where the device offers it and rejected where
    // it does not, rather than quietly becoming a different wrap mode.
    assert_eq!(
        !resource::new_sampler(gpu, &descriptor).is_null(),
        gpu.sampler_mirror_clamp_to_edge
    );
}

/// robustness2 makes a nil Metal buffer an explicit null descriptor, so a
/// store is valid and discarded instead of reaching an undefined descriptor.
#[test]
fn a_nil_buffer_store_is_discarded_with_robustness2() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let gpu = gpu_of(device).expect("a Vulkan device");
    if !gpu.null_descriptors {
        return;
    }
    let library = source_library(device, TEST_KERNEL);
    assert!(!library.is_null(), "the null-buffer kernel must translate");

    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let function = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("clear"),
        );
        let mut error = std::ptr::null_mut();
        let pipeline = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        assert!(!pipeline.is_null(), "compute pipeline creation failed");
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", pipeline);
        set_buffer_at(encoder, std::ptr::null_mut(), 0, 0);
        dispatch(encoder, 1);
        send_void(encoder, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
    }
}

// Small message-sending helpers: the tests drive the objects through the
// selectors a guest uses rather than through the Rust implementations, so a
// missing registration fails the test.

/// # Safety
///
/// `receiver` must implement `selector` as `id (id, SEL)`.
unsafe fn send(receiver: Id, selector: &str) -> Id {
    let sel = objc::sel::get(selector);
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(receiver) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the caller guarantees the signature.
    let call: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { call(receiver, sel) }
}

/// # Safety
///
/// `receiver` must implement `selector` as `void (id, SEL)`.
unsafe fn send_void(receiver: Id, selector: &str) {
    let sel = objc::sel::get(selector);
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(receiver) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the caller guarantees the signature.
    let call: unsafe extern "C" fn(Id, Sel) =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { call(receiver, sel) };
}

/// # Safety
///
/// `receiver` must implement `selector` as `double (id, SEL)`.
unsafe fn send_double(receiver: Id, selector: &str) -> f64 {
    let sel = objc::sel::get(selector);
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(receiver) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the caller guarantees the signature.
    let call: unsafe extern "C" fn(Id, Sel) -> f64 =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { call(receiver, sel) }
}

/// # Safety
///
/// `receiver` must implement `selector` as `void (id, SEL, id)`.
unsafe fn send_with_object(receiver: Id, selector: &str, argument: Id) {
    let sel = objc::sel::get(selector);
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(receiver) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the caller guarantees the signature.
    let call: unsafe extern "C" fn(Id, Sel, Id) =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { call(receiver, sel, argument) };
}

/// # Safety
///
/// `encoder` must be an `RosemaryMTLCommandEncoder`.
unsafe fn set_buffer_at(encoder: Id, buffer: Id, offset: u64, index: u64) {
    let sel = objc::sel::get("setBuffer:offset:atIndex:");
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(encoder) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the registered signature.
    let call: unsafe extern "C" fn(Id, Sel, Id, u64, u64) =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { call(encoder, sel, buffer, offset, index) };
}

/// # Safety
///
/// `encoder` must be an `RosemaryMTLCommandEncoder`.
unsafe fn dispatch(encoder: Id, groups: u64) {
    let sel = objc::sel::get("dispatchThreadgroups:threadsPerThreadgroup:");
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(encoder) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the registered signature.
    let call: unsafe extern "C" fn(Id, Sel, encoder::MtlSize, encoder::MtlSize) =
        unsafe { std::mem::transmute(method.implementation()) };
    let size = encoder::MtlSize {
        width: groups,
        height: 1,
        depth: 1,
    };
    let one = encoder::MtlSize {
        width: 1,
        height: 1,
        depth: 1,
    };
    unsafe { call(encoder, sel, size, one) };
}

use metal_translate::ir;

/// A vertex/fragment pair built directly in the Metal IR.
///
/// The MSL frontend has no vertex path yet and the AIR fixtures in this tree
/// are compute kernels, so the render test states the program it draws with
/// rather than parsing one.
fn triangle_library() -> metal_translate::TranslatedLibrary {
    let float4 = ir::Type::Vector {
        element: Box::new(ir::Type::Float { bits: 32 }),
        lanes: 4,
    };
    let vertex = ir::Function {
        name: "triangle_vertex".to_string(),
        result: float4.clone(),
        parameters: vec![ir::Parameter {
            value: ir::ValueId(0),
            name: "position".to_string(),
            ty: float4.clone(),
        }],
        values: vec![float4.clone()],
        blocks: vec![ir::Block {
            id: ir::BlockId(0),
            name: "entry".to_string(),
            instructions: Vec::new(),
            terminator: ir::Terminator::Return(Some(ir::Operand::Value(ir::ValueId(0)))),
        }],
    };
    let fragment = ir::Function {
        name: "triangle_fragment".to_string(),
        result: float4.clone(),
        parameters: Vec::new(),
        values: Vec::new(),
        blocks: vec![ir::Block {
            id: ir::BlockId(0),
            name: "entry".to_string(),
            instructions: Vec::new(),
            terminator: ir::Terminator::Return(Some(ir::Operand::Composite {
                ty: float4.clone(),
                elements: vec![
                    ir::Operand::f32(1.0),
                    ir::Operand::f32(0.0),
                    ir::Operand::f32(0.0),
                    ir::Operand::f32(1.0),
                ],
            })),
        }],
    };
    let module = ir::Module {
        static_initializers: Vec::new(),
        functions: vec![vertex, fragment],
        globals: Vec::new(),
        entries: vec![
            ir::EntryPoint {
                function: 0,
                stage: ir::Stage::Vertex,
                resources: Vec::new(),
                inputs: vec![ir::StageInput {
                    argument: 0,
                    binding: ir::StageBinding::Location {
                        location: 0,
                        interpolation: None,
                    },
                    name: Some("position".to_string()),
                    signed: false,
                }],
                outputs: vec![ir::StageOutput {
                    field: None,
                    binding: ir::StageBinding::Builtin(ir::Builtin::Position),
                    name: None,
                }],
            },
            ir::EntryPoint {
                function: 1,
                stage: ir::Stage::Fragment,
                resources: Vec::new(),
                inputs: Vec::new(),
                outputs: vec![ir::StageOutput {
                    field: None,
                    binding: ir::StageBinding::RenderTarget { index: 0 },
                    name: None,
                }],
            },
        ],
    };
    metal_translate::emit_spirv(&module, &metal_translate::TargetProfile::vulkan_1_3())
        .expect("the triangle program translates")
}

/// # Safety
///
/// `receiver` must implement `selector` as `void (id, SEL, NSUInteger)`.
unsafe fn send_number(receiver: Id, selector: &str, value: u64) {
    let sel = objc::sel::get(selector);
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(receiver) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the caller guarantees the signature.
    let call: unsafe extern "C" fn(Id, Sel, u64) =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { call(receiver, sel, value) };
}

/// # Safety
///
/// `receiver` must implement `objectAtIndexedSubscript:`.
unsafe fn element_at(receiver: Id, index: u64) -> Id {
    let sel = objc::sel::get("objectAtIndexedSubscript:");
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(receiver) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the registered signature.
    let call: unsafe extern "C" fn(Id, Sel, u64) -> Id =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { call(receiver, sel, index) }
}

/// A multisampled depth attachment resolved into a one-sample texture, which
/// is what `VK_KHR_depth_stencil_resolve` adds. The pass clears depth to one,
/// draws a triangle covering every sample at a depth of a half, and the
/// resolve destination must hold the half rather than the clear.
#[test]
fn a_multisampled_depth_attachment_resolves_into_its_destination() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let gpu = gpu_of(device).expect("a Vulkan device");
    const SAMPLES: u64 = 4;
    const SIZE: u64 = 8;
    // MTLPixelFormatDepth32Float, and the colour the fragment shader writes.
    const DEPTH32: u64 = 252;
    const BGRA8: u64 = 80;
    let multisampled = ash::vk::SampleCountFlags::TYPE_4;
    if !gpu
        .limits
        .framebuffer_depth_sample_counts
        .contains(multisampled)
        || !gpu
            .limits
            .framebuffer_color_sample_counts
            .contains(multisampled)
    {
        return;
    }
    let Some(resolving) = &gpu.depth_stencil_resolve else {
        return;
    };
    if !resolving
        .support
        .depth_modes
        .contains(ash::vk::ResolveModeFlags::SAMPLE_ZERO)
    {
        return;
    }

    let library = retain_library(device, triangle_library(), None, vec![0, 0], None);
    // SAFETY: as in `a_translated_triangle_draws_on_the_gpu`.
    let (vertex, fragment) = unsafe {
        (
            new_function_with_name(
                library,
                std::ptr::null(),
                foundation::string::from_str("triangle_vertex"),
            ),
            new_function_with_name(
                library,
                std::ptr::null(),
                foundation::string::from_str("triangle_fragment"),
            ),
        )
    };
    let vertex_descriptor = object::make(
        c"MTLVertexDescriptor",
        resource::VertexDescriptor {
            layouts: [resource::VertexLayout::default(); resource::VERTEX_SLOTS],
            attributes: [resource::VertexAttribute::default(); resource::VERTEX_SLOTS],
            layout_objects: std::ptr::null_mut(),
            attribute_objects: std::ptr::null_mut(),
        },
    );
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        send_number(
            element_at(send(vertex_descriptor, "layouts"), 0),
            "setStride:",
            16,
        );
        let attribute = element_at(send(vertex_descriptor, "attributes"), 0);
        send_number(attribute, "setFormat:", 31); // MTLVertexFormatFloat4
        send_number(attribute, "setBufferIndex:", 0);
    }
    let descriptor = object::make(
        c"MTLRenderPipelineDescriptor",
        pipeline::RenderPipelineDescriptor {
            vertex_function: vertex,
            fragment_function: fragment,
            vertex_descriptor,
            colors: [pipeline::ColorAttachment::fresh(); pipeline::COLOR_ATTACHMENTS],
            depth_format: DEPTH32,
            stencil_format: 0,
            sample_count: SAMPLES,
            alpha_to_coverage: false,
            color_objects: std::ptr::null_mut(),
        },
    );
    // SAFETY: a descriptor this test made.
    unsafe {
        send_number(
            element_at(send(descriptor, "colorAttachments"), 0),
            "setPixelFormat:",
            BGRA8,
        );
    }
    let mut error = std::ptr::null_mut();
    // SAFETY: a device and a descriptor this test made.
    let state = unsafe { new_render_pipeline(device, std::ptr::null(), descriptor, &mut error) };
    assert!(!state.is_null(), "render pipeline creation failed");
    // Depth is written only where the test's half is nearer than the clear.
    let depth_state = resource::new_depth_stencil_state(resource::DepthStencilState {
        // MTLCompareFunctionLess.
        depth_compare: 1,
        depth_write: true,
        ..resource::DepthStencilState::default()
    });

    let texture = |pixel_format, sample_count| {
        resource::new_texture(
            gpu,
            resource::TextureDescriptor {
                texture_type: 2,
                pixel_format,
                width: SIZE,
                height: SIZE,
                depth: 1,
                mipmap_level_count: 1,
                sample_count,
                array_length: 1,
                storage_mode: 2,
                usage: gpu::USAGE_RENDER_TARGET,
            },
        )
    };
    let color = texture(BGRA8, SAMPLES);
    let depth = texture(DEPTH32, SAMPLES);
    let resolved = texture(DEPTH32, 1);
    assert!(!color.is_null() && !depth.is_null() && !resolved.is_null());
    // SAFETY: the same calls the guest makes.
    let (vertices, readback, queue) = unsafe {
        (
            new_buffer(device, std::ptr::null(), 48, 0),
            new_buffer(device, std::ptr::null(), (SIZE * SIZE * 4) as usize, 0),
            new_command_queue(device, std::ptr::null()),
        )
    };
    // A triangle covering the whole target, half way into clip space.
    // SAFETY: a buffer this test made, mapped over its whole length.
    unsafe {
        let contents = object::body::<resource::Buffer>(vertices).inner.contents as *mut f32;
        for (index, value) in [
            -1.0f32, -1.0, 0.5, 1.0, //
            3.0, -1.0, 0.5, 1.0, //
            -1.0, 3.0, 0.5, 1.0,
        ]
        .iter()
        .enumerate()
        {
            contents.add(index).write(*value);
        }
    }

    let pass = object::make(
        c"MTLRenderPassDescriptor",
        encoder::RenderPassDescriptor::default(),
    );
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let attachment = element_at(send(pass, "colorAttachments"), 0);
        send_with_object(attachment, "setTexture:", color);
        send_number(attachment, "setLoadAction:", 2); // Clear
        send_number(attachment, "setStoreAction:", 1); // Store
        let attachment = send(pass, "depthAttachment");
        send_with_object(attachment, "setTexture:", depth);
        send_number(attachment, "setLoadAction:", 2); // Clear, to the default 1.0
        // StoreAndMultisampleResolve, with the default Sample0 filter.
        send_number(attachment, "setStoreAction:", 3);
        send_with_object(attachment, "setResolveTexture:", resolved);
    }

    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let command = send(queue, "commandBuffer");
        let render = render_encoder(command, pass);
        assert!(!render.is_null());
        send_with_object(render, "setRenderPipelineState:", state);
        send_with_object(render, "setDepthStencilState:", depth_state);
        set_buffer_named(render, "setVertexBuffer:offset:atIndex:", vertices, 0, 0);
        draw(render, 3);
        send_void(render, "endEncoding");

        let blit = send(command, "blitCommandEncoder");
        copy_texture_to_buffer(blit, resolved, readback, SIZE);
        send_void(blit, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");

        let contents = object::body::<resource::Buffer>(readback).inner.contents as *const f32;
        let middle = ((SIZE / 2) * SIZE + SIZE / 2) as usize;
        let depth = contents.add(middle).read();
        assert!(
            (depth - 0.5).abs() < 1.0e-5,
            "the depth resolve wrote {depth}, not the drawn 0.5"
        );
    }
}

/// The whole render path on a real device: a translated vertex/fragment pair,
/// a render pipeline, a render pass with a clear, one draw, and a blit which
/// reads the result back.
#[test]
fn a_translated_triangle_draws_on_the_gpu() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const SIZE: u64 = 8;
    let library = retain_library(device, triangle_library(), None, vec![0, 0], None);
    // SAFETY: a library this test made.
    let (vertex, fragment) = unsafe {
        (
            new_function_with_name(
                library,
                std::ptr::null(),
                foundation::string::from_str("triangle_vertex"),
            ),
            new_function_with_name(
                library,
                std::ptr::null(),
                foundation::string::from_str("triangle_fragment"),
            ),
        )
    };
    assert!(!vertex.is_null() && !fragment.is_null());

    // The vertex descriptor: one float4 stream at buffer index 0.
    let vertex_descriptor = object::make(
        c"MTLVertexDescriptor",
        resource::VertexDescriptor {
            layouts: [resource::VertexLayout::default(); resource::VERTEX_SLOTS],
            attributes: [resource::VertexAttribute::default(); resource::VERTEX_SLOTS],
            layout_objects: std::ptr::null_mut(),
            attribute_objects: std::ptr::null_mut(),
        },
    );
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let layouts = send(vertex_descriptor, "layouts");
        send_number(element_at(layouts, 0), "setStride:", 16);
        let attributes = send(vertex_descriptor, "attributes");
        let attribute = element_at(attributes, 0);
        send_number(attribute, "setFormat:", 31); // MTLVertexFormatFloat4
        send_number(attribute, "setOffset:", 0);
        send_number(attribute, "setBufferIndex:", 0);
    }

    let descriptor = object::make(
        c"MTLRenderPipelineDescriptor",
        pipeline::RenderPipelineDescriptor {
            vertex_function: vertex,
            fragment_function: fragment,
            vertex_descriptor,
            colors: [pipeline::ColorAttachment::fresh(); pipeline::COLOR_ATTACHMENTS],
            depth_format: 0,
            stencil_format: 0,
            sample_count: 1,
            alpha_to_coverage: false,
            color_objects: std::ptr::null_mut(),
        },
    );
    // SAFETY: a descriptor this test made.
    unsafe {
        let colors = send(descriptor, "colorAttachments");
        send_number(element_at(colors, 0), "setPixelFormat:", 80); // BGRA8Unorm
    }
    let mut error = std::ptr::null_mut();
    // SAFETY: a device and a descriptor this test made.
    let state = unsafe { new_render_pipeline(device, std::ptr::null(), descriptor, &mut error) };
    assert!(!state.is_null(), "render pipeline creation failed");

    // The colour attachment, the vertices and the buffer read back into.
    let target = resource::new_texture(
        gpu_of(device).expect("a Vulkan device"),
        resource::TextureDescriptor {
            texture_type: 2,
            pixel_format: 80,
            width: SIZE,
            height: SIZE,
            depth: 1,
            mipmap_level_count: 1,
            sample_count: 1,
            array_length: 1,
            storage_mode: 2,
            usage: gpu::USAGE_RENDER_TARGET,
        },
    );
    assert!(!target.is_null());
    // SAFETY: the same calls the guest makes.
    let (vertices, readback, queue) = unsafe {
        (
            new_buffer(device, std::ptr::null(), 48, 0),
            new_buffer(device, std::ptr::null(), (SIZE * SIZE * 4) as usize, 0),
            new_command_queue(device, std::ptr::null()),
        )
    };
    // A triangle which covers the whole target in Metal's clip space.
    // SAFETY: a buffer this test made, mapped over its whole length.
    unsafe {
        let contents = object::body::<resource::Buffer>(vertices).inner.contents as *mut f32;
        for (index, value) in [
            -1.0f32, -1.0, 0.0, 1.0, //
            3.0, -1.0, 0.0, 1.0, //
            -1.0, 3.0, 0.0, 1.0,
        ]
        .iter()
        .enumerate()
        {
            contents.add(index).write(*value);
        }
    }

    let pass = object::make(
        c"MTLRenderPassDescriptor",
        encoder::RenderPassDescriptor::default(),
    );
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let colors = send(pass, "colorAttachments");
        let attachment = element_at(colors, 0);
        send_with_object(attachment, "setTexture:", target);
        send_number(attachment, "setLoadAction:", 2); // Clear
        send_number(attachment, "setStoreAction:", 1); // Store
    }

    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let command = send(queue, "commandBuffer");
        let render = render_encoder(command, pass);
        assert!(!render.is_null());
        send_with_object(render, "setRenderPipelineState:", state);
        set_buffer_named(render, "setVertexBuffer:offset:atIndex:", vertices, 0, 0);
        draw(render, 3);
        send_void(render, "endEncoding");

        let blit = send(command, "blitCommandEncoder");
        copy_texture_to_buffer(blit, target, readback, SIZE);
        send_void(blit, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");

        let contents = object::body::<resource::Buffer>(readback).inner.contents;
        let middle = ((SIZE / 2) * SIZE + SIZE / 2) as usize * 4;
        let texel = std::slice::from_raw_parts(contents.add(middle), 4);
        // BGRA8Unorm: the fragment shader's opaque red is (0, 0, 255, 255).
        assert_eq!(texel, [0, 0, 255, 255], "the triangle did not draw red");
    }
}

/// `-replaceRegion:` is recorded for the next submission rather than run on
/// the spot, so the texels must still be there - in the order the copies were
/// made - by the time a command buffer reads them.
#[test]
fn replaced_texels_reach_the_next_command_buffer() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const SIZE: u64 = 8;
    let gpu = gpu_of(device).expect("a Vulkan device");
    let descriptor = resource::TextureDescriptor {
        texture_type: 2,
        pixel_format: 70, // RGBA8Unorm
        width: SIZE,
        height: SIZE,
        depth: 1,
        mipmap_level_count: 1,
        sample_count: 1,
        array_length: 1,
        storage_mode: 0,
        usage: gpu::USAGE_SHADER_READ,
    };
    let texture = resource::new_texture(gpu, descriptor);
    assert!(!texture.is_null());

    let whole: Vec<u8> = (0..SIZE * SIZE * 4).map(|byte| byte as u8).collect();
    let corner = [0xAB_u8; 2 * 2 * 4];
    // SAFETY: textures this test made, and byte runs covering each region.
    unsafe {
        replace_region(texture, [0, 0, SIZE, SIZE], &whole, SIZE * 4);
        // A later copy over part of the first must win.
        replace_region(texture, [1, 1, 2, 2], &corner, 2 * 4);
    }

    // SAFETY: the same calls the guest makes.
    let (readback, queue) = unsafe {
        (
            new_buffer(device, std::ptr::null(), (SIZE * SIZE * 4) as usize, 0),
            new_command_queue(device, std::ptr::null()),
        )
    };
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let command = send(queue, "commandBuffer");
        let blit = send(command, "blitCommandEncoder");
        copy_texture_to_buffer(blit, texture, readback, SIZE);
        send_void(blit, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");

        let contents = object::body::<resource::Buffer>(readback).inner.contents;
        let read = std::slice::from_raw_parts(contents, whole.len());
        let mut expected = whole.clone();
        for y in 1..3 {
            for x in 1..3 {
                let at = ((y * SIZE + x) * 4) as usize;
                expected[at..at + 4].copy_from_slice(&[0xAB; 4]);
            }
        }
        assert_eq!(read, expected.as_slice());
    }
}

/// `-getBytes:` reads back what `-replaceRegion:` wrote, without a command
/// buffer in between, and lays the rows out at the guest's stride.
#[test]
fn get_bytes_reads_back_replaced_texels() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const SIZE: u64 = 8;
    let gpu = gpu_of(device).expect("a Vulkan device");
    let texture = resource::new_texture(
        gpu,
        resource::TextureDescriptor {
            texture_type: 2,
            pixel_format: 70, // RGBA8Unorm
            width: SIZE,
            height: SIZE,
            depth: 1,
            mipmap_level_count: 1,
            sample_count: 1,
            array_length: 1,
            storage_mode: 0,
            usage: gpu::USAGE_SHADER_READ,
        },
    );
    assert!(!texture.is_null());
    let whole: Vec<u8> = (0..SIZE * SIZE * 4).map(|byte| byte as u8).collect();
    // SAFETY: a texture this test made, and bytes covering it. The short form
    // is the one most guests call.
    unsafe {
        let sel = objc::sel::get("replaceRegion:mipmapLevel:withBytes:bytesPerRow:");
        let method = objc::object::class_of(texture)
            .and_then(|class| class.method(sel))
            .expect("the selector is registered");
        let call: unsafe extern "C" fn(Id, Sel, [u64; 6], u64, *const u8, u64) =
            std::mem::transmute(method.implementation());
        call(
            texture,
            sel,
            [0, 0, 0, SIZE, SIZE, 1],
            0,
            whole.as_ptr(),
            SIZE * 4,
        );
    }

    // A 3x2 region at (2, 5), read at a row stride with four bytes of padding.
    const STRIDE: usize = 3 * 4 + 4;
    let mut read = [0xEE_u8; STRIDE * 2];
    // SAFETY: the buffer covers two rows at `STRIDE`.
    unsafe { get_bytes(texture, [2, 5, 3, 2], &mut read, STRIDE as u64) };
    for row in 0..2 {
        let at = (((5 + row) * SIZE + 2) * 4) as usize;
        let line = &read[row as usize * STRIDE..][..STRIDE];
        assert_eq!(&line[..12], &whole[at..at + 12], "row {row}");
        assert_eq!(&line[12..], &[0xEE; 4], "padding of row {row} was written");
    }
    objc::arc::release(texture);
}

/// # Safety
///
/// `texture` must be an `RosemaryMTLTexture`, and `bytes` must cover the
/// region at `bytes_per_row`.
unsafe fn get_bytes(texture: Id, region: [u64; 4], bytes: &mut [u8], bytes_per_row: u64) {
    #[repr(C)]
    struct Region {
        origin: [u64; 3],
        size: [u64; 3],
    }
    let sel = objc::sel::get("getBytes:bytesPerRow:fromRegion:mipmapLevel:");
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(texture) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the registered signature.
    let call: unsafe extern "C" fn(Id, Sel, *mut u8, u64, Region, u64) =
        unsafe { std::mem::transmute(method.implementation()) };
    let [x, y, width, height] = region;
    unsafe {
        call(
            texture,
            sel,
            bytes.as_mut_ptr(),
            bytes_per_row,
            Region {
                origin: [x, y, 0],
                size: [width, height, 1],
            },
            0,
        )
    };
}

/// A texture released while a copy into it is still waiting for a
/// submission must outlive that copy rather than be destroyed under it.
#[test]
fn a_texture_released_before_its_upload_runs_outlives_it() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let gpu = gpu_of(device).expect("a Vulkan device");
    let texture = resource::new_texture(
        gpu,
        resource::TextureDescriptor {
            texture_type: 2,
            pixel_format: 70, // RGBA8Unorm
            width: 4,
            height: 4,
            depth: 1,
            mipmap_level_count: 1,
            sample_count: 1,
            array_length: 1,
            storage_mode: 0,
            usage: gpu::USAGE_SHADER_READ,
        },
    );
    assert!(!texture.is_null());
    // SAFETY: a texture this test made, and bytes covering it.
    unsafe { replace_region(texture, [0, 0, 4, 4], &[7; 64], 16) };
    objc::arc::release(texture);
    // SAFETY: the same calls the guest makes.
    unsafe {
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        assert_eq!(send_number_result(command, "status"), 4);
    }
}

/// # Safety
///
/// `receiver` must implement `selector` returning an `NSUInteger`.
unsafe fn send_number_result(receiver: Id, selector: &str) -> u64 {
    let sel = objc::sel::get(selector);
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(receiver) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the registered signature.
    let call: unsafe extern "C" fn(Id, Sel) -> u64 =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { call(receiver, sel) }
}

/// # Safety
///
/// `texture` must be an `RosemaryMTLTexture` of four bytes a texel, and
/// `bytes` must cover the region at `bytes_per_row`.
unsafe fn replace_region(texture: Id, region: [u64; 4], bytes: &[u8], bytes_per_row: u64) {
    #[repr(C)]
    struct Region {
        origin: [u64; 3],
        size: [u64; 3],
    }
    let sel =
        objc::sel::get("replaceRegion:mipmapLevel:slice:withBytes:bytesPerRow:bytesPerImage:");
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(texture) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the registered signature.
    let call: unsafe extern "C" fn(Id, Sel, Region, u64, u64, *const u8, u64, u64) =
        unsafe { std::mem::transmute(method.implementation()) };
    let [x, y, width, height] = region;
    unsafe {
        call(
            texture,
            sel,
            Region {
                origin: [x, y, 0],
                size: [width, height, 1],
            },
            0,
            0,
            bytes.as_ptr(),
            bytes_per_row,
            0,
        )
    };
}

/// # Safety
///
/// `command` must be an `RosemaryMTLCommandBuffer` and `pass` a render pass
/// descriptor.
unsafe fn render_encoder(command: Id, pass: Id) -> Id {
    let sel = objc::sel::get("renderCommandEncoderWithDescriptor:");
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(command) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the registered signature.
    let call: unsafe extern "C" fn(Id, Sel, Id) -> Id =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { call(command, sel, pass) }
}

/// # Safety
///
/// `encoder` must be an `RosemaryMTLCommandEncoder`.
unsafe fn set_buffer_named(encoder: Id, selector: &str, buffer: Id, offset: u64, index: u64) {
    let sel = objc::sel::get(selector);
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(encoder) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the registered signature.
    let call: unsafe extern "C" fn(Id, Sel, Id, u64, u64) =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { call(encoder, sel, buffer, offset, index) };
}

/// # Safety
///
/// `encoder` must be an `RosemaryMTLCommandEncoder` inside a render pass.
unsafe fn draw(encoder: Id, vertices: u64) {
    let sel = objc::sel::get("drawPrimitives:vertexStart:vertexCount:instanceCount:");
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(encoder) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the registered signature.
    let call: unsafe extern "C" fn(Id, Sel, u64, u64, u64, u64) =
        unsafe { std::mem::transmute(method.implementation()) };
    // MTLPrimitiveTypeTriangle is 3.
    unsafe { call(encoder, sel, 3, 0, vertices, 1) };
}

/// # Safety
///
/// `encoder` must be an `RosemaryMTLCommandEncoder`.
unsafe fn copy_texture_to_buffer(encoder: Id, texture: Id, buffer: Id, size: u64) {
    let sel = objc::sel::get(
        "copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:",
    );
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(encoder) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the registered signature.
    #[allow(clippy::type_complexity)]
    let call: unsafe extern "C" fn(
        Id,
        Sel,
        Id,
        u64,
        u64,
        encoder::MtlOrigin,
        encoder::MtlSize,
        Id,
        u64,
        u64,
        u64,
    ) = unsafe { std::mem::transmute(method.implementation()) };
    unsafe {
        call(
            encoder,
            sel,
            texture,
            0,
            0,
            encoder::MtlOrigin { x: 0, y: 0, z: 0 },
            encoder::MtlSize {
                width: size,
                height: size,
                depth: 1,
            },
            buffer,
            0,
            size * 4,
            size * size * 4,
        )
    };
}

/// # Safety
///
/// `encoder` must be an `RosemaryMTLCommandEncoder`.
unsafe fn set_bytes_named(encoder: Id, selector: &str, bytes: &[u8], index: u64) {
    let sel = objc::sel::get(selector);
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(encoder) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the registered signature.
    let call: unsafe extern "C" fn(Id, Sel, *const u8, u64, u64) =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { call(encoder, sel, bytes.as_ptr(), bytes.len() as u64, index) };
}

/// The CPU cost of recording: many dispatches and draws with the per-call
/// binding churn a game's frame has, timed from the first bind to the end of
/// `-commit`. Run with `cargo test --release -p metal -- --ignored
/// --nocapture recording_cost`.
#[test]
#[ignore = "a benchmark, not a check"]
fn recording_cost() {
    let Some(device) = gpu_device() else {
        return;
    };
    const DISPATCHES: usize = 20_000;
    const DRAWS: usize = 20_000;
    let library = source_library(device, TEST_KERNEL);
    // SAFETY: objects this test made, sent the selectors they implement.
    let (pipeline, buffers, queue) = unsafe {
        let function = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("clear"),
        );
        let mut error = std::ptr::null_mut();
        let pipeline = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        let buffers = [
            new_buffer(device, std::ptr::null(), 256, 0),
            new_buffer(device, std::ptr::null(), 256, 0),
        ];
        (
            pipeline,
            buffers,
            new_command_queue(device, std::ptr::null()),
        )
    };
    let pool = objc::arc::pool_push();
    let compute = unsafe {
        let start = std::time::Instant::now();
        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", pipeline);
        for index in 0..DISPATCHES {
            match index % 4 {
                0 => set_bytes_named(encoder, "setBytes:length:atIndex:", &[0; 64], 0),
                _ => set_buffer_at(encoder, buffers[index % 2], 0, 0),
            }
            dispatch(encoder, 1);
        }
        send_void(encoder, "endEncoding");
        send_void(command, "commit");
        let recorded = start.elapsed();
        send_void(command, "waitUntilCompleted");
        recorded
    };
    objc::arc::pool_pop(pool);

    let library = retain_library(device, triangle_library(), None, vec![0, 0], None);
    // SAFETY: as in `a_translated_triangle_draws_on_the_gpu`.
    let render = unsafe {
        let vertex = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("triangle_vertex"),
        );
        let fragment = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("triangle_fragment"),
        );
        let vertex_descriptor = object::make(
            c"MTLVertexDescriptor",
            resource::VertexDescriptor {
                layouts: [resource::VertexLayout::default(); resource::VERTEX_SLOTS],
                attributes: [resource::VertexAttribute::default(); resource::VERTEX_SLOTS],
                layout_objects: std::ptr::null_mut(),
                attribute_objects: std::ptr::null_mut(),
            },
        );
        send_number(
            element_at(send(vertex_descriptor, "layouts"), 0),
            "setStride:",
            16,
        );
        let attribute = element_at(send(vertex_descriptor, "attributes"), 0);
        send_number(attribute, "setFormat:", 31);
        send_number(attribute, "setBufferIndex:", 0);
        let descriptor = object::make(
            c"MTLRenderPipelineDescriptor",
            pipeline::RenderPipelineDescriptor {
                vertex_function: vertex,
                fragment_function: fragment,
                vertex_descriptor,
                colors: [pipeline::ColorAttachment::fresh(); pipeline::COLOR_ATTACHMENTS],
                depth_format: 0,
                stencil_format: 0,
                sample_count: 1,
                alpha_to_coverage: false,
                color_objects: std::ptr::null_mut(),
            },
        );
        send_number(
            element_at(send(descriptor, "colorAttachments"), 0),
            "setPixelFormat:",
            80,
        );
        let mut error = std::ptr::null_mut();
        let state = new_render_pipeline(device, std::ptr::null(), descriptor, &mut error);
        let target = resource::new_texture(
            gpu_of(device).expect("a Vulkan device"),
            resource::TextureDescriptor {
                texture_type: 2,
                pixel_format: 80,
                width: 64,
                height: 64,
                depth: 1,
                mipmap_level_count: 1,
                sample_count: 1,
                array_length: 1,
                storage_mode: 2,
                usage: gpu::USAGE_RENDER_TARGET,
            },
        );
        let pass = object::make(
            c"MTLRenderPassDescriptor",
            encoder::RenderPassDescriptor::default(),
        );
        let attachment = element_at(send(pass, "colorAttachments"), 0);
        send_with_object(attachment, "setTexture:", target);
        send_number(attachment, "setLoadAction:", 2);
        send_number(attachment, "setStoreAction:", 1);

        let pool = objc::arc::pool_push();
        let start = std::time::Instant::now();
        let command = send(queue, "commandBuffer");
        let encoder = render_encoder(command, pass);
        send_with_object(encoder, "setRenderPipelineState:", state);
        for index in 0..DRAWS {
            set_buffer_named(
                encoder,
                "setVertexBuffer:offset:atIndex:",
                buffers[index % 2],
                0,
                0,
            );
            draw(encoder, 3);
        }
        send_void(encoder, "endEncoding");
        send_void(command, "commit");
        let recorded = start.elapsed();
        send_void(command, "waitUntilCompleted");
        objc::arc::pool_pop(pool);
        recorded
    };
    eprintln!(
        "recording: {:.2} us/dispatch, {:.2} us/draw",
        compute.as_secs_f64() * 1e6 / DISPATCHES as f64,
        render.as_secs_f64() * 1e6 / DRAWS as f64,
    );
}

/// The observer form answers the same devices, and the observer it hands out
/// is released once removed; removing it again, or nil, is harmless.
#[test]
fn a_device_observer_comes_with_the_device_list_until_removed() {
    #[repr(C)]
    struct Descriptor {
        reserved: u64,
        size: u64,
    }
    #[repr(C)]
    struct Literal {
        isa: u64,
        flags: i32,
        reserved: i32,
        invoke: unsafe extern "C" fn(*mut Literal, Id, Id),
        descriptor: &'static Descriptor,
    }
    unsafe extern "C" fn invoke(_block: *mut Literal, _device: Id, _name: Id) {
        panic!("no device is added or removed");
    }
    static DESCRIPTOR: Descriptor = Descriptor {
        reserved: 0,
        size: size_of::<Literal>() as u64,
    };
    bootstrap();
    let isa = objc::block::data_symbols()
        .into_iter()
        .find(|(name, _)| name == "__NSConcreteGlobalBlock")
        .map_or(0, |(_, address)| address);
    // A global block, as a literal capturing nothing is.
    let handler = Box::leak(Box::new(Literal {
        isa,
        flags: 1 << 28,
        reserved: 0,
        invoke,
        descriptor: &DESCRIPTOR,
    })) as *mut Literal as Id;
    let mut observer: Id = std::ptr::null_mut();
    // SAFETY: the framework ABI, with a writable out-parameter.
    unsafe {
        let devices = MTLCopyAllDevicesWithObserver(&mut observer, handler);
        let plain = MTLCopyAllDevices();
        assert_eq!(
            corefoundation::api::CFArrayGetCount(devices),
            corefoundation::api::CFArrayGetCount(plain)
        );
        assert!(!observer.is_null());
        assert!(object::is_kind(observer, c"RosemaryMTLDeviceObserver"));
        MTLRemoveDeviceObserver(observer);
        MTLRemoveDeviceObserver(observer);
        MTLRemoveDeviceObserver(std::ptr::null_mut());
        corefoundation::api::CFRelease(devices);
        corefoundation::api::CFRelease(plain);
    }
}
