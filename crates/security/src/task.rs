//! `SecTask`: what the running program is signed with.
//!
//! A Mac answers from the signature the kernel validated when it mapped the
//! executable. Here nothing validates it, so the identifier and entitlements are
//! read from the signature the guest's executable carries, as
//! `SecStaticCodeCheckValidityWithErrors` reads them, and the status word is the
//! one `csops` reports.
#![allow(non_snake_case)]

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use objc::class::Class;
use objc::object::Id;

/// A `SecTaskRef`. Both fields are owned and may be null: an unsigned program
/// has neither.
#[repr(C)]
struct Body {
    isa: *const Class,
    entitlements: CFTypeRef,
    identifier: CFTypeRef,
}

fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        let class = corefoundation::base::register(c"__R3SecTask", None, size_of::<Body>(), true);
        class.replace_method(
            objc::sel::get("dealloc"),
            dealloc as *const c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("a task class") }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: objc::sel::Sel) {
    // SAFETY: this class's object layout and the fields it owns.
    unsafe {
        let body = receiver as *mut Body;
        objc::arc::release((*body).entitlements as Id);
        objc::arc::release((*body).identifier as Id);
        objc::arc::finalize(receiver);
    }
}

/// The task's fields, or `None` for anything that is not a task.
///
/// # Safety
///
/// `task` is null or an object this runtime allocated.
unsafe fn body(task: CFTypeRef) -> Option<&'static Body> {
    // SAFETY: as the caller states.
    if task.is_null() || !unsafe { corefoundation::base::is_kind_of(task, class()) } {
        return None;
    }
    // SAFETY: an instance of this class, alive for as long as the caller holds it.
    Some(unsafe { &*(task as *const Body) })
}

/// A task for the program in `bytes`, owned by the caller.
fn from_executable(bytes: &[u8]) -> CFTypeRef {
    new(
        super::codesign::entitlements(bytes),
        super::codesign::signing_identifier(bytes),
    )
}

/// A task signed as `identifier` with `entitlements`, owned by the caller.
fn new(
    entitlements: Option<corefoundation::plist::Value>,
    identifier: Option<String>,
) -> CFTypeRef {
    let entitlements = entitlements
        .and_then(|value| corefoundation::propertylist::object_from_value(&value))
        .unwrap_or(std::ptr::null());
    let identifier = identifier.map_or(std::ptr::null(), |name| {
        corefoundation::string::from_bytes(name.as_bytes(), false)
    });
    // SAFETY: `Body` is the registered layout.
    let body = unsafe { corefoundation::base::allocate::<Body>(class()) };
    if body.is_null() {
        objc::arc::release(entitlements as Id);
        objc::arc::release(identifier as Id);
        return std::ptr::null();
    }
    // SAFETY: a freshly allocated body.
    unsafe {
        (*body).entitlements = entitlements;
        (*body).identifier = identifier;
    }
    body as CFTypeRef
}

/// Clear an optional `CFErrorRef *`: every failure below is an absent answer,
/// which Apple reports without an error.
///
/// # Safety
///
/// `error` is null or writable.
unsafe fn clear(error: *mut CFTypeRef) {
    if !error.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *error = std::ptr::null() };
    }
}

/// `SecTaskRef SecTaskCreateFromSelf(CFAllocatorRef allocator)`
///
/// Null only when the guest's executable cannot be read, as Apple returns null
/// when it cannot describe the task.
pub extern "C" fn SecTaskCreateFromSelf(_allocator: CFTypeRef) -> CFTypeRef {
    let Some(me) = darwin::process::me() else {
        return std::ptr::null();
    };
    match std::fs::read(&me.path) {
        Ok(bytes) => from_executable(&bytes),
        Err(_) => std::ptr::null(),
    }
}

/// `CFTypeRef SecTaskCopyValueForEntitlement(SecTaskRef task,
/// CFStringRef entitlement, CFErrorRef *error)`
///
/// # Safety
///
/// `task` is null or an object this runtime allocated; `entitlement` is null or
/// a CF object; `error` is null or writable.
pub unsafe extern "C" fn SecTaskCopyValueForEntitlement(
    task: CFTypeRef,
    entitlement: CFTypeRef,
    error: *mut CFTypeRef,
) -> CFTypeRef {
    // SAFETY: as the caller states.
    unsafe { clear(error) };
    // SAFETY: as the caller states.
    let Some(body) = (unsafe { body(task) }) else {
        return std::ptr::null();
    };
    if body.entitlements.is_null() || entitlement.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a dictionary this task owns, and a key the caller supplied.
    let value =
        unsafe { corefoundation::api::CFDictionaryGetValue(body.entitlements, entitlement) };
    objc::arc::retain(value as Id) as CFTypeRef
}

