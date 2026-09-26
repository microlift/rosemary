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
unsafe extern "C" fn perform_as_current_drawing_appearance(
    _receiver: Id,
    _sel: Sel,
    _appearance: Id,
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

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSAppearance").expect("declared before its methods");
    class_methods!(class, {
        "appearanceNamed:" => appearance_named, c"@24@0:8@16";
    });
    methods!(class, {
        "name" => name, c"@16@0:8";
        "performAsCurrentDrawingAppearance:" =>
            perform_as_current_drawing_appearance, c"v32@0:8@16@?24";
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
        // `void (id, SEL, id, void (^)(void))`, and `block` is a block literal
        // whose `invoke` is `body`.
        let run: unsafe extern "C" fn(Id, Sel, Id, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let sel = objc::sel::get("performAsCurrentDrawingAppearance:");
        // SAFETY: as above.
        unsafe {
            run(aqua(), sel, send_named(DARK_AQUA), block);
            // And a nil block is the language's no-op, not a crash.
            run(aqua(), sel, send_named(DARK_AQUA), std::ptr::null_mut());
        }
        assert_eq!(RAN.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
