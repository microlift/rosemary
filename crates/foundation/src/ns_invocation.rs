//! `NSInvocation`: refused by name, all six measured selectors.
//!
//! # The census
//!
//! The classref slot is `0xe6e78f8`, and the loader's own unbound report names
//! it: `_OBJC_CLASS_$_NSInvocation from /System/Library/Frameworks/
//! Foundation.framework`.
//!
//! | | |
//! |--|--:|
//! | confirmed loads of the classref | **3** |
//! | `objc_msgSend`-family sends the queue's 96-byte window found | **7** |
//! | distinct selectors this module refuses | **6** |
//! | files the three loads are in | **1** |
//!
//! The queue's `selectors` column reads `?` x7 because the selectors are hoisted
//! into stack slots up to **0x51a bytes** before the send - which is the trap
//! `AGENT-BRIEF.md` names ("a selector value can be hoisted into a stack slot
//! and sent much later"). They are recoverable, because the function fills the
//! slots from `__objc_selrefs` in one straight run at `+0x80a2cf7 .. +0x80a2da4`
//! and never writes them again:
//!
//! ```text
//!  80a2d2d  mov  rax,[0xe6e8ac8] = respondsToSelector:   -> [rbp-0x328]
//!  80a2d3b  mov  rax,[0xe6eab40] = instanceMethodSignatureForSelector:
//!                                                     -> [rbp-0x308]
//!  80a2d49  mov  rax,[0xe6eb8c0] = invocationWithMethodSignature:
//!                                                     -> [rbp-0x300]
//!  80a2d57  mov  rax,[0xe6eb8c8] = setSelector:          -> [rbp-0x2f8]
//!  80a2d65  mov  rax,[0xe6e91b0] = setTarget:            -> [rbp-0x2f0]
//!  80a2d73  mov  rax,[0xe6eb8d0] = invoke                -> [rbp-0x2e8]
//!  80a2d81  mov  rax,[0xe6eb8b8] = getReturnValue:       -> [rbp-0x320]
//!  80a2d8f  mov  rax,[0xe6eb8d8] = setArgument:atIndex:  -> [rbp-0x318]
//!  80a2d9d  mov  rax,[0xe6e86a8] = numberWithBool:
//! ```
//!
//! # What the guest does with it, from the bytes
//!
//! All three sites are the same six-step chain. The first, in full:
//!
//! ```text
//!  80a4fcc  mov  r14,QWORD PTR [rip+0x6642925]   # 0xe6e78f8  the class
//!  80a4fd3  mov  rdi,[rbp-0x280] ; 80a4fda mov rsi,[rbp-0x278]
//!  80a4fe1  call rax                            ; the target object
//!  80a4fe6  mov  rsi,[rbp-0x308] = instanceMethodSignatureForSelector:
//!  80a4fed  mov  rdx,r13                         ; the selector, from the
//!                                              ;   statement the user typed
//!  80a4ff0  call objc_msgSend                   ; -[target instanceMethod...]
//!  80a5001  mov  rdi,r14                        ; the class
//!  80a5004  mov  rsi,[rbp-0x300] = invocationWithMethodSignature:
//!  80a500b  mov  rdx,rax                        ; the signature just fetched
//!  80a500e  call objc_msgSend                   ; +[NSInvocation invocation...]
//!  80a501c  mov  r14,rax                        ; the invocation
//!  80a5028  mov  rdi,r14 ; 80a502b mov rsi,[rbp-0x2f8] = setSelector:
//  80a5035  call objc_msgSend                   ; -[inv setSelector:]
//!  80a5042  mov  rdi,r14 ; 80a5045 mov rsi,[rbp-0x2f0] = setTarget:
//  80a504c  call objc_msgSend                   ; -[inv setTarget:]
//!  80a5052  mov  ecx,0x2
//!  80a5057  mov  rdi,r14 ; 80a505a mov rsi,[rbp-0x318] = setArgument:atIndex:
//  80a5061  lea  rdx,[rbp-0x2b0]
//!  80a5068  call objc_msgSend                   ; -[inv setArgument:atIndex:2]
//!  80a506e  mov  rdi,r14 ; 80a5071 mov rsi,[rbp-0x2e8] = invoke
//!  80a5078  call objc_msgSend                   ; -[inv invoke]
//!  80a5085  mov  rdi,r14 ; 80a5088 mov rsi,[rbp-0x320] = getReturnValue:
//  80a508f  lea  rdx,[rbp-0x2b0]
//!  80a5096  call objc_msgSend                   ; -[inv getReturnValue:]
//! ```
//!
//! **Not one of the seven steps tests anything.** The signature at `+0x80a4ff0`
//! is not tested before it is handed to `+invocationWithMethodSignature:`, the
//! invocation is not tested before `-setSelector:`, and the raw word
//! `-getReturnValue:` writes into `[rbp-0x2b0]` is not tested before the caller
//! reads it. `-retainArguments` is not called on this path.
//!
//! # Who the caller is, and how confidently
//!
//! `test/tools/whichfile.py` brackets **both** the leading and the trailing
//! `__FILE__` marker of the region at the same file - `../../ui/accessibility/
//! platform/inspect/ax_call_statement_invoker_mac.mm` at `+0x80a4d9e` and again
//! at `+0x80a5220` - which is the tightest bracket this image offers and is
//! what `class-gap.md` already recorded for this class. A second, independent
//! fact points the same way and is *inside* the function:
//!
//! ```text
//!  80a54e5  cmp  QWORD PTR [rdx+0x20],0x5
//!  80a54ef  mov  ecx,DWORD PTR [rax]
//!  80a54f1  mov  edx,0x534c4146          ; "FALS"
//!  80a54f6  xor  ecx,edx
//!  80a54f8  movzx eax,BYTE PTR [rax+0x4]
//!  80a54fc  xor  eax,0x45                 ; 'E'
//!  80a54ff  or   eax,ecx
//!  80a5501  setne cl
//! ```
//!
//! which is a comparison of a literal against `"FALSE"` - the boolean-literal
//! arm of a statement parser. `ax_call_statement_invoker_mac.mm` is the
//! Accessibility Inspector's **"call a method on this AX object"** panel: the
//! target is an `AXUIElement`, the selector is one the user typed, and the
//! arguments are parsed out of the same string (`+[NSNumber numberWithBool:]` is
//! loaded at `+0x80a2d9d` for the `TRUE`/`FALSE` arm).
//!
//! # The missing capability, named
//!
//! `NSInvocation` is a **dynamic dispatch trampoline**: it takes a method
//! signature chosen at run time, lets a caller fill the argument slots, and then
//! performs the call with the ABI that signature describes. To answer it
//! truthfully this runtime would have to
//!
//! 1. read a `methodSignature` and derive a frame layout from its
//!    `NSMethodSignature` - which means reading **encodings**, and
//! 2. perform the call, and
//! 3. place the callee's return value where `objc_msgSend` would have put it,
//!    for **every** return convention `objc_msgSend` has.
//!
//! (1) is the part this repository cannot check. `MEMORY.md` records it
//! directly: **a method encoding is never read at dispatch**, every rule in this
//! tree is about a *declaration*, and `-floatValue` was once wired to an `f64`
//! return under a `d16` encoding, answered `0.0` for nearly every input, and
//! **nothing here could catch it**. `NSInvocation` is the one class whose entire
//! job is to make a dispatch depend on a signature, so it is the one class where
//! a wrong answer is guaranteed to be silent.
//!
//! (3) is the part that makes a wrong answer *indistinguishable from a right
//! one*. `-[NSInvocation getReturnValue:]` writes one raw word into a buffer the
//! guest supplied, and the guest then reads that word as the AX attribute it
//! asked for. A real accessibility server, a fabricated value and a zero all
//! produce the same shape of output: a number in the inspector's value column.
//! The guest cannot tell which it got, and neither can a reader of the run log.
//!
//! # Why nil is not used, in this file's own terms
//!
//! Because on the `NO` path in the guest's arithmetic every value is a value.
//! `-[inv getReturnValue:]` on a null invocation is a message to nil and writes
//! nothing, so the word the guest then reads is whatever was already in the
//! stack slot - and at `+0x80a507e` that slot is being **initialised** by the
//! guest itself:
//!
//! ```text
//!  80a507e  mov  BYTE PTR [rbp-0x2b0],0xaa
//! ```
//!
//! `0xaaaa...aaaa` is the pattern this compiler fills an uninitialised 64-bit
//! slot with (the same constant appears at `+0x3a91cef` and `+0x3a8dda6` in this
//! image). So a nil here does not produce zero, it produces **`0xaaaa…aaaa`**,
//! and the guest would print that as the value of a real accessibility attribute.
//! That is the outcome this project ranks first, produced by the mechanism
//! designed to avoid it.
//!
//! # Placement
//!
//! `NSInvocation` is in `Foundation.framework` and the loader's report names it,
//! so this module is in `foundation` and needs no argument of the kind
//! `uniform_type_identifiers.rs` makes.
//!
//! # What a real answer needs
//!
//! The third piece: a way to **call** an Objective-C method from a signature
//! rather than from an IMP chosen at dispatch. This tree has no such thing, and
//! building one is not the same work as building an answer to any single
//! selector - it is a second dispatch path with its own ABI rules, none of which
//! any rule in this repository can check. That is the honest reason for the
//! refusal, and it is a reason about **this tree**, not about the host: the
//! host could answer `+[NSInvocation invocationWithMethodSignature:]` truthfully
//! and a different runtime might.

