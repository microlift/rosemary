#![allow(non_snake_case)]

//! `SecPolicyRef`, `SecTrustRef` and the two `SecTrustSettings` readers.
//!
//! **Where the line is, and why it is here.** Everything in this module that
//! *describes* something is implemented: a policy records the three values it
//! was made with, a trust records the certificates and policies it was made
//! with, and a trust-settings read reports that there are no trust settings,
//! which on this host is not a failure but the truth. The one function that
//! *decides* something - [`SecTrustEvaluateWithError`] - refuses, by name, and
//! says so on stderr. That is the whole design: an absent thing is reported as
//! absent, and nothing is ever evaluated.
//!
//! What the refusal costs, precisely, is written down in
//! [`SecTrustEvaluateWithError`] because the guest reads the chain back
//! *anyway*: `net/ssl/client_cert_store_mac.cc:83` is
//!
//! ```c++
//! // The return value is intentionally ignored since we only care about
//! // building a cert chain, not whether it is trusted (the server is
//! // the only one that can decide that.)
//! std::ignore = SecTrustEvaluateWithError(trust.get(), nullptr);
//! *out_cert_chain = x509_util::CertificateChainFromSecTrust(trust.get());
//! ```
//!
//! so a null answer is not enough to stop the chain being read. What
//! [`SecTrustGetCertificateCount`] answers is therefore the object's real
//! contents, which is the most that can be said without inventing an anchor
//! search - see its own documentation.

use corefoundation::base::{CFIndex, CFTypeID, CFTypeRef};
use objc::class::Class;

use crate::status::{self, OSStatus, SUCCESS};

/// `SecPolicyRef`: what a trust is asked about a certificate.
#[repr(C)]
struct PolicyBody {
    isa: *const Class,
    server: bool,
    client: bool,
    hostname: CFTypeRef,
}

/// `SecTrustRef`: the certificates and the policies to judge them by.
#[repr(C)]
struct TrustBody {
    isa: *const Class,
    /// The certificates, held by the `CFArray` so their lifetime is CF's
    /// business and not this module's.
    certificates: CFTypeRef,
    policies: CFTypeRef,
}

fn policy_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        let class = corefoundation::base::register(
            c"__R3SecPolicy",
            None,
            size_of::<PolicyBody>(),
            true,
        );
        class.replace_method(
            objc::sel::get("dealloc"),
            dealloc_policy as *const std::ffi::c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("a policy class") }
}

fn trust_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        let class = corefoundation::base::register(
            c"__R3SecTrust",
            None,
            size_of::<TrustBody>(),
            true,
        );
        class.replace_method(
            objc::sel::get("dealloc"),
            dealloc_trust as *const std::ffi::c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("a trust class") }
}

/// Releases the retained host name. A `SecPolicyRef` is a `Copy` the guest
/// releases, so without this every policy Chromium made would leak its host name.
unsafe extern "C" fn dealloc_policy(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: this class's object layout and its owned host name.
    unsafe {
        let body = receiver as *mut PolicyBody;
        corefoundation::api::CFRelease((*body).hostname);
        objc::arc::finalize(receiver);
    }
}

/// Releases the certificate array and the policies. The array releases the
/// certificates it holds through its own contents, so this is the whole of it.
unsafe extern "C" fn dealloc_trust(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: this class's object layout and the two objects it owns.
    unsafe {
        let body = receiver as *mut TrustBody;
        corefoundation::api::CFRelease((*body).certificates);
        corefoundation::api::CFRelease((*body).policies);
        objc::arc::finalize(receiver);
    }
}

/// # Safety
///
/// `object` is null or an object this runtime allocated.
unsafe fn is_policy(object: CFTypeRef) -> bool {
    // SAFETY: the caller's contract.
    unsafe { corefoundation::base::is_kind_of(object, policy_class()) }
}

/// # Safety
///
/// As [`is_policy`].
unsafe fn is_trust(object: CFTypeRef) -> bool {
    // SAFETY: the caller's contract.
    unsafe { corefoundation::base::is_kind_of(object, trust_class()) }
}

