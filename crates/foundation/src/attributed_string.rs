use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};
use crate::geometry::{NSRect, NSSize};

fn class() -> &'static objc::class::Class {
    objc::class::lookup(c"NSAttributedString").expect("NSAttributedString is registered")
}

unsafe extern "C" fn init_with_string(receiver: Id, _sel: Sel, value: Id) -> Id {
    // SAFETY: `value` is null or an NSString supplied by the caller.
    let text = String::from_utf8_lossy(&unsafe { crate::string::bytes(value) }).into_owned();
    // SAFETY: this class is registered with `String` boxed storage.
    unsafe { crate::boxed::fill(receiver, text) };
    receiver
}

/// `-[NSAttributedString initWithString:attributes:]`.
///
/// The immutable facade currently stores text only. Accepting the attribute
/// dictionary still preserves the string for callers that measure it.
unsafe extern "C" fn init_with_string_attributes(
    receiver: Id,
    sel: Sel,
    value: Id,
    _attributes: Id,
) -> Id {
    // SAFETY: identical text argument and receiver contract to
    // `-initWithString:`.
    unsafe { init_with_string(receiver, sel, value) }
}

unsafe extern "C" fn attributed_string_with_string(_receiver: Id, _sel: Sel, value: Id) -> Id {
    // SAFETY: `value` is null or an NSString supplied by the caller.
    let text = String::from_utf8_lossy(&unsafe { crate::string::bytes(value) }).into_owned();
    let object = crate::boxed::make(class(), text);
    // SAFETY: a fresh object owned by this method.
    unsafe { corefoundation::api::CFAutorelease(object as corefoundation::base::CFTypeRef) as Id }
}

unsafe extern "C" fn string(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an instance of the class registered below.
    let text = unsafe { crate::boxed::contents::<String>(receiver, class()) }
        .map_or("", |text| text.as_str());
    objc::arc::autorelease(crate::string::from_str(text))
}

/// `-[NSAttributedString size]`.
///
/// Foundation cannot shape AppKit fonts, but callers still need a non-empty
/// canvas for the common draw-into-an-image path. This conservative estimate
/// keeps one line of text visible to that path until AppKit supplies glyph
/// metrics.
unsafe extern "C" fn size(receiver: Id, _sel: Sel) -> NSSize {
    // SAFETY: an instance of the class registered below.
    let count = unsafe { crate::boxed::contents::<String>(receiver, class()) }
        .map_or(0, |text| text.chars().count());
    NSSize {
        width: count as f64 * 8.0,
        height: if count == 0 { 0.0 } else { 16.0 },
    }
}

/// `-[NSAttributedString drawInRect:]`.
///
/// The focused-image machinery accepts the draw even though Foundation does
/// not itself rasterize AppKit glyphs.
unsafe extern "C" fn draw_in_rect(_receiver: Id, _sel: Sel, _rect: NSRect) {}

pub(crate) fn make(text: &str) -> Id {
    let object = crate::boxed::make(class(), text.to_owned());
    // SAFETY: a fresh object owned by this function.
    unsafe { corefoundation::api::CFAutorelease(object as corefoundation::base::CFTypeRef) as Id }
}

/// The mutable half of the pair, which the guest allocates rather than receives.
///
/// Recorded at `install` rather than looked up, because a lookup that fails here
/// would panic - and an `IMP` is an `extern "C"` function, so a panic cannot
/// unwind out of one and the process aborts inside the guest's own call.
static MUTABLE_CLASS: std::sync::OnceLock<&'static objc::class::Class> = std::sync::OnceLock::new();

fn mutable_class() -> &'static objc::class::Class {
    *MUTABLE_CLASS.get_or_init(|| {
        objc::class::lookup(c"NSMutableAttributedString")
            .expect("install() registers NSMutableAttributedString before any message reaches it")
    })
}

