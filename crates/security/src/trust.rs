//! Certificates, the basic X.509 policy, and trust evaluation.
//!
//! A program that runs its own TLS stack hands the server's chain to
//! `SecTrustEvaluate` and connects only if it is trusted. The chain is checked
//! for real - signatures, validity dates, and a path to a root - because an
//! answer of "trusted" for anything would turn every such program into one that
//! accepts any server.

#![allow(non_snake_case)]

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use objc::class::Class;
use objc::object::Id;
use rustls_pki_types::{CertificateDer, UnixTime};

type OSStatus = i32;
const SUCCESS: OSStatus = 0;
/// `errSecParam`.
const PARAM: OSStatus = -50;

/// `SecTrustResultType`.
mod result {
    /// `kSecTrustResultUnspecified`: trusted, with no explicit user setting.
    pub const UNSPECIFIED: u32 = 4;
    /// `kSecTrustResultRecoverableTrustFailure`: untrusted, but well formed.
    pub const RECOVERABLE: u32 = 5;
    /// `kSecTrustResultFatalTrustFailure`: a certificate could not be used at all.
    pub const FATAL: u32 = 6;
}

/// Trust-option flags (including expiration overrides, implicit anchors, and
/// revocation requirements) are not implemented. No nonzero combination may be
/// silently honored or ignored: in particular, ignoring a revocation-required
/// flag would turn a security requirement into a trust bypass.
const UNIMPLEMENTED: OSStatus = -4; // errSecUnimplemented

#[derive(Clone)]
enum Policy {
    /// A chain to a trusted root, without an extended-key-usage requirement.
    BasicX509,
    /// TLS policies require appropriate EKU and, for server authentication,
    /// matching DNS/IP identity when the caller supplied a name.
    Ssl {
        server: bool,
        hostname: Option<String>,
    },
}

enum Payload {
    Certificate(Vec<u8>),
    Policy(Policy),
    Trust {
        chain: Vec<Vec<u8>>,
        options: u32,
        policy: Policy,
        verify_time: Option<UnixTime>,
        custom_anchors: Option<Vec<Vec<u8>>>,
        anchors_only: bool,
        /// A nonempty OCSP response was rejected and has not been validated.
        /// The caller might ignore the setter error; evaluation must fail closed.
        unsupported_ocsp: bool,
    },
}

#[repr(C)]
struct Body {
    isa: *const Class,
    payload: *mut Payload,
}

fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        let class =
            corefoundation::base::register(c"__R3SecTrustObject", None, size_of::<Body>(), true);
        class.replace_method(
            objc::sel::get("dealloc"),
            dealloc as *const c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("a trust object class") }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: objc::sel::Sel) {
    // SAFETY: this class's layout, and the payload `new` boxed.
    unsafe {
        let body = receiver as *mut Body;
        drop(Box::from_raw((*body).payload));
        objc::arc::finalize(receiver);
    }
}

/// A handle holding `payload`, owned by the caller.
fn new(payload: Payload) -> CFTypeRef {
    // SAFETY: `Body` is the registered layout.
    let body = unsafe { corefoundation::base::allocate::<Body>(class()) };
    if body.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a freshly allocated body.
    unsafe { (*body).payload = Box::into_raw(Box::new(payload)) };
    body as CFTypeRef
}

/// The payload of one of these handles, or `None` for anything else.
///
/// # Safety
///
/// `object` is null or an object this runtime allocated, live for `'a`.
unsafe fn payload<'a>(object: CFTypeRef) -> Option<&'a mut Payload> {
    // SAFETY: the caller's guarantee.
    if object.is_null() || !unsafe { corefoundation::base::is_kind_of(object, class()) } {
        return None;
    }
    // SAFETY: a body of this class, whose payload lives as long as it does.
    Some(unsafe { &mut *(*(object as *const Body)).payload })
}

/// `object` itself, or each element if it is a `CFArray`.
///
/// # Safety
///
/// `object` is null or a live CoreFoundation object.
unsafe fn one_or_many(object: CFTypeRef) -> Vec<CFTypeRef> {
    use corefoundation::api::{
        CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex, CFGetTypeID,
    };
    if object.is_null() {
        return Vec::new();
    }
    // SAFETY: a live object, and an array when its type says so.
    unsafe {
        if CFGetTypeID(object) != CFArrayGetTypeID() {
            return vec![object];
        }
        (0..CFArrayGetCount(object))
            .map(|i| CFArrayGetValueAtIndex(object, i))
            .collect()
    }
}

/// `SecCertificateRef SecCertificateCreateWithData(CFAllocatorRef, CFDataRef)`.
///
/// Null unless the bytes are one DER-encoded X.509 certificate, as Apple's.
///
/// # Safety
///
/// `data` is null or a live `CFData`.
pub unsafe extern "C" fn SecCertificateCreateWithData(
    _allocator: CFTypeRef,
    data: CFTypeRef,
) -> CFTypeRef {
    if data.is_null()
        || unsafe { corefoundation::api::CFGetTypeID(data) }
            != unsafe { corefoundation::api::CFDataGetTypeID() }
    {
        return std::ptr::null();
    }
    // SAFETY: this is a live `CFData`, checked above.
    let bytes = unsafe {
        let length = corefoundation::api::CFDataGetLength(data);
        let base = corefoundation::api::CFDataGetBytePtr(data);
        if base.is_null() || length <= 0 {
            return std::ptr::null();
        }
        std::slice::from_raw_parts(base, length as usize).to_vec()
    };
    let der = CertificateDer::from(bytes.as_slice());
    if webpki::EndEntityCert::try_from(&der).is_err() {
        return std::ptr::null();
    }
    new(Payload::Certificate(bytes))
}

/// The host's root certificates, DER-encoded, read once.
///
/// From the bundle the host's own TLS libraries trust, so a program running
/// its own stack trusts the same servers the rest of the machine does.
/// `SSL_CERT_FILE` first, as OpenSSL reads it, then where Debian, Fedora,
/// NixOS and the BSDs keep theirs.
fn host_anchors() -> &'static [Vec<u8>] {
    static ANCHORS: std::sync::OnceLock<Vec<Vec<u8>>> = std::sync::OnceLock::new();
    ANCHORS.get_or_init(|| {
        use rustls_pki_types::pem::PemObject;
        const BUNDLES: &[&str] = &[
            "/etc/ssl/certs/ca-certificates.crt",
            "/etc/pki/tls/certs/ca-bundle.crt",
            "/etc/ssl/ca-bundle.pem",
            "/etc/ssl/cert.pem",
            "/usr/local/share/certs/ca-root-nss.crt",
        ];
        let named = std::env::var_os("SSL_CERT_FILE").map(std::path::PathBuf::from);
        let pem = named
            .into_iter()
            .chain(BUNDLES.iter().map(std::path::PathBuf::from))
            .find_map(|path| std::fs::read(path).ok())
            .unwrap_or_default();
        CertificateDer::pem_slice_iter(&pem)
            .filter_map(Result::ok)
            .filter(|der| webpki::anchor_from_trusted_cert(der).is_ok())
            .map(|der| der.to_vec())
            .collect()
    })
}

