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
        assert_ne!(symbol(name), None);
        assert_eq!(symbol(&format!("_{name}")), symbol(name));
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
                    invariant: false,
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
                    invariant: false,
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
            alpha_to_one: false,
            rasterization: true,
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
            alpha_to_one: false,
            rasterization: true,
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
                alpha_to_one: false,
                rasterization: true,
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

/// `MTLVisibilityResultMode`.
const VISIBILITY_DISABLED: u64 = 0;
const VISIBILITY_BOOLEAN: u64 = 1;
const VISIBILITY_COUNTING: u64 = 2;

/// The side of the target a covering draw fills, so each draw passes
/// `COVERING_SIZE` squared samples.
const COVERING_SIZE: u64 = 8;

/// A pipeline, a one-sample target and a triangle which covers all of it.
struct CoveringDraw {
    state: Id,
    target: Id,
    vertices: Id,
    queue: Id,
}

/// The triangle program and target of `a_translated_triangle_draws_on_the_gpu`,
/// or `None` where the device cannot count samples exactly.
fn covering_draw(device: Id) -> Option<CoveringDraw> {
    let gpu = gpu_of(device).expect("a Vulkan device");
    if !gpu.occlusion_precise {
        return None;
    }
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
    let vertex_descriptor = object::make(
        c"MTLVertexDescriptor",
        resource::VertexDescriptor {
            layouts: [resource::VertexLayout::default(); resource::VERTEX_SLOTS],
            attributes: [resource::VertexAttribute::default(); resource::VERTEX_SLOTS],
            layout_objects: std::ptr::null_mut(),
            attribute_objects: std::ptr::null_mut(),
        },
    );
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
            alpha_to_one: false,
            rasterization: true,
            color_objects: std::ptr::null_mut(),
        },
    );
    let mut error = std::ptr::null_mut();
    // SAFETY: objects this test made, sent the selectors they implement.
    let state = unsafe {
        let layouts = send(vertex_descriptor, "layouts");
        send_number(element_at(layouts, 0), "setStride:", 16);
        let attribute = element_at(send(vertex_descriptor, "attributes"), 0);
        send_number(attribute, "setFormat:", 31); // MTLVertexFormatFloat4
        send_number(attribute, "setOffset:", 0);
        send_number(attribute, "setBufferIndex:", 0);
        let colors = send(descriptor, "colorAttachments");
        send_number(element_at(colors, 0), "setPixelFormat:", 80); // BGRA8Unorm
        new_render_pipeline(device, std::ptr::null(), descriptor, &mut error)
    };
    assert!(!state.is_null(), "render pipeline creation failed");
    let target = resource::new_texture(
        gpu,
        resource::TextureDescriptor {
            texture_type: 2,
            pixel_format: 80,
            width: COVERING_SIZE,
            height: COVERING_SIZE,
            depth: 1,
            mipmap_level_count: 1,
            sample_count: 1,
            array_length: 1,
            storage_mode: 2,
            usage: gpu::USAGE_RENDER_TARGET,
        },
    );
    // SAFETY: the same calls the guest makes.
    let (vertices, queue) = unsafe {
        (
            new_buffer(device, std::ptr::null(), 48, 0),
            new_command_queue(device, std::ptr::null()),
        )
    };
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
    Some(CoveringDraw {
        state,
        target,
        vertices,
        queue,
    })
}

impl CoveringDraw {
    /// A pass clearing the target, with its visibility results in `results`.
    fn pass(&self, results: Id) -> Id {
        let pass = object::make(
            c"MTLRenderPassDescriptor",
            encoder::RenderPassDescriptor::default(),
        );
        // SAFETY: objects this test made, sent the selectors they implement.
        unsafe {
            let attachment = element_at(send(pass, "colorAttachments"), 0);
            send_with_object(attachment, "setTexture:", self.target);
            send_number(attachment, "setLoadAction:", 2); // Clear
            send_number(attachment, "setStoreAction:", 1); // Store
            send_with_object(pass, "setVisibilityResultBuffer:", results);
        }
        pass
    }

    /// # Safety
    ///
    /// `encoder` must be a render encoder inside a pass.
    unsafe fn bind(&self, encoder: Id) {
        unsafe {
            send_with_object(encoder, "setRenderPipelineState:", self.state);
            set_buffer_named(
                encoder,
                "setVertexBuffer:offset:atIndex:",
                self.vertices,
                0,
                0,
            );
        }
    }
}

/// A shared buffer of `words` results, each holding `value`.
fn results_buffer(device: Id, words: usize, value: u64) -> Id {
    // SAFETY: the same call the guest makes.
    let buffer = unsafe { new_buffer(device, std::ptr::null(), words * 8, 0) };
    for at in 0..words {
        // SAFETY: a buffer this test made, mapped over its whole length.
        unsafe {
            (object::body::<resource::Buffer>(buffer).inner.contents as *mut u64)
                .add(at)
                .write(value)
        };
    }
    buffer
}

fn result_at(buffer: Id, at: usize) -> u64 {
    // SAFETY: a shared buffer this test made, read after its work completed.
    unsafe {
        (object::body::<resource::Buffer>(buffer).inner.contents as *const u64)
            .add(at)
            .read()
    }
}

/// # Safety
///
/// `encoder` must be an `RosemaryMTLCommandEncoder`.
unsafe fn set_visibility_mode(encoder: Id, mode: u64, offset: u64) {
    let sel = objc::sel::get("setVisibilityResultMode:offset:");
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(encoder) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    // SAFETY: the registered signature.
    let call: unsafe extern "C" fn(Id, Sel, u64, u64) =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { call(encoder, sel, mode, offset) };
}

/// Runs at one offset in one pass add up, a disabled run is not counted, and
/// an offset the pass never names keeps what it held.
#[test]
fn visibility_results_add_each_offsets_runs_in_a_pass() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let Some(fixture) = covering_draw(device) else {
        return;
    };
    const UNTOUCHED: u64 = 0xabab_abab_abab_abab;
    let results = results_buffer(device, 3, UNTOUCHED);
    let pass = fixture.pass(results);
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let command = send(fixture.queue, "commandBuffer");
        let render = render_encoder(command, pass);
        fixture.bind(render);
        set_visibility_mode(render, VISIBILITY_COUNTING, 0);
        draw(render, 3);
        set_visibility_mode(render, VISIBILITY_DISABLED, 0);
        draw(render, 3);
        set_visibility_mode(render, VISIBILITY_COUNTING, 0);
        draw(render, 3);
        set_visibility_mode(render, VISIBILITY_BOOLEAN, 8);
        draw(render, 3);
        send_void(render, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
    }
    assert_eq!(result_at(results, 0), 2 * COVERING_SIZE * COVERING_SIZE);
    assert_ne!(result_at(results, 1), 0);
    assert_eq!(result_at(results, 2), UNTOUCHED);
}

