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

/// `io_connect_t IORegisterForSystemPower(void *refcon, IONotificationPortRef *,
/// IOServiceInterestCallback, io_object_t *notifier)`.
///
/// The signature is the one the guest's own headers give it, and it is not the
/// one the name suggests: the return value is **not** a status. It is the
/// `io_connect_t` session with `IOPMrootDomain`, and `IOPMLib.h` says so
/// outright - "Returns a io_connect_t session for the IOPMrootDomain **or
/// IO_OBJECT_NULL if request failed**", with the caller closing the return
/// value through `IOServiceClose`.
///
/// So the two out-parameters and the answer all have to be `IO_OBJECT_NULL`:
/// there is no `IOPMrootDomain` on this host, there is no notification port to
/// allocate on its behalf, and there is no notifier to hand back. The caller
/// stores the return value and later closes it
/// (`base/power_monitor/power_monitor_device_source_mac.mm`,
/// `PlatformDestroy`); it does not test it in a release build, because the
/// `DCHECK_NE` next to the assignment in `PlatformInit` is compiled out - so
/// this is a truthful answer the guest is verified to tolerate, not a silent
/// accept.
pub extern "C" fn IORegisterForSystemPower(
    _refcon: *mut std::ffi::c_void,
    port_out: *mut *mut std::ffi::c_void,
    _callback: Option<unsafe extern "C" fn(*mut std::ffi::c_void, u32, u32, *mut std::ffi::c_void)>,
    notifier: *mut u32,
) -> u32 {
    // Both out-parameters are written even on the failure path, because a
    // caller that has just been told "there is no root domain" still owns a
    // `ScopedIONotificationPortRef` and a `notifier` to tear down, and reading
    // either uninitialised is exactly the unowned-pointer bug this avoids.
    if !port_out.is_null() {
        // SAFETY: the caller's `IONotificationPortRef *`.
        unsafe { port_out.write(std::ptr::null_mut()) };
    }
    if !notifier.is_null() {
        // SAFETY: the caller's `io_object_t *`.
        unsafe { notifier.write(0) };
    }
    crate::trace_hid(format_args!(
        "IORegisterForSystemPower -> IO_OBJECT_NULL: there is no IOPMrootDomain here"
    ));
    0
}

/// `IOReturn IODeregisterForSystemPower(io_object_t *notifier)`.
///
/// `kIOReturnNoDevice`, which is what there is to say: [`IORegisterForSystemPower`]
/// registered nothing, so there is no notifier to unregister. The return value
/// is discarded by the one caller in the guest
/// (`PowerMonitorDeviceSource::PlatformDestroy`), so naming the refusal costs
/// nothing and is true.
pub extern "C" fn IODeregisterForSystemPower(_notifier: *mut u32) -> i32 {
    crate::registry::NO_DEVICE
}

/// `IOReturn IOAllowPowerChange(io_connect_t kernelPort, intptr_t id)`.
///
/// Only ever reached from a `kIOMessageCanSystemSleep` or
/// `kIOMessageSystemWillSleep` callback, and no such message can arrive: the
/// notification port [`IORegisterForSystemPower`] declined to create has no
/// source to be posted to. `kIOReturnNoDevice` is the true answer for the
/// argument the guest would pass - the `io_connect_t` it stored, which is
/// `IO_OBJECT_NULL`.
///
/// `IOCancelPowerChange` is this function's other spelling, with the same two
/// arguments and the same answer: there is no notification to cancel either,
/// because there was never one to allow.
pub extern "C" fn IOAllowPowerChange(_kernel_port: u32, _notification_id: isize) -> i32 {
    crate::registry::NO_DEVICE
}

/// `IOReturn IOPMAssertionCreateWithName(CFStringRef type,
/// IOPMAssertionLevel level, CFStringRef name, IOPMAssertionID *id)`.
///
/// `kIOReturnNoDevice`. An assertion is a hold on a piece of Mac power
/// management hardware, and there is none here, so no assertion can be created.
///
/// The guest checks this: `services/device/wake_lock/power_save_blocker/
/// power_save_blocker_mac.mm` VLOGs the return code and carries on without the
/// assertion, which is the whole of what a power-save blocker does when it
/// cannot hold a block.
///
/// # Safety
///
/// `type` and `name` are read as `CFStringRef`; `id` is written only on
/// success, and this never succeeds, so it is left untouched.
pub unsafe extern "C" fn IOPMAssertionCreateWithName(
    assertion_type: CFTypeRef,
    _level: i32,
    name: CFTypeRef,
    id: *mut u32,
) -> i32 {
    // SAFETY: both are `CFStringRef` or null, which is what the API takes;
    // `describe` reads them and nothing else.
    let described = describe(assertion_type, name);
    crate::trace_hid(format_args!(
        "IOPMAssertionCreateWithName({described}) -> kIOReturnNoDevice"
    ));
    // Not written: `*id` on a failure is the caller's to leave alone, and a
    // garbage id released later would be a use of memory this never allocated.
    let _ = id;
    crate::registry::NO_DEVICE
}

