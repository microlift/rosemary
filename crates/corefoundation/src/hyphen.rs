//! Hyphenation: the two functions Chromium calls, and what this host can
//! honestly answer.
//!
//! Apple's hyphenation is a set of language-specific pattern dictionaries that
//! ship in the system's resource directories.  A host without those directories
//! has no hyphenation, and `CFStringIsHyphenationAvailableForLocale` is the
//! function whose entire purpose is to say so before a caller builds anything on
//! top of it.  Chromium asks exactly that (`0xa591c69`, inside the function that
//! constructs a `Hyphenator`), and when the answer is false it takes the branch
//! that returns no hyphenator at all - so the truthful `false` is also the answer
//! that keeps the second function unreachable.
//!
//! That is worth being explicit about, because the alternative - answering
//! `true` and returning `kCFNotFound` from the second - would produce a hyphenator
//! object that can never hyphenate, and a caller that believes it can lay text
//! out with soft hyphens in it.
//!
//! The guest's second call site (`0xa591e6d`) passes six arguments in the order
//! `CFString.h` declares:
//!
//! ```c
//! CFIndex CFStringGetHyphenationLocationBeforeIndex(CFStringRef string, CFIndex location,
//!     CFRange limitRange, CFOptionFlags options, CFLocaleRef locale, UTF32Char *character);
//! ```
//!
//! which at `0xa591e52`-`0xa591e6d` is `rdi`=string, `esi`=location, `edx`=0 and
//! `ecx`=the source string's length (so `limitRange` is the whole string),
//! `r8d`=0 options, `r9`=the locale and `[rsp]`=0 for `character`.  A `CFRange`
//! is two `CFIndex` and is passed in two registers on this ABI, which is why
//! `edx`/`ecx` are the range and not one pointer.

// The C spellings are the interface, as they are in `api`.
#![allow(non_snake_case)]

use crate::api::{Boolean, CFRange};
use crate::base::{CFIndex, CFTypeRef};

/// `kCFNotFound`, the answer the header gives for "no hyphenation location".
///
/// `CFBase.h` puts it in the same enumeration as the run loop's activities and
/// gives it the value -1, which is also `CFIndex`'s own "not found" - so a caller
/// testing `< 0` and a caller comparing against `kCFNotFound` agree.
const NOT_FOUND: CFIndex = -1;

/// `Boolean CFStringIsHyphenationAvailableForLocale(CFLocaleRef locale)`
///
/// False, and the reason is a missing thing rather than a policy: the
/// dictionaries are not on this host.  Said once per locale, because a program
/// that asks about twenty locales should hear about it once and not twenty
/// times.
pub unsafe extern "C" fn CFStringIsHyphenationAvailableForLocale(locale: CFTypeRef) -> Boolean {
    // SAFETY: a locale or null; `as_string` is null-safe.
    let named = match unsafe { crate::string::as_string(locale) } {
        Some(s) => String::from_utf8_lossy(&s.utf8()).into_owned(),
        None => "the current locale".to_string(),
    };
    crate::base::refuse_once(
        "CFStringIsHyphenationAvailableForLocale",
        format!(
            "no for {named:?}: hyphenation is a table of language-specific patterns and this \
             host has no such tables.  Answering true would hand back a hyphenator that can \
             never hyphenate, so a caller would lay text out believing it had soft hyphens."
        ),
    );
    0
}

/// `CFIndex CFStringGetHyphenationLocationBeforeIndex(CFStringRef, CFIndex, CFRange, CFOptionFlags, CFLocaleRef, UTF32Char *)`
///
/// `kCFNotFound`, which is what the header specifies for "no location exists",
/// and `*character` is left alone because there is no character to suggest.
///
/// Unreachable from this guest - the only call site is behind
/// `CFStringIsHyphenationAvailableForLocale` answering true - so the answer here
/// is the specified one rather than a refusal with a made-up index.
pub unsafe extern "C" fn CFStringGetHyphenationLocationBeforeIndex(
    _string: CFTypeRef,
    _location: CFIndex,
    limit_range: CFRange,
    _options: u32,
    _locale: CFTypeRef,
    _character: *mut u32,
) -> CFIndex {
    // The header: "If the range length is negative, the behavior is undefined.
    // The range may be empty (length 0), in which case no hyphen location is
    // generated."  An empty range has its own answer, and it is the same one.
    if limit_range.length <= 0 {
        return NOT_FOUND;
    }
    NOT_FOUND
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the availability function: this host has no
    /// dictionaries, and saying so is the answer.
    #[test]
    fn no_locale_has_hyphenation_data_here() {
        crate::bootstrap();
        for identifier in ["en_US", "de_DE", "zh_CN", "xx_YY"] {
            let locale = crate::string::from_bytes(identifier.as_bytes(), false);
            // SAFETY: a locale and a null one, both of which the function takes
            // by name and never dereferences.
            assert_eq!(unsafe { CFStringIsHyphenationAvailableForLocale(locale) }, 0);
            assert_eq!(unsafe { CFStringIsHyphenationAvailableForLocale(std::ptr::null()) }, 0);
            // SAFETY: nothing else holds a reference.
            unsafe { crate::api::CFRelease(locale) };
        }
    }

    /// "No location" is `kCFNotFound` and not zero: a caller that tests the
    /// result against zero would read index 0 as a hyphenation point at the very
    /// start of the string.
    #[test]
    fn no_location_is_kcfnotfound() {
        crate::bootstrap();
        let range = CFRange {
            location: 0,
            // SAFETY: out-parameters are optional and this passes null.
            length: 10,
        };
        // SAFETY: every argument is either ignored or a plain value.
        let found = unsafe {
            CFStringGetHyphenationLocationBeforeIndex(
                std::ptr::null(),
                5,
                range,
                0,
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(found, NOT_FOUND);
        assert_eq!(found, -1, "CFBase.h's kCFNotFound");
    }
}
