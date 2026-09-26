use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

static SHARED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// `+[NSFontManager sharedFontManager]`.
unsafe extern "C" fn shared_font_manager(class: Id, _sel: Sel) -> Id {
    *SHARED.get_or_init(|| {
        // SAFETY: the receiver is the NSFontManager class declared by AppKit.
        let manager = unsafe {
            objc::object::create_instance(
                objc::class::from_ptr(class as *const objc::class::Class).expect("a class"),
                0,
            )
        };
        // AppKit owns the process-wide singleton.
        objc::arc::retain(manager);
        manager as usize
    }) as Id
}

/// `-[NSFontManager localizedNameForFamily:face:]`.
///
/// The fallback family exposed by CoreText has no separate localized names.
unsafe extern "C" fn localized_name(_receiver: Id, _sel: Sel, family: Id, face: Id) -> Id {
    if family.is_null() { face } else { family }
}

/// `-[NSFontManager fontWithFamily:traits:weight:size:]`.
///
/// The named family is resolved by CoreText. Trait and legacy integer weight
/// selection can refine a family containing several faces; the current font
/// facade resolves the family's regular face, which is still preferable to
/// returning nil for UI text that can use it.
unsafe extern "C" fn font_with_family(
    _receiver: Id,
    _sel: Sel,
    family: Id,
    _traits: usize,
    _weight: isize,
    size: f64,
) -> Id {
    if family.is_null() {
        // SAFETY: the system-font constructor's declared arguments.
        return unsafe { system_font_of_size(std::ptr::null_mut(), _sel, size) };
    }
    // SAFETY: `family` is an NSString as required by the selector.
    let font = unsafe { font_with_name(std::ptr::null_mut(), _sel, family, size) };
    if font.is_null() {
        // A Mac-only family such as Helvetica is routinely requested by old
        // games. Use the platform's system face when fontconfig has no exact
        // family rather than making otherwise renderable UI text disappear.
        unsafe { system_font_of_size(std::ptr::null_mut(), _sel, size) }
    } else {
        font
    }
}

/// `+[NSFont fontWithName:size:]`.
///
/// `NSFont` is toll-free bridged to `CTFont`, so the font is the Core Text one.
unsafe extern "C" fn font_with_name(_class: Id, _sel: Sel, name: Id, size: f64) -> Id {
    // SAFETY: null or a string, as the selector requires.
    let font =
        unsafe { crate::coretext::CTFontCreateWithName(name as *const _, size, std::ptr::null()) };
    // SAFETY: a fresh object, or null.
    unsafe { autorelease(font) }
}

/// `+systemFontOfSize:`, and its bold twin, which is the same face here: the
/// fallback family is the only one there is to pick from.
unsafe extern "C" fn system_font_of_size(_class: Id, _sel: Sel, size: f64) -> Id {
    // SAFETY: `kCTFontUIFontSystem` with no language.
    let font = unsafe { crate::coretext::CTFontCreateUIFontForLanguage(2, size, std::ptr::null()) };
    // SAFETY: a fresh object, or null.
    unsafe { autorelease(font) }
}

unsafe fn autorelease(font: *const std::ffi::c_void) -> Id {
    if font.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { corefoundation::api::CFAutorelease(font) as Id }
}

