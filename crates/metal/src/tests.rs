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
fn exported_attachment_descriptors_can_be_created_with_native_defaults() {
    isolate_metal_fixture!();
    let symbols = data_symbols();
    for (name, properties) in [
        (
            "MTLVertexAttributeDescriptor",
            [("format", 0), ("offset", 0), ("bufferIndex", 0)],
        ),
        (
            "MTLVertexBufferLayoutDescriptor",
            [("stride", 0), ("stepFunction", 1), ("stepRate", 1)],
        ),
        (
            "MTLRenderPipelineColorAttachmentDescriptor",
            [
                ("pixelFormat", 0),
                ("sourceRGBBlendFactor", 1),
                ("writeMask", 15),
            ],
        ),
        (
            "MTLRenderPassColorAttachmentDescriptor",
            [("loadAction", 0), ("storeAction", 1), ("level", 0)],
        ),
        (
            "MTLRenderPassDepthAttachmentDescriptor",
            [("loadAction", 2), ("storeAction", 0), ("level", 0)],
        ),
        (
            "MTLRenderPassStencilAttachmentDescriptor",
            [("loadAction", 2), ("storeAction", 0), ("level", 0)],
        ),
    ] {
        let symbol = format!("_OBJC_CLASS_$_{name}");
        let address = symbols
            .iter()
            .find(|(name, _)| name == &symbol)
            .expect("exported class")
            .1;
        // SAFETY: the exported class and its registered scalar properties.
        unsafe {
            let descriptor = send(address as Id, "new");
            assert!(!descriptor.is_null());
            for (getter, expected) in properties {
                assert_eq!(
                    send_number_result(descriptor, getter),
                    expected,
                    "{name}.{getter}"
                );
            }
            objc::arc::release(descriptor);
        }
    }
}

#[test]
fn render_pass_attachment_descriptors_keep_native_defaults_after_reset() {
    isolate_metal_fixture!();
    bootstrap();
    let class = objc::class::lookup(c"MTLRenderPassDescriptor").unwrap();
    // SAFETY: owned descriptor and its live attachment objects.
    unsafe {
        let pass = send(objc::class::ptr(class) as Id, "new");
        let colors = send(pass, "colorAttachments");
        let color = element_at(colors, 0);
        let depth = send(pass, "depthAttachment");
        let stencil = send(pass, "stencilAttachment");
        for attachment in [color, depth, stencil] {
            send_number(attachment, "setLoadAction:", 1);
            send_number(attachment, "setStoreAction:", 2);
        }
        let (assign, sel) = method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(
            colors,
            "setObject:atIndexedSubscript:",
        );
        assign(colors, sel, std::ptr::null_mut(), 0);
        send_with_object(pass, "setDepthAttachment:", std::ptr::null_mut());
        send_with_object(pass, "setStencilAttachment:", std::ptr::null_mut());
        let color = element_at(colors, 0);
        let depth = send(pass, "depthAttachment");
        let stencil = send(pass, "stencilAttachment");
        for (attachment, load, store) in [(color, 0, 1), (depth, 2, 0), (stencil, 2, 0)] {
            assert_eq!(send_number_result(attachment, "loadAction"), load);
            assert_eq!(send_number_result(attachment, "storeAction"), store);
        }
        let (clear_color, sel) =
            method::<unsafe extern "C" fn(Id, Sel) -> encoder::ClearColor>(color, "clearColor");
        let clear = clear_color(color, sel);
        assert_eq!(
            [clear.red, clear.green, clear.blue, clear.alpha],
            [0.0, 0.0, 0.0, 1.0]
        );
        assert_eq!(send_double(depth, "clearDepth"), 1.0);
        objc::arc::release(pass);
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

#[test]
fn texture_descriptor_copies_preserve_settings_and_are_independent() {
    isolate_metal_fixture!();
    bootstrap();
    let class = objc::class::lookup(c"MTLTextureDescriptor").expect("registered");
    let fields = [
        ("textureType", "setTextureType:", MTL_TEXTURE_TYPE_CUBE),
        (
            "pixelFormat",
            "setPixelFormat:",
            MTL_PIXEL_FORMAT_RGBA8_UNORM,
        ),
        ("width", "setWidth:", 64),
        ("height", "setHeight:", 32),
        ("depth", "setDepth:", 2),
        ("mipmapLevelCount", "setMipmapLevelCount:", 3),
        ("sampleCount", "setSampleCount:", 4),
        ("arrayLength", "setArrayLength:", 6),
        ("storageMode", "setStorageMode:", MTL_STORAGE_MODE_MANAGED),
        (
            "usage",
            "setUsage:",
            gpu::USAGE_SHADER_READ | gpu::USAGE_RENDER_TARGET,
        ),
    ];
    // SAFETY: objects this test owns, sent their registered selectors.
    unsafe {
        let original = send(send(objc::class::ptr(class) as Id, "alloc"), "init");
        for &(_, setter, value) in &fields {
            send_number(original, setter, value);
        }
        let (copy_with_zone, sel) = method::<
            unsafe extern "C" fn(Id, Sel, *mut std::ffi::c_void) -> Id,
        >(original, "copyWithZone:");
        let copies = [
            copy_with_zone(original, sel, std::ptr::null_mut()),
            send(original, "copy"),
        ];
        assert_ne!(copies[0], copies[1]);
        for &copy in &copies {
            assert!(!copy.is_null());
            assert_ne!(copy, original);
            assert_eq!(send_number_result(copy, "retainCount"), 1);
            for &(getter, _, value) in &fields {
                assert_eq!(send_number_result(copy, getter), value, "{getter}");
            }
        }
        send_number(original, "setWidth:", 128);
        send_number(copies[0], "setHeight:", 16);
        assert_eq!(send_number_result(original, "height"), 32);
        assert_eq!(send_number_result(copies[1], "height"), 32);
        objc::arc::release(original);
        for copy in copies {
            assert_eq!(send_number_result(copy, "width"), 64);
            objc::arc::release(copy);
        }
    }
}

#[test]
fn texture_descriptor_copies_report_native_zone_argument() {
    isolate_metal_fixture!();
    bootstrap();
    let class = objc::class::lookup(c"MTLTextureDescriptor").expect("registered");
    let copy_sel = objc::sel::get("copyWithZone:");
    let registered = class.method(copy_sel).expect("registered");
    // SAFETY: the registered method and objects created by this test.
    unsafe {
        let types = objc::api::method_getTypeEncoding(std::ptr::from_ref(registered).cast());
        // Captured from macOS 15.6.1, Metal.framework 368.52.
        assert_eq!(std::ffi::CStr::from_ptr(types), c"@24@0:8^{_NSZone=}16");
        let pool = objc::arc::pool_push();
        let descriptor = send(send(objc::class::ptr(class) as Id, "alloc"), "init");
        let (signature, sel) = method::<unsafe extern "C" fn(Id, Sel, Sel) -> Id>(
            descriptor,
            "methodSignatureForSelector:",
        );
        let signature = signature(descriptor, sel, copy_sel);
        assert!(!signature.is_null());
        assert_eq!(send_number_result(signature, "numberOfArguments"), 3);
        let (argument, sel) = method::<unsafe extern "C" fn(Id, Sel, u64) -> *const std::ffi::c_char>(
            signature,
            "getArgumentTypeAtIndex:",
        );
        assert_eq!(
            std::ffi::CStr::from_ptr(argument(signature, sel, 2)),
            c"^{_NSZone=}"
        );
        objc::arc::release(descriptor);
        objc::arc::pool_pop(pool);
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
/// copy into it imports only the range it names and a binding the whole
/// buffer, over pages a copy already imported. Each must see what the host
/// wrote, and the host what each wrote.
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
        let small_contents = object::body::<resource::Buffer>(small).inner.contents;
        std::ptr::copy_nonoverlapping([1u32, 2, 3, 4].as_ptr(), small_contents.cast(), 4);
        let queue = new_command_queue(device, std::ptr::null());

        let command = send(queue, "commandBuffer");
        let blit = send(command, "blitCommandEncoder");
        copy_buffer_range(blit, small, 0, large, OFFSET as u64, 16);
        send_void(blit, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        assert_eq!(
            std::slice::from_raw_parts(contents.add(OFFSET) as *const u32, 4),
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

/// Bytes a large shared buffer copy reads, written by the host after the copy
/// was encoded but before commit, never importing the source.
#[test]
fn a_copy_out_of_host_pages_reads_them_as_committed() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const LENGTH: usize = 4 << 20;
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let large = new_buffer(device, std::ptr::null(), LENGTH, 0);
        let small = new_buffer(device, std::ptr::null(), 16, 0);
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        let blit = send(command, "blitCommandEncoder");
        copy_buffer_range(blit, large, (LENGTH - 16) as u64, small, 0, 16);
        send_void(blit, "endEncoding");
        let contents = object::body::<resource::Buffer>(large).inner.contents;
        std::ptr::copy_nonoverlapping(
            [1u32, 2, 3, 4].as_ptr(),
            contents.add(LENGTH - 16).cast(),
            4,
        );
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        let small_contents = object::body::<resource::Buffer>(small).inner.contents;
        assert_eq!(
            std::slice::from_raw_parts(small_contents as *const u32, 4),
            [1, 2, 3, 4]
        );
        let pages = object::body::<resource::Buffer>(large).host_pages();
        assert!(pages.is_some_and(|pages| !pages.imported()));
        for buffer in [large, small] {
            objc::arc::release(buffer);
        }
    }
}

/// A copy encoded before the device was given its source, committed after
/// work which writes that source, reads what the work wrote.
#[test]
fn a_copy_out_of_host_pages_follows_device_writes_queued_before_it() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const LENGTH: usize = 4 << 20;
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let large = new_buffer(device, std::ptr::null(), LENGTH, 0);
        let source = new_buffer(device, std::ptr::null(), 16, 0);
        let small = new_buffer(device, std::ptr::null(), 16, 0);
        let source_contents = object::body::<resource::Buffer>(source).inner.contents;
        std::ptr::copy_nonoverlapping([5u32, 6, 7, 8].as_ptr(), source_contents.cast(), 4);
        let queue = new_command_queue(device, std::ptr::null());

        let reader = send(queue, "commandBuffer");
        let blit = send(reader, "blitCommandEncoder");
        copy_buffer_range(blit, large, 0, small, 0, 16);
        send_void(blit, "endEncoding");

        let writer = send(queue, "commandBuffer");
        let blit = send(writer, "blitCommandEncoder");
        copy_buffer_range(blit, source, 0, large, 0, 16);
        send_void(blit, "endEncoding");
        send_void(writer, "commit");

        send_void(reader, "commit");
        send_void(reader, "waitUntilCompleted");
        let small_contents = object::body::<resource::Buffer>(small).inner.contents;
        assert_eq!(
            std::slice::from_raw_parts(small_contents as *const u32, 4),
            [5, 6, 7, 8]
        );
        for buffer in [large, source, small] {
            objc::arc::release(buffer);
        }
    }
}

/// A descriptor set written for one binding of resources is used again by a
/// later dispatch in the command buffer which binds the same ones, and each
/// dispatch still reads what was bound for it.
#[test]
fn a_texture_bound_again_in_one_command_buffer_is_the_one_read() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const R32_UINT: u64 = 53; // MTLPixelFormatR32Uint
    const DISPATCHES: usize = 6;
    let source = "kernel void probe(texture2d<uint, access::read> source [[texture(0)]], \
                  device uint* out [[buffer(0)]]) { out[0] = source.read(uint2(0, 0)).x; }";
    let gpu = gpu_of(device).expect("a Vulkan device");
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let library = source_library(device, source);
        let function = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("probe"),
        );
        let mut error = std::ptr::null_mut();
        let kernel = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        assert!(!kernel.is_null());
        let textures = [1u32, 2].map(|value| {
            let texture = resource::new_texture(
                gpu,
                resource::TextureDescriptor {
                    texture_type: 2,
                    pixel_format: R32_UINT,
                    width: 1,
                    height: 1,
                    depth: 1,
                    mipmap_level_count: 1,
                    sample_count: 1,
                    array_length: 1,
                    storage_mode: 0,
                    usage: gpu::USAGE_SHADER_READ,
                },
            );
            replace_region(texture, [0, 0, 1, 1], &value.to_le_bytes(), 4);
            texture
        });
        let output = new_buffer(device, std::ptr::null(), DISPATCHES * 4, 0);
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", kernel);
        let (bind, sel) =
            method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(encoder, "setTexture:atIndex:");
        for index in 0..DISPATCHES {
            set_buffer_at(encoder, output, (index * 4) as u64, 0);
            bind(encoder, sel, textures[index % 2], 0);
            dispatch(encoder, 1);
        }
        send_void(encoder, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        let contents = object::body::<resource::Buffer>(output).inner.contents as *const u32;
        assert_eq!(
            std::slice::from_raw_parts(contents, DISPATCHES),
            [1, 2, 1, 2, 1, 2]
        );
    }
}

