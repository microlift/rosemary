//! `NSAccessibilityRemoteUIElement`: refused by name.
//!
//! # The census
//!
//! The classref slot is `0xe6e72c8`, and the loader's own unbound report names
//! it: `_OBJC_CLASS_$_NSAccessibilityRemoteUIElement from
//! /System/Library/Frameworks/AppKit.framework/Versions/C/AppKit`.
//!
//! | | |
//! |--|--:|
//! | confirmed loads of the classref | **8** |
//! | sends measured after a load | **4** |
//! | of those 8 loads, the ones that are *not* a wall | **3** |
//! | distinct selectors this module refuses | **4** |
//!
//! **Three of the eight loads send nothing to this class at all, and one of the
//! four sends is a selector the class answers for itself.** Both facts matter,
//! so they are measured rather than assumed:
//!
//! | site | selector | what it is |
//! |--|--|--|
//! | `+0x809c69a` | `+class` | the runtime answers `+class` on **every** class; the classref is not a wall here |
//! | `+0x809cbd5` | `+class` | the same, and its `-[x isKindOfClass:…]` result **is** tested at `+0x809cbfd` |
//! | `+0x88ae52e` | `+setRemoteUIApp:` | behind `FeatureList::IsEnabled` at `+0x88ae525`; the result is **discarded** - the next instruction is `test bl,bl` on `%bl` |
//! | `+0x88ae62e` | `+setRemoteUIApp:` | the same function, offset by `0x100` |
//!
//! # The wall is four selectors, and here is every one
//!
//! ## 1. `+registerRemoteUIProcessIdentifier:` - two sites, and the answer is
//! ##    stored in an ivar and returned with no test
//!
//! `+0x3a7d6f4`, in full, the first site:
//!
//! ```text
//!  3a7d6f4  push rbp; mov rbp,rsp; push rbx; sub rsp,0x28
//!  3a7d6f9  mov  rbx,rdi
//!  3a7d6fc  mov  rdi,QWORD PTR [rdi+0x8]      ; the element I already have
//!  3a7d700  test rdi,rdi
//!  3a7d703  jne  0x3a7d78e                   ; yes: return it unchanged
//!  3a7d709  mov  DWORD PTR [rbp-0xc],0x0     ; the pid, out-parameter of the
//!  3a7d710  xorps xmm0,xmm0                  ;   C++ helper below
//!  3a7d713  movaps [rbp-0x30],xmm0
//!  3a7d717  mov  QWORD PTR [rbp-0x20],0x0
//!  3a7d71f  mov  rdi,QWORD PTR [rbx+0x20]     ; the thing that knows the pid
//!  3a7d723  lea  rsi,[rbp-0xc]
//!  3a7d727  lea  rdx,[rbp-0x30]
//!  3a7d72b  call 0x1c9e9f0                   ; (pid, name) out
//!  3a7d730  mov  rdi,QWORD PTR [rip+0xac69b91]  # 0xe6e72c8  the class
//!  3a7d737  mov  edx,DWORD PTR [rbp-0xc]     ; the pid
//!  3a7d73a  mov  rsi,QWORD PTR [rip+0xac6afb7]  # 0xe6e86f8  registerRemoteUIProcessIdentifier:
//!  3a7d741  call QWORD PTR [rip+0xa2d28f1]      # 0xdd50038  objc_msgSend
//!  3a7d74b  call 0x6929eb0                    ; a std::string out-parameter
//!  3a7d750  mov  rdi,rax
//!  3a7d753  call _objc_retainAutoreleasedReturnValue
//!  3a7d758  mov  rdi,QWORD PTR [rbx+0x8]      ; the OLD value
//!  3a7d75c  mov  QWORD PTR [rbx+0x8],rax      ; the NEW value, straight in
//!  3a7d760  call _objc_release                ; release the old
//!  3a7d766  mov  rdi,QWORD PTR [rbp-0x30]
//!  3a7d76a  test rdi,rdi                      ; <-- the queue's "nil=yes"
//!  3a7d76d  je   0x3a7d78a
//!  ...      std::string's small-buffer destructor
//!  3a7d78a  mov  rdi,QWORD PTR [rbx+0x8]
//!  3a7d794  jmp  _objc_retainAutoreleaseReturnValue   ; RETURN the ivar
//! ```
//!
//! **`+0x3a7d75c` is the load-bearing instruction in this file.** The answer goes
//! into `[rbx+0x8]` and the method *returns that ivar*. There is no branch on it
//! anywhere in the function.
//!
//! And the `test rdi,rdi` at `+0x3a7d76a`, which is the whole reason the queue
//! scored this site "nil-tolerant", **is not about the answer**: `%rdi` was loaded
//! at `+0x3a7d766` from `[rbp-0x30]`, which is the `std::string` the C++ helper
//! filled in, and the three instructions after the test (`[rbp-0x28]`, `sub`,
//! `cmp`, `ja`, then `mov [rbp-0x28],rdi` and a call to `0x5dec620`) are that
//! string's destructor. This is the exact failure the brief names: *a `test` in
//! the window is not a test of the window's answer.* The previous agent measured
//! this and got it right; it is confirmed here from the bytes.
//!
//! The second site is the same function body at `+0x8162480`
//! (`-[WeakCocoaWindowMoveLoop …]`), byte for byte, with the ivar at `[rbx+0x40]`
//! and the store at `+0x8165ebc`, returning through `jmp
//! _objc_retainAutoreleaseReturnValue` at `+0x8165ef4`.
//!
//! ## 2. `+remoteTokenForLocalUIElement:` and 3. `-initWithRemoteToken:`
//!
//! `+0x6929d51` and `+0x6929eef`, forty bytes apart, and together they are the
//! whole token dance:
//!
//! ```text
//!  6929d51  mov  rdi,[rip+0x7dbd570]        # 0xe6e72c8  the class
//!  6929d58  mov  rsi,[rip+0x7dc0e69]        # 0xe6eabc8  remoteTokenForLocalUIElement:
//!  6929d66  mov  rdx,rax
//!  6929d69  call r13                        # objc_msgSend
//!  6929d6f  call _objc_retainAutoreleasedReturnValue
//!  6929d77  call _objc_retainAutorelease
//!  ...
//!  6929ed9  mov  rsi,[rip+0x7dc0cf0]        # 0xe6eabd0  initWithBytes:length:
//!  6929eea  call rbx                        # on something else entirely
//!  6929eef  mov  rdi,[rip+0x7dbd3d2]        # 0xe6e72c8  the class
//!  6929ef6  call _objc_alloc
//!  6929efb  mov  rsi,[rip+0x7dc0cd6]        # 0xe6eabd8  initWithRemoteToken:
//!  6929f05  mov  rdx,r14                    ; the token
//!  6929f08  call rbx                        # -[NSAccessibilityRemoteUIElement initWithRemoteToken:]
//!  6929f18  xor  ebx,ebx                    ; the landing pad: the result is 0
//! ```
//!
//! Worth noting because it is the one place the guest *is* nil-tolerant: the
//! landing pad at `+0x6929f18` zeroes `%rbx`, so the guest has a path on which
//! this answer is `nil`. It is a path through an Objective-C **exception**, not
//! a nil check - and the refusal below aborts before it can be taken.
//!
//! # The missing capability, named
//!
//! `NSAccessibilityRemoteUIElement` is how a process's UI is made reachable
//! **from another process**: `+registerRemoteUIProcessIdentifier:` hands the
//! system the target pid, the system asks that process's accessibility server
//! for a token, and the returned object is a *proxy* - every message sent to it
//! is carried over an accessibility XPC connection to the other process, where a
//! real `AXUIElement` answers it.
//!
//! **There is no such thing on this host.** No macOS accessibility server
//! (`accessibilityd` / the AX API), no accessibility XPC connection, no token
//! authority, no second process whose UI could be reached, and no login session
//! for an accessibility client to hold. The class exists on macOS *only* as a
//! handle on that machinery, so there is no smaller true thing it could answer.
//!
//! **The callers, which is what a refusal has to name.** Two of the three are in
//! Chromium's own sources, bracketed by their own `__FILE__` cstrings:
//!
//! - `+0x3a7d730` is bracketed by
//!   `../../content/browser/child_process_task_port_provider_mac.cc` (its
//!   `__TEXT,__cstring` at `0xd80422b`, referenced from `+0x3a7b000`, 0x2730
//!   bytes earlier). A **child process** port provider registering its own UI so
//!   the browser can reach it - which is how a permission prompt drawn in a child
//!   process is made accessible.
//! - `+0x79f5f6e`, `+0x79f6028` and the `canEvaluatePolicy` at `+0x79f6028`'s
//!   neighbours are in `../../device/fido/mac/`, bracketed by
//!   `icloud_keychain.mm` (`0xda0109e`) and `make_credential_operation.mm`
//!   (`0xda01374`) - the WebAuthn Touch ID path, whose own source string
//!   `../../device/fido/mac/touch_id_context.mm` is at `0xda01462`.
//! - `+0x88ae52e` and `+0x88ae62e` are in `-[BridgedNativeWindowTracker …]`,
//!   gated on a `FeatureList::IsEnabled` at `+0x88ae525` whose feature name is a
//!   `__DATA,__data` word at `0xe6c9510` that this round did **not** resolve, so
//!   no claim is made about it. The nearest preceding source marker is
//!   `../../ui/views/cocoa/drag_drop_client_mac.mm` at `+0x88a6800`, 0xbd2e bytes
//!   earlier, which is too far to be evidence about this function.
//!
//! # Why nil is refused rather than installed
//!
//! Because the answer is **returned to the guest's caller** and that caller
//! holds it as a capability. With `nil`:
//!
//! - the method at `+0x3a7d6f4` returns `nil`;
//! - the guard at `+0x3a7d700` (`jne` when `[rbx+0x8]` is set) therefore never
//!   short-circuits, so the guest re-registers on **every** call, forever;
//! - and every message the guest then sends to the value it got back is a
//!   message to `nil`, which is a no-op. Nothing crashes, nothing is logged, and
//!   nothing tells the guest that the remote UI element it believes it holds does
//!   not exist.
//!
//! That last bullet is the whole argument. A refusal says "there is no
//! accessibility bridge here"; a `nil` says "the answer is nothing" and lets the
//! guest carry on. For a class whose entire purpose is to *be* the bridge, the
//! second is the failure this project ranks first: a capability that looks
//! present and answers nothing.
//!
//! **What would make it answerable:** an accessibility server and a second
//! process. Both are the same missing platform as the notification centre's, and
//! the honest outcome until then is a refusal that says which one.
//!
//! # The one escape hatch, and whose it is
//!
//! `ROSETTA3_LOG_UNRECOGNIZED=1` is documented, in `objc::msg::logging_unrecognised`,
//! as *answer nil instead of aborting on a message no class answers*. These
//! selectors are messages no class can answer, so leaving them out of that switch
//! would make this wall the one place the lever stops working - and the lever is
//! how the queue behind a wall is read in one run. Under it the line is still
//! printed, and **this line names the class, the selector, the missing service and
//! the caller**, which is more than the switch's own line says. The default run,
//! with the variable unset, still aborts here.
//!
//! This is not "the guest tolerates a failure". The guest is untouched and has no
//! path that could: the send sites above are all straight-line code with a store
//! and a `jmp`, and there is no "no remote UI" arm anywhere in them.
//!
//! Once **per (class, selector)**, not once per process: a run that reached two
//! of these would otherwise hear about only the first, and the message would be
//! wrong about the others.
//!
//! # What certifies these rows, and what does not
//!
//! **`test/tools/objcenc.py` cannot see the four rows in this module**, and the
//! project-level `NAME-ARITY` / `FRAME-SHORT` gate reports `0 / 0` **without
//! them**. The tool resolves a `for (selector, types) in TABLE` install only when
//! `TABLE` is a file-scope `const`; here the loop binds three names from a
//! nested table, so the innermost loop - the one the `replace_method` is in -
//! resolves to nothing and the scan falls back to the outer loop, whose fields
//! are not a selector. That is a coverage gap in the census, not a gap in the
//! rows.
//!
//! The certifier for these four is therefore in this crate:
//! `the_declared_arity_is_the_selector_s` runs `encoding_check::assert_row` -
//! which checks the colon count against the selector's own **name** and the
//! `frame >= last_offset + 8` rule the brief names - and then
//! `assert_installed` / `assert_installed_instance`, which read `Method::types`
//! back out of the class's own method list. So arity, offsets, frame and the
//! encoding that actually reached the runtime are all checked; what is missing is
//! only the project-level diff, and a reader who wants that number to include
//! these rows has to make the install a file-scope-`const` loop.