use std::collections::BTreeSet;
use std::sync::Mutex;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// Published so a guest binding `_OBJC_CLASS_$_NSInvocation` gets a class that
/// **names itself** rather than an anonymous empty one.
pub const CLASSES: &[&str] = &["NSInvocation"];

/// What is missing, said in the refusal so the line names the capability and
/// not merely the class.
const MISSING_SIGNATURE_DISPATCH: &str = "The missing capability is dispatch from a method \
    signature rather than from an IMP chosen at dispatch, which is NSInvocation's entire job. \
    Answering it would mean deriving a frame layout from an NSMethodSignature, performing the call, \
    and placing the callee's return value where objc_msgSend would have put it for every return \
    convention objc_msgSend has. The first step is the one this repository cannot check, because a \
    method encoding is never read at dispatch and every rule in this tree is about a declaration - \
    which is how -floatValue was once wired to an f64 return under a d16 encoding and answered 0.0 \
    for nearly every input with nothing able to catch it. The last step is the one that makes a \
    wrong answer indistinguishable from a right one: -getReturnValue: writes one raw word and the \
    guest reads it as the accessibility attribute it asked for.";

/// The callers, said in the refusal, by name and by address.
const CALLERS: &str = "All three send sites are the same six-step chain, and not one of the seven \
    steps tests anything. The first is at 0x80a4fcc: the class into %r14, \
    -[target instanceMethodSignatureForSelector:] at 0x80a4ff0, \
    +[NSInvocation invocationWithMethodSignature:] at 0x80a500e, -setSelector: at 0x80a5035, \
    -setTarget: at 0x80a504c, -setArgument:atIndex:2 at 0x80a5068, -invoke at 0x80a5078 and \
    -getReturnValue: at 0x80a5096. The second is the same at 0x80a50e0 .. 0x80a5122 and the third \
    at 0x80a550a .. 0x80a554c. The selectors are hoisted into stack slots up to 0x51a bytes before \
    the send, which is why the queue's selector column reads '?' - the slots are filled once at \
    0x80a2cf7 .. 0x80a2da4 out of __objc_selrefs and never written again. The function is \
    ui/accessibility/platform/inspect/ax_call_statement_invoker_mac.mm, the Accessibility \
    Inspector's call-a-method panel: both the leading and the trailing __FILE__ marker of the \
    region are that file, and the FALSE literal comparison at 0x80a54f1 is inside the function.";