/// `MTLVisibilityResultTypeAccumulate` adds to the count already in the
/// buffer rather than starting from zero.
#[test]
fn an_accumulating_pass_adds_to_the_held_result() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let Some(fixture) = covering_draw(device) else {
        return;
    };
    const HELD: u64 = 5;
    let results = results_buffer(device, 1, HELD);
    let pass = fixture.pass(results);
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        send_number(pass, "setVisibilityResultType:", 1); // Accumulate
        let command = send(fixture.queue, "commandBuffer");
        let render = render_encoder(command, pass);
        fixture.bind(render);
        set_visibility_mode(render, VISIBILITY_COUNTING, 0);
        draw(render, 3);
        send_void(render, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
    }
    assert_eq!(result_at(results, 0), HELD + COVERING_SIZE * COVERING_SIZE);
}

/// Each parallel sub-encoder records its own queries, and the pass sums them
/// all into the one offset they name.
#[test]
fn parallel_sub_encoders_add_into_one_result() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let Some(fixture) = covering_draw(device) else {
        return;
    };
    let results = results_buffer(device, 1, 0);
    let pass = fixture.pass(results);
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let command = send(fixture.queue, "commandBuffer");
        let sel = objc::sel::get("parallelRenderCommandEncoderWithDescriptor:");
        let method = objc::object::class_of(command)
            .and_then(|class| class.method(sel))
            .expect("the selector is registered");
        let call: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            std::mem::transmute(method.implementation());
        let parallel = call(command, sel, pass);
        for _ in 0..2 {
            let render = send(parallel, "renderCommandEncoder");
            fixture.bind(render);
            set_visibility_mode(render, VISIBILITY_COUNTING, 0);
            draw(render, 3);
            send_void(render, "endEncoding");
        }
        send_void(parallel, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
    }
    assert_eq!(result_at(results, 0), 2 * COVERING_SIZE * COVERING_SIZE);
}

/// The implementation `selector` has on `receiver`, as a function of type `F`.
///
/// # Safety
///
/// `F` must be the registered signature.
unsafe fn method<F: Copy>(receiver: Id, selector: &str) -> (F, Sel) {
    let sel = objc::sel::get(selector);
    // SAFETY: an object the runtime allocated.
    let method = unsafe { objc::object::class_of(receiver) }
        .and_then(|class| class.method(sel))
        .expect("the selector is registered");
    let implementation = method.implementation();
    // SAFETY: the caller guarantees the signature.
    (unsafe { std::mem::transmute_copy(&implementation) }, sel)
}

/// Commit a command buffer which writes 42 into `output` through `kernel`,
/// after `before` has run on it.
///
/// # Safety
///
/// Objects of this crate's, `kernel` a compute pipeline writing `output[0]`.
unsafe fn commit_clear(queue: Id, kernel: Id, output: Id, before: impl FnOnce(Id)) -> Id {
    unsafe {
        let command = send(queue, "commandBuffer");
        before(command);
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", kernel);
        set_buffer_at(encoder, output, 0, 0);
        dispatch(encoder, 1);
        send_void(encoder, "endEncoding");
        send_void(command, "commit");
        command
    }
}

#[test]
fn an_encoded_event_wait_holds_work_until_the_host_signals() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let event = send(device, "newSharedEvent");
        assert!(!event.is_null());
        let library = source_library(device, TEST_KERNEL);
        let function = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("clear"),
        );
        let mut error = std::ptr::null_mut();
        let kernel = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        let output = new_buffer(device, std::ptr::null(), 16, 0);
        let queue = new_command_queue(device, std::ptr::null());
        let (encode, _) = method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(
            send(queue, "commandBuffer"),
            "encodeWaitForEvent:value:",
        );
        let command = commit_clear(queue, kernel, output, |command| {
            encode(
                command,
                objc::sel::get("encodeWaitForEvent:value:"),
                event,
                1,
            );
        });
        let contents = object::body::<resource::Buffer>(output).inner.contents as *const u32;
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(
            *contents, 0,
            "the dispatch ran before the event was signalled"
        );

        let (set_value, sel) =
            method::<unsafe extern "C" fn(Id, Sel, u64)>(event, "setSignaledValue:");
        set_value(event, sel, 1);
        send_void(command, "waitUntilCompleted");
        assert_eq!(*contents, 42);
        assert_eq!(send_number_result(event, "signaledValue"), 1);
    }
}

#[test]
fn an_encoded_event_signal_follows_the_work_before_it() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let event = send(device, "newSharedEvent");
        let library = source_library(device, TEST_KERNEL);
        let function = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("clear"),
        );
        let mut error = std::ptr::null_mut();
        let kernel = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        let output = new_buffer(device, std::ptr::null(), 16, 0);
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", kernel);
        set_buffer_at(encoder, output, 0, 0);
        dispatch(encoder, 1);
        send_void(encoder, "endEncoding");
        let (signal, sel) =
            method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(command, "encodeSignalEvent:value:");
        signal(command, sel, event, 5);
        send_void(command, "commit");
        let (wait, sel) = method::<unsafe extern "C" fn(Id, Sel, u64, u64) -> bool>(
            event,
            "waitUntilSignaledValue:timeoutMS:",
        );
        assert!(wait(event, sel, 5, 5_000));
        let contents = object::body::<resource::Buffer>(output).inner.contents as *const u32;
        assert_eq!(*contents, 42);
        send_void(command, "waitUntilCompleted");
    }
}