/// Parse precisely the same system anchors exposed by
/// `SecTrustCopyAnchorCertificates`; no unrelated Mozilla roots are implicitly
/// trusted if the host's CA store is unavailable or has been restricted.
fn host_trust_anchors() -> &'static [rustls_pki_types::TrustAnchor<'static>] {
    static ROOTS: std::sync::OnceLock<Vec<rustls_pki_types::TrustAnchor<'static>>> =
        std::sync::OnceLock::new();
    ROOTS.get_or_init(|| {
        host_anchors()
            .iter()
            .filter_map(|der| {
                webpki::anchor_from_trusted_cert(&CertificateDer::from(der.as_slice()))
                    .ok()
                    .map(|anchor| anchor.to_owned())
            })
            .collect()
    })
}

/// `OSStatus SecTrustCopyAnchorCertificates(CFArrayRef *anchors)`.
///
/// # Safety
///
/// `anchors` is null or writable.
pub unsafe extern "C" fn SecTrustCopyAnchorCertificates(anchors: *mut CFTypeRef) -> OSStatus {
    if anchors.is_null() {
        return PARAM;
    }
    let certificates: Vec<CFTypeRef> = host_anchors()
        .iter()
        .map(|der| new(Payload::Certificate(der.clone())))
        .collect();
    if certificates.iter().any(|certificate| certificate.is_null()) {
        for certificate in certificates {
            if !certificate.is_null() {
                unsafe { corefoundation::api::CFRelease(certificate) };
            }
        }
        unsafe { anchors.write(std::ptr::null()) };
        return -108; // errSecAllocate; never return a truncated anchor array
    }
    let array = corefoundation::collections::new_array(
        &certificates,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for certificate in certificates {
        // SAFETY: the array took its own reference.
        unsafe { corefoundation::api::CFRelease(certificate) };
    }
    // SAFETY: writable, per the caller.
    unsafe { anchors.write(array) };
    if array.is_null() { -108 } else { SUCCESS }
}

/// The DER bytes of a certificate handle.
///
/// # Safety
///
/// As [`payload`].
unsafe fn der_of<'a>(certificate: CFTypeRef) -> Option<&'a [u8]> {
    // SAFETY: the caller's guarantee.
    match unsafe { payload(certificate) } {
        Some(Payload::Certificate(der)) => Some(der),
        _ => None,
    }
}

/// `CFDataRef SecCertificateCopyData(SecCertificateRef)`.
///
/// # Safety
///
/// `certificate` is null or a live object.
pub unsafe extern "C" fn SecCertificateCopyData(certificate: CFTypeRef) -> CFTypeRef {
    // SAFETY: the caller's guarantee.
    match unsafe { der_of(certificate) } {
        Some(der) => corefoundation::collections::new_data(der.to_vec()),
        None => std::ptr::null(),
    }
}

/// Read one definite-length ASN.1 TLV; never index beyond the encoded input.
/// This is used for an informational subject label, not certificate validation.
fn der_element<'a>(input: &mut &'a [u8]) -> Option<(u8, &'a [u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&length_octet, remaining) = rest.split_first()?;
    let (length, content) = if length_octet & 0x80 == 0 {
        (usize::from(length_octet), remaining)
    } else {
        let count = usize::from(length_octet & 0x7f);
        if count == 0 || count > 4 || count > remaining.len() {
            return None;
        }
        let mut length = 0usize;
        for &byte in &remaining[..count] {
            length = length.checked_mul(256)?.checked_add(byte as usize)?;
        }
        (length, &remaining[count..])
    };
    if length > content.len() {
        return None;
    }
    let (value, rest) = content.split_at(length);
    *input = rest;
    Some((tag, value))
}

/// Extract a conventional Common Name from a webpki-validated X.509 subject.
/// A subject may lack a CN; the API then returns null instead of making one up.
fn certificate_subject_summary(der: &[u8]) -> Option<String> {
    let certificate = CertificateDer::from(der);
    let anchor = webpki::anchor_from_trusted_cert(&certificate).ok()?;
    let mut name: &[u8] = anchor.subject.as_ref();
    // webpki's subject is the Name SEQUENCE payload; accept its tagged form.
    if name.first() == Some(&0x30) {
        let mut outer = name;
        let (tag, inner) = der_element(&mut outer)?;
        if tag != 0x30 || !outer.is_empty() {
            return None;
        }
        name = inner;
    }
    let mut organization = None;
    while !name.is_empty() {
        let (set_tag, mut set) = der_element(&mut name)?;
        if set_tag != 0x31 {
            return None;
        }
        while !set.is_empty() {
            let (sequence_tag, mut ava) = der_element(&mut set)?;
            if sequence_tag != 0x30 {
                return None;
            }
            let (oid_tag, oid) = der_element(&mut ava)?;
            let (value_tag, value) = der_element(&mut ava)?;
            if oid_tag != 0x06 || !ava.is_empty() {
                return None;
            }
            let text = match value_tag {
                // UTF8String, PrintableString and TeletexString.
                0x0c | 0x13 | 0x14 => String::from_utf8(value.to_vec()).ok(),
                0x1e if value.len() % 2 == 0 => {
                    let utf16: Vec<u16> = value
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| u16::from_be_bytes(*b))
                        .collect();
                    String::from_utf16(&utf16).ok()
                }
                _ => None,
            };
            if oid == [0x55, 0x04, 0x03] {
                // 2.5.4.3 commonName
                return text.filter(|s| !s.is_empty());
            }
            if oid == [0x55, 0x04, 0x0a] && organization.is_none() {
                // organizationName
                organization = text.filter(|s| !s.is_empty());
            }
        }
    }
    organization
}

/// `CFStringRef SecCertificateCopySubjectSummary(SecCertificateRef certificate)`.
///
/// # Safety
/// `certificate` must be a live SecCertificateRef.
pub unsafe extern "C" fn SecCertificateCopySubjectSummary(certificate: CFTypeRef) -> CFTypeRef {
    let Some(der) = (unsafe { der_of(certificate) }) else {
        return std::ptr::null();
    };
    let Some(summary) = certificate_subject_summary(der) else {
        return std::ptr::null();
    };
    corefoundation::string::from_bytes(summary.as_bytes(), false)
}

/// `OSStatus SecCertificateGetData(SecCertificateRef, CSSM_DATA *)`.
///
/// `CSSM_DATA` is a length and a pointer, `size_t` wide on LP64. The pointer is
/// into the certificate, which is what Apple hands back too: valid for as long
/// as the caller holds the certificate.
///
/// # Safety
///
/// `certificate` is null or a live object, and `data` is null or writable.
pub unsafe extern "C" fn SecCertificateGetData(
    certificate: CFTypeRef,
    data: *mut (usize, *const u8),
) -> OSStatus {
    // SAFETY: the caller's guarantee.
    let Some(der) = (unsafe { der_of(certificate) }) else {
        return PARAM;
    };
    if data.is_null() {
        return PARAM;
    }
    // SAFETY: writable, per the caller.
    unsafe { data.write((der.len(), der.as_ptr())) };
    SUCCESS
}

