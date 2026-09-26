#![allow(non_snake_case)]

//! `SecStaticCodeRef`, `SecCodeRef` guests, and the signer.
//!
//! The two functions here that predate this session - `SecCodeCopySelf` and
//! `SecCodeCheckValidity` - are unchanged. What is added below is the rest of
//! the surface Chromium Framework imports, and its organising fact is
//! [`SecStaticCodeCreateWithPath`]: **every Mach-O on this host is unsigned**, so
//! `errSecCSUnsigned` is not a failure here, it is the description.
//!
//! That status is load-bearing, because the guest is written for it.
//! `chrome/browser/apps/app_shim/code_signature_mac.cc:62-73`:
//!
//! ```c++
//! // If the framework bundle is unsigned there is nothing else to do. We treat
//! // this as success because there's no identity to protect or even match, so
//! // it's not dangerous to let the shim connect.
//! if (status == errSecCSUnsigned) {
//!   return base::unexpected(MissingRequirementReason::NoOrAdHocSignature);
//! }
//! // If there was an error obtaining the SecStaticCodeRef something is very
//! // broken or something bad is happening, deny.
//! if (status != errSecSuccess) {
//!   DumpOSStatusError(status, "SecStaticCodeCreateWithPath");
//!   return base::unexpected(MissingRequirementReason::Error);
//! }
//! ```
//!
//! So the truthful status is also the one the guest handles as benign, and the
//! three functions below it - `SecCodeCopySigningInformation`,
//! `SecCodeCopyDesignatedRequirement` and `SecRequirementCopyString` - are never
//! reached from that caller. They are still bound and still refuse by name,
//! because an unbound import is a jump through a zero slot.
//!
//! `SecCodeCopyGuestWithAttributes` is the sharp one. Its caller
//! (`code_signature_mac.cc:203` and `:222`) feeds the result straight to
//! [`SecCodeCheckValidity`], which answers `errSecSuccess` for a handle this
//! module made. A guest object here would therefore be reported as a *validly
//! signed process*, which is the most consequential wrong answer available in
//! this framework. So it is refused, with `errSecCSNoSuchCode` - "host has no
//! guest with the requested attributes" - which is true: there is no
//! code-signing database on this host to hold a guest.

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use objc::class::Class;
use objc::object::Id;

use crate::status::{self, OSStatus};
use crate::status::SUCCESS;

/// A `SecCodeRef` or a `SecRequirementRef`.
///
/// One class for both: nothing here tells them apart, and a guest holds each
/// only to pass it back. The payload is what the handle was made from - the
/// requirement's text, or the running program's path - so that a report can say
/// which requirement was not met rather than only that one was not.
#[repr(C)]
struct Body {
    isa: *const Class,
    description: CFTypeRef,
}

fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        let class =
            corefoundation::base::register(c"__R3SecCodeObject", None, size_of::<Body>(), true);
        class.replace_method(
            objc::sel::get("dealloc"),
            dealloc as *const c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("a code object class") }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: objc::sel::Sel) {
    // SAFETY: this class's object layout and its owned description.
    unsafe {
        let body = receiver as *mut Body;
        objc::arc::release((*body).description as Id);
        objc::arc::finalize(receiver);
    }
}

/// A handle carrying `description`, owned by the caller.
fn new(description: CFTypeRef) -> CFTypeRef {
    // SAFETY: `Body` is the registered layout.
    let body = unsafe { corefoundation::base::allocate::<Body>(class()) };
    if body.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a freshly allocated body.
    unsafe { (*body).description = description };
    body as CFTypeRef
}

/// # Safety
///
/// `object` is null or an object this runtime allocated.
unsafe fn is_ours(object: CFTypeRef) -> bool {
    unsafe { corefoundation::base::is_kind_of(object, class()) }
}

/// `OSStatus SecRequirementCreateWithString(CFStringRef text, SecCSFlags flags,
/// SecRequirementRef *requirement)`
///
/// The text is kept rather than parsed. Parsing it would buy nothing: the check
/// below cannot be met by any code on this host, so a requirement that had been
/// understood and one that had not would be refused alike.
///
/// # Safety
///
/// `requirement` is null or a writable `SecRequirementRef`.
pub unsafe extern "C" fn SecRequirementCreateWithString(
    text: CFTypeRef,
    _flags: u32,
    requirement: *mut CFTypeRef,
) -> OSStatus {
    if requirement.is_null() {
        return status::PARAM;
    }
    // SAFETY: a string or null.
    if unsafe { corefoundation::string::as_string(text) }.is_none() {
        return status::PARAM;
    }
    let held = objc::arc::retain(text as Id) as CFTypeRef;
    // SAFETY: the caller states it is writable.
    unsafe { *requirement = new(held) };
    SUCCESS
}

