//! `NSSpeechSynthesizer`: refused by name.
//!
//! # The census
//!
//! The classref slot is `0xe6e7440`, and the loader's own unbound report names
//! it: `_OBJC_CLASS_$_NSSpeechSynthesizer from
//! /System/Library/Frameworks/AppKit.framework/Versions/C/AppKit`. **AppKit,
//! not AVFoundation** - `NSSpeechSynthesizer` is AppKit's class, and the
//! `AVSpeech*` trio in the queue is a different one.
//!
//! | | |
//! |--|--:|
//! | confirmed loads of the classref | **6** |
//! | sends the queue's 96-byte window found | **5** |
//! | distinct selectors this module refuses | **5** |
//! | calls in the region that are *not* a message to the synthesizer | **29** |
//!
//! # The whole thing is 0x430 bytes, and every call in it is accounted for
//!
//! The five loads and five sends are not scattered: they are five
//! `+initWithVoice:` constructions of **one lazy singleton** and the messages to
//! it, all inside `0x692a220..0x692a5e0`. That contiguity is what makes the next
//! table a **structural** claim rather than a sample of three.
//!
//! The singleton is a `std::call_once`-shaped pair: an object at `0xe73db38` and
//! a "has it been built" byte at `0xe73db40`. The five sites are the five
//! *distinct* places the once-flag is tested, and each has a matching
//! `__cxa_guard`-style acquire at `0x56826f0` / release at `0x5682823`.
//!
//! Every `call` and `jmp` instruction in the region, by kind:
//!
//! | target | what it is | ever receives the synthesizer? |
//! |--|--|--|
//! | `0xdd50038` | `objc_msgSend` | **yes** - 6 sends, and they are all this class |
//! | `0xdd50030` / `0xdd50040` | `objc_release` / `objc_retain` | ARC bookkeeping; `retain` once, `release` on the way out |
//> | `0xcca0438` | `objc_alloc` | no - the allocator, before there is an object |
//! | `0xcca042c` | `objc_retainAutoreleasedReturnValue` | no |
//! | `0xcca02dc` | `__Unwind_Resume` | no - an exception landing pad |
//! | `0x5d99b20` | a `std::string` constructor | no - the text, made from the caller's `std::string` |
//> | `0x5dec620` | `operator delete` | no |
//> | `0x5da2a20` | the three TTS entry points' own prologue | no |
//> | `0x6923f10`, `0x691dfe0` | two out-of-line helpers | no |
//> | `0x56826f0` / `0x5682823` / `0x56828a2` | the once-flag acquire / release / abort | no |
//!
//! **Not one C function in 0x430 bytes ever receives the synthesizer.** The only
//! things it is handed to are `objc_msgSend` and the ARC retain/release pair.
//! That is the same structural shape the `NSSpellChecker` module found, and it
//! is what makes the three entry points below readable in full.
//!
//! # The three entry points, and the answer each one reads
//!
//! ## `+0x692a220` - speak
//!
//! ```text
//!  692a22d  movzx eax,BYTE PTR [rip+0x7e1390c]   # 0xe73db40   the once-flag
//!  692a236  je     0x692a2ef                        ; not built: build it
//!  692a23c  mov  rdi,[rip+0x7e138f5]             # 0xe73db38   the synthesizer
//!  692a243  mov  rsi,[rip+0x7dbf156]             # 0xe6e93a0   isSpeaking
//!  692a24a  call objc_msgSend
//!  692a250  test al,al
//!  692a252  je   0x692a277                         ; not speaking: go straight on
//!  692a263  mov  rdi,[rip+0x7e138ce]             # 0xe73db38
//!  692a26a  mov  rsi,[rip+0x7dc0977]             # 0xe6eabe8   stopSpeaking
//!  692a271  call objc_msgSend                     ; interrupt, then:
//  692a286  mov  rdi,[rip+0x7e138ab]             # 0xe73db38
//!  692a2a7  call 0x5d99b20                        ; the text, as an NSString
//!  692a2bd  mov  rsi,[rip+0x7dc092c]             # 0xe6eabf0   startSpeakingString:
//  692a2c7  mov  rdx,rax
//!  692a2ca  call objc_msgSend
//!  692a2da  call r14                              ; objc_release
//!  692a2ed  jmp  rax                              ; and return
//! ```
//!
//! ## `+0x692a470` - stop, at a boundary or not
//!
//! Tail-jumps `stopSpeaking:` to the singleton at `+0x692a4a6` when its own
//! argument is in `0..=0x18f` (`cmp eax,0x18f; ja` at `+0x692a480`), and
//! otherwise tail-jumps **the same selector** to a different object at
//! `+0x692a4c5` with `xor edx,edx`.
//!
//! ## `+0x692a530` - is it speaking
//!
//! ```text
//!  692a547  movzx eax,BYTE PTR [rip+0x7e135f2]   # 0xe73db40
//!  692a550  je   0x692a57e                         ; not built: build it
//!  692a552  mov  rdi,[rip+0x7e135df]             # 0xe73db38
//!  692a565  mov  rsi,[rip+0x7dbee34]             # 0xe6e93a0   isSpeaking
//!  692a56c  call objc_msgSend
//!  692a572  test al,al
//!  692a574  setne al
//!  692a57d  ret
//! ```
//!
//! **`-isSpeaking` is a readback**, and that is the only honest reason to look
//! harder before refusing. It is also why refusing is still right, and the reason
//! is the *other* one.
//!
//! # The missing capability, named
//!
//! `NSSpeechSynthesizer` is a client of the system's speech-synthesis service -
//! `speechsynthesizerd`, reached over an XPC connection, rendering through an
//! installed voice.
//!
//! **There is no such service here, and no voice either.** No
//! `speechsynthesizerd`, no XPC service to reach it over, no login session to
//! start one in, and no `/System/Library/Speech` with a voice in it - so
//! `+[NSSpeechSynthesizer defaultVoice]` has nothing to name either. The
//! framework class exists on macOS only as a handle on that service.
//!
//! # Why `nil` is refused rather than installed, and this is the sharpest case
//! # in the queue
//!
//! Because `-startSpeakingString:` is **one-way `void` and reports nothing**,
//! and because the guest reports the speech to the page *itself*.
//!
//! With `nil`:
//!
//! - `[[NSSpeechSynthesizer alloc] initWithVoice:0]` answers `nil`, the once-flag
//!   is still set, and the singleton stays `nil`.
//! - `-isSpeaking` answers `NO` - so the guest's readback is honest, and it
//!   concludes that nothing is speaking. That is true.
//! - `-stopSpeaking` and `-stopSpeaking:` are messages to `nil` and do nothing.
//! - `-startSpeakingString:` is a message to `nil` and **does nothing, and
//!   returns nothing to say so**.
//! - And then the guest carries on. Chromium's TTS stack reports `SPEECH_EVENT_`
//!   `START` to the renderer itself, and emits `END` from a completion timer, so
//!   the page receives `onstart` and `onend` for a synthesis that never
//!   happened.
//!
//! **So the run is silent, correct, and wrong at the same time, and the guest
//! has no way to find out.** This is the outcome the project ranks first: not a
//! crash, not a wrong number, but a success that is not one. It is the same
//! shape as `-[NSUserNotificationCenter deliverNotification:]` - also `void`,
//! also with the guest reporting the outcome to its own client optimistically -
//! and it is refused here for the same reason and with the same rule.
//!
//! **What would make it answerable:** a speech-synthesis service and an
//! installed voice. Both are the same missing platform as the notification
//! centre's, and the honest outcome until then is a refusal that says which.
//!
//! # The operational consequence, stated rather than discovered later
//!
//! **This refusal is a hard stop for any page that calls
//! `window.speechSynthesis.speak()`, and it is not on the milestone's path.**
//! The gate is `-initWithVoice:`, which is the first thing the speak path does -
//! `--dump-dom about:blank` asks for no speech, so the default run does not
//! reach it. That claim is a measurement, not an assumption, and it is reported
//! with the run that tested it rather than left here as a prediction.
//!
//! The refusal introduces **no new stop point**: with
//! `ROSETTA3_NAME_MISSING_CLASSES=1` the classref gets an anonymous empty class,
//! `+alloc` is inherited from `NSObject`, and `-initWithVoice:` is then an
//! *unrecognised selector*, which this runtime already answers by aborting by
//! name. Registering the class and refusing changes which line is printed at a
//! point that already stops, and nothing else. That is the same argument
//! `foundation::notification` makes, and the run is the evidence.
//!
//! # `ROSETTA3_LOG_UNRECOGNIZED=1`
//!
//! The runtime's own diagnostic switch answers `nil` instead of aborting on a
//! message no class answers. These are messages no class can answer, so leaving
//! them out of that switch would make this wall the one place the lever stops
//! working - and the lever is how the queue behind a wall is read in one run.
//! Under it the line is still printed, and **this line names the class, the
//! selector, the missing service and the caller**, which is more than the
//! switch's own line says. The default run, with the variable unset, still
//! aborts here.
//!
//! Once **per (class, selector)**, not once per process: a run that reached both
//! `SpeakText` and `StopSpeaking` would otherwise hear about only the first, and
//! the message would be wrong about the other.
//!
//! # What is deliberately not installed
//!
//! - `+[NSSpeechSynthesizer defaultVoice]` and
//!   `+[AVSpeechSynthesisVoice voiceWithIdentifier:]`. The `defaultVoice` send is
//!   at `+0x3ab29f1`, inside `SyntheticPinchEvent`, and its receiver is a
//!   **different** classref from the one this module is about - the queue's
//!   "5 sends" for `NSSpeechSynthesizer` counts the `initWithVoice:` sites only
//!   and lists `+0x3ab29ea` with no send, which is right: that load feeds
//!   `+defaultVoice` on the same class, and it is the only other place the
//!   classref is loaded that is not one of the five singleton sites. It is left
//!   alone rather than refused here, because the refusal would be about a
//!   different thing - see the next paragraph.
//! - `-[NSSpeechSynthesizer setVoice:]`, `-[NSSpeechSynthesizer setRate:]`,
//!   `setPitch:`, `setVolume:` and the rest of the surface. The census measured
//!   five sends to this class in the whole 214 MB of `__TEXT,__text`; the five
//!   are the table below.
//! - `speakString:`. It is sent at `+0x692a452` - but to a **different receiver**,
//! `[[0xdd50048]]`, not to the singleton, so installing it on
//!   `NSSpeechSynthesizer` would be installing a method on a class nothing
//!   messages. This is the "an array has no per-element reference" shape of
//!   mistake: a selector in the same function is not a selector on the same
//!   receiver.
//!
//! # What certifies these rows, and what does not
//!
//! **`test/tools/objcenc.py` cannot see the five rows in this module**, so the
//! project-level `NAME-ARITY` / `FRAME-SHORT` gate reports `0 / 0` **without
//! them**; `accessibility_remote_ui`'s comment gives the mechanism and it is the
//! same one here. The certifier is in this crate:
//! `the_declared_arity_is_the_selector_s` runs `encoding_check::assert_row` and
//! then `assert_installed_instance`, which reads `Method::types` back out of the
//! class's own method list. The `stopSpeaking:` row is the one whose *argument
//! type* is a judgement rather than a measurement, and the table says so beside
//! the row.