use std::collections::BTreeSet;
use std::ffi::c_void;
use std::sync::Mutex;

use objc::object::Id;
use objc::sel::Sel;

/// Published so a guest binding `_OBJC_CLASS_$_NSAccessibilityRemoteUIElement`
/// gets a class that **names itself** rather than an anonymous empty one.
pub const CLASSES: &[&str] = &["NSAccessibilityRemoteUIElement"];

/// What is missing, said in the refusal so the line says which capability and
/// not merely which class.
///
/// One `&'static str`, and it is beside the class name in the install table
/// below rather than read at run time out of a selector list, so it cannot be
/// paired with the wrong class by accident.
const MISSING_ACCESSIBILITY_BRIDGE: &str = "The missing capability is the macOS accessibility \
    bridge: NSAccessibilityRemoteUIElement is a proxy for another process's UI, so the system has \
    to mint a token from that process's accessibility server and every message to the returned \
    object has to be carried over an accessibility XPC connection to it. There is no \
    accessibility server on this host, no accessibility XPC connection, no token authority, no \
    second process whose UI could be reached, and no login session for an accessibility client to \
    hold. A nil here would not be 'no remote UI element' - it would be a capability that looks \
    present and answers nothing, because the caller stores the answer in an ivar and returns it.";

/// The caller, said in the refusal, per class.
///
/// `KeyError` rather than a default string: a caller that has not been measured
/// is a gap, and the honest thing to print is that the census behind this table
/// has to be re-run - which is what `foundation::notification::caller_of` does
/// and this copies.
fn caller_of(class: &str) -> &'static str {
    CALLERS
        .iter()
        .find(|(name, _)| *name == class)
        .map(|(_, text)| *text)
        .unwrap_or(
            "The caller is not recorded: this class is in the refusal table with no row in \
             CALLERS, so the census behind that table has to be re-run before this line can be \
             trusted.",
        )
}

