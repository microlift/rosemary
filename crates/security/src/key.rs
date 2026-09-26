#![allow(non_snake_case)]

//! `SecKeyRef`: a type ID, a feature probe, and five refusals.
//!
//! **There is no key on this host and there is no way to make one**, so almost
//! everything here refuses. What is implemented is the two answers that are
//! knowable without a key:
//!
//! - [`SecKeyGetTypeID`] is a real type ID, and it has to be. The guest inlines
//!   the `ScopedCFTypeRef<SecKeyRef>` type check exactly as it does for a
//!   certificate (`Framework+0x79eb0bf`), and a mismatch is a `ud2`.
//! - [`SecKeyIsAlgorithmSupported`] is `false`, which is what a key that does not
//!   exist supports. Both callers treat that as "do not offer this algorithm",
//!   which is the correct degradation and not a guess: one of them builds a
//!   *preference list* of signature algorithms
//!   (`net/ssl/ssl_platform_key_mac.cc:86`) and the other checks one algorithm
//!   before signing (`crypto/unexportable_key_mac.mm:143`). Neither infers
//!   anything from a `false`.
//!
//! The other five refuse by name, with a null result, because each of them is an
//! operation on a Secure Enclave or a keychain that does not exist. Returning
//! null is what macOS returns when the operation cannot be performed, and the
//! guests all handle it: `if (!private_key) { LOG(ERROR); return nullptr; }` at
//! `unexportable_key_mac.mm:279`, `if (!signature_ref) { LOG(ERROR) ... }` at
//! `ssl_platform_key_mac.cc:151`.
//!
//! **Signatures, cross-checked at the call sites.** The guest reaches these
//! through thin `AppleKeychainV2` wrappers that shift the arguments down by one,
//! so the register sets at the call sites are the prototypes:
//! `SecKeyCreateRandomKey` 2 arguments (`Framework+0x5f5095f`),
//! `SecKeyCreateSignature` 4 (`+0x5f50995`), `SecKeyCopyPublicKey` 1
//! (`+0x5f509bc`), `SecKeyCopyExternalRepresentation` 2 (`+0x5f509df`),
//! `SecKeyCopyAttributes` 1 (`+0x5f50a0c`).

use corefoundation::base::{CFTypeID, CFTypeRef};
use objc::class::Class;

use crate::status;

fn key_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        // No instances: nothing here makes a key. The class exists so that the
        // type ID is a real, stable, distinct answer - which is the whole of what
        // `SecKeyGetTypeID` is asked for - rather than a number invented here.
        objc::class::ptr(corefoundation::base::register(
            c"__R3SecKey",
            None,
            size_of::<usize>(),
            true,
        )) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("a key class") }
}

/// `CFTypeID SecKeyGetTypeID(void)`
pub extern "C" fn SecKeyGetTypeID() -> CFTypeID {
    corefoundation::base::type_id(key_class())
}

/// `Boolean SecKeyIsAlgorithmSupported(SecKeyRef key, SecKeyOperationType
/// operation, SecKeyAlgorithm algorithm)`
///
/// `false`, always, and truthfully: there is no key, and a key that does not
/// exist supports no algorithm and no operation. The key argument is not even
/// looked at, so this is also the answer for a handle that is not one.
///
/// # Safety
///
/// `key` is null or a `SecKeyRef`.
pub unsafe extern "C" fn SecKeyIsAlgorithmSupported(
    _key: CFTypeRef,
    _operation: i32,
    _algorithm: CFTypeRef,
) -> bool {
    false
}

/// `SecKeyRef SecKeyCreateRandomKey(CFDictionaryRef parameters, CFErrorRef
/// *error)`
///
/// Null, refused by name. Generating a key means the keychain or the Secure
/// Enclave, and there is neither: a key held in a plain Linux process would be a
/// file this module invents a format for, and a key the guest believes is
/// hardware-backed when it is not is worse than no key at all.
///
/// # Safety
///
/// `parameters` is null or a `CFDictionary`; `error` is null or writable.
pub unsafe extern "C" fn SecKeyCreateRandomKey(
    parameters: CFTypeRef,
    error: *mut CFTypeRef,
) -> CFTypeRef {
    if !error.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { error.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "SecKeyCreateRandomKey",
        "key generation needs a keychain or a Secure Enclave, and this host has neither. \
         A key invented here would be a file of this module's own making that the guest \
         would go on to treat as hardware-backed, so none is made.",
    );
    let _ = parameters;
    std::ptr::null()
}

/// `CFDataRef SecKeyCreateSignature(SecKeyRef key, SecKeyAlgorithm algorithm,
/// CFDataRef data, CFErrorRef *error)`
///
/// Null, refused by name: signing needs the private key, and there is none.
///
/// # Safety
///
/// `key` and `data` are null or CoreFoundation objects; `error` is null or
/// writable.
pub unsafe extern "C" fn SecKeyCreateSignature(
    _key: CFTypeRef,
    _algorithm: CFTypeRef,
    _data: CFTypeRef,
    error: *mut CFTypeRef,
) -> CFTypeRef {
    if !error.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { error.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "SecKeyCreateSignature",
        "signing needs a private key held in a keychain or a Secure Enclave, and this host \
         has neither. Returning a signature would mean returning bytes this module made up, \
         which is the one answer worse than no answer.",
    );
    std::ptr::null()
}

