#![allow(non_snake_case)]

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use objc::class::Class;
use objc::object::Id;

type OSStatus = i32;
const SUCCESS: OSStatus = 0;
/// `errSecCSInvalidObjectRef`: a handle that is not one of these.
const INVALID_OBJECT_REF: OSStatus = -67071;
/// `errSecParam`.
const PARAM: OSStatus = -50;
/// `errSecCSObjectRequired`: a required pointer argument was null.
const OBJECT_REQUIRED: OSStatus = -67069;
/// `errSecCSStaticCodeNotFound`: nothing on disk at the path.
const STATIC_CODE_NOT_FOUND: OSStatus = -67068;
/// `errSecCSUnsigned`: the image carries no signature to read.
const UNSIGNED: OSStatus = -67062;
/// `errSecCSReqFailed`: the code does not satisfy the requirement.
const REQ_FAILED: OSStatus = -67050;

/// `CSMAGIC_EMBEDDED_SIGNATURE`: the superblob `LC_CODE_SIGNATURE` points at.
const CSMAGIC_EMBEDDED_SIGNATURE: u32 = 0xfade_0cc0;
/// `CSMAGIC_EMBEDDED_ENTITLEMENTS`: the XML entitlements blob.
const CSMAGIC_EMBEDDED_ENTITLEMENTS: u32 = 0xfade_7171;
/// `CSSLOT_ENTITLEMENTS`.
const CSSLOT_ENTITLEMENTS: u32 = 5;

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
        return PARAM;
    }
    // SAFETY: a string or null.
    if unsafe { corefoundation::string::as_string(text) }.is_none() {
        return PARAM;
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
        return PARAM;
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
        return PARAM;
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
        return INVALID_OBJECT_REF;
    }
    // SAFETY: as above; a null requirement means "any valid code", which is
    // still no code, so it is refused the same way.
    if !requirement.is_null() && !unsafe { is_ours(requirement) } {
        return INVALID_OBJECT_REF;
    }
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosemary: security: the guest checked its own code signature. There is no \
             code-signing on this host, so nothing was examined and the check is reported as \
             passing - see crates/security/src/codesign.rs"
        );
    });
    SUCCESS
}

/// `OSStatus SecStaticCodeCreateWithPath(CFURLRef path, SecCSFlags flags,
/// SecStaticCodeRef *staticCode)`
///
/// # Safety
///
/// `path` is null or a CF object; `out` is null or a writable `SecStaticCodeRef`.
pub unsafe extern "C" fn SecStaticCodeCreateWithPath(
    path: CFTypeRef,
    _flags: u32,
    out: *mut CFTypeRef,
) -> OSStatus {
    if out.is_null() {
        return OBJECT_REQUIRED;
    }
    // SAFETY: null or a CF object, as the caller states.
    let Some(path) = unsafe { corefoundation::url::as_url(path) }.and_then(|url| url.file_path())
    else {
        return PARAM;
    };
    if !std::path::Path::new(&path).exists() {
        return STATIC_CODE_NOT_FOUND;
    }
    let description = corefoundation::string::from_bytes(path.as_bytes(), false);
    // SAFETY: the caller states it is writable.
    unsafe { *out = new(description) };
    SUCCESS
}

/// `OSStatus SecStaticCodeCheckValidity(SecStaticCodeRef staticCode,
/// SecCSFlags flags, SecRequirementRef requirement)`
///
/// # Safety
///
/// As [`SecStaticCodeCheckValidityWithErrors`].
pub unsafe extern "C" fn SecStaticCodeCheckValidity(
    code: CFTypeRef,
    flags: u32,
    requirement: CFTypeRef,
) -> OSStatus {
    // SAFETY: as the caller states.
    unsafe { SecStaticCodeCheckValidityWithErrors(code, flags, requirement, std::ptr::null_mut()) }
}

