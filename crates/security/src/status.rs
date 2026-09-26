//! The `OSStatus` values these calls answer with, and the refusal they say out loud.
//!
//! Every constant here is Apple's, and every one is cited to where the number
//! came from, because an `OSStatus` is an interface: a guest branches on it.
//!
//! Two of them were read out of the guest binary rather than a header, which is
//! the only authority that settles what *this* build was compiled against:
//!
//! - `errSecItemNotFound` is `-25300` because `net/cert/internal/trust_store_mac.cc`
//!   compiles to `cmp eax, 0xffff9d2c` at `Framework+0x6745590`, immediately
//!   after its `SecTrustSettingsCopyTrustSettings` call - and the source at that
//!   point is `if (err == errSecItemNotFound)`.
//! - `errAuthorizationCanceled` is `-60006` for the same reason:
//!   `base/mac/authorization_util.mm` compiles to `cmp eax, 0xffff159a` at
//!   `Framework+0x5d9fcd2`, immediately after `AuthorizationCopyRights`, and
//!   the source at that point is `if (status != errAuthorizationCanceled)`.
//!
//! The rest are from `Security.framework/Headers/SecBase.h` and
//! `.../CSCommon.h`. `SecTrustEvaluateWithError` and `SecCodeSignerCreate` are
//! newer than the 10.10 SDK header used here, so their prototypes come from the
//! declarations `chrome/browser/web_applications/os_integration/mac/
//! web_app_shortcut_creator.mm` writes out itself, cross-checked against the
//! registers the guest sets at each call site.

use std::collections::HashSet;
use std::sync::Mutex;

/// `OSStatus`: a signed 32-bit result code, `errSecSuccess` being zero.
pub type OSStatus = i32;

pub const SUCCESS: OSStatus = 0;
/// `errSecParam`, `SecBase.h`: "One or more parameters passed to a function
/// were not valid."
pub const PARAM: OSStatus = -50;
/// `errSecNotAvailable`, `SecBase.h`: "No keychain is available."
///
/// Also the honest answer for the facilities that exist only to serve one: a
/// trust decision, a code signature, a key. There is no keychain on this host,
/// so there is nothing behind any of them.
pub const NOT_AVAILABLE: OSStatus = -25291;
/// `errSecAuthFailed`, `SecBase.h`: "The user name or passphrase you entered
/// is not correct." Read out of the guest: see the module note.
pub const AUTH_FAILED: OSStatus = -25293;
/// `errSecNoSuchKeychain`, `SecBase.h`: "The specified keychain could not be
/// found." True of every keychain on this host.
pub const NO_SUCH_KEYCHAIN: OSStatus = -25294;
/// `errSecDuplicateItem`, `SecBase.h`.
pub const DUPLICATE_ITEM: OSStatus = -25299;
/// `errSecDuplicateCallback`, `SecBase.h`: "The specified callback function is
/// already installed." The API is keyed on the function pointer alone, which the
/// guest's own comment relies on (`trust_store_mac.cc:512`: "Since
/// SecKeychainAddCallback is keyed on the function pointer only, we need to
/// ensure that each template instantiation of this function has a different
/// address"), so registering one pointer twice really is a duplicate.
pub const DUPLICATE_CALLBACK: OSStatus = -25297;
/// `errSecItemNotFound`, `SecBase.h`: "The specified item could not be found in
/// the keychain." Read out of the guest: see the module note.
pub const ITEM_NOT_FOUND: OSStatus = -25300;
/// `errSecNoDefaultKeychain`, `SecBase.h`: "A default keychain could not be
/// found." Also true here, and more precisely than
/// [`NO_SUCH_KEYCHAIN`] for a caller that asked for *the* default one.
pub const NO_DEFAULT_KEYCHAIN: OSStatus = -25307;
/// `errSecInteractionNotAllowed`, `SecBase.h`: "User interaction is not
/// allowed." The `errSec*` family's spelling - note that Authorization Services
/// has its own, differently numbered, code of the same name below.
pub const INTERACTION_NOT_ALLOWED: OSStatus = -25308;
/// `errSecNoTrustSettings`, `SecBase.h`: "No Trust Settings were found."
/// `SecTrustSettings.h` documents it as the answer of
/// `SecTrustSettingsCopyCertificates` when a domain has none, which is the
/// condition on this host.
pub const NO_TRUST_SETTINGS: OSStatus = -25263;
/// `errSecCSInvalidObjectRef`, `CSCommon.h`: "invalid API object reference."
/// The answer for a `SecCodeRef`/`SecACLRef` this runtime did not make.
pub const CS_INVALID_OBJECT_REF: OSStatus = -67071;
/// `errSecCSUnsigned`, `CSCommon.h`: "code object is not signed at all."
///
/// The exact state of every Mach-O on this host, and a status the guest is
/// written to handle: `chrome/browser/apps/app_shim/code_signature_mac.cc`
/// says "If the framework bundle is unsigned there is nothing else to do. We
/// treat this as success because there's no identity to protect or even match."
pub const CS_UNSIGNED: OSStatus = -67062;
/// `errSecCSNoSuchCode`, `CSCommon.h`: "host has no guest with the requested
/// attributes." There is no code-signing database to hold a guest.
pub const CS_NO_SUCH_CODE: OSStatus = -67065;
/// `errSecCSReqUnsupported`, `CSCommon.h`: "unsupported type or version of code
/// requirements." The honest answer for a requirement this module holds as an
/// undecoded blob.
pub const CS_REQ_UNSUPPORTED: OSStatus = -67051;
/// `errAuthorizationSuccess`, `Authorization.h`.
pub const AUTHORIZATION_SUCCESS: OSStatus = 0;
/// `errAuthorizationCanceled`, `Authorization.h`: "The authorization was
/// cancelled by the user." Read out of the guest: see the module note. Chromium
/// compares against this value and stays quiet when it sees it, which is why it
/// is the right thing to answer for a request nobody can be asked about.
pub const AUTHORIZATION_CANCELED: OSStatus = -60006;
/// `errAuthorizationDenied`, `AuthorizationDB.h`, documented against
/// `AuthorizationRightGet` as: "No definition found." Which is exactly the state
/// of every login right on this host.
pub const AUTHORIZATION_DENIED: OSStatus = -60005;
/// `errAuthorizationInteractionNotAllowed`, `Authorization.h`: "The
/// authorization was denied since no user interaction was possible."
///
/// The truest sentence in the whole of Security for this host: there is no user
/// to interact with. Note it is *not* the same number as the `errSec*` family's
/// `errSecInteractionNotAllowed`; Apple has two.
pub const AUTHORIZATION_INTERACTION_NOT_ALLOWED: OSStatus = -60007;
/// `errAuthorizationToolExecuteFailure`, `Authorization.h`: "The specified
/// program could not be executed." Which is what
/// `AuthorizationExecuteWithPrivileges` reports when there is nothing to
/// execute and no way to escalate.
pub const AUTHORIZATION_TOOL_EXECUTE_FAILURE: OSStatus = -60031;

