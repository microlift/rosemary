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
