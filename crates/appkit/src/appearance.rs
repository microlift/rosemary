use std::sync::atomic::{AtomicUsize, Ordering};

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// `NSAppearanceNameAqua`.
///
/// Spelled as AppKit spells the symbol, because that is how the guest reaches
/// it: `Framework+0x81424e0` loads the `__got` slot at `+0xdd50508`, whose
/// indirect symbol is `_NSAppearanceNameAqua`.
const AQUA: &str = "NSAppearanceNameAqua";

/// `NSAppearanceNameDarkAqua`, from the `__got` slot at `+0xdd50500`, whose
/// indirect symbol is `_NSAppearanceNameDarkAqua`.
const DARK_AQUA: &str = "NSAppearanceNameDarkAqua";

static AQUA_OBJECT: AtomicUsize = AtomicUsize::new(0);
static DARK_AQUA_OBJECT: AtomicUsize = AtomicUsize::new(0);

/// The slot an appearance name is cached in, or `None` for a name this host has
/// no appearance for.
fn slot_for(name: &str) -> Option<&'static AtomicUsize> {
    Some(match name {
        AQUA => &AQUA_OBJECT,
        DARK_AQUA => &DARK_AQUA_OBJECT,
        _ => return None,
    })
}

/// The appearance `name` names, if one has already been made.
///
/// Reads without creating, so that a getter never has the side effect of
/// allocating.
fn peek(name: &str) -> Option<Id> {
    let existing = slot_for(name)?.load(Ordering::Acquire);
    match existing {
        0 => None,
        existing => Some(existing as Id),
    }
}

/// The appearance `name` names, made once and then kept for the process.
fn named(name: &str) -> Option<Id> {
    if let Some(existing) = peek(name) {
        return Some(existing);
    }
    let slot = slot_for(name)?;
    let class = objc::class::lookup(c"NSAppearance").expect("declared before its methods");
    let appearance = objc::object::create_instance(class, 0);
    // AppKit hands these out for the lifetime of the process. Keep one alive
    // even if a non-ARC guest releases its result.
    objc::arc::retain(appearance);
    match slot.compare_exchange(0, appearance as usize, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => Some(appearance),
        Err(existing) => {
            objc::arc::release(appearance);
            Some(existing as Id)
        }
    }
}

/// The process-wide `NSAppearanceNameAqua` object.
pub fn aqua() -> Id {
    named(AQUA).expect("a name this function matched")
}

/// The process-wide `NSAppearanceNameDarkAqua` object.
pub fn dark_aqua() -> Id {
    named(DARK_AQUA).expect("a name this function matched")
}

/// The `__got` cells for the two exported name symbols.
///
/// The guest reads each of these as **the address of a pointer**, not as the
/// address of a string: `Framework+0x81424e0` does `mov rax, [rip+..]` to pick up
/// the cell and then `mov rdx, [rax]` to read the `NSString *` out of it, and
/// `Framework+0x3ab5202` does the same. So what has to be published is a word
/// holding the string.
///
/// This is load-bearing, not a detail of how the two names are spelled. The
/// runtime's own unbound-import report lists `_NSAppearanceNameDarkAqua`
/// (`from AppKit.framework`) and **not** `_NSAppearanceNameAqua`, so without
/// this the dark name's `__got` word read as zero, the argument arrived as nil,
/// and `+appearanceNamed:` was being asked about nothing - a nil that skips the
/// caller's block silently rather than loudly. The name string *is* the
/// appearance's identity in this model, and it is the same literal `-name`
/// already returned, so the cell and `-name` cannot drift apart.
///
/// `_kCATransactionDisableActions`, the CATransaction key, is in that same
/// report and is deliberately **not** published here; see `crate::quartzcore`
/// for why.
pub fn name_cells() -> Vec<(String, u64)> {
    [
        ("_NSAppearanceNameAqua", AQUA),
        ("_NSAppearanceNameDarkAqua", DARK_AQUA),
    ]
    .into_iter()
    .map(|(symbol, text)| {
        let cell =
            corefoundation::base::global_cell(symbol, foundation::notification::name_object(text));
        (symbol.to_string(), cell)
    })
    .collect()
}

