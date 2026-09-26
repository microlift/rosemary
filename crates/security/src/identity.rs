#![allow(non_snake_case)]

//! `SecIdentityRef`: a certificate and its private key, in a keychain.
//!
//! **There is no identity on this host, and the refusal is at the search**, not
//! at the copies. That ordering is the whole design: `SecIdentitySearchCreate`
//! answers `errSecItemNotFound`, so a guest that enumerates identities gets an
//! empty list and every `SecIdentityCopy*` below is never reached. Each one is
//! still bound and still refuses by name, because an unbound import is a jump
//! through a zero slot and that is how runs are lost - but the chain that matters
//! is closed at the search, where the answer is a fact rather than a refusal.
//!
//! The path is `net/ssl/client_cert_identity_mac.cc` and
//! `net/ssl/client_cert_store_mac.cc`, reached only when a TLS connection asks
//! for a client certificate. `--headless --dump-dom about:blank` asks for
//! nothing over TLS, and there is no keychain to have an identity in, so the
//! search is empty on both counts.

use corefoundation::base::CFTypeRef;

use crate::status::{self, OSStatus};

/// `OSStatus SecIdentitySearchCreate(CFTypeRef keychainOrArray, CSSM_KEYUSE
/// keyUsage, SecIdentitySearchRef *searchRef)`
///
/// `errSecItemNotFound`, and null for the search handle.
///
/// A search creates one *if* a matching identity exists; with no keychain there
/// is nothing to find, and `errSecItemNotFound` is `SecBase.h`'s own name for
/// that. The guest is written for it - `net::ClientCertStoreMac::GetClientCerts`
/// turns a non-`noErr` status into "no client certificates", which is the truth
/// on a host with no keychain and no configured client certificate.
///
/// # Safety
///
/// `keychain_or_array` is null or CoreFoundation objects; `search_ref` is null or
/// writable.
pub unsafe extern "C" fn SecIdentitySearchCreate(
    _keychain_or_array: CFTypeRef,
    _key_usage: u32,
    search_ref: *mut CFTypeRef,
) -> OSStatus {
    if !search_ref.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { search_ref.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "SecIdentitySearchCreate",
        "an identity is a certificate and its private key held in a keychain, and this host \
         has no keychain. The search is refused here rather than answered with an empty \
         result set, because saying so is what tells a reader the keychain is the missing \
         thing and not the guest's own configuration.",
    );
    status::ITEM_NOT_FOUND
}

/// `OSStatus SecIdentitySearchCopyNext(SecIdentitySearchRef searchRef,
/// SecIdentityRef *identity)`
///
/// `errSecItemNotFound`, which is also what this returns at the end of a search
/// that *did* find things, so a caller cannot tell an empty keychain from an
/// exhausted cursor - and could not act on the difference anyway.
///
/// # Safety
///
/// `search_ref` is null or a search this module made; `identity` is null or
/// writable.
pub unsafe extern "C" fn SecIdentitySearchCopyNext(
    _search_ref: CFTypeRef,
    identity: *mut CFTypeRef,
) -> OSStatus {
    if !identity.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { identity.write(std::ptr::null_mut()) };
    }
    status::ITEM_NOT_FOUND
}

/// `OSStatus SecIdentityCopyCertificate(SecIdentityRef identityRef,
/// SecCertificateRef *certificateRef)`
///
/// Null, refused by name. Reached only with an identity, and there is none.
///
/// # Safety
///
/// `identity_ref` is null or an identity; `certificate_ref` is null or writable.
pub unsafe extern "C" fn SecIdentityCopyCertificate(
    _identity_ref: CFTypeRef,
    certificate_ref: *mut CFTypeRef,
) -> OSStatus {
    if !certificate_ref.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { certificate_ref.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "SecIdentityCopyCertificate",
        "no identity exists on this host, so there is no certificate to copy. A certificate \
         here would be one this module chose, attached to an identity the guest never had.",
    );
    status::ITEM_NOT_FOUND
}

