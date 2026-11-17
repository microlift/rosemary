use super::*;

#[test]
fn shader_loop_early_returns_preserve_gpu_results() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let source = "uint find_value(uint stop) {
        if (stop > 6u) return 17u;
        for (uint i = 0u; i < 4u; i++) {
            if (i == stop) return 100u + i;
        }
        return 4u;
    }
    kernel void probe(device uint* output [[buffer(0)]], uint index [[thread_position_in_grid]]) {
        if (index >= 8u) return;
        output[index] = find_value(index);
    }";
    check_results(device, source, &[100, 101, 102, 103, 4, 4, 4, 17]);
}

#[test]
fn shader_nested_loop_early_returns_preserve_gpu_results() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let source = "uint find_value(uint stop) {
        if (stop > 6u) return 17u;
        uint i = 0u;
        for (; i < 4u; i++) {
            if (i == stop) return 100u + i;
            if (stop == 5u && i == 2u) break;
        }
        return 200u + i;
    }
    kernel void probe(device uint* output [[buffer(0)]], uint index [[thread_position_in_grid]]) {
        if (index >= 8u) return;
        uint total = 0u;
        for (uint j = 0u; j < 3u; j++) {
            if (index == 0u && j == 1u) continue;
            total += find_value(index + j);
        }
        output[index] = total;
    }";
    check_results(device, source, &[202, 306, 409, 509, 610, 423, 238, 51]);
}

fn check_results(device: Id, source: &str, expected: &[u32]) {
    let library = source_library(device, source);
    assert!(!library.is_null());
    // SAFETY: test-owned Metal objects, with a shared buffer of eight uints.
    unsafe {
        let name = foundation::string::from_str("probe");
        let function = new_function_with_name(library, std::ptr::null(), name);
        assert!(!function.is_null());
        let mut error = std::ptr::null_mut();
        let pipeline = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        assert!(!pipeline.is_null() && error.is_null());
        let buffer = new_buffer(device, std::ptr::null(), 8 * 4, 0);
        let queue = new_command_queue(device, std::ptr::null());
        assert!(!buffer.is_null() && !queue.is_null());
        let contents = object::body::<resource::Buffer>(buffer)
            .inner
            .contents
            .cast::<u32>();
        std::slice::from_raw_parts_mut(contents, 8).fill(u32::MAX);
        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", pipeline);
        set_buffer_at(encoder, buffer, 0, 0);
        dispatch(encoder, 8);
        send_void(encoder, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        assert_eq!(std::slice::from_raw_parts(contents, 8), expected);
        for value in [name, pipeline, function, library, buffer, queue] {
            objc::arc::release(value);
        }
    }
}