/// `OSStatus SecRequirementCreateWithData(CFDataRef data, SecCSFlags flags,
/// SecRequirementRef *requirement)`
///
/// The compiled form of the same thing. Kept with no description, because the
/// bytes are a requirement blob this tree does not decode.
///
/// # Safety
///
/// As [`SecRequirementCreateWithString`].
pub unsafe extern "C" fn SecRequirementCreateWithData(
    _data: CFTypeRef,
    _flags: u32,
    requirement: *mut CFTypeRef,
) -> OSStatus {
    if requirement.is_null() {
        return status::PARAM;
    }
    // SAFETY: the caller states it is writable.
    unsafe { *requirement = new(std::ptr::null()) };
    SUCCESS
}

/// `OSStatus SecCodeCopySelf(SecCSFlags flags, SecCodeRef *self)`
///
/// # Safety
///
/// `out` is null or a writable `SecCodeRef`.
pub unsafe extern "C" fn SecCodeCopySelf(_flags: u32, out: *mut CFTypeRef) -> OSStatus {
    if out.is_null() {
        return status::PARAM;
    }
    let path = std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let description = corefoundation::string::from_bytes(path.as_bytes(), false);
    // SAFETY: the caller states it is writable.
    unsafe { *out = new(description) };
    SUCCESS
}

/// `OSStatus SecCodeCheckValidity(SecCodeRef code, SecCSFlags flags,
/// SecRequirementRef requirement)`
///
/// Always `errSecSuccess`, with nothing verified - see the module note. Said
/// once on stderr, because a signature check that did not happen is exactly the
/// thing that must not pass unremarked.
///
/// # Safety
///
/// Both arguments are null or objects this runtime allocated.
pub unsafe extern "C" fn SecCodeCheckValidity(
    code: CFTypeRef,
    _flags: u32,
    requirement: CFTypeRef,
) -> OSStatus {
    if !unsafe { is_ours(code) } {
        return status::CS_INVALID_OBJECT_REF;
    }
    // SAFETY: as above; a null requirement means "any valid code", which is
    // still no code, so it is refused the same way.
    if !requirement.is_null() && !unsafe { is_ours(requirement) } {
        return status::CS_INVALID_OBJECT_REF;
    }
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: security: the guest checked its own code signature. There is no \
             code-signing on this host, so nothing was examined and the check is reported as \
             passing - see crates/security/src/codesign.rs"
        );
    });
    SUCCESS
}

/// `OSStatus SecStaticCodeCreateWithPath(CFURLRef path, SecCSFlags flags,
/// SecStaticCodeRef *staticCode)`
///
/// `errSecCSUnsigned` (`CSCommon.h`: "code object is not signed at all") and a
/// null handle. The module note has the guest's own comment on why that is the
/// benign answer.
///
/// The path is not examined, so the answer does not depend on whether the file
/// exists: a missing file and an unsigned one are different questions, and
/// answering about this one alone is the whole of the answer.
///
/// # Safety
///
/// `path` is null or a `CFURLRef`; `static_code` is null or writable.
pub unsafe extern "C" fn SecStaticCodeCreateWithPath(
    _path: CFTypeRef,
    _flags: u64,
    static_code: *mut CFTypeRef,
) -> OSStatus {
    if !static_code.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { static_code.write(std::ptr::null_mut()) };
    }
    // `SecStaticCodeRef` is a `CFTypeRef`, and the guest compares the type ID of
    // what it gets back - so a handle has to be absent, not a stand-in.
    if static_code.is_null() {
        return status::PARAM;
    }
    status::CS_UNSIGNED
}

/// `OSStatus SecCodeCopySigningInformation(SecStaticCodeRef code, SecCSFlags flags,
/// CFDictionaryRef *information)`
///
/// Refused by name. On macOS this returns the code's identity, its entitlements
/// and its flags - and this host has no code signature, so it has no identity and
/// no entitlements to report. A dictionary here would be a set of claims about a
/// signature that was never verified.
///
/// # Safety
///
/// `code` is null or a `SecStaticCodeRef`; `information` is null or writable.
pub unsafe extern "C" fn SecCodeCopySigningInformation(
    _code: CFTypeRef,
    _flags: u32,
    information: *mut CFTypeRef,
) -> OSStatus {
    if !information.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { information.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "SecCodeCopySigningInformation",
        "a code's signing information is its verified identity, its entitlements and its \
         flags. Nothing on this host is signed, so there is no verified identity to report, and \
         a dictionary here would be claims about a signature that was never checked.",
    );
    status::CS_UNSIGNED
}