/// `+[NSMutableAttributedString alloc]`'s `-init`: an empty mutable string.
unsafe extern "C" fn mutable_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: this class is registered with `String` boxed storage.
    unsafe { crate::boxed::fill(receiver, String::new()) };
    receiver
}

/// `-[NSMutableAttributedString initWithString:]`.
unsafe extern "C" fn mutable_init_with_string(receiver: Id, _sel: Sel, value: Id) -> Id {
    // SAFETY: `value` is null or an NSString supplied by the caller.
    let text = String::from_utf8_lossy(&unsafe { crate::string::bytes(value) }).into_owned();
    // SAFETY: this class is registered with `String` boxed storage.
    unsafe { crate::boxed::fill(receiver, text) };
    receiver
}

/// `-[NSMutableAttributedString initWithString:attributes:]`.
///
/// The attributes are dropped for the same reason the immutable half drops them:
/// nothing in this tree shapes text, and the string is what every reader of a
/// menu title or a bubble message actually wants. Saying so is better than
/// storing a dictionary nothing consults.
unsafe extern "C" fn mutable_init_with_string_attributes(
    receiver: Id,
    sel: Sel,
    value: Id,
    _attributes: Id,
) -> Id {
    // SAFETY: identical text argument and receiver contract to
    // `-initWithString:`.
    unsafe { mutable_init_with_string(receiver, sel, value) }
}

/// `-appendString:`, the one mutation the guest's four `initWithString:` sites
/// are followed by: a title is built and then added to.
unsafe extern "C" fn mutable_append_string(receiver: Id, _sel: Sel, value: Id) {
    // SAFETY: this class is registered with `String` boxed storage.
    let addition = String::from_utf8_lossy(&unsafe { crate::string::bytes(value) }).into_owned();
    if let Some(text) = unsafe { crate::boxed::contents::<String>(receiver, mutable_class()) } {
        text.push_str(&addition);
    }
}

/// `-length`, in UTF-16 code units, which is what `NSString` counts and what
/// every range-taking selector on this class is measured in.
unsafe extern "C" fn mutable_length(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: this class is registered with `String` boxed storage.
    unsafe { crate::boxed::contents::<String>(receiver, mutable_class()) }
        .map_or(0, |text| text.encode_utf16().count())
}

/// `-deleteCharactersInRange:`, which the guest's
/// `AXPlatformNodeCocoa AXAttributedStringForRange:` path needs to hand back a
/// substring as an attributed string.
unsafe extern "C" fn mutable_delete_range(receiver: Id, _sel: Sel, location: usize, length: usize) {
    // SAFETY: this class is registered with `String` boxed storage.
    let Some(text) = (unsafe { crate::boxed::contents::<String>(receiver, mutable_class()) }) else {
        return;
    };
    // UTF-16 offsets, clamped the way a range out of bounds is: an empty string
    // rather than a panic, because an IMP cannot unwind and the alternative is
    // a fault inside the guest's own call.
    let units: Vec<u16> = text.encode_utf16().collect();
    let start = location.min(units.len());
    let end = location.saturating_add(length).min(units.len());
    if start >= end {
        return;
    }
    let kept = String::from_utf16_lossy(&units[..start]);
    let kept = format!("{kept}{}", String::from_utf16_lossy(&units[end..]));
    *text = kept;
}

