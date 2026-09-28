//! `NSPopUpButton`: refused by name. The control itself is not implemented in
//! this tree, and a stand-in would be a file dialog that draws a button and
//! opens nothing.
//!
//! # The census, and where the queue under-reports it
//!
//! The classref slot is `0xe6e7a18`, and the loader's own unbound report names
//! it: `_OBJC_CLASS_$_NSPopUpButton from /System/Library/Frameworks/
//! AppKit.framework/Versions/C/AppKit`.
//!
//! | | |
//! |--|--:|
//! | confirmed loads of the classref | **1** |
//! | sends the queue's 96-byte window found | **3** |
//! | of those, sends to the **class object** | **1** |
//! | of those, sends to the **instance** the guest allocated | **2** |
//! | sends to the instance that the queue does **not** count | **3+** |
//! | of the instance sends, selectors **not in the image** | **1** |
//!
//! **The queue's `inst` column reads `-` for this class and that is a tool bug,
//! not a measurement.** `+0x815fb02` is `call _objc_alloc` on the classref, seven
//! bytes after the load, so the guest *does* instantiate `NSPopUpButton`.
//! `test/tools/classcensus.py:162` writes `stubnames[va] = nm.lstrip("_")` while
//! `ALLOCATORS` (line 102) keeps the leading underscore, so `name in
//! ALLOCATORS` is false for every stub and the column reads `-` for all 122
//! rows. Verified by running the constants:
//!
//! ```text
//! stubnames value            = 'objc_alloc'
//! 'objc_alloc' in ALLOCATORS = False
//! ```
//!
//! **The larger gap is the one the load count cannot see.** A classref load
//! counts the *allocation* for an instance wall, not the messages: every send
//! after it has the instance as its receiver and no classref load anywhere near
//! it. Three further sends to the instance are within 200 bytes and are counted
//! by nobody.
//!
//! # The one load, and the whole construction, from the bytes
//!
//! ```text
//!  815fafb  mov  rdi,QWORD PTR [rip+0x6587f16]   # 0xe6e7a18  the class
//!  815fb02  call _objc_alloc
//!  815fb07  mov  rsi,[0xe6ec1a8] = initWithFrame:pullsDown:
//!  815fb0e  mov  r15,[0xdd503b0]                 ; _NSZeroRect
//!  815fb15  movups xmm0,[r15]                    ;   origin.x, origin.y
//!  815fb19  movups xmm1,[r15+0x10]               ;   size.width, size.height
//!  815fb1e  movups [rsp+0x10],xmm1
//!  815fb23  movups [rsp],xmm0
//!  815fb27  mov  rdi,rax                         ; the allocation
//!  815fb2a  xor  edx,edx                         ; pullsDown: NO
//!  815fb2c  call objc_msgSend                   ; -initWithFrame:pullsDown:
//!  815fb32  mov  QWORD PTR [rbp-0x48],rax         ; <-- the button, into the stack slot
//!  815fb36  mov  rdi,QWORD PTR [rbp-0x48]
//!  815fb3a  mov  rsi,r14                         ; <-- a selector from the CALLER
//!  815fb3d  xor  edx,edx                         ;     one argument: nil
//!  815fb3f  call objc_msgSend                   ; -[button <r14> nil]
//!  815fb45  mov  rsi,[0xe6e9208] = setTag:
//!  815fb4c  mov  edx,0x4d2                       ; 1234
//!  815fb51  mov  rdi,QWORD PTR [rbp-0x48]
//!  815fb55  call objc_msgSend                   ; -[button setTag:1234]
//!  ...
//!  815fb9f  mov  rsi,r14 ; 0x815fbb1 call      ; -[<NSView> r14 nil]
//!  815fbaa  mov  rsi,[0xe6e87b0] = addSubview:
//!  815fbb1  mov  rdx,QWORD PTR [rbp-0x48]        ;   the button
//!  815fbb1  call objc_msgSend                   ; -[<NSView> addSubview:button]
//!  ...
//!  815fc35  mov  rsi,[0xe6ec1b0] = topAnchor
//!  815fc6d  mov  rsi,[0xe6ec1b8] = constraintEqualToAnchor:constant:
//!  815fcc5  mov  rsi,[0xe6ec1c0] = leadingAnchor
//! ```
//!
//! Three things are read out of the image rather than assumed: the frame is
//! `_NSZeroRect` (`0xdd503b0` holds four zero doubles, checked), `pullsDown:` is
//! `NO` (`xor edx,edx`), and the tag is **1234** (`mov edx,0x4d2`).
//!
//! ## The one selector I could not resolve, and am not going to invent
//!
//! `+0x815fb3f` sends a **two-argument** message to the button with its selector
//! in `%r14`, and `%r14` is `mov r14,r8` at `+0x815f74b` - the **fifth parameter
//! of the enclosing C++ function**, seven parameters in all
//! (`rdi, esi, rdx, rcx, r8, r9d, [rbp+0x18]`). It is not a `__objc_selrefs`
//! load, so `selcensus` cannot see it.
//!
//! I could not recover it either: a raw `call rel32` scan over the whole of
//! `__TEXT,__text` for a target in `0x815f6e0 .. 0x815f720` finds **zero** call
//! sites, so the function is entered through a function pointer and the value in
//! `%r8` is not statically recoverable from the image.
//!
//! **So the two selectors refused below are a lower bound on this class's
//! surface, and this is a gap rather than a decision.** A third selector exists
//! and is not named here. That does not change the answer - the class is absent
//! from `classes::HIERARCHY` either way - but a reader should know the
//! measurement is incomplete rather than assume it is not.
//!
//! ## The queue's `initWithFrame:pullsDown:` x2 is one send and one wrong label
//!
//! The second "send" the queue counted is `+0x815fb3f`, and it is labelled
//! `initWithFrame:pullsDown:` because the window's "last argument register loaded
//! from `__objc_selrefs`" rule falls back to `+0x815fb07` when the selector
//! arrives in a register (`mov rsi,r14`, not `mov rsi,[rip+…]`). That is the
//! failure `classcensus.py`'s own docstring calls "one class object can carry
//! several messages in one window and one message can arrive with the selector
//! already in a register" - here it arrives in a register from **outside** the
//! function.
//!
//! # Who the caller is
//!
//! **Positively established, not bracketed**: the `__FILE__` string
//! `../../components/remote_cocoa/app_shim/select_file_dialog_bridge.mm` at
//! `0x815f874` is read **from inside** the enclosing function
//! (`0x815f710 .. 0x8161e30`), by `test/cstrings-in.py`. That is Chromium's
//! **remote file dialog**, and the class the image's `__objc_const` method list
//! names for the enclosing method is `ExtensionDropdownHandler`, whose job is to
//! put a drop-down of allowed file extensions at the bottom of the dialog.
//!
//! ## So this is not on the milestone's path, and saying so is part of the answer
//!
//! `remote_cocoa`'s file dialog is the mojo path a **remote** front end uses; the
//! milestone is `--headless --dump-dom about:blank`, which opens no dialog. So
//! refusing this class costs the milestone nothing today. It is still worth
//! refusing rather than leaving to an anonymous empty class, because an empty
//! class turns the first message into a generic abort with no name, and this one
//! is reached from a function whose own `__FILE__` string is right there.
//!
//! # The missing capability, named
//!
//! The pop-up button itself. `NSPopUpButton` is an `NSControl` with a title, an
//! ordered list of items, a selected index, a cell that draws the current
//! selection, and a menu that pops under the button when it is clicked. AppKit
//! in this tree registers `NSControl` and `NSButton` in `classes::HIERARCHY` and
//! **no `NSPopUpButton` row**, and this module does not add one.
//!
//! The alternative was considered and rejected: a `NSPopUpButton` that is an
//! `NSControl` with no items, no selection and no menu would satisfy
//! `initWithFrame:pullsDown:` and `setTag:` exactly, would cost two lines, and
//! would put a **dead button in a file dialog** - a control the user can see and
//! click that opens nothing. `classes::class_data()` publishes a class whether or
//! not it can do anything, so the guest would hold a real `NSControl` it believes
//! is a working drop-down. That is the fabricated-plausible-value failure with a
//! widget on it.
//!
//! # What a real answer needs
//!
//! `NSControl` item storage and a menu-popping action: `-addItemWithTitle:`,
//! `-itemAtIndex:`, `-indexOfSelectedItem`, `-selectItemAtIndex:`,
//! `-removeAllItems`, `-titleOfSelectedItem`, and a cell class that draws the
//! selection. None of that is a two-line addition and none of it is a *capability
//! of the host* - it is unfinished AppKit in this tree, and the honest outcome
//! until it is written is a refusal that says so.
//!
//! # `ROSETTA3_LOG_UNRECOGNIZED`
//!
//! These two selectors are messages no class answers, so leaving them out of that
//! switch would make this wall the one place the lever stops working - and the
//! lever is how the queue behind a wall is read in one run. Under it the line is
//! still printed and it names the class, the selector, the missing control and
//! the caller. The default run, with the variable unset, still aborts here.