/// `SecPolicyRef SecPolicyCreateBasicX509(void)`.
///
/// # Safety
///
/// None beyond being called as the C function it is.
pub unsafe extern "C" fn SecPolicyCreateBasicX509() -> CFTypeRef {
    new(Payload::Policy(Policy::BasicX509))
}

/// `SecPolicyRef SecPolicyCreateSSL(Boolean server, CFStringRef hostname)`.
/// A named TLS server is verified for both hostname and server-auth usage.
/// Null hostname still enforces the TLS extended-key-usage restriction.
///
/// # Safety
/// `hostname` must be null or a live CFString.
pub unsafe extern "C" fn SecPolicyCreateSSL(server: u8, hostname: CFTypeRef) -> CFTypeRef {
    let name = if hostname.is_null() {
        None
    } else {
        let Some(name) = (unsafe { corefoundation::string::as_string(hostname) }) else {
            return std::ptr::null();
        };
        // Do not substitute replacement characters for an invalid hostname.
        // The actual identity is independently validated by webpki.
        let Ok(name) = String::from_utf16(&name.utf16()) else {
            return std::ptr::null();
        };
        Some(name)
    };
    new(Payload::Policy(Policy::Ssl {
        server: server != 0,
        hostname: name,
    }))
}

/// `OSStatus SecTrustCreateWithCertificates(CFTypeRef certificates, CFTypeRef
/// policies, SecTrustRef *trust)`.
///
/// `certificates` is one certificate or an array, the leaf first. Only one
/// basic X.509 or SSL policy is supported; unsupported policies fail closed.
///
/// # Safety
///
/// Both are null or live objects, and `trust` is null or writable.
pub unsafe extern "C" fn SecTrustCreateWithCertificates(
    certificates: CFTypeRef,
    policies: CFTypeRef,
    trust: *mut CFTypeRef,
) -> OSStatus {
    if trust.is_null() {
        return PARAM;
    }
    // A failed constructor must not leave an old trust handle in an out-param.
    unsafe { trust.write(std::ptr::null()) };
    let mut chain = Vec::new();
    // SAFETY: the caller's guarantee.
    for certificate in unsafe { one_or_many(certificates) } {
        // SAFETY: an element of a live array, or the object itself.
        match unsafe { payload(certificate) } {
            Some(Payload::Certificate(der)) => chain.push(der.clone()),
            _ => return PARAM,
        }
    }
    // Apple SecTrustCreateWithCertificates permits a NULL policies argument
    // and chooses the Basic X.509 policy by default. An explicitly empty
    // policy array is not equivalent to that caller-selected default.
    let mut selected = if policies.is_null() {
        Some(Policy::BasicX509)
    } else {
        None
    };
    // SAFETY: the caller's guarantee.
    for candidate in unsafe { one_or_many(policies) } {
        // SAFETY: a live policy handle from the caller.
        let Some(Payload::Policy(policy)) = (unsafe { payload(candidate) }) else {
            return PARAM;
        };
        if selected.replace(policy.clone()).is_some() {
            return PARAM; // Combined policies need conjunction, not an arbitrary first.
        }
    }
    let Some(policy) = selected else { return PARAM };
    if chain.is_empty() {
        return PARAM;
    }
    // SAFETY: writable, checked above.
    let created = new(Payload::Trust {
        chain,
        options: 0,
        policy,
        verify_time: None,
        custom_anchors: None,
        anchors_only: false,
        unsupported_ocsp: false,
    });
    if created.is_null() {
        return -108; // errSecAllocate
    }
    unsafe { trust.write(created) };
    SUCCESS
}

/// `OSStatus SecTrustSetOptions(SecTrustRef, SecTrustOptionFlags)`.
///
/// Nonzero flags cannot be modeled safely: permitting `RequireRevPerCert`
/// without revocation checking, or synthesizing validity time for `AllowExpired`,
/// would lead to false trust. Remember the unimplemented state even if the
/// caller discards the returned status; setting flags back to zero restores
/// ordinary evaluation.
///
/// # Safety
///
/// `trust` is null or a live object.
pub unsafe extern "C" fn SecTrustSetOptions(trust: CFTypeRef, flags: u32) -> OSStatus {
    // SAFETY: the caller's guarantee.
    match unsafe { payload(trust) } {
        Some(Payload::Trust { options, .. }) => {
            *options = flags;
            if flags == 0 { SUCCESS } else { UNIMPLEMENTED }
        }
        _ => PARAM,
    }
}

/// `OSStatus SecTrustSetVerifyDate(SecTrustRef trust, CFDateRef verifyDate)`.
/// The override affects actual certificate validity verification, never the
/// system clock or other connections.
///
/// # Safety
/// `trust` must be a live trust handle, `date` a CFDate.
pub unsafe extern "C" fn SecTrustSetVerifyDate(trust: CFTypeRef, date: CFTypeRef) -> OSStatus {
    // Apple's SecTrustSetVerifyDate allows NULL to clear an earlier date.
    // Do not reinterpret it as January 1970 or reject a valid reset.
    let time = if date.is_null() {
        None
    } else {
        let Some(timestamp) = foundation::timer::interval_since_1970(date as Id) else {
            return PARAM;
        };
        if !timestamp.is_finite() || !(0.0..=253402300799.0).contains(&timestamp) {
            return PARAM;
        }
        Some(UnixTime::since_unix_epoch(
            std::time::Duration::from_secs_f64(timestamp),
        ))
    };
    // SAFETY: a live trust handle from the caller.
    match unsafe { payload(trust) } {
        Some(Payload::Trust { verify_time, .. }) => {
            *verify_time = time;
            SUCCESS
        }
        _ => PARAM,
    }
}

