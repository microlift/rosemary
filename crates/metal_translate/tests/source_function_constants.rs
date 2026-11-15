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