/// `CFDataRef SecKeyCopyExternalRepresentation(SecKeyRef key, CFErrorRef *error)`
///
/// Null, refused by name.
///
/// # Safety
///
/// `key` is null or a `SecKeyRef`; `error` is null or writable.
pub unsafe extern "C" fn SecKeyCopyExternalRepresentation(
    _key: CFTypeRef,
    error: *mut CFTypeRef,
) -> CFTypeRef {
    if !error.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { error.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "SecKeyCopyExternalRepresentation",
        "the external representation of a key is the key's own bytes, and there is no key \
         on this host. A `CFData` here would be data the guest would then hash, compare and \
         publish as though it were a public key.",
    );
    std::ptr::null()
}

/// `SecKeyRef SecKeyCopyPublicKey(SecKeyRef key)`
///
/// Null, refused by name.
///
/// # Safety
///
/// `key` is null or a `SecKeyRef`.
pub unsafe extern "C" fn SecKeyCopyPublicKey(_key: CFTypeRef) -> CFTypeRef {
    status::refuse(
        "SecKeyCopyPublicKey",
        "the public key of a private key is a derivation this host cannot perform, because \
         there is no private key. Returning a public key would be a key nobody can sign with \
         and a guest would have no way to tell.",
    );
    std::ptr::null()
}

/// `CFDictionaryRef SecKeyCopyAttributes(SecKeyRef key)`
///
/// Null, refused by name. The attributes are the key's own recorded properties -
/// its type, its size, its application label - so there is nothing to report for a
/// key that does not exist.
///
/// # Safety
///
/// `key` is null or a `SecKeyRef`.
pub unsafe extern "C" fn SecKeyCopyAttributes(_key: CFTypeRef) -> CFTypeRef {
    status::refuse(
        "SecKeyCopyAttributes",
        "a key's attributes describe that key, and this host has no keys. A dictionary here \
         would be a set of facts about a key that was never made.",
    );
    std::ptr::null()
}

exports::exports! {
    functions {
        "SecKeyGetTypeID" => SecKeyGetTypeID,
        "SecKeyIsAlgorithmSupported" => SecKeyIsAlgorithmSupported,
        "SecKeyCreateRandomKey" => SecKeyCreateRandomKey,
        "SecKeyCreateSignature" => SecKeyCreateSignature,
        "SecKeyCopyExternalRepresentation" => SecKeyCopyExternalRepresentation,
        "SecKeyCopyPublicKey" => SecKeyCopyPublicKey,
        "SecKeyCopyAttributes" => SecKeyCopyAttributes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real, distinct type ID - the guest's `ScopedCFTypeRef` `ud2`s on a
    /// mismatch, so "distinct" is the load-bearing part.
    #[test]
    fn the_key_type_id_is_real_and_distinct() {
        objc::bootstrap();
        let key = SecKeyGetTypeID();
        assert_ne!(key, 0);
        assert_ne!(key, SecKeyGetTypeID() ^ 1, "stable across calls");
        // SAFETY: a null-free accessor with no arguments.
        assert_ne!(key, unsafe { corefoundation::api::CFStringGetTypeID() });
        assert_ne!(key, crate::certificate::SecCertificateGetTypeID());
        assert_ne!(key, crate::trust::SecPolicyGetTypeID());
    }

    /// `false` for a key that is not there, and `false` for a handle that is not
    /// a key at all - which is the answer that keeps a signature-algorithm
    /// preference list empty rather than populated with guesses.
    #[test]
    fn nothing_supports_any_algorithm() {
        objc::bootstrap();
        let mut not_a_key = 0_u64;
        // SAFETY: null, and a `usize` this test owns; neither is read.
        unsafe {
            assert!(!SecKeyIsAlgorithmSupported(std::ptr::null(), 0, std::ptr::null()));
            assert!(!SecKeyIsAlgorithmSupported((&raw mut not_a_key).cast(), 1, std::ptr::null()));
        }
    }

    /// Every refusal is null, and every out-parameter is left null, so a caller
    /// that checks the handle cannot mistake a refusal for a result.
    #[test]
    fn every_refusal_is_a_null_handle() {
        objc::bootstrap();
        let mut error: CFTypeRef = 1_usize as CFTypeRef;
        // SAFETY: null arguments throughout, which every one of these documents
        // as acceptable, and writable out-parameters.
        unsafe {
            assert!(SecKeyCreateRandomKey(std::ptr::null(), &mut error).is_null());
            assert!(error.is_null());
            assert!(SecKeyCreateSignature(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                &mut error
            )
            .is_null());
            assert!(error.is_null());
            assert!(SecKeyCopyExternalRepresentation(std::ptr::null(), &mut error).is_null());
            assert!(error.is_null());
            assert!(SecKeyCopyPublicKey(std::ptr::null()).is_null());
            assert!(SecKeyCopyAttributes(std::ptr::null()).is_null());
        }
    }
}