/// `OSStatus SecStaticCodeCheckValidityWithErrors(SecStaticCodeRef staticCode,
/// SecCSFlags flags, SecRequirementRef requirement, CFErrorRef *errors)`
///
/// A requirement that only asks whether the code holds an entitlement is
/// answered from the image's own entitlements blob, because that is data the
/// file carries and a wrong answer changes behavior: Qt decides whether it is
/// sandboxed this way. Anything else is the signature check of
/// [`SecCodeCheckValidity`], and passes the same way.
///
/// # Safety
///
/// `code` and `requirement` are null or objects this runtime allocated; `errors`
/// is null or writable.
pub unsafe extern "C" fn SecStaticCodeCheckValidityWithErrors(
    code: CFTypeRef,
    flags: u32,
    requirement: CFTypeRef,
    errors: *mut CFTypeRef,
) -> OSStatus {
    if !errors.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *errors = std::ptr::null() };
    }
    // SAFETY: as the caller states.
    let entitlement = unsafe { required_entitlement(requirement) };
    let Some(entitlement) = entitlement else {
        // SAFETY: as the caller states.
        return unsafe { SecCodeCheckValidity(code, flags, requirement) };
    };
    // SAFETY: as the caller states.
    if !unsafe { is_ours(code) } {
        return INVALID_OBJECT_REF;
    }
    // SAFETY: one of this module's handles, whose description is a path.
    let path = unsafe { description(code) };
    let Some(path) = path else {
        return INVALID_OBJECT_REF;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return STATIC_CODE_NOT_FOUND;
    };
    match entitlements(&bytes) {
        None => UNSIGNED,
        Some(held) if held.get(&entitlement).is_some() => SUCCESS,
        Some(_) => REQ_FAILED,
    }
}

/// The text a handle was made from.
///
/// # Safety
///
/// `object` is one of this module's handles.
unsafe fn description(object: CFTypeRef) -> Option<String> {
    // SAFETY: this class's layout, as the caller states.
    let text = unsafe { (*(object as *const Body)).description };
    // SAFETY: a description is null or a string.
    let text = unsafe { corefoundation::string::as_string(text) }?;
    String::from_utf8(text.utf8()).ok()
}

/// The key of a requirement of the form `entitlement["key"] exists`.
///
/// # Safety
///
/// `requirement` is null or an object this runtime allocated.
unsafe fn required_entitlement(requirement: CFTypeRef) -> Option<String> {
    // SAFETY: as the caller states.
    if requirement.is_null() || !unsafe { is_ours(requirement) } {
        return None;
    }
    // SAFETY: one of this module's handles.
    let text = unsafe { description(requirement) }?;
    let key = text
        .trim()
        .strip_prefix("entitlement[\"")?
        .strip_suffix("exists")?
        .trim_end()
        .strip_suffix("\"]")?;
    (!key.contains('"')).then(|| key.to_string())
}

/// The entitlements the guest's slice of `bytes` is signed with, or `None` for
/// an image with no signature. A signature without an entitlements blob holds
/// none, which is an empty dictionary rather than an unsigned image.
fn entitlements(bytes: &[u8]) -> Option<corefoundation::plist::Value> {
    let signature = match darwin::sysctl::guest_arch() {
        darwin::sysctl::GuestArch::X86_64 => signature_of(bytes, machish::spec::arch::X86_64),
        darwin::sysctl::GuestArch::Arm64 => signature_of(bytes, machish::spec::arch::Arm64),
    };
    let (image, data) = signature?;
    let start = data.dataoff as usize;
    let superblob = image.get(start..start.checked_add(data.datasize as usize)?)?;
    let blob = blob_in_slot(superblob, CSSLOT_ENTITLEMENTS);
    let Some(blob) = blob else {
        return Some(corefoundation::plist::Value::Dictionary(Default::default()));
    };
    (be32(blob, 0)? == CSMAGIC_EMBEDDED_ENTITLEMENTS).then_some(())?;
    corefoundation::plist::parse(blob.get(8..)?).ok()
}

/// The thin image for `spec` and its `LC_CODE_SIGNATURE`, if it has one.
fn signature_of<S: machish::spec::MachSpec>(
    bytes: &[u8],
    spec: S,
) -> Option<(&[u8], machish::image::LinkeditData)> {
    let object = machish::image::MachObject::parse_any(bytes, spec).ok()?;
    let data = object.commands.iter().find_map(|command| match command {
        machish::image::LoadCommand::CodeSignature(data) => Some(*data),
        _ => None,
    })?;
    Some((object.bytes(), data))
}

