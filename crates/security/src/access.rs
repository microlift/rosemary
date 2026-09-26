#![allow(non_snake_case)]

//! `SecAccessControlRef` - implemented - and the legacy `SecAccess`/`SecACL`
//! writers - refused.
//!
//! # Why the access *control* is a real object
//!
//! `SecAccessControlCreateWithFlags` is the one function here that would abort
//! Chromium if it answered null. `crypto/unexportable_key_mac.mm:251`:
//!
//! ```c++
//! base::apple::ScopedCFTypeRef<SecAccessControlRef> access(
//!     SecAccessControlCreateWithFlags(kCFAllocatorDefault,
//!                                     kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
//!                                     control_flags, /*error=*/nil));
//! CHECK(access);
//! ```
//!
//! So a null answer is a `CHECK` failure and a process abort, and a fabricated
//! object would be a lie about the OS's key-protection policy. Neither is
//! avoidable, so the third thing is taken: the object is *real*, and it records
//! exactly the two things it was asked for - the accessibility class and the
//! creation flags. That is what a `SecAccessControl` is: a description.
//!
//! What it does **not** do is enforce anything, and that is the honest limit. On
//! macOS the flags are enforced by the Secure Enclave and the keychain at the
//! moment a key is used; this host has neither, so `kSecAccessControlUserPresence`
//! here means "the caller asked for a user to be present", and nothing more. The
//! chain ends honestly at [`crate::key::SecKeyCreateRandomKey`], which refuses,
//! so no key is ever created under a policy that could not be enforced.
//!
//! # Why the legacy ACL path is refused outright
//!
//! `SecAccessCreate`, `SecAccessCopyACLList`, `SecACLCopyContents` and
//! `SecACLSetContents` are the pre-10.7 keychain ACL API. Every one of them reads
//! or writes the ACL of an item *in a keychain file*, and there is no keychain.
//! `SecACLSetContents` in particular writes. The single caller is
//! `chrome/browser/extensions/api/image_writer_private/
//! removable_storage_provider_mac.cc`, which builds an ACL for a new keychain item
//! so that a WebUSB device can be written to; without a keychain the whole
//! operation is unavailable, and the extension API reports that rather than
//! pretending.

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use objc::class::Class;
use objc::object::Id;

use crate::status::{self, OSStatus};

/// A `SecAccessControlRef`: the accessibility class and the flags asked for.
#[repr(C)]
struct AccessControlBody {
    isa: *const Class,
    protection: CFTypeRef,
    flags: u32,
}

fn access_control_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        let class = corefoundation::base::register(
            c"__R3SecAccessControl",
            None,
            size_of::<AccessControlBody>(),
            true,
        );
        class.replace_method(
            objc::sel::get("dealloc"),
            dealloc as *const c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("an access control class") }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: objc::sel::Sel) {
    // SAFETY: this class's object layout and its owned accessibility string.
    unsafe {
        let body = receiver as *mut AccessControlBody;
        corefoundation::api::CFRelease((*body).protection);
        objc::arc::finalize(receiver);
    }
}

/// Whether `object` is an access control this module made.
///
/// Only the test needs it: the guest checks a `SecAccessControlRef` with
/// `CFGetTypeID`, and does not import `SecAccessControlGetTypeID`, so no exported
/// function in this module is ever handed one.
///
/// # Safety
///
/// `object` is null or an object this runtime allocated.
#[cfg(test)]
unsafe fn is_access_control(object: CFTypeRef) -> bool {
    // SAFETY: the caller's contract.
    unsafe { corefoundation::base::is_kind_of(object, access_control_class()) }
}

