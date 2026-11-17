use metal_translate::{TargetProfile, translate_msl};
use rspirv::binary::Disassemble;

const SOURCE: &str =
    "struct Out { float4 position [[position]]; uint viewport [[viewport_array_index]]; };
    vertex Out indexed(uint instance [[instance_id]]) {
        Out result; result.position = float4(0.0, 0.0, 0.0, 1.0);
        result.viewport = instance; return result;
    }";

#[test]
fn vertex_viewport_indices_require_the_output_feature() {
    let mut target = TargetProfile {
        multi_viewport: true,
        ..TargetProfile::vulkan_1_3()
    };
    let error = translate_msl(SOURCE, &target).unwrap_err().to_string();
    assert!(error.contains("ShaderViewportIndex"), "{error}");
    target.shader_output_viewport_index = true;
    let library = translate_msl(SOURCE, &target).unwrap();
    let module = rspirv::dr::load_words(&library.entries[0].words).unwrap();
    let text = module.disassemble();
    assert!(
        text.contains("OpCapability ShaderViewportIndex\n"),
        "{text}"
    );
    assert!(!text.contains("ShaderViewportIndexLayerEXT"));
    target.multi_viewport = false;
    assert!(
        translate_msl(SOURCE, &target)
            .unwrap_err()
            .to_string()
            .contains("MultiViewport")
    );
}

#[test]
fn vulkan_11_uses_the_viewport_output_extension() {
    let target = TargetProfile {
        vulkan_version: (1, 1),
        spirv_version: (1, 3),
        multi_viewport: true,
        shader_output_viewport_index: true,
        ..TargetProfile::vulkan_1_3()
    };
    let library = translate_msl(SOURCE, &target).unwrap();
    let module = rspirv::dr::load_words(&library.entries[0].words).unwrap();
    let text = module.disassemble();
    assert!(
        text.contains("OpCapability ShaderViewportIndexLayerEXT"),
        "{text}"
    );
    assert!(text.contains("SPV_EXT_shader_viewport_index_layer"));
}

#[test]
fn fragment_viewport_index_is_flat_without_vertex_output_feature() {
    let target = TargetProfile {
        multi_viewport: true,
        ..TargetProfile::vulkan_1_3()
    };
    let source = "fragment float4 colour(uint index [[viewport_array_index]]) { return float4(float(index)); }";
    let library = translate_msl(source, &target).unwrap();
    let module = rspirv::dr::load_words(&library.entries[0].words).unwrap();
    let viewport = module
        .annotations
        .iter()
        .find(|i| {
            i.operands.contains(&rspirv::dr::Operand::BuiltIn(
                rspirv::spirv::BuiltIn::ViewportIndex,
            ))
        })
        .unwrap()
        .operands[0]
        .clone();
    assert!(module.annotations.iter().any(|i| i.operands
        == [
            viewport.clone(),
            rspirv::dr::Operand::Decoration(rspirv::spirv::Decoration::Flat)
        ]));
    assert!(!module.disassemble().contains("ShaderViewportIndex"));
}
