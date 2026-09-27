//! What this crate answers, and the one measurement that bounds it.
//!
//! AVFoundation is not implemented here. What is implemented is the surface the
//! guest actually reaches, and that surface is *five class-method sends to one
//! class*.
//!
//! | selector | asked at | answered |
//! |---|---|---|
//! | `+authorizationStatusForMediaType:` | `+0x4598703` | [`RESTRICTED`], for the reasons on [`authorization_status_for_media_type`] |
//! | `+requestAccessForMediaType:completionHandler:` | `+0x459894e` | **not** - and see below |
//! | `+deviceWithUniqueID:` | `+0x7914ccc` | **not** |
//! | `+reactionEffectsEnabled` | `+0x7933e8f` | **not** |
//! | `+reactionEffectGesturesEnabled` | `+0x7933eac` | **not** |
//!
//! Those five are the complete set, and the bound on it is not a sample. The
//! dyld bind stream (`test/tools/objc-classrefs.py`) gives
//! `_OBJC_CLASS_$_AVCaptureDevice` **one** `__objc_classrefs` slot, `0xe6e74f8`;
//! `test/tools/refs.py rip 0xe6e74f8` over all 214 MB of `__TEXT,__text` finds
//! **five** code sites that reference it, `+0x4598703`, `+0x459894e`,
//! `+0x7914ccc`, `+0x7933e8f` and `+0x7933eac`; and the selector each of those
//! loads resolves through `__objc_selrefs` to a distinct name, so five sites
//! means five sends and not four. A send the guest cannot spell is a send it
//! cannot make either: the selector pool `__TEXT,__objc_methname` (106 657
//! bytes) contains all five of these names.
//!
//! # `+devicesWithMediaType:` and `+defaultDeviceWithMediaType:`
//!
//! They are here and they are correct, and the measurement says they are not
//! load-bearing: **neither name is in the guest at all.** See
//! [`devices_with_media_type`] for the walk and the positive control in it.
//!
//! # The other three are not answered, on purpose
//!
//! `+requestAccessForMediaType:completionHandler:` is the one that matters, and
//! it is *unreachable while this crate's answer holds*: it is only called after
//! a `kNotDetermined`, which this host has no way to produce truthfully - see
//! [`RESTRICTED`]. It is left unimplemented rather than answered, so that if a
//! future answer ever does report `kNotDetermined`, the guest gets a refusal
//! that names itself instead of a completion block that never fires.
//!
//! `+deviceWithUniqueID:` and the two reaction-effects probes are left
//! unanswered, and the reason is not "not yet": the first is **nil-tolerant**
//! (`+0x7914ccc` retains its argument, sends, and then tests the answer -
//! `test rdx,rdx; je +0x7914f1c` - before storing it), so a nil from this
//! runtime is a value its caller already handles, and answering it as a device
//! would invent one. The other two are behind a prior check (`+0x7933e86` calls
//! a four-argument routine with `(1, 0xe, 0, 0)` and `test eax,eax; je
//! +0x7933eca` skips both sends when it answers zero) - what that check asks was
//! not determined, so no claim is made about it beyond that.
//!
//! **Which caller reaches those three was not determined.** They sit in a
//! compilation unit at `0x79xxxxxx`, not in `system_media_capture_permissions`
//! and not in `permission_bubble_media_access_handler`, and no `__FILE__` string
//! is referenced from their functions, so the file is unattributed here. They
//! are behind the milestone wall and are reported, not guessed at.

use objc::object::Id;
use objc::sel::Sel;