/// `SecAccessControlRef SecAccessControlCreateWithFlags(CFAllocatorRef
/// allocator, CFTypeRef protection, SecAccessControlCreateFlags flags, CFErrorRef
/// *error)`
///
/// A real object holding `protection` and `flags`. The module note says why, and
/// what it does not do.
///
/// `errSecParam` becomes a null handle, which is what `SecAccessControl.h`
/// documents: an accessibility value that is not one of the `kSecAttrAccessible*`
/// strings is not a protection class, and refusing it is a check rather than a
/// formality - the guest passes
/// `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`, and an unknown string
/// stored as though it were a policy would be a policy with no meaning.
///
/// # Safety
///
/// `protection` is null or a `CFString`; `error` is null or writable.
pub unsafe extern "C" fn SecAccessControlCreateWithFlags(
    _allocator: CFTypeRef,
    protection: CFTypeRef,
    flags: u32,
    error: *mut CFTypeRef,
) -> CFTypeRef {
    if !error.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { error.write(std::ptr::null_mut()) };
    }
    // The `kSecAttrAccessible*` protection classes, which is what
    // `SecAccessControl.h` says `protection` must be ("One of kSecAttrAccessible
    // constants"). The seven are the complete set `SecItem.h` declares, and the
    // strings are compared by content rather than by identity because the guest
    // passes the imported global `_kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`
    // (`Framework+0xdd50750`, read at `+0x5f50ddb`), whose value is whatever that
    // data import binds to.
    const ACCESSIBLE: [&[u8]; 7] = [
        b"kSecAttrAccessibleWhenUnlocked",
        b"kSecAttrAccessibleAfterFirstUnlock",
        b"kSecAttrAccessibleAlways",
        b"kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly",
        b"kSecAttrAccessibleWhenUnlockedThisDeviceOnly",
        b"kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly",
        b"kSecAttrAccessibleAlwaysThisDeviceOnly",
    ];
    // SAFETY: the caller's contract; the accessor answers `None` for anything
    // that is not a string.
    let Some(text) = (unsafe { corefoundation::string::as_string(protection) }) else {
        status::refuse(
            "SecAccessControlCreateWithFlags",
            "the protection class is not a string this runtime can read, so it cannot be \
             checked against the kSecAttrAccessible classes and no access control is made. \
             Note the guest passes the *imported global* kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly, \
             so this is also what an unbound data import looks like from here - and the guest's \
             `CHECK(access)` at crypto/unexportable_key_mac.mm:251 will abort on the null.",
        );
        return std::ptr::null();
    };
    let name = text.utf8();
    if !ACCESSIBLE.contains(&name.as_slice()) {
        status::refuse(
            "SecAccessControlCreateWithFlags",
            &format!(
                "the protection class {name:?} is not one of the seven kSecAttrAccessible \
                 classes SecItem.h declares, so it is refused rather than stored as though it \
                 were a policy. The guest's `CHECK(access)` at crypto/unexportable_key_mac.mm:251 \
                 will abort on the null, which names the file and line."
            ),
        );
        return std::ptr::null();
    }
    // SAFETY: `AccessControlBody` is the registered layout.
    let body = unsafe { corefoundation::base::allocate::<AccessControlBody>(access_control_class()) };
    if body.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a freshly allocated body.
    unsafe {
        (*body).protection = corefoundation::api::CFRetain(protection);
        (*body).flags = flags;
        body as CFTypeRef
    }
}

/// The accessibility and flags a `SecAccessControlRef` records, for the test.
#[cfg(test)]
fn recorded(object: CFTypeRef) -> (CFTypeRef, u32) {
    // SAFETY: the caller states it is an access control this module made.
    unsafe {
        let body = &*(object as *const AccessControlBody);
        (body.protection, body.flags)
    }
}

/// `OSStatus SecAccessCreate(CFStringRef descriptor, CFArrayRef trustedlist,
/// SecAccessRef *accessRef)`
///
/// Refused by name. An ACL is a property of a keychain item, and making one means
/// making a keychain.
///
/// # Safety
///
/// `descriptor` is null or a `CFString`; `trustedlist` is null or an array;
/// `access_ref` is null or writable.
pub unsafe extern "C" fn SecAccessCreate(
    _descriptor: CFTypeRef,
    _trustedlist: CFTypeRef,
    access_ref: *mut CFTypeRef,
) -> OSStatus {
    if !access_ref.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { access_ref.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "SecAccessCreate",
        "a SecAccess is the access-control list of a keychain item, and this host has no \
         keychain. The one caller builds one for a new item so that a WebUSB device can be \
         written to; without a store there is nothing for the list to belong to.",
    );
    status::NO_SUCH_KEYCHAIN
}

/// `OSStatus SecAccessCopyACLList(SecAccessRef accessRef, CFArrayRef *aclList)`
///
/// Refused by name, and the list left null. Nothing here holds a `SecAccess`, so
/// the argument cannot be one this module made.
///
/// # Safety
///
/// `access_ref` is null or a `SecAccessRef`; `acl_list` is null or writable.
pub unsafe extern "C" fn SecAccessCopyACLList(
    _access_ref: CFTypeRef,
    acl_list: *mut CFTypeRef,
) -> OSStatus {
    if !acl_list.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { acl_list.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "SecAccessCopyACLList",
        "there is no SecAccess on this host, because there is no keychain for one to be an \
         ACL of.",
    );
    status::CS_INVALID_OBJECT_REF
}

/// `OSStatus SecACLCopyContents(SecACLRef acl, CFArrayRef *applicationList,
/// CFStringRef *description, SecKeychainPromptSelector *promptSelector)`
///
/// Refused by name. Every out-parameter is nulled or zeroed first, so a caller
/// that ignores the status does not read a stale pointer - the guest immediately
/// does `CFArrayGetCount(acl_list.get())` on what comes back
/// (`removable_storage_provider_mac.cc`).
///
/// # Safety
///
/// `acl` is null or a `SecACLRef`; the other three are null or writable.
pub unsafe extern "C" fn SecACLCopyContents(
    _acl: CFTypeRef,
    application_list: *mut CFTypeRef,
    description: *mut CFTypeRef,
    prompt_selector: *mut u32,
) -> OSStatus {
    if !application_list.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { application_list.write(std::ptr::null_mut()) };
    }
    if !description.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { description.write(std::ptr::null_mut()) };
    }
    if !prompt_selector.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { prompt_selector.write(0) };
    }
    status::refuse(
        "SecACLCopyContents",
        "reading an ACL means reading a keychain item's access-control list, and this host \
         has no keychain items.",
    );
    status::NO_SUCH_KEYCHAIN
}

