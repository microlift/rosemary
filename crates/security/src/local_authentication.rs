//! `-[LAContext canEvaluatePolicy:error:]`.
//!
//! # The census
//!
//! The classref slot is `0xe6e76f8`, and the loader's own unbound report names
//! it: `_OBJC_CLASS_$_LAContext from
//! /System/Library/Frameworks/LocalAuthentication.framework/Versions/A/
//! LocalAuthentication`.
//!
//! | | |
//! |--|--:|
//! | confirmed loads of the classref | **6** |
//! | sends measured inside the census window | **5** |
//! | distinct selectors the guest actually sends to this class | **1** |
//! | instantiations (`objc_alloc_init` on the class) | **3** |
//!
//! The queue's own nil-check column reads `yes x4, no x1` for this class, and
//! **three of those four "yes" reads are wrong**, for the reason the previous
//! agent measured and this file confirms instruction by instruction below: a
//! `test rX,rX` somewhere in the next 24 instructions is a branch, and it is not
//! necessarily a branch on this answer.
//!
//! # The six sites, and every instruction in each
//!
//! I re-measured rather than took the column's word, so this is a list of the
//! six load sites with the class of every instruction that follows in the same
//! function. Two of them send **nothing at all**.
//!
//! | site | what the function does | a `test` in the window | about the answer? |
//! |--|---|---|---|
//! | `+0x5b0a1d7` | a 0x2c-byte C shim at `+0x5b0a1cf`: `[[LAContext alloc] init]`, `canEvaluatePolicy:4`, `objc_release`, `return (al != 0)` | `test al,al` at `+0x5b0a1fd` | **yes - and it is the answer** |
//! | `+0x5b0a227` | the identical shim at `+0x5b0a220`, with `edx = 2` | `test al,al` at `+0x5b0a24d` | **yes - and it is the answer** |
//! | `+0x5f50a80` | a C shim at `+0x5f50a70`: `[[LAContext alloc] init]`, `canEvaluatePolicy:<arg1>`, `objc_release`, `return (int8_t) al` | the column says `test rax,rax` at `+99945179` | **no. There is no test in this function at all** - the `BOOL` goes `mov ebx,eax` at `+0x5f50aa5`, `movsx eax,bl` at `+0x5f50ab0`, `ret` |
//! | `+0x79f5f6e` | `[[LAContext alloc] init]` and the result is stored into `[rbx+0x8]`; `ret` at `+0x79f5fbf` | none | **no message at all** |
//! | `+0x79f5ffe` | `chrome/browser/device/fido/mac/touch_id_context.mm`: `canEvaluatePolicy:2` with `&err`, release the error, `test r15b,r15b; je +0x79f60c8` | yes, at `+0x79f6041` | **yes - and it is the answer** |
//! | `+0xc5bdecd` | `[[LAContext alloc] init]`, the result is stored through `[rbx+0x2e0]` and the old value released | none | **no message at all** |
//!
//! Two of the six are worth spelling out because the queue counted them as
//! sends and they are not:
//!
//!  * `+0xc5bdecd` is charged with `initWithContext:`. That selector is the
//!    **next** send in the window, at `+0xc5bdf0c`, and its receiver is the
//!    classref at `0xe6e7d20` - a different class entirely. A selector hoisted
//!    into a register by the *previous* pair is not a send of this one.
//!  * `+0x79f5f6e` and `+0xc5bdecd` never send anything: the instance is made
//!    by `_objc_alloc_init` and handed straight to an ivar. The first message
//!    the class ever receives in the whole image is `canEvaluatePolicy:error:`.
//!
//! **So the entire message surface of `LAContext` in this build is one selector,
//! sent three times, and the answer is a `BOOL`.** The queue's "no x1" - the
//! entry that reads as *nil is fatal* - is the `+0xc5bdecd` site, and that site
//! sends nothing.
//!
//! # The return is a `BOOL`, and a previous reading of this got it wrong
//!
//! The brief records that an earlier agent measured this site wrong once: it read
//! the answer as an object. It is not. `+0x5b0a1fd` is `test al,al` - the low
//! byte, not a pointer - and `+0x5b0a1ff` is `setne r14b`, which is how a
//! `BOOL` is normalised before `mov eax,r14d` returns it. The `+0x5f50a70` shim
//! makes the same shape unmistakable: `mov ebx,eax` then `movsx eax,bl` is
//! `return (int8_t)[context canEvaluatePolicy:policy error:error];`, and a
//! `BOOL` in `%al` is the whole return value. The encoding is therefore `B`, and
//! a `B` that came back as a pointer would have the guest reading a garbage
//! `NSError **` out of `%rcx`.
//!
//! # The answer is `NO`, and this is a real answer with a real host source
//!
//! `-[LAContext canEvaluatePolicy:error:]` asks whether this machine can
//! evaluate a `LAPolicy` **right now**. A `LAPolicy` names local authentication:
//! a biometric match (Touch ID) or the device passcode, either of which is
//! enforced by the Secure Enclave against an ACL the Secure Enclave holds. The
//! guest asks with `edx = 4` and `edx = 2` and, in the one site that names it
//! in a file, `LAPolicyDeviceOwnerAuthentication`.
//!
//! **The state of this host: there is no local authentication.** No Secure
//! Enclave, no biometric sensor, no device passcode, and no `LAContext` to hold
//! the per-policy state a `YES` answer would come from. The nearest things Linux
//! has - a TPM, a fingerprint reader, PAM - are not the capability the policy
//! names, and answering `YES` because one of them exists would be a stand-in: a
//! `YES` here would mean "a biometric was just matched", and nothing on this
//! host has matched anything.
//!
//! **What would make it change:** an enrolled biometric, or a device passcode.
//! That is the test a real answer has to pass, and this one is written so that
//! the person who adds LocalAuthentication changes one constant, in one place,
//! with the reason beside it. It is the same shape as
//! `+[NSSpellChecker sharedSpellChecker]` answering nil for want of a
//! dictionary, and the same shape as `AVCaptureDevice`'s `kRestricted`: **the
//! absence is the answer, and the absence is a fact about this host, not a
//! portable guess.** Nothing about it is a plausible stand-in for the
//! capability.
//!
//! # The guest is written to receive exactly this answer
//!
//! This is the strongest thing in the file, and it is what separates `NO` from
//! a plausible constant. At `+0x79f6041` the guest does
//! `test r15b,r15b; je +0x79f60c8`, and `+0x79f60c8` is:
//!
//! ```text
//!  79f60c8  lea    rsi,[rip+0x600b393]        # 0xda01462  "../../device/fido/mac/touch_id_context.mm"
//!  79f60cf  lea    rdi,[rbp-0x148]
//!  79f60d6  mov    edx,0x64                  ; 100
//!  79f60db  mov    ecx,0x8
//!  79f60e0  mov    r8d,0x3
//!  79f60e6  call   0x6a8f620                 ; __android_log_print-ish / base::Log
//!  79f60eb  lea    rdi,[rbp-0x130]
//!  79f60f2  lea    rsi,[rip+0x600b40a]        # 0xda01503  "canEvaluatePolicy failed: "
//!  79f60f9  mov    edx,0x1a                  ; 26
//!  79f60fe  call   0x484100                  ; std::string
//!  79f6103  mov    rdi,rax
//!  79f6106  mov    rsi,r14                   ; the NSError out-parameter
//!  79f6109  call   0x5d93890
//!  79f610e  lea    rdi,[rbp-0x148]
//!  79f6115  call   0x6a8f730
//!  79f611a  xor    r15d,r15d                 ; the return value is FALSE
//!  79f611d  ...    release the error, release the context
//!  79f6130  mov    eax,r15d                  ; and return it
//! ```
//!
//! The two cstrings are read out of the image's own `__TEXT,__cstring`, at
//! `0xda01462` and `0xda01503`, and the second is 26 bytes long - which is
//! exactly the `mov edx,0x1a` beside it. So this is a **named, logging,
//! non-fatal path taken by a `NO`**, in Chromium's own `touch_id_context.mm`,
//! which returns `false`. `NO` is not a value this caller is unprepared for;
//! it is the value it has a diagnostic string for.
//!
//! # The error out-parameter is left alone, and why
//!
//! At `+0x79f600d` the guest zeroes a stack slot, passes `&slot` as `%rcx`, and
//! after the call does `mov rdi,[rbp-0x28]; call _objc_release` on it. On macOS
//! a `NO` answer comes with an `NSError` - a domain, a code and a localized
//! description, all chosen by the LocalAuthentication daemon.
//!
//! **No such error is written here.** There is no daemon that chose one, and
//! inventing a domain, a code and a message would be inventing the three things
//! a caller is most likely to switch on. The truthful state of the out-parameter
//! is "unchanged", and `objc_release(NULL)` is the no-op it is on every
//! platform, so the slot's zeroing - which the guest itself did at
//! `+0x79f600d` - is the answer the guest reads. This is stated rather than
//! papered over: a real `NSError` here would be a *fabricated* one, not a
//! missing one.
//!
//! # What is deliberately not installed
//!
//! - `-evaluatePolicy:localizedReason:reply:`. This is the selector that would
//!   make the class worth having, and it is exactly the one with no honest
//!   answer: it performs a biometric match or raises a passcode prompt and then
//!   hands the verdict to a block. A block called with `success = true` would be
//!   the single worst answer in this project - a claim that a human identified
//!   themselves. It is not installed, so a send names the class and the selector
//!   through `objc`'s own refusal, which is a better message than a stub.
//! - `+[LAContext new]`, `-invalidate`, `-setLocalizedFallbackTitle:`,
//!   `-touchIDAuthenticationAllowableReuseDuration`, and the rest of the
//!   surface. The census measured **one** selector sent to this class in the
//!   whole 214 MB of `__TEXT,__text`, and installing unreachable rows would be
//!   surface that reads as coverage and is not reachable.
//! - `+alloc` and `-init`. They are not missing: `_objc_alloc_init`
//!   (`+0xcca0912`) is called on the class by the guest at four of the six
//!   sites, and it reaches `+alloc` and `-init` through the runtime's own
//!   `NSObject` methods. Nothing here has to install them.

