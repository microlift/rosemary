//! The one refusal every question in this crate gets, and the `NSError` it
//! arrives with.
//!
//! # What is missing
//!
//! OpenDirectory is not a library this host is missing one entry point of. It
//! is a **Mach service**, and the guest itself says so: its embedded sandbox
//! profile in `__TEXT,__const` allows `mach-lookup` for
//!
//! ```text
//! @0xced1765  com.apple.system.opendirectoryd.libinfo
//! @0xced3004  com.apple.system.opendirectoryd.membership
//! @0xced482e  com.apple.system.opendirectoryd.membership
//! @0xced4c3f  com.apple.system.opendirectoryd.membership
//! ```
//!
//! Four `global-name` allowances, in the Framework itself, in a binary with no
//! OpenDirectory to read them from. On this host there is no `bootstrap_look_up`,
//! no `directoryd`, no `odutil` and no `/var/db/dslocal` - all four measured, and
//! all four the absence of a *service*, not of a *header*.
//!
//! So `+[ODSession defaultSession]` has no true answer but nil: an
//! `ODSessionRef` is a client handle to that service, and there is nothing to
//! open a handle to. That is not a "polite segfault" and it is not a guess -
//! see [`crate`] for the measured proof that the caller has a path for it.

use objc::object::Id;

/// The domain of every error this crate produces.
///
/// **Not** Apple's `ODErrorDomain`, and deliberately: that string is not
/// derivable here. It appears nowhere in the guest's `__TEXT` (searched for, and
/// absent - see the crate note), and the guest never reads an error's domain in
/// this path. Inventing Apple's spelling would be a claim about a framework that
/// is not on this host, in order to look like the real thing.
///
/// It is a domain a caller can recognise as *this runtime speaking*, which is
/// the honest thing for a refusal to be.
pub const DOMAIN: &str = "rosetta3.OpenDirectoryUnavailable";

/// The one code this crate produces.
///
/// **Negative**, and that is load-bearing rather than stylistic. OpenDirectory's
/// own codes are small positive integers (`eODSuccess` is `0`), so a negative
/// code cannot be mistaken for one of Apple's by a caller that switches on it.
/// The same reasoning, and the same choice, is `NSURL_ERROR_UNKNOWN` in
/// `crates/foundation/src/error.rs`.
pub const CODE: i64 = -1;

/// Say once on stderr that a question is not answered here, and why.
///
/// Once per `(what, why)`, because every one of these is reachable from a loop
/// in principle and a refusal that repeats per iteration buries the rest of the
/// run. This is the same discipline as `corefoundation::base::refuse_once` and
/// `security::status::refuse`, and for the same reason: a refusal that says
/// nothing is indistinguishable from a value.
pub fn say(what: &str) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static SAID: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let fresh = SAID
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_default()
        .insert(what.to_string());
    if fresh {
        eprintln!(
            "rosetta3: opendirectory: {what}: not answered - there is no OpenDirectory service on \
             this host (no bootstrap_look_up, no directoryd, no /var/db/dslocal; the guest's own \
             sandbox profile names com.apple.system.opendirectoryd.{{libinfo,membership}}), so \
             nothing can be enumerated. The caller is told nil, which is what its own source \
             checks for."
        );
    }
}

/// The `NSError` a refusal hands back through an `NSError **` out-parameter.
///
/// # Safety
///
/// `out` must be null, or a pointer to a writable `Id`.
pub unsafe fn set(out: *mut Id, what: &str) {
    if out.is_null() {
        return;
    }
    // SAFETY: the caller's obligation, checked immediately above.
    unsafe {
        *out = foundation::error::make(DOMAIN, CODE, what);
    }
}

/// Refuse: name the selector on stderr, hand back nil, and - where the signature
/// has one - fill the `NSError **` with [`DOMAIN`]/[`CODE`].
///
/// # Safety
///
/// `out` must be null, or a pointer to a writable `Id`.
pub unsafe fn refuse(out: *mut Id, what: &str) -> Id {
    say(what);
    // SAFETY: the caller's obligation, forwarded to `set`.
    unsafe { set(out, what) };
    std::ptr::null_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refusal_is_nil_and_fills_the_error_out_parameter() {
        // `NSError`'s accessors are installed by `foundation`'s bootstrap, so a
        // test that reads one back has to have bootstrapped: otherwise it is
        // testing a class with no methods, which looks exactly like a refusal
        // that produced a broken error.
        foundation::bootstrap();
        let mut out: Id = std::ptr::null_mut();
        let got = unsafe { refuse(&raw mut out, "unit test") };
        assert!(got.is_null(), "a refusal must not hand back an object");
        assert!(!out.is_null(), "a nil result owes the caller an error");
        let error = out;

        // Read the three fields back through the runtime's own dispatch, so a
        // refusal whose error is unreadable - which is a refusal that says
        // nothing - fails here rather than in a guest.
        let send = |name: &str| -> Id {
            let sel = objc::sel::get(name);
            let f: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
                unsafe { std::mem::transmute(objc::msg::entry()) };
            unsafe { f(error, sel) }
        };
        let domain = send("domain");
        assert!(!domain.is_null(), "the refusal's error has no domain");
        let code: i64 = {
            let sel = objc::sel::get("code");
            let f: unsafe extern "C" fn(Id, objc::sel::Sel) -> i64 =
                unsafe { std::mem::transmute(objc::msg::entry()) };
            unsafe { f(error, sel) }
        };
        assert_eq!(code, CODE, "the refusal's error carries another code");
        let text = send("localizedDescription");
        assert!(!text.is_null(), "the refusal's error has no description");
        objc::arc::release(text);
        objc::arc::release(domain);
    }

    #[test]
    fn a_null_out_parameter_is_tolerated_rather_than_written() {
        // The guest passes `error:nil` at both `valuesForAttribute:` sites, so
        // this is a shape it really uses, not a defensive invention.
        let got = unsafe { refuse(std::ptr::null_mut(), "unit test, null out") };
        assert!(got.is_null());
    }

    #[test]
    fn the_code_cannot_be_mistaken_for_one_of_apples() {
        // OpenDirectory's own codes are non-negative (`eODSuccess` is 0), which
        // is the whole reason this one is negative.
        const { assert!(CODE < 0, "a positive code could be an OpenDirectory error code") };
        assert!(!DOMAIN.contains("ODError"), "must not claim Apple's domain");
    }
}