/// Command buffers committed faster than they reach the queue are submitted
/// together, and each still completes with its own work done.
#[test]
fn command_buffers_committed_back_to_back_each_complete() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const COUNT: usize = 256;
    const COMPLETED: u64 = 4; // MTLCommandBufferStatusCompleted
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let source = new_buffer(device, std::ptr::null(), COUNT * 4, 0);
        let target = new_buffer(device, std::ptr::null(), COUNT * 4, 0);
        let values: Vec<u32> = (1..=COUNT as u32).collect();
        let source_contents = object::body::<resource::Buffer>(source).inner.contents;
        std::ptr::copy_nonoverlapping(values.as_ptr(), source_contents.cast(), COUNT);
        let queue = new_command_queue(device, std::ptr::null());
        let commands: Vec<Id> = (0..COUNT)
            .map(|index| {
                let command = objc::arc::retain(send(queue, "commandBuffer"));
                let blit = send(command, "blitCommandEncoder");
                let offset = (index * 4) as u64;
                copy_buffer_range(blit, source, offset, target, offset, 4);
                send_void(blit, "endEncoding");
                send_void(command, "commit");
                command
            })
            .collect();
        send_void(commands[COUNT - 1], "waitUntilCompleted");
        for &command in &commands {
            send_void(command, "waitUntilCompleted");
            assert_eq!(send_number_result(command, "status"), COMPLETED);
            objc::arc::release(command);
        }
        let target_contents = object::body::<resource::Buffer>(target).inner.contents;
        assert_eq!(
            std::slice::from_raw_parts(target_contents as *const u32, COUNT),
            values
        );
        for buffer in [source, target] {
            objc::arc::release(buffer);
        }
    }
}

/// Work queued behind an event wait runs once the host signals it, so a copy
/// there reads what the host wrote after committing it.
#[test]
fn a_copy_out_of_host_pages_behind_an_event_wait_reads_them_as_signalled() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const LENGTH: usize = 4 << 20;
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let event = send(device, "newSharedEvent");
        assert!(!event.is_null());
        let large = new_buffer(device, std::ptr::null(), LENGTH, 0);
        let small = new_buffer(device, std::ptr::null(), 16, 0);
        let queue = new_command_queue(device, std::ptr::null());
        let waiting = send(queue, "commandBuffer");
        let (encode, wait) =
            method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(waiting, "encodeWaitForEvent:value:");
        encode(waiting, wait, event, 1);
        send_void(waiting, "commit");

        let command = send(queue, "commandBuffer");
        let blit = send(command, "blitCommandEncoder");
        copy_buffer_range(blit, large, 0, small, 0, 16);
        send_void(blit, "endEncoding");
        send_void(command, "commit");

        let contents = object::body::<resource::Buffer>(large).inner.contents;
        std::ptr::copy_nonoverlapping([9u32, 10, 11, 12].as_ptr(), contents.cast(), 4);
        let (set_value, sel) =
            method::<unsafe extern "C" fn(Id, Sel, u64)>(event, "setSignaledValue:");
        set_value(event, sel, 1);
        send_void(command, "waitUntilCompleted");
        let small_contents = object::body::<resource::Buffer>(small).inner.contents;
        assert_eq!(
            std::slice::from_raw_parts(small_contents as *const u32, 4),
            [9, 10, 11, 12]
        );
        send_void(waiting, "waitUntilCompleted");
        let gpu = gpu_of(device).expect("a Vulkan device");
        assert_eq!(
            gpu.event_waits_in_flight
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
        for buffer in [large, small] {
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
                early_fragment_tests: false,
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
                early_fragment_tests: false,
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
    if let Some(device) = gpu_device() {
        check_depth_resolve(device);
    }
}

#[test]
fn a_multisampled_depth_attachment_resolves_without_dynamic_rendering() {
    isolate_metal_fixture!();
    if let Some(device) = gpu_device_without_dynamic_rendering() {
        check_depth_resolve(device);
    }
}

fn check_depth_resolve(device: Id) {
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
            input_topology: 0,
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
        if gpu.dynamic_rendering {
            // The resolve is the pass's, not the pipeline's: the draw used
            // the pipeline compiled when the state was made.
            let state = object::body::<pipeline::RenderPipeline>(state);
            assert_eq!(state.variant_count(), 1);
        }
    }
}

/// A render pipeline descriptor drawing `triangle_library`'s triangle into
/// one BGRA8 target, owned by the caller.
fn triangle_pipeline_descriptor(device: Id) -> Id {
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
            depth_format: 0,
            stencil_format: 0,
            sample_count: 1,
            alpha_to_coverage: false,
            alpha_to_one: false,
            rasterization: true,
            input_topology: 0,
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
    descriptor
}

/// A global block whose `invoke` is `imp`: copying one is a no-op, so a
/// test needs no copy or dispose helpers.
#[repr(C)]
struct GlobalBlock {
    isa: *const std::ffi::c_void,
    flags: i32,
    reserved: i32,
    invoke: *const std::ffi::c_void,
    descriptor: *const [u64; 2],
}

/// `Block_descriptor_1`: reserved, then the block's size.
static GLOBAL_BLOCK_DESCRIPTOR: [u64; 2] = [0, size_of::<GlobalBlock>() as u64];

impl GlobalBlock {
    fn new(imp: *const std::ffi::c_void) -> Box<Self> {
        let isa = objc::class::lookup(c"__NSGlobalBlock__")
            .map(|class| objc::class::ptr(class) as *const std::ffi::c_void)
            .expect("registered by `objc::block`");
        Box::new(Self {
            isa,
            // BLOCK_IS_GLOBAL.
            flags: 1 << 28,
            reserved: 0,
            invoke: imp,
            descriptor: &GLOBAL_BLOCK_DESCRIPTOR,
        })
    }

