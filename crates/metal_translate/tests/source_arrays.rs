use metal_translate::{TargetProfile, translate_msl};

#[test]
fn array_reference_templates_match_fixed_bounds_and_update_the_caller() {
    for count in [1, 3, 6] {
        let values = (0..count)
            .map(|i| format!("{}.0", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!(
            "template<typename T, uint N>
            void transfer(thread T (&destination)[N], thread const T (&source)[N]) {{
                T scratch[N];
                for (uint i = 0; i < N; i++) scratch[i] = source[i];
                for (uint i = 0; i < N; destination[i] = scratch[i] + T(1.0), i++);
            }}
            fragment float4 probe() {{
                float source[{count}] = {{ {values} }};
                float destination[{count}] = {{}};
                transfer(destination, source);
                return float4(destination[0], destination[{last}], source[0], source[{last}]);
            }}",
            last = count - 1
        );
        let fixed = source
            .replace("template<typename T, uint N>", "")
            .replace("T ", "float ")
            .replace("T(", "float(")
            .replace("[N]", &format!("[{count}]"))
            .replace("i < N", &format!("i < {count}u"));
        let target = TargetProfile::vulkan_1_3();
        let actual = translate_msl(&source, &target).unwrap();
        let expected = translate_msl(&fixed, &target).unwrap();
        assert_eq!(
            actual.entries[0].words, expected.entries[0].words,
            "{count}"
        );
    }
}

#[test]
fn array_reference_template_deduction_rejects_conflicting_lengths_and_types() {
    for second in ["float second[3] = {};", "int second[2] = {};"] {
        let source = format!("template<typename T, uint N>
            void transfer(thread T (&first)[N], thread const T (&second)[N]) {{ first[0] = second[0]; }}
            fragment float4 probe() {{ float first[2] = {{}}; {second} transfer(first, second); return float4(first[0]); }}");
        assert!(translate_msl(&source, &TargetProfile::vulkan_1_3()).is_err());
    }
}

#[test]
fn comma_expressions_sequence_effects_and_return_the_right_value() {
    let prefix = "void first(thread uint& x) { x = 1u; }
        void second(thread uint& x) { x += 2u; }
        void third(thread uint& x) { x *= 3u; }";
    let combined = format!(
        "{prefix} fragment uint4 probe() {{ uint x = 0u;
        (first(x), second(x), third(x)); return uint4(x, (5u, 7u), 0u, 1u); }}"
    );
    let separate = format!(
        "{prefix} fragment uint4 probe() {{ uint x = 0u;
        first(x); second(x); third(x); return uint4(x, 7u, 0u, 1u); }}"
    );
    let target = TargetProfile::vulkan_1_3();
    assert_eq!(
        translate_msl(&combined, &target).unwrap().entries[0].words,
        translate_msl(&separate, &target).unwrap().entries[0].words
    );
    let barrier = "threadgroup_barrier(mem_flags::mem_threadgroup)";
    let combined = format!("kernel void probe() {{ ({barrier}, {barrier}); }}");
    let separate = format!("kernel void probe() {{ {barrier}; {barrier}; }}");
    assert_eq!(
        translate_msl(&combined, &target).unwrap().entries[0].words,
        translate_msl(&separate, &target).unwrap().entries[0].words
    );
}