pub(crate) fn install() {
    let class = crate::boxed::register::<String>(c"NSAttributedString", None);
    methods!(class, {
        "initWithString:" => init_with_string, c"@24@0:8@16";
        "initWithString:attributes:" => init_with_string_attributes, c"@32@0:8@16@24";
        "string" => string, c"@16@0:8";
        "size" => size, c"{CGSize=dd}16@0:8";
        "drawInRect:" => draw_in_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
    });
    class_methods!(class, {
        "attributedStringWithString:" => attributed_string_with_string, c"@24@0:8@16";
    });

    // The mutable half. `NSAttributedString` is the superclass, so `-string`,
    // `-size` and `-drawInRect:` are inherited and read the same storage -
    // which is the point: a guest that appends and then asks for `-string` has
    // to get its own edit back, not the immutable half's original.
    //
    // Four of the seven call sites are `[[NSMutableAttributedString alloc]
    // initWithString:]` and one is `[[NSMutableAttributedString alloc] init]`,
    // all read off the disassembly of the Framework: `mov rdi,
    // [rip+_OBJC_CLASS_$_NSMutableAttributedString]; call _objc_alloc; mov rsi,
    // [rip@"initWithString:"]; call objc_msgSend` at `Framework+0x1c62986`,
    // `+0x80453d6`, `+0x80456bb`, `+0x81527ae` and `+0xbc96aff`, and
    // `... call _objc_alloc_init` at `+0x98e137e`.
    //
    // Two of the owners of those sites are on the start-up path rather than in a
    // leaf feature: `-[MenuControllerCocoaDelegateImpl
    // controllerWillAddItem:fromModel:atIndex:]`
    // (`chrome/browser/ui/cocoa/menu_controller_cocoa.mm`) is how every
    // main-menu item's title is built, and the main menu is constructed while
    // the browser starts; `-[ConfirmQuitFrameView setMessageText:]` is the quit
    // bubble. The other three are `-[AXPlatformNodeCocoa
    // AXAttributedStringForRange:-]`, `-AXAttributedStringForTextMarkerRange:-`
    // and a `+[NSDistributedNotificationCenter
    // postNotificationName:object:]` name constant.
    let mutable = crate::boxed::register::<String>(c"NSMutableAttributedString", Some(class));
    MUTABLE_CLASS.set(mutable).ok();
    methods!(mutable, {
        "init" => mutable_init, c"@16@0:8";
        "initWithString:" => mutable_init_with_string, c"@24@0:8@16";
        "initWithString:attributes:" => mutable_init_with_string_attributes, c"@32@0:8@16@24";
        "appendString:" => mutable_append_string, c"v24@0:8@16";
        "length" => mutable_length, c"Q16@0:8";
        // **One colon, so exactly one argument.** This read `v32@0:8Q16Q24`,
        // which declares a *second* `NSUInteger` — and a selector's colon
        // count is a property of its name, so no self-consistency check can
        // argue with that. It is the shape of the
        // `performAsCurrentDrawingAppearance:` crash, one step from live: the
        // IMP was already right (four words, which is what a 16-byte `NSRange`
        // occupies in two registers) and only the string was wrong, so a test
        // that shared the misreading passed anyway.
        //
        // The Framework states the correct shape **17 times** for a
        // one-argument `NSRange` selector — on `setMarkedRange:` (protocol
        // `NSTextInputClient`), `setAccessibilitySelectedTextRange:`,
        // `setAccessibilityColumnIndexRange:` and 14 more. Frame 32 = 16+16.
        // Found by `test/tools/objcenc.py diff`; the guest does not currently
        // send this selector, which is the only reason it is latent and not
        // live.
        "deleteCharactersInRange:" => mutable_delete_range, c"v32@0:8{_NSRange=QQ}16";
        // `-beginEditing` and `-endEditing` bracket a batch of mutations that
        // this class applies immediately, so both are the identity. The guest
        // brackets `AXPlatformNodeCocoa`'s attributed-string build with them.
        "beginEditing" => begin_end_editing, c"v16@0:8";
        "endEditing" => begin_end_editing, c"v16@0:8";
    });
}