/// `-[NSAppearance name]`, the documented name used to compare appearances.
///
/// An appearance this process made reports the name it was made for. One that
/// came from somewhere else - a nib, say - reports [`AQUA`], which is the
/// truthful statement about this host: there is no appearance server, so Aqua
/// is what everything is effectively drawn with.
pub(crate) unsafe extern "C" fn name(receiver: Id, _sel: Sel) -> Id {
    let named = match peek(DARK_AQUA) {
        Some(dark) if dark == receiver => DARK_AQUA,
        _ => AQUA,
    };
    foundation::notification::name_object(named) as Id
}

/// `+[NSAppearance appearanceNamed:]`.
///
/// **Only the two names above resolve, and every other name is nil - which is
/// Apple's own answer for a name it does not know rather than a stand-in for
/// one.** The guest sends this six times and every one of them is one of the
/// two: `Framework+0x3ab5216` and `Framework+0x8142497` reach it with
/// `_NSAppearanceNameAqua` or `_NSAppearanceNameDarkAqua` straight out of a
/// `__got` slot, never a computed string.
///
/// A refusal was the alternative and it would have been worse than nil in both
/// directions. The guest does **not** check this result: at
/// `Framework+0x3ab5216` the answer goes into `r13` and is the receiver of the
/// next send at `+0x3ab5280`, and `Framework+0x8142497` does the same into `r14`
/// for `+0x8142547`. So a nil here is not a crash - messaging nil is defined
/// and this runtime implements it - but it is *silent*: the appearance-scoped
/// block the caller passed would simply never run, and the drawing it was
/// meant to switch would stay light while the rest of the program believed it
/// had switched. Two names that AppKit guarantees to have, answered with the
/// two objects it would hand out, is the answer that cannot mislead.
unsafe extern "C" fn appearance_named(_class: Id, _sel: Sel, name: Id) -> Id {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the selector's argument is an `NSAppearanceName`, which is an
    // `NSString`.
    let name = match String::from_utf8(unsafe { foundation::string::bytes(name) }) {
        Ok(name) => name,
        Err(_) => return std::ptr::null_mut(),
    };
    named(&name).unwrap_or(std::ptr::null_mut())
}

/// `-performAsCurrentDrawingAppearance:`.
///
/// **The block runs; no appearance is made current, because there is nothing
/// here to make one current.** On a Mac this pushes `appearance` as the drawing
/// appearance for the duration of the block, so that `-effectiveAppearance` and
/// the drawing colours inside it follow that appearance. This host has no
/// appearance server and `-effectiveAppearance` keeps answering Aqua whatever
/// is passed, so the only half of the method that can be truthful is the half
/// that runs the caller's work - and that half is also the half that matters:
/// the block is the caller's whole reason for the call.
///
/// Not optional alongside `appearanceNamed:`. Both of the guest's sites send
/// this selector into the answer to `+appearanceNamed:` one instruction later,
/// so leaving it unanswered would abort the same run a correct
/// `appearanceNamed:` was supposed to save, and answering `appearanceNamed:`
/// with nil would skip the block with nothing said.
///
/// The block is not copied, because it is run before this returns and so never
/// escapes.
///
/// **This takes exactly one argument, and the encoding below is what says so.**
/// It used to declare four: `(_receiver, _sel, _appearance, block)` under
/// `v32@0:8@16@?24`. That is self-consistent and completely wrong, because it
/// invents an object parameter the selector does not have. `objc_msgSend` puts
/// the block in `rdx` and leaves `rcx` holding whatever the previous call left
/// there, so this read its "block" out of `rcx` — a stale pointer — and called
/// it. In the guest that pointer came from the release build's allocator and
/// the process died executing the heap (`SEGV_ACCERR` on a *fetch*, at a pc
/// inside `[anon:mimalloc]`). Nothing about the call looked wrong: the method
/// ran, and it ran *a* block, and the guest could not tell until the pointer it
/// happened to pick was not code.
///
/// Both halves of the correction are measured, not recalled:
/// * the call site at `Framework+0x3ab5280` sets only `rdi`/`rsi`/`rdx` and
///   never writes `rcx`;
/// * `v24@0:8@?16` is the **only** `v<frame>@0:8@<one arg>` encoding string in
///   the Framework, and it is the guest's own declaration of this selector —
///   void, 24-byte frame, self at 0, `_cmd` at 8, block at 16.
unsafe extern "C" fn perform_as_current_drawing_appearance(
    _receiver: Id,
    _sel: Sel,
    block: Id,
) {
    if let Some(f) = unsafe { crate::animation::invoke(block) } {
        // SAFETY: `void (^)(void)`, which is what the selector declares, and
        // `invoke` is the block's own entry point.
        unsafe {
            let f: unsafe extern "C" fn(Id) = std::mem::transmute(f);
            f(block);
        }
    }
}