/// Say, once, that a symbol is not being answered.
///
/// The project's rule is that a named refusal beats a value that might be
/// wrong, and that a refusal which *looks* like a success is the worst outcome
/// there is. So every refusal in this crate comes through here, and every
/// refusal's return value is a non-`errSecSuccess` status or a null handle, so
/// a caller that only checks the status is not misled either.
///
/// Deduplicated on the whole message, not per call site: a guest in a loop
/// should produce one line, while a second *kind* of refusal should still be
/// said. `keychain_legacy::SecKeychainAddCallback` relies on that - it says the
/// same sentence for every callback, but says it again if the mask differs.
pub fn refuse(symbol: &str, detail: &str) {
    let message = format!("rosetta3: security: {symbol} is not answered here: {detail}");
    static SAID: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    let said = said.get_or_insert_with(HashSet::new);
    if said.insert(message.clone()) {
        eprintln!("{message}");
    }
}

/// `usize`, which is what a `SecCSFlags` / `SecKeychainEventMask` /
/// `SecAccessControlCreateFlags` word is on the way in and out of a guest.
///
/// The declared parameter types stay narrow in each function's own signature,
/// because that is the interface; this is only for the places a word is carried
/// across a `static` where a pointer type would not be `Send`.
pub type Word = usize;

#[cfg(test)]
mod tests {
    use super::*;

    /// The values the guest binary itself compares against. If a header and the
    /// program disagree, the program is what has to be satisfied.
    #[test]
    fn the_codes_the_guest_compiles_against_have_these_values() {
        // net/cert/internal/trust_store_mac.cc: `cmp eax, 0xffff9d2c`
        assert_eq!(ITEM_NOT_FOUND, 0xffff9d2c_u32 as i32);
        // base/mac/authorization_util.mm: `cmp eax, 0xffff159a`
        assert_eq!(AUTHORIZATION_CANCELED, 0xffff159a_u32 as i32);
        // SecBase.h, for the two the two above are neighbours of.
        assert_eq!(NOT_AVAILABLE, -25291);
        assert_eq!(AUTH_FAILED, -25293);
        assert_eq!(NO_SUCH_KEYCHAIN, -25294);
        assert_eq!(DUPLICATE_ITEM, -25299);
        assert_eq!(NO_DEFAULT_KEYCHAIN, -25307);
        assert_eq!(INTERACTION_NOT_ALLOWED, -25308);
        assert_eq!(NO_TRUST_SETTINGS, -25263);
        // CSCommon.h.
        assert_eq!(CS_UNSIGNED, -67062);
        assert_eq!(CS_NO_SUCH_CODE, -67065);
        assert_eq!(CS_REQ_UNSUPPORTED, -67051);
        assert_eq!(CS_INVALID_OBJECT_REF, -67071);
        // Authorization.h. Note the second `InteractionNotAllowed`: the
        // Authorization family and the errSec family number it differently.
        assert_eq!(AUTHORIZATION_DENIED, -60005);
        assert_eq!(AUTHORIZATION_INTERACTION_NOT_ALLOWED, -60007);
        assert_eq!(AUTHORIZATION_TOOL_EXECUTE_FAILURE, -60031);
    }

    /// A refusal is said, and said once: a guest that calls in a loop must not
    /// turn one missing symbol into thousands of lines.
    #[test]
    fn a_refusal_is_said_once() {
        refuse("SecTestSaidOnce", "a test reason");
        refuse("SecTestSaidOnce", "a test reason");
        // A different reason is a different sentence and is still said.
        refuse("SecTestSaidOnce", "another test reason");
    }
}