/// The blob a `CS_SuperBlob` indexes under `slot`, bounded by its own length.
fn blob_in_slot(superblob: &[u8], slot: u32) -> Option<&[u8]> {
    if be32(superblob, 0)? != CSMAGIC_EMBEDDED_SIGNATURE {
        return None;
    }
    let count = be32(superblob, 8)? as usize;
    (0..count).find_map(|index| {
        let entry = 12 + index * 8;
        if be32(superblob, entry)? != slot {
            return None;
        }
        let offset = be32(superblob, entry + 4)? as usize;
        let length = be32(superblob, offset + 4)? as usize;
        superblob.get(offset..offset.checked_add(length)?)
    })
}

fn be32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

exports::exports! {
    functions {
        "SecStaticCodeCreateWithPath" => SecStaticCodeCreateWithPath,
        "SecStaticCodeCheckValidity" => SecStaticCodeCheckValidity,
        "SecStaticCodeCheckValidityWithErrors" => SecStaticCodeCheckValidityWithErrors,
        "SecRequirementCreateWithString" => SecRequirementCreateWithString,
        "SecRequirementCreateWithData" => SecRequirementCreateWithData,
        "SecCodeCopySelf" => SecCodeCopySelf,
        "SecCodeCheckValidity" => SecCodeCheckValidity,
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
            INVALID_OBJECT_REF
        );
        // SAFETY: three objects this test owns.
        unsafe {
            corefoundation::api::CFRelease(requirement);
            corefoundation::api::CFRelease(code);
            corefoundation::api::CFRelease(text);
        }
    }

    fn superblob(entitlements: &[u8]) -> Vec<u8> {
        let mut blob = CSMAGIC_EMBEDDED_ENTITLEMENTS.to_be_bytes().to_vec();
        blob.extend_from_slice(&(8 + entitlements.len() as u32).to_be_bytes());
        blob.extend_from_slice(entitlements);
        let offset = 12 + 8;
        let mut superblob = CSMAGIC_EMBEDDED_SIGNATURE.to_be_bytes().to_vec();
        superblob.extend_from_slice(&((offset + blob.len()) as u32).to_be_bytes());
        superblob.extend_from_slice(&1u32.to_be_bytes());
        superblob.extend_from_slice(&CSSLOT_ENTITLEMENTS.to_be_bytes());
        superblob.extend_from_slice(&(offset as u32).to_be_bytes());
        superblob.extend_from_slice(&blob);
        superblob
    }

    #[test]
    fn finds_the_entitlements_blob_by_slot() {
        let plist =
            b"<plist><dict><key>com.apple.security.device.camera</key><true/></dict></plist>";
        let superblob = superblob(plist);
        let blob = blob_in_slot(&superblob, CSSLOT_ENTITLEMENTS).expect("an entitlements blob");
        assert_eq!(&blob[8..], plist);
        assert!(blob_in_slot(&superblob, CSSLOT_ENTITLEMENTS + 1).is_none());
    }

    #[test]
    fn reads_only_entitlement_exists_requirements() {
        objc::bootstrap();
        let read = |text: &str| {
            let text = corefoundation::string::from_bytes(text.as_bytes(), false);
            let mut requirement = std::ptr::null();
            // SAFETY: a string this test made and a writable out-parameter.
            unsafe { SecRequirementCreateWithString(text, 0, &mut requirement) };
            // SAFETY: the handle just made.
            let key = unsafe { required_entitlement(requirement) };
            // SAFETY: two objects this test owns.
            unsafe {
                corefoundation::api::CFRelease(requirement);
                corefoundation::api::CFRelease(text);
            }
            key
        };
        assert_eq!(
            read(r#"entitlement["com.apple.security.app-sandbox"] exists"#).as_deref(),
            Some("com.apple.security.app-sandbox")
        );
        assert_eq!(read("anchor apple generic"), None);
    }
}