/// `-bestMatchFromAppearancesWithNames:`, the selector the milestone died on.
///
/// **The question is answerable on this host, and the answer is not nil.** The
/// three call sites (`Framework+0x8102cb3`, `+0x8103492`, `+0x81035d4`) all ask
/// the same question in the same shape, and the re-disassembly of all three says
/// what it is:
///
/// ```text
/// 8102c4d  [NSApp effectiveAppearance]                     -> r15
/// 8102c68  mov [rbp-0x60],rax ; element 0 = _NSAppearanceNameAqua     (got 0xdd50508)
/// 8102c76  mov [rbp-0x70],rax ; _NSAppearanceNameDarkAqua, kept for the compare below
/// 8102c7a  mov [rbp-0x58],rax ; element 1 = _NSAppearanceNameDarkAqua (got 0xdd50500)
/// 8102c7e  mov rdi,[0xe6e71b8]  ; _OBJC_CLASS_$_NSArray, bound by name
/// 8102c85  mov rsi,[0xe6e7df8]  ; arrayWithObjects:count:
/// 8102c8c  lea rdx,[rbp-0x60]   ; the two names, contiguously
/// 8102c90  mov ecx,0x2
/// 8102c95  call objc_msgSend                           -> r14, the array
/// 8102ca6  mov rsi,[0xe6eba18]  ; bestMatchFromAppearancesWithNames:
/// 8102cb3  call objc_msgSend                           <-- the fault
/// 8102cbc  _objc_retainAutoreleasedReturnValue         -> r13, the answer
/// 8102cd7  mov rsi,[0xe6e85c8]  ; isEqual:
/// 8102ce1  mov rdx,[rbp-0x70]   ; the DarkAqua *name string*, not an appearance
/// 8102ce5  call objc_msgSend                           -> r14d
/// 8102cf9  test r14b,r14b
/// 8102cfc  setne BYTE PTR [rbx+0x69]                  ; not-dark
/// 8102d00  sete  al
/// 8102d03  mov  DWORD PTR [rbx+0x7c],eax             ; dark
/// ```
///
/// So the argument is a list of **names**, and the answer is retained and
/// immediately compared, with `isEqual:`, **against the DarkAqua name string**
/// - not against an appearance. The two derived bytes are the pair
/// `not-dark` (`+0x69`) and `dark` (`+0x7c`), and they are the guest's *only*
/// dark-mode bit.
///
/// **Where the two bytes are used, which is why they matter.** The record they
/// land in is a function-local static at `__DATA,__common` `0xe9301a0`, built
/// by the initialiser at `Framework+0x8102ba0` behind the `__cxa_guard` byte at
/// `0xe930258` (the accessor's own fast path is `Framework+0x586d89a`, and its
/// cold path at `0x586d8f8` is `__cxa_guard_acquire` -> `0x8102ba0` ->
/// `__cxa_guard_release`). The record holds six flags, and its three sites write
/// them like this:
///
/// | site | where | writes |
/// |---|---|---|
/// | `+0x8102cb3` | the initialiser | record `+0x69` not-dark, `+0x7c` dark |
/// | `+0x8103492` | the block body at `0x81033e0`, i.e. the `-initWithHandler:` handler of the guest's own `NativeThemeEffectiveAppearanceObserver` | the same pair, on the object it notifies |
/// | `+0x81035d4` | a member function, `this` in `[rbp-0x48]` | the **globals** `0xe9302c9` (not-dark, `0x8103614`) and `0xe9302dc` (dark, `0x8103627`) |
///
/// The globals are then copied into a per-object cache at `Framework+0x39b9e37`
/// (`[rbx+0xff8]`, `[rbx+0xff9]`, `[rbx+0xffc]`, `[rbx+0x1000]`, `[rbx+0x1004]`,
/// `[rbx+0x1005]`), and `Framework+0x39f6780` diffs the six incoming fields
/// `+0x69, +0x70, +0x7c, +0x80, +0x78, +0x79` against exactly those six cached
/// fields and calls `+0x39c46c0` when any of them moved. So these two bytes are
/// a *change notification* input for the renderer's theme, and a wrong bit is a
/// wrong theme for the whole process, silently.
///
/// **Why nil would be wrong rather than merely survivable.** Nothing tests the
/// result for nil: at `+0x8102cb3` the retain at `0x8102cbc` is followed
/// straight by the two `objc_release`s and the `isEqual:` send, with no test of
/// `r13` anywhere between, and the same holds at the other two - the census's
/// own nil-test column reads `None` for all three load sites of the `0xe6eba18`
/// selref. So nil does not crash: each site retains it and messages it, and this
/// runtime answers message-to-nil. The two bytes would then come out of
/// `[nil isEqual:NSAppearanceNameDarkAqua]`, i.e. out of *our* nil handling
/// rather than out of any appearance matching - and they would be the right bytes
/// **by accident**. The accident is visible in the guest's own list: element 0
/// is `_NSAppearanceNameAqua`, and `-[NSApplication effectiveAppearance]`
/// (`crates/appkit/src/application.rs:166`) answers [`aqua`], whose `-name` is
/// exactly that string ([`name`], below). So *a name in the list does match the
/// appearance in effect*, Apple's own answer here is a non-nil match, and nil
/// would be asserting "nothing in the list matched" - which is false on this
/// host. A nil that produces the right byte by the wrong route is the shape of
/// wrong answer this project exists to prevent, so it is not what is installed.
///
/// **What is installed instead, and why it invents nothing.** The receiver *is*
/// the appearance in effect, and [`name`] already answers what it is called, so
/// the whole of Apple's method reduces here to a lookup of that one name in the
/// caller's list: **return the receiver when its own name is in the list, and
/// nil when no name in the list is.** That needs no appearance-matching
/// service, no second appearance and no stand-in - the object handed back is the
/// very object `-effectiveAppearance` just handed the guest, and it matches
/// because it is what is in effect, not because it was built to match. The `nil`
/// branch is load-bearing rather than defensive: with a list of
/// `NSAppearanceNameDarkAqua` alone the answer is nil, because the appearance in
/// effect is Aqua and DarkAqua is not in the list.
///
/// **What this does not settle.** Whether Apple's real
/// `bestMatchFromAppearancesWithNames:` returns an `NSAppearance *` (what the
/// declaration says, and what is returned here) or an `NSAppearanceName` cannot
/// be read off this binary, because the guest's *only* use of the answer is the
/// single `isEqual:` above and both readings give the same byte for an Aqua
/// system. Type-correctness is the tiebreaker, and it is why this returns an
/// appearance rather than the name string that would also satisfy `isEqual:`.
/// `-[NSObject isEqual:]` is pointer identity (`crates/objc/src/nsobject.rs`),
/// so an appearance is not equal to a name, and the bytes the guest stores are
/// `not-dark = 1, dark = 0` - which is the truth on this host.
unsafe extern "C" fn best_match_from_appearances_with_names(
    receiver: Id,
    _sel: Sel,
    names: Id,
) -> Id {
    if receiver.is_null() || names.is_null() {
        return std::ptr::null_mut();
    }
    let wanted = unsafe { foundation::string::bytes(name(receiver, objc::sel::get("name"))) };
    for candidate in elements_of(names) {
        // SAFETY: an `NSAppearanceName` is an `NSString`, which is what the
        // selector's argument declares, and the array's own `-count` bounds the
        // loop.
        if unsafe { foundation::string::bytes(candidate) } == wanted {
            return receiver;
        }
    }
    std::ptr::null_mut()
}