use std::collections::BTreeSet;
use std::sync::Mutex;

use foundation::geometry::NSRect;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

/// Published so a guest binding `_OBJC_CLASS_$_NSPopUpButton` gets a class that
/// **names itself** rather than an anonymous empty one.
pub const CLASSES: &[&str] = &["NSPopUpButton"];

/// What is missing, said in the refusal so the line names the capability and
/// not merely the class.
const MISSING_POP_UP_CONTROL: &str = "The missing capability is the pop-up button control \
    itself. NSPopUpButton is an NSControl with a title, an ordered list of items, a selected index, a \
    cell that draws the current selection, and a menu that pops under the button when it is clicked. \
    AppKit in this tree registers NSControl and NSButton in classes::HIERARCHY and no NSPopUpButton \
    row, and this module does not add one, because a control with no items, no selection and no menu \
    would satisfy initWithFrame:pullsDown: and setTag: exactly and would put a dead button in a file \
    dialog - something the user can see and click that opens nothing.";

/// The caller, said in the refusal, by name and by address.
const CALLERS: &str = "The single load of the classref is at 0x815fafb, and call _objc_alloc \
    follows it seven bytes later at 0x815fb02, so the guest instantiates this class - which the \
    queue's inst column reports as '-' only because test/tools/classcensus.py compares a \
    lstripped stub name against a set that keeps the leading underscore. \
    -initWithFrame:pullsDown: is at 0x815fb2c with _NSZeroRect (0xdd503b0, four zero doubles) and \
    pullsDown:NO, -setTag:1234 at 0x815fb55, and the button is then added with -addSubview: at \
    0x815fbb1 and anchored with -topAnchor / -leadingAnchor / \
    -constraintEqualToAnchor:constant: at 0x815fc35, 0x815fcc5 and 0x815fc6d. The enclosing \
    function is -[ExtensionDropdownHandler …] in \
    components/remote_cocoa/app_shim/select_file_dialog_bridge.mm - established positively, \
    because that file's __FILE__ string at 0x815f874 is read from inside the function. One more \
    message goes to the button at 0x815fb3f whose selector is in the caller's fifth parameter and \
    is NOT named here: a raw call rel32 scan of the whole of __TEXT,__text for a target in \
    0x815f6e0..0x815f720 finds no call site, so the function is entered through a function \
    pointer and the selector is not statically recoverable. These two selectors are therefore a \
    lower bound on this class's surface, and that is a gap in the measurement rather than a \
    decision. The path is the remote file dialog, which --headless --dump-dom about:blank never \
    opens, so this refusal costs the milestone nothing today.";

