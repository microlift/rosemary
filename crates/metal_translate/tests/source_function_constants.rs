use metal_translate::ir::Operand;
use metal_translate::{
    FunctionConstantValues, MTL_DATA_TYPE_FLOAT, MTL_DATA_TYPE_UINT, PassPipeline, TargetProfile,
    parse_msl, specialize, translate_msl_specialized,
};

const SOURCE: &str = "
constant uint width [[function_constant(3)]];
constant uint effective = is_function_constant_defined(width) ? width : 8u;
constant uint doubled [[maybe_unused]] = effective * 2u;
fragment float4 output_width() { return float4(float(doubled)); }
";

#[test]
fn optional_source_constants_specialize_derived_globals_by_index_and_name() {
    for (by_name, supplied, expected) in [
        (false, None, 16),
        (false, Some(12u32), 24),
        (true, Some(5u32), 10),
    ] {
        let mut module = parse_msl(SOURCE).unwrap();
        let slot = module.globals[0].function_constant.as_ref().unwrap();
        assert_eq!(
            (slot.index, slot.name.as_str(), slot.encoding.as_str()),
            (3, "width", "j")
        );
        let mut values = FunctionConstantValues::new();
        if let Some(value) = supplied {
            if by_name {
                values
                    .set_name("width", MTL_DATA_TYPE_UINT, &value.to_le_bytes())
                    .unwrap();
            } else {
                values
                    .set_index(3, MTL_DATA_TYPE_UINT, &value.to_le_bytes())
                    .unwrap();
            }
        }
        specialize(&mut module, &values).unwrap();
        assert!(module.static_initializers.is_empty());
        let result = module
            .globals
            .iter()
            .find(|global| global.name == "doubled")
            .unwrap();
        assert_eq!(result.initializer, Some(Operand::u32(expected)));
    }
}

#[test]
fn source_constant_declarations_reject_invalid_indices_types_and_initializers() {
    for declaration in [
        "constant uint value [[function_constant(65536)]];",
        "constant uint value [[function_constant(-1)]];",
        "constant uint value [[function_constant(1)]] = 4u;",
        "constant uint value[2] [[function_constant(1)]];",
    ] {
        assert!(
            parse_msl(&format!(
                "{declaration} fragment float4 color() {{ return float4(0.0); }}"
            ))
            .is_err(),
            "{declaration}"
        );
    }
    let mut module = parse_msl(SOURCE).unwrap();
    let mut values = FunctionConstantValues::new();
    values
        .set_index(3, MTL_DATA_TYPE_FLOAT, &1.0f32.to_le_bytes())
        .unwrap();
    assert!(specialize(&mut module, &values).is_err());
}

#[test]
fn source_specialization_cache_separates_values_and_macro_defaults() {
    use metal_translate::translate_msl_specialized_cached;
    let source = SOURCE.replace("8u", "DEFAULT_WIDTH");
    let target = TargetProfile::vulkan_1_3();
    let passes = PassPipeline::default();
    let macros = |value: &str| vec![("DEFAULT_WIDTH".to_owned(), value.to_owned())];
    let empty = FunctionConstantValues::new();
    let first =
        translate_msl_specialized_cached(&source, &macros("8u"), &target, &passes, &empty).unwrap();
    let second =
        translate_msl_specialized_cached(&source, &macros("9u"), &target, &passes, &empty).unwrap();
    assert_ne!(first.entries[0].words, second.entries[0].words);
    let mut values = FunctionConstantValues::new();
    values
        .set_index(3, MTL_DATA_TYPE_UINT, &12u32.to_le_bytes())
        .unwrap();
    let overridden =
        translate_msl_specialized_cached(&source, &macros("8u"), &target, &passes, &values)
            .unwrap();
    assert_ne!(first.entries[0].words, overridden.entries[0].words);
    let repeated =
        translate_msl_specialized_cached(&source, &macros("8u"), &target, &passes, &empty).unwrap();
    assert_eq!(first.entries[0].words, repeated.entries[0].words);
}

#[test]
fn a_function_reading_a_constant_without_a_value_is_rejected_alone() {
    let target = TargetProfile::vulkan_1_3();
    for expression in ["width", "width * 2u"] {
        let source = format!(
            "constant uint width [[function_constant(3)]];
            constant uint required_value = {expression};
            fragment float4 required() {{ return float4(float(required_value)); }}
            fragment float4 independent() {{ return float4(1.0); }}"
        );
        let names = |library: &metal_translate::TranslatedLibrary| {
            let entries: Vec<_> = library.entries.iter().map(|e| e.name.clone()).collect();
            let rejected: Vec<_> = library.rejected.iter().map(|e| e.name.clone()).collect();
            (entries, rejected)
        };
        let empty = FunctionConstantValues::new();
        let unspecialized =
            translate_msl_specialized(&source, &[], &target, &PassPipeline::default(), &empty)
                .unwrap();
        assert_eq!(
            names(&unspecialized),
            (vec!["independent".to_owned()], vec!["required".to_owned()])
        );
        let mut values = FunctionConstantValues::new();
        values
            .set_index(3, MTL_DATA_TYPE_UINT, &4u32.to_le_bytes())
            .unwrap();
        let specialized =
            translate_msl_specialized(&source, &[], &target, &PassPipeline::default(), &values)
                .unwrap();
        assert_eq!(
            names(&specialized),
            (
                vec!["required".to_owned(), "independent".to_owned()],
                vec![]
            )
        );
    }
}

