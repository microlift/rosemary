#![allow(non_snake_case)]

//! `SecCertificateRef`: a DER certificate, and nothing more.
//!
//! Three functions, and the whole of what a certificate is to the API that holds
//! one: the type ID, a constructor from DER, and the DER back out again.
//!
//! **Why a real object rather than a refusal.** Chromium compares
//! `CFGetTypeID(ref)` against `SecCertificateGetTypeID()` and *traps* on a
//! mismatch. `net/cert/internal/trust_store_mac.cc` inlines that comparison at
//! `Framework+0x5d93434`:
//!
//! ```text
//! 5d93422  call   _CFGetTypeID
//! 5d9342a  call   _SecCertificateGetTypeID
//! 5d93434  cmp    r14,rcx
//! 5d93437  cmove  rax,QWORD PTR [rbx]    ; a match: hand the object on
//! 5d93441  sete   cl                     ; null object, or a match
//! 5d93449  test   rax,rax
//! 5d9344c  setne  dl
//! 5d93450  or     dl,cl
//! 5d93452  jne    <return>
//! 5d93454  int3
//! 5d93456  ud2                            ; neither: the CHECK has failed
//! ```
//!
//! So a type ID that is not the object's own is not a wrong answer that shows
//! up later: it is a `ud2` in the middle of the certificate path. The same shape
//! is instantiated for `SecKeyRef` (`SecKeyGetTypeID`) and for `SecPolicyRef`
//! (`SecPolicyGetTypeID`, called at `Framework+0x6747cb8`).
//!
//! **What the object holds.** The DER it was made from, and nothing derived from
//! it. No signature is checked, no issuer resolved, no validity computed - so
//! nothing here can be a trust decision, which is the line this crate is written
//! on. `SecCertificateCopyData` hands back exactly the bytes that went in, which
//! is the entire round trip macOS performs for a certificate built from DER.

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use objc::class::Class;
use objc::object::Id;

/// A certificate: the `CFData` of DER it was created from.
#[repr(C)]
struct Body {
    isa: *const Class,
    der: CFTypeRef,
}

fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        let class = corefoundation::base::register(
            c"__R3SecCertificate",
            None,
            size_of::<Body>(),
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
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("a certificate class") }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: objc::sel::Sel) {
    // SAFETY: this class's object layout and its owned `CFData`.
    unsafe {
        let body = receiver as *mut Body;
        corefoundation::api::CFRelease((*body).der);
        objc::arc::finalize(receiver);
    }
}

/// Whether `object` is a certificate this module made.
///
/// # Safety
///
/// `object` is null or an object this runtime allocated.
pub(crate) unsafe fn is_certificate(object: CFTypeRef) -> bool {
    // SAFETY: the caller's contract.
    unsafe { corefoundation::base::is_kind_of(object, class()) }
}

/// `SecCertificateRef SecCertificateCreateWithData(CFAllocatorRef allocator,
/// CFDataRef data)`
///
/// The DER is kept, not parsed. The one thing checked is that it *is* a DER
/// certificate rather than an arbitrary blob: `Certificate ::= SEQUENCE { ... }`
/// is the universal constructed tag `0x30` followed by a definite length that
/// covers exactly the buffer. That is a real check and not a formality - macOS
/// rejects a buffer that is not a certificate, and accepting one silently would
/// make `SecCertificateCopyData` agree with a caller about bytes that were never
/// a certificate at all.
///
/// Null for anything else, which is macOS's own answer. The allocator is ignored,
/// as it is for every CF object this runtime makes.
///
/// # Safety
///
/// `data` is null or a `CFData`.
pub unsafe extern "C" fn SecCertificateCreateWithData(
    _allocator: CFTypeRef,
    data: CFTypeRef,
) -> CFTypeRef {
    // SAFETY: the caller's contract; the accessor answers `None` for anything
    // that is not a `CFData`, which is the null answer macOS gives too.
    let Some(der) = (unsafe { corefoundation::collections::data(data) }) else {
        return std::ptr::null();
    };
    if !is_der_certificate(der) {
        return std::ptr::null();
    }
    // SAFETY: `Body` is the registered layout.
    let body = unsafe { corefoundation::base::allocate::<Body>(class()) };
    if body.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a freshly allocated body; the retain is what makes the `dealloc`
    // above's release correct rather than one too many.
    unsafe {
        (*body).der = corefoundation::api::CFRetain(data);
        body as CFTypeRef
    }
}