/// `(class, selector)` pairs already named, so the refusal speaks once each.
static SAID: Mutex<BTreeSet<(String, String)>> = Mutex::new(BTreeSet::new());

/// Say once that there is no pop-up button here, then stop.
fn refuse(class: &str, sel: Sel) {
    // SAFETY: `objc::sel::name` is the selector itself, which `sel_registerName`
    // made a NUL-terminated string, and this is the only read of it.
    let name = unsafe { std::ffi::CStr::from_ptr(objc::sel::name(sel)) };
    let selector = name.to_string_lossy().into_owned();
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if said.insert((class.to_string(), selector.clone())) {
        eprintln!(
            "rosetta3: appkit: {class} {selector}: refused by name. {MISSING_POP_UP_CONTROL} \
             {CALLERS}{switch}",
            switch = if objc::msg::logging_unrecognised() {
                " ROSETTA3_LOG_UNRECOGNIZED is set, so this refusal is answering nil instead of \
                 aborting; a run this way cannot be trusted."
            } else {
                ""
            },
        );
    }
    match objc::msg::logging_unrecognised() {
        // `receiver` is not a stand-in for an answer: it is what "answer nil"
        // means, and the only way out of this function.
        true => {}
        false => std::process::abort(),
    }
}

/// The IMP for `-initWithFrame:pullsDown:`.
///
/// The `NSRect` is four `double`s passed **by value**, which the C ABI puts in
/// `%xmm0` and `%xmm1` - which is exactly what the guest's own four `movups`
/// instructions before the call at `+0x815fb2c` are loading. The `pullsDown:`
/// flag is the next integer register, and `xor edx,edx` is the guest passing
/// `NO`. Neither is read: a refusal that ignored them would still be a refusal,
/// and reading them and doing nothing with them is the part that would be a lie.
unsafe extern "C" fn refusing_init(
    receiver: Id,
    sel: Sel,
    _frame: NSRect,
    _pulls_down: bool,
) -> usize {
    // SAFETY: a receiver here is a live object this runtime allocated; a null
    // receiver cannot reach an IMP, because `objc_msgSend` returns before one is
    // chosen. The class name is read off the **receiver** rather than baked in,
    // so the message cannot name a class other than the one it was installed on.
    let class = unsafe { objc::object::class_of(receiver) }
        .map_or("<no class: the receiver was null>", |c| c.name_str());
    refuse(class, sel);
    0
}