#[test]
fn literal_global_initializers_keep_their_diagnostics() {
    let error = parse_msl(
        "constant float table[4] = {1.0, 2.0};
        fragment float4 color() { return float4(table[0]); }",
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("initialized with 2 items"),
        "{error}"
    );
}

#[test]
fn globals_derived_from_other_globals_are_computed() {
    let mut module = parse_msl(
        "constant uint base = 3u;
        constant uint derived = base * 2u;
        fragment float4 color() { return float4(float(derived)); }",
    )
    .unwrap();
    specialize(&mut module, &FunctionConstantValues::new()).unwrap();
    let derived = module
        .globals
        .iter()
        .find(|global| global.name == "derived")
        .unwrap();
    assert_eq!(derived.initializer, Some(Operand::u32(6)));
}

#[test]
fn required_constants_leave_a_source_template_until_specialization() {
    use metal_translate::{
        PassPipeline, TargetProfile, compile_msl_template, translate_msl_template,
        translate_msl_template_specialized,
    };

    let source = "constant uint width [[function_constant(3)]];
        constant uint required_value = width * 2u;
        fragment float4 required_width() { return float4(float(required_value)); }";
    let target = TargetProfile::vulkan_1_3();
    let passes = PassPipeline::default();
    let template = compile_msl_template(source, &[]).unwrap();
    assert_eq!(
        template.functions().collect::<Vec<_>>(),
        vec![metal_translate::MslFunctionInfo {
            name: "required_width".to_owned(),
            stage: metal_translate::ir::Stage::Fragment,
        }]
    );
    assert_eq!(
        template.function_constants().collect::<Vec<_>>(),
        vec![metal_translate::FunctionConstantInfo {
            index: 3,
            name: "width".to_owned(),
            encoding: "j".to_owned(),
        }]
    );

    let base = translate_msl_template(&template, &target, &passes).unwrap();
    assert!(base.entries.is_empty());
    assert_eq!(base.rejected.len(), 1);
    assert_eq!(base.rejected[0].name, "required_width");
    assert!(base.rejected[0].reason.contains("has no defined value"));

    let empty = FunctionConstantValues::new();
    let unspecialized =
        translate_msl_template_specialized(&template, &target, &passes, &empty).unwrap();
    assert!(unspecialized.entries.is_empty());
    assert_eq!(unspecialized.rejected.len(), 1);
    assert_eq!(unspecialized.rejected[0].name, "required_width");
    let mut values = FunctionConstantValues::new();
    values.set_index(3, 33, &12u32.to_le_bytes()).unwrap();
    let specialized =
        translate_msl_template_specialized(&template, &target, &passes, &values).unwrap();
    assert_eq!(specialized.entries.len(), 1);
    assert_eq!(specialized.entries[0].name, "required_width");
}

#[test]
fn reflection_lists_only_constants_each_entry_uses() {
    let source = "constant uint width [[function_constant(3)]];
        constant uint height [[function_constant(4)]];
        constant uint effective_width = is_function_constant_defined(width) ? width : 8u;
        fragment float4 uses_width() { return float4(float(effective_width)); }
        fragment float4 uses_height() { return float4(float(height)); }
        fragment float4 uses_neither() { return float4(1.0); }";
    let module = parse_msl(source).unwrap();
    let reflected = metal_translate::entry_function_constants(&module);
    fn names(constants: &[metal_translate::FunctionConstantInfo]) -> Vec<(&str, u32)> {
        constants
            .iter()
            .map(|constant| (constant.name.as_str(), constant.index))
            .collect::<Vec<_>>()
    }
    assert_eq!(names(&reflected[0]), vec![("width", 3)]);
    assert_eq!(names(&reflected[1]), vec![("height", 4)]);
    assert!(reflected[2].is_empty());

    let template = metal_translate::compile_msl_template(source, &[]).unwrap();
    assert_eq!(
        names(&template.function_constants_for("uses_width")),
        vec![("width", 3)]
    );
    assert!(template.function_constants_for("uses_neither").is_empty());
}

#[test]
fn a_template_does_not_accept_an_ordinary_uninitialized_constant() {
    use metal_translate::compile_msl_template;
    let source = "constant uint missing;
        fragment float4 invalid() { return float4(float(missing)); }";
    let error = compile_msl_template(source, &[]).unwrap_err();
    assert!(error.to_string().contains("expected `=`"));
}