/// `(class, selector)` pairs already named, so the refusal speaks once each.
static SAID: Mutex<BTreeSet<(String, String)>> = Mutex::new(BTreeSet::new());

/// Say once that there is no signature-driven dispatch, then stop.
fn refuse(class: &str, sel: Sel) {
    // SAFETY: `objc::sel::name` is the selector itself, which `sel_registerName`
    // made a NUL-terminated string, and this is the only read of it.
    let name = unsafe { std::ffi::CStr::from_ptr(objc::sel::name(sel)) };
    let selector = name.to_string_lossy().into_owned();
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if said.insert((class.to_string(), selector.clone())) {
        eprintln!(
            "rosetta3: foundation: {class} {selector}: refused by name. \
             {MISSING_SIGNATURE_DISPATCH} {CALLERS}{switch}",
            switch = if objc::msg::logging_unrecognised() {
                " ROSETTA3_LOG_UNRECOGNIZED is set, so this refusal is answering nil instead of \
                 aborting; a run this way cannot be trusted."
            } else {
                ""
            },
        );
    }
    match objc::msg::logging_unrecognised() {
        // `0` here is not a stand-in for an answer. It is what "answer nil"
        // means, and it is the only way out of this function; the guest's own
        // arithmetic is what makes it a bad answer, which is why the default
        // configuration stops instead.
        true => {}
        false => std::process::abort(),
    }
}

