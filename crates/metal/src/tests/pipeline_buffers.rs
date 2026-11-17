use super::*;

#[test]
fn pipeline_buffer_arrays_copy_assignments_and_keep_retained_elements_alive() {
    isolate_metal_fixture!();
    bootstrap();
    // SAFETY: live descriptors and their registered property signatures.
    unsafe {
        let class =
            objc::class::ptr(objc::class::lookup(c"MTLRenderPipelineDescriptor").unwrap()) as Id;
        let descriptor = send(class, "new");
        let vertex = objc::arc::retain(send(descriptor, "vertexBuffers"));
        let fragment = send(descriptor, "fragmentBuffers");
        assert!(!vertex.is_null() && !fragment.is_null());
        assert_ne!(vertex, fragment);
        let element = objc::arc::retain(element_at(vertex, 30));
        assert_eq!(element_at(vertex, 30), element);
        assert_eq!(send_number_result(element, "mutability"), 0);
        send_number(element, "setMutability:", 2);
        let (assign, sel) = method::<unsafe extern "C" fn(Id, Sel, Id, u64)>(
            vertex,
            "setObject:atIndexedSubscript:",
        );
        assign(vertex, sel, element, 30);
        assert_eq!(
            element_at(vertex, 30),
            element,
            "self-assignment preserves identity"
        );
        assign(vertex, sel, element, 0);
        let assigned = objc::arc::retain(element_at(vertex, 0));
        assert_ne!(assigned, element);
        send_number(element, "setMutability:", 1);
        assert_eq!(send_number_result(assigned, "mutability"), 2);
        assert_eq!(send_number_result(element_at(fragment, 0), "mutability"), 0);
        let snapshot = pipeline::copy_render_pipeline_descriptor(descriptor);
        let copied = element_at(send(snapshot, "vertexBuffers"), 0);
        send_number(assigned, "setMutability:", 1);
        assert_eq!(send_number_result(copied, "mutability"), 2);
        send_void(descriptor, "reset");
        assert_eq!(send(descriptor, "vertexBuffers"), vertex);
        assert_ne!(element_at(vertex, 30), element);
        assert_eq!(send_number_result(element_at(vertex, 30), "mutability"), 0);
        assert_eq!(send_number_result(element, "mutability"), 1);
        objc::arc::release(descriptor);
        assert_eq!(send_number_result(element_at(vertex, 0), "mutability"), 0);
        for value in [vertex, element, assigned, snapshot] {
            objc::arc::release(value);
        }
    }
}

#[test]
fn pipeline_buffer_copy_preserves_subclasses_and_concurrent_getters_share_one_element() {
    isolate_metal_fixture!();
    bootstrap();
    // SAFETY: registered classes and objects owned by this test.
    unsafe {
        let base = objc::class::ptr(objc::class::lookup(c"MTLPipelineBufferDescriptor").unwrap());
        let subclass = objc::api::objc_allocateClassPair(
            base.cast(),
            c"RosemaryBufferDescriptorCopyProbe".as_ptr(),
            0,
        );
        assert!(!subclass.is_null());
        objc::api::objc_registerClassPair(subclass);
        let original = send(subclass as Id, "new");
        send_number(original, "setMutability:", 2);
        let copied = send(original, "copy");
        assert_ne!(copied, original);
        assert_eq!(objc::api::object_getClass(copied), subclass);
        assert_eq!(send_number_result(copied, "mutability"), 2);
        let class =
            objc::class::ptr(objc::class::lookup(c"MTLPipelineBufferDescriptorArray").unwrap());
        let array = send(class as Id, "new");
        let address = array as usize;
        let threads: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(move || element_at(address as Id, 7) as usize))
            .collect();
        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(
            results
                .iter()
                .all(|&value| value == results[0] && value != 0)
        );
        for value in [original, copied, array] {
            objc::arc::release(value);
        }
    }
}

#[test]
fn compute_pipeline_buffer_reset_matches_the_recovery_image() {
    isolate_metal_fixture!();
    let device = device_for_tests();
    let library = source_library(device, TEST_KERNEL);
    // SAFETY: registered methods and live objects.
    unsafe {
        let name = foundation::string::from_str("clear");
        let function = new_function_with_name(library, std::ptr::null(), name);
        let class = objc::class::ptr(objc::class::lookup(c"MTLComputePipelineDescriptor").unwrap());
        let descriptor = send(class as Id, "new");
        send_with_object(descriptor, "setComputeFunction:", function);
        let array = send(descriptor, "buffers");
        let old = objc::arc::retain(element_at(array, 0));
        send_number(old, "setMutability:", 2);
        send_void(descriptor, "reset");
        assert_eq!(send(descriptor, "buffers"), array);
        assert_eq!(send(descriptor, "computeFunction"), function);
        assert_ne!(element_at(array, 0), old);
        assert_eq!(send_number_result(element_at(array, 0), "mutability"), 0);
        assert_eq!(send_number_result(old, "mutability"), 2);
        for value in [old, descriptor, function, name, library] {
            objc::arc::release(value);
        }
    }
}