/// `AVAuthorizationStatus`, from `<AVFoundation/AVCaptureDevice.h>`.
///
/// The guest reads this as the raw return of `objc_msgSend` and uses it
/// unsigned, so the four enumerators are also the four answers this file is
/// allowed to give. [`MAX_VALUE`] is why, and the reason a value is
/// [`RESTRICTED`] is documented on the function.
pub mod av_authorization_status {
    /// "Determination of authorization has not yet been made."
    pub const NOT_DETERMINED: i64 = 0;
    /// "The client is prevented from accessing the resource, possibly due to
    /// some other restriction."
    pub const RESTRICTED: i64 = 1;
    /// "The client has been denied access to the resource."
    pub const DENIED: i64 = 2;
    /// The client has been granted access to the resource.
    pub const AUTHORIZED: i64 = 3;

    /// `kMaxValue`, and one past the last enumerator. The guest compares against
    /// this: `cmp r15, 0x4; jae` at `Framework+0x4598726` jumps to the `int3` at
    /// `+0x4598761` when the status is `>= 4`, which is the source's
    /// `default: NOTREACHED()`. So there is no value outside `0..=3` to answer
    /// with, and no `nil` either - `r15` *is* the status.
    pub const MAX_VALUE: i64 = 3;
}

/// The one answer this file gives, and the whole of why it is this one.
///
/// # The question
///
/// `+[AVCaptureDevice authorizationStatusForMediaType:]` asks macOS's **TCC** -
/// the privacy database, `/Library/Application Support/com.apple.TCC/TCC.db` -
/// whether the user has granted this application camera or microphone access.
/// The answer is a lookup in that database; it does not depend on whether a
/// capture device exists, which is why this method is reachable on a host with
/// no device at all (the device list is a *separate* query, and the guest does
/// not even make it - see the module docs).
///
/// # The state of this host
///
/// There is no TCC database, no capture device, and no mechanism by which a
/// grant could ever be recorded: the *only* thing that can put a decision into
/// TCC is the prompt that TCC itself raises, and TCC is what is missing. So of
/// the four enumerators, three assert something that is not the case here:
///
/// * **`AVAuthorizationStatusAuthorized` (3)** says a user granted access. There
///   is no user and no grant. It is also the one answer that would make the
///   guest believe capture is permitted.
/// * **`AVAuthorizationStatusDenied` (2)** says a user was asked and said no.
///   No user was asked and nothing was denied - there was nobody to ask and no
///   record to deny into.
/// * **`AVAuthorizationStatusNotDetermined` (0)** says a decision is *pending*.
///   That is a claim about a prompt that is waiting for an answer, and it is
///   worse than false here: the guest's own consumer treats this value as "ask
///   the OS", which means `[AVCaptureDevice
///   requestAccessForMediaType:completionHandler:]`
///   (`Framework+0x459894e`) and then waits on a completion block that no TCC
///   can ever call. That is a hang, not an answer.
/// * **`AVAuthorizationStatusRestricted` (1)** says the client is prevented from
///   reaching the resource, and the documented wording is deliberately broader
///   than "parental controls": *"possibly due to some other restriction"*. That
///   is the state of this host, and the reason is not a user decision.
///
/// # Is it a fact about this host, or a portable guess?
///
/// About this host. It is not a value that would still be right elsewhere: on a
/// Mac with a camera and a TCC database, an application that has not yet asked
/// is `NotDetermined`, and one the user allowed is `Authorized`. All four
/// enumerators are host-specific answers; only one of them is *true* here.
///
/// The one thing that would be wrong is to read `Restricted` as "a policy
/// exists that forbids this". Nothing here forbids anything. What is true is
/// the weaker statement the enumerator is able to make, and no enumerator can
/// make a stronger one without asserting a user that does not exist.
///
/// # The consequence for the milestone
///
/// The caller on the measured path is
/// `system_media_permissions::LogSystemMediaPermissionsStartupStats`
/// (`Framework+0x45908c0`, reached from `browser_process_impl.cc` at
/// `+0x56b9bb1`). For `Restricted` it logs one UMA enumeration sample and, on
/// a profile with no `system_permission.*.first_blocked_time` pref set, returns
/// out of `MaybeLogAdditional*SystemPermissionStats` immediately
/// (`test rax,rax; je` at `+0x459093a` and `+0x4590c31`). It then goes on to
/// `CheckSystemScreenCapturePermission()`, which is already answered on this
/// host: the gate at `+0x4590ec0` is
/// `FeatureList::IsEnabled(kMacSystemScreenCapturePermissionCheck)`, the
/// feature's second word at `0xe6da958` is `1`, and `1` is enabled - confirmed
/// five for five against the same field on `kNativeNotifications` (1),
/// `kSystemNotifications` (1), `kNewMacNotificationAPI` (0) and `kNoReferrers`
/// (0) - so the call at `+0x4590ed2` to `_CGPreflightScreenCaptureAccess` *is*
/// made, and is answered false by `crates/darwin/src/cg_unavailable.rs`. So the
/// `Restricted` branch is a leaf: it does not walk into a second unanswerable
/// call.
///
/// For `NotDetermined` the same function would return a value the *other*
/// consumer treats as terminal (`permission_bubble_media_access_handler.cc`,
/// `Framework+0x4560f50`: `kRestricted`/`kDenied` both set `final_result` to
/// `MediaStreamRequestResult::SYSTEM_PERMISSION_DENIED`, the `mov r8d, 0xf` at
/// `+0x4561074`; only `kNotDetermined` re-enters the OS). `Restricted` is the
/// answer that both consumers treat as a decision.
pub const RESTRICTED: i64 = av_authorization_status::RESTRICTED;

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
        // A class method: `authorizationStatusForMediaType:` is `+`, and the
        // guest's site (`mov rdi,QWORD PTR [rip+...] # 0xe6e74f8`,
        // `Framework+0x4598703`) loads a class reference, not an instance.
        metaclass.replace_method(
            objc::sel::get("authorizationStatusForMediaType:"),
            authorization_status_for_media_type as *const std::ffi::c_void,
            c"q24@0:8@16",
        );
    });
}