/// `OSStatus SecCodeCopyDesignatedRequirement(SecStaticCodeRef code, SecCSFlags
/// flags, SecRequirementRef *requirement)`
///
/// Refused by name. A designated requirement is derived from a signature, and
/// there is none. The requirement object that *is* made here - by
/// [`SecRequirementCreateWithString`] - carries the text it was given and nothing
/// derived, and this function is the one that would have to derive something.
///
/// # Safety
///
/// `code` is null or a `SecStaticCodeRef`; `requirement` is null or writable.
pub unsafe extern "C" fn SecCodeCopyDesignatedRequirement(
    _code: CFTypeRef,
    _flags: u32,
    requirement: *mut CFTypeRef,
) -> OSStatus {
    if !requirement.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { requirement.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "SecCodeCopyDesignatedRequirement",
        "a designated requirement is read out of a code signature, and nothing on this host \
         has one. Deriving one from nothing would be a rule invented here and then checked \
         against code this module also does not examine.",
    );
    status::CS_UNSIGNED
}

/// `OSStatus SecRequirementCopyString(SecRequirementRef requirement, SecCSFlags
/// flags, CFStringRef *text)`
///
/// The text the requirement was created with, and `errSecSuccess`.
///
/// This one is real rather than a refusal, because
/// [`SecRequirementCreateWithString`] keeps the text it was handed and this
/// function hands the same text back. That is not a derived value and not a
/// decision: it is the requirement's own words, which is what
/// `code_signature_mac.cc:126` asks for when it builds the string it later parses
/// with `SecRequirementCreateWithString`.
///
/// The one thing that cannot be answered is a requirement that came from
/// somewhere else - a `SecRequirementRef` from a blob, which
/// [`SecRequirementCreateWithData`] accepts and deliberately does not decode.
/// For that, `errSecCSReqUnsupported` ("unsupported type or version of code
/// requirements"), which is the honest description of holding a blob whose
/// contents were not read.
///
/// # Safety
///
/// `requirement` is null or a requirement this module made; `text` is null or
/// writable.
pub unsafe extern "C" fn SecRequirementCopyString(
    requirement: CFTypeRef,
    _flags: u32,
    text: *mut CFTypeRef,
) -> OSStatus {
    if !text.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { text.write(std::ptr::null_mut()) };
    }
    if !unsafe { is_ours(requirement) } {
        return status::CS_INVALID_OBJECT_REF;
    }
    // SAFETY: checked above, so this is a `Body`.
    let description = unsafe { (*(requirement as *const Body)).description };
    if description.is_null() {
        status::refuse(
            "SecRequirementCopyString",
            "this requirement was made from a compiled requirement blob, which this module \
             does not decode, so there are no words to copy back out of it.",
        );
        return status::CS_REQ_UNSUPPORTED;
    }
    if !text.is_null() {
        // A `Copy`, so the caller owns it - which is what `GetFrameworkRequirement`
        // relies on when it puts the string in a `ScopedCFTypeRef`.
        // SAFETY: a `CFString` this module holds a reference to.
        unsafe { text.write(corefoundation::api::CFRetain(description)) };
    }
    SUCCESS
}

/// `OSStatus SecCodeCopyGuestWithAttributes(SecCodeRef host, CFDictionaryRef
/// attributes, SecCSFlags flags, SecCodeRef *guest)`
///
/// Refused by name, with `errSecCSNoSuchCode` - "host has no guest with the
/// requested attributes". See the module note: the caller passes the result to
/// [`SecCodeCheckValidity`], which would report success for a handle this module
/// made, so anything returned here would be a fabricated *valid signature*.
///
/// # Safety
///
/// `host` is null or a `SecCodeRef`; `attributes` is null or a `CFDictionary`;
/// `guest` is null or writable.
pub unsafe extern "C" fn SecCodeCopyGuestWithAttributes(
    _host: CFTypeRef,
    _attributes: CFTypeRef,
    _flags: u32,
    guest: *mut CFTypeRef,
) -> OSStatus {
    if !guest.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { guest.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "SecCodeCopyGuestWithAttributes",
        "this looks up another process's code identity by pid or audit token. There is no \
         code-signing database on this host to hold one, and the caller passes what it gets \
         straight to SecCodeCheckValidity - which would then report a *valid signature* for a \
         process nobody has examined. errSecCSNoSuchCode rather than a handle.",
    );
    status::CS_NO_SUCH_CODE
}

