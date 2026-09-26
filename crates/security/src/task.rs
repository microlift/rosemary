#![allow(non_snake_case)]

//! `SecTask`: the running process's code-signing identity.
//!
//! Chromium asks for it before `main`. PartitionAlloc's `UseMapJit()` - reached
//! from `AllocatorShimOverrideAppleDefaultZone`, a static constructor - creates
//! a task for itself and asks whether it carries the
//! `com.apple.security.cs.allow-jit` entitlement, to decide whether to `mmap`
//! JIT regions with `MAP_JIT`. Left unbound, `SecTaskCreateFromSelf` was a jump
//! through a zero slot at Framework `+0x5ddaa59`.
//!
//! A process on this host has no code signature, so it has no entitlements, no
//! signing identifier and no code-signing status flags. That is also exactly
//! what macOS answers for an unsigned process - a null value and no error, not a
//! failure - so every answer here is the true one rather than a stand-in. In
//! particular `allow-jit` is absent, so a guest takes its "no `MAP_JIT`" path,
//! which on x86-64 is the ordinary one.

use corefoundation::base::CFTypeRef;
use objc::class::Class;

/// A `SecTaskRef`. It carries nothing: the only task it can describe is this
/// process, and the process has no signature to describe.
#[repr(C)]
struct Body {
    isa: *const Class,
}

fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        let class = corefoundation::base::register(c"__R3SecTask", None, size_of::<Body>(), true);
        objc::class::ptr(class) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("a task class") }
}

/// # Safety
///
/// `object` is null or an object this runtime allocated.
unsafe fn is_task(object: CFTypeRef) -> bool {
    // SAFETY: the caller's contract.
    unsafe { corefoundation::base::is_kind_of(object, class()) }
}

/// `SecTaskRef SecTaskCreateFromSelf(CFAllocatorRef allocator)`
///
/// A new task object for this process, owned by the caller. The allocator is
/// ignored, as it is for every CF object this runtime makes.
pub extern "C" fn SecTaskCreateFromSelf(_allocator: CFTypeRef) -> CFTypeRef {
    // SAFETY: `Body` is the registered layout.
    let body = unsafe { corefoundation::base::allocate::<Body>(class()) };
    body as CFTypeRef
}

/// `CFTypeRef SecTaskCopyValueForEntitlement(SecTaskRef task, CFStringRef
/// entitlement, CFErrorRef *error)`
///
/// Null, with `*error` left alone: the entitlement is absent, which is not an
/// error. See the module note - this is macOS's own answer for an unsigned
/// process, and the one a caller testing `if (error)` before `if (value)`
/// handles as "not entitled".
///
/// # Safety
///
/// `task` is a task this module made; `error` is null or writable.
pub unsafe extern "C" fn SecTaskCopyValueForEntitlement(
    task: CFTypeRef,
    _entitlement: CFTypeRef,
    _error: *mut CFTypeRef,
) -> CFTypeRef {
    // SAFETY: the caller's contract.
    debug_assert!(unsafe { is_task(task) }, "not a SecTaskRef");
    std::ptr::null()
}

/// `CFStringRef SecTaskCopySigningIdentifier(SecTaskRef task, CFErrorRef *error)`
///
/// Null: an unsigned process has no signing identifier, and macOS reports that
/// with a null result rather than an error.
///
/// # Safety
///
/// As [`SecTaskCopyValueForEntitlement`].
pub unsafe extern "C" fn SecTaskCopySigningIdentifier(
    task: CFTypeRef,
    _error: *mut CFTypeRef,
) -> CFTypeRef {
    // SAFETY: the caller's contract.
    debug_assert!(unsafe { is_task(task) }, "not a SecTaskRef");
    std::ptr::null()
}

/// `uint32_t SecTaskGetCodeSignStatus(SecTaskRef task)`
///
/// The kernel's `CS_*` status flags for the task. Zero - no `CS_VALID`, no
/// `CS_HARD`, no `CS_RUNTIME` - is the status of code that is not signed, which
/// is all code on this host.
///
/// # Safety
///
/// `task` is a task this module made.
pub unsafe extern "C" fn SecTaskGetCodeSignStatus(task: CFTypeRef) -> u32 {
    // SAFETY: the caller's contract.
    debug_assert!(unsafe { is_task(task) }, "not a SecTaskRef");
    0
}

/// `CFTypeID SecTaskGetTypeID(void)`
pub extern "C" fn SecTaskGetTypeID() -> corefoundation::base::CFTypeID {
    corefoundation::base::type_id(class())
}

exports::exports! {
    functions {
        "SecTaskCreateFromSelf" => SecTaskCreateFromSelf,
        "SecTaskCopyValueForEntitlement" => SecTaskCopyValueForEntitlement,
        "SecTaskCopySigningIdentifier" => SecTaskCopySigningIdentifier,
        "SecTaskGetCodeSignStatus" => SecTaskGetCodeSignStatus,
        "SecTaskGetTypeID" => SecTaskGetTypeID,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sequence PartitionAlloc's `UseMapJit()` runs, and the answer it acts
    /// on: a real task object, no value and no error for `allow-jit`.
    #[test]
    fn an_unsigned_process_has_a_task_and_no_entitlements() {
        objc::bootstrap();
        let task = SecTaskCreateFromSelf(std::ptr::null());
        assert!(!task.is_null());
        // SAFETY: an object just made.
        assert!(unsafe { is_task(task) });
        let name = corefoundation::string::from_bytes(b"com.apple.security.cs.allow-jit", false);
        let mut error: CFTypeRef = std::ptr::null();
        // SAFETY: the task above, a string, and a writable error slot.
        unsafe {
            assert!(SecTaskCopyValueForEntitlement(task, name, &mut error).is_null());
            assert!(error.is_null(), "an absent entitlement is not an error");
            assert!(SecTaskCopySigningIdentifier(task, &mut error).is_null());
            assert!(error.is_null());
            assert_eq!(SecTaskGetCodeSignStatus(task), 0);
            assert_eq!(corefoundation::api::CFGetTypeID(task), SecTaskGetTypeID());
            corefoundation::api::CFRelease(name);
            corefoundation::api::CFRelease(task);
        }
    }
}