#[test]
fn placement_heap_buffers_at_one_offset_share_memory() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const PLACEMENT: u64 = 1;
    const PRIVATE: u64 = 2;
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let descriptor = send(
            send(
                objc::class::ptr(objc::class::lookup(c"MTLHeapDescriptor").expect("registered"))
                    as Id,
                "alloc",
            ),
            "init",
        );
        let (set, _) = method::<unsafe extern "C" fn(Id, Sel, u64)>(descriptor, "setSize:");
        set(descriptor, objc::sel::get("setSize:"), 1 << 20);
        set(descriptor, objc::sel::get("setType:"), PLACEMENT);
        set(descriptor, objc::sel::get("setStorageMode:"), PRIVATE);
        let heap = new_heap(device, std::ptr::null(), descriptor);
        assert!(!heap.is_null());
        let (place, sel) = method::<unsafe extern "C" fn(Id, Sel, u64, u64, u64) -> Id>(
            heap,
            "newBufferWithLength:options:offset:",
        );
        let written = place(heap, sel, 16, PRIVATE << 4, 0);
        let aliased = place(heap, sel, 16, PRIVATE << 4, 0);
        assert!(!written.is_null() && !aliased.is_null());
        assert!(place(heap, sel, 16, PRIVATE << 4, 1 << 20).is_null());

        let library = source_library(device, TEST_KERNEL);
        let function = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("clear"),
        );
        let mut error = std::ptr::null_mut();
        let kernel = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        let shared = new_buffer(device, std::ptr::null(), 16, 0);
        let queue = new_command_queue(device, std::ptr::null());
        let command = commit_clear(queue, kernel, written, |_| {});
        send_void(command, "waitUntilCompleted");
        let command = send(queue, "commandBuffer");
        let blit = send(command, "blitCommandEncoder");
        copy_buffer(blit, aliased, shared, 4);
        send_void(blit, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        let contents = object::body::<resource::Buffer>(shared).inner.contents as *const u32;
        assert_eq!(*contents, 42);
    }
}

#[test]
fn a_released_placement_heap_frees_its_memory_after_its_last_resource() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const PLACEMENT: u64 = 1;
    const PRIVATE: u64 = 2;
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let descriptor = send(
            send(
                objc::class::ptr(objc::class::lookup(c"MTLHeapDescriptor").expect("registered"))
                    as Id,
                "alloc",
            ),
            "init",
        );
        let (set, _) = method::<unsafe extern "C" fn(Id, Sel, u64)>(descriptor, "setSize:");
        set(descriptor, objc::sel::get("setSize:"), 1 << 20);
        set(descriptor, objc::sel::get("setType:"), PLACEMENT);
        set(descriptor, objc::sel::get("setStorageMode:"), PRIVATE);
        let heap = new_heap(device, std::ptr::null(), descriptor);
        assert!(!heap.is_null());
        let memory = resource::placement_of(heap).expect("a placement heap");
        let (place, sel) = method::<unsafe extern "C" fn(Id, Sel, u64, u64, u64) -> Id>(
            heap,
            "newBufferWithLength:options:offset:",
        );
        let buffer = place(heap, sel, 16, PRIVATE << 4, 0);
        assert!(!buffer.is_null());

        objc::arc::release(heap);
        assert!(
            memory.upgrade().is_some(),
            "the placed buffer still holds it"
        );
        objc::arc::release(buffer);
        assert!(memory.upgrade().is_none());
    }
}

/// A texture buffer has no image, so every call which would copy or attach
/// one is refused rather than handed to Vulkan with a null `VkImage`.
#[test]
fn a_texture_buffer_refuses_image_operations() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const R32_UINT: u64 = 53;
    const SHADER_READ: u64 = 1;
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let texels: [u32; 4] = [10, 20, 30, 40];
        let backing =
            new_buffer_with_bytes(device, std::ptr::null(), texels.as_ptr().cast(), 16, 0);
        let class =
            objc::class::ptr(objc::class::lookup(c"MTLTextureDescriptor").expect("registered"))
                as Id;
        let (describe, sel) = method::<unsafe extern "C" fn(Id, Sel, u64, u64, u64, u64) -> Id>(
            class,
            "textureBufferDescriptorWithPixelFormat:width:resourceOptions:usage:",
        );
        let descriptor = describe(class, sel, R32_UINT, 4, 0, SHADER_READ);
        let (make, sel) = method::<unsafe extern "C" fn(Id, Sel, Id, u64, u64) -> Id>(
            backing,
            "newTextureWithDescriptor:offset:bytesPerRow:",
        );
        let texture = make(backing, sel, descriptor, 0, 16);
        assert!(!texture.is_null());

        let region = resource::Region {
            origin: [0; 3],
            size: [1, 1, 1],
        };
        let written = 7u32;
        let (replace, sel) = method::<
            unsafe extern "C" fn(Id, Sel, resource::Region, u64, *const u8, u64),
        >(texture, "replaceRegion:mipmapLevel:withBytes:bytesPerRow:");
        replace(texture, sel, region, 0, (&raw const written).cast(), 4);
        let mut read = 0xdead_beefu32;
        let (get, sel) = method::<unsafe extern "C" fn(Id, Sel, *mut u8, u64, resource::Region, u64)>(
            texture,
            "getBytes:bytesPerRow:fromRegion:mipmapLevel:",
        );
        get(texture, sel, (&raw mut read).cast(), 4, region, 0);
        assert_eq!(read, 0xdead_beef);
        let contents = object::body::<resource::Buffer>(backing).inner.contents as *const u32;
        assert_eq!(*contents, 10);

        let (view, sel) = method::<unsafe extern "C" fn(Id, Sel, u64) -> Id>(
            texture,
            "newTextureViewWithPixelFormat:",
        );
        assert!(view(texture, sel, R32_UINT).is_null());

        let pass = object::make(
            c"MTLRenderPassDescriptor",
            encoder::RenderPassDescriptor::default(),
        );
        let attachment = element_at(send(pass, "colorAttachments"), 0);
        send_with_object(attachment, "setTexture:", texture);
        assert!(send(attachment, "texture").is_null());
    }
}

#[test]
fn a_texture_buffer_reads_the_buffer_it_was_made_from() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const R32_UINT: u64 = 53;
    const SHADER_READ: u64 = 1;
    let source = "kernel void copy(texture_buffer<uint> source [[texture(0)]],\n\
                                   device uint* output [[buffer(0)]]) {\n\
                      output[0] = source.read(uint(2)).x + source.get_width();\n\
                  }";
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let library = source_library(device, source);
        let function = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("copy"),
        );
        let mut error = std::ptr::null_mut();
        let kernel = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        assert!(!kernel.is_null());
        let texels: [u32; 4] = [10, 20, 30, 40];
        let backing =
            new_buffer_with_bytes(device, std::ptr::null(), texels.as_ptr().cast(), 16, 0);
        let class =
            objc::class::ptr(objc::class::lookup(c"MTLTextureDescriptor").expect("registered"))
                as Id;
        let (describe, sel) = method::<unsafe extern "C" fn(Id, Sel, u64, u64, u64, u64) -> Id>(
            class,
            "textureBufferDescriptorWithPixelFormat:width:resourceOptions:usage:",
        );
        let descriptor = describe(class, sel, R32_UINT, 4, 0, SHADER_READ);
        let (make, sel) = method::<unsafe extern "C" fn(Id, Sel, Id, u64, u64) -> Id>(
            backing,
            "newTextureWithDescriptor:offset:bytesPerRow:",
        );
        let texture = make(backing, sel, descriptor, 0, 16);
        assert!(!texture.is_null());

        let output = new_buffer(device, std::ptr::null(), 16, 0);
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", kernel);
        set_buffer_at(encoder, output, 0, 0);
        let (bind, sel) =
            method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(encoder, "setTexture:atIndex:");
        bind(encoder, sel, texture, 0);
        dispatch(encoder, 1);
        send_void(encoder, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        let contents = object::body::<resource::Buffer>(output).inner.contents as *const u32;
        assert_eq!(*contents, 30 + 4);
    }
}