/// The IMP for `-setTag:`, whose `NSInteger` is a signed 64-bit word.
unsafe extern "C" fn refusing_void_one(receiver: Id, sel: Sel, _tag: i64) {
    // SAFETY: as above.
    let class = unsafe { objc::object::class_of(receiver) }
        .map_or("<no class: the receiver was null>", |c| c.name_str());
    refuse(class, sel);
}

/// Every selector this module installs, with the encoding declared for it.
///
/// `#[cfg(test)]` because only the tests read it, exactly as
/// `spell_checker.rs:152` does: a table-driven install written from a `const`
/// would be invisible to `test/tools/objcenc.py`, and the certifier for the rows
/// is therefore in this crate.
///
/// The first row is the awkward one and the reason is worth stating: an
/// `NSRect` is a **struct**, and `encoding_check::assert_row` refuses a struct
/// argument **by design** - "a struct is one Rust parameter and several words,
/// and pretending otherwise is how a wrong offset gets called a pass". So
/// `assert_row` is not called on it, and `the_struct_row_is_checked_by_hand`
/// checks the two properties that survive the refusal: the offsets, and the frame
/// against the struct's own width.
#[cfg(test)]
const MEASURED: &[(&str, &std::ffi::CStr)] = &[
    (
        "initWithFrame:pullsDown:",
        // `self` at 0, `_cmd` at 8 (unencoded), the rect at 16 - four doubles, so
        // 32 bytes - and the `BOOL` at 48. Apple's own string for this selector
        // is `@64@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16B48`; 64 rather than 56 is
        // frame padding, which nothing reads at dispatch, and this tree's rule is
        // `frame >= last_offset + 8` with the struct's own width accounted for.
        // MEMORY.md's measurement - "a `c` argument is `+4` in 102 of 102 of the
        // guest's own declarations and `+8` in none" - is what puts the flag at
        // 48 rather than 44.
        c"@56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16B48",
    ),
    // One colon, one `NSInteger` (`q`, a signed long on this ABI), frame
    // `8 * (2 + 1)` = 24.
    ("setTag:", c"v24@0:8q16"),
];

/// The struct row's selector, on its own, so the test can find the row without
/// matching on a name it also uses as a table key.
#[cfg(test)]
const STRUCT_ROW: &str = "initWithFrame:pullsDown:";

