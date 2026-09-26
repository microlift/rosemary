#![allow(non_snake_case)]

//! The deprecated `SecKeychain*` API: one real function, and the absence of a
//! keychain.
//!
//! # `SecKeychainAddCallback` is on Chromium's start-up path
//!
//! This is the only one of the 53 that is. `chrome/browser/chrome_browser_main.cc`
//! calls it from `ChromeBrowserMainParts::PreCreateThreadsImpl`, which runs in the
//! browser process before the message loop. The guest's own disassembly, at
//! `Framework+0x56bf12b`:
//!
//! ```text
//! 56bf015  lea  rdi,[rip+...]        # 0xd8f0eb2  'sysctl.proc_translated'
//! 56bf02f  call _sysctlbyname
//! 56bf034  cmp  eax,0xffffffff      ; no such sysctl: not translated
//! 56bf037  je   0x56bf120            ; -> straight to the keychain call
//! ...
//! 56bf120  lea  rdi,[rip+...]        # 0x3f5660    &KeychainCallback
//! 56bf127  xor  esi,esi             ; event mask = 0
//! 56bf129  xor  edx,edx             ; context    = nullptr
//! 56bf12b  call _SecKeychainAddCallback
//! ```
//!
//! and the source at that line says exactly what the arguments mean
//! (`chrome_browser_main.cc:1149-1161`):
//!
//! ```c++
//! // Get the Keychain API to register for distributed notifications on the main
//! // thread ... (Note that the callback mask here is empty. I don't want to
//! // register for any callbacks, I just want to initialize the mechanism.)
//! SecKeychainAddCallback(&KeychainCallback, 0, nullptr);
//! ```
//!
//! with the callback itself being `OSStatus KeychainCallback(...) { return noErr; }`
//! (line 518) - it does nothing.
//!
//! So the truthful answer is `errSecSuccess` with the registration recorded and
//! nothing ever delivered, and for the empty mask that is not even a compromise:
//! **macOS also stores a callback that no event can match.** The registration is
//! real, and the absence of events is real, and the one caller that passes a
//! non-empty mask - `net::TrustStoreMac::KeychainChangedNotifier`, which wants
//! to know when the user changes trust settings - is told so on stderr, once,
//! by name. Its only other reaction to a non-zero status is an
//! `OSSTATUS_LOG(ERROR, ...)` (`trust_store_mac.cc:502`), so nothing it does
//! changes.
//!
//! # The other ten
//!
//! There is no keychain on this host, and no way to have one: a keychain is a
//! file in a format this module would have to invent, and a wrong keychain is
//! silent data loss. So a *read* answers `errSecItemNotFound` - Apple's own "the
//! item could not be found", and the true answer - and anything that would
//! *write* refuses by name. The one exception is `SecKeychainCopyDefault`, which
//! answers `errSecNoDefaultKeychain`, which is a stronger and equally true
//! statement: not "no such item" but "there is no default keychain to look in".
//!
//! Who the callers are, and why none of them is on the milestone path:
//! `crypto::AppleKeychain` (the shared serialised wrapper at
//! `Framework+0x5f50570..0x5f50970`) is reached from
//! `components/wifi/wifi_service_mac.mm` (Wi-Fi passwords),
//! `chrome/browser/extensions/api/enterprise_reporting_private/
//! chrome_desktop_report_request_helper.cc` (device-trust secrets) and
//! `crypto/unexportable_key_mac.mm` (key wrapping). None of those runs for
//! `--headless --dump-dom about:blank`.

use corefoundation::base::CFTypeRef;
use objc::class::Class;

use crate::status::{self, OSStatus};

/// One registered callback, kept so that "it was registered" is a fact about
/// this process rather than a claim - and so that registering the same function
/// pointer twice is recognisable, which is how this API is keyed.
struct Registration {
    callback: usize,
    mask: u32,
    context: usize,
}

static REGISTRATIONS: std::sync::Mutex<Vec<Registration>> = std::sync::Mutex::new(Vec::new());

