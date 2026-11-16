use super::*;

#[test]
fn software_short_wrap_and_signed_consumers_run_on_the_gpu() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let source = "#include <metal_stdlib>
        using namespace metal;
        kernel void probe(device uint* input [[buffer(0)]],
            device uint* output [[buffer(1)]], uint id [[thread_position_in_grid]]) {
            ushort unsigned_value = ushort(input[id]);
            short signed_value = short(input[id]);
            ushort wrapped = ushort(unsigned_value + ushort(1));
            short divided = short(signed_value / short(-2));
            output[id] = uint(wrapped);
            output[6 + id] = as_type<uint>(int(divided));
        }";
    let translated =
        metal_translate::translate_msl(source, &metal_translate::TargetProfile::vulkan_1_3())
            .expect("the short fallback kernel translates");
    let library = library::retain_library(device, translated, None, None);
    assert!(!library.is_null());
    let input = [0u32, 1, 0x7fff, 0x8000, 0xfffe, 0xffff];
    let mut expected = [0u32; 12];
    for (index, value) in input.iter().copied().enumerate() {
        expected[index] = u32::from((value as u16).wrapping_add(1));
        let signed = value as u16 as i16;
        expected[6 + index] = u32::from_ne_bytes(i32::from(signed / -2).to_ne_bytes());
    }

    // SAFETY: test-owned Metal objects and mapped shared buffers.
    unsafe {
        let name = foundation::string::from_str("probe");
        let function = new_function_with_name(library, std::ptr::null(), name);
        let mut error = std::ptr::null_mut();
        let pipeline = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        assert!(!pipeline.is_null() && error.is_null());
        let input_buffer = new_buffer_with_bytes(
            device,
            std::ptr::null(),
            input.as_ptr().cast(),
            std::mem::size_of_val(&input),
            0,
        );
        let output_buffer = new_buffer(
            device,
            std::ptr::null(),
            std::mem::size_of_val(&expected),
            0,
        );
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", pipeline);
        set_buffer_at(encoder, input_buffer, 0, 0);
        set_buffer_at(encoder, output_buffer, 0, 1);
        dispatch(encoder, input.len() as u64);
        send_void(encoder, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");

        let output = object::body::<resource::Buffer>(output_buffer)
            .inner
            .contents
            .cast::<u32>();
        assert_eq!(std::slice::from_raw_parts(output, expected.len()), expected);
        for object in [
            name,
            pipeline,
            function,
            library,
            input_buffer,
            output_buffer,
            queue,
        ] {
            objc::arc::release(object);
        }
    }
}