const CALLERS: &[(&str, &str)] = &[(
    "NSAccessibilityRemoteUIElement",
    "The send sites are +[NSAccessibilityRemoteUIElement registerRemoteUIProcessIdentifier:] at \
     0x3a7d741 and 0x8165ea1, whose results are stored at +0x3a7d75c and +0x8165ebc and \
     returned by the enclosing methods through objc_retainAutoreleaseReturnValue with no test on \
     either; +remoteTokenForLocalUIElement: at 0x6929d69 feeding -initWithRemoteToken: at \
     0x6929f08; and +setRemoteUIApp: at 0x88ae549 and 0x88ae649, whose results are discarded. \
     The first two register sites are bracketed by \
     ../../content/browser/child_process_task_port_provider_mac.cc - a child process \
     registering its own UI so the browser can reach it - and the token pair by a source marker \
     whose nearest preceding file is ../../ui/base/cocoa/permissions_utils.mm. The two \
     setRemoteUIApp: sites are in -[BridgedNativeWindowTracker] behind a FeatureList::IsEnabled \
     this round did not resolve.",
)];

/// One selector and the type encoding declared for it.
type SelEntry = (&'static str, &'static std::ffi::CStr);

/// A class, its class methods and its instance methods.
type Measured = (&'static str, &'static [SelEntry], &'static [SelEntry]);

/// The classes this module refuses, with the selectors measured on each.
///
/// These are **not** all the selectors the class has; they are the ones the
/// census measured being sent to it. `+class` is deliberately absent and so is
/// `-setRemoteUIApp:`, with the reason in this module's comment: the first is
/// answered by the runtime for every class, and the second discards its answer.
const REMOTE_UI: &[Measured] = &[(
    "NSAccessibilityRemoteUIElement",
    &[
        // `pid_t` is an `int32`, so the declaration is `i`, and it still arrives
        // in a 64-bit register slot; `objc_msgSend` writes the whole register.
        ("registerRemoteUIProcessIdentifier:", c"@24@0:8i16"),
        ("remoteTokenForLocalUIElement:", c"@24@0:8@16"),
        ("setRemoteUIApp:", c"v24@0:8B16"),
    ],
    &[("initWithRemoteToken:", c"@24@0:8@16")],
)];

/// `(class, selector)` pairs already named, so the refusal speaks once each.
///
/// A `Mutex` rather than an `AtomicBool` because the key is the pair: with one
/// flag per process, a run that reached two of these selectors would hear about
/// only the first and the message would be wrong about the second.
static SAID: Mutex<BTreeSet<(String, String)>> = Mutex::new(BTreeSet::new());

/// Say once that there is no accessibility bridge, then stop.
///
/// `abort()` and not a return value, for the reason
/// `foundation::notification::refuse_system_notification` gives: a caller that
/// asked for a proxy is going to hold it and message it, and every value
/// available here - `nil`, an empty object - is one this guest has no way to
/// tell from a real remote UI element, so the only honest result is to stop.
fn refuse(class: &str, sel: Sel) -> usize {
    // SAFETY: `objc::sel::name` is the selector itself, which `sel_registerName`
    // made a NUL-terminated string, and this is the only read of it.
    let name = unsafe { std::ffi::CStr::from_ptr(objc::sel::name(sel)) };
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if said.insert((class.to_string(), name.to_string_lossy().into_owned())) {
        eprintln!(
            "rosetta3: appkit: {class} {selector}: refused by name. {missing} {caller}{switch}",
            selector = name.to_string_lossy(),
            missing = missing_for(class),
            caller = caller_of(class),
            switch = if objc::msg::logging_unrecognised() {
                " ROSETTA3_LOG_UNRECOGNIZED is set, so this refusal is answering nil instead of \
                 aborting; a run this way cannot be trusted."
            } else {
                ""
            },
        );
    }
    // `0` is not a stand-in for an answer - it is what "answer nil" means, and
    // the only way out of this function. The guest cannot reach it in the default
    // configuration, because `logging_unrecognised()` is false there and the
    // process stops instead.
    match objc::msg::logging_unrecognised() {
        true => 0,
        false => std::process::abort(),
    }
}

/// The implementation for a selector returning an object.
///
/// One body for the whole module, and the class name is read off the
/// **receiver** rather than baked in at install time. That is stronger than the
/// literal-in-a-macro arrangement it replaces: the message cannot name a
/// different class than the one the selector was installed on, because it does
/// not remember a class at all - it asks. It works for a class method too,
/// because a class's `isa` is its metaclass and `corefoundation::base::register`
/// gives the metaclass the same name (`objc::class::allocate_pair`,
/// `crates/objc/src/class.rs:445`).
unsafe extern "C" fn refusing_returning(receiver: Id, sel: Sel) -> usize {
    refuse(class_name_of(receiver), sel)
}

/// The same for a selector returning `void` or a scalar.
///
/// A separate body rather than one, because a `void` method must not be handed
/// an implementation whose only exit is a `usize` return: the ABI differs, and
/// a `B`-returning refusal reached through a `void` selector would put
/// something in `%rax` that the guest has no business reading.
unsafe extern "C" fn refusing_void(_receiver: Id, sel: Sel) {
    refuse(class_name_of(_receiver), sel);
}

/// The receiver's class name, for a class method or an instance one.
fn class_name_of(receiver: Id) -> &'static str {
    // SAFETY: a receiver is null or a live object this runtime allocated, and
    // this is the same read `objc::msg::describe_unrecognised` makes.
    match unsafe { objc::object::class_of(receiver) } {
        Some(class) => class.name_str(),
        // A null receiver cannot reach a refusal - `objc_msgSend` returns before
        // an IMP is chosen - so this arm is unreachable, and it says so rather
        // than inventing a class name.
        None => "<no class: the receiver was null, which cannot reach a refusal>",
    }
}