/// `OSStatus SecTrustSetAnchorCertificates(SecTrustRef, CFArrayRef)`.
/// Validate caller-supplied root certificates before permitting them as trust
/// anchors; they do not implicitly add a trust exception for arbitrary chains.
///
/// # Safety
/// `anchors` must be null or a live CFArray of SecCertificateRef handles.
pub unsafe extern "C" fn SecTrustSetAnchorCertificates(
    trust: CFTypeRef,
    anchors: CFTypeRef,
) -> OSStatus {
    // Apple's argument is a CFArray, not a single SecCertificateRef.
    if !anchors.is_null()
        && unsafe { corefoundation::api::CFGetTypeID(anchors) }
            != unsafe { corefoundation::api::CFArrayGetTypeID() }
    {
        return PARAM;
    }
    let mut der = Vec::new();
    // SAFETY: the caller provides an array of live certificates.
    for cert in unsafe { one_or_many(anchors) } {
        let Some(Payload::Certificate(bytes)) = (unsafe { payload(cert) }) else {
            return PARAM;
        };
        if webpki::anchor_from_trusted_cert(&CertificateDer::from(bytes.as_slice())).is_err() {
            return PARAM;
        }
        der.push(bytes.clone());
    }
    // SAFETY: the caller provides a live trust handle.
    match unsafe { payload(trust) } {
        Some(Payload::Trust {
            custom_anchors,
            anchors_only,
            ..
        }) => {
            if anchors.is_null() {
                // Apple's NULL argument restores the default anchor set.
                // An explicitly empty array instead requests an empty set.
                *custom_anchors = None;
                *anchors_only = false;
            } else {
                *custom_anchors = Some(der);
                *anchors_only = true; // Default after supplying custom anchors.
            }
            SUCCESS
        }
        _ => PARAM,
    }
}

/// `OSStatus SecTrustSetAnchorCertificatesOnly(SecTrustRef, Boolean)`.
/// When false, built-in roots remain usable alongside caller-provided roots.
///
/// # Safety
/// `trust` must be a live trust handle.
pub unsafe extern "C" fn SecTrustSetAnchorCertificatesOnly(trust: CFTypeRef, only: u8) -> OSStatus {
    // SAFETY: a live trust handle from the caller.
    match unsafe { payload(trust) } {
        Some(Payload::Trust { anchors_only, .. }) => {
            *anchors_only = only != 0;
            SUCCESS
        }
        _ => PARAM,
    }
}

/// `OSStatus SecTrustSetOCSPResponse(SecTrustRef, CFTypeRef response)`.
///
/// A non-null response is a caller-supplied cryptographic assertion, not an
/// assertion we can honor until the responder signature, signer authorization,
/// CertID, thisUpdate/nextUpdate and revocation status are checked. This subset
/// therefore returns errSecUnimplemented for any non-null input and makes this
/// trust permanently untrusted, even if the caller ignores that status or
/// later replaces the response with NULL. This deliberately prevents treating
/// malformed, empty or unsigned OCSP evidence as proof of good status.
/// A NULL response on a fresh trust means no stapled evidence was provided;
/// path validation then follows the default offline policy (no affirmative
/// revocation guarantee). No OCSP network calls are attempted.
///
/// # Safety
/// `response` is null or a live CoreFoundation object.
pub unsafe extern "C" fn SecTrustSetOCSPResponse(
    trust: CFTypeRef,
    response: CFTypeRef,
) -> OSStatus {
    // SAFETY: a live trust handle from the caller.
    match unsafe { payload(trust) } {
        Some(Payload::Trust {
            unsupported_ocsp, ..
        }) => {
            if response.is_null() {
                return SUCCESS;
            }
            // Even a malformed invocation must poison this trust when a caller
            // ignores the setter's error and goes on to evaluate it.
            *unsupported_ocsp = true;
            // Distinguish an unsupported (well-typed) OCSP input from an
            // invalid parameter. Neither may be treated as validated OCSP.
            let data_type = unsafe { corefoundation::api::CFDataGetTypeID() };
            let kind = unsafe { corefoundation::api::CFGetTypeID(response) };
            let items = if kind == data_type {
                vec![response]
            } else if kind == unsafe { corefoundation::api::CFArrayGetTypeID() } {
                unsafe { one_or_many(response) }
            } else {
                return PARAM;
            };
            if items.is_empty()
                || items.iter().any(|&item| {
                    item.is_null()
                        || unsafe { corefoundation::api::CFGetTypeID(item) } != data_type
                        || unsafe { corefoundation::api::CFDataGetLength(item) } <= 0
                })
            {
                PARAM
            } else {
                UNIMPLEMENTED
            }
        }
        _ => PARAM,
    }
}

/// `CFStringRef SecCopyErrorMessageString(OSStatus status, void *reserved)`.
/// Apple returns a retained string for recognized Security OSStatus errors.
///
/// # Safety
/// The `reserved` pointer is unused and must be null by API contract.
pub unsafe extern "C" fn SecCopyErrorMessageString(
    status: OSStatus,
    _reserved: *const c_void,
) -> CFTypeRef {
    let message = match status {
        SUCCESS => "No error".to_string(),
        PARAM => "One or more parameters passed to the function were not valid".to_string(),
        UNIMPLEMENTED => {
            "The requested Security framework operation is not implemented".to_string()
        }
        -67843 => "The certificate could not be verified".to_string(),
        -108 => "Memory allocation failed".to_string(),
        _ => return std::ptr::null(), // Apple's unknown OSStatus has no description.
    };
    corefoundation::string::from_bytes(message.as_bytes(), false)
}

/// The basic policy's extended key usage check, which is none: a certificate
/// restricted to some purpose is still a valid X.509 certificate.
struct AnyUsage;

impl webpki::ExtendedKeyUsageValidator for AnyUsage {
    fn validate(&self, _iter: webpki::KeyPurposeIdIter<'_, '_>) -> Result<(), webpki::Error> {
        Ok(())
    }
}

/// Evaluate a signature-validated path to the host trust store (or explicit
/// anchors), enforce EKU and DNS/IP identity, and check all certificate dates.
/// The host store differs from macOS Keychain trust settings; this emulator
/// intentionally does not invent per-user trust settings or revocation evidence.
fn evaluate(
    chain: &[Vec<u8>],
    options: u32,
    policy: &Policy,
    verify_time: Option<UnixTime>,
    custom_anchors: Option<&[Vec<u8>]>,
    anchors_only: bool,
    unsupported_ocsp: bool,
) -> u32 {
    if unsupported_ocsp || options != 0 {
        return result::RECOVERABLE;
    }
    let Some((leaf, rest)) = chain.split_first() else {
        return result::FATAL;
    };
    let leaf = CertificateDer::from(leaf.as_slice());
    let Ok(end_entity) = webpki::EndEntityCert::try_from(&leaf) else {
        return result::FATAL;
    };
    let intermediates: Vec<_> = rest
        .iter()
        .map(|der| CertificateDer::from(der.as_slice()))
        .collect();
    let parsed_anchors: Vec<_> = custom_anchors
        .unwrap_or_default()
        .iter()
        .map(|der| CertificateDer::from(der.as_slice()))
        .collect();
    // The caller's `anchors_only` flag excludes system roots even when the
    // caller never supplied any custom anchors (or subsequently reset them).
    // An empty custom set combined with `only=true` means no trust anchors.
    let mut roots = if anchors_only {
        Vec::new()
    } else {
        host_trust_anchors().to_vec()
    };
    for der in &parsed_anchors {
        let Ok(anchor) = webpki::anchor_from_trusted_cert(der) else {
            return result::FATAL;
        };
        roots.push(anchor);
    }
    if roots.is_empty() {
        return result::RECOVERABLE;
    }
    let time = verify_time.unwrap_or_else(UnixTime::now);
    macro_rules! verify {
        ($usage:expr) => {
            end_entity.verify_for_usage(
                webpki::ALL_VERIFICATION_ALGS,
                &roots,
                &intermediates,
                time,
                $usage,
                None,
                None,
            )
        };
    }
    let verified = match policy {
        Policy::BasicX509 => verify!(AnyUsage),
        Policy::Ssl { server: true, .. } => verify!(webpki::KeyUsage::server_auth()),
        Policy::Ssl { server: false, .. } => verify!(webpki::KeyUsage::client_auth()),
    };
    if verified.is_err() {
        return result::RECOVERABLE;
    }
    if let Policy::Ssl {
        hostname: Some(name),
        ..
    } = policy
    {
        let Ok(name) = rustls_pki_types::ServerName::try_from(name.as_str()) else {
            return result::RECOVERABLE;
        };
        if end_entity.verify_is_valid_for_subject_name(&name).is_err() {
            return result::RECOVERABLE;
        }
    }
    result::UNSPECIFIED
}