/// Whether `bytes` opens as a DER `SEQUENCE` whose length is the whole buffer.
fn is_der_certificate(bytes: &[u8]) -> bool {
    if bytes.len() < 2 || bytes[0] != 0x30 {
        return false;
    }
    // The header is the tag, the length byte, and however many extra bytes the
    // long form spends - so the total is `2 + extra + length`, not `2 + length`.
    let (length, header) = match bytes[1] {
        // 0x80 is the indefinite form, which DER does not use in a certificate.
        n if n < 0x80 => (usize::from(n), 2usize),
        // 0x81..=0x84: a one-to-four byte long-form length.
        0x81..=0x84 => {
            let extra = usize::from(bytes[1] & 0x7f);
            if bytes.len() < 2 + extra {
                return false;
            }
            let mut length = 0usize;
            for byte in &bytes[2..2 + extra] {
                length = match length
                    .checked_mul(256)
                    .and_then(|value| value.checked_add(usize::from(*byte)))
                {
                    Some(value) => value,
                    None => return false,
                };
            }
            // DER's shortest form: `81 7f` encodes 127 in two bytes, and DER
            // does not permit it.
            if length < 0x80 {
                return false;
            }
            (length, 2 + extra)
        }
        _ => return false,
    };
    // Exactly the buffer: a certificate's outer SEQUENCE is the whole thing, so
    // trailing bytes mean this is not one certificate.
    length.checked_add(header) == Some(bytes.len())
}

/// `CFDataRef SecCertificateCopyData(SecCertificateRef certificate)`
///
/// A `Copy`, so the caller owns what it is handed: a fresh `CFData` over the same
/// bytes.
///
/// # Safety
///
/// `certificate` is null or a certificate this module made.
pub unsafe extern "C" fn SecCertificateCopyData(certificate: CFTypeRef) -> CFTypeRef {
    // SAFETY: the caller's contract.
    if !unsafe { is_certificate(certificate) } {
        return std::ptr::null();
    }
    // SAFETY: checked just above, so this is a `Body` holding a `CFData`.
    let der = unsafe { (*(certificate as *const Body)).der };
    // SAFETY: this module's own `CFData`.
    let Some(bytes) = (unsafe { corefoundation::collections::data(der) }) else {
        return std::ptr::null();
    };
    // A zero-length certificate cannot be constructed, so this is never an empty
    // `CFData`: a null here would read as "the copy failed", which is a
    // different thing.
    corefoundation::collections::new_data(bytes.clone())
}

/// `CFTypeID SecCertificateGetTypeID(void)`
pub extern "C" fn SecCertificateGetTypeID() -> corefoundation::base::CFTypeID {
    corefoundation::base::type_id(class())
}