/// `OSStatus SecACLSetContents(SecACLRef acl, CFArrayRef applicationList,
/// CFStringRef description, SecKeychainPromptSelector promptSelector)`
///
/// Refused by name. This one *writes* an ACL, which is the clearest case in the
/// whole set of a function whose only job is to modify a file this module does not
/// have.
///
/// # Safety
///
/// `acl` is null or a `SecACLRef`; `application_list` and `description` are null
/// or objects.
pub unsafe extern "C" fn SecACLSetContents(
    _acl: CFTypeRef,
    _application_list: CFTypeRef,
    _description: CFTypeRef,
    _prompt_selector: u32,
) -> OSStatus {
    status::refuse(
        "SecACLSetContents",
        "writing an ACL means writing a keychain file, and this module will not invent one: \
         a keychain of its own making would be a file whose format nothing else can read, \
         holding access rules a user never agreed to.",
    );
    status::NO_SUCH_KEYCHAIN
}

exports::exports! {
    functions {
        "SecAccessControlCreateWithFlags" => SecAccessControlCreateWithFlags,
        "SecAccessCreate" => SecAccessCreate,
        "SecAccessCopyACLList" => SecAccessCopyACLList,
        "SecACLCopyContents" => SecACLCopyContents,
        "SecACLSetContents" => SecACLSetContents,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `CHECK(access)` at `unexportable_key_mac.mm:251` must not fire: a real
    /// object comes back, and it holds exactly what was asked for.
    #[test]
    fn an_access_control_is_the_description_it_was_given() {
        objc::bootstrap();
        let protection =
            corefoundation::string::from_bytes(b"kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly", false);
        // SAFETY: a `CFString`, and a null error out-parameter.
        let access = unsafe {
            SecAccessControlCreateWithFlags(
                std::ptr::null(),
                protection,
                // kSecAccessControlPrivateKeyUsage
                0x0000_0002,
                std::ptr::null_mut(),
            )
        };
        assert!(!access.is_null(), "a null here is a CHECK failure in the guest");
        // SAFETY: an access control this test made.
        unsafe {
            assert!(is_access_control(access));
            let (recorded_protection, flags) = recorded(access);
            assert_eq!(flags, 0x0000_0002);
            assert_eq!(
                foundation::string::bytes(recorded_protection as objc::object::Id),
                b"kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly"
            );
            corefoundation::api::CFRelease(access);
        }
        // SAFETY: a `CFString` this test made.
        unsafe { corefoundation::api::CFRelease(protection) };
    }

    /// A protection value that is not an accessibility class is null, and `*error`
    /// is left null - which is what `SecAccessControl.h` documents.
    #[test]
    fn a_protection_value_that_is_not_a_class_is_refused() {
        objc::bootstrap();
        let not_a_class = corefoundation::string::from_bytes(b"kSecAttrSomethingElse", false);
        let mut error: CFTypeRef = 1_usize as CFTypeRef;
        // SAFETY: a `CFString` that is not a protection class, and a writable
        // error slot.
        unsafe {
            assert!(
                SecAccessControlCreateWithFlags(
                    std::ptr::null(),
                    not_a_class,
                    0,
                    &mut error
                )
                .is_null()
            );
            assert!(error.is_null());
            // SAFETY: null protection is not a class either.
            assert!(
                SecAccessControlCreateWithFlags(
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut()
                )
                .is_null()
            );
            corefoundation::api::CFRelease(not_a_class);
        }
    }

    /// The legacy ACL path refuses, and hands back nothing a caller could
    /// dereference.
    #[test]
    fn the_legacy_acl_path_refuses_and_returns_nothing() {
        objc::bootstrap();
        let mut list: CFTypeRef = 1_usize as CFTypeRef;
        let mut description: CFTypeRef = 1_usize as CFTypeRef;
        let mut selector: u32 = 9;
        // SAFETY: null arguments, which every one of these documents as
        // acceptable, and writable out-parameters.
        unsafe {
            assert_eq!(
                SecAccessCreate(std::ptr::null(), std::ptr::null(), &mut list),
                status::NO_SUCH_KEYCHAIN
            );
            assert!(list.is_null());
            assert_eq!(
                SecAccessCopyACLList(std::ptr::null(), &mut list),
                status::CS_INVALID_OBJECT_REF
            );
            assert!(list.is_null());
            assert_eq!(
                SecACLCopyContents(std::ptr::null(), &mut list, &mut description, &mut selector),
                status::NO_SUCH_KEYCHAIN
            );
            assert!(list.is_null());
            assert!(description.is_null());
            assert_eq!(selector, 0);
            assert_eq!(
                SecACLSetContents(
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0
                ),
                status::NO_SUCH_KEYCHAIN
            );
        }
    }
}