    fn id(&self) -> Id {
        self as *const Self as Id
    }
}

/// What a creation handler was handed, retained, and the thread it ran on.
static CREATED: Mutex<Option<(usize, bool, std::thread::ThreadId)>> = Mutex::new(None);
static CREATED_READY: std::sync::Condvar = std::sync::Condvar::new();

unsafe extern "C" fn record_creation(_block: Id, object: Id, error: Id) {
    *CREATED.lock().expect("creation record") = Some((
        objc::arc::retain(object) as usize,
        error.is_null(),
        std::thread::current().id(),
    ));
    CREATED_READY.notify_all();
}

/// `newRenderPipelineStateWithDescriptor:completionHandler:` compiles on
/// another thread, from the descriptor as it was when the call was made.
#[test]
fn an_asynchronous_render_pipeline_compiles_off_the_calling_thread() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let descriptor = triangle_pipeline_descriptor(device);
    let block = GlobalBlock::new(record_creation as *const std::ffi::c_void);
    // SAFETY: a device, descriptor and handler this test made.
    unsafe {
        compile::new_render_pipeline(device, std::ptr::null(), descriptor, block.id());
        // Three samples is no sample count: compiled from the live
        // descriptor, the pipeline would fail.
        send_number(descriptor, "setSampleCount:", 3);
    }
    let (created, _) = CREATED_READY
        .wait_timeout_while(
            CREATED.lock().expect("creation record"),
            std::time::Duration::from_secs(60),
            |created| created.is_none(),
        )
        .expect("creation record");
    let (state, no_error, thread) = created.expect("the handler ran");
    assert!(state != 0 && no_error, "the pipeline was not made");
    assert_ne!(thread, std::thread::current().id());
    if gpu_of(device)
        .expect("a Vulkan device")
        .dynamic_render_state
    {
        // SAFETY: the render pipeline state the handler was given.
        let state = unsafe { object::body::<pipeline::RenderPipeline>(state as Id) };
        assert_eq!(
            state.variant_count(),
            1,
            "compiled before it was handed over"
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
            input_topology: 0,
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

/// Copies queued for one submission overlap on the device unless they reach
/// the same texels: a later copy over part of a texture lands after the copy
/// of the whole, and a copy into another texture between them is untouched.
#[test]
fn a_later_copy_over_the_same_texels_lands_after_an_earlier_one() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const SIZE: u64 = 8;
    let gpu = gpu_of(device).expect("a Vulkan device");
    let texture = || {
        resource::new_texture(
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
        )
    };
    let (patched, other) = (texture(), texture());
    let whole = vec![0x11u8; (SIZE * SIZE * 4) as usize];
    let patch = vec![0x22u8; 4 * 4 * 4];
    let elsewhere = vec![0x33u8; (SIZE * SIZE * 4) as usize];
    // SAFETY: textures this test made, and bytes covering each region.
    unsafe {
        replace_region(patched, [0, 0, SIZE, SIZE], &whole, SIZE * 4);
        replace_region(other, [0, 0, SIZE, SIZE], &elsewhere, SIZE * 4);
        replace_region(patched, [2, 2, 4, 4], &patch, 4 * 4);
    }
    let mut read = vec![0u8; (SIZE * SIZE * 4) as usize];
    // SAFETY: the buffer covers the texture at its tight stride.
    unsafe { get_bytes(patched, [0, 0, SIZE, SIZE], &mut read, SIZE * 4) };
    for (index, texel) in read.chunks(4).enumerate() {
        let (x, y) = (index as u64 % SIZE, index as u64 / SIZE);
        let inside = (2..6).contains(&x) && (2..6).contains(&y);
        let expected = if inside { 0x22 } else { 0x11 };
        assert_eq!(texel, [expected; 4], "texel ({x}, {y})");
    }
    // SAFETY: as above.
    unsafe { get_bytes(other, [0, 0, SIZE, SIZE], &mut read, SIZE * 4) };
    assert!(read.iter().all(|byte| *byte == 0x33));
    objc::arc::release(patched);
    objc::arc::release(other);
}

/// The time a burst of texture streaming takes to reach the device: textures
/// made and filled mip by mip, as a game loading assets fills them, then the
/// submission they run ahead of. Run with `cargo test --release -p metal --
/// --ignored --nocapture streaming_upload_cost`.
#[test]
#[ignore = "a benchmark, not a check"]
fn streaming_upload_cost() {
    let Some(device) = gpu_device() else {
        return;
    };
    const TEXTURES: usize = 64;
    const SIZE: u64 = 256;
    const LEVELS: u64 = 9;
    const ROUNDS: usize = 9;
    let gpu = gpu_of(device).expect("a Vulkan device");
    let bytes = vec![0x5Au8; (SIZE * SIZE * 4) as usize];
    // SAFETY: a device of this crate's.
    let queue = unsafe { new_command_queue(device, std::ptr::null()) };
    let mut times = Vec::new();
    for _ in 0..ROUNDS {
        let start = std::time::Instant::now();
        let textures: Vec<Id> = (0..TEXTURES)
            .map(|_| {
                resource::new_texture(
                    gpu,
                    resource::TextureDescriptor {
                        texture_type: 2,
                        pixel_format: 70, // RGBA8Unorm
                        width: SIZE,
                        height: SIZE,
                        depth: 1,
                        mipmap_level_count: LEVELS,
                        sample_count: 1,
                        array_length: 1,
                        storage_mode: 0,
                        usage: gpu::USAGE_SHADER_READ,
                    },
                )
            })
            .collect();
        // SAFETY: textures this test made, and bytes covering each level.
        unsafe {
            let sel = objc::sel::get("replaceRegion:mipmapLevel:withBytes:bytesPerRow:");
            let method = objc::object::class_of(textures[0])
                .and_then(|class| class.method(sel))
                .expect("the selector is registered");
            let call: unsafe extern "C" fn(Id, Sel, [u64; 6], u64, *const u8, u64) =
                std::mem::transmute(method.implementation());
            for &texture in &textures {
                for level in 0..LEVELS {
                    let side = (SIZE >> level).max(1);
                    call(
                        texture,
                        sel,
                        [0, 0, 0, side, side, 1],
                        level,
                        bytes.as_ptr(),
                        side * 4,
                    );
                }
            }
            let command = send(queue, "commandBuffer");
            send_void(command, "commit");
            send_void(command, "waitUntilCompleted");
        }
        times.push(start.elapsed().as_secs_f64());
        for texture in textures {
            objc::arc::release(texture);
        }
    }
    times.sort_by(f64::total_cmp);
    eprintln!(
        "{TEXTURES} textures of {LEVELS} levels made, filled and submitted: {:.2} ms median, {:.2} ms best",
        times[ROUNDS / 2] * 1e3,
        times[0] * 1e3,
    );
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
/// `texture` must be an `RosemaryMTLTexture`, and `bytes` must cover the
/// region at `bytes_per_row`.
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
                input_topology: 0,
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

const RESOURCE_DRAW_SOURCE: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct VertexIn { float4 position [[attribute(0)]]; };
struct VertexOut { float4 position [[position]]; float2 uv; };
struct Uniforms { float4x4 transform; float4 tint; };
vertex VertexOut resource_vertex(VertexIn in [[stage_in]],
                                 constant Uniforms& uniforms [[buffer(1)]],
                                 constant float4* instance [[buffer(2)]]) {
    VertexOut out;
    out.position = uniforms.transform * in.position + instance[0];
    out.uv = in.position.xy;
    return out;
}
fragment float4 resource_fragment(VertexOut in [[stage_in]],
                                  texture2d<float> albedo [[texture(0)]],
                                  texture2d<float> detail [[texture(1)]],
                                  sampler linear [[sampler(0)]],
                                  constant float4& tint [[buffer(0)]]) {
    return albedo.sample(linear, in.uv) * detail.sample(linear, in.uv) * tint;
}
"#;

/// The pipeline of [`RESOURCE_DRAW_SOURCE`]: a float4 vertex stream at index
/// 0, drawing into one `BGRA8Unorm` attachment.
fn resource_draw_pipeline(device: Id) -> Id {
    const FLOAT4: u64 = 31; // MTLVertexFormatFloat4
    let library = source_library(device, RESOURCE_DRAW_SOURCE);
    assert!(!library.is_null(), "the benchmark shaders must translate");
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let vertex = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("resource_vertex"),
        );
        let fragment = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("resource_fragment"),
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
        send_number(attribute, "setFormat:", FLOAT4);
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
                input_topology: 0,
                color_objects: std::ptr::null_mut(),
            },
        );
        send_number(
            element_at(send(descriptor, "colorAttachments"), 0),
            "setPixelFormat:",
            BGRA8,
        );
        let mut error = std::ptr::null_mut();
        let state = new_render_pipeline(device, std::ptr::null(), descriptor, &mut error);
        assert!(!state.is_null(), "the benchmark pipeline must build");
        state
    }
}

/// `MTLPixelFormatBGRA8Unorm`.
const BGRA8: u64 = 80;