use std::collections::BTreeSet;
use std::ffi::c_void;
use std::sync::Mutex;

use objc::object::Id;
use objc::sel::Sel;

/// Published so a guest binding `_OBJC_CLASS_$_NSSpeechSynthesizer` gets a
/// class that **names itself** rather than an anonymous empty one.
pub const CLASSES: &[&str] = &["NSSpeechSynthesizer"];

/// What is missing, said in the refusal so the line says which capability and
/// not merely which class.
const MISSING_SPEECH_SERVICE: &str = "The missing capability is the system speech-synthesis \
    service: NSSpeechSynthesizer is a client of speechsynthesizerd, reached over an XPC \
    connection, rendering through an installed voice. There is no speechsynthesizerd on this host, no \
    XPC service to reach one over, no login session to start one in, and no installed voice for it \
    to render with - so +defaultVoice has nothing to name either. A nil here would be worse than a \
    fault: -startSpeakingString: is one-way void and reports nothing, and the guest reports the \
    speech to the page itself, so the page would get onstart and onend for a synthesis that never \
    happened.";

/// The caller, said in the refusal.
///
/// A `KeyError`-shaped fallback rather than a panic: a caller that has not been
/// measured is a gap, and the honest thing to print is that the census behind
/// this table has to be re-run.
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
    "NSSpeechSynthesizer",
    "The five sites are the five tests of one lazy singleton pair - the object at 0xe73db38 and the \
     once-flag byte at 0xe73db40 - inside 0x692a220..0x692a5e0, and each builds it with \
     -initWithVoice:0 (edx = 0, the default voice) at 0x692a31b, 0x692a365, 0x692a3af, 0x692a4f3 and \
     0x692a5a6. The three entry points are a speak path at 0x692a220 (-isSpeaking, \
     -stopSpeaking, -startSpeakingString:), a stop path at 0x692a470 that tail-jumps -stopSpeaking: \
     at 0x692a4a6, and a -isSpeaking readback at 0x692a530 whose BOOL is returned through setne al. \
     The image's own method list puts this region in -[KeyEquivalentAndModifierMask], and that name \
     is not evidence: the list is sparse, so 'enclosing' is the nearest preceding IMP, and this \
     0x430-byte region also holds the accessibility-token pair this project refuses in \
     accessibility_remote_ui.rs. The nearest preceding __FILE__ cstring for both is \
     ../../ui/base/cocoa/permissions_utils.mm, 0x9c7 bytes earlier with no marker in between, so \
     the bracket is reported rather than the file. What is Chromium's text-to-speech, and it is \
     reached from a page that calls speechSynthesis.",
)];