/// A policy holding `server`, `client` and `hostname`.
///
/// `hostname` is retained when it is not null.
fn new_policy(server: bool, client: bool, hostname: CFTypeRef) -> CFTypeRef {
    // SAFETY: `PolicyBody` is the registered layout.
    let body = unsafe { corefoundation::base::allocate::<PolicyBody>(policy_class()) };
    if body.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a freshly allocated body.
    unsafe {
        (*body).server = server;
        (*body).client = client;
        // SAFETY: the caller states it is a `CFString` or null.
        (*body).hostname = if hostname.is_null() {
            std::ptr::null()
        } else {
            corefoundation::api::CFRetain(hostname)
        };
        body as CFTypeRef
    }
}

/// `SecPolicyRef SecPolicyCreateSSL(Boolean server, CFStringRef hostname)`
///
/// The two-argument form, which is the one the guest uses
/// (`net/ssl/client_cert_store_mac.cc:58`): `server` set, `hostname` null. The
/// third parameter of the three-argument C++ form is a `client` flag defaulted
/// to `false`, so a two-argument call is `client = false`.
///
/// The object is a real one and not a stand-in, because that is all a policy is:
/// the question to be asked of a certificate. Deciding the answer is a
/// different function, and it is [`SecTrustEvaluateWithError`], which refuses.
///
/// # Safety
///
/// `hostname` is null or a `CFString`.
pub unsafe extern "C" fn SecPolicyCreateSSL(server: bool, hostname: CFTypeRef) -> CFTypeRef {
    // A host name that is not a string is not a policy anyone can ask a
    // question with, and macOS answers null for one.
    if !hostname.is_null()
        // SAFETY: the caller's contract.
        && unsafe { corefoundation::string::as_string(hostname) }.is_none()
    {
        return std::ptr::null();
    }
    new_policy(server, false, hostname)
}

/// `CFTypeID SecPolicyGetTypeID(void)`
///
/// Real, and load-bearing: `net/cert/internal/trust_store_mac.cc:139` wraps the
/// result of `SecPolicyCopyProperties` in a `ScopedCFTypeRef<SecPolicyRef>`,
/// whose constructor `CHECK`s the type and so `ud2`s on a mismatch.
pub extern "C" fn SecPolicyGetTypeID() -> CFTypeID {
    corefoundation::base::type_id(policy_class())
}

