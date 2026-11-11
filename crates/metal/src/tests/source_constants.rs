use super::*;

#[test]
fn source_function_constants_keep_library_inputs_and_created_function_values() {
    isolate_metal_fixture!();
    let device = device_for_tests();
    let source = "constant uint width [[function_constant(3)]];
        constant uint effective = is_function_constant_defined(width) ? width : 8u;
        fragment float4 output_width() { return float4(float(effective + BIAS)); }";
    let mut macros = vec![("BIAS".to_owned(), "2u".to_owned())];
    let original_macros = macros.clone();
    let mut error = std::ptr::null_mut();
    // SAFETY: live objects and the registered API signatures used by the guest.
    unsafe {
        let library =
            library::library_from_source(device, Some(source.as_bytes()), &macros, &mut error);
        assert!(!library.is_null());
        assert!(error.is_null());
        macros[0].1 = "99u".to_owned();
        let constants = constant_values();
        let (set, sel) = method::<unsafe extern "C" fn(Id, Sel, *const std::ffi::c_void, u64, u64)>(
            constants,
            "setConstantValue:type:atIndex:",
        );
        let mut value = 12u32;
        set(constants, sel, std::ptr::from_ref(&value).cast(), 33, 3);
        value = 100;
        let name = foundation::string::from_str("output_width");
        let function =
            new_function_with_constants(library, std::ptr::null(), name, constants, &mut error);
        assert!(!function.is_null());
        assert!(error.is_null());
        assert_eq!(value, 100);
        send_void(constants, "reset");
        let default =
            new_function_with_constants(library, std::ptr::null(), name, constants, &mut error);
        assert!(!default.is_null());
        let snapshot = object::body::<Function>(function)
            .artifact()
            .unwrap()
            .words
            .clone();
        assert_ne!(
            snapshot,
            object::body::<Function>(default).artifact().unwrap().words
        );
        let mut expected_values = metal_translate::FunctionConstantValues::new();
        expected_values
            .set_index(3, 33, &12u32.to_le_bytes())
            .unwrap();
        let expected = metal_translate::translate_msl_specialized(
            source,
            &original_macros,
            &metal_translate::TargetProfile::vulkan_1_3(),
            metal_translate::PassPipeline::from_env(),
            &expected_values,
        )
        .unwrap();
        assert_eq!(snapshot, expected.entries[0].words);
        objc::arc::release(library);
        objc::arc::release(constants);
        objc::arc::release(name);
        assert_eq!(
            snapshot,
            object::body::<Function>(function).artifact().unwrap().words
        );
        objc::arc::release(function);
        objc::arc::release(default);
    }
}