/// `SecIdentityRef SecIdentityCopyPreferred(CFStringRef name, CFArrayRef
/// keyUsage, CFArrayRef validIssuers)`
///
/// Null, refused by name. "Preferred" is a statement about a keychain's contents,
/// and there are none.
///
/// # Safety
///
/// `name` is null or a `CFString`; `key_usage` and `valid_issuers` are null or
/// arrays.
pub unsafe extern "C" fn SecIdentityCopyPreferred(
    _name: CFTypeRef,
    _key_usage: CFTypeRef,
    _valid_issuers: CFTypeRef,
) -> CFTypeRef {
    status::refuse(
        "SecIdentityCopyPreferred",
        "the preferred identity is whichever one a keychain says is preferred, and this \
         host has no keychain. Naming one would be this module picking a key on the \
         guest's behalf.",
    );
    std::ptr::null()
}

/// `OSStatus SecIdentityCopyPrivateKey(SecIdentityRef identityRef, SecKeyRef
/// *privateKeyRef)`
///
/// Null, refused by name. `client_cert_identity_mac.cc:29` logs the status and
/// hands `nullptr` to its callback, which is the honest outcome: a client
/// certificate with no usable key.
///
/// # Safety
///
/// `identity_ref` is null or an identity; `private_key_ref` is null or writable.
pub unsafe extern "C" fn SecIdentityCopyPrivateKey(
    _identity_ref: CFTypeRef,
    private_key_ref: *mut CFTypeRef,
) -> OSStatus {
    if !private_key_ref.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { private_key_ref.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "SecIdentityCopyPrivateKey",
        "a private key lives in a keychain or a Secure Enclave, and this host has neither. \
         A key returned here would be a key this module invented, offered to a TLS stack as \
         though the user had configured one.",
    );
    status::NO_SUCH_KEYCHAIN
}

exports::exports! {
    functions {
        "SecIdentitySearchCreate" => SecIdentitySearchCreate,
        "SecIdentitySearchCopyNext" => SecIdentitySearchCopyNext,
        "SecIdentityCopyCertificate" => SecIdentityCopyCertificate,
        "SecIdentityCopyPreferred" => SecIdentityCopyPreferred,
        "SecIdentityCopyPrivateKey" => SecIdentityCopyPrivateKey,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The search is where the chain closes: non-success, and a null handle.
    #[test]
    fn the_search_finds_nothing_and_says_so() {
        objc::bootstrap();
        let mut search: CFTypeRef = 1_usize as CFTypeRef;
        // SAFETY: null is the documented "every keychain" argument, and a
        // writable out-parameter.
        unsafe {
            assert_eq!(
                SecIdentitySearchCreate(std::ptr::null(), 0, &mut search),
                status::ITEM_NOT_FOUND
            );
            assert!(search.is_null());
            let mut identity: CFTypeRef = 1_usize as CFTypeRef;
            assert_eq!(
                SecIdentitySearchCopyNext(std::ptr::null(), &mut identity),
                status::ITEM_NOT_FOUND
            );
            assert!(identity.is_null());
        }
    }

    /// Every copy is a refusal, with a null out-parameter, so a caller that only
    /// checks the handle still sees nothing.
    #[test]
    fn every_copy_is_refused() {
        objc::bootstrap();
        let mut out: CFTypeRef = 1_usize as CFTypeRef;
        // SAFETY: null arguments throughout, which every one of these documents
        // as acceptable, and writable out-parameters.
        unsafe {
            assert_eq!(
                SecIdentityCopyCertificate(std::ptr::null(), &mut out),
                status::ITEM_NOT_FOUND
            );
            assert!(out.is_null());
            assert_eq!(
                SecIdentityCopyPrivateKey(std::ptr::null(), &mut out),
                status::NO_SUCH_KEYCHAIN
            );
            assert!(out.is_null());
            assert!(SecIdentityCopyPreferred(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null()
            )
            .is_null());
        }
    }
}