/// The CPU cost of a draw which binds what a game's draws bind: a vertex
/// stream, uniform and instance buffers at moving offsets, two textures, a
/// sampler and a few bytes of constants, with the textures and offsets
/// changing from draw to draw. Run with `cargo test --release -p metal --
/// --ignored --nocapture recording_cost_with_resources`.
#[test]
#[ignore = "a benchmark, not a check"]
fn recording_cost_with_resources() {
    let Some(device) = gpu_device() else {
        return;
    };
    // Shaped like a game's frame: tens of passes of tens of draws each.
    const FRAMES: usize = 25;
    const ENCODERS_PER_FRAME: usize = 40;
    const DRAWS_PER_ENCODER: usize = 20;
    const DRAWS: usize = FRAMES * ENCODERS_PER_FRAME * DRAWS_PER_ENCODER;
    const ROUNDS: usize = 5;
    const RGBA8: u64 = 70; // MTLPixelFormatRGBA8Unorm
    const UINT16: u64 = 0; // MTLIndexTypeUInt16
    const TRIANGLES: u64 = 3; // MTLPrimitiveTypeTriangle
    const UNIFORM_STRIDE: u64 = 256;
    let gpu = gpu_of(device).expect("a Vulkan device");
    let state = resource_draw_pipeline(device);
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let texture = |pixel_format, usage| {
            resource::new_texture(
                gpu,
                resource::TextureDescriptor {
                    texture_type: 2,
                    pixel_format,
                    width: 64,
                    height: 64,
                    depth: 1,
                    mipmap_level_count: 1,
                    sample_count: 1,
                    array_length: 1,
                    storage_mode: 2,
                    usage,
                },
            )
        };
        let target = texture(BGRA8, gpu::USAGE_RENDER_TARGET);
        let textures: Vec<Id> = (0..8)
            .map(|_| texture(RGBA8, gpu::USAGE_SHADER_READ))
            .collect();
        let sampler = resource::new_sampler(gpu, &resource::SamplerDescriptor::default());
        let vertices = new_buffer(device, std::ptr::null(), 64 * 16, 0);
        let indices = new_buffer(device, std::ptr::null(), 64 * 2, 0);
        let uniforms = new_buffer(device, std::ptr::null(), 64 * UNIFORM_STRIDE as usize, 0);
        let instances = new_buffer(device, std::ptr::null(), 64 * 16, 0);
        let queue = new_command_queue(device, std::ptr::null());
        let pass = object::make(
            c"MTLRenderPassDescriptor",
            encoder::RenderPassDescriptor::default(),
        );
        let attachment = element_at(send(pass, "colorAttachments"), 0);
        send_with_object(attachment, "setTexture:", target);
        send_number(attachment, "setLoadAction:", 2);
        send_number(attachment, "setStoreAction:", 1);

        let mut best = f64::MAX;
        let mut best_cpu = f64::MAX;
        for _ in 0..ROUNDS {
            let pool = objc::arc::pool_push();
            let start = std::time::Instant::now();
            let cpu_start = thread_cpu_time();
            let mut last = std::ptr::null_mut();
            for _ in 0..FRAMES {
                let command = send(queue, "commandBuffer");
                for encoder_index in 0..ENCODERS_PER_FRAME {
                    let encoder = render_encoder(command, pass);
                    let (set_texture, set_texture_sel) = method::<
                        unsafe extern "C" fn(Id, Sel, Id, u64),
                    >(
                        encoder, "setFragmentTexture:atIndex:"
                    );
                    let (set_sampler, set_sampler_sel) =
                        method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(
                            encoder,
                            "setFragmentSamplerState:atIndex:",
                        );
                    let (draw_indexed, draw_indexed_sel) = method::<
                        unsafe extern "C" fn(Id, Sel, u64, u64, u64, Id, u64),
                    >(
                        encoder,
                        "drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:",
                    );
                    send_with_object(encoder, "setRenderPipelineState:", state);
                    set_buffer_named(encoder, "setVertexBuffer:offset:atIndex:", vertices, 0, 0);
                    set_sampler(encoder, set_sampler_sel, sampler, 0);
                    for draw in 0..DRAWS_PER_ENCODER {
                        let index = encoder_index * DRAWS_PER_ENCODER + draw;
                        let slot = (index % 64) as u64;
                        set_buffer_named(
                            encoder,
                            "setVertexBuffer:offset:atIndex:",
                            uniforms,
                            slot * UNIFORM_STRIDE,
                            1,
                        );
                        set_buffer_named(
                            encoder,
                            "setVertexBuffer:offset:atIndex:",
                            instances,
                            slot * 16,
                            2,
                        );
                        set_texture(encoder, set_texture_sel, textures[index % 8], 0);
                        set_texture(encoder, set_texture_sel, textures[(index / 8) % 8], 1);
                        if index.is_multiple_of(4) {
                            set_bytes_named(
                                encoder,
                                "setFragmentBytes:length:atIndex:",
                                &[1; 16],
                                0,
                            );
                        }
                        draw_indexed(encoder, draw_indexed_sel, TRIANGLES, 3, UINT16, indices, 0);
                    }
                    send_void(encoder, "endEncoding");
                }
                send_void(command, "commit");
                last = command;
            }
            let recorded = start.elapsed();
            let recorded_cpu = thread_cpu_time() - cpu_start;
            send_void(last, "waitUntilCompleted");
            assert_eq!(send_number_result(last, "status"), 4);
            objc::arc::pool_pop(pool);
            best = best.min(recorded.as_secs_f64() * 1e6 / DRAWS as f64);
            best_cpu = best_cpu.min(recorded_cpu * 1e6 / DRAWS as f64);
        }
        eprintln!(
            "recording with resources: {best:.3} us/draw, {best_cpu:.3} us/draw of this \
             thread's CPU (best of {ROUNDS})"
        );
    }
}

/// Seconds of CPU time the calling thread has used.
fn thread_cpu_time() -> f64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid clock and a live timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut now) };
    now.tv_sec as f64 + now.tv_nsec as f64 * 1e-9
}

/// The device time of a frame made like a game's: many short render passes,
/// measured three ways. Dependent passes each sample what the one before
/// drew, so each waits for its predecessor. Rewriting passes draw over one of
/// two targets in turn, sampling a texture nothing in the frame writes, so
/// each waits only at its attachment. Independent passes each draw a target
/// of their own and wait for nothing. Run with `cargo test --release -p metal
/// -- --ignored --nocapture encoder_barrier_device_cost`.
#[test]
#[ignore = "a benchmark, not a check"]
fn encoder_barrier_device_cost() {
    let Some(device) = gpu_device() else {
        return;
    };
    const PASSES: usize = 80;
    const DRAWS_PER_PASS: usize = 8;
    const FRAMES: usize = 30;
    const WIDTH: u64 = 256;
    const HEIGHT: u64 = 256;
    const UINT16: u64 = 0; // MTLIndexTypeUInt16
    const TRIANGLES: u64 = 3; // MTLPrimitiveTypeTriangle
    const LOAD: u64 = 1; // MTLLoadActionLoad
    const STORE: u64 = 1; // MTLStoreActionStore
    let gpu = gpu_of(device).expect("a Vulkan device");
    let state = resource_draw_pipeline(device);
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let target = || {
            resource::new_texture(
                gpu,
                resource::TextureDescriptor {
                    texture_type: 2,
                    pixel_format: BGRA8,
                    width: WIDTH,
                    height: HEIGHT,
                    depth: 1,
                    mipmap_level_count: 1,
                    sample_count: 1,
                    array_length: 1,
                    storage_mode: 2,
                    usage: gpu::USAGE_RENDER_TARGET | gpu::USAGE_SHADER_READ,
                },
            )
        };
        let targets: Vec<Id> = (0..PASSES).map(|_| target()).collect();
        let source = target();
        let sampler = resource::new_sampler(gpu, &resource::SamplerDescriptor::default());
        // A triangle which covers the target.
        let corners: [f32; 12] = [
            -1.0, -1.0, 0.0, 1.0, 3.0, -1.0, 0.0, 1.0, -1.0, 3.0, 0.0, 1.0,
        ];
        let vertices =
            new_buffer_with_bytes(device, std::ptr::null(), corners.as_ptr().cast(), 48, 0);
        let indices =
            new_buffer_with_bytes(device, std::ptr::null(), [0u16, 1, 2].as_ptr().cast(), 6, 0);
        let mut uniform = [0f32; 20];
        for diagonal in 0..4 {
            uniform[diagonal * 5] = 1.0;
        }
        uniform[16..].fill(1.0);
        let uniforms =
            new_buffer_with_bytes(device, std::ptr::null(), uniform.as_ptr().cast(), 80, 0);
        let instances = new_buffer(device, std::ptr::null(), 16, 0);
        let queue = new_command_queue(device, std::ptr::null());
        let passes: Vec<Id> = targets
            .iter()
            .map(|&target| {
                let pass = object::make(
                    c"MTLRenderPassDescriptor",
                    encoder::RenderPassDescriptor::default(),
                );
                let attachment = element_at(send(pass, "colorAttachments"), 0);
                send_with_object(attachment, "setTexture:", target);
                send_number(attachment, "setLoadAction:", LOAD);
                send_number(attachment, "setStoreAction:", STORE);
                pass
            })
            .collect();

        type Pick = fn(usize) -> (usize, Option<usize>);
        let modes: [(&str, Pick); 3] = [
            ("dependent", |index| (index % 2, Some((index + 1) % 2))),
            ("rewriting", |index| (index % 2, None)),
            ("independent", |index| (index, None)),
        ];
        for (mode, pick) in modes {
            let mut times = Vec::new();
            for _ in 0..FRAMES {
                let pool = objc::arc::pool_push();
                let command = send(queue, "commandBuffer");
                for index in 0..PASSES {
                    let (drawn, sampled) = pick(index);
                    let sampled = sampled.map_or(source, |sampled| targets[sampled]);
                    let encoder = render_encoder(command, passes[drawn]);
                    let (set_texture, set_texture_sel) = method::<
                        unsafe extern "C" fn(Id, Sel, Id, u64),
                    >(
                        encoder, "setFragmentTexture:atIndex:"
                    );
                    let (set_sampler, set_sampler_sel) =
                        method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(
                            encoder,
                            "setFragmentSamplerState:atIndex:",
                        );
                    let (draw_indexed, draw_indexed_sel) = method::<
                        unsafe extern "C" fn(Id, Sel, u64, u64, u64, Id, u64),
                    >(
                        encoder,
                        "drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:",
                    );
                    send_with_object(encoder, "setRenderPipelineState:", state);
                    set_buffer_named(encoder, "setVertexBuffer:offset:atIndex:", vertices, 0, 0);
                    set_buffer_named(encoder, "setVertexBuffer:offset:atIndex:", uniforms, 0, 1);
                    set_buffer_named(encoder, "setVertexBuffer:offset:atIndex:", instances, 0, 2);
                    set_bytes_named(encoder, "setFragmentBytes:length:atIndex:", &[0; 16], 0);
                    set_texture(encoder, set_texture_sel, sampled, 0);
                    set_texture(encoder, set_texture_sel, sampled, 1);
                    set_sampler(encoder, set_sampler_sel, sampler, 0);
                    for _ in 0..DRAWS_PER_PASS {
                        draw_indexed(encoder, draw_indexed_sel, TRIANGLES, 3, UINT16, indices, 0);
                    }
                    send_void(encoder, "endEncoding");
                }
                send_void(command, "commit");
                send_void(command, "waitUntilCompleted");
                assert_eq!(send_number_result(command, "status"), 4);
                times.push(
                    send_double(command, "GPUEndTime") - send_double(command, "GPUStartTime"),
                );
                objc::arc::pool_pop(pool);
            }
            times.sort_by(f64::total_cmp);
            eprintln!(
                "device time for {PASSES} {mode} passes: {:.3} ms median, {:.3} ms best",
                times[FRAMES / 2] * 1e3,
                times[0] * 1e3,
            );
        }
    }
}