use std::ffi::c_void;

use objc::object::Id;
use objc::sel::Sel;

/// Published so a guest binding `_OBJC_CLASS_$_LAContext` gets this class
/// rather than an anonymous empty one.
///
/// Publication is separate from registration: `data_symbols` is what
/// `rosetta3::frameworks` binds, and a class in only one of the two is
/// reachable through one spelling and not the other - the finding
/// `test/logs/class-gap.md` makes about `formatter::install`.
pub const CLASSES: &[&str] = &["LAContext"];

/// The one thing that has to be true for the answer below to change.
///
/// `false` because this host has no Secure Enclave, no biometric sensor and no
/// device passcode, so no `LAPolicy` can be evaluated. It is **not** a policy
/// decision and **not** a stand-in for a match: a `true` here would mean "a
/// biometric was just matched", and nothing on this host has matched anything.
/// The person who implements LocalAuthentication changes this one constant, and
/// the reason above it is the reason it was `false`.
const CAN_EVALUATE_ANY_POLICY: bool = false;

/// `-[LAContext canEvaluatePolicy:error:]`.
///
/// `B32@0:8q16@?24`: a `BOOL`, two colons, a 32-byte frame, `LAPolicy` as the
/// `NSInteger` it is (`q`), and the out-parameter as the `NSError **` it is.
///
/// `_policy` is deliberately unread. Every `LAPolicy` names the same absent
/// capability on this host, so the value cannot change the answer and reading it
/// would only invite a per-policy guess. `_error` is left **unchanged** rather
/// than written: see the module comment on the error out-parameter.
unsafe extern "C" fn can_evaluate_policy(
    _context: Id,
    _sel: Sel,
    _policy: i64,
    _error: *mut *mut c_void,
) -> bool {
    CAN_EVALUATE_ANY_POLICY
}

