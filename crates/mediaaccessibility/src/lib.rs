//! MediaAccessibility's caption appearance: the style a user picks in
//! Accessibility > Captions for subtitles a player draws itself.
//!
//! There is no Captions pane here for a user to pick a style in, so every
//! answer is the system default style, for both the user and the default
//! domain, and nothing ever posts
//! `kMACaptionAppearanceSettingsChangedNotification`.
//!
//! The functions and enums are `MACaptionAppearance.h`'s. Apple does not
//! publish the default style's values, so the ones below follow the WebVTT
//! defaults that a player falls back to without a style: white text, on black
//! at 80% opacity, in no window.
//! FIXME(ACCURACY): read the values from a macOS default profile once one is
//! available to compare against.

use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections;

/// `MACaptionAppearanceDomain`. Both domains answer the same here.
pub type Domain = isize;

/// `MACaptionAppearanceBehavior`.
pub mod behavior {
    /// `kMACaptionAppearanceBehaviorUseValue`: the user's value wins.
    pub const USE_VALUE: isize = 0;
    /// `kMACaptionAppearanceBehaviorUseContentIfAvailable`: the content's
    /// own style wins where it has one, which is what a style the user never
    /// changed allows.
    pub const USE_CONTENT_IF_AVAILABLE: isize = 1;
}

/// `MACaptionAppearanceTextEdgeStyle`.
pub mod text_edge {
    pub const UNDEFINED: isize = 0;
    pub const NONE: isize = 1;
    pub const RAISED: isize = 2;
    pub const DEPRESSED: isize = 3;
    pub const UNIFORM: isize = 4;
    pub const DROP_SHADOW: isize = 5;
}

/// `MACaptionAppearanceFontStyle`.
pub mod font_style {
    pub const DEFAULT: isize = 0;
    pub const MONOSPACED_WITH_SERIF: isize = 1;
    pub const PROPORTIONAL_WITH_SERIF: isize = 2;
    pub const MONOSPACED_WITHOUT_SERIF: isize = 3;
    pub const PROPORTIONAL_WITHOUT_SERIF: isize = 4;
    pub const CASUAL: isize = 5;
    pub const CURSIVE: isize = 6;
    pub const SMALL_CAPITAL: isize = 7;
}

/// `kMACaptionAppearanceSettingsChangedNotification`'s text.
pub const SETTINGS_CHANGED: &str = "com.apple.MediaAccessibility.captionAppearanceSettingsChanged";

/// The default style. FIXME(ACCURACY): see the module comment.
struct Style {
    foreground: [f64; 3],
    foreground_opacity: f64,
    background: [f64; 3],
    background_opacity: f64,
    window: [f64; 3],
    window_opacity: f64,
    window_corner_radius: f64,
    relative_size: f64,
    text_edge: isize,
}

const DEFAULT_STYLE: Style = Style {
    foreground: [1.0, 1.0, 1.0],
    foreground_opacity: 1.0,
    background: [0.0, 0.0, 0.0],
    background_opacity: 0.8,
    window: [0.0, 0.0, 0.0],
    window_opacity: 0.0,
    window_corner_radius: 0.0,
    relative_size: 1.0,
    text_edge: text_edge::NONE,
};

/// The family and PostScript name a font style stands for: the classic Mac
/// face of each kind. FIXME(ACCURACY): Apple's own choices are not published.
fn face_of(style: isize) -> (&'static str, &'static str) {
    match style {
        font_style::MONOSPACED_WITH_SERIF => ("Courier", "Courier"),
        font_style::PROPORTIONAL_WITH_SERIF => ("Times", "Times-Roman"),
        font_style::MONOSPACED_WITHOUT_SERIF => ("Menlo", "Menlo-Regular"),
        font_style::CASUAL => ("Marker Felt", "MarkerFelt-Thin"),
        font_style::CURSIVE => ("Snell Roundhand", "SnellRoundhand"),
        font_style::SMALL_CAPITAL => ("Copperplate", "Copperplate"),
        // The default and proportional sans serif, and any value the header
        // does not name.
        _ => ("Helvetica", "Helvetica"),
    }
}

/// Every getter's `behavior` out-parameter, which may be NULL.
///
/// # Safety
/// `behavior` is null or writable.
unsafe fn report(behavior: *mut isize) {
    if !behavior.is_null() {
        // SAFETY: the caller's.
        unsafe { *behavior = behavior::USE_CONTENT_IF_AVAILABLE };
    }
}

/// A new opaque color: the opacity is its own getter, and a caller combines
/// the two.
fn color(rgb: [f64; 3]) -> CFTypeRef {
    // SAFETY: plain values.
    unsafe { appkit::cgcontext::CGColorCreateGenericRGB(rgb[0], rgb[1], rgb[2], 1.0) }
}

