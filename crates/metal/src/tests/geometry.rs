use super::*;

#[test]
fn refraction_matches_normal_incidence_bending_and_total_internal_reflection() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let source = "kernel void probe(device float4* output [[buffer(0)]], uint index [[thread_position_in_grid]]) {
        if (index >= 3u) return;
        float3 incident = float3(0.0, -1.0, 0.0);
        float ratio = 2.0;
        if (index == 1u) { incident = float3(0.6, -0.8, 0.0); ratio = 0.75; }
        if (index == 2u) { incident = float3(0.8660254037844386, -0.5, 0.0); }
        output[index] = float4(refract(incident, float3(0.0, 1.0, 0.0), ratio), 1.0);
    }";
    let library = source_library(device, source);
    assert!(!library.is_null());
    // SAFETY: test-owned objects and a shared buffer of three float4 values.
    unsafe {
        let name = foundation::string::from_str("probe");
        let function = new_function_with_name(library, std::ptr::null(), name);
        let mut error = std::ptr::null_mut();
        let pipeline = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        assert!(!pipeline.is_null() && error.is_null());
        let buffer = new_buffer(device, std::ptr::null(), 3 * 16, 0);
        let queue = new_command_queue(device, std::ptr::null());
        assert!(!buffer.is_null() && !queue.is_null());
        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", pipeline);
        set_buffer_at(encoder, buffer, 0, 0);
        dispatch(encoder, 3);
        send_void(encoder, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");
        let contents = object::body::<resource::Buffer>(buffer)
            .inner
            .contents
            .cast::<f32>();
        let actual = std::slice::from_raw_parts(contents, 12);
        let expected = [
            0.0,
            -1.0,
            0.0,
            1.0,
            0.45,
            -(0.7975f32).sqrt(),
            0.0,
            1.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ];
        for (&actual, expected) in actual.iter().zip(expected) {
            assert!(
                (actual - expected).abs() < 0.000002,
                "{actual} != {expected}"
            );
        }
        for value in [name, pipeline, function, library, buffer, queue] {
            objc::arc::release(value);
        }
    }
}