/// Clear colour as `-setClearColor:` takes it.
#[repr(C)]
struct ClearColor([f64; 4]);

/// A render pass descriptor drawing into `target` with `load` and storing.
///
/// # Safety
///
/// `target` must be a texture of this crate's.
unsafe fn single_target_pass(target: Id, load: u64, clear: [f64; 4]) -> Id {
    const STORE: u64 = 1;
    // SAFETY: objects this function makes, sent the selectors they implement.
    unsafe {
        let pass = object::make(
            c"MTLRenderPassDescriptor",
            encoder::RenderPassDescriptor::default(),
        );
        let attachment = element_at(send(pass, "colorAttachments"), 0);
        send_with_object(attachment, "setTexture:", target);
        send_number(attachment, "setLoadAction:", load);
        send_number(attachment, "setStoreAction:", STORE);
        let (set_clear, sel) =
            method::<unsafe extern "C" fn(Id, Sel, ClearColor)>(attachment, "setClearColor:");
        set_clear(attachment, sel, ClearColor(clear));
        pass
    }
}

/// A pass which only draws into its attachments ends with a barrier naming
/// them, and the pass after it, which samples that attachment, and a copy
/// after that both see what was drawn.
#[test]
fn a_pass_samples_what_the_pass_before_drew() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const SIZE: u64 = 4;
    const CLEAR: u64 = 2; // MTLLoadActionClear
    const DONT_CARE: u64 = 0; // MTLLoadActionDontCare
    let gpu = gpu_of(device).expect("a Vulkan device");
    let state = resource_draw_pipeline(device);
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let target = || {
            resource::new_texture(
                gpu,
                resource::TextureDescriptor {
                    texture_type: 2,
                    pixel_format: BGRA8,
                    width: SIZE,
                    height: SIZE,
                    depth: 1,
                    mipmap_level_count: 1,
                    sample_count: 1,
                    array_length: 1,
                    storage_mode: 2,
                    usage: gpu::USAGE_RENDER_TARGET | gpu::USAGE_SHADER_READ,
                },
            )
        };
        let (drawn, sampled) = (target(), target());
        let sampler = resource::new_sampler(gpu, &resource::SamplerDescriptor::default());
        let corners: [f32; 12] = [
            -1.0, -1.0, 0.0, 1.0, 3.0, -1.0, 0.0, 1.0, -1.0, 3.0, 0.0, 1.0,
        ];
        let vertices =
            new_buffer_with_bytes(device, std::ptr::null(), corners.as_ptr().cast(), 48, 0);
        let mut uniform = [0f32; 20];
        for diagonal in 0..4 {
            uniform[diagonal * 5] = 1.0;
        }
        let uniforms =
            new_buffer_with_bytes(device, std::ptr::null(), uniform.as_ptr().cast(), 80, 0);
        let instances = new_buffer(device, std::ptr::null(), 16, 0);
        let readback = new_buffer(device, std::ptr::null(), (SIZE * SIZE * 4) as usize, 0);
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");

        // Blue, which is [255, 0, 0, 255] in BGRA.
        let clear = render_encoder(
            command,
            single_target_pass(drawn, CLEAR, [0.0, 0.0, 1.0, 1.0]),
        );
        send_void(clear, "endEncoding");

        let encoder = render_encoder(command, single_target_pass(sampled, DONT_CARE, [0.0; 4]));
        send_with_object(encoder, "setRenderPipelineState:", state);
        set_buffer_named(encoder, "setVertexBuffer:offset:atIndex:", vertices, 0, 0);
        set_buffer_named(encoder, "setVertexBuffer:offset:atIndex:", uniforms, 0, 1);
        set_buffer_named(encoder, "setVertexBuffer:offset:atIndex:", instances, 0, 2);
        let tint = [1.0f32; 4];
        set_bytes_named(
            encoder,
            "setFragmentBytes:length:atIndex:",
            std::slice::from_raw_parts(tint.as_ptr().cast(), 16),
            0,
        );
        let (set_texture, sel) = method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(
            encoder,
            "setFragmentTexture:atIndex:",
        );
        set_texture(encoder, sel, drawn, 0);
        set_texture(encoder, sel, drawn, 1);
        let (set_sampler, sel) = method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(
            encoder,
            "setFragmentSamplerState:atIndex:",
        );
        set_sampler(encoder, sel, sampler, 0);
        draw(encoder, 3);
        send_void(encoder, "endEncoding");

        let blit = send(command, "blitCommandEncoder");
        copy_texture_to_buffer(blit, sampled, readback, SIZE);
        send_void(blit, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        assert_eq!(send_number_result(command, "status"), 4);
        let pixels = std::slice::from_raw_parts(
            object::body::<resource::Buffer>(readback).inner.contents,
            (SIZE * SIZE * 4) as usize,
        );
        for pixel in pixels.chunks(4) {
            assert_eq!(pixel, [255, 0, 0, 255]);
        }
    }
}

/// A pass whose fragment shader writes a buffer is followed by work which
/// sees the write, though the pass's attachments are not what was written.
/// On a device whose caches keep buffers coherent this passes whichever
/// barrier follows the pass; it keeps the path which names all memory working.
#[test]
fn a_buffer_a_fragment_shader_writes_is_seen_by_the_next_encoder() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const SIZE: u64 = 4;
    const DONT_CARE: u64 = 0; // MTLLoadActionDontCare
    let source = r#"