/// `+[AVCaptureDevice devicesWithMediaType:]`.
///
/// No capture backend exists, so every media type has no devices - the answer
/// a Mac without a camera or microphone gives, which callers already handle.
///
/// **Measured: the guest never sends this, and this is a negative with a
/// positive control in the same table.** Neither name is in
/// `__TEXT,__objc_methname` at all - that selector pool holds 106 657 bytes and
/// does contain `authorizationStatusForMediaType:`,
/// `requestAccessForMediaType:completionHandler:`, `deviceWithUniqueID:`,
/// `reactionEffectsEnabled` and `reactionEffectGesturesEnabled`, but neither of
/// these two. So there is no name for a selref to point at, and all 2 895
/// entries of `__DATA,__objc_selrefs` were walked to confirm none does. This is
/// not a device query the milestone is waiting on; it is here because the two
/// methods are the public surface of the class, and a fabricated device would
/// be a lie if anything ever asked.
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

/// `+[AVCaptureDevice authorizationStatusForMediaType:]` -> `AVAuthorizationStatus`.
///
/// [`RESTRICTED`], for the reasons in its own documentation. The media type is
/// not consulted and that is not a shortcut: the fact being reported is a
/// property of *this host* - it has no TCC, so no media type's authorization
/// can be granted - and that is the same answer for every argument, including
/// one this runtime has never heard of. Refusing an argument it cannot
/// classify would be refusing a question it can answer.
///
/// (The argument on the measured path is a real `NSString`, not nil: the guest
/// loads `&_AVMediaTypeAudio` out of its `__got` and dereferences it, and that
/// global is published at `crates/rosetta3/src/frameworks.rs:332` as the
/// four-character code `"soun"`. Nothing here depends on that being right.)
///
/// Said once, because a wrong answer that looks like success is worse than a
/// named refusal and this answer is a claim about a missing facility rather
/// than a lookup in one.
unsafe extern "C" fn authorization_status_for_media_type(
    _class: Id,
    _sel: Sel,
    _media_type: Id,
) -> i64 {
    said();
    RESTRICTED
}