pub(crate) fn install() {
    // `register`, not `abstract_class`, and not a row in `classes::HIERARCHY`:
    // `call _objc_alloc` on this classref is at `+0x815fb02`, so the class **is**
    // instantiated by the guest and has to be instantiable here or the refusal
    // would be reported for a different reason than the missing control.
    //
    // It is deliberately **not** given `NSControl` as a superclass. `NSPopUpButton`
    // *is* an `NSControl` on macOS, and giving it that superclass would make the
    // guest's own inherited messages (`-addSubview:`, `-setFrame:`,
    // `-topAnchor`) resolve, so a partial pop-up button would look like a
    // working one. Registered bare, every message the guest sends to it lands on
    // this module's refusal or on the runtime's own "no class answers" - which is
    // the outcome a control that does not exist should have.
    let class =
        corefoundation::base::register(c"NSPopUpButton", None, size_of::<usize>(), false);
    methods!(class, {
        "initWithFrame:pullsDown:" => refusing_init,
            c"@56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16B48";
        "setTag:" => refusing_void_one, c"v24@0:8q16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared arity is the selector's own name's - except for the struct
    /// row, which `assert_row` cannot certify, and the encoding **that reached
    /// the runtime** is the one declared rather than a copy of it.
    #[test]
    fn the_declared_arity_is_the_selector_s() {
        for (selector, types) in MEASURED {
            if *selector != STRUCT_ROW {
                crate::encoding_check::assert_row(selector, types);
            }
            crate::encoding_check::assert_installed_instance(
                c"NSPopUpButton",
                selector,
                types,
            );
        }
    }

    /// The struct row's two surviving properties, checked by hand and **named as
    /// a substitute** rather than passed off as `assert_row`.
    ///
    /// `assert_row` refuses a struct argument, and that refusal is the right one:
    /// a struct is one Rust parameter and four words on the stack, so a checker
    /// that treated `{CGRect=…}16` as one 8-byte argument would certify an offset
    /// that is wrong by 24. What is left to check by hand is the offsets and the
    /// frame, and both are pinned here against numbers read out of the guest:
    /// `self` at 0, the rect at 16 (`_cmd` at 8, unencoded), the `BOOL` at 48, and
    /// a frame that reaches `48 + 8`.
    #[test]
    fn the_struct_row_is_checked_by_hand() {
        let (_, types) = MEASURED
            .iter()
            .find(|(selector, _)| *selector == STRUCT_ROW)
            .expect("the struct row is in MEASURED");
        assert_eq!(
            types.to_bytes()[0],
            b'@',
            "-initWithFrame:pullsDown: returns an object, so the object-returning body is the one \
             that has to be installed"
        );
        // The rect is four `double`s: `{CGPoint=dd}{CGSize=dd}`, so 32 bytes, and
        // the encoding has to say so or a reader cannot tell how wide it is.
        assert!(
            types
                .to_bytes()
                .windows(b"{CGPoint=dd}{CGSize=dd}".len())
                .any(|w| w == b"{CGPoint=dd}{CGSize=dd}"),
            "the rect argument is declared as a CGRect of two CGPoint/CGSize structs, and this \
             check is what would notice if it stopped being one"
        );
        let colons = STRUCT_ROW.bytes().filter(|b| *b == b':').count();
        assert_eq!(colons, 2, "two colons: the frame argument and the flag");
        // Offsets: self at 0, the rect at 16, the flag at 16 + 32.
        let frame = crate::encoding_check::declared_frame(types);
        assert!(
            frame >= 48 + 8,
            "the frame is {frame} and the last argument is the BOOL at 48, so the frame has to \
             reach {} for objc_msgSend's register to be inside it",
            48 + 8
        );
    }

    /// Two selectors refused, and **only** those two.
    ///
    /// The list of unmeasured selectors is the point: `-addItemWithTitle:`,
    /// `-removeAllItems`, `-indexOfSelectedItem` and `-selectItemAtIndex:` are the
    /// surface a working pop-up button would need, and none of them is installed.
    /// They are not installed because they were **not measured** - the one
    /// unresolved selector is disclosed above and the rest are not sent anywhere
    /// in the image - and a refusal names a missing capability *at a site the
    /// guest has*. An invented one names it at a site it does not.
    ///
    /// Nor is there a `classes::HIERARCHY` row and a `NSControl` superclass, and
    /// that is deliberate: with either, the guest's `-addSubview:` at `+0x815fbb1`
    /// and `-topAnchor` at `+0x815fc35` would succeed and the guest would hold a
    /// control it believes works. `install` registers the class bare, so the only
    /// IMPs it has are the two below.
    #[test]
    fn exactly_the_measured_selectors_are_installed() {
        crate::bootstrap();
        let class =
            objc::class::lookup(c"NSPopUpButton").expect("installed by this module");
        for (selector, _) in MEASURED {
            assert!(
                class.own_method(objc::sel::get(selector)).is_some(),
                "-[{selector}] is measured and must be refused by name"
            );
        }
        for unmeasured in [
            "addItemWithTitle:",
            "removeAllItems",
            "indexOfSelectedItem",
            "selectItemAtIndex:",
        ] {
            assert!(
                class.own_method(objc::sel::get(unmeasured)).is_none(),
                "-{unmeasured} is not measured and must not be installed: a refusal names a missing \
                 capability at a site the guest has, and an invented one names it at a site it does \
                 not"
            );
        }
        // `classes::HIERARCHY` is the other half: a row there would make the
        // class a real `NSControl` and the guest's wiring would resolve.
        assert!(
            !crate::classes::names().contains(&"NSPopUpButton"),
            "NSPopUpButton must not have an NSControl row: the control is not implemented here and \
             a partial one would look like a working drop-down"
        );
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
                crate::data_symbols()
                    .iter()
                    .any(|(symbol, _)| symbol == &format!("_OBJC_CLASS_$_{name}")),
                "{name} is published as data, not only registered"
            );
        }
    }

    /// The class is **instantiable**, because the guest allocates it.
    ///
    /// `call _objc_alloc` on this classref is at `+0x815fb02`, seven bytes after
    /// the load at `+0x815fafb`. A class registered as abstract would refuse that
    /// for a reason that has nothing to do with the missing control, and the guest
    /// would report that instead of this refusal.
    #[test]
    fn the_class_is_instantiable_because_the_guest_allocates_it() {
        crate::bootstrap();
        let class =
            objc::class::lookup(c"NSPopUpButton").expect("installed by this module");
        // SAFETY: `+alloc` is `id (Class, SEL)`, the class is registered, and
        // `objc::msg::entry()` is this runtime's own `objc_msgSend`.
        let alloc: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let object = unsafe { alloc(objc::class::ptr(class) as Id, objc::sel::get("alloc")) };
        assert!(!object.is_null(), "+alloc on this class must succeed");
        // And the object's own class is the one the refusal will name, read
        // with the runtime's own `class_of` rather than by sending
        // `-description` - a message this tree does not implement, so sending it
        // would abort the test binary for a reason that has nothing to do with
        // the property under test.
        //
        // SAFETY: `object` is live and `class_of` is a field read.
        let of = unsafe { objc::object::class_of(object) };
        assert_eq!(
            of.map(|c| c.name_str()),
            Some("NSPopUpButton"),
            "the allocation's class must be the one the refusal names"
        );
    }

    /// The missing capability is named, and the line says what a stand-in would
    /// look like - not only that the control is absent.
    #[test]
    fn the_missing_capability_is_named_rather_than_declared_unsupported() {
        assert!(MISSING_POP_UP_CONTROL.contains("NSControl"));
        assert!(
            MISSING_POP_UP_CONTROL.contains("dead button"),
            "the line has to say what the two-line stand-in would have produced, because that is \
             the alternative a reader has to weigh"
        );
        assert!(
            MISSING_POP_UP_CONTROL.contains("classes::HIERARCHY"),
            "the line has to name where the class is absent, because 'not implemented here' is \
             only actionable if the reader is told where to look"
        );
        for site in ["0x815fafb", "0x815fb02", "0x815fb2c", "0x815fb55", "0x815fb3f"] {
            assert!(
                CALLERS.contains(site),
                "the refusal names no address for {site}, and the address is the checkable part"
            );
        }
        // And the unresolved selector is disclosed rather than quietly dropped.
        assert!(
            CALLERS.contains("NOT named here"),
            "the refusal has to say that one selector could not be recovered, or a reader will \
             take these two rows for the whole surface"
        );
    }
}