/// `IOReturn IOPMAssertionRelease(IOPMAssertionID)`.
///
/// `kIOReturnNoDevice`, for the same reason: no assertion was ever created, so
/// there is none to release. The one caller tests the return only to decide
/// whether to log it.
pub extern "C" fn IOPMAssertionRelease(_assertion: u32) -> i32 {
    crate::registry::NO_DEVICE
}

/// `IOReturn IOPMCopyCPUPowerStatus(CFDictionaryRef *status)`.
///
/// `kIOReturnNotFound`, which is what `IOPMLib.h` documents this exact case
/// as: "Returns kIOReturnNotFound if CPU PowerStatus has not been published."
/// It has not: `IOPMrootDomain` publishes it, and there is none.
///
/// The out-parameter is documented as populated only on success, so it is left
/// alone. The guest checks the return and falls back to its own
/// "unknown frequency" answer (`base/power_monitor/cpu_frequency_utils.cc`).
///
/// # Safety
///
/// `status` is a `CFDictionaryRef *`; nothing is written to it.
pub unsafe extern "C" fn IOPMCopyCPUPowerStatus(_status: *mut CFTypeRef) -> i32 {
    crate::registry::NOT_FOUND
}

/// Name an assertion for the trace line, from whatever the guest passed.
fn describe(assertion_type: CFTypeRef, name: CFTypeRef) -> String {
    let text = |value: CFTypeRef| {
        // SAFETY: a `CFStringRef` or null.
        unsafe { corefoundation::string::as_string(value) }
            .map(|s| String::from_utf8_lossy(&s.utf8()).into_owned())
            .unwrap_or_else(|| "<null>".to_string())
    };
    format!("{}, {}", text(assertion_type), text(name))
}

exports::exports! {
    functions {
        "IOPSCopyPowerSourcesInfo" => IOPSCopyPowerSourcesInfo,
        "IOPSCopyPowerSourcesList" => IOPSCopyPowerSourcesList,
        "IOPSGetPowerSourceDescription" => IOPSGetPowerSourceDescription,
        "IOPSNotificationCreateRunLoopSource" => IOPSNotificationCreateRunLoopSource,
        "IORegisterForSystemPower" => IORegisterForSystemPower,
        "IODeregisterForSystemPower" => IODeregisterForSystemPower,
        "IOAllowPowerChange" | "IOCancelPowerChange" => IOAllowPowerChange,
        "IOPMAssertionCreateWithName" => IOPMAssertionCreateWithName,
        "IOPMAssertionRelease" => IOPMAssertionRelease,
        "IOPMCopyCPUPowerStatus" => IOPMCopyCPUPowerStatus,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two out-parameters and the return must all say "no such device"
    /// together: the caller stores the return and tears the other two down, so
    /// a port or a notifier left unwritten would be read uninitialised.
    #[test]
    fn registering_for_system_power_reports_no_root_domain_on_every_out() {
        let mut port: *mut std::ffi::c_void = 0x1234_5678usize as *mut _;
        let mut notifier: u32 = 0xdead_beef;
        let connect = IORegisterForSystemPower(
            std::ptr::null_mut(),
            &mut port,
            None,
            &mut notifier,
        );
        assert_eq!(connect, 0, "IO_OBJECT_NULL, per IOPMLib.h");
        assert!(port.is_null());
        assert_eq!(notifier, 0);
    }

    /// A null out-parameter must not fault: it is the one shape a caller can
    /// get wrong, and the refusal is the same either way.
    #[test]
    fn the_refusal_survives_a_caller_that_passed_no_out_parameters() {
        let connect = IORegisterForSystemPower(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            None,
            std::ptr::null_mut(),
        );
        assert_eq!(connect, 0);
    }

    /// `kIOReturnNoDevice` and `kIOReturnNotFound` are the codes the headers
    /// name for these two cases. Getting the *number* right is the difference
    /// between a refusal a caller can classify and a refusal it can only print.
    #[test]
    fn the_refusals_are_the_codes_iokit_names() {
        assert_eq!(
            crate::registry::NO_DEVICE as u32,
            0xe000_0000 | 0x2c0,
            "iokit_common_err(0x2c0)"
        );
        assert_eq!(
            crate::registry::NOT_FOUND as u32,
            0xe000_0000 | 0x2f0,
            "iokit_common_err(0x2f0)"
        );
        assert_eq!(
            crate::registry::NOT_FOUND,
            // SAFETY: a null out-parameter is the case being tested; nothing is
            // written through it.
            unsafe { IOPMCopyCPUPowerStatus(std::ptr::null_mut()) }
        );
        assert_eq!(IODeregisterForSystemPower(std::ptr::null_mut()), crate::registry::NO_DEVICE);
        assert_eq!(IOAllowPowerChange(0, 0), crate::registry::NO_DEVICE);
    }
}