/// MoltenVK fills in the vertex descriptor a pipeline descriptor vends instead
/// of setting one, and a set descriptor is a copy the guest can go on editing.
#[test]
fn a_pipeline_descriptor_vends_and_copies_its_vertex_descriptor() {
    isolate_metal_fixture!();
    bootstrap();
    const FLOAT3: u64 = 30;
    let class_of = |name: &std::ffi::CStr| {
        objc::class::ptr(objc::class::lookup(name).expect("registered")) as Id
    };
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let descriptor = send(
            send(class_of(c"MTLRenderPipelineDescriptor"), "alloc"),
            "init",
        );
        let vended = send(descriptor, "vertexDescriptor");
        assert!(!vended.is_null());
        assert_eq!(vended, send(descriptor, "vertexDescriptor"));
        send_number(
            element_at(send(vended, "attributes"), 2),
            "setFormat:",
            FLOAT3,
        );
        let held = object::body::<pipeline::RenderPipelineDescriptor>(descriptor).vertex_descriptor;
        assert_eq!(
            object::body::<resource::VertexDescriptor>(held).attributes[2].format,
            FLOAT3
        );

        let own = send(send(class_of(c"MTLVertexDescriptor"), "alloc"), "init");
        let attribute = element_at(send(own, "attributes"), 0);
        send_number(attribute, "setFormat:", FLOAT3);
        send_with_object(descriptor, "setVertexDescriptor:", own);
        send_number(attribute, "setFormat:", 0);
        let held = send(descriptor, "vertexDescriptor");
        assert_ne!(held, own);
        assert_eq!(
            object::body::<resource::VertexDescriptor>(held).attributes[0].format,
            FLOAT3
        );
    }
}

/// Each dispatch reads what the one before it wrote: in a serial encoder by
/// Metal's ordering, in a concurrent one by the barrier between them.
#[test]
fn serial_dispatches_and_concurrent_barriers_order_writes() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const STEPS: u32 = 256;
    const DISPATCH_SERIAL: u64 = 0;
    const DISPATCH_CONCURRENT: u64 = 1;
    const SCOPE_BUFFERS: u64 = 1;
    let library = source_library(
        device,
        "kernel void step(device uint* value [[buffer(0)]]) { value[0] = value[0] + 1; }",
    );
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let function = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("step"),
        );
        let mut error = std::ptr::null_mut();
        let pipeline = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        assert!(!pipeline.is_null());
        let queue = new_command_queue(device, std::ptr::null());
        for dispatch_type in [DISPATCH_SERIAL, DISPATCH_CONCURRENT] {
            let buffer = new_buffer(device, std::ptr::null(), 16, 0);
            let command = send(queue, "commandBuffer");
            let (make, sel) = method::<unsafe extern "C" fn(Id, Sel, u64) -> Id>(
                command,
                "computeCommandEncoderWithDispatchType:",
            );
            let encoder = make(command, sel, dispatch_type);
            send_with_object(encoder, "setComputePipelineState:", pipeline);
            set_buffer_at(encoder, buffer, 0, 0);
            for _ in 0..STEPS {
                dispatch(encoder, 1);
                if dispatch_type == DISPATCH_CONCURRENT {
                    send_number(encoder, "memoryBarrierWithScope:", SCOPE_BUFFERS);
                }
            }
            send_void(encoder, "endEncoding");
            send_void(command, "commit");
            send_void(command, "waitUntilCompleted");
            let contents = object::body::<resource::Buffer>(buffer).inner.contents;
            assert_eq!(
                std::ptr::read(contents as *const u32),
                STEPS,
                "type {dispatch_type}"
            );
        }
    }
}

/// Run `name` from `source` once per element of a one-dimensional grid of
/// `threads`, with `buffers` bound from index 0.
fn run_source_kernel(device: Id, source: &str, name: &str, buffers: &[Id], threads: u64) {
    let library = source_library(device, source);
    assert!(!library.is_null(), "the kernel must translate");
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let function = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str(name),
        );
        assert!(!function.is_null(), "{name} must translate");
        let mut error = std::ptr::null_mut();
        let pipeline = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        assert!(!pipeline.is_null());
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", pipeline);
        for (index, buffer) in buffers.iter().enumerate() {
            set_buffer_at(encoder, *buffer, 0, index as u64);
        }
        dispatch(encoder, threads);
        send_void(encoder, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
    }
}

fn buffer_with_bytes(device: Id, bytes: &[u8]) -> Id {
    // SAFETY: the same call the guest makes, with bytes it copies.
    unsafe {
        new_buffer_with_bytes(
            device,
            std::ptr::null(),
            bytes.as_ptr().cast(),
            bytes.len(),
            0,
        )
    }
}

fn buffer_bytes(buffer: Id, length: usize) -> Vec<u8> {
    // SAFETY: a shared buffer of at least `length` bytes the device is done with.
    unsafe {
        let contents = object::body::<resource::Buffer>(buffer).inner.contents;
        std::slice::from_raw_parts(contents as *const u8, length).to_vec()
    }
}

