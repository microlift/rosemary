use metal_translate::{TargetProfile, translate_msl};

#[test]
fn helper_returns_before_and_inside_a_loop_translate() {
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
    let library = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap();
    let module = rspirv::dr::load_words(&library.entries[0].words).unwrap();
    assert_eq!(
        module
            .all_inst_iter()
            .filter(|instruction| instruction.class.opcode == rspirv::spirv::Op::LoopMerge)
            .count(),
        1
    );
}

#[test]
fn loop_arms_sharing_a_continue_path_translate() {
    // Both arms of the selection reach the loop's latch through one shared
    // block, so the inner selections only get their own merges once that
    // path is cloned.
    let source = "kernel void probe(device float* output [[buffer(0)]], uint index [[thread_position_in_grid]]) {
        float t = output[index];
        int i = 0;
        int mode = 0;
        int misses = 0;
        float result = 0.0;
        while (true) {
            if (!(i < 16 && t < 100.0)) { break; }
            if (t > 70.0) { result = 0.0; break; }
            if (mode == 0) {
                if (output[i] > 0.0) {
                    t -= 1.0;
                    i += 1;
                    mode = -1;
                    continue;
                } else {
                    t += 4.0;
                }
                mode = 0;
            } else {
                float density = output[i + 1];
                if (density <= 0.0) {
                    misses += 1;
                    mode = misses < 10 ? -1 : 0;
                } else {
                    result += density;
                    if (result > 1.0) break;
                    mode = -1;
                }
                t += 1.0;
            }
            i += 1;
        }
        output[index] = result + t;
    }";
    let library = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap();
    let module = rspirv::dr::load_words(&library.entries[0].words).unwrap();
    assert_eq!(
        module
            .all_inst_iter()
            .filter(|instruction| instruction.class.opcode == rspirv::spirv::Op::LoopMerge)
            .count(),
        1
    );
}