/// `OSStatus SecCodeSignerCreate(CFDictionaryRef parameters, SecCSFlags flags,
/// SecCodeSignerRef *signer)`
///
/// Refused by name. The prototype is Chromium's own, written out in
/// `web_app_shortcut_creator.mm:70` and cross-checked against the three registers
/// the guest sets at `Framework+0x46fc8bf`; the reference in that file is Apple's
/// own `OSX/libsecurity_codesigning/lib/SecCodeSigner.h`.
///
/// Signing needs a code-signing identity. The guest passes
/// `kSecCodeSignerIdentity: [NSNull null]` - an *ad-hoc* signature with no identity
/// - and a non-`errSecSuccess` return is handled as "cannot create the shortcut"
/// (`return false`), which is the truth here.
///
/// # Safety
///
/// `parameters` is null or a `CFDictionary`; `signer` is null or writable.
pub unsafe extern "C" fn SecCodeSignerCreate(
    _parameters: CFTypeRef,
    _flags: u32,
    signer: *mut CFTypeRef,
) -> OSStatus {
    if !signer.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { signer.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "SecCodeSignerCreate",
        "signing needs a code-signing identity from a keychain, and this host has no \
         keychain and no identity. The guest asks for an ad-hoc signature here, which still \
         needs the signing machinery to produce one.",
    );
    status::NOT_AVAILABLE
}

/// `OSStatus SecCodeSignerAddSignatureWithErrors(SecCodeSignerRef signer,
/// SecStaticCodeRef code, SecCSFlags flags, CFErrorRef *errors)`
///
/// Refused by name. Four arguments, from the same declaration and the four
/// registers at `Framework+0x46fc8e2..0x46fc900`.
///
/// `errors` is left null: there is no `CFError` object to describe a refusal that
/// is the same refusal every time, and the caller logs it
/// (`web_app_shortcut_creator.mm:1060`) rather than inspecting it.
///
/// # Safety
///
/// `signer` and `code` are null or objects; `errors` is null or writable.
pub unsafe extern "C" fn SecCodeSignerAddSignatureWithErrors(
    _signer: CFTypeRef,
    _code: CFTypeRef,
    _flags: u32,
    errors: *mut CFTypeRef,
) -> OSStatus {
    if !errors.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { errors.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "SecCodeSignerAddSignatureWithErrors",
        "adding a signature means writing a code signature blob into a Mach-O, verified against \
         an identity from a keychain. Neither the identity nor the verification exists here, and \
         a signature written without either would be a claim of authenticity that nothing \
         supports.",
    );
    status::NOT_AVAILABLE
}