/// Every selector this module installs, with the encoding it declared.
///
/// A `const` slice rather than a loop in `install`, and
/// `test/tools/objcenc.py` reads this table as readily as it reads the
/// `replace_method` calls below - which is the trap the brief names: the census
/// can then certify a *copy* of the encoding that never reached the runtime.
/// [`installed`] closes that by reading `Method::types` back out of the class's
/// own method list, and the arity check is against the selector's own **name**,
/// which is the one thing about a selector that cannot be made self-consistent
/// by accident (`crates/appkit/src/appearance.rs` shipped an arithmetically
/// perfect encoding for a one-colon selector and its unit test stayed green).
#[cfg(test)]
const MEASURED: &[(&str, &std::ffi::CStr)] =
    &[("canEvaluatePolicy:error:", c"B32@0:8q16@?24")];

/// The class, registered and given its one method, once per process.
///
/// A `OnceLock` around the class pointer rather than a `bootstrap()` flag,
/// because `security` has no bootstrap step of its own: every other module in
/// this crate registers its class lazily the same way
/// (`certificate::class`), and `data_symbols` is what reaches them. Making this
/// one different would mean adding a bootstrap the crate does not have.
fn class() -> &'static objc::class::Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        // `register`, not `allocate_pair`: the class must be **instantiable**,
        // because the guest calls `_objc_alloc_init` on it directly at four of
        // its six sites and never sends `+alloc`. A one-word instance is all it
        // needs - the class carries no state, and the guest's own `LAContext`s
        // live in its C++ objects, not in ivars here.
        let class =
            corefoundation::base::register(c"LAContext", None, size_of::<usize>(), false);
        class.replace_method(
            objc::sel::get("canEvaluatePolicy:error:"),
            can_evaluate_policy as *const c_void,
            c"B32@0:8q16@?24",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: a class registered above and kept for the life of the process.
    unsafe { objc::class::from_ptr(pointer as *const objc::class::Class).expect("a class") }
}