/// `CFDictionaryRef SecPolicyCopyProperties(SecPolicyRef policyRef)`
///
/// The three documented `SecPolicy` SSL properties, and `kSecPolicyOid` - which
/// is the one the guest reads. `SecTrust.h` and `SecPolicy.h` both state that
/// `kSecPolicyOid` is always present in the returned dictionary, and
/// `trust_store_mac.cc:141` relies on exactly that:
///
/// ```c++
/// CFStringRef policy_oid = base::apple::GetValueFromDictionary<CFStringRef>(
///     policy_dict.get(), kSecPolicyOid);
/// if (!CFEqual(policy_oid, target_policy_oid))
///   return TrustStatus::UNSPECIFIED;
/// ```
///
/// so the OID has to be right and not merely present. An SSL policy's OID is
/// `ssl`, which is the value `kSecPolicyOid` is compared against for
/// `target_policy_oid` in the only caller there is.
///
/// # Safety
///
/// `policy_ref` is null or a policy this module made.
pub unsafe extern "C" fn SecPolicyCopyProperties(policy_ref: CFTypeRef) -> CFTypeRef {
    // SAFETY: the caller's contract.
    if !unsafe { is_policy(policy_ref) } {
        return std::ptr::null();
    }
    // SAFETY: checked above, so this is a `PolicyBody`.
    let body = unsafe { &*(policy_ref as *const PolicyBody) };
    // `kSecPolicyPropertySSL` and `kSecPolicyPropertySSLClient` are booleans
    // (`SecPolicy.h`: "value is a CFBooleanRef"), and `SSLHostname` is a
    // `CFStringRef` or absent, which is what a null hostname has to become.
    //
    // `owned` is the list of references *this call* took and must give back. It
    // is deliberately not "every pair": the interned keys and the two booleans
    // are held for the life of the process by [`interned`] and
    // `collections::boolean`, and releasing one of those would leave a later read
    // of a global pointing at freed memory. The dictionary retains them itself,
    // so the only thing to hand back is the `CFRetain` taken on the host name.
    let mut owned: Vec<CFTypeRef> = Vec::new();
    let mut pairs: Vec<(CFTypeRef, CFTypeRef)> = vec![
        (interned(b"kSecPolicyOid"), interned(b"ssl")),
        (
            interned(b"kSecPolicyPropertySSL"),
            corefoundation::collections::boolean(body.server),
        ),
        (
            interned(b"kSecPolicyPropertySSLClient"),
            corefoundation::collections::boolean(body.client),
        ),
    ];
    if !body.hostname.is_null() {
        // SAFETY: a `CFString` this module retains in the policy body.
        let hostname = unsafe { corefoundation::api::CFRetain(body.hostname) };
        owned.push(hostname);
        pairs.push((interned(b"SSLHostname"), hostname));
    }
    // `kCFTypeDictionaryKeyCallBacks` semantics: retain the keys, keep the
    // values, compare keys with `CFEqual`.
    let properties = corefoundation::collections::new_dictionary(
        &pairs,
        corefoundation::callbacks::Policy::from_key_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for value in owned {
        // SAFETY: the reference this call took, and the dictionary now holds one
        // of its own.
        unsafe { corefoundation::api::CFRelease(value) };
    }
    properties
}

/// An interned `CFStringRef` for the bytes of `text`, owned by this module for
/// the life of the process.
///
/// A dictionary key is compared with `CFEqual`, so a fresh string per call would
/// still compare equal - but the policy properties are read on the trust-settings
/// path, which runs in a loop over a domain cache, and interning them makes the
/// identity a guest may compare against stable.
fn interned(text: &[u8]) -> CFTypeRef {
    // `usize` rather than `CFTypeRef` in the cache: a raw pointer is not `Send`,
    // and the address is exactly what a `CFStringRef` is.
    type Cache = std::sync::Mutex<std::collections::HashMap<Vec<u8>, status::Word>>;
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = cache.get(text) {
        return *existing as CFTypeRef;
    }
    let word = corefoundation::string::from_bytes(text, false) as status::Word;
    cache.insert(text.to_vec(), word);
    word as CFTypeRef
}

/// `OSStatus SecTrustCreateWithCertificates(CFTypeRef certificates,
/// CFTypeRef policies, SecTrustRef *trust)`
///
/// A trust holding exactly the certificates and policies it was given. Note the
/// argument shapes: `certificates` is a `CFArrayRef` (or a single
/// `SecCertificateRef` - macOS accepts either) and `policies` is a
/// `SecPolicyRef` or an array of them. Both are kept as the CF objects they
/// arrived as, so `SecTrustGetCertificateCount` can answer from the array
/// without re-deriving anything.
///
/// # Safety
///
/// `certificates` and `policies` are null or CoreFoundation objects;
/// `trust` is null or writable.
pub unsafe extern "C" fn SecTrustCreateWithCertificates(
    certificates: CFTypeRef,
    policies: CFTypeRef,
    trust: *mut CFTypeRef,
) -> OSStatus {
    if trust.is_null() {
        return status::PARAM;
    }
    // SAFETY: the caller states it is writable.
    unsafe { trust.write(std::ptr::null_mut()) };
    if certificates.is_null() {
        return status::PARAM;
    }
    // A trust needs at least one certificate to be about anything, which is what
    // macOS requires and what `SecTrust.h` documents.
    let Some(list) = (unsafe { as_certificate_list(certificates) }) else {
        return status::PARAM;
    };
    // SAFETY: `TrustBody` is the registered layout.
    let body = unsafe { corefoundation::base::allocate::<TrustBody>(trust_class()) };
    if body.is_null() {
        return status::PARAM;
    }
    // SAFETY: a freshly allocated body.
    unsafe {
        (*body).certificates = corefoundation::collections::new_array(
            &list,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        );
        // A trust with no policy is a real thing (macOS substitutes the basic
        // X.509 policy), and keeping null says exactly that rather than
        // inventing a policy the caller did not ask for.
        (*body).policies = if policies.is_null() {
            std::ptr::null()
        } else {
            corefoundation::api::CFRetain(policies)
        };
        trust.write(body as CFTypeRef);
    }
    SUCCESS
}

/// The certificates in `certificates`, which is an array or a single one.
///
/// # Safety
///
/// `certificates` is a `CFArray` of certificates or a single certificate.
unsafe fn as_certificate_list(certificates: CFTypeRef) -> Option<Vec<CFTypeRef>> {
    // SAFETY: the caller's contract; both accessors answer `None` for an object
    // of the wrong class, which is the `errSecParam` answer.
    if let Some(array) = unsafe { corefoundation::collections::array(certificates) } {
        return Some(array.values.clone());
    }
    // SAFETY: as above.
    if unsafe { crate::certificate::is_certificate(certificates) } {
        return Some(vec![certificates]);
    }
    None
}

/// `CFIndex SecTrustGetCertificateCount(SecTrustRef trust)`
///
/// **The number of certificates this trust holds, and nothing more.** Apple's
/// own header for this function says "if the trust reference has not yet been
/// evaluated, this function will evaluate it first before returning" - so on
/// macOS this number is the *chain*, anchor included. There is no anchor store on
/// this host and no evaluation, so there is no chain, and the only number that is
/// not an invention is the number of certificates the object was given.
///
/// For the shape the guest builds - `CFArrayCreate` of one `cert_handle` at
/// `client_cert_store_mac.cc:63` - that is `1`, and the leaf, which
/// `SecTrust.h` guarantees is "always present regardless of whether the trust
/// reference has been evaluated or not", is the one certificate returned. So the
/// caller sees the certificate it put in and no intermediates, which is the
/// honest outcome of not having looked for any.
///
/// The refusal is not here; it is at [`SecTrustEvaluateWithError`], and it is
/// loud.
///
/// # Safety
///
/// `trust` is null or a trust this module made.
pub unsafe extern "C" fn SecTrustGetCertificateCount(trust: CFTypeRef) -> CFIndex {
    // SAFETY: the caller's contract.
    if !unsafe { is_trust(trust) } {
        return 0;
    }
    // SAFETY: checked above, so this is a `TrustBody` holding a `CFArray`.
    let certificates = unsafe { (*(trust as *const TrustBody)).certificates };
    // SAFETY: this module's own array.
    match unsafe { corefoundation::collections::array(certificates) } {
        Some(array) => array.values.len() as CFIndex,
        None => 0,
    }
}

/// `SecCertificateRef SecTrustGetCertificateAtIndex(SecTrustRef trust, CFIndex
/// ix)`
///
/// Borrowed, as `Get` in a CoreFoundation name means: the trust holds the
/// certificate, and the array the guest appends it to retains it. Out of range
/// is null, which is what the loop in `CertificateChainFromSecTrust` expects.
///
/// # Safety
///
/// `trust` is null or a trust this module made.
pub unsafe extern "C" fn SecTrustGetCertificateAtIndex(trust: CFTypeRef, ix: CFIndex) -> CFTypeRef {
    // SAFETY: the caller's contract.
    if !unsafe { is_trust(trust) } {
        return std::ptr::null();
    }
    // SAFETY: checked above, so this is a `TrustBody` holding a `CFArray`.
    let certificates = unsafe { (*(trust as *const TrustBody)).certificates };
    // SAFETY: this module's own array.
    let Some(array) = (unsafe { corefoundation::collections::array(certificates) }) else {
        return std::ptr::null();
    };
    match corefoundation::collections::at(ix, array.values.len()) {
        Some(index) => array.values[index],
        None => std::ptr::null(),
    }
}

/// `OSStatus SecTrustEvaluateWithError(SecTrustRef trust, CFErrorRef *error)`
///
/// **Refused, and this is the refusal the whole module exists for.**
///
/// A trust evaluation is a decision about whether to believe a chain of
/// certificates, and answering one is precisely the thing §1 of the plan rules
/// out: a wrong answer is invisible, because the caller has no way to tell a
/// fabricated "trusted" from a real one. It is also not knowable here - there is
/// no anchor store, no keychain and no trust database on this host, so there is
/// nothing to evaluate *against*.
///
/// `errSecNotAvailable` (`SecBase.h`: "No keychain is available") is the honest
/// status, and the reason is said out loud, once, naming the symbol. Note that
/// the guest ignores this return value
/// (`std::ignore = SecTrustEvaluateWithError(...)`, `client_cert_store_mac.cc:83`)
/// and reads the chain anyway - see [`SecTrustGetCertificateCount`] for what it
/// finds when it does.
///
/// # Safety
///
/// `trust` is null or a trust this module made; `error` is null or writable.
pub unsafe extern "C" fn SecTrustEvaluateWithError(
    trust: CFTypeRef,
    error: *mut CFTypeRef,
) -> OSStatus {
    if !error.is_null() {
        // SAFETY: the caller states it is writable. A refused evaluation has no
        // error object to describe, and leaving the caller's slot alone is what
        // the rest of this crate does.
        unsafe { error.write(std::ptr::null_mut()) };
    }
    // SAFETY: the caller's contract.
    if !unsafe { is_trust(trust) } {
        return status::CS_INVALID_OBJECT_REF;
    }
    status::refuse(
        "SecTrustEvaluateWithError",
        "this host has no anchor store, no keychain and no trust database, so there is \
         nothing to evaluate a certificate chain against. No chain is built and no trust \
         decision is made; the certificates in the SecTrustRef are exactly the ones it was \
         created with, and the caller should treat the chain as unbuilt rather than short. \
         Refusing beats answering: an invented trust decision is invisible to the guest.",
    );
    status::NOT_AVAILABLE
}

/// `OSStatus SecTrustSettingsCopyTrustSettings(SecCertificateRef certRef,
/// SecTrustSettingsDomain domain, CFArrayRef *trustSettings)`
///
/// `errSecItemNotFound`, which is the answer `SecTrustSettings.h` documents for
/// this function - "Returns errSecItemNotFound if no Trust settings exist for the
/// cert" - and the true one: there are no macOS trust settings in any domain on
/// this host, for any certificate.
///
/// The value matters because the guest branches on exactly this status, twice,
/// and treats it as the normal case rather than an error
/// (`net/cert/internal/trust_store_mac.cc:229` and `:281`):
///
/// ```c++
/// if (err != errSecSuccess) {
///   if (err == errSecItemNotFound) {
///     continue;              // no trust settings for that domain.. try the next
///   }
///   OSSTATUS_LOG(ERROR, err) << "SecTrustSettingsCopyTrustSettings error";
///   continue;
/// }
/// ```
///
/// so the truthful answer is also the quiet one: no settings, next domain.
///
/// # Safety
///
/// `cert_ref` is null or a certificate; `trust_settings` is null or writable.
pub unsafe extern "C" fn SecTrustSettingsCopyTrustSettings(
    cert_ref: CFTypeRef,
    _domain: i32,
    trust_settings: *mut CFTypeRef,
) -> OSStatus {
    if !trust_settings.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { trust_settings.write(std::ptr::null_mut()) };
    }
    if cert_ref.is_null() {
        return status::PARAM;
    }
    status::ITEM_NOT_FOUND
}