/// The IMP for a class method that returns an object.
///
/// The class name is read off the **receiver** rather than baked in at install
/// time, so a message cannot name a different class than the one the selector
/// was installed on: it does not remember a class at all, it asks.
unsafe extern "C" fn refusing_returning(receiver: Id, sel: Sel) -> usize {
    // SAFETY: a receiver here is a live object this runtime allocated; and a
    // null receiver cannot reach an IMP at all, because `objc_msgSend` returns
    // before an IMP is chosen.
    let class = unsafe { objc::object::class_of(receiver) }
        .map_or("<no class: the receiver was null>", |c| c.name_str());
    refuse(class, sel);
    0
}

/// The IMP for an instance method whose declared return is `void`.
unsafe extern "C" fn refusing_void(receiver: Id, sel: Sel) {
    // SAFETY: as above.
    let class = unsafe { objc::object::class_of(receiver) }
        .map_or("<no class: the receiver was null>", |c| c.name_str());
    refuse(class, sel);
}

/// One argument, for the four one-colon instance rows.
unsafe extern "C" fn refusing_void_one(_receiver: Id, sel: Sel, _argument: Id) {
    // SAFETY: as above.
    let class = unsafe { objc::object::class_of(_receiver) }
        .map_or("<no class: the receiver was null>", |c| c.name_str());
    refuse(class, sel);
}

/// Two arguments, for `-setArgument:atIndex:`.
///
/// A separate body rather than one with a variadic tail, because the argument
/// count comes from the selector's own colon count and `assert_row` checks the
/// *declaration*: this signature has to agree with `@0:8@16Q24` for the check to
/// mean anything.
unsafe extern "C" fn refusing_void_two(
    receiver: Id,
    sel: Sel,
    _argument: Id,
    _index: usize,
) {
    // SAFETY: as above.
    let class = unsafe { objc::object::class_of(receiver) }
        .map_or("<no class: the receiver was null>", |c| c.name_str());
    refuse(class, sel);
}

/// Every selector this module installs, with the encoding declared for it.
///
/// `#[cfg(test)]` because only the tests read it, as `spell_checker.rs:152` does:
/// a table-driven install written from a `const` is invisible to
/// `test/tools/objcenc.py`, so the certifier for these rows is in this crate.
#[cfg(test)]
const MEASURED_CLASS: &[(&str, &std::ffi::CStr)] =
    &[("invocationWithMethodSignature:", c"@24@0:8@16")];

#[cfg(test)]
const MEASURED_INSTANCE: &[(&str, &std::ffi::CStr)] = &[
    ("setSelector:", c"v24@0:8@16"),
    ("setTarget:", c"v24@0:8@16"),
    ("setArgument:atIndex:", c"v32@0:8@16Q24"),
    ("invoke", c"v16@0:8"),
    // `@?` and not `^v`: this tree's encoding checker parses a two-character
    // `^v` as a `^` followed by a missing number and panics with the encoding in
    // the message. `@?` is the spelling `encoding_check`'s own doc comment gives
    // for "a pointer to an unknown pointee", which is what Apple's `void *`
    // out-parameter is. It is a *declaration* either way - nothing reads it at
    // dispatch - and the frame and offsets are what `assert_row` checks.
    ("getReturnValue:", c"v24@0:8@?16"),
];

