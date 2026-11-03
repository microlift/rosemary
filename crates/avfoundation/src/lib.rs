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
        metaclass.replace_method(
            objc::sel::get("authorizationStatusForMediaType:"),
            authorization_status_for_media_type as *const std::ffi::c_void,
            c"q24@0:8@16",
        );
        metaclass.replace_method(
            objc::sel::get("requestAccessForMediaType:completionHandler:"),
            request_access_for_media_type as *const std::ffi::c_void,
            c"v32@0:8@16@?24",
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

/// `AVAuthorizationStatusDenied`.
const AUTHORIZATION_STATUS_DENIED: isize = 2;
/// `AVAuthorizationStatusAuthorized`.
const AUTHORIZATION_STATUS_AUTHORIZED: isize = 3;

/// `AVMediaTypeAudio`.
const MEDIA_TYPE_AUDIO: &[u8] = b"soun";

/// Whether a program may capture `media_type`.
///
/// Audio is granted: recording goes through the HAL's input device, and the
/// host decides there whether this process may use a microphone, as its own
/// privacy prompt or sound server permissions say. Anything else is denied,
/// since there is no camera backend and a grant would send a caller to open
/// a session that cannot run.
fn authorized(media_type: Id) -> bool {
    // SAFETY: a guest passes an `NSString`, or nil.
    let Some(media_type) = (unsafe { corefoundation::string::as_string(media_type as _) }) else {
        return false;
    };
    media_type.utf8() == MEDIA_TYPE_AUDIO
}

fn authorization_status(media_type: Id) -> isize {
    match authorized(media_type) {
        true => AUTHORIZATION_STATUS_AUTHORIZED,
        false => AUTHORIZATION_STATUS_DENIED,
    }
}

/// `+[AVCaptureDevice authorizationStatusForMediaType:]`.
///
/// Never not-determined: the answer is already known, and a caller that saw
/// not-determined would prompt for something nothing here can change.
unsafe extern "C" fn authorization_status_for_media_type(
    _class: Id,
    _sel: Sel,
    media_type: Id,
) -> isize {
    authorization_status(media_type)
}

/// A `void (^)(BOOL granted)` waiting to be called with its answer.
struct Pending {
    handler: Id,
    granted: bool,
}

/// `isa`, `flags`, `reserved`, then `invoke`.
const BLOCK_INVOKE_OFFSET: usize = 16;

/// `+[AVCaptureDevice requestAccessForMediaType:completionHandler:]`.
///
/// Answers at once with the status above, on a global queue as AVFoundation
/// does: callers take a lock around the request that the handler takes too, so
/// calling it before returning would deadlock them.
unsafe extern "C" fn request_access_for_media_type(
    _class: Id,
    _sel: Sel,
    media_type: Id,
    handler: Id,
) {
    if handler.is_null() {
        return;
    }
    let pending = Box::new(Pending {
        handler: objc::block::copy(handler),
        granted: authorized(media_type),
    });
    // SAFETY: `answer` takes back the box this leaks, once.
    unsafe {
        darwin::dispatch::queue::async_f(
            darwin::dispatch::queue::get_global_queue(0, 0),
            Box::into_raw(pending) as u64,
            answer as *const () as u64,
        )
    };
}

/// Call a [`Pending`] handler and let go of it.
///
/// # Safety
///
/// `context` is a `Box<Pending>` [`request_access_for_media_type`] leaked.
unsafe extern "C" fn answer(context: *mut std::ffi::c_void) {
    // SAFETY: the caller's obligation.
    let pending = unsafe { Box::from_raw(context.cast::<Pending>()) };
    // SAFETY: a block literal, whose `invoke` is its fourth word.
    let invoke = unsafe {
        *(pending.handler as *const u8)
            .add(BLOCK_INVOKE_OFFSET)
            .cast::<Option<unsafe extern "C" fn(Id, i8)>>()
    };
    if let Some(invoke) = invoke {
        // SAFETY: the guest passed this block as a `void (^)(BOOL)`.
        unsafe { invoke(pending.handler, i8::from(pending.granted)) };
    }
    objc::block::release(pending.handler);
}

/// AVFoundation's string constants whose text is their own name.
///
/// Programs read these from static initializers, long before they decide
/// whether to capture or play anything, so an unbound one faults at load.
const NAMED_STRINGS: &[&str] = &[
    "AVCaptureSessionPresetLow",
    "AVCaptureSessionPresetMedium",
    "AVCaptureSessionPresetHigh",
    "AVCaptureSessionPresetPhoto",
    "AVCaptureSessionPreset320x240",
    "AVCaptureSessionPreset352x288",
    "AVCaptureSessionPreset640x480",
    "AVCaptureSessionPreset960x540",
    "AVCaptureSessionPreset1280x720",
    "AVCaptureSessionPreset1920x1080",
    "AVPlayerItemDidPlayToEndTimeNotification",
    "AVPlayerItemFailedToPlayToEndTimeNotification",
    "AVPlayerItemFailedToPlayToEndTimeErrorKey",
    "AVPlayerItemPlaybackStalledNotification",
    "AVURLAssetPreferPreciseDurationAndTimingKey",
];

/// AVFoundation's string constants whose text differs from their name.
const STRINGS: &[(&str, &str)] = &[
    ("_AVAudioTimePitchAlgorithmSpectral", "Spectral"),
    ("_AVAudioTimePitchAlgorithmTimeDomain", "TimeDomain"),
    ("_AVAudioTimePitchAlgorithmVarispeed", "Varispeed"),
];

/// The Objective-C class objects and constants exported by AVFoundation.framework.
pub fn data_symbols() -> Vec<(String, u64)> {
    bootstrap();
    let class = objc::class::lookup(c"AVCaptureDevice").expect("AVCaptureDevice is registered");
    let mut out = corefoundation::base::named_string_globals(NAMED_STRINGS);
    out.extend(corefoundation::base::string_globals(STRINGS));
    out.extend([
        (
            "_OBJC_CLASS_$_AVCaptureDevice".to_string(),
            objc::class::ptr(class) as usize as u64,
        ),
        (
            "_OBJC_METACLASS_$_AVCaptureDevice".to_string(),
            objc::class::ptr(class.metaclass()) as usize as u64,
        ),
    ]);
    out
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

    fn authorization_for(media_type: &str) -> isize {
        let implementation: unsafe extern "C" fn(Id, Sel, Id) -> isize =
            unsafe { std::mem::transmute(class_method("authorizationStatusForMediaType:")) };
        let media_type = corefoundation::string::from_bytes(media_type.as_bytes(), false);
        let status = unsafe {
            implementation(
                std::ptr::null_mut(),
                objc::sel::get("authorizationStatusForMediaType:"),
                media_type as Id,
            )
        };
        objc::arc::release(media_type as Id);
        status
    }

    #[test]
    fn capture_device_authorizes_audio() {
        assert_eq!(authorization_for("soun"), AUTHORIZATION_STATUS_AUTHORIZED);
    }

    #[test]
    fn capture_device_denies_video() {
        assert_eq!(authorization_for("vide"), AUTHORIZATION_STATUS_DENIED);
        let implementation: unsafe extern "C" fn(Id, Sel, Id) -> isize =
            unsafe { std::mem::transmute(class_method("authorizationStatusForMediaType:")) };
        let status = unsafe {
            implementation(
                std::ptr::null_mut(),
                objc::sel::get("authorizationStatusForMediaType:"),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, AUTHORIZATION_STATUS_DENIED);
    }

    /// The handler hears the same answer the status gives, and hears it after
    /// the request has returned.
    #[test]
    fn capture_device_request_answers_with_the_status() {
        use std::sync::atomic::{AtomicI8, Ordering};
        static HEARD: AtomicI8 = AtomicI8::new(-1);

        #[repr(C)]
        struct Literal {
            isa: *const std::ffi::c_void,
            flags: i32,
            reserved: i32,
            invoke: unsafe extern "C" fn(Id, i8),
            descriptor: *const std::ffi::c_void,
        }
        unsafe extern "C" fn heard(_block: Id, granted: i8) {
            HEARD.store(granted, Ordering::SeqCst);
        }
        const BLOCK_IS_GLOBAL: i32 = 1 << 28;
        let mut literal = Literal {
            isa: std::ptr::null(),
            flags: BLOCK_IS_GLOBAL,
            reserved: 0,
            invoke: heard,
            descriptor: std::ptr::null(),
        };
        let implementation: unsafe extern "C" fn(Id, Sel, Id, Id) = unsafe {
            std::mem::transmute(class_method("requestAccessForMediaType:completionHandler:"))
        };
        let media_type = corefoundation::string::from_bytes(b"soun", false);
        unsafe {
            implementation(
                std::ptr::null_mut(),
                objc::sel::get("requestAccessForMediaType:completionHandler:"),
                media_type as Id,
                (&raw mut literal).cast(),
            )
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while HEARD.load(Ordering::SeqCst) < 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(HEARD.load(Ordering::SeqCst), 1);
        objc::arc::release(media_type as Id);
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