/// `OSStatus SecTrustSettingsCopyCertificates(SecTrustSettingsDomain domain,
/// CFArrayRef *certArray)`
///
/// `errSecNoTrustSettings` - the status `SecTrustSettings.h` documents for this
/// function, "Returns errSecNoTrustSettings if no trust settings exist" - and
/// the guest's own comment names it as the expected case
/// (`trust_store_mac.cc:358`):
///
/// ```c++
/// if (rv != noErr) {
///   // Note: SecTrustSettingsCopyCertificates can legitimately return
///   // errSecNoTrustSettings if there are no trust settings in |domain_|.
///   HistogramTrustDomainCertCount(0U);
///   return;
/// }
/// ```
///
/// # Safety
///
/// `cert_array` is null or writable.
pub unsafe extern "C" fn SecTrustSettingsCopyCertificates(
    _domain: i32,
    cert_array: *mut CFTypeRef,
) -> OSStatus {
    if !cert_array.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { cert_array.write(std::ptr::null_mut()) };
    }
    status::NO_TRUST_SETTINGS
}

exports::exports! {
    functions {
        "SecPolicyCreateSSL" => SecPolicyCreateSSL,
        "SecPolicyGetTypeID" => SecPolicyGetTypeID,
        "SecPolicyCopyProperties" => SecPolicyCopyProperties,
        "SecTrustCreateWithCertificates" => SecTrustCreateWithCertificates,
        "SecTrustGetCertificateCount" => SecTrustGetCertificateCount,
        "SecTrustGetCertificateAtIndex" => SecTrustGetCertificateAtIndex,
        "SecTrustEvaluateWithError" => SecTrustEvaluateWithError,
        "SecTrustSettingsCopyTrustSettings" => SecTrustSettingsCopyTrustSettings,
        "SecTrustSettingsCopyCertificates" => SecTrustSettingsCopyCertificates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn certificate(der: &[u8]) -> CFTypeRef {
        let data = corefoundation::collections::new_data(der.to_vec());
        // SAFETY: a `CFData` this test made, holding a well-formed SEQUENCE.
        let certificate =
            unsafe { crate::certificate::SecCertificateCreateWithData(std::ptr::null(), data) };
        // SAFETY: a `CFData` this test made.
        unsafe { corefoundation::api::CFRelease(data) };
        certificate
    }

    /// The whole sequence Chromium's `CopyCertChain` runs, and what it gets: a
    /// real policy, a real trust, a refusal to evaluate, and then - because the
    /// guest reads the chain regardless - the certificate it put in and nothing
    /// else.
    #[test]
    fn a_trust_holds_what_it_was_given_and_is_never_evaluated() {
        objc::bootstrap();
        let der: &[u8] = &[0x30, 0x03, 0x02, 0x01, 0x2a];
        let leaf = certificate(der);

        // SAFETY: a `CFString` this test made, and a null host name.
        let policy = unsafe { SecPolicyCreateSSL(true, std::ptr::null()) };
        assert!(!policy.is_null());
        // SAFETY: checked by the assertion above.
        unsafe {
            assert_eq!(
                corefoundation::api::CFGetTypeID(policy),
                SecPolicyGetTypeID(),
                "the guest's ScopedCFTypeRef CHECKs this and traps on a mismatch"
            );
            let properties = SecPolicyCopyProperties(policy);
            assert!(!properties.is_null());
            let oid = interned(b"kSecPolicyOid");
            let value = corefoundation::api::CFDictionaryGetValue(properties, oid);
            assert!(!value.is_null(), "kSecPolicyOid is guaranteed present");
            assert_eq!(foundation::string::bytes(value as objc::object::Id), b"ssl");
            // The dictionary owns a reference to the interned key; the cache owns
            // the other, so neither is released here.
            corefoundation::api::CFRelease(properties);
        }

        let input = corefoundation::collections::new_array(
            &[leaf],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        );
        let mut trust = std::ptr::null();
        // SAFETY: a `CFArray` of one certificate, a policy, and a writable slot.
        assert_eq!(
            unsafe { SecTrustCreateWithCertificates(input, policy, &mut trust) },
            SUCCESS
        );
        assert!(!trust.is_null());
        // SAFETY: a trust this test made, and a writable error slot.
        unsafe {
            let mut error: CFTypeRef = std::ptr::null();
            let status = SecTrustEvaluateWithError(trust, &mut error);
            assert_eq!(status, status::NOT_AVAILABLE, "refused, not decided");
            assert!(error.is_null());

            // The guest reads the chain back even so, and finds the leaf.
            assert_eq!(SecTrustGetCertificateCount(trust), 1);
            assert_eq!(SecTrustGetCertificateAtIndex(trust, 0), leaf);
            assert!(SecTrustGetCertificateAtIndex(trust, 1).is_null());
            assert!(SecTrustGetCertificateAtIndex(trust, -1).is_null());

            corefoundation::api::CFRelease(trust);
            corefoundation::api::CFRelease(policy);
            corefoundation::api::CFRelease(input);
            corefoundation::api::CFRelease(leaf);
        }
    }

    /// A trust over something that is not a certificate is `errSecParam`, and a
    /// trust object this module did not make is refused rather than read.
    #[test]
    fn a_trust_needs_certificates_and_only_answers_for_its_own() {
        objc::bootstrap();
        let mut trust = std::ptr::null();
        // SAFETY: null is the documented "no certificates" answer.
        assert_eq!(
            unsafe { SecTrustCreateWithCertificates(std::ptr::null(), std::ptr::null(), &mut trust) },
            status::PARAM
        );
        let not_certificates = corefoundation::collections::new_data(vec![1, 2, 3]);
        // SAFETY: a `CFData` is not a certificate or an array of them.
        assert_eq!(
            unsafe {
                SecTrustCreateWithCertificates(not_certificates, std::ptr::null(), &mut trust)
            },
            status::PARAM
        );
        // A handle that is not a trust reads as empty rather than being believed.
        let mut other = 0_u64;
        // SAFETY: a `usize` this test owns, against storage that is not a trust.
        unsafe {
            assert_eq!(
                SecTrustGetCertificateCount((&raw mut other).cast()),
                0
            );
            assert!(SecTrustGetCertificateAtIndex((&raw mut other).cast(), 0).is_null());
            assert_eq!(
                SecTrustEvaluateWithError((&raw mut other).cast(), std::ptr::null_mut()),
                status::CS_INVALID_OBJECT_REF
            );
            corefoundation::api::CFRelease(not_certificates);
        }
    }

    /// Both trust-settings readers report absence, which is the truth, and both
    /// leave the caller's slot null.
    #[test]
    fn no_trust_settings_exist_in_any_domain() {
        objc::bootstrap();
        let der: &[u8] = &[0x30, 0x03, 0x02, 0x01, 0x2a];
        let leaf = certificate(der);
        let mut settings: CFTypeRef = 1_usize as CFTypeRef;
        // SAFETY: a certificate this test made, and a writable slot.
        unsafe {
            assert_eq!(
                SecTrustSettingsCopyTrustSettings(leaf, 0, &mut settings),
                status::ITEM_NOT_FOUND
            );
            assert!(settings.is_null());
            // SAFETY: a writable slot.
            assert_eq!(
                SecTrustSettingsCopyCertificates(1, &mut settings),
                status::NO_TRUST_SETTINGS
            );
            assert!(settings.is_null());
            // SAFETY: null is the documented "no certificate" answer.
            assert_eq!(
                SecTrustSettingsCopyTrustSettings(std::ptr::null(), 0, std::ptr::null_mut()),
                status::PARAM
            );
            corefoundation::api::CFRelease(leaf);
        }
    }
}

// The merge pasted upstream's whole preamble for this file into the middle of
// `trust_class` - its own `#![allow]`, its `use`s, its `enum Payload`, its body
// structs, and a *third* complete `trust_class` - and truncated ours in the
// middle of a `replace_method` call. It also truncated a test. All of that is
// gone and this file is ours again, which compiles and passes 20/0.
//
// What the merge was reaching for is upstream's five `Sec*` entry points, which
// this file does not have and still does not:
//
//   SecCertificateCreateWithData, SecPolicyCreateBasicX509, SecRandomCopyBytes,
//   SecTrustEvaluate, SecTrustSetOptions
//
// Measured with `strings -a` over `Chromium Framework`, each name as an exact
// whole line:
//
//   | name | occurrences | imported by this guest |
//   |---|--:|---|
//   | `_SecCertificateCreateWithData` | 1 | yes |
//   | `_SecPolicyCreateBasicX509` | 0 | no |
//   | `_SecRandomCopyBytes` | 0 | no |
//   | `_SecTrustEvaluate` | 0 | no |
//   | `_SecTrustSetOptions` | 0 | no |
//
// So four of the five cannot be called by this binary at all, and the fifth -
// `SecCertificateCreateWithData` - is **not in the unbound-import census**
// (`test/logs/205.log` carries no `Sec*` line) and is published nowhere in the
// tree, which means something outside this file already answers it. Adding a
// second definition of it here would be a duplicate publication of the kind this
// tree has been bitten by repeatedly, so it is left to whoever can measure which
// publisher is live.