pub(crate) fn install() {
    // `register`, not `abstract_class`: the guest has to be able to
    // `_objc_alloc` this class, because the whole point is that the *answer*
    // becomes an `NSInvocation` the guest then messages five more times. An
    // abstract class would report a different failure from the one this module
    // is about.
    let class =
        corefoundation::base::register(c"NSInvocation", None, size_of::<usize>(), false);

    class_methods!(class, {
        "invocationWithMethodSignature:" => refusing_returning, c"@24@0:8@16";
    });
    methods!(class, {
        "setSelector:" => refusing_void_one, c"v24@0:8@16";
        "setTarget:" => refusing_void_one, c"v24@0:8@16";
        "setArgument:atIndex:" => refusing_void_two, c"v32@0:8@16Q24";
        "invoke" => refusing_void, c"v16@0:8";
        "getReturnValue:" => refusing_void_one, c"v24@0:8@?16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the tests compare an installed IMP against a body, so this is the
    // only place `c_void` is named; a module-level import would be an unused
    // import in a release build.
    use std::ffi::c_void;

    /// The declared arity is the selector's own name's, and the encoding that
    /// **reached the runtime** is the one declared rather than a copy of it.
    ///
    /// `encoding_check` here has `assert_installed` for the **metaclass** only -
    /// it was written for AppKit's `class_methods!` rows and `appkit`'s copy is
    /// the one that grew `assert_installed_instance`. So the instance half is
    /// checked below by the same two calls `assert_installed` makes, and by the
    /// same `Method::types` read, rather than by a second helper nobody can find.
    #[test]
    fn the_declared_arity_is_the_selector_s() {
        for (selector, types) in MEASURED_CLASS {
            crate::encoding_check::assert_row(selector, types);
            crate::encoding_check::assert_installed(c"NSInvocation", selector, types);
        }
        for (selector, types) in MEASURED_INSTANCE {
            crate::encoding_check::assert_row(selector, types);
            let installed = objc::class::lookup(c"NSInvocation")
                .expect("installed by this module")
                .own_method(objc::sel::get(selector))
                .unwrap_or_else(|| panic!("-[{selector}] is not installed"));
            // SAFETY: `Method::types` hands back the `CString` the method was
            // installed with, and every `Method` in this runtime is leaked.
            let installed = unsafe { std::ffi::CStr::from_ptr(installed.types()) };
            assert_eq!(
                installed.to_bytes(),
                types.to_bytes(),
                "-[{selector}] is installed with {installed:?} and declared as {types:?}"
            );
        }
    }

    /// Six selectors refused, and **only** those six.
    ///
    /// The negative half matters: `-[NSInvocation methodSignatureForSelector:]`
    /// and `+[NSInvocation invocation]` are real selectors this class has, and
    /// installing an IMP for a selector no measured site sends is unreachable
    /// surface pretending otherwise.
    #[test]
    fn exactly_the_measured_selectors_are_installed() {
        crate::bootstrap();
        let class =
            objc::class::lookup(c"NSInvocation").expect("installed by this module");
        for (selector, _) in MEASURED_CLASS {
            assert!(
                class.metaclass().own_method(objc::sel::get(selector)).is_some(),
                "+[{selector}] is measured and must be refused by name"
            );
        }
        for (selector, _) in MEASURED_INSTANCE {
            assert!(
                class.own_method(objc::sel::get(selector)).is_some(),
                "-[{selector}] is measured and must be refused by name"
            );
        }
        for unmeasured in ["methodSignatureForSelector:", "retainArguments", "dealloc"] {
            assert!(
                class.own_method(objc::sel::get(unmeasured)).is_none(),
                "-{unmeasured} is not measured and must not be refused: a refusal names a missing \
                 capability at a site the guest has, and an invented one names it at a site it does \
                 not"
            );
        }
    }

    /// Registered **and** published, so the guest's classref lands on a class
    /// whose methods name themselves rather than on an anonymous empty one.
    #[test]
    fn the_class_is_published() {
        crate::bootstrap();
        for name in CLASSES {
            let cname = std::ffi::CString::new(*name).unwrap();
            assert!(objc::class::lookup(&cname).is_some(), "{name} registered");
            assert!(
                crate::class_data()
                    .iter()
                    .any(|(symbol, _)| symbol == &format!("_OBJC_CLASS_$_{name}")),
                "{name} is published as data, not only registered"
            );
        }
    }

    /// The refusal is on the **class the receiver names**, and every row has a
    /// body whose declared shape it can accept.
    ///
    /// This is the property that keeps a refusal from being misattributed - the
    /// body does not remember a class name, it asks the receiver - and the
    /// property that keeps `-setArgument:atIndex:` from being handed the
    /// one-argument body, which would read `%rcx` as a value it has no use for.
    /// **Nothing here sends a message**, deliberately: every body in this module
    /// ends in `abort()` unless `ROSETTA3_LOG_UNRECOGNIZED` is set, so a test
    /// that called one would take the test binary down with it, and a test that
    /// asserted the switch was on would be red in a default `cargo test`. The
    /// IMP each row has is read out of the class's own method list instead.
    #[test]
    fn each_row_has_the_body_its_shape_calls_for() {
        crate::bootstrap();
        let class =
            objc::class::lookup(c"NSInvocation").expect("installed by this module");
        let meta = class.metaclass();
        for (selector, types) in MEASURED_CLASS {
            let installed = meta
                .method(objc::sel::get(selector))
                .expect("measured class method is installed")
                .implementation();
            assert_eq!(
                installed,
                refusing_returning as *const c_void,
                "+[{selector}] returns an object and must get the object-returning refusal"
            );
            // And the declared shape agrees, so the body cannot be handed a
            // register the declaration says is not there.
            assert_eq!(types.to_bytes()[0], b'@', "+[{selector}] declares id");
        }
        for (selector, types) in MEASURED_INSTANCE {
            let installed = class
                .method(objc::sel::get(selector))
                .expect("measured instance method is installed")
                .implementation();
            let want = match *selector {
                "invoke" => refusing_void as *const c_void,
                "setArgument:atIndex:" => refusing_void_two as *const c_void,
                _ => refusing_void_one as *const c_void,
            };
            assert_eq!(installed, want, "-[{selector}] got the wrong IMP");
            // And the declaration agrees with the body, which is the property
            // the pairing exists to protect: `-invoke` has **no** colons, so it
            // is the only row whose encoding mentions no argument register, and
            // the only one that may take the no-argument body. Every other row
            // declares an `@16`, and so must not.
            let colons = selector.bytes().filter(|b| *b == b':').count();
            // The one thing `assert_row` cannot tell a reader here is *which*
            // body a row is allowed to have, and that follows from the colon
            // count alone: `v16@0:8` names no offset past `self`, so it is a
            // zero-colon row and no-argument body, and every other row names at
            // least one.
            let words = crate::encoding_check::arguments(types);
            assert_eq!(
                words.len(),
                colons + 1,
                "-[{selector}] declares {colons} colon(s), so {colons} argument(s) plus self, \
                 and {types:?} declares {}",
                words.len()
            );
            // And the three bodies are three distinct addresses, or the pairing
            // above would hold for the wrong reason.
            assert_ne!(
                refusing_void as *const c_void,
                refusing_void_one as *const c_void,
                "the no-argument and one-argument bodies must be distinguishable"
            );
        }
    }

    /// The missing capability is named, and the line says why nil was refused -
    /// not only what is absent.
    #[test]
    fn the_missing_capability_is_named_rather_than_declared_unsupported() {
        assert!(MISSING_SIGNATURE_DISPATCH.contains("NSMethodSignature"));
        assert!(
            MISSING_SIGNATURE_DISPATCH.contains("encoding is never read at dispatch"),
            "the reason this cannot be implemented here is a property of this tree's checking, and \
             the line has to say which"
        );
        assert!(
            MISSING_SIGNATURE_DISPATCH.contains("getReturnValue:"),
            "the reason a wrong answer would be silent is that one raw word is read back as an \
             accessibility attribute, and the line has to name that word's method"
        );
        for site in ["0x80a4fcc", "0x80a500e", "0x80a5078", "0x80a5096", "0x80a54f1"] {
            assert!(
                CALLERS.contains(site),
                "the refusal names no address for {site}, and the address is the checkable part"
            );
        }
    }
}