/// One line, once - the same "say a refusal by name, once" shape as
/// `darwin::cg_unavailable::note`, and the reason it is here even though this is
/// an answer rather than a refusal: a wrong answer that looks like success is
/// worse than a named refusal, so an answer that is a *claim about a missing
/// facility* announces what it is claiming.
fn said() {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!(
            "rosetta3: AVCaptureDevice: +[authorizationStatusForMediaType:] answers \
             AVAuthorizationStatusRestricted (1) - this host has no TCC database, no capture \
             device and no way to record a grant, so no media type's authorization can be \
             granted; the other three answers each assert a user who does not exist. The \
             guest's own NOTREACHED for a value outside 0..3 is at Framework+0x4598761."
        );
    }
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

    fn status_for(media_type: Id) -> i64 {
        let implementation: unsafe extern "C" fn(Id, Sel, Id) -> i64 =
            unsafe { std::mem::transmute(class_method("authorizationStatusForMediaType:")) };
        unsafe {
            implementation(
                std::ptr::null_mut(),
                objc::sel::get("authorizationStatusForMediaType:"),
                media_type,
            )
        }
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
    fn authorization_status_is_restricted() {
        assert_eq!(
            status_for(std::ptr::null_mut()),
            av_authorization_status::RESTRICTED
        );
    }

    /// The value the guest's `cmp r15, 0x4; jae` demands, asserted as a range
    /// and not only as an equality, so a future edit to an out-of-range literal
    /// fails here rather than as an `int3` in the guest.
    #[test]
    fn authorization_status_is_inside_the_guests_accepted_range() {
        let status = status_for(std::ptr::null_mut());
        assert!(
            (0..=av_authorization_status::MAX_VALUE).contains(&status),
            "Framework+0x4598726 sends every value >= 4 to the int3 at +0x4598761, got {status}"
        );
    }

    /// The answer does not depend on the media type, and *that* is the honest
    /// shape: the fact is about this host's missing TCC, not about which type is
    /// being asked for. `nil` and the two constants the guest actually sends
    /// (`"soun"`, `"vide"`) and a string nobody has heard of all get the same
    /// answer, so the method cannot be smuggling a per-type guess.
    #[test]
    fn authorization_status_does_not_depend_on_the_media_type() {
        let pool = objc::arc::pool_push();
        for text in ["soun", "vide", "muxd", "not a media type"] {
            let string = foundation::string::from_str(text);
            assert_eq!(
                status_for(string),
                av_authorization_status::RESTRICTED,
                "{text}"
            );
        }
        assert_eq!(
            status_for(std::ptr::null_mut()),
            av_authorization_status::RESTRICTED
        );
        objc::arc::pool_pop(pool);
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

    /// The path the guest takes, not the IMP fetched by hand.
    ///
    /// The guest's site loads a **class reference** and calls `objc_msgSend`
    /// with the class object as the receiver (`Framework+0x4598703`), so the
    /// method has to be found by the runtime's own dispatch, from the class
    /// object, with no `ROSETTA3_LOG_UNRECOGNIZED` to paper over a miss - the
    /// previous test's `class.metaclass().method(...)` proves the method is
    /// *registered*, which is not the same claim.
    #[test]
    fn authorization_status_resolves_through_msg_send() {
        bootstrap();
        let class = objc::class::lookup(c"AVCaptureDevice").expect("AVCaptureDevice");
        let send: unsafe extern "C" fn(Id, Sel, Id) -> i64 =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let media_type = foundation::string::from_str("soun");
        let status = unsafe {
            send(
                objc::class::ptr(class) as Id,
                objc::sel::get("authorizationStatusForMediaType:"),
                media_type,
            )
        };
        assert_eq!(status, av_authorization_status::RESTRICTED);
    }
}