#include <metal_stdlib>
using namespace metal;
struct VertexIn { float4 position [[attribute(0)]]; };
vertex float4 cover(VertexIn in [[stage_in]]) { return in.position; }
fragment float4 mark(float4 position [[position]], device uint* out [[buffer(0)]]) {
    out[0] = 7;
    return float4(0.0);
}
"#;
    let gpu = gpu_of(device).expect("a Vulkan device");
    let library = source_library(device, source);
    assert!(!library.is_null(), "the shaders must translate");
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let vertex = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("cover"),
        );
        let fragment = new_function_with_name(
            library,
            std::ptr::null(),
            foundation::string::from_str("mark"),
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
                input_topology: 0,
                color_objects: std::ptr::null_mut(),
            },
        );
        send_number(
            element_at(send(descriptor, "colorAttachments"), 0),
            "setPixelFormat:",
            BGRA8,
        );
        let mut error = std::ptr::null_mut();
        let state = new_render_pipeline(device, std::ptr::null(), descriptor, &mut error);
        assert!(!state.is_null(), "the pipeline must build");
        let target = resource::new_texture(
            gpu,
            resource::TextureDescriptor {
                texture_type: 2,
                pixel_format: BGRA8,
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
        let corners: [f32; 12] = [
            -1.0, -1.0, 0.0, 1.0, 3.0, -1.0, 0.0, 1.0, -1.0, 3.0, 0.0, 1.0,
        ];
        let vertices =
            new_buffer_with_bytes(device, std::ptr::null(), corners.as_ptr().cast(), 48, 0);
        const PRIVATE: u64 = 0x20;
        let written = new_buffer(device, std::ptr::null(), 16, PRIVATE);
        let readback = new_buffer(device, std::ptr::null(), 16, 0);
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        let encoder = render_encoder(command, single_target_pass(target, DONT_CARE, [0.0; 4]));
        send_with_object(encoder, "setRenderPipelineState:", state);
        set_buffer_named(encoder, "setVertexBuffer:offset:atIndex:", vertices, 0, 0);
        set_buffer_named(encoder, "setFragmentBuffer:offset:atIndex:", written, 0, 0);
        draw(encoder, 3);
        send_void(encoder, "endEncoding");
        let blit = send(command, "blitCommandEncoder");
        copy_buffer(blit, written, readback, 4);
        send_void(blit, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        assert_eq!(send_number_result(command, "status"), 4);
        let contents = object::body::<resource::Buffer>(readback).inner.contents as *const u32;
        assert_eq!(*contents, 7);
    }
}

/// The side of the targets the hazard tests draw into.
const HAZARD_SIZE: u64 = 4;
const LOAD_CLEAR: u64 = 2; // MTLLoadActionClear
const LOAD_LOAD: u64 = 1; // MTLLoadActionLoad
const LOAD_DONT_CARE: u64 = 0; // MTLLoadActionDontCare
/// Blue and red as `-setClearColor:` takes them, and as BGRA8 bytes.
const BLUE: [f64; 4] = [0.0, 0.0, 1.0, 1.0];
const RED: [f64; 4] = [1.0, 0.0, 0.0, 1.0];
const BLUE_PIXEL: [u8; 4] = [255, 0, 0, 255];
const RED_PIXEL: [u8; 4] = [0, 0, 255, 255];

/// [`RESOURCE_DRAW_SOURCE`] covering its target with what it samples.
struct SamplingDraw {
    device: Id,
    state: Id,
    vertices: Id,
    uniforms: Id,
    instances: Id,
    sampler: Id,
    queue: Id,
}

impl SamplingDraw {
    fn new(device: Id) -> Self {
        let gpu = gpu_of(device).expect("a Vulkan device");
        let corners: [f32; 12] = [
            -1.0, -1.0, 0.0, 1.0, 3.0, -1.0, 0.0, 1.0, -1.0, 3.0, 0.0, 1.0,
        ];
        let mut uniform = [0f32; 20];
        for diagonal in 0..4 {
            uniform[diagonal * 5] = 1.0;
        }
        // SAFETY: a device of this crate's.
        unsafe {
            Self {
                device,
                state: resource_draw_pipeline(device),
                vertices: new_buffer_with_bytes(
                    device,
                    std::ptr::null(),
                    corners.as_ptr().cast(),
                    48,
                    0,
                ),
                uniforms: new_buffer_with_bytes(
                    device,
                    std::ptr::null(),
                    uniform.as_ptr().cast(),
                    80,
                    0,
                ),
                instances: new_buffer(device, std::ptr::null(), 16, 0),
                sampler: resource::new_sampler(gpu, &resource::SamplerDescriptor::default()),
                queue: new_command_queue(device, std::ptr::null()),
            }
        }
    }

    fn target(&self) -> Id {
        resource::new_texture(
            gpu_of(self.device).expect("a Vulkan device"),
            resource::TextureDescriptor {
                texture_type: 2,
                pixel_format: BGRA8,
                width: HAZARD_SIZE,
                height: HAZARD_SIZE,
                depth: 1,
                mipmap_level_count: 1,
                sample_count: 1,
                array_length: 1,
                storage_mode: 2,
                usage: gpu::USAGE_RENDER_TARGET | gpu::USAGE_SHADER_READ,
            },
        )
    }

    /// A target cleared to `color` by work which has completed.
    fn filled(&self, color: [f64; 4]) -> Id {
        let target = self.target();
        // SAFETY: objects this fixture made.
        unsafe {
            let command = send(self.queue, "commandBuffer");
            let pass = render_encoder(command, single_target_pass(target, LOAD_CLEAR, color));
            send_void(pass, "endEncoding");
            send_void(command, "commit");
            send_void(command, "waitUntilCompleted");
        }
        target
    }

    /// Set up `encoder` to draw `texture`, sampled, over its whole target.
    ///
    /// # Safety
    ///
    /// `encoder` must be a render encoder of this crate's.
    unsafe fn prepare(&self, encoder: Id, texture: Id) {
        // SAFETY: the caller guarantees the encoder.
        unsafe {
            send_with_object(encoder, "setRenderPipelineState:", self.state);
            set_buffer_named(
                encoder,
                "setVertexBuffer:offset:atIndex:",
                self.vertices,
                0,
                0,
            );
            set_buffer_named(
                encoder,
                "setVertexBuffer:offset:atIndex:",
                self.uniforms,
                0,
                1,
            );
            set_buffer_named(
                encoder,
                "setVertexBuffer:offset:atIndex:",
                self.instances,
                0,
                2,
            );
            let tint = [1.0f32; 4];
            set_bytes_named(
                encoder,
                "setFragmentBytes:length:atIndex:",
                std::slice::from_raw_parts(tint.as_ptr().cast(), 16),
                0,
            );
            let (set_sampler, sel) = method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(
                encoder,
                "setFragmentSamplerState:atIndex:",
            );
            set_sampler(encoder, sel, self.sampler, 0);
            self.bind(encoder, texture);
        }
    }

    /// Bind `texture` as both textures the shader multiplies.
    ///
    /// # Safety
    ///
    /// As for [`Self::prepare`].
    unsafe fn bind(&self, encoder: Id, texture: Id) {
        // SAFETY: the caller guarantees the encoder.
        unsafe {
            let (set_texture, sel) = method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(
                encoder,
                "setFragmentTexture:atIndex:",
            );
            set_texture(encoder, sel, texture, 0);
            set_texture(encoder, sel, texture, 1);
        }
    }

    /// Copy each of `targets` out after the rest of `command`, commit it, and
    /// answer their pixels.
    ///
    /// # Safety
    ///
    /// `command` must be a recording command buffer of this crate's.
    unsafe fn read(&self, command: Id, targets: &[Id]) -> Vec<Vec<[u8; 4]>> {
        let bytes = (HAZARD_SIZE * HAZARD_SIZE * 4) as usize;
        // SAFETY: the caller guarantees the command buffer.
        unsafe {
            let blit = send(command, "blitCommandEncoder");
            let readbacks: Vec<Id> = targets
                .iter()
                .map(|target| {
                    let readback = new_buffer(self.device, std::ptr::null(), bytes, 0);
                    copy_texture_to_buffer(blit, *target, readback, HAZARD_SIZE);
                    readback
                })
                .collect();
            send_void(blit, "endEncoding");
            send_void(command, "commit");
            send_void(command, "waitUntilCompleted");
            assert_eq!(send_number_result(command, "status"), 4);
            readbacks
                .iter()
                .map(|readback| {
                    let contents = object::body::<resource::Buffer>(*readback).inner.contents;
                    std::slice::from_raw_parts(contents, bytes)
                        .chunks(4)
                        .map(|pixel| pixel.try_into().expect("four bytes"))
                        .collect()
                })
                .collect()
        }
    }
}

fn recorded_barriers() -> u64 {
    encoder::TEST_RECORDED_BARRIERS.load(std::sync::atomic::Ordering::Relaxed)
}

fn assert_all(pixels: &[[u8; 4]], expected: [u8; 4], what: &str) {
    assert!(
        pixels.iter().all(|pixel| *pixel == expected),
        "{what}: {pixels:?}"
    );
}

/// Passes which share no resource are not ordered against each other: no
/// barrier is recorded between them, and the copy after both sees each.
#[test]
fn passes_sharing_nothing_record_no_barrier_between_them() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let fixture = SamplingDraw::new(device);
    let (blue, red) = (fixture.filled(BLUE), fixture.filled(RED));
    let (first, second) = (fixture.target(), fixture.target());
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let command = send(fixture.queue, "commandBuffer");
        let before = recorded_barriers();
        for (target, source) in [(first, blue), (second, red)] {
            let pass = render_encoder(command, single_target_pass(target, LOAD_DONT_CARE, BLUE));
            fixture.prepare(pass, source);
            draw(pass, 3);
            send_void(pass, "endEncoding");
        }
        assert_eq!(recorded_barriers() - before, 0);
        let pixels = fixture.read(command, &[first, second]);
        assert_all(&pixels[0], BLUE_PIXEL, "first pass");
        assert_all(&pixels[1], RED_PIXEL, "second pass");
    }
}

/// A texture an earlier pass drew which is bound after a pass began drawing
/// splits it: the draws after the binding see what the earlier pass drew.
#[test]
fn a_texture_bound_after_drawing_began_waits_for_the_pass_which_drew_it() {
    isolate_metal_fixture!();
    if let Some(device) = gpu_device() {
        check_split_pass_sees_earlier_pass(device);
    }
}

#[test]
fn a_split_render_pass_continues_without_dynamic_rendering() {
    isolate_metal_fixture!();
    if let Some(device) = gpu_device_without_dynamic_rendering() {
        check_split_pass_sees_earlier_pass(device);
    }
}

fn check_split_pass_sees_earlier_pass(device: Id) {
    let fixture = SamplingDraw::new(device);
    let red = fixture.filled(RED);
    let (drawn, target) = (fixture.target(), fixture.target());
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let command = send(fixture.queue, "commandBuffer");
        let clear = render_encoder(command, single_target_pass(drawn, LOAD_CLEAR, BLUE));
        send_void(clear, "endEncoding");
        let before = recorded_barriers();
        let pass = render_encoder(command, single_target_pass(target, LOAD_DONT_CARE, RED));
        fixture.prepare(pass, red);
        draw(pass, 3);
        fixture.bind(pass, drawn);
        draw(pass, 3);
        send_void(pass, "endEncoding");
        assert_eq!(recorded_barriers() - before, 1, "one split");
        let pixels = fixture.read(command, &[target]);
        assert_all(&pixels[0], BLUE_PIXEL, "the draw after the split");
    }
}