/// The elements of an `NSArray`, read with the guest's own `-count` and
/// `-objectAtIndex:`.
///
/// Sent rather than read out of this runtime's storage, because the array is
/// the **caller's** and may be a subclass whose layout is not ours to
/// interpret. `accessibility.rs` reads a guest array the same way. An array
/// that cannot answer both selectors is an empty list, which is what makes the
/// match above nil rather than a guess.
fn elements_of(array: Id) -> Vec<Id> {
    // SAFETY: `respondsToSelector:` is `BOOL (*)(id, SEL)` and the array is
    // the caller's, alive across the call.
    let answers = |name: &str| unsafe {
        objc::shorthand::objc_opt_respondsToSelector(array, objc::sel::get(name))
    };
    if !answers("count") || !answers("objectAtIndex:") {
        return Vec::new();
    }
    let count_sel = objc::sel::get("count");
    let at_sel = objc::sel::get("objectAtIndex:");
    // SAFETY: both sends are guarded just above, and `-count` is what the array
    // itself reports, so the loop cannot read past its end.
    unsafe {
        let count_fn: unsafe extern "C" fn(Id, Sel) -> usize =
            std::mem::transmute(objc::msg::entry());
        let at_fn: unsafe extern "C" fn(Id, Sel, usize) -> Id =
            std::mem::transmute(objc::msg::entry());
        let count = count_fn(array, count_sel);
        let mut out = Vec::with_capacity(count.min(4096));
        for index in 0..count {
            out.push(at_fn(array, at_sel, index));
        }
        out
    }
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSAppearance").expect("declared before its methods");
    class_methods!(class, {
        "appearanceNamed:" => appearance_named, c"@24@0:8@16";
    });
    methods!(class, {
        "name" => name, c"@16@0:8";
        "bestMatchFromAppearancesWithNames:" =>
            best_match_from_appearances_with_names, c"@24@0:8@16";
        "performAsCurrentDrawingAppearance:" =>
            perform_as_current_drawing_appearance, c"v24@0:8@?16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `objc_msgSend` a test uses, so a send reaches its method by name.
    fn send_named(name: &str) -> Id {
        // SAFETY: `+appearanceNamed:` is `id (*)(Class, SEL, NSAppearanceName)`.
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let class = objc::class::ptr(objc::class::lookup(c"NSAppearance").expect("declared"));
        let requested = foundation::string::from_str(name) as Id;
        // SAFETY: the class and the argument are as the selector declares.
        let answer = unsafe { f(class as Id, objc::sel::get("appearanceNamed:"), requested) };
        unsafe { corefoundation::api::CFRelease(requested.cast()) };
        answer
    }

    fn send_name(receiver: Id) -> Vec<u8> {
        // SAFETY: `-name` is `id (*)(id, SEL)`.
        let f: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        // SAFETY: the receiver is one this module made.
        let answer = unsafe { f(receiver, objc::sel::get("name")) };
        // SAFETY: an `NSString` the method returned.
        unsafe { foundation::string::bytes(answer) }
    }

    /// An autoreleased `NSArray` of interned name strings, which is the shape
    /// of the array the guest builds at `Framework+0x8102c7e`.
    ///
    /// This is *the same array the guest gets*: `+[NSArray arrayWithObjects:count:]`
    /// is `CFAutorelease(new_array(...))` in `foundation/src/collections.rs:800`,
    /// so the guest's array and this one are made by one line of code, and
    /// `elements_of` reads whichever it is handed through the guest's own
    /// `-count` and `-objectAtIndex:` rather than this runtime's storage.
    fn array(names: &[&'static str]) -> Id {
        let values: Vec<corefoundation::base::CFTypeRef> = names
            .iter()
            .map(|n| foundation::notification::name_object(n) as corefoundation::base::CFTypeRef)
            .collect();
        let array = corefoundation::collections::new_array(
            &values,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        );
        // SAFETY: a fresh array this test owns, handed on with the +1 a pool
        // takes - the same one `+arrayWithObjects:count:` returns.
        unsafe { corefoundation::api::CFAutorelease(array) as Id }
    }

    /// The answer, through `objc_msgSend` so it reaches the method by name and
    /// with the one register the guest puts the list in.
    fn best_match(receiver: Id, names: Id) -> Id {
        // SAFETY: `bestMatchFromAppearancesWithNames:` is
        // `id (*)(id, SEL, NSArray *)` - one colon, one argument.
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        // SAFETY: the receiver is one this module made and `names` is the array
        // `array` made.
        unsafe {
            f(
                receiver,
                objc::sel::get("bestMatchFromAppearancesWithNames:"),
                names,
            )
        }
    }

    #[test]
    fn aqua_is_a_stable_named_appearance() {
        crate::bootstrap();
        let appearance = aqua();
        assert_eq!(appearance, aqua());
        assert_eq!(
            unsafe { foundation::string::bytes(name(appearance, objc::sel::get("name"))) },
            b"NSAppearanceNameAqua"
        );
    }

    /// The two names the guest sends are two names, and they are two objects:
    /// `Framework+0x8142484` picks between them on one flag.
    #[test]
    fn the_two_names_the_guest_sends_resolve_to_different_appearances() {
        crate::bootstrap();
        let aqua_appearance = send_named(AQUA);
        let dark = send_named(DARK_AQUA);
        assert!(!aqua_appearance.is_null());
        assert!(!dark.is_null());
        assert_ne!(aqua_appearance, dark);
        assert_eq!(aqua_appearance, aqua());
        assert_eq!(dark, dark_aqua());
        assert_eq!(send_name(aqua_appearance), AQUA.as_bytes());
        assert_eq!(send_name(dark), DARK_AQUA.as_bytes());
    }

    /// Anything else is nil, which is what AppKit does for a name it does not
    /// have, and is a different answer from a refusal.
    #[test]
    fn a_name_this_host_has_no_appearance_for_is_nil() {
        crate::bootstrap();
        assert!(send_named("NSAppearanceNameVibrantDark").is_null());
        assert!(send_named("").is_null());
        // SAFETY: `+appearanceNamed:` on a nil name is the nil path.
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let class = objc::class::ptr(objc::class::lookup(c"NSAppearance").expect("declared"));
        assert!(
            unsafe {
                f(
                    class as Id,
                    objc::sel::get("appearanceNamed:"),
                    std::ptr::null_mut(),
                )
            }
            .is_null()
        );
    }

    /// The two names the guest builds the array from, as the guest builds it,
    /// and the answer the match gives for each of the three receivers the three
    /// call sites use.
    ///
    /// This is the whole decision, in one place: the array is
    /// `@[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]` exactly as
    /// `Framework+0x8102c7e` builds it, and the answer is the receiver when the
    /// receiver's own name is in the list. So the appearance the guest has in
    /// effect (Aqua) matches, and the two bytes the guest derives are
    /// `not-dark = 1, dark = 0` - produced by the match rather than by
    /// message-to-nil.
    #[test]
    fn the_match_is_the_receiver_when_its_own_name_is_in_the_list() {
        crate::bootstrap();
        let names = array(&[AQUA, DARK_AQUA]);
        assert_eq!(best_match(aqua(), names), aqua());
        assert_eq!(best_match(dark_aqua(), names), dark_aqua());
        // And what the guest then does with it: an appearance is not equal to
        // a name, so the DarkAqua comparison is false and Aqua is "not dark".
        let equal: unsafe extern "C" fn(Id, Sel, Id) -> bool =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let dark_name = foundation::notification::name_object(DARK_AQUA) as Id;
        // SAFETY: both receivers are objects and the selector takes one.
        let is_dark = unsafe {
            equal(
                best_match(aqua(), names),
                objc::sel::get("isEqual:"),
                dark_name,
            )
        };
        assert!(!is_dark, "Aqua in effect is not the DarkAqua name");
    }

    /// **The nil branch is a decision, not a fallback.** A list that does not
    /// contain the appearance in effect matches nothing on this host, and the
    /// answer is nil - which is what makes the non-nil answer above a match
    /// rather than a constant. It is also the answer that produces the *same
    /// two bytes* the guest would have got from nil, which is the whole reason
    /// this selector had to be answered rather than left to message-to-nil.
    #[test]
    fn a_list_without_the_appearance_in_effect_matches_nothing() {
        crate::bootstrap();
        let dark_only = array(&[DARK_AQUA]);
        assert!(best_match(aqua(), dark_only).is_null());
        let aqua_only = array(&[AQUA]);
        assert_eq!(best_match(aqua(), aqua_only), aqua());
        assert!(best_match(dark_aqua(), aqua_only).is_null());
        assert!(best_match(aqua(), array(&[])).is_null());
        // A nil list, and a nil receiver, are the language's nil path.
        assert!(best_match(aqua(), std::ptr::null_mut()).is_null());
        assert!(best_match(std::ptr::null_mut(), dark_only).is_null());
    }

    /// The two exported name symbols the guest loads out of its `__got`, and the
    /// shape it loads them in: the symbol is the address of a *word*, and the
    /// word holds the `NSString *`. `_NSAppearanceNameDarkAqua` was an unbound
    /// data import, which is what had `appearanceNamed:` asking about nothing.
    #[test]
    fn the_exported_name_symbols_are_cells_holding_the_name() {
        crate::bootstrap();
        let expected = [
            (DARK_AQUA, "_NSAppearanceNameDarkAqua"),
            (AQUA, "_NSAppearanceNameAqua"),
        ];
        let cells = name_cells();
        assert_eq!(cells.len(), expected.len());
        for (text, symbol) in expected {
            let (_, cell) = cells
                .iter()
                .find(|(name, _)| name == symbol)
                .unwrap_or_else(|| panic!("{symbol} is published"));
            // SAFETY: the published address is a `Box::leak`ed word, live for
            // the life of the process, exactly as a framework's `__DATA` word is.
            let word = unsafe { *(*cell as *const Id) };
            assert!(!word.is_null(), "{symbol} holds nothing");
            // SAFETY: the word holds an interned `NSString` this runtime made.
            assert_eq!(unsafe { foundation::string::bytes(word) }, text.as_bytes());
        }
    }

    /// A `void (^)(void)` in the shape one of these tests can hand over.
    #[repr(C)]
    struct TestBlock {
        isa: *const std::ffi::c_void,
        flags: i32,
        reserved: i32,
        invoke: *const std::ffi::c_void,
        descriptor: *const (usize, usize),
    }

    static DESCRIPTOR: (usize, usize) = (0, 24);

    #[test]
    fn the_block_runs_and_a_missing_one_does_not_crash() {
        crate::bootstrap();
        static RAN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

        unsafe extern "C" fn body(_block: Id) {
            RAN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }

        let isa =
            objc::class::ptr(objc::class::lookup(c"__NSGlobalBlock__").expect("a block class"));
        let block = TestBlock {
            isa: isa as *const std::ffi::c_void,
            // `BLOCK_IS_GLOBAL`, which is what makes a copy a no-op.
            flags: 1 << 28,
            reserved: 0,
            invoke: body as *const std::ffi::c_void,
            descriptor: &DESCRIPTOR,
        };
        let block = &block as *const TestBlock as Id;

        // SAFETY: `-performAsCurrentDrawingAppearance:` is
        // `void (^)(void)` taking **one** argument, the block, and `block` is a
        // block literal whose `invoke` is `body`.
        //
        // This used to call it as `(id, SEL, id, id)` with the appearance in
        // `rdx` and the block in `rcx`, which agreed with the wrong four-
        // argument signature and so passed while the guest's own call — which
        // never writes `rcx` — reached a stale register. **A test that shares
        // its author's misreading of a signature cannot catch the
        // misreading.** This one now sends the same three registers the guest
        // sends, so a future arity mistake fails here rather than in the guest.
        let run: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let sel = objc::sel::get("performAsCurrentDrawingAppearance:");
        // SAFETY: as above.
        unsafe {
            run(aqua(), sel, block);
            // And a nil block is the language's no-op, not a crash.
            run(aqua(), sel, std::ptr::null_mut());
        }
        assert_eq!(RAN.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