/// `CFStringRef SecTaskCopySigningIdentifier(SecTaskRef task, CFErrorRef *error)`
///
/// # Safety
///
/// As [`SecTaskCopyValueForEntitlement`].
pub unsafe extern "C" fn SecTaskCopySigningIdentifier(
    task: CFTypeRef,
    error: *mut CFTypeRef,
) -> CFTypeRef {
    // SAFETY: as the caller states.
    unsafe { clear(error) };
    // SAFETY: as the caller states.
    let Some(body) = (unsafe { body(task) }) else {
        return std::ptr::null();
    };
    objc::arc::retain(body.identifier as Id) as CFTypeRef
}

/// `uint32_t SecTaskGetCodeSignStatus(SecTaskRef task)`
///
/// The `CS_OPS_STATUS` word, from `csops` so the two cannot disagree.
///
/// # Safety
///
/// `task` is null or an object this runtime allocated.
pub unsafe extern "C" fn SecTaskGetCodeSignStatus(task: CFTypeRef) -> u32 {
    // SAFETY: as the caller states.
    if unsafe { body(task) }.is_none() {
        return 0;
    }
    let mut status = 0_u32;
    // SAFETY: a four-byte out-parameter, which is all `CS_OPS_STATUS` writes.
    let result = unsafe {
        darwin::bsd::posix::csops(
            0,
            darwin::bsd::posix::CS_OPS_STATUS,
            (&raw mut status).cast(),
            size_of::<u32>(),
        )
    };
    if result == 0 { status } else { 0 }
}

exports::exports! {
    functions {
        "SecTaskCreateFromSelf" => SecTaskCreateFromSelf,
        "SecTaskCopyValueForEntitlement" => SecTaskCopyValueForEntitlement,
        "SecTaskCopySigningIdentifier" => SecTaskCopySigningIdentifier,
        "SecTaskGetCodeSignStatus" => SecTaskGetCodeSignStatus,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> CFTypeRef {
        corefoundation::string::from_bytes(value.as_bytes(), false)
    }

    #[test]
    fn answers_entitlements_and_identifier_it_was_signed_with() {
        objc::bootstrap();
        let entitlements = corefoundation::plist::parse(
            b"<plist><dict><key>com.apple.security.device.camera</key><true/></dict></plist>",
        )
        .expect("a plist");
        let task = new(Some(entitlements), Some("io.example.app".into()));
        let held = text("com.apple.security.device.camera");
        let absent = text("com.apple.security.app-sandbox");
        let mut error = 1 as CFTypeRef;
        // SAFETY: a task, two strings and an out-parameter this test owns.
        unsafe {
            let value = SecTaskCopyValueForEntitlement(task, held, &mut error);
            assert!(!value.is_null());
            assert!(error.is_null());
            assert!(SecTaskCopyValueForEntitlement(task, absent, &mut error).is_null());
            let identifier = SecTaskCopySigningIdentifier(task, std::ptr::null_mut());
            let identifier = corefoundation::string::as_string(identifier).expect("a string");
            assert_eq!(identifier.utf8(), b"io.example.app");
            for object in [value, task, held, absent] {
                corefoundation::api::CFRelease(object);
            }
        }
    }

    #[test]
    fn an_unsigned_task_has_no_entitlements_or_identifier() {
        objc::bootstrap();
        let task = new(None, None);
        let key = text("com.apple.security.app-sandbox");
        // SAFETY: a task and a string this test owns.
        unsafe {
            assert!(!task.is_null());
            assert!(SecTaskCopyValueForEntitlement(task, key, std::ptr::null_mut()).is_null());
            assert!(SecTaskCopySigningIdentifier(task, std::ptr::null_mut()).is_null());
            assert_eq!(SecTaskGetCodeSignStatus(task), 0);
            corefoundation::api::CFRelease(task);
            corefoundation::api::CFRelease(key);
        }
    }

    #[test]
    fn refuses_an_object_that_is_not_a_task() {
        objc::bootstrap();
        let other = text("not a task");
        // SAFETY: a CF object this test owns.
        unsafe {
            assert!(SecTaskCopySigningIdentifier(other, std::ptr::null_mut()).is_null());
            assert!(SecTaskCopyValueForEntitlement(other, other, std::ptr::null_mut()).is_null());
            corefoundation::api::CFRelease(other);
        }
    }
}