/// `OSStatus SecTrustEvaluate(SecTrustRef, SecTrustResultType *)`.
///
/// # Safety
///
/// `trust` is null or a live object, and `out` is null or writable.
pub unsafe extern "C" fn SecTrustEvaluate(trust: CFTypeRef, out: *mut u32) -> OSStatus {
    if out.is_null() {
        return PARAM;
    }
    // SAFETY: the caller's guarantee.
    let Some(Payload::Trust {
        chain,
        options,
        policy,
        verify_time,
        custom_anchors,
        anchors_only,
        unsupported_ocsp,
    }) = (unsafe { payload(trust) })
    else {
        return PARAM;
    };
    let verdict = evaluate(
        chain,
        *options,
        policy,
        *verify_time,
        custom_anchors.as_deref(),
        *anchors_only,
        *unsupported_ocsp,
    );
    // SAFETY: non-null writable result pointer checked above.
    unsafe { out.write(verdict) };
    SUCCESS
}

/// `Boolean SecTrustEvaluateWithError(SecTrustRef, CFErrorRef *error)`.
/// A successful evaluation enforces certificate path, usage, date and (for
/// named SSL servers) hostname. Untrusted certificates never yield true.
///
/// # Safety
/// The trust is live and `error` is null or a writable output pointer.
pub unsafe extern "C" fn SecTrustEvaluateWithError(trust: CFTypeRef, error: *mut CFTypeRef) -> u8 {
    if !error.is_null() {
        unsafe { error.write(std::ptr::null()) };
    }
    let Some(Payload::Trust {
        chain,
        options,
        policy,
        verify_time,
        custom_anchors,
        anchors_only,
        unsupported_ocsp,
    }) = (unsafe { payload(trust) })
    else {
        return 0;
    };
    if evaluate(
        chain,
        *options,
        policy,
        *verify_time,
        custom_anchors.as_deref(),
        *anchors_only,
        *unsupported_ocsp,
    ) == result::UNSPECIFIED
    {
        return 1;
    }
    if !error.is_null() {
        let description =
            "The certificate chain, TLS purpose, name, or validity period could not be verified";
        unsafe {
            error.write(
                foundation::error::create("NSOSStatusErrorDomain", -67843, description, &[])
                    as CFTypeRef,
            )
        };
    }
    0
}