/// A `char` or `void` buffer viewed through a cast reads and writes whole
/// values at any byte offset, leaving the bytes around them alone, and a
/// `bool` member is the byte Metal lays out.
#[test]
fn a_byte_buffer_is_read_and_written_as_any_type_at_any_offset() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const SOURCE: &str = "#include <metal_stdlib>\n\
        using namespace metal;\n\
        struct Flags { bool negate; bool twice; };\n\
        template<typename T>\n\
        static void split(device T* out, uint value) { out[0] = (T)value; out[1] = (T)(value >> 16); }\n\
        kernel void view(const device char* records [[buffer(0)]],\n\
                         device MTLDrawIndexedPrimitivesIndirectArguments* args [[buffer(1)]],\n\
                         device void* halves [[buffer(2)]],\n\
                         constant Flags& flags [[buffer(3)]],\n\
                         uint idx [[thread_position_in_grid]]) {\n\
            const device auto& record = *reinterpret_cast<const device MTLDrawIndexedPrimitivesIndirectArguments*>(records + idx * 22 + 1);\n\
            args[idx] = record;\n\
            device auto& arg = args[idx];\n\
            if (flags.twice) { arg.instanceCount *= 2; }\n\
            if (flags.negate) { arg.baseVertex = -arg.baseVertex; }\n\
            split(&((device ushort*)halves)[idx * 3 + 1], record.indexCount);\n\
        }";
    let records: [[i32; 5]; 2] = [[0x0003_0007, 5, 9, -4, 11], [0x0001_0002, 6, 10, 7, 12]];
    let mut packed = vec![0xEEu8; 2 * 22 + 1];
    for (index, record) in records.iter().enumerate() {
        let at = index * 22 + 1;
        for (field, value) in record.iter().enumerate() {
            packed[at + field * 4..at + field * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
    let buffers = [
        buffer_with_bytes(device, &packed),
        buffer_with_bytes(device, &[0; 40]),
        buffer_with_bytes(device, &[0xFF; 12]),
        buffer_with_bytes(device, &[0, 1]),
    ];
    run_source_kernel(device, SOURCE, "view", &buffers, 2);

    let args: Vec<i32> = buffer_bytes(buffers[1], 40)
        .chunks(4)
        .map(|word| i32::from_le_bytes(word.try_into().unwrap()))
        .collect();
    assert_eq!(
        args,
        [0x0003_0007, 10, 9, -4, 11, 0x0001_0002, 12, 10, 7, 12]
    );
    let halves: Vec<u16> = buffer_bytes(buffers[2], 12)
        .chunks(2)
        .map(|half| u16::from_le_bytes(half.try_into().unwrap()))
        .collect();
    assert_eq!(halves, [0xFFFF, 7, 3, 0xFFFF, 2, 1]);
}

/// A `switch` enters at its matching label, or `default` wherever that is,
/// falls through to the cases after it, and `break` leaves only the switch.
#[test]
fn a_switch_falls_through_from_its_label_until_a_break() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const SOURCE: &str = "#include <metal_stdlib>\n\
        using namespace metal;\n\
        typedef enum : uint8_t { Zero, One, Four = 4, Five } Step;\n\
        kernel void steps(device uint* out [[buffer(0)]], uint idx [[thread_position_in_grid]]) {\n\
            uint trail = 0;\n\
            for (uint pass = 0; pass < 2; pass++) {\n\
                switch ((Step)idx) {\n\
                    case Zero: trail = trail * 10 + 1;\n\
                    case One: trail = trail * 10 + 2; break;\n\
                    case Four: trail = trail * 10 + 3;\n\
                    default: trail = trail * 10 + 9;\n\
                    case Five: trail = trail * 10 + 5; break;\n\
                }\n\
            }\n\
            out[idx] = trail;\n\
        }";
    let out = buffer_with_bytes(device, &[0; 28]);
    run_source_kernel(device, SOURCE, "steps", &[out], 7);
    let trails: Vec<u32> = buffer_bytes(out, 28)
        .chunks(4)
        .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
        .collect();
    assert_eq!(trails, [1212, 22, 9595, 9595, 395395, 55, 9595]);
}

/// `MTLPixelFormat`.
const PIXEL_FORMAT_R32_UINT: u64 = 53;
const PIXEL_FORMAT_RGBA8_UNORM: u64 = 70;
/// `MTLTextureType`.
const TEXTURE_TYPE_2D: u64 = 2;
const TEXTURE_TYPE_3D: u64 = 7;

fn kernel_from_source(device: Id, source: &str, name: &str) -> Id {
    let library = source_library(device, source);
    assert!(!library.is_null(), "{name} must translate");
    // SAFETY: a device and a library this test made.
    unsafe {
        let function = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str(name),
        );
        assert!(!function.is_null());
        let mut error = std::ptr::null_mut();
        let pipeline = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        assert!(!pipeline.is_null(), "{name} pipeline");
        pipeline
    }
}

fn readable_texture(device: Id, texture_type: u64, pixel_format: u64, size: [u64; 3]) -> Id {
    let gpu = gpu_of(device).expect("a Vulkan device");
    let texture = resource::new_texture(
        gpu,
        resource::TextureDescriptor {
            texture_type,
            pixel_format,
            width: size[0],
            height: size[1],
            depth: size[2],
            mipmap_level_count: 1,
            sample_count: 1,
            array_length: 1,
            storage_mode: 0,
            usage: gpu::USAGE_SHADER_READ,
        },
    );
    assert!(!texture.is_null());
    texture
}

/// Four bytes a texel, differing between texels and between seeds.
fn texel_pattern(texels: u64, seed: u32) -> Vec<u8> {
    (0..texels as u32)
        .flat_map(|at| {
            at.wrapping_mul(2_654_435_761)
                .wrapping_add(seed)
                .to_ne_bytes()
        })
        .collect()
}

/// A device which does not push descriptors, so that buffers go through
/// remembered sets and dynamic offsets as well.
fn gpu_device_without_push_descriptors() -> Option<Id> {
    gpu::TEST_WITHOUT_PUSH_DESCRIPTORS.store(true, Ordering::Release);
    let device = gpu_device()?;
    assert_eq!(gpu_of(device).expect("a GPU").max_push_descriptors, 0);
    Some(device)
}

fn check_remembered_sets_keep_their_textures(device: Id) {
    let kernel = kernel_from_source(
        device,
        "kernel void add(device uint* output [[buffer(0)]],\n\
                         texture2d<uint> source [[texture(0)]]) {\n\
             output[0] = output[0] + source.read(uint2(0, 0)).x;\n\
         }",
        "add",
    );
    // More than a command buffer remembers per set index.
    const TEXTURES: usize = 12;
    let mut expected = [0u32; TEXTURES];
    let values: [u32; TEXTURES] = std::array::from_fn(|at| 1 + 37 * at as u32);
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let textures: [Id; TEXTURES] = std::array::from_fn(|at| {
            let texture =
                readable_texture(device, TEXTURE_TYPE_2D, PIXEL_FORMAT_R32_UINT, [1, 1, 1]);
            replace_region(texture, [0, 0, 1, 1], &values[at].to_ne_bytes(), 4);
            texture
        });
        let outputs: [Id; TEXTURES] =
            std::array::from_fn(|_| new_buffer(device, std::ptr::null(), 16, 0));
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        for pass in 0..4 {
            let encoder = send(command, "computeCommandEncoder");
            send_with_object(encoder, "setComputePipelineState:", kernel);
            let (bind, sel) =
                method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(encoder, "setTexture:atIndex:");
            for step in 0..TEXTURES * 3 {
                // Two materials alternating, then a sweep which displaces them.
                let at = match step % 3 {
                    0 => pass % TEXTURES,
                    1 => (pass + 1) % TEXTURES,
                    _ => step % TEXTURES,
                };
                bind(encoder, sel, textures[at], 0);
                set_buffer_at(encoder, outputs[at], 0, 0);
                dispatch(encoder, 1);
                expected[at] += values[at];
            }
            send_void(encoder, "endEncoding");
        }
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        for at in 0..TEXTURES {
            let contents = object::body::<resource::Buffer>(outputs[at]).inner.contents;
            assert_eq!(
                std::ptr::read(contents as *const u32),
                expected[at],
                "texture {at}"
            );
        }
    }
}