/// A keychain. There are none, so this class has no instances; it exists so the
/// refusal below has a type to name and so a handle a guest makes up is
/// recognisable as one of ours.
#[repr(C)]
struct Body {
    isa: *const Class,
}

fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        objc::class::ptr(corefoundation::base::register(
            c"__R3SecKeychain",
            None,
            size_of::<Body>(),
            true,
        )) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("a keychain class") }
}

/// `OSStatus SecKeychainAddCallback(SecKeychainCallback callbackFunction,
/// SecKeychainEventMask eventMask, void *userContext)`
///
/// `errSecSuccess`, the registration recorded, and no event ever delivered -
/// because there is no keychain for an event to come from. See the module note
/// for why that is the true answer rather than a convenient one, and for the
/// guest's own reason for making the call with an empty mask.
///
/// Two refusals, both of which need no keychain to be right:
/// `errSecParam` for a null callback, which is what `SecKeychain.h` documents,
/// and `errSecDuplicateCallback` for a function pointer already registered -
/// because the registration is keyed on the pointer alone, which is the guest's
/// own stated reason for giving each `KeychainChangedNotifier` instantiation its
/// own `KeychainCallback` address (`trust_store_mac.cc:512`).
pub extern "C" fn SecKeychainAddCallback(
    callback: usize,
    event_mask: u32,
    user_context: usize,
) -> OSStatus {
    if callback == 0 {
        return status::PARAM;
    }
    let mut registrations = REGISTRATIONS.lock().unwrap_or_else(|e| e.into_inner());
    if registrations
        .iter()
        .any(|held| held.callback == callback)
    {
        return status::DUPLICATE_CALLBACK;
    }
    registrations.push(Registration {
        callback,
        mask: event_mask,
        context: user_context,
    });
    if event_mask != 0 {
        // The record is read back rather than the arguments, so the sentence
        // describes the registration that now exists - which is what a reader
        // debugging a trust store that will not refresh needs to see.
        let held = registrations.last().expect("just pushed");
        status::refuse(
            "SecKeychainAddCallback",
            &format!(
                "the callback at {:#x} is registered for mask {:#x} (context {:#x}) and will \
                 never be called: there is no keychain on this host, so there is no keychain \
                 event to deliver it. A trust store or an observer waiting to be told that \
                 keychain trust settings changed will not be told.",
                held.callback, held.mask, held.context
            ),
        );
    }
    status::SUCCESS
}

/// `OSStatus SecKeychainCopyDefault(SecKeychainRef *keychain)`
///
/// `errSecNoDefaultKeychain`, which is `SecBase.h`'s own words for "A default
/// keychain could not be found" - and there is none, because there are no
/// keychains. The caller is
/// `chrome_desktop_report_request_helper.cc`, which reports the status up and
/// stops; it does not retry or assert.
///
/// # Safety
///
/// `keychain` is null or writable.
pub unsafe extern "C" fn SecKeychainCopyDefault(keychain: *mut CFTypeRef) -> OSStatus {
    if !keychain.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { keychain.write(std::ptr::null_mut()) };
    }
    status::NO_DEFAULT_KEYCHAIN
}