exports::exports! {
    functions {
        "SecCertificateCreateWithData" => SecCertificateCreateWithData,
        "SecCertificateCopyData" => SecCertificateCopyData,
        "SecCertificateGetTypeID" => SecCertificateGetTypeID,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A certificate built from DER gives the same DER back, through a type ID
    /// that agrees with the object's own class - which is the comparison the
    /// guest traps on.
    #[test]
    fn a_certificate_is_the_der_it_was_made_from() {
        objc::bootstrap();
        // A real certificate shape: SEQUENCE { SEQUENCE { INTEGER 1 }, SEQUENCE {},
        // BIT STRING }. Eleven bytes of content, so the length is 0x0b.
        let der: &[u8] = &[
            0x30, 0x0b, 0x30, 0x03, 0x02, 0x01, 0x01, 0x30, 0x00, 0x03, 0x02, 0x01, 0x00,
        ];
        let data = corefoundation::collections::new_data(der.to_vec());
        // SAFETY: a `CFData` this test made.
        let certificate = unsafe { SecCertificateCreateWithData(std::ptr::null(), data) };
        assert!(!certificate.is_null());
        // SAFETY: a certificate the call above made.
        unsafe {
            assert!(is_certificate(certificate));
            assert_eq!(
                corefoundation::api::CFGetTypeID(certificate),
                SecCertificateGetTypeID(),
                "the guest CHECKs this and traps on a mismatch"
            );
            let copy = SecCertificateCopyData(certificate);
            assert!(!copy.is_null());
            assert_eq!(corefoundation::collections::data(copy).unwrap(), der);
            // A `Copy`, so the two are separate objects with equal contents.
            assert_ne!(copy, data);
            corefoundation::api::CFRelease(copy);
            corefoundation::api::CFRelease(certificate);
        }
        // SAFETY: a `CFData` this test made.
        unsafe { corefoundation::api::CFRelease(data) };
    }

    /// The short-form length that covers exactly the buffer, a long-form one that
    /// does, and the shapes a certificate is not.
    #[test]
    fn only_a_der_sequence_is_a_certificate() {
        assert!(is_der_certificate(&[0x30, 0x00]));
        assert!(is_der_certificate(&[0x30, 0x03, 0x02, 0x01, 0x2a]));
        // `81 7f` is 127 in long form, which DER does not permit.
        assert!(!is_der_certificate(&[0x30, 0x81, 0x7f]));
        // Long form, 128 bytes of content behind a three-byte header: 131 in all.
        let mut long = vec![0x30, 0x81, 0x80];
        long.extend(std::iter::repeat_n(0u8, 0x80));
        assert_eq!(long.len(), 131);
        assert!(is_der_certificate(&long));
        // Long form whose length overshoots the buffer by one.
        let mut short = vec![0x30, 0x81, 0x80];
        short.extend(std::iter::repeat_n(0u8, 0x7f));
        assert!(!is_der_certificate(&short));
        // A trailing byte: the SEQUENCE is not the whole buffer.
        assert!(!is_der_certificate(&[0x30, 0x01, 0x05, 0x00]));
        // Not a SEQUENCE, and too short to be one.
        assert!(!is_der_certificate(&[0x04, 0x01, 0x05]));
        assert!(!is_der_certificate(&[0x30]));
        assert!(!is_der_certificate(&[]));
        // A length that runs off the end of the buffer, and a five-byte long form
        // (which X.690 does not allow at all).
        assert!(!is_der_certificate(&[0x30, 0x7f, 0x00]));
        assert!(!is_der_certificate(&[0x30, 0x85, 0x01]));
        // The indefinite form.
        assert!(!is_der_certificate(&[0x30, 0x80, 0x00, 0x00]));
    }

    /// A `CFData` that is not a certificate, and something that is not a
    /// `CFData` at all, are both null - the way macOS answers a malformed one.
    #[test]
    fn a_blob_is_not_a_certificate() {
        objc::bootstrap();
        let data = corefoundation::collections::new_data(vec![1, 2, 3, 4]);
        // SAFETY: a `CFData` this test made, holding four bytes that are not DER.
        assert!(unsafe { SecCertificateCreateWithData(std::ptr::null(), data) }.is_null());
        // SAFETY: null is the documented "no data" answer.
        assert!(unsafe { SecCertificateCreateWithData(std::ptr::null(), std::ptr::null()) }.is_null());
        // SAFETY: a `CFData` this test made.
        unsafe { corefoundation::api::CFRelease(data) };
    }
}