/// A set remembered from an earlier encoder, or displaced and written again,
/// still names the texture it was written with.
#[test]
fn a_remembered_descriptor_set_keeps_its_texture_across_encoders() {
    isolate_metal_fixture!();
    if let Some(device) = gpu_device() {
        check_remembered_sets_keep_their_textures(device);
    }
}

#[test]
fn a_remembered_descriptor_set_keeps_its_texture_without_push_descriptors() {
    isolate_metal_fixture!();
    if let Some(device) = gpu_device_without_push_descriptors() {
        check_remembered_sets_keep_their_textures(device);
    }
}

fn check_each_dispatch_reads_its_own_bytes(device: Id) {
    let kernel = kernel_from_source(
        device,
        "kernel void add(device uint* output [[buffer(0)]],\n\
                         constant uint& amount [[buffer(1)]]) {\n\
             output[0] = output[0] + amount;\n\
         }",
        "add",
    );
    const OUTPUTS: usize = 3;
    // Enough copies to pass the end of an upload chunk at any alignment.
    const DISPATCHES: u32 = 20_000;
    const DISPATCHES_PER_ENCODER: u32 = 5_000;
    let mut expected = [0u32; OUTPUTS];
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let outputs: [Id; OUTPUTS] =
            std::array::from_fn(|_| new_buffer(device, std::ptr::null(), 16, 0));
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        let mut encoder: Id = std::ptr::null_mut();
        for step in 0..DISPATCHES {
            if step % DISPATCHES_PER_ENCODER == 0 {
                if !encoder.is_null() {
                    send_void(encoder, "endEncoding");
                }
                encoder = send(command, "computeCommandEncoder");
                send_with_object(encoder, "setComputePipelineState:", kernel);
            }
            let at = step as usize % OUTPUTS;
            let amount = step % 251 + 1;
            set_buffer_at(encoder, outputs[at], 0, 0);
            set_bytes_named(
                encoder,
                "setBytes:length:atIndex:",
                &amount.to_ne_bytes(),
                1,
            );
            dispatch(encoder, 1);
            expected[at] += amount;
        }
        send_void(encoder, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        for at in 0..OUTPUTS {
            let contents = object::body::<resource::Buffer>(outputs[at]).inner.contents;
            assert_eq!(std::ptr::read(contents as *const u32), expected[at]);
        }
    }
}

#[test]
fn each_dispatch_reads_its_own_set_bytes_across_chunks_and_encoders() {
    isolate_metal_fixture!();
    if let Some(device) = gpu_device() {
        check_each_dispatch_reads_its_own_bytes(device);
    }
}

/// Without pushes a remembered set matches whatever its dynamic offsets are,
/// so each dispatch must still be handed the offset of its own copy.
#[test]
fn each_dispatch_reads_its_own_set_bytes_without_push_descriptors() {
    isolate_metal_fixture!();
    if let Some(device) = gpu_device_without_push_descriptors() {
        check_each_dispatch_reads_its_own_bytes(device);
    }
}

/// Samplers are resolved when the pipeline is made and read by position when
/// binding, so each must reach the shader as the one it declared.
#[test]
fn each_constexpr_sampler_keeps_its_own_filter() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let kernel = kernel_from_source(
        device,
        "kernel void probe(device float* output [[buffer(0)]],\n\
                           texture2d<float> source [[texture(0)]]) {\n\
             constexpr sampler near(coord::normalized, address::clamp_to_edge, filter::nearest);\n\
             constexpr sampler smooth(coord::normalized, address::clamp_to_edge, filter::linear);\n\
             output[0] = source.sample(near, float2(0.4, 0.5), level(0.0)).x;\n\
             output[1] = source.sample(smooth, float2(0.5, 0.5), level(0.0)).x;\n\
             output[2] = source.sample(near, float2(0.9, 0.5), level(0.0)).x;\n\
         }",
        "probe",
    );
    let texels: [u8; 8] = [0, 0, 0, 255, 255, 255, 255, 255];
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let texture =
            readable_texture(device, TEXTURE_TYPE_2D, PIXEL_FORMAT_RGBA8_UNORM, [2, 1, 1]);
        replace_region(texture, [0, 0, 2, 1], &texels, 8);
        let output = new_buffer(device, std::ptr::null(), 16, 0);
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", kernel);
        set_buffer_at(encoder, output, 0, 0);
        let (bind, sel) =
            method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(encoder, "setTexture:atIndex:");
        bind(encoder, sel, texture, 0);
        dispatch(encoder, 1);
        send_void(encoder, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        let contents = object::body::<resource::Buffer>(output).inner.contents as *const f32;
        let read = [*contents, *contents.add(1), *contents.add(2)];
        assert!(read[0].abs() < 0.01, "nearest, left texel: {read:?}");
        assert!((read[1] - 0.5).abs() < 0.03, "linear, midpoint: {read:?}");
        assert!(
            (read[2] - 1.0).abs() < 0.01,
            "nearest, right texel: {read:?}"
        );
    }
}

/// Threads racing to compile one variant all answer the one kept in the map,
/// and it runs: the losers' pipelines were destroyed, not the winner's.
#[test]
fn racing_variant_compiles_agree_on_one_pipeline_which_runs() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let kernel = kernel_from_source(device, TEST_KERNEL, "clear");
    const THREADS: usize = 8;
    for width in 1..=16u32 {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
        let threads: Vec<_> = (0..THREADS)
            .map(|_| {
                let barrier = barrier.clone();
                let state = kernel as usize;
                std::thread::spawn(move || {
                    barrier.wait();
                    // SAFETY: a compute pipeline this test made and never frees.
                    let state = unsafe { object::body::<pipeline::ComputePipeline>(state as Id) };
                    state.variant([width, 1, 1]).expect("the variant compiles")
                })
            })
            .collect();
        let made: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().expect("no panic"))
            .collect();
        assert!(made.windows(2).all(|pair| pair[0] == pair[1]), "{made:?}");
    }
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let output = new_buffer(device, std::ptr::null(), 16, 0);
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", kernel);
        set_buffer_at(encoder, output, 0, 0);
        dispatch(encoder, 1);
        send_void(encoder, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        let contents = object::body::<resource::Buffer>(output).inner.contents;
        assert_eq!(std::ptr::read(contents as *const u32), 42);
    }
}