/// `CGColorRef MACaptionAppearanceCopyForegroundColor(domain, behavior *)`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn MACaptionAppearanceCopyForegroundColor(
    _domain: Domain,
    behavior: *mut isize,
) -> CFTypeRef {
    unsafe { report(behavior) };
    color(DEFAULT_STYLE.foreground)
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn MACaptionAppearanceCopyBackgroundColor(
    _domain: Domain,
    behavior: *mut isize,
) -> CFTypeRef {
    unsafe { report(behavior) };
    color(DEFAULT_STYLE.background)
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn MACaptionAppearanceCopyWindowColor(
    _domain: Domain,
    behavior: *mut isize,
) -> CFTypeRef {
    unsafe { report(behavior) };
    color(DEFAULT_STYLE.window)
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn MACaptionAppearanceGetForegroundOpacity(
    _domain: Domain,
    behavior: *mut isize,
) -> f64 {
    unsafe { report(behavior) };
    DEFAULT_STYLE.foreground_opacity
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn MACaptionAppearanceGetBackgroundOpacity(
    _domain: Domain,
    behavior: *mut isize,
) -> f64 {
    unsafe { report(behavior) };
    DEFAULT_STYLE.background_opacity
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn MACaptionAppearanceGetWindowOpacity(
    _domain: Domain,
    behavior: *mut isize,
) -> f64 {
    unsafe { report(behavior) };
    DEFAULT_STYLE.window_opacity
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn MACaptionAppearanceGetWindowRoundedCornerRadius(
    _domain: Domain,
    behavior: *mut isize,
) -> f64 {
    unsafe { report(behavior) };
    DEFAULT_STYLE.window_corner_radius
}

/// The text size as a multiple of the size the content asks for.
#[allow(non_snake_case)]
pub unsafe extern "C" fn MACaptionAppearanceGetRelativeCharacterSize(
    _domain: Domain,
    behavior: *mut isize,
) -> f64 {
    unsafe { report(behavior) };
    DEFAULT_STYLE.relative_size
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn MACaptionAppearanceGetTextEdgeStyle(
    _domain: Domain,
    behavior: *mut isize,
) -> isize {
    unsafe { report(behavior) };
    DEFAULT_STYLE.text_edge
}

/// `CTFontDescriptorRef MACaptionAppearanceCopyFontDescriptorForStyle(domain,
/// behavior *, fontStyle)`: a descriptor naming the face, family and style,
/// which is what a caller reads `kCTFontFamilyNameAttribute` and
/// `kCTFontStyleNameAttribute` from.
#[allow(non_snake_case)]
pub unsafe extern "C" fn MACaptionAppearanceCopyFontDescriptorForStyle(
    _domain: Domain,
    behavior: *mut isize,
    style: isize,
) -> CFTypeRef {
    unsafe { report(behavior) };
    let (family, name) = face_of(style);
    let key = |text: &'static str| foundation::notification::name_object(text) as CFTypeRef;
    let family = corefoundation::string::from_bytes(family.as_bytes(), false);
    let name = corefoundation::string::from_bytes(name.as_bytes(), false);
    let face = corefoundation::string::from_bytes(b"Regular", false);
    let attributes = collections::new_dictionary(
        &[
            (key("NSFontNameAttribute"), name),
            (key("NSFontFamilyAttribute"), family),
            (key("NSFontFaceAttribute"), face),
        ],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        false,
    );
    for object in [family, name, face] {
        // SAFETY: the references made above; the dictionary holds its own.
        unsafe { corefoundation::api::CFRelease(object) };
    }
    // SAFETY: a fresh dictionary, which is what an unresolved descriptor is.
    let descriptor = unsafe { appkit::coretext::CTFontDescriptorCreateWithAttributes(attributes) };
    // SAFETY: the dictionary made above, now retained by the descriptor.
    unsafe { corefoundation::api::CFRelease(attributes) };
    descriptor
}

/// The `CFStringRef` globals this framework defines.
const STRING_GLOBALS: &[(&str, &str)] = &[(
    "_kMACaptionAppearanceSettingsChangedNotification",
    SETTINGS_CHANGED,
)];

pub fn data_symbols() -> Vec<(String, u64)> {
    corefoundation::base::string_globals(STRING_GLOBALS)
}

exports::exports! {
    functions {
        "MACaptionAppearanceCopyForegroundColor" => MACaptionAppearanceCopyForegroundColor,
        "MACaptionAppearanceCopyBackgroundColor" => MACaptionAppearanceCopyBackgroundColor,
        "MACaptionAppearanceCopyWindowColor" => MACaptionAppearanceCopyWindowColor,
        "MACaptionAppearanceGetForegroundOpacity" => MACaptionAppearanceGetForegroundOpacity,
        "MACaptionAppearanceGetBackgroundOpacity" => MACaptionAppearanceGetBackgroundOpacity,
        "MACaptionAppearanceGetWindowOpacity" => MACaptionAppearanceGetWindowOpacity,
        "MACaptionAppearanceGetWindowRoundedCornerRadius" =>
            MACaptionAppearanceGetWindowRoundedCornerRadius,
        "MACaptionAppearanceGetRelativeCharacterSize" => MACaptionAppearanceGetRelativeCharacterSize,
        "MACaptionAppearanceGetTextEdgeStyle" => MACaptionAppearanceGetTextEdgeStyle,
        "MACaptionAppearanceCopyFontDescriptorForStyle" =>
            MACaptionAppearanceCopyFontDescriptorForStyle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn components(color: CFTypeRef) -> [f64; 4] {
        // SAFETY: a color this crate made, which has four components.
        unsafe {
            assert_eq!(appkit::cgcontext::CGColorGetNumberOfComponents(color), 4);
            let p = appkit::cgcontext::CGColorGetComponents(color);
            [*p, *p.add(1), *p.add(2), *p.add(3)]
        }
    }

    fn text(object: CFTypeRef) -> String {
        // SAFETY: a string this runtime made.
        let string = unsafe { corefoundation::string::as_string(object) }.expect("a string");
        String::from_utf8_lossy(&string.utf8()).into_owned()
    }

    #[test]
    fn every_color_is_a_new_reference_and_behavior_may_be_null() {
        appkit::bootstrap();
        let mut behavior = -1isize;
        for copy in [
            MACaptionAppearanceCopyForegroundColor,
            MACaptionAppearanceCopyBackgroundColor,
            MACaptionAppearanceCopyWindowColor,
        ] {
            // SAFETY: a valid out-parameter, then none.
            let (a, b) = unsafe { (copy(1, &mut behavior), copy(0, std::ptr::null_mut())) };
            assert!(!a.is_null() && !b.is_null());
            assert_ne!(a, b, "Copy is a new reference each time");
            assert_eq!(components(a), components(b));
            assert_eq!(behavior, behavior::USE_CONTENT_IF_AVAILABLE);
            unsafe {
                corefoundation::api::CFRelease(a);
                corefoundation::api::CFRelease(b);
            }
        }
    }

    #[test]
    fn the_default_style_is_white_text_on_a_translucent_black_background() {
        appkit::bootstrap();
        let none = std::ptr::null_mut();
        unsafe {
            let fg = MACaptionAppearanceCopyForegroundColor(1, none);
            let bg = MACaptionAppearanceCopyBackgroundColor(1, none);
            assert_eq!(components(fg), [1.0, 1.0, 1.0, 1.0]);
            assert_eq!(components(bg), [0.0, 0.0, 0.0, 1.0]);
            assert_eq!(MACaptionAppearanceGetForegroundOpacity(1, none), 1.0);
            assert_eq!(MACaptionAppearanceGetBackgroundOpacity(1, none), 0.8);
            assert_eq!(MACaptionAppearanceGetWindowOpacity(1, none), 0.0);
            assert_eq!(
                MACaptionAppearanceGetWindowRoundedCornerRadius(1, none),
                0.0
            );
            assert_eq!(MACaptionAppearanceGetRelativeCharacterSize(1, none), 1.0);
            assert_eq!(
                MACaptionAppearanceGetTextEdgeStyle(1, none),
                text_edge::NONE
            );
            corefoundation::api::CFRelease(fg);
            corefoundation::api::CFRelease(bg);
        }
    }

    #[test]
    fn a_font_descriptor_names_its_family_and_face() {
        appkit::bootstrap();
        let family_key = foundation::notification::name_object("NSFontFamilyAttribute");
        let face_key = foundation::notification::name_object("NSFontFaceAttribute");
        for (style, family) in [
            (font_style::DEFAULT, "Helvetica"),
            (font_style::PROPORTIONAL_WITH_SERIF, "Times"),
            (font_style::MONOSPACED_WITH_SERIF, "Courier"),
            (99, "Helvetica"),
        ] {
            let mut behavior = -1isize;
            // SAFETY: a valid out-parameter.
            let descriptor =
                unsafe { MACaptionAppearanceCopyFontDescriptorForStyle(1, &mut behavior, style) };
            assert!(!descriptor.is_null());
            assert_eq!(behavior, behavior::USE_CONTENT_IF_AVAILABLE);
            unsafe {
                let got = appkit::coretext::CTFontDescriptorCopyAttribute(
                    descriptor,
                    family_key as CFTypeRef,
                );
                assert_eq!(text(got), family);
                corefoundation::api::CFRelease(got);
                let got = appkit::coretext::CTFontDescriptorCopyAttribute(
                    descriptor,
                    face_key as CFTypeRef,
                );
                assert_eq!(text(got), "Regular");
                corefoundation::api::CFRelease(got);
                corefoundation::api::CFRelease(descriptor);
            }
        }
    }

    #[test]
    fn the_notification_name_is_one_object() {
        foundation::bootstrap();
        let symbols = data_symbols();
        let (name, cell) = &symbols[0];
        assert_eq!(name, "_kMACaptionAppearanceSettingsChangedNotification");
        // SAFETY: a global cell holding a string object.
        let object = unsafe { *(*cell as *const CFTypeRef) };
        assert_eq!(text(object), SETTINGS_CHANGED);
        assert_eq!(
            object,
            foundation::notification::name_object(SETTINGS_CHANGED) as CFTypeRef
        );
    }
}