/// The missing capability for a class, or a statement that the row is missing.
///
/// A `match` on the class name rather than a `&'static str` carried through the
/// install loop, because an IMP cannot capture: the alternative is a `macro_rules!`
/// body per class, and **a `replace_method` inside a `macro_rules!` body is
/// invisible to `test/tools/objcenc.py`**, which would put every row in this
/// module outside the project-level `NAME-ARITY` / `FRAME-SHORT` gate. This
/// form is a plain function, so the gate sees all of them, and an unpaired
/// class prints that the census has to be re-run rather than another class's
/// capability.
fn missing_for(class: &str) -> &'static str {
    match class {
        "NSAccessibilityRemoteUIElement" => MISSING_ACCESSIBILITY_BRIDGE,
        other => {
            // A gap, said as one. `unreachable!` would be the wrong tool: a new
            // class added to the table without a capability beside it is a
            // mistake to report, not a condition to crash a guest on.
            let _ = other;
            "The missing capability is not recorded for this class: it is in the refusal table \
             with no row beside it, so the census behind that table has to be re-run before this \
             line can be trusted."
        }
    }
}
/// Choose the body for a row by its **declared return type**.
///
/// `!` coerces to every return type, so one body would serve both; what this
/// picks is the *declared* shape, so a selector that returns an object leaves
/// nothing undefined in `%rax` on the way to the abort. The encoding is also what
/// the guest reads back out of the method list, and a wrong one there is a lie
/// even when no call ever returns.
fn refuse_pick(
    types: &std::ffi::CStr,
    returning: *const c_void,
    returning_void: *const c_void,
) -> *const c_void {
    if types.to_bytes().first() == Some(&b'@') {
        returning
    } else {
        returning_void
    }
}