/// `OSStatus SecKeychainGetStatus(SecKeychainRef keychain, SecKeychainStatus
/// *keychainStatus)`
///
/// `errSecParam`: a keychain that is not there cannot have a status, and
/// `SecKeychain.h` names `errSecParam` for an invalid parameter. The out-parameter
/// is zeroed rather than left alone, because `kSecUnlockItem` is 1 - an untouched
/// slot would read as a status this call never set.
///
/// The caller only wants to know whether a keychain is *unlocked*, and reports
/// any error rather than acting on it
/// (`chrome_desktop_report_request_helper.cc:205`), so the truthful refusal costs
/// it one log line.
///
/// # Safety
///
/// `keychain` is null or a keychain; `keychain_status` is null or writable.
pub unsafe extern "C" fn SecKeychainGetStatus(
    keychain: CFTypeRef,
    keychain_status: *mut i32,
) -> OSStatus {
    if !keychain_status.is_null() {
        // SAFETY: the caller states it is writable. Zeroed rather than left
        // alone: `kSecUnlockItem` is 1, so an untouched slot could read as a
        // status this call never set.
        unsafe { keychain_status.write(0) };
    }
    if keychain.is_null() {
        return status::PARAM;
    }
    // A keychain this module made would be an unlocked one, truthfully, because
    // nothing is ever locked. There are none, so this is unreachable in practice;
    // it is here so the function is not a lie for the handle it would accept.
    // SAFETY: the caller's contract.
    if unsafe { corefoundation::base::is_kind_of(keychain, class()) } {
        return status::SUCCESS;
    }
    status::PARAM
}

/// `OSStatus SecKeychainSetUserInteractionAllowed(Boolean state)`
///
/// Refused. A process on this host has no user to interact with, so "allow
/// interaction" cannot be turned on; answering `errSecSuccess` would be a claim
/// about a prompt that can never appear.
///
/// The guest's use is a scope guard -
/// `crypto::ScopedKeychainUserInteractionAllowed user_interaction_allowed(FALSE,
/// &status); if (status != noErr) return status;` at
/// `chrome_desktop_report_request_helper.cc:164` - so a failure here is
/// "no keychain access", which is the truth.
pub extern "C" fn SecKeychainSetUserInteractionAllowed(_state: bool) -> OSStatus {
    status::refuse(
        "SecKeychainSetUserInteractionAllowed",
        "this process has no user to interact with, so there is no prompt to allow or \
         disallow. Answering success would claim a dialog that can never appear.",
    );
    status::INTERACTION_NOT_ALLOWED
}

/// `OSStatus SecKeychainGetUserInteractionAllowed(Boolean *state)`
///
/// Refused, with `state` set to `false` first. `false` is the truthful reading -
/// interaction is not allowed, because there is none - and the non-zero status
/// says that this is a fact about the absence of a user rather than a stored
/// preference.
///
/// # Safety
///
/// `state` is null or writable.
pub unsafe extern "C" fn SecKeychainGetUserInteractionAllowed(state: *mut bool) -> OSStatus {
    if !state.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { state.write(false) };
    }
    status::refuse(
        "SecKeychainGetUserInteractionAllowed",
        "there is no user to interact with on this host, so interaction is not allowed. \
         `false` is written because that is the answer; the status says it is not a stored \
         preference.",
    );
    status::INTERACTION_NOT_ALLOWED
}

/// `OSStatus SecKeychainFindGenericPassword(CFTypeRef keychainOrArray, UInt32
/// serviceNameLength, const char *serviceName, UInt32 accountNameLength, const
/// char *accountName, UInt32 *passwordLength, void **passwordData,
/// SecKeychainItemRef *itemRef)`
///
/// `errSecItemNotFound`, which is `SecBase.h`'s "The specified item could not be
/// found in the keychain" and is true: there is no keychain, so no item.
///
/// # Safety
///
/// `service_name` and `account_name` are null or C strings; `password_length`,
/// `password_data` and `item_ref` are null or writable.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn SecKeychainFindGenericPassword(
    _keychain_or_array: CFTypeRef,
    _service_name_length: u32,
    _service_name: *const u8,
    _account_name_length: u32,
    _account_name: *const u8,
    password_length: *mut u32,
    password_data: *mut *mut core::ffi::c_void,
    item_ref: *mut CFTypeRef,
) -> OSStatus {
    // Every out-parameter is zeroed rather than left alone: a caller that
    // ignores the status would otherwise read whatever was on its stack, and
    // `passwordData` is immediately `new std::string(static_cast<char*>(...),
    // length)` in the guest.
    if !password_length.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { password_length.write(0) };
    }
    if !password_data.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { password_data.write(std::ptr::null_mut()) };
    }
    if !item_ref.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { item_ref.write(std::ptr::null_mut()) };
    }
    status::ITEM_NOT_FOUND
}

