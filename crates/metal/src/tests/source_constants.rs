use super::*;
use metal_translate::MTL_DATA_TYPE_UINT;

const OPTIONAL_WIDTH: &str = "constant uint width [[function_constant(3)]];
    constant uint effective = is_function_constant_defined(width) ? width : 8u;
    fragment float4 output_width() { return float4(float(effective + BIAS)); }";

const REQUIRED_WIDTH: &str = "constant uint width [[function_constant(3)]];
    fragment float4 output_width() { return float4(float(width)); }";

/// # Safety
///
/// `device` must be a live `RosemaryMTLDevice`.
unsafe fn source_library(device: Id, source: &str, macros: &[(String, String)]) -> Id {
    let mut error = std::ptr::null_mut();
    // SAFETY: the caller's device, and an error slot this function owns.
    let library = unsafe {
        library::library_from_source(device, Some(source.as_bytes()), macros, &mut error)
    };
    assert!(!library.is_null());
    assert!(error.is_null());
    library
}

/// `-setConstantValue:type:atIndex:` with a `uint`.
///
/// # Safety
///
/// `constants` must be a live `MTLFunctionConstantValues`.
unsafe fn set_uint(constants: Id, index: u64, value: u32) {
    // SAFETY: the registered signature of the selector.
    let (set, sel) = unsafe {
        method::<unsafe extern "C" fn(Id, Sel, *const std::ffi::c_void, u64, u64)>(
            constants,
            "setConstantValue:type:atIndex:",
        )
    };
    // SAFETY: a `uint` read as the type it is declared.
    unsafe {
        set(
            constants,
            sel,
            std::ptr::from_ref(&value).cast(),
            MTL_DATA_TYPE_UINT,
            index,
        );
    }
}

fn expected_words(source: &str, macros: &[(String, String)], width: Option<u32>) -> Vec<u32> {
    let mut values = metal_translate::FunctionConstantValues::new();
    if let Some(width) = width {
        values
            .set_index(3, MTL_DATA_TYPE_UINT, &width.to_le_bytes())
            .unwrap();
    }
    let translated = metal_translate::translate_msl_specialized(
        source,
        macros,
        &metal_translate::TargetProfile::vulkan_1_3(),
        metal_translate::PassPipeline::from_env(),
        &values,
    )
    .unwrap();
    translated.entries[0].words.clone()
}

/// # Safety
///
/// `function` must be a live `RosemaryMTLFunction`.
unsafe fn words(function: Id) -> Vec<u32> {
    // SAFETY: the caller's contract; the function holds its library.
    unsafe { object::body::<Function>(function).artifact() }
        .unwrap()
        .words
        .clone()
}

#[test]
fn source_specialization_uses_the_macros_and_values_given_when_made() {
    isolate_metal_fixture!();
    let device = device_for_tests();
    let mut macros = vec![("BIAS".to_owned(), "2u".to_owned())];
    let original_macros = macros.clone();
    let name = foundation::string::from_str("output_width");
    let mut error = std::ptr::null_mut();
    // SAFETY: live objects and the registered API signatures used by the guest.
    unsafe {
        let library = source_library(device, OPTIONAL_WIDTH, &macros);
        macros[0].1 = "99u".to_owned();
        let constants = constant_values();
        set_uint(constants, 3, 12);
        let function =
            new_function_with_constants(library, std::ptr::null(), name, constants, &mut error);
        assert!(!function.is_null());
        set_uint(constants, 3, 100);
        assert_eq!(
            words(function),
            expected_words(OPTIONAL_WIDTH, &original_macros, Some(12))
        );
        objc::arc::release(function);
        objc::arc::release(constants);
        objc::arc::release(library);
        objc::arc::release(name);
    }
}

#[test]
fn source_specialization_without_values_uses_the_default() {
    isolate_metal_fixture!();
    let device = device_for_tests();
    let macros = vec![("BIAS".to_owned(), "2u".to_owned())];
    let name = foundation::string::from_str("output_width");
    let mut error = std::ptr::null_mut();
    // SAFETY: live objects and the registered API signatures used by the guest.
    unsafe {
        let library = source_library(device, OPTIONAL_WIDTH, &macros);
        let constants = constant_values();
        let function =
            new_function_with_constants(library, std::ptr::null(), name, constants, &mut error);
        assert!(!function.is_null());
        assert_eq!(
            words(function),
            expected_words(OPTIONAL_WIDTH, &macros, None)
        );
        objc::arc::release(function);
        objc::arc::release(constants);
        objc::arc::release(library);
        objc::arc::release(name);
    }
}

#[test]
fn specialized_source_function_outlives_its_library() {
    isolate_metal_fixture!();
    let device = device_for_tests();
    let macros = vec![("BIAS".to_owned(), "2u".to_owned())];
    let name = foundation::string::from_str("output_width");
    let mut error = std::ptr::null_mut();
    // SAFETY: live objects and the registered API signatures used by the guest.
    unsafe {
        let library = source_library(device, OPTIONAL_WIDTH, &macros);
        let constants = constant_values();
        set_uint(constants, 3, 12);
        let function =
            new_function_with_constants(library, std::ptr::null(), name, constants, &mut error);
        objc::arc::release(library);
        objc::arc::release(constants);
        assert_eq!(
            words(function),
            expected_words(OPTIONAL_WIDTH, &macros, Some(12))
        );
        objc::arc::release(function);
        objc::arc::release(name);
    }
}

#[test]
fn source_function_reading_a_required_constant_is_made_only_with_its_value() {
    isolate_metal_fixture!();
    let device = device_for_tests();
    let name = foundation::string::from_str("output_width");
    let mut error = std::ptr::null_mut();
    // SAFETY: live objects and the registered API signatures used by the guest.
    unsafe {
        let library = source_library(device, REQUIRED_WIDTH, &[]);
        let constants = constant_values();
        let without =
            new_function_with_constants(library, std::ptr::null(), name, constants, &mut error);
        assert!(without.is_null());
        assert!(!error.is_null());
        set_uint(constants, 3, 12);
        error = std::ptr::null_mut();
        let function =
            new_function_with_constants(library, std::ptr::null(), name, constants, &mut error);
        assert!(!function.is_null());
        assert!(error.is_null());
        assert_eq!(
            words(function),
            expected_words(REQUIRED_WIDTH, &[], Some(12))
        );
        objc::arc::release(function);
        objc::arc::release(constants);
        objc::arc::release(library);
        objc::arc::release(name);
    }
}