/// Requests round up to a power of two and take the smallest buffer which
/// fits; one larger than the pool ever keeps is made at exactly its size.
#[test]
fn the_staging_pool_recycles_the_smallest_fit() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let gpu = gpu_of(device).expect("a Vulkan device");
    const MIB: u64 = 1 << 20;
    let small = gpu.acquire_staging_buffer(MIB).expect("staging");
    assert_eq!(small.length, MIB);
    assert!(!small.contents.is_null());
    let handle = small.buffer;
    gpu.release_staging_buffer(small);
    let again = gpu.acquire_staging_buffer(MIB - 1).expect("staging");
    assert_eq!(again.buffer, handle);
    gpu.release_staging_buffer(again);

    let huge = gpu.acquire_staging_buffer(65 * MIB).expect("staging");
    assert_eq!(huge.length, 65 * MIB);
    gpu.release_staging_buffer(huge);

    let smaller = gpu.acquire_staging_buffer(8 * MIB).expect("staging");
    let (smaller_buffer, smaller_length) = (smaller.buffer, smaller.length);
    let larger = gpu.acquire_staging_buffer(16 * MIB).expect("staging");
    gpu.release_staging_buffer(larger);
    gpu.release_staging_buffer(smaller);
    let fit = gpu.acquire_staging_buffer(4 * MIB).expect("staging");
    assert_eq!(fit.buffer, smaller_buffer);
    assert_eq!(fit.length, smaller_length);
    gpu.release_staging_buffer(fit);
}

/// Transfers too large for an upload chunk round-trip through recycled
/// staging at tight and padded strides, and none sees another's bytes.
#[test]
fn large_texture_transfers_round_trip_through_recycled_staging() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const SIZE: u64 = 1024;
    const WIDTH: u64 = 600;
    const HEIGHT: u64 = 500;
    const PADDING: usize = 12;
    const ROW: usize = WIDTH as usize * 4;
    const STRIDE: usize = ROW + PADDING;
    let size = [SIZE, SIZE, 1];
    let texture = readable_texture(device, TEXTURE_TYPE_2D, PIXEL_FORMAT_RGBA8_UNORM, size);
    let other = readable_texture(device, TEXTURE_TYPE_2D, PIXEL_FORMAT_RGBA8_UNORM, size);
    for seed in 0..8u32 {
        let whole = texel_pattern(SIZE * SIZE, seed);
        let other_whole = texel_pattern(SIZE * SIZE, !seed);
        // SAFETY: textures this test made, and bytes covering their regions.
        unsafe {
            // Both pending at once, so they must not share a staging buffer.
            replace_region(texture, [0, 0, SIZE, SIZE], &whole, SIZE * 4);
            replace_region(other, [0, 0, SIZE, SIZE], &other_whole, SIZE * 4);

            let mut read = vec![0xEE_u8; whole.len()];
            get_bytes(texture, [0, 0, SIZE, SIZE], &mut read, SIZE * 4);
            assert!(read == whole, "tight read, seed {seed}");
            get_bytes(other, [0, 0, SIZE, SIZE], &mut read, SIZE * 4);
            assert!(read == other_whole, "second texture, seed {seed}");

            // A smaller region out of a larger recycled buffer.
            let mut padded = vec![0xEE_u8; STRIDE * HEIGHT as usize];
            get_bytes(texture, [10, 20, WIDTH, HEIGHT], &mut padded, STRIDE as u64);
            for row in 0..HEIGHT {
                let at = (((20 + row) * SIZE + 10) * 4) as usize;
                let line = &padded[row as usize * STRIDE..][..STRIDE];
                assert!(line[..ROW] == whole[at..at + ROW], "row {row}, seed {seed}");
                assert_eq!(line[ROW..], [0xEE; PADDING], "padding of row {row}");
            }
        }
    }
}

/// A 3D region reads back whole through the one copy of tight strides, and
/// image by image through a padded image stride.
#[test]
fn get_bytes_reads_every_image_of_a_3d_region() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const WIDTH: u64 = 16;
    const HEIGHT: u64 = 8;
    const DEPTH: u64 = 4;
    const ROW: usize = WIDTH as usize * 4;
    const IMAGE: usize = ROW * HEIGHT as usize;
    const PADDING: usize = 32;
    const PADDED_IMAGE: usize = IMAGE + PADDING;
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Region {
        origin: [u64; 3],
        size: [u64; 3],
    }
    let region = Region {
        origin: [0, 0, 0],
        size: [WIDTH, HEIGHT, DEPTH],
    };
    let texture = readable_texture(
        device,
        TEXTURE_TYPE_3D,
        PIXEL_FORMAT_RGBA8_UNORM,
        [WIDTH, HEIGHT, DEPTH],
    );
    let whole = texel_pattern(WIDTH * HEIGHT * DEPTH, 7);
    // SAFETY: a texture this test made, and bytes covering its region.
    unsafe {
        let (replace, sel) =
            method::<unsafe extern "C" fn(Id, Sel, Region, u64, u64, *const u8, u64, u64)>(
                texture,
                "replaceRegion:mipmapLevel:slice:withBytes:bytesPerRow:bytesPerImage:",
            );
        replace(
            texture,
            sel,
            region,
            0,
            0,
            whole.as_ptr(),
            ROW as u64,
            IMAGE as u64,
        );
        let (read, sel) =
            method::<unsafe extern "C" fn(Id, Sel, *mut u8, u64, u64, Region, u64, u64)>(
                texture,
                "getBytes:bytesPerRow:bytesPerImage:fromRegion:mipmapLevel:slice:",
            );
        let mut tight = vec![0xEE_u8; whole.len()];
        read(
            texture,
            sel,
            tight.as_mut_ptr(),
            ROW as u64,
            IMAGE as u64,
            region,
            0,
            0,
        );
        assert!(tight == whole, "tight strides");

        let mut padded = vec![0xEE_u8; PADDED_IMAGE * DEPTH as usize];
        read(
            texture,
            sel,
            padded.as_mut_ptr(),
            ROW as u64,
            PADDED_IMAGE as u64,
            region,
            0,
            0,
        );
        for image in 0..DEPTH as usize {
            let line = &padded[image * PADDED_IMAGE..][..PADDED_IMAGE];
            assert!(
                line[..IMAGE] == whole[image * IMAGE..][..IMAGE],
                "image {image}"
            );
            assert_eq!(line[IMAGE..], [0xEE; PADDING], "padding of image {image}");
        }
    }
}