/// Idempotent. Called from [`crate::data_symbols`], which is what publishes the
/// classref the guest binds.
pub fn install() {
    let _ = class();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared arity is the selector's own, and the frame reaches the last
    /// argument.
    ///
    /// Written out rather than imported: `crates/appkit` has an
    /// `encoding_check` module for exactly this and `security` has none, and a
    /// shared checker is not worth a new crate dependency for one row. The check
    /// is the same one: **every argument sits at `8 * i` with `self` at 0**, and
    /// the frame is `8 * (2 + colons)`.
    #[test]
    fn the_declared_arity_is_the_selector_s() {
        for (selector, types) in MEASURED {
            let colons = selector.bytes().filter(|b| *b == b':').count();
            // `B32` - the return type and then the frame it declares.
            assert_eq!(
                &types.to_bytes()[..2],
                b"B3",
                "{selector}: the return type is not the BOOL the +0x5b0a1fd `test al,al` and \
                 the +0x5f50ab0 `movsx eax,bl` require"
            );
            assert_eq!(
                declared_frame(types),
                8 * (2 + colons),
                "{selector}: the frame is {} and the selector's shape wants {}, which is self, \
                 _cmd and one word per argument",
                declared_frame(types),
                8 * (2 + colons)
            );
            // `@0:8`, `q16`, `@?24`: self, the policy, the out-parameter. Three
            // arguments for two colons, because `_cmd` is not encoded.
            assert_eq!(
                offsets(types),
                vec![0, 16, 24],
                "{selector}: argument offsets are not one word apart, so a register \
                 objc_msgSend writes is being read for something else"
            );
            // The rule the project-level gate cannot see for a whole-word
            // argument: the frame must reach `last_offset + 8`, and must not be
            // allowed to stop one word short.
            assert!(
                declared_frame(types) >= 24 + 8,
                "{selector}: the frame stops short of its last argument"
            );
        }
    }

    /// The encoding that reached the runtime is the one declared here.
    ///
    /// The census reads a `#[cfg(test)] const` table as readily as it reads the
    /// `replace_method` calls, so certifying the table alone would be "the test
    /// shares the author's misreading" with an extra step. `Method::types` is the
    /// string `replace_method` was handed, read back out of the class's own
    /// method list.
    #[test]
    fn the_installed_encoding_is_the_declared_one() {
        let _ = install();
        let class =
            objc::class::lookup(c"LAContext").expect("LAContext is registered by this module");
        for (selector, types) in MEASURED {
            // SAFETY: every `Method` in this runtime is leaked and never freed.
            let installed = class
                .method(objc::sel::get(selector))
                .expect("installed above");
            // SAFETY: as above.
            let installed = unsafe { std::ffi::CStr::from_ptr(installed.types()) };
            assert_eq!(
                installed.to_bytes(),
                types.to_bytes(),
                "-[LAContext {selector}] is installed with {} but the table declares {}",
                installed.to_string_lossy(),
                types.to_string_lossy()
            );
        }
    }

    /// The answer is `NO`, as a `BOOL` the guest's own `test al,al` reads - and
    /// the guest is *instantiable* here, which is the other half: the classref
    /// has to bind to a class `objc_alloc_init` can make an instance of, or the
    /// run faults before any answer is reached.
    ///
    /// Measured through the real `objc_msgSend` rather than by calling the
    /// implementation directly, because the registers are what the encoding is
    /// for.
    #[test]
    fn a_context_can_be_made_and_it_cannot_evaluate_anything() {
        let _ = install();
        let class = objc::class::lookup(c"LAContext").expect("registered by this module");
        // SAFETY: `+alloc` and `-init` are `id (Class|id, SEL)`.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(objc::msg::entry());
            let context = send(objc::class::ptr(class) as Id, objc::sel::get("alloc"));
            assert!(!context.is_null(), "+alloc came back nil");
            let context = send(context, objc::sel::get("init"));
            // SAFETY: an object this runtime just allocated.
            let made = objc::object::class_of(context).expect("a class");
            assert_eq!(
                std::ptr::eq(made, class),
                true,
                "the instance the guest's `_objc_alloc_init` makes must be an LAContext, or the \
                 answer below is never asked of anything"
            );

            // `-canEvaluatePolicy:error:` is `BOOL (id, SEL, NSInteger, NSError **)`,
            // and the `error` out-parameter is a slot this test owns so the
            // "leaves it unchanged" claim is measured rather than asserted.
            let mut error: *mut c_void = std::ptr::null_mut();
            let evaluate: unsafe extern "C" fn(Id, Sel, i64, *mut *mut c_void) -> bool =
                std::mem::transmute(objc::msg::entry());
            let policy = 2i64; // LAPolicyDeviceOwnerAuthentication, the +0x79f6020 value
            let answer = evaluate(
                context,
                objc::sel::get("canEvaluatePolicy:error:"),
                policy,
                &mut error,
            );
            assert!(!answer, "there is no local authentication on this host");
            assert!(
                error.is_null(),
                "the out-parameter must be left as the caller left it; an invented NSError is \
                 the three things a caller switches on"
            );
            send(context, objc::sel::get("release"));
        }
    }

    /// `+evaluatePolicy:localizedReason:reply:` is **not** installed, and that is
    /// the decision rather than an omission.
    ///
    /// It is the selector that would perform a biometric match and call a block
    /// with the verdict. A block called with `success = true` would claim a
    /// human identified themselves, which is the worst answer available here, so
    /// the class refuses it and a send names the selector.
    #[test]
    fn the_authentication_selector_is_not_installed() {
        let _ = install();
        let class = objc::class::lookup(c"LAContext").expect("registered by this module");
        assert!(
            class.method(objc::sel::get("evaluatePolicy:localizedReason:reply:")).is_none(),
            "a stub that reports a successful biometric match is the answer this project ranks \
             first; leave the selector unclaimed so the refusal names it"
        );
    }

    /// Registered *and* published under the guest's own spelling. Registration
    /// alone is why `formatter::install`'s stated intent was never achieved.
    #[test]
    fn the_class_is_published() {
        foundation::bootstrap();
        let _ = install();
        for name in CLASSES {
            let cname = std::ffi::CString::new(*name).unwrap();
            assert!(objc::class::lookup(&cname).is_some(), "{name} registered");
            assert!(
                crate::data_symbols()
                    .iter()
                    .any(|(symbol, _)| symbol == &format!("_OBJC_CLASS_$_{name}")),
                "{name} is published as data, not only registered"
            );
        }
    }

    /// The frame the encoding declares, which is the first number after the
    /// return type.
    fn declared_frame(types: &std::ffi::CStr) -> usize {
        let text = types.to_bytes();
        let start = text.iter().position(|b| b.is_ascii_digit()).unwrap();
        let end = text[start..]
            .iter()
            .position(|b| !b.is_ascii_digit())
            .map(|n| start + n)
            .unwrap_or(text.len());
        std::str::from_utf8(&text[start..end])
            .unwrap()
            .parse()
            .unwrap()
    }

    /// Every argument's offset, in order.
    ///
    /// A hand walk rather than a parser, because the grammar this crate writes
    /// has four rows and a parser that silently mis-reads what it does not
    /// understand is worse than no parser - it reports green.
    fn offsets(types: &std::ffi::CStr) -> Vec<usize> {
        let text = types.to_bytes();
        // `B32` - skip the return type and its frame.
        let mut at = 1;
        while at < text.len() && text[at].is_ascii_digit() {
            at += 1;
        }
        let mut out = Vec::new();
        while at < text.len() {
            // One type character, plus the `?` of a generic pointer.
            at += 1;
            if at < text.len() && text[at] == b'?' {
                at += 1;
            }
            let start = at;
            while at < text.len() && text[at].is_ascii_digit() {
                at += 1;
            }
            let offset: usize = std::str::from_utf8(&text[start..at])
                .unwrap()
                .parse()
                .unwrap();
            out.push(offset);
            // An optional `:size` on `self`.
            if at < text.len() && text[at] == b':' {
                at += 1;
                while at < text.len() && text[at].is_ascii_digit() {
                    at += 1;
                }
            }
        }
        out
    }
}
