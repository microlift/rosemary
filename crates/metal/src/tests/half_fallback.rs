use super::*;

#[test]
fn software_half_rounding_matches_binary16_on_the_gpu() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let source = "#include <metal_stdlib>
        using namespace metal;
        kernel void probe(device uint* output [[buffer(0)]]) {
            half tie_even_down = half(1.00048828125f);
            half tie_even_up = half(1.00146484375f);
            half minimum_subnormal = half(5.960464477539063e-8f);
            half tie_to_zero = half(2.98023223876953125e-8f);
            half overflow = half(65520.0f);
            half rounded_product = half(1.0009765625f) * half(1.0009765625f);
            output[0] = as_type<uint>(float(tie_even_down));
            output[1] = as_type<uint>(float(tie_even_up));
            output[2] = as_type<uint>(float(minimum_subnormal));
            output[3] = as_type<uint>(float(tie_to_zero));
            output[4] = as_type<uint>(float(overflow));
            output[5] = as_type<uint>(float(rounded_product));
        }";
    let translated =
        metal_translate::translate_msl(source, &metal_translate::TargetProfile::vulkan_1_3())
            .expect("the fallback kernel translates");
    let library = library::retain_library(device, translated, None, None);
    assert!(!library.is_null());

    // SAFETY: test-owned Metal objects and the selectors they implement.
    unsafe {
        let name = foundation::string::from_str("probe");
        let function = new_function_with_name(library, std::ptr::null(), name);
        let mut error = std::ptr::null_mut();
        let pipeline = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        assert!(!pipeline.is_null() && error.is_null());
        let buffer = new_buffer(device, std::ptr::null(), 6 * 4, 0);
        let queue = new_command_queue(device, std::ptr::null());
        let command = send(queue, "commandBuffer");
        let encoder = send(command, "computeCommandEncoder");
        send_with_object(encoder, "setComputePipelineState:", pipeline);
        set_buffer_at(encoder, buffer, 0, 0);
        dispatch(encoder, 1);
        send_void(encoder, "endEncoding");
        send_void(command, "commit");
        send_void(command, "waitUntilCompleted");

        let contents = object::body::<resource::Buffer>(buffer)
            .inner
            .contents
            .cast::<u32>();
        assert_eq!(
            std::slice::from_raw_parts(contents, 6),
            [
                0x3f80_0000,
                0x3f80_4000,
                0x3380_0000,
                0x0000_0000,
                0x7f80_0000,
                0x3f80_4000,
            ]
        );
        for object in [name, pipeline, function, library, buffer, queue] {
            objc::arc::release(object);
        }
    }
}

/// Run `probe(input, output, id)` once per 32-bit input word and return the
/// 32-bit words it writes.
fn run_word_probe(device: Id, source: &str, input: &[u32]) -> Vec<u32> {
    let translated =
        metal_translate::translate_msl(source, &metal_translate::TargetProfile::vulkan_1_3())
            .expect("the fallback kernel translates");
    let library = library::retain_library(device, translated, None, None);
    assert!(!library.is_null());
    // SAFETY: test-owned Metal objects and mapped shared buffers.
    unsafe {
        let name = foundation::string::from_str("probe");
        let function = new_function_with_name(library, std::ptr::null(), name);
        let mut error = std::ptr::null_mut();
        let pipeline = new_compute_pipeline(device, std::ptr::null(), function, &mut error);
        assert!(!pipeline.is_null() && error.is_null());
        let bytes = std::mem::size_of_val(input);
        let input_buffer =
            new_buffer_with_bytes(device, std::ptr::null(), input.as_ptr().cast(), bytes, 0);
        let output_buffer = new_buffer(device, std::ptr::null(), bytes, 0);
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
        let words = std::slice::from_raw_parts(output, input.len()).to_vec();
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
        words
    }
}

#[test]
fn software_half_rounding_matches_reference_values_on_the_gpu() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let source = "#include <metal_stdlib>
        using namespace metal;
        kernel void probe(device float* input [[buffer(0)]],
            device uint* output [[buffer(1)]], uint id [[thread_position_in_grid]]) {
            half value = half(input[id]);
            output[id] = as_type<uint>(float(value));
        }";
    let mut bits = Vec::new();
    for exponent in 0..=255u32 {
        for mantissa in [
            0,
            1,
            0x0fff,
            0x1000,
            0x1001,
            0x1fff,
            0x2000,
            0x003f_ffff,
            0x007f_ffff,
        ] {
            bits.push((exponent << 23) | mantissa);
            bits.push(0x8000_0000 | (exponent << 23) | mantissa);
        }
    }
    let mut random = 0x1234_5678u32;
    for _ in 0..16_384 {
        random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        if random & 0x7f80_0000 != 0x7f80_0000 {
            bits.push(random);
        }
    }
    let expected: Vec<u32> = bits
        .iter()
        .map(|bits| {
            half::f16::from_f32(f32::from_bits(*bits))
                .to_f32()
                .to_bits()
        })
        .collect();
    assert!(bits.len() < 65_536);
    assert_eq!(run_word_probe(device, source, &bits), expected);
}

#[test]
fn software_half_rounds_integer_conversions_on_the_gpu() {
    isolate_metal_fixture!();
    let Some(device) = gpu_device() else {
        return;
    };
    let source = "#include <metal_stdlib>
        using namespace metal;
        kernel void probe(device int* input [[buffer(0)]],
            device uint* output [[buffer(1)]], uint id [[thread_position_in_grid]]) {
            half value = half(input[id]);
            output[id] = as_type<uint>(float(value));
        }";
    // Ties at 2049 and 2051 round to even; 65520 and beyond overflow.
    let input = [2048i32, 2049, 2051, -2049, 65_504, 65_519, 65_520, 1 << 20];
    let words: Vec<u32> = input.iter().map(|value| *value as u32).collect();
    let expected: Vec<u32> = input
        .iter()
        .map(|value| half::f16::from_f32(*value as f32).to_f32().to_bits())
        .collect();
    assert_eq!(run_word_probe(device, source, &words), expected);
}