/// `-newBufferWithBytes:` does not zero its memory first, so a later
/// `-newBufferWithLength:` over the same range must still read as zeros.
#[test]
fn a_new_buffer_reads_zero_over_a_range_bytes_were_written_to() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const PRIVATE: u64 = 2 << 4;
    const ROUNDS: usize = 8;
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let queue = new_command_queue(device, std::ptr::null());
        for length in [4usize << 10, 64 << 10, 1 << 20, 8 << 20] {
            let bytes = vec![0xAB_u8; length];
            for round in 0..ROUNDS {
                let dirty =
                    new_buffer_with_bytes(device, std::ptr::null(), bytes.as_ptr(), length, 0);
                let contents = object::body::<resource::Buffer>(dirty).inner.contents;
                assert!(std::slice::from_raw_parts(contents, length) == &bytes[..]);
                objc::arc::release(dirty);
                let clean = new_buffer(device, std::ptr::null(), length, 0);
                let contents = object::body::<resource::Buffer>(clean).inner.contents;
                assert!(
                    std::slice::from_raw_parts(contents, length)
                        .iter()
                        .all(|byte| *byte == 0),
                    "shared, {length} bytes, round {round}"
                );
                objc::arc::release(clean);
            }
            let read_back = new_buffer(device, std::ptr::null(), length, 0);
            let read = |private: Id| {
                let command = send(queue, "commandBuffer");
                let blit = send(command, "blitCommandEncoder");
                copy_buffer(blit, private, read_back, length as u64);
                send_void(blit, "endEncoding");
                send_void(command, "commit");
                send_void(command, "waitUntilCompleted");
                std::slice::from_raw_parts(
                    object::body::<resource::Buffer>(read_back).inner.contents,
                    length,
                )
            };
            for round in 0..ROUNDS {
                let dirty = new_buffer_with_bytes(
                    device,
                    std::ptr::null(),
                    bytes.as_ptr(),
                    length,
                    PRIVATE,
                );
                assert!(read(dirty) == &bytes[..], "private bytes, round {round}");
                objc::arc::release(dirty);
                let clean = new_buffer(device, std::ptr::null(), length, PRIVATE);
                assert!(
                    read(clean).iter().all(|byte| *byte == 0),
                    "private, {length} bytes, round {round}"
                );
                objc::arc::release(clean);
            }
            objc::arc::release(read_back);
        }
    }
}

/// The resolve does not zero the space its results go in, so what earlier
/// uploads left in a recycled chunk must not reach the visibility buffer.
#[test]
fn visibility_results_ignore_a_dirty_recycled_upload_chunk() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let Some(fixture) = covering_draw(device) else {
        return;
    };
    let kernel = kernel_from_source(device, TEST_KERNEL, "clear");
    let junk = [0xFF_u8; 4096];
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        for _ in 0..4 {
            let command = send(fixture.queue, "commandBuffer");
            let encoder = send(command, "computeCommandEncoder");
            send_with_object(encoder, "setComputePipelineState:", kernel);
            for _ in 0..400 {
                set_bytes_named(encoder, "setBytes:length:atIndex:", &junk, 0);
                dispatch(encoder, 1);
            }
            send_void(encoder, "endEncoding");
            send_void(command, "commit");
            send_void(command, "waitUntilCompleted");
        }
        for _ in 0..4 {
            let results = results_buffer(device, 2, 0x1234);
            let pass = fixture.pass(results);
            let command = send(fixture.queue, "commandBuffer");
            let render = render_encoder(command, pass);
            fixture.bind(render);
            set_visibility_mode(render, VISIBILITY_COUNTING, 0);
            draw(render, 3);
            set_visibility_mode(render, VISIBILITY_BOOLEAN, 8);
            draw(render, 3);
            send_void(render, "endEncoding");
            send_void(command, "commit");
            send_void(command, "waitUntilCompleted");
            assert_eq!(result_at(results, 0), COVERING_SIZE * COVERING_SIZE);
            assert_ne!(result_at(results, 1), 0);
            assert_ne!(result_at(results, 1), u64::MAX);
        }
    }
}

/// What the cache saves is in a form the driver takes back: a new cache made
/// from the file holds more than an empty one.
#[test]
fn a_saved_pipeline_cache_is_taken_back_by_the_driver() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let gpu = gpu_of(device).expect("a Vulkan device");
    if gpu.pipeline_cache_path().is_none() {
        return;
    }
    let kernel = kernel_from_source(device, TEST_KERNEL, "clear");
    // SAFETY: a compute pipeline this test made.
    let state = unsafe { object::body::<pipeline::ComputePipeline>(kernel) };
    for width in 1..=8 {
        state.variant([width, 1, 1]).expect("the variant compiles");
    }
    gpu.save_pipeline_cache();
    let empty = gpu
        .reopened_pipeline_cache_bytes(Some(&[]))
        .expect("a cache");
    // SAFETY: the device's own cache.
    let held = unsafe { gpu.device.get_pipeline_cache_data(gpu.pipeline_cache()) }
        .expect("cache data")
        .len();
    if held <= empty {
        // A driver is free to keep nothing, and then has nothing to take back.
        return;
    }
    let reopened = gpu
        .reopened_pipeline_cache_bytes(None)
        .expect("the saved file is whole");
    assert!(
        reopened > empty,
        "{reopened} bytes against {empty} when empty"
    );
}

/// A guest which is killed keeps most of what it compiled: enough new
/// pipelines save the cache without the process exiting.
#[test]
fn the_pipeline_cache_saves_after_enough_new_pipelines_without_exiting() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let gpu = gpu_of(device).expect("a Vulkan device");
    let Some(path) = gpu.pipeline_cache_path() else {
        return;
    };
    let modified = || {
        std::fs::metadata(path)
            .and_then(|file| file.modified())
            .ok()
    };
    let before = modified();
    let kernel = kernel_from_source(device, TEST_KERNEL, "clear");
    // SAFETY: a compute pipeline this test made.
    let state = unsafe { object::body::<pipeline::ComputePipeline>(kernel) };
    // Past the save interval, whatever already counted towards it.
    for width in 1..=70 {
        state.variant([width, 1, 1]).expect("the variant compiles");
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while modified() == before {
        assert!(
            std::time::Instant::now() < deadline,
            "no save after 70 new pipelines"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(gpu.reopened_pipeline_cache_bytes(None).is_some());
}
