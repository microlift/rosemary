use objc::object::Id;
use objc::sel::Sel;

/// Install this crate's classes and methods. Idempotent.
pub fn bootstrap() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        foundation::bootstrap();
        let class = corefoundation::base::abstract_class(c"AVCaptureDevice", None);
        let metaclass = class.metaclass();
        metaclass.replace_method(
            objc::sel::get("devicesWithMediaType:"),
            devices_with_media_type as *const std::ffi::c_void,
            c"@24@0:8@16",
        );
        metaclass.replace_method(
            objc::sel::get("defaultDeviceWithMediaType:"),
            default_device_with_media_type as *const std::ffi::c_void,
            c"@24@0:8@16",
        );
    });
}

/// `+[AVCaptureDevice devicesWithMediaType:]`.
///
/// No capture backend exists, so every media type has no devices - the answer
/// a Mac without a camera or microphone gives, which callers already handle.
unsafe extern "C" fn devices_with_media_type(_class: Id, _sel: Sel, _media_type: Id) -> Id {
    let array = corefoundation::collections::new_array(
        &[],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    objc::arc::autorelease(array as Id)
}

/// `+[AVCaptureDevice defaultDeviceWithMediaType:]`: nil, for the same reason.
unsafe extern "C" fn default_device_with_media_type(_class: Id, _sel: Sel, _media_type: Id) -> Id {
    std::ptr::null_mut()
}

/// The Objective-C class objects exported by AVFoundation.framework.
pub fn data_symbols() -> Vec<(String, u64)> {
    bootstrap();
    let class = objc::class::lookup(c"AVCaptureDevice").expect("AVCaptureDevice is registered");
    vec![
        (
            "_OBJC_CLASS_$_AVCaptureDevice".to_string(),
            objc::class::ptr(class) as usize as u64,
        ),
        (
            "_OBJC_METACLASS_$_AVCaptureDevice".to_string(),
            objc::class::ptr(class.metaclass()) as usize as u64,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class_method(name: &str) -> *const std::ffi::c_void {
        bootstrap();
        let class = objc::class::lookup(c"AVCaptureDevice").expect("AVCaptureDevice");
        class
            .metaclass()
            .method(objc::sel::get(name))
            .expect("class method")
            .implementation()
    }

    #[test]
    fn capture_device_lists_no_devices() {
        let implementation: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            unsafe { std::mem::transmute(class_method("devicesWithMediaType:")) };
        let pool = objc::arc::pool_push();
        let array = unsafe {
            implementation(
                std::ptr::null_mut(),
                objc::sel::get("devicesWithMediaType:"),
                std::ptr::null_mut(),
            )
        };
        assert!(!array.is_null());
        let contents = unsafe { corefoundation::collections::array(array as _) }.expect("array");
        assert!(contents.values.is_empty());
        objc::arc::pool_pop(pool);
    }

    #[test]
    fn capture_device_has_no_default_device() {
        let implementation: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            unsafe { std::mem::transmute(class_method("defaultDeviceWithMediaType:")) };
        let device = unsafe {
            implementation(
                std::ptr::null_mut(),
                objc::sel::get("defaultDeviceWithMediaType:"),
                std::ptr::null_mut(),
            )
        };
        assert!(device.is_null());
    }

    #[test]
    fn capture_device_exports_its_class_and_metaclass() {
        let symbols = data_symbols();
        for name in [
            "_OBJC_CLASS_$_AVCaptureDevice",
            "_OBJC_METACLASS_$_AVCaptureDevice",
        ] {
            assert!(symbols.iter().any(|(n, a)| n == name && *a != 0), "{name}");
        }
    }
}