/// A pass which overwrites a texture an earlier pass sampled waits for that
/// pass, which sees what the texture held before.
#[test]
fn a_pass_overwriting_a_texture_waits_for_the_pass_which_sampled_it() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let fixture = SamplingDraw::new(device);
    let sampled = fixture.filled(BLUE);
    let target = fixture.target();
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let command = send(fixture.queue, "commandBuffer");
        let pass = render_encoder(command, single_target_pass(target, LOAD_DONT_CARE, RED));
        fixture.prepare(pass, sampled);
        draw(pass, 3);
        send_void(pass, "endEncoding");
        let before = recorded_barriers();
        let clear = render_encoder(command, single_target_pass(sampled, LOAD_CLEAR, RED));
        send_void(clear, "endEncoding");
        assert_eq!(recorded_barriers() - before, 1);
        let pixels = fixture.read(command, &[target, sampled]);
        assert_all(&pixels[0], BLUE_PIXEL, "what the first pass sampled");
        assert_all(&pixels[1], RED_PIXEL, "the overwritten texture");
    }
}

/// A pass which draws over an attachment an earlier pass wrote lands after
/// it, behind a barrier over the attachment alone.
#[test]
fn a_pass_drawing_over_an_earlier_passes_attachment_lands_after_it() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let fixture = SamplingDraw::new(device);
    let red = fixture.filled(RED);
    let target = fixture.target();
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let command = send(fixture.queue, "commandBuffer");
        let clear = render_encoder(command, single_target_pass(target, LOAD_CLEAR, BLUE));
        send_void(clear, "endEncoding");
        let before = recorded_barriers();
        let pass = render_encoder(command, single_target_pass(target, LOAD_LOAD, BLUE));
        fixture.prepare(pass, red);
        draw(pass, 3);
        send_void(pass, "endEncoding");
        assert_eq!(recorded_barriers() - before, 1);
        let pixels = fixture.read(command, &[target]);
        assert_all(&pixels[0], RED_PIXEL, "the later pass");
    }
}

/// A pass which loads its attachment and draws nothing changes nothing, so
/// it is not recorded and orders nothing.
#[test]
fn a_pass_which_loads_and_draws_nothing_leaves_its_attachment_alone() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let fixture = SamplingDraw::new(device);
    let target = fixture.filled(BLUE);
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let command = send(fixture.queue, "commandBuffer");
        let before = recorded_barriers();
        let pass = render_encoder(command, single_target_pass(target, LOAD_LOAD, RED));
        send_void(pass, "endEncoding");
        assert_eq!(recorded_barriers() - before, 0);
        let pixels = fixture.read(command, &[target]);
        assert_all(&pixels[0], BLUE_PIXEL, "the loaded attachment");
    }
}

/// Culling and winding are set per draw: one pipeline drawn under each
/// combination culls exactly the triangles Metal would, and is compiled
/// once, when it is made, rather than once per combination.
#[test]
fn culling_and_winding_change_without_another_pipeline() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const CULL_NONE: u64 = 0;
    const CULL_BACK: u64 = 2;
    const CLOCKWISE: u64 = 0;
    const COUNTER_CLOCKWISE: u64 = 1;
    let fixture = SamplingDraw::new(device);
    let blue = fixture.filled(BLUE);
    let cases = [
        (CULL_BACK, CLOCKWISE),
        (CULL_BACK, COUNTER_CLOCKWISE),
        (CULL_NONE, CLOCKWISE),
    ];
    let targets: Vec<Id> = cases.iter().map(|_| fixture.target()).collect();
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let command = send(fixture.queue, "commandBuffer");
        for (&(cull, winding), &target) in cases.iter().zip(&targets) {
            let pass = render_encoder(command, single_target_pass(target, LOAD_CLEAR, RED));
            fixture.prepare(pass, blue);
            send_number(pass, "setCullMode:", cull);
            send_number(pass, "setFrontFacingWinding:", winding);
            draw(pass, 3);
            send_void(pass, "endEncoding");
        }
        let pixels = fixture.read(command, &targets);
        let drawn = |pixels: &[[u8; 4]]| pixels.iter().all(|pixel| *pixel == BLUE_PIXEL);
        let cleared = |pixels: &[[u8; 4]]| pixels.iter().all(|pixel| *pixel == RED_PIXEL);
        // The triangle faces one way, so back-face culling removes it under
        // exactly one of the two windings.
        assert!(
            (drawn(&pixels[0]) && cleared(&pixels[1]))
                || (cleared(&pixels[0]) && drawn(&pixels[1])),
            "{:?} / {:?}",
            pixels[0],
            pixels[1]
        );
        assert_all(&pixels[2], BLUE_PIXEL, "unculled");
        let gpu = gpu_of(device).expect("a Vulkan device");
        if gpu.dynamic_render_state {
            let state = object::body::<crate::pipeline::RenderPipeline>(fixture.state);
            assert_eq!(state.variant_count(), 1);
        }
    }
}

/// Fill and depth clip mode are set per draw where the device can: wireframe
/// leaves the middle of the triangle undrawn, clamping changes nothing for a
/// triangle inside the depth range, and neither compiles another pipeline.
#[test]
fn fill_and_depth_clip_modes_change_without_another_pipeline() {
    isolate_metal_fixture!();
    if let Some(device) = gpu_device() {
        check_fill_and_depth_clip_modes(device);
    }
}

#[test]
fn fill_and_depth_clip_modes_draw_without_dynamic_rendering() {
    isolate_metal_fixture!();
    if let Some(device) = gpu_device_without_dynamic_rendering() {
        check_fill_and_depth_clip_modes(device);
    }
}

fn check_fill_and_depth_clip_modes(device: Id) {
    const FILL: u64 = 0;
    const LINES: u64 = 1;
    const CLIP: u64 = 0;
    const CLAMP: u64 = 1;
    let fixture = SamplingDraw::new(device);
    let blue = fixture.filled(BLUE);
    let cases = [(FILL, CLIP), (LINES, CLIP), (FILL, CLAMP)];
    let targets: Vec<Id> = cases.iter().map(|_| fixture.target()).collect();
    // SAFETY: objects this test made, sent the selectors they implement.
    unsafe {
        let command = send(fixture.queue, "commandBuffer");
        for (&(fill, clip), &target) in cases.iter().zip(&targets) {
            let pass = render_encoder(command, single_target_pass(target, LOAD_CLEAR, RED));
            fixture.prepare(pass, blue);
            send_number(pass, "setTriangleFillMode:", fill);
            send_number(pass, "setDepthClipMode:", clip);
            draw(pass, 3);
            send_void(pass, "endEncoding");
        }
        let pixels = fixture.read(command, &targets);
        let middle = (HAZARD_SIZE / 2 * HAZARD_SIZE + HAZARD_SIZE / 2) as usize;
        assert_all(&pixels[0], BLUE_PIXEL, "filled");
        assert_eq!(pixels[1][middle], RED_PIXEL, "inside a wireframe triangle");
        assert_all(&pixels[2], BLUE_PIXEL, "clamped");
        if gpu_of(device)
            .expect("a Vulkan device")
            .dynamic_raster
            .is_some()
        {
            let state = object::body::<crate::pipeline::RenderPipeline>(fixture.state);
            assert_eq!(state.variant_count(), 1);
        }
    }
}

const MTL_PIXEL_FORMAT_DEPTH32_FLOAT: u64 = 252;
const MTL_STORAGE_MODE_PRIVATE: u64 = 2;
const MTL_COMPARE_FUNCTION_LESS_EQUAL: u64 = 3;
const MTL_LOAD_ACTION_CLEAR: u64 = 2;
const MTL_STORE_ACTION_STORE: u64 = 1;

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
    covering_draw_with_depth(device, MTL_PIXEL_FORMAT_INVALID, 0.0)
}