/// `OSStatus SecKeychainAddGenericPassword(SecKeychainRef keychain, UInt32
/// serviceNameLength, const char *serviceName, UInt32 accountNameLength, const
/// char *accountName, UInt32 passwordLength, const void *passwordData,
/// SecKeychainItemRef *itemRef)`
///
/// Refused by name rather than answered `errSecDuplicateItem` or
/// `errSecSuccess`: a store invented here would hold a credential the user never
/// entered, and a later read of it would be a wrong answer that looked like a
/// right one.
///
/// # Safety
///
/// `keychain` is null or a keychain; `service_name`, `account_name` and
/// `password_data` are null or readable for the stated lengths; `item_ref` is
/// null or writable.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn SecKeychainAddGenericPassword(
    _keychain: CFTypeRef,
    _service_name_length: u32,
    _service_name: *const u8,
    _account_name_length: u32,
    _account_name: *const u8,
    _password_length: u32,
    _password_data: *const core::ffi::c_void,
    item_ref: *mut CFTypeRef,
) -> OSStatus {
    if !item_ref.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { item_ref.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "SecKeychainAddGenericPassword",
        "storing a password needs a keychain file, and this module will not invent one: a \
         store of its own making would hand the guest a credential the user never entered, \
         and every later read of it would be confidently wrong.",
    );
    status::NO_SUCH_KEYCHAIN
}

/// `OSStatus SecKeychainItemDelete(SecKeychainItemRef itemRef)`
///
/// `errSecItemNotFound`: there is no item to delete, and `SecBase.h` says so in
/// those words.
///
/// # Safety
///
/// `item_ref` is null or a keychain item.
pub unsafe extern "C" fn SecKeychainItemDelete(_item_ref: CFTypeRef) -> OSStatus {
    status::ITEM_NOT_FOUND
}

/// `OSStatus SecKeychainItemFreeContent(SecKeychainAttributeList *attrList, void
/// *data)`
///
/// `errSecSuccess`, and this one really is a success rather than a refusal: the
/// function frees a buffer that [`SecKeychainFindGenericPassword`] allocated, and
/// since that answer was `errSecItemNotFound` the buffer is null and there is
/// nothing to free. It is bound because an unbound import is a jump through a
/// zero slot, which is how runs are lost - and because this is the shape of a
/// correct answer for a guest that got nothing.
///
/// # Safety
///
/// `attr_list` and `data` are null or came from this API.
pub unsafe extern "C" fn SecKeychainItemFreeContent(
    _attr_list: *mut core::ffi::c_void,
    _data: *mut core::ffi::c_void,
) -> OSStatus {
    status::SUCCESS
}

/// `OSStatus SecKeychainItemCopyKeychain(SecKeychainItemRef itemRef,
/// SecKeychainRef *keychainRef)`
///
/// `errSecItemNotFound` - there is no such item, and so no keychain for it to be
/// in - and `errSecParam` for no item at all. The refusal is by name because the
/// alternative is a handle to a keychain object, which would imply a store behind
/// it that does not exist.
///
/// # Safety
///
/// `item_ref` is null or a keychain item; `keychain_ref` is null or writable.
pub unsafe extern "C" fn SecKeychainItemCopyKeychain(
    item_ref: CFTypeRef,
    keychain_ref: *mut CFTypeRef,
) -> OSStatus {
    if !keychain_ref.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { keychain_ref.write(std::ptr::null_mut()) };
    }
    if item_ref.is_null() {
        return status::PARAM;
    }
    status::refuse(
        "SecKeychainItemCopyKeychain",
        "this host has no keychain items, so there is no keychain for one to be in. \
         errSecItemNotFound rather than a handle, because handing back a keychain object \
         would imply a store behind it.",
    );
    status::ITEM_NOT_FOUND
}

