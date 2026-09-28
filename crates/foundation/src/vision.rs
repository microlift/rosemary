//! `-[VNDetectBarcodesRequest supportedSymbologiesAndReturnError:]` and
//! `+supportedSymbologies` answer `nil`, and `+class` is deliberately left to the
//! runtime.
//!
//! # The census
//!
//! The classref slot is `0xe6e74c0`, and the loader's own unbound report names
//! it: `_OBJC_CLASS_$_VNDetectBarcodesRequest from /System/Library/Frameworks/
//! Vision.framework/Versions/A/Vision`.
//!
//! | | |
//! |--|--:|
//! | confirmed loads of the classref | **3** |
//! | sends the queue's 96-byte window found | **5** |
//! | of those, sends of `+class` | **2** |
//! | of those, a message to **another** receiver with this class as the argument | **1** |
//! | of those, the **wall** | **2** |
//! | loads followed by `_objc_alloc*` | **1** |
//!
//! **Two of the five are not a wall**, for the reason
//! `accessibility_remote_ui.rs` found and states: `+class` is answered by the
//! runtime for **every** class, so `+[VNDetectBarcodesRequest class]` cannot be
//! unrecognised however empty the class is. Refusing it would replace a true
//! answer with a refusal at two sites that need nothing from a barcode
//! detector.
//!
//! # The two `+class` sites
//!
//! ```text
//!  3d1d0cd  mov  rdi,[0xe6e74c0]            ; the class
//!  3d1d0d4  mov  rsi,[0xe6e7ec0] = class
//!  3d1d0db  call objc_msgSend              ; +[VNDetectBarcodesRequest class]
//!  3d1d0e1  mov  rsi,[0xe6e7ed8] = isKindOfClass:
//!  3d1d0e8  mov  rdi,r13                   ; SOME OTHER REQUEST
//!  3d1d0eb  mov  rdx,rax                   ; the class just fetched
//!  3d1d0ee  call objc_msgSend              ; -[otherRequest isKindOfClass:<class>]
//!  3d1d0f4  test al,al ; je 0x3d1d106
//!  3d1d0f8  mov  rdi,r13 ; 0x3d1d0fb call _objc_retain ; mov r15,rax
//!  3d1d106  xor  r15d,r15d                ; not a barcode request
//! ```
//!
//! A **type test**, and the answer NO is the truth: nothing on this host creates a
//! `VNDetectBarcodesRequest`, so no request can be an instance of one.
//! `-isKindOfClass:` is sent to `r13`, not to this class, and the queue's
//! `selector` column putting it in this class's list is the window attributing a
//! send to the wrong receiver.
//!
//! The second, at `+0x3d225c3`, is the same `+class` and the answer goes straight
//! into a `std::function`'s captured `Class`:
//!
//! ```text
//!  3d225c3  mov  rdi,[0xe6e74c0]
//!  3d225ca  mov  rsi,[0xe6e7ec0] = class
//!  3d225d1  call objc_msgSend              ; the Class object
//!  3d225d7  mov  r12,rax
//!  3d225fb  mov  edi,0x40 ; call operator new
//!  3d2260c  mov  DWORD PTR [rax],0x1
//!  3d22612  lea  rax,[0x3d1fe00]           ; the std::function's vtable
//!  3d22619  mov  QWORD PTR [r13+0x8],rax
//! ```
//!
//! Still only `+class`. Not a wall.
//!
//! # The wall, and the `test` that is not about the answer
//!
//! ```text
//!  3d245eb  mov  edi,0x1
//!  3d245f0  mov  esi,0xc
//!  3d245f5  xor  edx,edx
//!  3d245f7  xor  ecx,ecx
//!  3d245f9  call 0x3e95e0                  ; a two-int struct by value:
//!                                          ;   base::Feature is {int,int} and
//!                                          ;   FeatureList::IsEnabled takes it
//!                                          ;   in %edi/%esi. Returns AL.
//!  3d245fe  mov  rdi,QWORD PTR [rip+0xa9c2ebb]   # 0xe6e74c0  the class
//!  3d24605  test eax,eax                    ; <-- ON THE FEATURE CHECK
//!  3d24607  je   0x3d24643
//!  3d24609  call _objc_alloc_init           ; [[VNDetectBarcodesRequest alloc] init]
//!  3d2460e  mov  r14,rax
//!  3d24611  mov  QWORD PTR [rbp-0x18],0x0   ; NSError* error = nil
//!  3d24619  mov  rsi,[0xe6e97e8] = supportedSymbologiesAndReturnError:
//!  3d24620  lea  rdx,[rbp-0x18]
//!  3d24624  mov  rdi,rax
//!  3d24627  call objc_msgSend              ; -[request supportedSymbologiesAndReturnError:&e]
//!  3d2462d  mov  rdi,rax ; retainAutoreleasedReturnValue ; mov rbx,rax
//!  3d24638  mov  rdi,r14 ; call _objc_release
//!  3d24641  jmp  0x3d2465b
//!  3d24643  mov  rsi,[0xe6e97f0] = supportedSymbologies
//!  3d2464a  call objc_msgSend              ; +[VNDetectBarcodesRequest supportedSymbologies]
//!  3d24650  mov  rdi,rax ; retainAutoreleasedReturnValue ; mov rbx,rax
//!  3d2465b  mov  rdi,rbx
//!  3d2465e  add  rsp,0x10 ; pop rbx ; pop r14 ; pop rbp
//!  3d24666  jmp  _objc_autoreleaseReturnValue
//! ```
//!
//! **`test eax,eax` at `+0x3d24605` is on the feature check, seven bytes after the
//! classref load and four before the `_objc_alloc_init` that uses it.** It is not
//! a test of anything this class answered. The queue's `nil-checked` column reads
//! `no x2, yes x3` for this class and neither half is about the answer: the three
//! "yes" are the `+class` sends, whose nil the guest *does* test, and the two
//! "no" are the sends whose answers are never tested because they are **returned
//!**.
//!
//! ## The one caller, and what it does with the answer
//!
//! A raw `call rel32` scan of the whole of `__TEXT,__text` for the target
//! `0x3d245e0` finds exactly one call site, `+0x3d23fe1`:
//!
//! ```text
//!  3d23fde  mov  rdi,rsi
//!  3d23fe1  call 0x3d245e0                 ; the function above
//!  3d23fe6  mov  rdi,rax
//!  3d23fe9  call _objc_retainAutoreleasedReturnValue
//!  3d23fee  mov  rbx,rax
//!  3d23ff1  mov  rsi,[0xe6e7e90] = count
//!  3d23ff8  mov  rdi,rax
//!  3d23ffb  call objc_msgSend              ; -[answer count]
//!  3d24004  mov  r15,rax                   ; the count, as a size
//!  3d24015  ...                            ; (end - begin) / 4
//!  3d24019  cmp  rax,r15 ; jae 0x3d24093   ; already have >= count: skip
//!  3d24028  cmp  r15,0x3fffffffffffffff ; ja 0x3d24530
//!  3d24038  lea  rdi,[r15*4+0x0]
//!  3d24040  call operator new             ; a vector of `count` four-byte elements
//! ```
//!
//! **The answer is used as a count and as a loop bound, and the nil-tolerant path
//! is the guest's own.** `-count` on a null array is 0, `0x3d24019`'s `jae` is
//! taken, the `operator new(0)` is skipped, and the caller ends up with an **empty
//! list of barcode symbologies**. There is no branch anywhere for "the platform
//! could not tell me", and none is needed.
//!
//! # Why `nil` is the truthful answer here, and not a stand-in
//!
//! `supportedSymbologiesAndReturnError:` asks which barcode symbologies **this
//! system** can decode. The answer is a property of the OS image: the Vision
//! framework's resources and the camera hardware's decoder. There is no Vision
//! framework on this host and no camera, so the true list is **empty**.
//!
//! A non-empty list would have to be written down here, and a written-down list of
//! symbologies is an invented constant of exactly the kind this project forbids -
//! worse, it is an invented constant the **guest would then act on**: Chromium's
//! Barcode Detection API answers `BarcodeDetector.getSupportedFormats()` from it,
//! so a fabricated list would tell a web page that this browser can decode QR
//! codes and Aztec codes, and every subsequent `detect()` would fail.
//!
//! `nil` is not a refusal here and should not be read as one. It is the platform
//! saying "none", which is true, and the guest's own arithmetic turns it into an
//! empty list.
//!
//! ## The `NSError **` is left untouched, and why that is right
//!
//! `+0x3d24611` writes `nil` into `[rbp-0x18]` and `+0x3d24620` passes its
//! address; nothing in the function reads `[rbp-0x18]` again, and the caller does
//! not receive it. So there is nothing to report an error **through**, and writing
//! one would be inventing a failure that did not happen. "No symbologies" is not
//! an error: the question was answered.
//!
//! # Which file: not established, and the bracket is wrong
//!
//! `test/tools/whichfile.py` brackets all three sites between
//! `../../services/resource_coordinator/memory_instrumentation/coordinator_impl.cc`
//! (`+0x7157`) and `../../services/shape_detection/detection_utils_mac.mm`
//! (`-0x155`). The second and third sit between
//! `detection_utils_mac.mm` (`+0x53a1`, `+0x73f7`) and
//! `../../mojo/public/c/system/thunks.cc` (`-0x4ef3`, `-0x2e9d`). **Both
//! brackets are wrong:** fetched at tag `130.0.6723.91`,
//! `services/shape_detection/detection_utils_mac.mm` is 116 lines and contains no
//! `supportedSymbologies` and no `VNDetectBarcodesRequest` outside one
//! `ObjCCastStrict`; `coordinator_impl.cc` is a memory-instrumentation service.
//!
//! `test/cstrings-in.py` over `0x3d22580:0x3d22700` returns only
//! `'Formats hint contains UNKNOWN BarcodeFormat.'` at `+0x3d226b4`, which is a
//! `LOG()` string from a **different** function in the same region and not a
//! `__FILE__` marker; over `0x3d245c0:0x3d246c0` it returns only `'vector'`. So
//! **the enclosing translation unit is not established and no claim is made
//! about it.** This is the **fifth** wrong `whichfile.py` attribution recorded
//! this session, and the failure mode is the documented one: a marker is emitted
//! by whichever function in the unit contains the first `LOG`, so a long unit's
//! markers are scattered and a bracket around them says very little. Everything
//! above rests on the disassembly.
//!
//! # Placement, and why it is `foundation` and not a new crate
//!
//! `VNDetectBarcodesRequest` is from `Vision.framework`, which has no crate in
//! this tree. This module is in `foundation` because `FRAMEWORKS` in
//! `crates/rosetta3/src/frameworks.rs` is `lib.flat(true)` - a data symbol
//! published by any crate binds for any framework, so which crate publishes
//! `_OBJC_CLASS_$_VNDetectBarcodesRequest` is cosmetic as far as the loader is
//! concerned - and because the module carries no state and no framework-specific
//! machinery. Moving it to a `crates/vision` is a `git mv` plus four registration
//! lines and nothing here would change. The orchestrator may prefer that, and
//! this says so rather than pretending the placement is principled.
//!
//! # What a real answer needs
//!
//! The macOS Vision framework's barcode decoder, and the hardware behind it. Not
//! derivable from the image and not a thing a host can be asked about.

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// Published so a guest binding `_OBJC_CLASS_$_VNDetectBarcodesRequest` gets a
/// class that **names itself** rather than an anonymous empty one.
pub const CLASSES: &[&str] = &["VNDetectBarcodesRequest"];