/// `+[NSFont systemFontSize]`.
///
/// **Refused by name, because no source for the number was found - not because
/// the number is hard to compute.**
///
/// Apple's own documentation for the symbol is the whole of what it says, and
/// it names no value:
///
/// > Returns the size of the standard system font.
/// >
/// > The standard system font size in points.
///
/// (`NSFont/systemFontSize`, developer.apple.com, fetched while writing this.)
/// There is no value to copy, and on a current macOS there would not be a
/// constant to copy: the answer tracks the Accessibility "Text size" setting.
/// This host has no such setting, so it has no such number.
///
/// The guest's use of the answer is what makes a guess worse than a refusal
/// rather than merely no better. `Framework+[0x6949100,0x69491b0)` is a shared
/// accessor for `systemFontOfSize:`, `menuFontOfSize:` and `toolTipsFontOfSize:`,
/// and when its argument is 0 it asks this method for the default
/// (`Framework+0x694914b`, then `cvttsd2si` at `+0x6949151`, so that site wants
/// only an integer). `Framework+[0x694a470,0x694a500)` truncates the same way at
/// `+0x694a498`. But `Framework+0x8b6d9fd` passes the answer **straight back**
/// into `+[NSFont systemFontOfSize:]` with the full `CGFloat` still in `xmm0`,
/// so there the value becomes the point size of a real font. Nothing on that
/// path faults and nothing on it reports anything: a wrong number produces text
/// laid out at a size nobody chose, which is the outcome this project treats as
/// the worst one available.
///
/// Under `ROSETTA3_LOG_UNRECOGNIZED` it says so once and answers 0.0, so a
/// diagnostic run can walk past it to the rest of the queue - the same trade the
/// runtime's own unrecognized-selector path makes, and said in as many words,
/// because a 0-point font is not an answer.
unsafe extern "C" fn system_font_size(_class: Id, _sel: Sel) -> f64 {
    const WHY: &str = "+[NSFont systemFontSize]: Apple's documentation names no value for it \
         (it tracks the Accessibility \"Text size\" setting, and this host has no such \
         setting), and Framework+0x8b6d9fd feeds the answer straight into \
         +[NSFont systemFontOfSize:] as a point size - so any number here would be a wrong \
         font size that reported nothing";
    if objc::msg::logging_unrecognised() {
        static REPORTED: std::sync::Once = std::sync::Once::new();
        REPORTED.call_once(|| {
            eprintln!("rosetta3: appkit: {WHY}; answering 0.0 so the run can continue");
        });
        return 0.0;
    }
    objc::fail!(
        "{WHY}. Refusing is the answer; give this host a system font size deliberately \
         if it is to have one"
    );
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSFont").expect("declared before its methods");
    class_methods!(class, {
        "fontWithName:size:" => font_with_name, c"@32@0:8@16d24";
        "systemFontOfSize:" => system_font_of_size, c"@24@0:8d16";
        "boldSystemFontOfSize:" => system_font_of_size, c"@24@0:8d16";
        "systemFontSize" => system_font_size, c"d16@0:8";
    });

    let class = objc::class::lookup(c"NSFontManager").expect("declared before its methods");
    class_methods!(class, {
        "sharedFontManager" => shared_font_manager, c"@16@0:8";
    });
    methods!(class, {
        "localizedNameForFamily:face:" => localized_name, c"@32@0:8@16@24";
        "fontWithFamily:traits:weight:size:" => font_with_family, c"@48@0:8@16Q24q32d40";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_font_manager_is_stable() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSFontManager").unwrap();
        let class = objc::class::ptr(class).cast();
        // SAFETY: direct calls with the Objective-C signature installed above.
        let first = unsafe { shared_font_manager(class, objc::sel::get("sharedFontManager")) };
        let second = unsafe { shared_font_manager(class, objc::sel::get("sharedFontManager")) };
        assert!(!first.is_null());
        assert_eq!(first, second);
    }

    #[test]
    fn localized_font_name_uses_the_family() {
        crate::bootstrap();
        let family = foundation::string::from_str("Sans") as Id;
        let face = foundation::string::from_str("Regular") as Id;
        // SAFETY: direct call with the Objective-C signature installed above.
        let result = unsafe {
            localized_name(
                std::ptr::null_mut(),
                objc::sel::get("localizedNameForFamily:face:"),
                family,
                face,
            )
        };
        assert_eq!(result, family);
        unsafe {
            corefoundation::api::CFRelease(family.cast());
            corefoundation::api::CFRelease(face.cast());
        }
    }

    /// `+[NSFont systemFontSize]` refuses, so there is no value to assert. What
    /// *is* assertable, and is the point, is that the class **answers** the
    /// selector: a bound method that stops and names itself, not an
    /// unrecognized-selector abort and not a silent number.
    ///
    /// The distinction is the whole of it. Left unregistered, the guest's send
    /// lands on the runtime's unrecognized path, which under
    /// `ROSETTA3_LOG_UNRECOGNIZED` answers 0 in `xmm0` without a word - and
    /// `Framework+0x8b6d9fd` feeds that straight into a point size. A 0-point
    /// font is invisible text and says nothing, which is the failure this
    /// project is built to avoid.
    #[test]
    fn the_system_font_size_is_a_bound_named_refusal() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSFont").expect("NSFont");
        let method = class
            .metaclass()
            .method(objc::sel::get("systemFontSize"))
            .expect("+[NSFont systemFontSize] is answered");
        // Not the runtime's forwarding placeholder, which is what an
        // unimplemented selector resolves to.
        assert_ne!(
            method.implementation() as usize,
            objc::msg::forward() as usize
        );
        // And the guest reads the answer as a `CGFloat` out of `xmm0`, so the
        // encoding has to be the one that says so.
        // SAFETY: a method's type encoding is a NUL-terminated string the
        // runtime owns.
        let types = unsafe { std::ffi::CStr::from_ptr(method.types()) };
        assert_eq!(
            types.to_str().ok(),
            Some("d16@0:8"),
            "+[NSFont systemFontSize] takes self and _cmd and returns a CGFloat"
        );
    }
}
