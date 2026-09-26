//! `CFBundleGetPackageInfo`: the four-character codes that used to say what made
//! a bundle.
//!
//! # The signature, and where it came from
//!
//! ```c
//! void CFBundleGetPackageInfo(CFBundleRef bundle, UInt32 *packageType, UInt32 *packageCreator);
//! ```
//!
//! Two out-pointers and a void return, from `CFBundle.h`.  The guest's call site
//! (`0x45477bb`-`0x45477d5`, in the Carbon hot-key signature builder) passes
//! exactly that: `rdi` = the main bundle, `esi` = 0 for `packageType` - the
//! header's "pass NULL if you do not need this" - and `rdx` = the stack slot it
//! pre-loaded with `0x3f3f3f3f`, `'????'`.
//!
//! # The answer, and why zero is the honest one
//!
//! The codes are read from a `PKG___` resource inside the bundle, which is how
//! Classic Mac OS applications declared "this is a `TEXT`/`WBOX` document of tool
//! `Mr B`".  Nothing modern has one: a signed macOS application carries its
//! identity in a code signature and its version in `Info.plist`, and Apple's own
//! `CFBundle.c` no longer contains this function at all - it is exported and
//! inert.  So for every bundle that exists today there is no `packageType` and no
//! `packageCreator` to report, and the codes CF reports are zero.
//!
//! Zero is also what the guest already expects: it pre-loads `'????'`, which is
//! `0x3f3f3f3f`, so a caller that gets zero back can tell the difference between
//! "no code" and "some code", and this guest does not - it feeds whatever it gets
//! into a Carbon hot-key signature, where `'????'` is the value a bundle with no
//! signature has.

// The C spellings are the interface, as they are in `api`.
#![allow(non_snake_case)]

use crate::base::CFTypeRef;

/// `'????'`, the four-character code for "no code", as a big-endian `UInt32`.
pub const NONE: u32 = 0x3f3f_3f3f;

/// `void CFBundleGetPackageInfo(CFBundleRef bundle, UInt32 *packageType, UInt32 *packageCreator)`
///
/// Both out-parameters are written, always, because the function returns void
/// and a caller that passes a pointer has no way to ask whether it was filled -
/// the header's contract is that it is.
pub unsafe extern "C" fn CFBundleGetPackageInfo(
    bundle: CFTypeRef,
    package_type: *mut u32,
    package_creator: *mut u32,
) {
    // SAFETY: each out-parameter is either null or a `UInt32 *` the caller owns.
    unsafe {
        if !package_type.is_null() {
            package_type.write(0);
        }
        if !package_creator.is_null() {
            package_creator.write(0);
        }
    }
    // Said once, and only for a bundle: `NULL` is a caller asking about nothing,
    // and a diagnostic about nothing is noise.
    if bundle.is_null() {
        return;
    }
    crate::base::refuse_once(
        "CFBundleGetPackageInfo",
        "no package type or package creator for this bundle: both codes come from a PKG___ \
         resource, and no modern bundle carries one - a signed application states its identity in \
         a code signature instead.  Both out-parameters written as 0, which is what Apple's \
         CoreFoundation reports for a bundle with no PKG___ resource, and which is distinct from \
         the '????' (0x3f3f3f3f) the caller pre-loaded as its own default."
            .to_string(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Void return and two out-parameters, so the contract is that both are
    /// written - a caller that asked has no other way to find out.
    #[test]
    fn both_codes_are_written() {
        crate::bootstrap();
        let mut package_type = NONE;
        let mut package_creator = NONE;
        // SAFETY: a bundle this test made and two live `UInt32`s.
        unsafe {
            CFBundleGetPackageInfo(
                crate::string::from_bytes(b"/Applications/Chromium.app", false),
                &mut package_type,
                &mut package_creator,
            )
        };
        assert_eq!(package_type, 0, "no PKG___ resource, so no type");
        assert_eq!(package_creator, 0, "and no creator");
        assert_ne!(package_creator, NONE, "which is not the '????' the caller defaulted to");
    }

    /// The header's "pass NULL if you do not need this", which the guest's own
    /// call site does for `packageType`.
    #[test]
    fn a_null_out_parameter_is_allowed() {
        crate::bootstrap();
        let mut package_creator = NONE;
        // SAFETY: a null first out-parameter is what the header documents.
        unsafe {
            CFBundleGetPackageInfo(std::ptr::null(), std::ptr::null_mut(), &mut package_creator)
        };
        assert_eq!(package_creator, 0);
    }
}