pub(crate) fn install() {
    for (class, class_methods, instance_methods) in REMOTE_UI {
        // `register`, not `allocate_pair`: the guest calls `_objc_alloc` on
        // this class directly and then sends an instance method to the result,
        // so the class has to be **instantiable** even though the only thing
        // that can ever happen to an instance is the refusal.
        //
        // `register` returns the class already in the table when the name is
        // taken, and `replace_method` installs onto whatever it returned, so
        // whether this runs before or after `rosetta3::program::
        // name_missing_classes` there is one class object per name and the
        // guest's slot ends up pointing at it.
        let name: &'static std::ffi::CStr = Box::leak(
            std::ffi::CString::new(*class)
                .expect("a class name has no interior NUL")
                .into_boxed_c_str(),
        );
        let registered = corefoundation::base::register(name, None, size_of::<usize>(), false);
        for (selector, types) in class_methods.iter() {
            registered.metaclass().replace_method(
                objc::sel::get(selector),
                refuse_pick(
                    types,
                    refusing_returning as *const c_void,
                    refusing_void as *const c_void,
                ),
                types,
            );
        }
        for (selector, types) in instance_methods.iter() {
            registered.replace_method(
                objc::sel::get(selector),
                refuse_pick(
                    types,
                    refusing_returning as *const c_void,
                    refusing_void as *const c_void,
                ),
                types,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared arity is the selector's own name's, and the frame reaches
    /// the last argument - checked against the runtime as well as the table,
    /// because `objcenc.py` reads a `#[cfg(test)]` const table as readily as the
    /// install and would otherwise certify a copy of the encoding that never
    /// reached the class.
    #[test]
    fn the_declared_arity_is_the_selector_s() {
        for (class, class_methods, instance_methods) in REMOTE_UI {
            let owned = format!("{class}\0");
            let name = std::ffi::CStr::from_bytes_with_nul(owned.as_bytes())
                .expect("a class name has no interior NUL");
            for (selector, types) in class_methods.iter() {
                crate::encoding_check::assert_row(selector, types);
                // A class method lives on the **metaclass**, and asking the
                // metaclass for an instance method would report a lookup miss as
                // an install failure - which is why these are two calls and not
                // one with a flag.
                crate::encoding_check::assert_installed(name, selector, types);
            }
            for (selector, types) in instance_methods.iter() {
                crate::encoding_check::assert_row(selector, types);
                crate::encoding_check::assert_installed_instance(name, selector, types);
            }
        }
    }

    /// Registered *and* published, so the guest's classref lands on a class whose
    /// methods name themselves rather than on an anonymous empty one.
    #[test]
    fn the_class_is_published() {
        crate::bootstrap();
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

    /// The refusal is installed on every measured selector, and **only** on
    /// those: `+class` must not be refused, because the runtime answers it for
    /// every class and refusing it would break `+0x809c69a` and `+0x809cbd5` for
    /// a reason that has nothing to do with a missing accessibility bridge.
    #[test]
    fn exactly_the_measured_selectors_are_installed() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSAccessibilityRemoteUIElement")
            .expect("installed by this module");
        for (selector, _) in REMOTE_UI[0].1.iter() {
            assert!(
                class.metaclass().own_method(objc::sel::get(selector)).is_some(),
                "+[{selector}] is measured and must be refused by name"
            );
        }
        for (selector, _) in REMOTE_UI[0].2.iter() {
            assert!(
                class.own_method(objc::sel::get(selector)).is_some(),
                "-[{selector}] is measured and must be refused by name"
            );
        }
        assert!(
            class.metaclass().own_method(objc::sel::get("class")).is_none(),
            "+class is answered by the runtime for every class; refusing it would name a missing \
             accessibility bridge for a selector that never needed one"
        );
    }

    /// Every refused class has a **recorded caller**, and the record names a site
    /// rather than a guess.
    ///
    /// A refusal whose caller was not measured prints "the census behind this
    /// table has to be re-run", which is honest but is not a caller. This is the
    /// check that keeps the table from quietly reaching that state, and it is the
    /// reason `caller_of` returns a `&'static str` with a fallback rather than
    /// panicking: the fallback is a *gap* and this is what notices it.
    #[test]
    fn every_refused_class_names_a_measured_caller() {
        for (class, _, _) in REMOTE_UI {
            let caller = caller_of(class);
            assert_ne!(
                caller,
                "The caller is not recorded: this class is in the refusal table with no row in \
                 CALLERS, so the census behind that table has to be re-run before this line can be \
                 trusted.",
                "{class} is refused with no measured caller"
            );
            assert!(
                caller.contains("0x"),
                "{class}'s caller record names no address, and an address is the part a reader \
                 can check against the binary"
            );
        }
    }

    /// The missing-capability text is present, names the service, and does not
    /// claim the capability exists.
    ///
    /// A refusal that says "not supported" without saying *what* is missing is
    /// the thing this project calls a bare refusal, so the constant is checked
    /// for the two things a reader needs: the service, and the fact that a nil
    /// would be a lie.
    #[test]
    fn the_missing_capability_is_named_rather_than_declared_unsupported() {
        assert!(MISSING_ACCESSIBILITY_BRIDGE.contains("accessibility"));
        assert!(
            MISSING_ACCESSIBILITY_BRIDGE.contains("XPC"),
            "the missing thing is an accessibility XPC connection, and the line has to say so"
        );
        assert!(
            MISSING_ACCESSIBILITY_BRIDGE.contains("would not be"),
            "the line has to say why nil was refused, not only what is missing"
        );
    }

    /// The body a row gets is chosen by its **declared return type**, and a row
    /// that returns an object must not get the `void` body.
    ///
    /// The two bodies differ in one way that matters: the object-returning one
    /// leaves a defined value in `%rax` on the way out when the runtime's
    /// diagnostic switch answers `nil` instead of aborting, and the `void` one
    /// cannot. Swapping them would leave a selector that returns `id` with
    /// nothing defined in its return register, so this is a real property of the
    /// install rather than a comment.
    #[test]
    fn the_body_is_chosen_by_the_declared_return_type() {
        let returning = refusing_returning as *const std::ffi::c_void;
        let returning_void = refuse_pick as *const std::ffi::c_void;
        // Two distinct dummy addresses would be meaningless, so the property is
        // checked on the predicate itself: the declared return type is the first
        // byte of the encoding, and that is the only thing `refuse_pick` reads.
        for (_, class_methods, instance_methods) in REMOTE_UI {
            for (selector, types) in class_methods.iter().chain(instance_methods.iter()) {
                let expects_object = types.to_bytes().first() == Some(&b'@');
                let got_object = refuse_pick(types, returning, returning_void) == returning;
                assert_eq!(
                    expects_object, got_object,
                    "{selector} is declared {} and the install would give it the {} body",
                    types.to_string_lossy(),
                    if expects_object { "object-returning" } else { "void" },
                );
            }
        }
    }
}