/// `OSStatus SecKeychainItemCreateFromContent(SecItemClass itemClass,
/// SecKeychainAttributeList *attrList, UInt32 length, const void *data,
/// SecKeychainRef keychainRef, SecAccessRef initialAccess, SecKeychainItemRef
/// *itemRef)`
///
/// Refused by name, together with the rest of the legacy ACL path in
/// [`crate::access`]. This one writes a new keychain item, which means writing a
/// keychain file.
///
/// # Safety
///
/// `attr_list` is null or a `SecKeychainAttributeList`; `data` is null or readable
/// for `length` bytes; `keychain_ref` and `initial_access` are null or objects;
/// `item_ref` is null or writable.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn SecKeychainItemCreateFromContent(
    _item_class: u32,
    _attr_list: *mut core::ffi::c_void,
    _length: u32,
    _data: *const core::ffi::c_void,
    _keychain_ref: CFTypeRef,
    _initial_access: CFTypeRef,
    item_ref: *mut CFTypeRef,
) -> OSStatus {
    if !item_ref.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { item_ref.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "SecKeychainItemCreateFromContent",
        "creating a keychain item means writing a keychain file, and this module will not \
         invent one. See crates/security/src/access.rs for the rest of the legacy ACL path, \
         which is refused for the same reason.",
    );
    status::NO_SUCH_KEYCHAIN
}