exports::exports! {
    functions {
        "SecRequirementCreateWithString" => SecRequirementCreateWithString,
        "SecRequirementCreateWithData" => SecRequirementCreateWithData,
        "SecCodeCopySelf" => SecCodeCopySelf,
        "SecCodeCheckValidity" => SecCodeCheckValidity,
        "SecStaticCodeCreateWithPath" => SecStaticCodeCreateWithPath,
        "SecCodeCopySigningInformation" => SecCodeCopySigningInformation,
        "SecCodeCopyDesignatedRequirement" => SecCodeCopyDesignatedRequirement,
        "SecRequirementCopyString" => SecRequirementCopyString,
        "SecCodeCopyGuestWithAttributes" => SecCodeCopyGuestWithAttributes,
        "SecCodeSignerCreate" => SecCodeSignerCreate,
        "SecCodeSignerAddSignatureWithErrors" => SecCodeSignerAddSignatureWithErrors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pair a program makes, and the answer it gets: both handles come
    /// back, and the check passes - while a handle this module did not make is
    /// still refused, which is what keeps the success from being unconditional.
    #[test]
    fn the_check_passes_for_a_handle_this_module_made() {
        objc::bootstrap();
        let text = corefoundation::string::from_bytes(b"anchor apple generic", false);
        let mut requirement = std::ptr::null();
        let mut code = std::ptr::null();
        // SAFETY: a string this test made, and two writable out-parameters.
        unsafe {
            assert_eq!(
                SecRequirementCreateWithString(text, 0, &mut requirement),
                SUCCESS
            );
            assert_eq!(SecCodeCopySelf(0, &mut code), SUCCESS);
        }
        assert!(!requirement.is_null() && !code.is_null());
        // SAFETY: two handles the calls above made.
        assert_eq!(
            unsafe { SecCodeCheckValidity(code, 0, requirement) },
            SUCCESS
        );
        // SAFETY: a handle this test owns, against storage that is not one.
        let mut other = 0_u64;
        assert_eq!(
            unsafe { SecCodeCheckValidity((&raw mut other).cast(), 0, requirement) },
            status::CS_INVALID_OBJECT_REF
        );
        // SAFETY: three objects this test owns.
        unsafe {
            corefoundation::api::CFRelease(requirement);
            corefoundation::api::CFRelease(code);
            corefoundation::api::CFRelease(text);
        }
    }

    /// `errSecCSUnsigned` and a null handle - the status the guest is written to
    /// treat as benign, and the true one for every Mach-O here.
    #[test]
    fn every_code_object_here_is_unsigned() {
        objc::bootstrap();
        let mut code: CFTypeRef = 1_usize as CFTypeRef;
        // SAFETY: a null path, which the answer does not depend on, and a
        // writable out-parameter.
        unsafe {
            assert_eq!(
                SecStaticCodeCreateWithPath(std::ptr::null(), 0, &mut code),
                status::CS_UNSIGNED
            );
            assert!(code.is_null());
            assert_eq!(
                SecStaticCodeCreateWithPath(std::ptr::null(), 0, std::ptr::null_mut()),
                status::PARAM
            );
        }
    }

    /// The requirement's own words come back, because that is all it holds; and
    /// one made from a compiled blob has none, and says so.
    #[test]
    fn a_requirement_gives_back_the_text_it_was_given() {
        objc::bootstrap();
        let text = corefoundation::string::from_bytes(b"anchor apple generic", false);
        let mut requirement = std::ptr::null();
        // SAFETY: a string this test made, and a writable out-parameter.
        unsafe {
            assert_eq!(
                SecRequirementCreateWithString(text, 0, &mut requirement),
                SUCCESS
            );
            let mut copy: CFTypeRef = std::ptr::null();
            assert_eq!(
                SecRequirementCopyString(requirement, 0, &mut copy),
                SUCCESS
            );
            assert!(!copy.is_null());
            assert_eq!(
                foundation::string::bytes(copy as Id),
                b"anchor apple generic"
            );
            corefoundation::api::CFRelease(copy);

            // A requirement from a blob holds no text, and admits it.
            let mut from_blob = std::ptr::null();
            assert_eq!(
                SecRequirementCreateWithData(std::ptr::null(), 0, &mut from_blob),
                SUCCESS
            );
            let mut none: CFTypeRef = 1_usize as CFTypeRef;
            assert_eq!(
                SecRequirementCopyString(from_blob, 0, &mut none),
                status::CS_REQ_UNSUPPORTED
            );
            assert!(none.is_null());
            corefoundation::api::CFRelease(from_blob);

            // A requirement this module did not make is refused, not read.
            let mut other = 0_u64;
            let mut text_out: CFTypeRef = 1_usize as CFTypeRef;
            assert_eq!(
                SecRequirementCopyString((&raw mut other).cast(), 0, &mut text_out),
                status::CS_INVALID_OBJECT_REF
            );
            assert!(text_out.is_null());
            corefoundation::api::CFRelease(requirement);
            corefoundation::api::CFRelease(text);
        }
    }

    /// Every remaining function refuses, and hands back nothing the caller could
    /// mistake for a verified code object - which matters most for
    /// `SecCodeCopyGuestWithAttributes`, whose result goes straight into
    /// `SecCodeCheckValidity`.
    #[test]
    fn the_rest_refuse_and_hand_back_nothing() {
        objc::bootstrap();
        let mut out: CFTypeRef = 1_usize as CFTypeRef;
        // SAFETY: null arguments throughout, which every one of these documents
        // as acceptable, and writable out-parameters.
        unsafe {
            assert_eq!(
                SecCodeCopySigningInformation(std::ptr::null(), 0, &mut out),
                status::CS_UNSIGNED
            );
            assert!(out.is_null());
            assert_eq!(
                SecCodeCopyDesignatedRequirement(std::ptr::null(), 0, &mut out),
                status::CS_UNSIGNED
            );
            assert!(out.is_null());
            assert_eq!(
                SecCodeCopyGuestWithAttributes(
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    &mut out
                ),
                status::CS_NO_SUCH_CODE
            );
            assert!(out.is_null(), "nothing here may reach SecCodeCheckValidity");
            assert_eq!(
                SecCodeSignerCreate(std::ptr::null(), 0, &mut out),
                status::NOT_AVAILABLE
            );
            assert!(out.is_null());
            let mut errors: CFTypeRef = 1_usize as CFTypeRef;
            assert_eq!(
                SecCodeSignerAddSignatureWithErrors(
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    &mut errors
                ),
                status::NOT_AVAILABLE
            );
            assert!(errors.is_null());
        }
    }
}