/// A covering draw whose pipeline has `depth_format`, with its triangle at
/// depth `z`.
fn covering_draw_with_depth(device: Id, depth_format: u64, z: f32) -> Option<CoveringDraw> {
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
            depth_format,
            stencil_format: 0,
            sample_count: 1,
            alpha_to_coverage: false,
            alpha_to_one: false,
            rasterization: true,
            input_topology: 0,
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
            -1.0f32, -1.0, z, 1.0, //
            3.0, -1.0, z, 1.0, //
            -1.0, 3.0, z, 1.0,
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

/// An unchanged depth bias is reusable, but a later nonzero bias must still
/// change rasterized depth across draws in the same encoder.
#[test]
fn repeated_depth_bias_preserves_changed_depth_tests() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    // On a float format the constant bias is in units of the primitive's
    // depth exponent, which a depth of zero leaves to the implementation.
    const DEPTH: f32 = 0.5;
    const NONZERO_BIAS: f32 = 1024.0;
    let Some(fixture) = covering_draw_with_depth(device, MTL_PIXEL_FORMAT_DEPTH32_FLOAT, DEPTH)
    else {
        return;
    };
    let gpu = gpu_of(device).expect("a Vulkan device");
    let depth = resource::new_texture(
        gpu,
        resource::TextureDescriptor {
            texture_type: MTL_TEXTURE_TYPE_2D,
            pixel_format: MTL_PIXEL_FORMAT_DEPTH32_FLOAT,
            width: COVERING_SIZE,
            height: COVERING_SIZE,
            depth: 1,
            mipmap_level_count: 1,
            sample_count: 1,
            array_length: 1,
            storage_mode: MTL_STORAGE_MODE_PRIVATE,
            usage: gpu::USAGE_RENDER_TARGET,
        },
    );
    assert!(!depth.is_null());
    let depth_state = resource::new_depth_stencil_state(resource::DepthStencilState {
        depth_compare: MTL_COMPARE_FUNCTION_LESS_EQUAL,
        depth_write: true,
        ..resource::DepthStencilState::default()
    });
    let results = results_buffer(device, 6, 0);
    let pass = fixture.pass(results);
    // SAFETY: the same selectors and resources an MTLRenderCommandEncoder uses.
    unsafe {
        let attachment = send(pass, "depthAttachment");
        send_with_object(attachment, "setTexture:", depth);
        send_number(attachment, "setLoadAction:", MTL_LOAD_ACTION_CLEAR);
        send_number(attachment, "setStoreAction:", MTL_STORE_ACTION_STORE);
        let command = send(fixture.queue, "commandBuffer");
        let render = render_encoder(command, pass);
        fixture.bind(render);
        send_with_object(render, "setDepthStencilState:", depth_state);
        let (set_bias, selector) = method::<unsafe extern "C" fn(Id, Sel, f32, f32, f32)>(
            render,
            "setDepthBias:slopeScale:clamp:",
        );
        for (index, bias) in [0.0f32, 0.0, NONZERO_BIAS, NONZERO_BIAS, 0.0, 0.0]
            .into_iter()
            .enumerate()
        {
            set_bias(render, selector, bias, 0.0, 0.0);
            set_visibility_mode(render, VISIBILITY_COUNTING, (index * 8) as u64);
            draw(render, 3);
        }
        send_void(render, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
    }
    let filled = COVERING_SIZE * COVERING_SIZE;
    assert_eq!(
        (0..6)
            .map(|index| result_at(results, index))
            .collect::<Vec<_>>(),
        [filled, filled, 0, 0, filled, filled],
    );
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
    if let Some(device) = gpu_device() {
        check_parallel_sub_encoders(device);
    }
}

#[test]
fn parallel_sub_encoders_inherit_a_render_pass_without_dynamic_rendering() {
    isolate_metal_fixture!();
    if let Some(device) = gpu_device_without_dynamic_rendering() {
        check_parallel_sub_encoders(device);
    }
}

fn check_parallel_sub_encoders(device: Id) {
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

/// Serial dispatch barriers and concurrent closing barriers must order
/// subsequent encoders, even when only one dispatch precedes the boundary.
#[test]
fn compute_encoders_order_subsequent_dispatches() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const DISPATCH_SERIAL: u64 = 0;
    const DISPATCH_CONCURRENT: u64 = 1;
    const SCOPE_BUFFERS: u64 = 1;
    let library = source_library(
        device,
        "kernel void step(device uint* value [[buffer(0)]]) { value[0] += 1; }",
    );
    // SAFETY: live Metal objects and registered selectors with their ABI.
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
        for (kind, dispatches_per_encoder, encoders, barrier_after_each) in [
            (DISPATCH_SERIAL, 0, 16, false),
            (DISPATCH_CONCURRENT, 0, 16, false),
            (DISPATCH_SERIAL, 1, 64, false),
            (DISPATCH_SERIAL, 3, 32, false),
            (DISPATCH_SERIAL, 3, 32, true),
            (DISPATCH_CONCURRENT, 1, 64, false),
            (DISPATCH_CONCURRENT, 2, 32, false),
            (DISPATCH_CONCURRENT, 2, 32, true),
        ] {
            let buffer = new_buffer(device, std::ptr::null(), 16, 0);
            let command = send(queue, "commandBuffer");
            let (make, sel) = method::<unsafe extern "C" fn(Id, Sel, u64) -> Id>(
                command,
                "computeCommandEncoderWithDispatchType:",
            );
            for _ in 0..encoders {
                let encoder = make(command, sel, kind);
                send_with_object(encoder, "setComputePipelineState:", pipeline);
                set_buffer_at(encoder, buffer, 0, 0);
                let before =
                    encoder::TEST_RECORDED_BARRIERS.load(std::sync::atomic::Ordering::Relaxed);
                for index in 0..dispatches_per_encoder {
                    dispatch(encoder, 1);
                    if barrier_after_each
                        || (kind == DISPATCH_CONCURRENT && index + 1 < dispatches_per_encoder)
                    {
                        send_number(encoder, "memoryBarrierWithScope:", SCOPE_BUFFERS);
                    }
                }
                send_void(encoder, "endEncoding");
                let recorded = encoder::TEST_RECORDED_BARRIERS
                    .load(std::sync::atomic::Ordering::Relaxed)
                    - before;
                let expected = u64::from(dispatches_per_encoder.max(1));
                assert_eq!(
                    recorded, expected,
                    "type {kind}: unexpected redundant barrier"
                );
            }
            send_void(command, "commit");
            send_void(command, "waitUntilCompleted");
            let contents = object::body::<resource::Buffer>(buffer).inner.contents;
            assert_eq!(
                std::ptr::read(contents.cast::<u32>()),
                encoders * dispatches_per_encoder,
                "type {kind}"
            );
        }
    }
}

#[test]
fn generating_mipmaps_ends_a_blit_encoder_with_one_barrier() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const LEVELS: u64 = 4;
    let gpu = gpu_of(device).expect("a Vulkan device");
    let texture = resource::new_texture(
        gpu,
        resource::TextureDescriptor {
            texture_type: 2,
            pixel_format: 70, // RGBA8Unorm
            width: 1 << (LEVELS - 1),
            height: 1 << (LEVELS - 1),
            depth: 1,
            mipmap_level_count: LEVELS,
            sample_count: 1,
            array_length: 1,
            storage_mode: 0,
            usage: gpu::USAGE_SHADER_READ,
        },
    );
    assert!(!texture.is_null());
    // SAFETY: live Metal objects and registered selectors with their ABI.
    unsafe {
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        let blit = send(command, "blitCommandEncoder");
        let before = encoder::TEST_RECORDED_BARRIERS.load(std::sync::atomic::Ordering::Relaxed);
        send_with_object(blit, "generateMipmapsForTexture:", texture);
        send_void(blit, "endEncoding");
        let recorded =
            encoder::TEST_RECORDED_BARRIERS.load(std::sync::atomic::Ordering::Relaxed) - before;
        assert_eq!(recorded, LEVELS - 1);
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        assert_eq!(send_number_result(command, "status"), 4);
    }
    objc::arc::release(texture);
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

/// A device which begins passes through render passes and compiles fill and
/// depth clip mode into its pipelines, as one without dynamic rendering does.
fn gpu_device_without_dynamic_rendering() -> Option<Id> {
    gpu::TEST_WITHOUT_DYNAMIC_RENDERING.store(true, Ordering::Release);
    let device = gpu_device()?;
    let gpu = gpu_of(device).expect("a GPU");
    assert!(!gpu.dynamic_rendering && gpu.dynamic_raster.is_none());
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

/// A texture cut from a heap range an earlier texture used must read as
/// zeros, as fresh memory does, including formats Vulkan cannot clear.
#[test]
fn a_new_texture_reads_zero_over_a_range_texels_were_written_to() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    const SIZE: u64 = 64;
    const ROUNDS: usize = 4;
    const RGBA8_UNORM: u64 = 70;
    const BC1_RGBA: u64 = 130;
    let gpu = gpu_of(device).expect("a Vulkan device");
    for (format, row_bytes) in [(RGBA8_UNORM, SIZE * 4), (BC1_RGBA, SIZE / 4 * 8)] {
        let rows = match format {
            BC1_RGBA => SIZE / 4,
            _ => SIZE,
        };
        let length = (row_bytes * rows) as usize;
        let make = || {
            let texture = resource::new_texture(
                gpu,
                resource::TextureDescriptor {
                    texture_type: 2,
                    pixel_format: format,
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
            texture
        };
        let mut reused = false;
        for round in 0..ROUNDS {
            let dirty = make();
            let bytes = vec![0xAB_u8; length];
            let mut read = vec![0_u8; length];
            // SAFETY: a texture this test made, and bytes covering it.
            unsafe {
                replace_region(dirty, [0, 0, SIZE, SIZE], &bytes, row_bytes);
                get_bytes(dirty, [0, 0, SIZE, SIZE], &mut read, row_bytes);
            }
            assert!(read == bytes, "format {format}, round {round}");
            objc::arc::release(dirty);
            let clean = make();
            // SAFETY: a texture this test made.
            let image = unsafe { &object::body::<resource::Texture>(clean).image };
            reused |= image
                .heap
                .is_some_and(|(_, allocation)| !allocation.pristine);
            // SAFETY: as above.
            unsafe { get_bytes(clean, [0, 0, SIZE, SIZE], &mut read, row_bytes) };
            assert!(
                read.iter().all(|byte| *byte == 0),
                "format {format}, round {round}"
            );
            objc::arc::release(clean);
        }
        assert!(reused, "format {format} never landed on a reused range");
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