/// One selector and the type encoding declared for it.
type SelEntry = (&'static str, &'static std::ffi::CStr);

/// A class, its class methods and its instance methods.
type Measured = (&'static str, &'static [SelEntry], &'static [SelEntry]);

/// The classes this module refuses, with the selectors measured on each.
const SPEECH: &[Measured] = &[(
    "NSSpeechSynthesizer",
    // No class method: the guest never sends one to this class. `+alloc` is
    // reached through `_objc_alloc` and inherited from `NSObject`, and
    // `+defaultVoice` is at +0x3ab29f1 in `SyntheticPinchEvent`, which this
    // round did not attribute.
    &[],
    &[
        // `edx = 0` at every one of the five sites, so the argument is the
        // default voice and is never read.
        ("initWithVoice:", c"@24@0:8@16"),
        ("isSpeaking", c"B16@0:8"),
        ("stopSpeaking", c"v16@0:8"),
        // The third register of this send is never written in the function that
        // makes it: `+0x692a4a6` is a tail jump, so `rdx` is whatever the
        // caller passed. **The argument's width is therefore not determined by
        // the disassembly**, and `B` is declared rather than left blank because
        // a `void` method that refuses is never read back and the declaration
        // only has to be arithmetically honest. This is the one row in the
        // table whose type is a judgement, and it is called out here so a reader
        // does not mistake it for a measurement.
        ("stopSpeaking:", c"v24@0:8B16"),
        ("startSpeakingString:", c"v24@0:8@16"),
    ],
)];

/// `(class, selector)` pairs already named, so the refusal speaks once each.
static SAID: Mutex<BTreeSet<(String, String)>> = Mutex::new(BTreeSet::new());

/// Say once that there is no speech service, then stop.
///
/// `abort()` and not a return value, for the reason
/// `foundation::notification::refuse_system_notification` gives: a caller that
/// asked for speech is going to report the speech to its own client, and the
/// only value available here - `nil` - is one that produces a *successful* run
/// and a silent page. There is no honest value to return, so the only honest
/// result is to stop.
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
        "NSSpeechSynthesizer" => MISSING_SPEECH_SERVICE,
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
    for (class, class_methods, instance_methods) in SPEECH {
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

    /// The declared arity is the selector's own name's, the frame reaches the
    /// last argument, and the encoding that reached the runtime is the one
    /// declared - the three checks, because `objcenc.py` reads a `#[cfg(test)]`
    /// const table as readily as the install and would otherwise certify a copy
    /// of the encoding that never reached the class.
    #[test]
    fn the_declared_arity_is_the_selector_s() {
        crate::bootstrap();
        for (class, class_methods, instance_methods) in SPEECH {
            let owned = format!("{class}\0");
            let name =
                std::ffi::CStr::from_bytes_with_nul(owned.as_bytes()).expect("no interior NUL");
            for (selector, types) in class_methods.iter() {
                crate::encoding_check::assert_row(selector, types);
                crate::encoding_check::assert_installed(name, selector, types);
            }
            for (selector, types) in instance_methods.iter() {
                crate::encoding_check::assert_row(selector, types);
                crate::encoding_check::assert_installed_instance(name, selector, types);
            }
        }
    }

    /// Registered *and* published under the guest's own spelling.
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

    /// The class is **instantiable**, which is the half of the registration that
    /// a `class_methods!`-only module would miss: the guest calls `_objc_alloc`
    /// on it at all five sites, so a class that could not be allocated would
    /// fault before the refusal could name anything.
    #[test]
    fn the_class_can_be_instantiated_by_the_runtime_function_the_guest_calls() {
        crate::bootstrap();
        let class =
            objc::class::lookup(c"NSSpeechSynthesizer").expect("installed by this module");
        // SAFETY: `+alloc` and `-init` are `id (Class|id, SEL)`, and this is the
        // pair `objc_alloc_init` itself makes.
        let made = unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(objc::msg::entry());
            let object = send(objc::class::ptr(class) as Id, objc::sel::get("alloc"));
            assert!(!object.is_null(), "+alloc came back nil");
            send(object, objc::sel::get("init"))
        };
        // SAFETY: an object this runtime just allocated.
        let made_class = unsafe { objc::object::class_of(made) }.expect("a class");
        assert!(std::ptr::eq(made_class, class));
        // SAFETY: `-release` is `void (id, SEL)`.
        unsafe {
            let release: unsafe extern "C" fn(Id, Sel) =
                std::mem::transmute(objc::msg::entry());
            release(made, objc::sel::get("release"));
        }
    }

    /// Exactly the five measured selectors are refused, and `+alloc`,
    /// `+defaultVoice` and `speakString:` are not among them.
    ///
    /// Each exclusion is a measurement, and the last two are the ones a reader
    /// would otherwise expect to find: `+defaultVoice` is sent at `+0x3ab29f1` in
    /// another translation unit, and `speakString:` at `+0x692a452` goes to a
    /// different receiver. Installing either would be surface that reads as
    /// coverage of a send that never reaches this class.
    #[test]
    fn exactly_the_measured_selectors_are_refused() {
        crate::bootstrap();
        let class =
            objc::class::lookup(c"NSSpeechSynthesizer").expect("installed by this module");
        for (selector, _) in SPEECH[0].2.iter() {
            assert!(
                class.own_method(objc::sel::get(selector)).is_some(),
                "-[{selector}] is measured and must be refused by name"
            );
        }
        for selector in ["alloc", "defaultVoice", "speakString:"] {
            assert!(
                class.method(objc::sel::get(selector)).is_none(),
                "-[{selector}] is not a send to this class in the image, and a refusal on it \
                 would name a missing speech service for a message nothing sends here"
            );
        }
        assert!(
            class.metaclass().own_method(objc::sel::get("defaultVoice")).is_none(),
            "+defaultVoice is sent at +0x3ab29f1 inside SyntheticPinchEvent, a translation unit \
             this round did not attribute; it is reported rather than refused here"
        );
    }

    /// The refusal names a measured caller, and a caller record without an
    /// address is not a caller a reader can check.
    #[test]
    fn every_refused_class_names_a_measured_caller() {
        for (class, _, _) in SPEECH {
            let caller = caller_of(class);
            assert!(
                caller.contains("0x"),
                "{class}'s caller record names no address"
            );
        }
    }

    /// The missing-capability text names the service, and says why `nil` was
    /// refused - which is the part that keeps this from being a bare "not
    /// supported".
    #[test]
    fn the_missing_capability_is_named_and_the_nil_is_explained() {
        assert!(MISSING_SPEECH_SERVICE.contains("speechsynthesizerd"));
        assert!(MISSING_SPEECH_SERVICE.contains("void"));
        assert!(
            MISSING_SPEECH_SERVICE.contains("onstart"),
            "the line has to say what the page would be told, because that is the harm"
        );
    }

    /// The body a row gets is chosen by its **declared return type**, so a row
    /// that returns `id` is never given the `void` body - which would leave
    /// nothing defined in its return register on the way to the abort.
    #[test]
    fn the_body_is_chosen_by_the_declared_return_type() {
        let returning = refusing_returning as *const c_void;
        let returning_void = refuse_pick as *const c_void;
        for (selector, types) in SPEECH[0].2.iter() {
            let expects_object = types.to_bytes().first() == Some(&b'@');
            assert_eq!(
                expects_object,
                refuse_pick(types, returning, returning_void) == returning,
                "{selector} would get the {} body",
                if expects_object { "void" } else { "object-returning" },
            );
        }
        // `isSpeaking` is the one row that returns a scalar: it must get the
        // `void` body, because a `BOOL` refusal has nothing to put in `%rax`.
        assert_eq!(
            refuse_pick(c"B16@0:8", returning, returning_void),
            returning_void
        );
    }
}