exports::exports! {
    functions {
        "SecKeychainAddCallback" => SecKeychainAddCallback,
        "SecKeychainCopyDefault" => SecKeychainCopyDefault,
        "SecKeychainGetStatus" => SecKeychainGetStatus,
        "SecKeychainSetUserInteractionAllowed" => SecKeychainSetUserInteractionAllowed,
        "SecKeychainGetUserInteractionAllowed" => SecKeychainGetUserInteractionAllowed,
        "SecKeychainFindGenericPassword" => SecKeychainFindGenericPassword,
        "SecKeychainAddGenericPassword" => SecKeychainAddGenericPassword,
        "SecKeychainItemDelete" => SecKeychainItemDelete,
        "SecKeychainItemFreeContent" => SecKeychainItemFreeContent,
        "SecKeychainItemCopyKeychain" => SecKeychainItemCopyKeychain,
        "SecKeychainItemCreateFromContent" => SecKeychainItemCreateFromContent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact call `chrome_browser_main.cc:1161` makes, and the answer it
    /// needs: success, a registration recorded, and nothing to deliver it.
    #[test]
    fn the_empty_mask_registration_succeeds_and_records() {
        objc::bootstrap();
        let before = REGISTRATIONS.lock().unwrap_or_else(|e| e.into_inner()).len();
        // &KeychainCallback, 0, nullptr - the guest's three arguments.
        assert_eq!(SecKeychainAddCallback(0x3f5660, 0, 0), status::SUCCESS);
        let registrations = REGISTRATIONS.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(registrations.len(), before + 1);
        let last = registrations.last().expect("just pushed");
        assert_eq!(last.mask, 0);
        assert_eq!(last.context, 0);
    }

    /// A non-empty mask is still registered - the registration is real - and the
    /// absence of an event source is said out loud, once, naming the symbol.
    #[test]
    fn a_non_empty_mask_is_registered_and_the_absence_is_named() {
        objc::bootstrap();
        let before = REGISTRATIONS.lock().unwrap_or_else(|e| e.into_inner()).len();
        // net::TrustStoreMac::KeychainChangedNotifier, with a real mask.
        assert_eq!(SecKeychainAddCallback(0x6744b4c, 0x0000_0002, 0), status::SUCCESS);
        assert_eq!(
            REGISTRATIONS.lock().unwrap_or_else(|e| e.into_inner()).len(),
            before + 1,
            "the registration is a fact, not a claim"
        );
        // A null callback is `errSecParam`, which is true without a keychain.
        assert_eq!(SecKeychainAddCallback(0, 0, 0), status::PARAM);
    }

    /// The API is keyed on the function pointer alone - the guest's own stated
    /// reason for giving each notifier its own `KeychainCallback` address - so a
    /// second registration of the same pointer is a duplicate.
    #[test]
    fn registering_one_pointer_twice_is_a_duplicate() {
        objc::bootstrap();
        let before = REGISTRATIONS.lock().unwrap_or_else(|e| e.into_inner()).len();
        assert_eq!(SecKeychainAddCallback(0x6a1b2c3d, 0, 0), status::SUCCESS);
        assert_eq!(
            SecKeychainAddCallback(0x6a1b2c3d, 0, 0),
            status::DUPLICATE_CALLBACK
        );
        assert_eq!(
            REGISTRATIONS.lock().unwrap_or_else(|e| e.into_inner()).len(),
            before + 1,
            "a refused duplicate does not register a second time"
        );
    }

    /// There is no default keychain, no item in any of them, and every
    /// out-parameter a caller might read is zeroed first.
    #[test]
    fn there_is_no_keychain_and_no_item_in_it() {
        objc::bootstrap();
        let mut handle: CFTypeRef = 1_usize as CFTypeRef;
        // SAFETY: a writable slot for each.
        unsafe {
            assert_eq!(SecKeychainCopyDefault(&mut handle), status::NO_DEFAULT_KEYCHAIN);
            assert!(handle.is_null());

            let mut length: u32 = 99;
            let mut data = 1_usize as *mut core::ffi::c_void;
            let mut item: CFTypeRef = 1_usize as CFTypeRef;
            assert_eq!(
                SecKeychainFindGenericPassword(
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    &mut length,
                    &mut data,
                    &mut item,
                ),
                status::ITEM_NOT_FOUND
            );
            // A caller that ignored the status must not read a stale length and
            // build a string from it.
            assert_eq!(length, 0);
            assert!(data.is_null());
            assert!(item.is_null());

            assert_eq!(SecKeychainItemDelete(std::ptr::null()), status::ITEM_NOT_FOUND);
            // Freeing content that was never allocated is a success.
            assert_eq!(
                SecKeychainItemFreeContent(std::ptr::null_mut(), std::ptr::null_mut()),
                status::SUCCESS
            );
            let mut keychain: CFTypeRef = 1_usize as CFTypeRef;
            assert_eq!(
                SecKeychainItemCopyKeychain(1_usize as CFTypeRef, &mut keychain),
                status::ITEM_NOT_FOUND
            );
            assert!(keychain.is_null());
            assert_eq!(
                SecKeychainItemCopyKeychain(std::ptr::null(), &mut keychain),
                status::PARAM
            );

            let mut status_out: i32 = 7;
            assert_eq!(SecKeychainGetStatus(std::ptr::null(), &mut status_out), status::PARAM);
            assert_eq!(status_out, 0, "zeroed, not left as a plausible status");

            assert_eq!(
                SecKeychainSetUserInteractionAllowed(false),
                status::INTERACTION_NOT_ALLOWED
            );
            let mut allowed = true;
            assert_eq!(
                SecKeychainGetUserInteractionAllowed(&mut allowed),
                status::INTERACTION_NOT_ALLOWED
            );
            assert!(!allowed, "interaction is not allowed, because there is no user");
        }
    }

    /// Nothing here invents a store: the two write paths are non-success and say
    /// so by name.
    #[test]
    fn the_write_paths_refuse() {
        objc::bootstrap();
        let mut item: CFTypeRef = 1_usize as CFTypeRef;
        // SAFETY: null arguments, which every one of these documents as
        // acceptable, and a writable out-parameter.
        unsafe {
            assert_eq!(
                SecKeychainAddGenericPassword(
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    &mut item,
                ),
                status::NO_SUCH_KEYCHAIN
            );
            assert!(item.is_null());
            assert_eq!(
                SecKeychainItemCreateFromContent(
                    0,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    &mut item,
                ),
                status::NO_SUCH_KEYCHAIN
            );
            assert!(item.is_null());
        }
    }
}