/// Every selector this module installs, with the encoding declared for it.
///
/// `#[cfg(test)]` because only the tests read it, as `spell_checker.rs:152` does:
/// a table-driven install written from a `const` is invisible to
/// `test/tools/objcenc.py`, so the certifier for these rows is in this crate.
#[cfg(test)]
const MEASURED_CLASS: &[(&str, &std::ffi::CStr)] =
    &[("supportedSymbologies", c"@16@0:8")];

#[cfg(test)]
const MEASURED_INSTANCE: &[(&str, &std::ffi::CStr)] =
    &[("supportedSymbologiesAndReturnError:", c"@24@0:8@?16")];

/// The shared body for both rows, and it answers **null**.
///
/// `0` is not a stand-in: it is what "no symbologies" is, and the caller's own
/// arithmetic - `-count` on the answer, used as a vector's size - turns null into
/// an empty list, which is what this host can truthfully claim. See this module's
/// comment for the disassembly of that arithmetic.
///
/// The `NSError **` argument is **deliberately not written to**. The guest
/// initialises it to null, never reads it back, and does not pass it on, and
/// "no symbologies" is not a failure - so writing an error would be inventing one.
unsafe extern "C" fn no_symbologies(_receiver: Id, _sel: Sel, _error: *mut Id) -> usize {
    0
}

