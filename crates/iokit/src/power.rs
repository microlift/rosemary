#![allow(non_snake_case)]

use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections;

/// `CFTypeRef IOPSCopyPowerSourcesInfo(void)` - a `Copy`, so the caller
/// releases it.
pub unsafe extern "C" fn IOPSCopyPowerSourcesInfo() -> CFTypeRef {
    collections::new_dictionary(
        &[],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        false,
    )
}

/// `CFArrayRef IOPSCopyPowerSourcesList(CFTypeRef blob)`.
///
/// Empty, and owned by the caller. The blob is not consulted: this
/// implementation puts nothing in it.
pub unsafe extern "C" fn IOPSCopyPowerSourcesList(_blob: CFTypeRef) -> CFTypeRef {
    collections::new_array(&[], Policy::from_type_callbacks(), false)
}

/// `CFDictionaryRef IOPSGetPowerSourceDescription(CFTypeRef blob, CFTypeRef ps)`.
///
/// Null, which is the answer for a source that is not in the list - and the
/// list is empty, so no caller can hold one that is. A `Get`, so nothing is
/// owned either way.
pub unsafe extern "C" fn IOPSGetPowerSourceDescription(
    _blob: CFTypeRef,
    _source: CFTypeRef,
) -> CFTypeRef {
    std::ptr::null_mut()
}

/// `CFDictionaryRef IOPSCopyExternalPowerAdapterDetails(void)`.
///
/// Null, which is what a Mac with no adapter attached answers: wattage and
/// identity are an SMC's to report, and the host exposes none, so naming an
/// adapter would be inventing one. A `Copy`, so nothing is owned either way.
pub unsafe extern "C" fn IOPSCopyExternalPowerAdapterDetails() -> CFTypeRef {
    std::ptr::null_mut()
}

/// `CFRunLoopSourceRef IOPSNotificationCreateRunLoopSource(callback, context)`.
///
/// Null: nothing here changes power state, so a source would be one that never
/// fires. The callback is deliberately not retained - a program that gets null
/// knows it is not registered, where one holding a source that never fires
/// waits for a notification that cannot arrive.
pub unsafe extern "C" fn IOPSNotificationCreateRunLoopSource(
    _callback: *const std::ffi::c_void,
    _context: *mut std::ffi::c_void,
) -> CFTypeRef {
    std::ptr::null_mut()
}

/// Where the assertion ids come from. Zero is `kIOPMNullAssertionID`.
static NEXT_ASSERTION: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// `IOReturn IOPMAssertionCreateWithDescription(type, name, details, reason,
/// bundle, timeout, timeoutAction, IOPMAssertionID *)`.
///
/// Granted and not enforced: a guest asks to keep the display awake while it runs,
/// and a refusal would only make it report an error for something this host's
/// idle policy does not let it do anything about. Each id is distinct, so a
/// release names what it created.
pub unsafe extern "C" fn IOPMAssertionCreateWithDescription(
    _kind: CFTypeRef,
    _name: CFTypeRef,
    _details: CFTypeRef,
    _reason: CFTypeRef,
    _bundle: CFTypeRef,
    _timeout: f64,
    _timeout_action: CFTypeRef,
    assertion: *mut u32,
) -> i32 {
    new_assertion(assertion);
    0
}

/// `IOReturn IOPMAssertionRelease(IOPMAssertionID)`.
pub unsafe extern "C" fn IOPMAssertionRelease(_assertion: u32) -> i32 {
    0
}

/// A fresh, distinct assertion id, written through `out` when it is non-null.
fn new_assertion(out: *mut u32) {
    if !out.is_null() {
        let id = NEXT_ASSERTION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: an out parameter the caller sized.
        unsafe { out.write(id) };
    }
}

/// `IOReturn IOPMAssertionCreateWithName(CFStringRef type, IOPMAssertionLevel
/// level, CFStringRef name, IOPMAssertionID *)`.
///
/// As [`IOPMAssertionCreateWithDescription`]: granted, not enforced, and the
/// level is not consulted because nothing here ever checks an assertion's
/// state - there is no idle timer for one to hold off.
pub unsafe extern "C" fn IOPMAssertionCreateWithName(
    _kind: CFTypeRef,
    _level: u32,
    _name: CFTypeRef,
    assertion: *mut u32,
) -> i32 {
    new_assertion(assertion);
    0
}

/// `IOReturn IOPMAssertionDeclareUserActivity(CFStringRef name,
/// IOPMUserActiveType type, IOPMAssertionID *)`.
///
/// A one-shot assertion id and nothing else: the activity it declares would
/// reset this host's idle timer, and nothing here lets a guest's idleness
/// dim or sleep the display in the first place.
pub unsafe extern "C" fn IOPMAssertionDeclareUserActivity(
    _name: CFTypeRef,
    _kind: u32,
    assertion: *mut u32,
) -> i32 {
    new_assertion(assertion);
    0
}

exports::exports! {
    functions {
        "IOPMAssertionCreateWithDescription" => IOPMAssertionCreateWithDescription,
        "IOPMAssertionCreateWithName" => IOPMAssertionCreateWithName,
        "IOPMAssertionDeclareUserActivity" => IOPMAssertionDeclareUserActivity,
        "IOPMAssertionRelease" => IOPMAssertionRelease,
        "IOPSCopyPowerSourcesInfo" => IOPSCopyPowerSourcesInfo,
        "IOPSCopyPowerSourcesList" => IOPSCopyPowerSourcesList,
        "IOPSGetPowerSourceDescription" => IOPSGetPowerSourceDescription,
        "IOPSCopyExternalPowerAdapterDetails" => IOPSCopyExternalPowerAdapterDetails,
        "IOPSNotificationCreateRunLoopSource" => IOPSNotificationCreateRunLoopSource,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every assertion a guest creates gets a distinct id, whichever call
    /// made it - so a release names the one it is given back.
    #[test]
    fn assertions_from_either_entry_point_get_distinct_ids() {
        let mut a = 0u32;
        let mut b = 0u32;
        let mut c = 0u32;
        unsafe {
            assert_eq!(
                IOPMAssertionCreateWithName(std::ptr::null(), 255, std::ptr::null(), &mut a),
                0
            );
            assert_eq!(
                IOPMAssertionDeclareUserActivity(std::ptr::null(), 1, &mut b),
                0
            );
            assert_eq!(
                IOPMAssertionCreateWithDescription(
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0.0,
                    std::ptr::null(),
                    &mut c,
                ),
                0
            );
        }
        assert_ne!(a, 0);
        assert_ne!(b, 0);
        assert_ne!(c, 0);
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    /// A null out-parameter is accepted, as a caller that only wants to know
    /// whether the call succeeded is entitled to pass one.
    #[test]
    fn a_null_assertion_slot_does_not_crash() {
        unsafe {
            assert_eq!(
                IOPMAssertionCreateWithName(
                    std::ptr::null(),
                    255,
                    std::ptr::null(),
                    std::ptr::null_mut()
                ),
                0
            );
            assert_eq!(
                IOPMAssertionDeclareUserActivity(std::ptr::null(), 1, std::ptr::null_mut()),
                0
            );
        }
    }
}