/// `-beginEditing` / `-endEditing`: the identity, because every mutation here
/// takes effect at once.
unsafe extern "C" fn begin_end_editing(_receiver: Id, _sel: Sel) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn class_named(name: &std::ffi::CStr) -> Id {
        crate::bootstrap();
        objc::class::ptr(objc::class::lookup(name).expect("registered")) as Id
    }

    /// The guest's own shape, `[[NSMutableAttributedString alloc]
    /// initWithString:]`, and the read-back through the *inherited* `-string`.
    ///
    /// The read-back is the test. A mutable class that stored its text anywhere
    /// the immutable `-string` cannot see would answer the original string
    /// forever, which is a plausible-looking wrong answer: every write would
    /// succeed and no reader would ever see it.
    #[test]
    fn a_guest_built_mutable_attributed_string_reads_back_through_the_inherited_string() {
        crate::bootstrap();
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            let send0: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            let send1: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
            let text: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            let len: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());

            let s = send1(
                send0(class_named(c"NSMutableAttributedString"), objc::sel::get("alloc")),
                objc::sel::get("initWithString:"),
                crate::string::from_str("File"),
            );
            assert!(!s.is_null());
            assert_eq!(
                crate::string::bytes(text(s, objc::sel::get("string"))),
                b"File".to_vec()
            );
            assert_eq!(len(s, objc::sel::get("length")), 4);

            send1(s, objc::sel::get("appendString:"), crate::string::from_str(" > New Tab"));
            assert_eq!(
                crate::string::bytes(text(s, objc::sel::get("string"))),
                b"File > New Tab".to_vec(),
                "the append did not reach the inherited -string"
            );
            assert_eq!(len(s, objc::sel::get("length")), 14);
        }
    }

    /// `[[NSMutableAttributedString alloc] init]` and
    /// `-initWithString:attributes:`, the other two shapes in the image.
    #[test]
    fn an_empty_mutable_attributed_string_and_one_with_attributes_both_hold_their_text() {
        crate::bootstrap();
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            let send0: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            let send2: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            let text: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            let len: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());
            let klass = class_named(c"NSMutableAttributedString");

            let empty = send0(send0(klass, objc::sel::get("alloc")), objc::sel::get("init"));
            assert!(!empty.is_null());
            assert_eq!(len(empty, objc::sel::get("length")), 0);

            let dictionary = send0(
                class_named(c"NSMutableDictionary"),
                objc::sel::get("dictionary"),
            );
            let with = send2(
                send0(klass, objc::sel::get("alloc")),
                objc::sel::get("initWithString:attributes:"),
                crate::string::from_str("Quit Chromium?"),
                dictionary,
            );
            assert_eq!(
                crate::string::bytes(text(with, objc::sel::get("string"))),
                b"Quit Chromium?".to_vec()
            );
        }
    }

    /// `-deleteCharactersInRange:` in UTF-16 units, and `-beginEditing` /
    /// `-endEditing` around it, which is how `AXPlatformNodeCocoa` brackets the
    /// build.
    ///
    /// The astral case is the one that matters: `NSAttributedString` measures in
    /// UTF-16 code units, so a range computed on characters is wrong for
    /// anything outside the BMP, and a byte- or char-indexed delete would cut a
    /// surrogate pair in half.
    #[test]
    fn a_delete_takes_a_utf16_range_and_the_editing_bracket_is_the_identity() {
        crate::bootstrap();
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            let send0: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            let send1: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
            let send2: unsafe extern "C" fn(Id, Sel, usize, usize) -> Id =
                std::mem::transmute(objc::msg::entry());
            let text: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            let len: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());

            let s = send1(
                send0(class_named(c"NSMutableAttributedString"), objc::sel::get("alloc")),
                objc::sel::get("initWithString:"),
                crate::string::from_str("a\u{1F600}b"),
            );
            // Four UTF-16 units: one for 'a', two for the emoji, one for 'b'.
            assert_eq!(len(s, objc::sel::get("length")), 4);
            send0(s, objc::sel::get("beginEditing"));
            send2(s, objc::sel::get("deleteCharactersInRange:"), 1, 2);
            send0(s, objc::sel::get("endEditing"));
            assert_eq!(
                crate::string::bytes(text(s, objc::sel::get("string"))),
                "ab".as_bytes().to_vec(),
                "the delete cut at the wrong place, or the bracket changed the result"
            );
        }
    }
}