/// `int SecRandomCopyBytes(SecRandomRef, size_t, void *)`. The generator
/// argument is `kSecRandomDefault` - null - in every caller.
///
/// # Safety
///
/// `bytes` is writable for `count` bytes.
pub unsafe extern "C" fn SecRandomCopyBytes(_rnd: CFTypeRef, count: usize, bytes: *mut u8) -> i32 {
    if count == 0 {
        return 0;
    }
    if bytes.is_null() {
        return -1;
    }
    // SAFETY: the caller's guarantee.
    let buffer = unsafe { std::slice::from_raw_parts_mut(bytes, count) };
    match getrandom::fill(buffer) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

exports::exports! {
    functions {
        "SecCertificateCreateWithData" => SecCertificateCreateWithData,
        "SecPolicyCreateBasicX509" => SecPolicyCreateBasicX509,
        "SecPolicyCreateSSL" => SecPolicyCreateSSL,
        "SecTrustSetVerifyDate" => SecTrustSetVerifyDate,
        "SecTrustSetOCSPResponse" => SecTrustSetOCSPResponse,
        "SecCopyErrorMessageString" => SecCopyErrorMessageString,
        "SecTrustEvaluateWithError" => SecTrustEvaluateWithError,
        "SecTrustSetAnchorCertificates" => SecTrustSetAnchorCertificates,
        "SecTrustSetAnchorCertificatesOnly" => SecTrustSetAnchorCertificatesOnly,
        "SecTrustCreateWithCertificates" => SecTrustCreateWithCertificates,
        "SecTrustSetOptions" => SecTrustSetOptions,
        "SecTrustEvaluate" => SecTrustEvaluate,
        "SecRandomCopyBytes" => SecRandomCopyBytes,
        "SecTrustCopyAnchorCertificates" => SecTrustCopyAnchorCertificates,
        "SecCertificateCopyData" => SecCertificateCopyData,
        "SecCertificateCopySubjectSummary" => SecCertificateCopySubjectSummary,
        "SecCertificateGetData" => SecCertificateGetData,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every anchor reads back as the bytes it was made from, through both
    /// spellings of the question.
    #[test]
    fn anchors_read_back_as_their_der() {
        let mut array: CFTypeRef = std::ptr::null();
        assert_eq!(
            unsafe { SecTrustCopyAnchorCertificates(&mut array) },
            SUCCESS
        );
        let count = unsafe { corefoundation::api::CFArrayGetCount(array) };
        if count == 0 {
            // No host CA store is not a trust success; evaluation will reject
            // absent an explicitly configured caller-provided root.
            unsafe { corefoundation::api::CFRelease(array) };
            return;
        }
        let first = unsafe { corefoundation::api::CFArrayGetValueAtIndex(array, 0) };
        let data = unsafe { SecCertificateCopyData(first) };
        let length = unsafe { corefoundation::api::CFDataGetLength(data) } as usize;
        let mut cssm = (0usize, std::ptr::null::<u8>());
        assert_eq!(unsafe { SecCertificateGetData(first, &mut cssm) }, SUCCESS);
        assert_eq!(cssm.0, length);
        assert_eq!(host_anchors()[0].len(), length);
        unsafe {
            corefoundation::api::CFRelease(data);
            corefoundation::api::CFRelease(array);
        }
    }

    /// A certificate that chains to nothing: well formed, so it is accepted as a
    /// certificate, but evaluation must not call it trusted.
    #[test]
    fn a_self_signed_certificate_is_not_trusted() {
        objc::bootstrap();
        let der = self_signed();
        let data = data(&der);
        let mut trust = std::ptr::null();
        let mut verdict = 0;
        // SAFETY: objects this test made, and writable out-parameters.
        unsafe {
            let certificate = SecCertificateCreateWithData(std::ptr::null(), data);
            assert!(!certificate.is_null(), "a DER certificate is accepted");
            let policy = SecPolicyCreateBasicX509();
            assert_eq!(
                SecTrustCreateWithCertificates(certificate, policy, &mut trust),
                SUCCESS
            );
            assert_eq!(SecTrustSetOptions(trust, 0), SUCCESS);
            assert_eq!(SecTrustEvaluate(trust, &mut verdict), SUCCESS);
            assert_eq!(verdict, result::RECOVERABLE);
            for object in [trust, policy, certificate, data] {
                corefoundation::api::CFRelease(object);
            }
        }
    }

    /// Trust evaluation must not be able to accept a self-signed certificate
    /// simply because its caller supplied an SSL hostname, an explicit date,
    /// or a failed stapled-OCSP response.
    #[test]
    fn ssl_trust_rejects_untrusted_chain_and_preserves_rejection() {
        foundation::bootstrap();
        let der = self_signed();
        let source = data(&der);
        let hostname = corefoundation::string::from_bytes(b"example.com", false);
        // SAFETY: every object here is a valid CoreFoundation handle created above.
        unsafe {
            let certificate = SecCertificateCreateWithData(std::ptr::null(), source);
            let policy = SecPolicyCreateSSL(1, hostname);
            assert!(!certificate.is_null());
            assert!(!policy.is_null());
            let mut trust = std::ptr::null();
            assert_eq!(
                SecTrustCreateWithCertificates(certificate, policy, &mut trust),
                SUCCESS
            );
            assert_eq!(
                SecTrustSetVerifyDate(
                    trust,
                    foundation::timer::date_since_1970(1_800_000_000.0) as _
                ),
                SUCCESS
            );
            let stapled = data(b"invalid stapled OCSP evidence");
            assert_eq!(SecTrustSetOCSPResponse(trust, stapled), -4);
            let mut error = std::ptr::null();
            assert_eq!(SecTrustEvaluateWithError(trust, &mut error), 0);
            assert!(!error.is_null());
            let mut verdict = 0;
            assert_eq!(SecTrustEvaluate(trust, &mut verdict), SUCCESS);
            assert_ne!(verdict, result::UNSPECIFIED);
            for object in [certificate, policy, trust, hostname, source, stapled, error] {
                corefoundation::api::CFRelease(object);
            }
        }
    }

    /// macOS's SecTrust.c constructs SecPolicyCreateBasicX509() when the
    /// `policies` parameter is NULL; rejecting it was an ABI incompatibility.
    #[test]
    fn null_date_restores_normal_verification_time() {
        foundation::bootstrap();
        let cert_data = data(&self_signed());
        unsafe {
            let cert = SecCertificateCreateWithData(std::ptr::null(), cert_data);
            let policy = SecPolicyCreateBasicX509();
            let mut trust = std::ptr::null();
            assert_eq!(
                SecTrustCreateWithCertificates(cert, policy, &mut trust),
                SUCCESS
            );
            let date = foundation::timer::date_since_1970(1_800_000_000.0);
            assert_eq!(SecTrustSetVerifyDate(trust, date as _), SUCCESS);
            if let Some(Payload::Trust { verify_time, .. }) = payload(trust) {
                assert!(verify_time.is_some());
            } else {
                panic!("expected trust");
            }
            assert_eq!(SecTrustSetVerifyDate(trust, std::ptr::null()), SUCCESS);
            if let Some(Payload::Trust { verify_time, .. }) = payload(trust) {
                assert!(verify_time.is_none());
            } else {
                panic!("expected trust");
            }
            for obj in [cert, policy, trust, cert_data, date as CFTypeRef] {
                corefoundation::api::CFRelease(obj);
            }
        }
    }

    #[test]
    fn a_null_policy_selects_basic_x509_without_trusting_the_certificate() {
        foundation::bootstrap();
        let bytes = data(&self_signed());
        // SAFETY: owned, live test objects and writable trust pointer.
        unsafe {
            let cert = SecCertificateCreateWithData(std::ptr::null(), bytes);
            assert!(!cert.is_null());
            let mut trust = std::ptr::null();
            assert_eq!(
                SecTrustCreateWithCertificates(cert, std::ptr::null(), &mut trust),
                SUCCESS
            );
            assert!(!trust.is_null());
            let mut verdict = 0;
            assert_eq!(SecTrustEvaluate(trust, &mut verdict), SUCCESS);
            assert_ne!(verdict, result::UNSPECIFIED);
            corefoundation::api::CFRelease(trust);
            corefoundation::api::CFRelease(cert);
            corefoundation::api::CFRelease(bytes);
        }
    }

    #[test]
    fn invalid_ssl_policy_and_bad_anchors_fail_closed() {
        foundation::bootstrap();
        // SAFETY: every argument here is an owned CF object or a null value.
        unsafe {
            let non_string = data(b"not a CFString");
            assert!(SecPolicyCreateSSL(1, non_string).is_null());
            let not_data = corefoundation::string::from_bytes(b"not CFData", false);
            assert!(SecCertificateCreateWithData(std::ptr::null(), not_data).is_null());
            let certificate_data = data(&self_signed());
            let certificate = SecCertificateCreateWithData(std::ptr::null(), certificate_data);
            let policy = SecPolicyCreateBasicX509();
            let mut trust = std::ptr::null();
            assert_eq!(
                SecTrustCreateWithCertificates(certificate, policy, &mut trust),
                SUCCESS
            );
            let invalid_anchor = corefoundation::collections::new_array(
                &[non_string],
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            );
            assert_eq!(SecTrustSetAnchorCertificates(trust, invalid_anchor), PARAM);
            let date_before_epoch = foundation::timer::date_since_1970(-1.0);
            assert_eq!(SecTrustSetVerifyDate(trust, date_before_epoch as _), PARAM);
            let mut verdict = 0;
            assert_eq!(SecTrustEvaluate(trust, &mut verdict), SUCCESS);
            assert_ne!(verdict, result::UNSPECIFIED);
            for object in [
                non_string,
                not_data,
                certificate_data,
                certificate,
                policy,
                trust,
                invalid_anchor,
            ] {
                corefoundation::api::CFRelease(object);
            }
        }
    }

    /// A real CA-signed server chain succeeds only under its designated root,
    /// must match the requested DNS name, and cannot accept unsupported OCSP
    /// data even when a caller ignores the OCSP setter's error status.
    #[test]
    fn trusted_tls_chain_enforces_hostname_ocsp_and_anchor_reset() {
        foundation::bootstrap();
        let source = data(include_bytes!("testdata/tls_unit_leaf.der"));
        let root_data = data(include_bytes!("testdata/tls_unit_ca.der"));
        let hostname = corefoundation::string::from_bytes(b"studio.example.test", false);
        let wrong_name = corefoundation::string::from_bytes(b"other.example.test", false);
        // SAFETY: all inputs are live CF objects, and trust out parameters are writable.
        unsafe {
            let certificate = SecCertificateCreateWithData(std::ptr::null(), source);
            let ca = SecCertificateCreateWithData(std::ptr::null(), root_data);
            assert!(!certificate.is_null() && !ca.is_null());
            let policy = SecPolicyCreateSSL(1, hostname);
            let mut trust = std::ptr::null();
            assert_eq!(
                SecTrustCreateWithCertificates(certificate, policy, &mut trust),
                SUCCESS
            );
            // The verification time is explicit so this test is independent of
            // wall-clock changes on the build machine.
            let date = foundation::timer::date_since_1970(1_800_000_000.0);
            assert_eq!(SecTrustSetVerifyDate(trust, date as _), SUCCESS);
            // The fixture root is deliberately not a system-trusted CA.
            assert_eq!(SecTrustEvaluateWithError(trust, std::ptr::null_mut()), 0);
            let anchors = corefoundation::collections::new_array(
                &[ca],
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            );
            assert_eq!(SecTrustSetAnchorCertificates(trust, anchors), SUCCESS);
            assert_eq!(SecTrustEvaluateWithError(trust, std::ptr::null_mut()), 1);

            let bad_policy = SecPolicyCreateSSL(1, wrong_name);
            let mut bad_trust = std::ptr::null();
            assert_eq!(
                SecTrustCreateWithCertificates(certificate, bad_policy, &mut bad_trust),
                SUCCESS
            );
            assert_eq!(SecTrustSetAnchorCertificates(bad_trust, anchors), SUCCESS);
            assert_eq!(
                SecTrustEvaluateWithError(bad_trust, std::ptr::null_mut()),
                0
            );

            let ocsp = data(b"arbitrary unvalidated stapled OCSP response");
            assert_eq!(SecTrustSetOCSPResponse(trust, ocsp), -4);
            assert_eq!(SecTrustEvaluateWithError(trust, std::ptr::null_mut()), 0);
            // A NULL/empty follow-up cannot reset the fail-closed OCSP marker.
            assert_eq!(SecTrustSetOCSPResponse(trust, std::ptr::null()), SUCCESS);
            assert_eq!(SecTrustEvaluateWithError(trust, std::ptr::null_mut()), 0);

            assert_eq!(
                SecTrustSetAnchorCertificates(trust, std::ptr::null()),
                SUCCESS
            );
            let Some(Payload::Trust {
                custom_anchors,
                anchors_only,
                ..
            }) = payload(trust)
            else {
                panic!("expected an emulated SecTrust object");
            };
            assert!(
                custom_anchors.is_none() && !*anchors_only,
                "NULL restores defaults"
            );
            // Unlike NULL, a provided empty array is an explicit empty anchor set.
            let empty = corefoundation::collections::new_array(
                &[],
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            );
            assert_eq!(SecTrustSetAnchorCertificates(trust, empty), SUCCESS);
            let Some(Payload::Trust {
                custom_anchors,
                anchors_only,
                ..
            }) = payload(trust)
            else {
                panic!("expected an emulated SecTrust object");
            };
            assert!(custom_anchors.as_ref().is_some_and(Vec::is_empty) && *anchors_only);
            assert!(SecCopyErrorMessageString(-1234567, std::ptr::null()).is_null());
            for object in [
                source,
                root_data,
                hostname,
                wrong_name,
                certificate,
                ca,
                policy,
                bad_policy,
                trust,
                bad_trust,
                anchors,
                empty,
                ocsp,
            ] {
                corefoundation::api::CFRelease(object);
            }
        }
    }

    /// Pinning a root is an explicit caller authorization, not a way to skip
    /// checking the leaf's signature, valid times, intended usage or hostname.
    /// All timestamps are fixed, and the PKI fixture is checked into the tree.
    #[test]
    fn tls_policy_enforces_signature_time_eku_and_name() {
        let good_leaf = include_bytes!("testdata/tls_unit_leaf.der").to_vec();
        let root = include_bytes!("testdata/tls_unit_ca.der").to_vec();
        let anchor = [root];
        let anchors = Some(anchor.as_slice());
        let named_ssl = Policy::Ssl {
            server: true,
            hostname: Some("studio.example.test".into()),
        };
        let at =
            |timestamp: u64| UnixTime::since_unix_epoch(std::time::Duration::from_secs(timestamp));
        let check = |leaf: Vec<u8>, policy: &Policy, date| {
            evaluate(&[leaf], 0, policy, Some(date), anchors, true, false)
        };
        assert_eq!(
            check(good_leaf.clone(), &named_ssl, at(1_800_000_000)),
            result::UNSPECIFIED
        );
        assert_eq!(
            check(good_leaf.clone(), &named_ssl, at(1_600_000_000)),
            result::RECOVERABLE,
            "not yet valid"
        );
        assert_eq!(
            check(good_leaf.clone(), &named_ssl, at(5_000_000_000)),
            result::RECOVERABLE,
            "expired"
        );
        let wrong_name = Policy::Ssl {
            server: true,
            hostname: Some("other.example.test".into()),
        };
        assert_eq!(
            check(good_leaf.clone(), &wrong_name, at(1_800_000_000)),
            result::RECOVERABLE
        );
        let wrong_ip = Policy::Ssl {
            server: true,
            hostname: Some("127.0.0.1".into()),
        };
        assert_eq!(
            check(good_leaf.clone(), &wrong_ip, at(1_800_000_000)),
            result::RECOVERABLE
        );
        let invalid_name = Policy::Ssl {
            server: true,
            hostname: Some("not a DNS name".into()),
        };
        assert_eq!(
            check(good_leaf.clone(), &invalid_name, at(1_800_000_000)),
            result::RECOVERABLE
        );
        let client = Policy::Ssl {
            server: false,
            hostname: None,
        };
        assert_eq!(
            check(good_leaf.clone(), &client, at(1_800_000_000)),
            result::RECOVERABLE,
            "a serverAuth-only certificate cannot act as a TLS client"
        );
        // Alter the RSA/ECDSA signature without altering parseable TBSCertificate.
        let mut forged = good_leaf.clone();
        let last = forged.last_mut().unwrap();
        *last ^= 0x40;
        assert_eq!(
            check(forged, &named_ssl, at(1_800_000_000)),
            result::RECOVERABLE,
            "mutating the signature must invalidate the path"
        );
        // A policy that requires revocation cannot silently succeed without OCSP.
        assert_eq!(
            evaluate(
                &[good_leaf.clone()],
                0x10, // kSecTrustOptionRequireRevPerCert
                &named_ssl,
                Some(at(1_800_000_000)),
                anchors,
                true,
                false
            ),
            result::RECOVERABLE
        );
        assert_eq!(
            evaluate(
                &[good_leaf],
                0,
                &named_ssl,
                Some(at(1_800_000_000)),
                None,
                true,
                false
            ),
            result::RECOVERABLE,
            "host roots do not include this fixture root"
        );
    }

    /// A rejected setter must not permit trust on a subsequent evaluation.
    #[test]
    fn malformed_ocsp_and_unsupported_trust_options_poison_evaluation() {
        foundation::bootstrap();
        let leaf_data = data(include_bytes!("testdata/tls_unit_leaf.der"));
        let root_data = data(include_bytes!("testdata/tls_unit_ca.der"));
        let name = corefoundation::string::from_bytes(b"studio.example.test", false);
        // SAFETY: the arguments below are owned, valid CF objects.
        unsafe {
            let leaf = SecCertificateCreateWithData(std::ptr::null(), leaf_data);
            let root = SecCertificateCreateWithData(std::ptr::null(), root_data);
            let policy = SecPolicyCreateSSL(1, name);
            let mut trust = std::ptr::null();
            assert_eq!(
                SecTrustCreateWithCertificates(leaf, policy, &mut trust),
                SUCCESS
            );
            let anchors = corefoundation::collections::new_array(
                &[root],
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            );
            assert_eq!(SecTrustSetAnchorCertificates(trust, anchors), SUCCESS);
            let fixed_date = foundation::timer::date_since_1970(1_800_000_000.0);
            assert_eq!(SecTrustSetVerifyDate(trust, fixed_date as _), SUCCESS);
            assert_eq!(SecTrustEvaluateWithError(trust, std::ptr::null_mut()), 1);
            assert_eq!(SecTrustSetOptions(trust, 0x10), UNIMPLEMENTED);
            assert_eq!(SecTrustEvaluateWithError(trust, std::ptr::null_mut()), 0);
            assert_eq!(SecTrustSetOptions(trust, 0), SUCCESS);
            assert_eq!(SecTrustEvaluateWithError(trust, std::ptr::null_mut()), 1);
            // Empty DER and wrong CF types are both rejected and poison trust.
            let empty_data = data(b"");
            assert_eq!(SecTrustSetOCSPResponse(trust, empty_data), PARAM);
            assert_eq!(SecTrustEvaluateWithError(trust, std::ptr::null_mut()), 0);
            assert_eq!(SecTrustSetOCSPResponse(trust, std::ptr::null()), SUCCESS);
            assert_eq!(SecTrustEvaluateWithError(trust, std::ptr::null_mut()), 0);
            for obj in [
                leaf_data, root_data, leaf, root, name, policy, trust, anchors, empty_data,
            ] {
                corefoundation::api::CFRelease(obj);
            }
        }
    }

    /// An empty custom anchor set must not silently fall back to system roots.
    #[test]
    fn anchor_modes_and_invalid_constructor_fail_closed() {
        foundation::bootstrap();
        let leaf_data = data(include_bytes!("testdata/tls_unit_leaf.der"));
        let root_data = data(include_bytes!("testdata/tls_unit_ca.der"));
        let name = corefoundation::string::from_bytes(b"studio.example.test", false);
        // SAFETY: every pointer below is a valid, live CF object.
        unsafe {
            let leaf = SecCertificateCreateWithData(std::ptr::null(), leaf_data);
            let root = SecCertificateCreateWithData(std::ptr::null(), root_data);
            let policy = SecPolicyCreateSSL(1, name);
            let mut trust = std::ptr::null();
            assert_eq!(
                SecTrustCreateWithCertificates(leaf, policy, &mut trust),
                SUCCESS
            );
            let custom = corefoundation::collections::new_array(
                &[root],
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            );
            assert_eq!(SecTrustSetAnchorCertificates(trust, custom), SUCCESS);
            let fixed_date = foundation::timer::date_since_1970(1_800_000_000.0);
            assert_eq!(SecTrustSetVerifyDate(trust, fixed_date as _), SUCCESS);
            assert_eq!(SecTrustEvaluateWithError(trust, std::ptr::null_mut()), 1);
            assert_eq!(SecTrustSetAnchorCertificatesOnly(trust, 0), SUCCESS);
            assert_eq!(SecTrustEvaluateWithError(trust, std::ptr::null_mut()), 1);
            let empty = corefoundation::collections::new_array(
                &[],
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            );
            assert_eq!(SecTrustSetAnchorCertificates(trust, empty), SUCCESS);
            assert_eq!(SecTrustEvaluateWithError(trust, std::ptr::null_mut()), 0);
            assert_eq!(
                SecTrustSetAnchorCertificates(trust, root),
                PARAM,
                "anchors must be an array"
            );
            assert_eq!(SecTrustEvaluateWithError(trust, std::ptr::null_mut()), 0);
            assert_eq!(SecTrustSetAnchorCertificates(trust, custom), SUCCESS);
            assert_eq!(SecTrustEvaluateWithError(trust, std::ptr::null_mut()), 1);
            let invalid_policy = data(b"not a SecPolicy");
            let mut old = trust;
            assert_eq!(
                SecTrustCreateWithCertificates(leaf, invalid_policy, &mut old),
                PARAM
            );
            assert!(old.is_null(), "failed constructor clears the output");
            corefoundation::api::CFRelease(invalid_policy);
            for obj in [
                leaf_data, root_data, leaf, root, name, policy, trust, custom, empty,
            ] {
                corefoundation::api::CFRelease(obj);
            }
        }
    }

    #[test]
    fn bytes_that_are_not_a_certificate_make_no_certificate() {
        objc::bootstrap();
        let data = data(b"not a certificate");
        // SAFETY: a `CFData` this test made.
        unsafe {
            assert!(SecCertificateCreateWithData(std::ptr::null(), data).is_null());
            corefoundation::api::CFRelease(data);
        }
    }

    #[test]
    fn random_bytes_fill_the_buffer() {
        let mut buffer = [0u8; 64];
        // SAFETY: a writable buffer of the stated length.
        assert_eq!(
            unsafe { SecRandomCopyBytes(std::ptr::null(), buffer.len(), buffer.as_mut_ptr()) },
            0
        );
        assert!(buffer.iter().any(|&b| b != 0));
    }

    /// Subject summaries are informational and must never invent a name.
    #[test]
    fn certificate_summary_uses_real_der_subject() {
        assert_eq!(
            certificate_subject_summary(&self_signed()),
            Some("rosetta3 test".to_string())
        );
        assert_eq!(certificate_subject_summary(b"not a cert"), None);
        assert_eq!(der_element(&mut &[0x30, 0x82, 0xff, 0xff][..]), None);
    }

    /// A self-signed P-256 certificate, `CN=rosetta3 test`, valid for a century
    /// from its making so the test does not expire.
    fn self_signed() -> Vec<u8> {
        include_bytes!("testdata/self_signed.der").to_vec()
    }

    fn data(bytes: &[u8]) -> CFTypeRef {
        // SAFETY: a live slice of the stated length.
        unsafe {
            corefoundation::api::CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as _)
        }
    }
}
