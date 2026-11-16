use metal_translate::{TargetProfile, parse_msl, translate_msl};

#[test]
fn source_refraction_uses_the_standard_vector_operation() {
    for lanes in [2, 3, 4] {
        let source = format!(
            "fragment float{lanes} probe(float4 p [[position]]) {{
            return refract(float{lanes}(p.x), float{lanes}(p.y), p.z); }}"
        );
        let library = translate_msl(&source, &TargetProfile::vulkan_1_3()).unwrap();
        let module = rspirv::dr::load_words(&library.entries[0].words).unwrap();
        assert!(
            module
                .all_inst_iter()
                .any(
                    |instruction| instruction.class.opcode == rspirv::spirv::Op::ExtInst
                        && instruction.operands.get(1)
                            == Some(&rspirv::dr::Operand::LiteralExtInstInteger(
                                rspirv::spirv::GlslStd450Op::Refract as u32
                            ))
                )
        );
    }
    let target = TargetProfile {
        shader_float16: true,
        ..TargetProfile::vulkan_1_3()
    };
    translate_msl(
        "fragment half3 probe(float4 p [[position]]) {
        return refract(half3(p.x), half3(p.y), half(p.z)); }",
        &target,
    )
    .unwrap();
}

#[test]
fn source_refraction_rejects_mismatched_shapes() {
    for arguments in [
        "float2(1.0), float3(1.0), 1.0",
        "float3(1.0), float3(1.0), float2(1.0)",
        "1.0, 1.0, 1.0",
    ] {
        assert!(
            parse_msl(&format!(
                "fragment float4 probe() {{ return float4(refract({arguments})); }}"
            ))
            .is_err()
        );
    }
}