/// The class-method row, with the same answer and no argument.
unsafe extern "C" fn no_symbologies_class(_class: Id, _sel: Sel) -> usize {
    0
}

pub(crate) fn install() {
    // `register`, not `abstract_class`: `call _objc_alloc_init` on this classref
    // is at `+0x3d24609`, so the guest **does** instantiate it, and an abstract
    // class would refuse that for a reason that has nothing to do with the
    // missing Vision framework.
    let class = corefoundation::base::register(
        c"VNDetectBarcodesRequest",
        None,
        size_of::<usize>(),
        false,
    );
    class_methods!(class, {
        "supportedSymbologies" => no_symbologies_class, c"@16@0:8";
    });
    methods!(class, {
        "supportedSymbologiesAndReturnError:" => no_symbologies, c"@24@0:8@?16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared arity is the selector's own name's, and the encoding **that
    /// reached the runtime** is the one declared rather than a copy of it.
    ///
    /// `@?` and not `^@`: this tree's encoding checker reads `^` as a complete
    /// type and then finds a non-digit where a number belongs, so `^@16` would
    /// panic with the encoding in the message. `@?` is the spelling
    /// `encoding_check`'s own doc comment gives for "a pointer to an unknown
    /// pointee", which is what `NSError **` is. It is a *declaration* either way -
    /// nothing reads it at dispatch - and the frame and offsets are what is
    /// checked.
    #[test]
    fn the_declared_arity_is_the_selector_s() {
        for (selector, types) in MEASURED_CLASS {
            crate::encoding_check::assert_row(selector, types);
            crate::encoding_check::assert_installed(
                c"VNDetectBarcodesRequest",
                selector,
                types,
            );
        }
        for (selector, types) in MEASURED_INSTANCE {
            crate::encoding_check::assert_row(selector, types);
            let installed = objc::class::lookup(c"VNDetectBarcodesRequest")
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

    /// Two selectors installed, and `+class` **not** among them.
    ///
    /// This is the negative half and it is the one the queue's count gets wrong:
    /// two of the five measured sends are `+class`, the runtime answers that for
    /// every class, and installing anything on it would replace a true answer
    /// with a refusal at two sites that need nothing from a barcode detector.
    #[test]
    fn exactly_the_measured_selectors_are_installed() {
        crate::bootstrap();
        let class = objc::class::lookup(c"VNDetectBarcodesRequest")
            .expect("installed by this module");
        for (selector, _) in MEASURED_CLASS {
            assert!(
                class.metaclass().own_method(objc::sel::get(selector)).is_some(),
                "+[{selector}] is measured and must be installed"
            );
        }
        for (selector, _) in MEASURED_INSTANCE {
            assert!(
                class.own_method(objc::sel::get(selector)).is_some(),
                "-[{selector}] is measured and must be installed"
            );
        }
        assert!(
            class.metaclass().own_method(objc::sel::get("class")).is_none(),
            "+class is answered by the runtime for every class; installing anything here would \
             break the two type tests at 0x3d1d0db and 0x3d225d1 for a reason that has nothing to \
             do with a missing Vision framework"
        );
    }

    /// Registered **and** published, and **instantiable**, because
    /// `_objc_alloc_init` is called on it at `+0x3d24609`.
    #[test]
    fn the_class_is_published_and_instantiable() {
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
        let class = objc::class::lookup(c"VNDetectBarcodesRequest").unwrap();
        // SAFETY: `+alloc` is `id (Class, SEL)`, the class is registered, and
        // `objc::msg::entry()` is this runtime's own `objc_msgSend`.
        let send: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let receiver = objc::class::ptr(class) as Id;
        // The guest's own chain, exactly: `[[VNDetectBarcodesRequest alloc] init]`.
        // `-init` is `NSObject`'s (`crates/objc/src/nsobject.rs:52`), so the
        // second message is answered by the runtime and not by this module.
        let object = unsafe { send(receiver, objc::sel::get("alloc")) };
        assert!(!object.is_null(), "+alloc must succeed");
        let object = unsafe { send(object, objc::sel::get("init")) };
        assert!(!object.is_null(), "-init must succeed: NSObject answers it");
    }

    /// Both rows answer **null**, and the consequence is the empty list the
    /// guest's caller then has.
    ///
    /// Measured the way the guest measures it: the caller at `+0x3d23ffb` sends
    /// `-count` to the answer and uses it as a vector's size, so the property
    /// under test is that a null answer produces a count of zero and no
    /// allocation.
    #[test]
    fn both_rows_answer_null_and_the_count_is_zero() {
        crate::bootstrap();
        let class = objc::class::lookup(c"VNDetectBarcodesRequest")
            .expect("installed by this module");
        // SAFETY: the class-method row is `id (Class, SEL)` as declared above and
        // installed by `install`.
        let class_method: unsafe extern "C" fn(Id, Sel) -> usize =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let from_class = unsafe {
            class_method(objc::class::ptr(class) as Id, objc::sel::get("supportedSymbologies"))
        };
        assert_eq!(from_class, 0, "+supportedSymbologies must answer null");

        // SAFETY: `+alloc` is `id (Class, SEL)`; then the instance row is
        // `id (id, SEL, NSError **)` as declared above.
        let send: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let receiver = objc::class::ptr(class) as Id;
        let object = unsafe { send(receiver, objc::sel::get("alloc")) };
        let object = unsafe { send(object, objc::sel::get("init")) };
        let mut error: Id = std::ptr::null_mut();
        let instance_method: unsafe extern "C" fn(Id, Sel, *mut Id) -> usize =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let answer = unsafe {
            instance_method(
                object,
                objc::sel::get("supportedSymbologiesAndReturnError:"),
                &mut error as *mut Id,
            )
        };
        assert_eq!(answer, 0, "-supportedSymbologiesAndReturnError: must answer null");
        assert!(
            error.is_null(),
            "the NSError out-parameter must be left alone: the guest initialises it to null, never \
             reads it back, and 'no symbologies' is not a failure - writing an error would be \
             inventing one"
        );
        // And the consequence, which is the whole argument for nil over a
        // written-down list.
        // SAFETY: `-count` is `NSUInteger (id, SEL)`.
        let count: unsafe extern "C" fn(Id, Sel) -> u64 =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        assert_eq!(
            unsafe { count(answer as Id, objc::sel::get("count")) },
            0,
            "a null answer must count as zero symbologies, which is what the guest's caller at \
             +0x3d23ffb turns into an empty list"
        );
    }
}
