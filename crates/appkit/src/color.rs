use std::cell::Cell;
use std::ffi::c_void;

use foundation::geometry::{CGFloat, NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
pub struct Color {
    isa: *const c_void,
    red: CGFloat,
    green: CGFloat,
    blue: CGFloat,
    alpha: CGFloat,
}

pub const INSTANCE_SIZE: usize = size_of::<Color>();

/// The components of `color`, or opaque black for anything that is not one.
///
/// # Safety
///
/// `color` must be null or an object this runtime allocated.
pub unsafe fn components(color: Id) -> (f64, f64, f64, f64) {
    let is_color = unsafe { objc::object::class_of(color) }.is_some_and(|c| {
        objc::class::lookup(c"NSColor").is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    });
    if !is_color {
        return (0.0, 0.0, 0.0, 1.0);
    }
    // SAFETY: the check above says it is one of this crate's colours.
    let c = unsafe { &*(color as *const Color) };
    (c.red, c.green, c.blue, c.alpha)
}

/// A colour object holding these components, owned by nobody: the caller gets it
/// back autoreleased, as every `+colorWith...` constructor's result is.
fn make(red: f64, green: f64, blue: f64, alpha: f64) -> Id {
    objc::arc::autorelease(allocate(red, green, blue, alpha))
}

/// A colour object with a reference this function keeps.
fn allocate(red: f64, green: f64, blue: f64, alpha: f64) -> Id {
    let class = objc::class::lookup(c"NSColor").expect("declared before its methods");
    // SAFETY: `Color` is the layout the class was registered with.
    let color = unsafe { corefoundation::base::allocate::<Color>(class) };
    // SAFETY: freshly allocated; every field is written before it is read.
    unsafe {
        (*color).red = red;
        (*color).green = green;
        (*color).blue = blue;
        (*color).alpha = alpha;
    }
    color as Id
}

/// The object for one named colour, made once and kept for the process.
///
/// **Not autoreleased, and that is the whole point.** `+controlTextColor` on a Mac
/// answers the same object every time and that object lives for ever, so a caller
/// may put one straight into a `static` - which is exactly what a widget toolkit
/// does: Qt's Cocoa theme fills a file-scope table of role colours from these in a
/// module initializer and reads it back whenever it builds a palette. An
/// autoreleased colour there is freed when the pool drains and the table is left
/// holding dangling pointers, which surfaces much later as a message sent to freed
/// memory in code that has nothing to do with colours.
///
/// Interned by name rather than per call for the same reason identity matters
/// elsewhere in this tree: a caller comparing two `+textColor`s expects one object.
fn singleton(name: &str, red: f64, green: f64, blue: f64, alpha: f64) -> Id {
    static COLORS: std::sync::Mutex<Vec<(String, usize)>> = std::sync::Mutex::new(Vec::new());
    let mut colors = COLORS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, object)) = colors.iter().find(|(n, _)| n == name) {
        return *object as Id;
    }
    let color = allocate(red, green, blue, alpha);
    // Retained forever: a named colour a guest can release is one it can make
    // dangle, and these are handed out unowned.
    objc::arc::make_immortal(color);
    colors.push((name.to_string(), color as usize));
    color
}

/// The named colours, as `(selector, r, g, b, a)`.
///
/// The first group is the device colours, whose values are their names. The
/// second is the semantic ones - `+textColor` and its relatives - whose value
/// depends on the appearance. These are the light values; the semantic roles
/// below resolve against the current drawing appearance when requested.
static NAMED: &[(&str, f64, f64, f64, f64)] = &[
    ("textColor", 0.0, 0.0, 0.0, 1.0),
    ("labelColor", 0.0, 0.0, 0.0, 1.0),
    ("windowBackgroundColor", 0.93, 0.93, 0.93, 1.0),
    ("controlColor", 0.93, 0.93, 0.93, 1.0),
    ("controlTextColor", 0.0, 0.0, 0.0, 1.0),
    ("controlBackgroundColor", 1.0, 1.0, 1.0, 1.0),
    ("textBackgroundColor", 1.0, 1.0, 1.0, 1.0),
    ("selectedTextColor", 0.0, 0.0, 0.0, 1.0),
    ("windowFrameTextColor", 0.0, 0.0, 0.0, 1.0),
    ("disabledControlTextColor", 0.53, 0.53, 0.53, 1.0),
    // The selection and header colours a widget toolkit builds a palette out of.
    // Qt asks for all of these while its Cocoa plugin is still loading, and one
    // it cannot get is not a wrong shade - it is an unrecognised selector and a
    // program that stops. The accent-dependent ones are the blue accent's, which
    // is the default and the only one this runtime has: there is no
    // `NSColorAppearance` here to read another from.
    ("headerTextColor", 0.0, 0.0, 0.0, 1.0),
    ("headerColor", 0.93, 0.93, 0.93, 1.0),
    ("selectedControlColor", 0.70, 0.84, 1.0, 1.0),
    ("selectedControlTextColor", 0.0, 0.0, 0.0, 1.0),
    ("selectedTextBackgroundColor", 0.70, 0.84, 1.0, 1.0),
    ("alternateSelectedControlColor", 0.0, 0.39, 0.88, 1.0),
    ("alternateSelectedControlTextColor", 1.0, 1.0, 1.0, 1.0),
    ("selectedContentBackgroundColor", 0.0, 0.39, 0.88, 1.0),
    ("selectedMenuItemTextColor", 1.0, 1.0, 1.0, 1.0),
    // "Unemphasized" is the selection in a view that is not the first responder:
    // grey rather than accented, with the ordinary text colour on top.
    (
        "unemphasizedSelectedContentBackgroundColor",
        0.86,
        0.86,
        0.86,
        1.0,
    ),
    (
        "unemphasizedSelectedTextBackgroundColor",
        0.86,
        0.86,
        0.86,
        1.0,
    ),
    ("unemphasizedSelectedTextColor", 0.0, 0.0, 0.0, 1.0),
    // The focus ring is drawn over what it surrounds, so its alpha is part of
    // the colour rather than something the caller applies.
    ("keyboardFocusIndicatorColor", 0.0, 0.37, 0.89, 0.25),
    ("linkColor", 0.0, 0.41, 0.85, 1.0),
    ("shadowColor", 0.0, 0.0, 0.0, 1.0),
    ("highlightColor", 1.0, 1.0, 1.0, 1.0),
    ("gridColor", 0.90, 0.90, 0.90, 1.0),
    // The label tiers and the separator differ from `labelColor` only in alpha,
    // which is how AppKit defines them.
    ("secondaryLabelColor", 0.0, 0.0, 0.0, 0.5),
    ("tertiaryLabelColor", 0.0, 0.0, 0.0, 0.26),
    ("quaternaryLabelColor", 0.0, 0.0, 0.0, 0.1),
    ("placeholderTextColor", 0.0, 0.0, 0.0, 0.25),
    ("separatorColor", 0.0, 0.0, 0.0, 0.1),
    ("underPageBackgroundColor", 0.60, 0.60, 0.60, 1.0),
    ("alternatingContentBackgroundColor", 0.96, 0.96, 0.96, 1.0),
    // The `systemX` palette, whose values are Apple's published sRGB ones. A
    // program picks one to draw with rather than asking the appearance about it.
    ("systemRedColor", 1.0, 0.23, 0.19, 1.0),
    ("systemGreenColor", 0.16, 0.80, 0.25, 1.0),
    ("systemBlueColor", 0.0, 0.48, 1.0, 1.0),
    ("systemOrangeColor", 1.0, 0.58, 0.0, 1.0),
    ("systemYellowColor", 1.0, 0.80, 0.0, 1.0),
    ("systemPinkColor", 1.0, 0.18, 0.33, 1.0),
    ("systemPurpleColor", 0.69, 0.32, 0.87, 1.0),
    ("systemTealColor", 0.35, 0.78, 0.98, 1.0),
    ("systemIndigoColor", 0.35, 0.34, 0.84, 1.0),
    ("systemBrownColor", 0.64, 0.52, 0.37, 1.0),
    ("systemGrayColor", 0.56, 0.56, 0.58, 1.0),
    ("controlAccentColor", 0.0, 0.39, 0.88, 1.0),
    ("blackColor", 0.0, 0.0, 0.0, 1.0),
    ("whiteColor", 1.0, 1.0, 1.0, 1.0),
    ("clearColor", 0.0, 0.0, 0.0, 0.0),
    ("redColor", 1.0, 0.0, 0.0, 1.0),
    ("greenColor", 0.0, 1.0, 0.0, 1.0),
    ("blueColor", 0.0, 0.0, 1.0, 1.0),
    ("yellowColor", 1.0, 1.0, 0.0, 1.0),
    ("cyanColor", 0.0, 1.0, 1.0, 1.0),
    ("magentaColor", 1.0, 0.0, 1.0, 1.0),
    ("grayColor", 0.5, 0.5, 0.5, 1.0),
    ("lightGrayColor", 2.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0, 1.0),
    ("darkGrayColor", 1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0, 1.0),
];

/// One `+namedColor`, resolved by the selector it was sent.
///
/// A single implementation for the whole table rather than twelve: the selector
/// is an argument of every method, so the name it was reached by is the lookup
/// key, and twelve near-identical functions would be twelve places to get a
/// component wrong.
unsafe extern "C" fn named(_class: Id, sel: Sel) -> Id {
    // SAFETY: a registered selector is a NUL-terminated interned name.
    let name = unsafe { std::ffi::CStr::from_ptr(sel) };
    let name = name.to_string_lossy();
    let dark = match name.as_ref() {
        "textColor"
        | "labelColor"
        | "controlTextColor"
        | "selectedTextColor"
        | "windowFrameTextColor"
        | "headerTextColor"
        | "selectedControlTextColor"
        | "unemphasizedSelectedTextColor" => Some((1.0, 1.0)),
        "secondaryLabelColor" => Some((1.0, 0.5)),
        "tertiaryLabelColor" => Some((1.0, 0.26)),
        "quaternaryLabelColor" => Some((1.0, 0.1)),
        "placeholderTextColor" => Some((1.0, 0.25)),
        "separatorColor" => Some((1.0, 0.1)),
        "windowBackgroundColor" | "controlColor" | "headerColor" => Some((0.18, 1.0)),
        "controlBackgroundColor" | "textBackgroundColor" => Some((0.12, 1.0)),
        "unemphasizedSelectedContentBackgroundColor"
        | "unemphasizedSelectedTextBackgroundColor" => Some((0.30, 1.0)),
        "gridColor" => Some((0.30, 1.0)),
        "underPageBackgroundColor" => Some((0.10, 1.0)),
        "alternatingContentBackgroundColor" => Some((0.16, 1.0)),
        _ => None,
    };
    if let Some((gray, alpha)) = dark
        && crate::appearance::is_dark()
    {
        return singleton(&format!("{name}:dark"), gray, gray, gray, alpha);
    }
    match NAMED.iter().find(|(n, ..)| *n == name) {
        Some((_, r, g, b, a)) => singleton(&name, *r, *g, *b, *a),
        // Unreachable through dispatch - the selector was installed from this
        // very table - and named rather than silent in case it stops being.
        None => objc::fail!("+[NSColor {name}]: not one of this runtime's named colours"),
    }
}

/// An autoreleased colour of these components, for a decoder.
pub(crate) fn rgba(red: f64, green: f64, blue: f64, alpha: f64) -> Id {
    make(red, green, blue, alpha)
}

/// The named system colour `name`, such as `labelColor`, or `None` for one this
/// runtime does not define.
pub(crate) fn named_color(name: &str) -> Option<Id> {
    NAMED
        .iter()
        .find(|(n, ..)| *n == name)
        .map(|_| unsafe { named(std::ptr::null_mut(), objc::sel::get(name)) })
}

unsafe extern "C" fn alternating_row_colors(_class: Id, _sel: Sel) -> Id {
    static COLORS: std::sync::Mutex<[usize; 2]> = std::sync::Mutex::new([0; 2]);
    let dark = crate::appearance::is_dark() as usize;
    let mut colors = COLORS.lock().unwrap_or_else(|e| e.into_inner());
    let kept = &mut colors[dark];
    if *kept != 0 {
        return *kept as Id;
    }
    let even = named_color("controlBackgroundColor").expect("a named color");
    let odd = named_color("alternatingContentBackgroundColor").expect("a named color");
    let array = corefoundation::collections::new_array(
        &[
            even as corefoundation::base::CFTypeRef,
            odd as corefoundation::base::CFTypeRef,
        ],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    *kept = array as usize;
    array as Id
}

/// `-highlightWithLevel:`
unsafe extern "C" fn highlight_with_level(receiver: Id, _sel: Sel, level: CGFloat) -> Id {
    // SAFETY: a colour of this crate's.
    let (r, g, b, a) = unsafe { components(receiver) };
    let t = if level.is_nan() {
        0.0
    } else {
        level.clamp(0.0, 1.0)
    };
    let mix = |c: f64| c + (1.0 - c) * t;
    make(mix(r), mix(g), mix(b), a)
}

unsafe extern "C" fn color_with_rgba(
    _class: Id,
    _sel: Sel,
    red: CGFloat,
    green: CGFloat,
    blue: CGFloat,
    alpha: CGFloat,
) -> Id {
    make(red, green, blue, alpha)
}

unsafe extern "C" fn color_with_white(_class: Id, _sel: Sel, white: CGFloat, alpha: CGFloat) -> Id {
    make(white, white, white, alpha)
}

unsafe extern "C" fn component(receiver: Id, sel: Sel) -> CGFloat {
    // SAFETY: a colour of this crate's.
    let c = unsafe { &*(receiver as *const Color) };
    // SAFETY: a registered selector is a NUL-terminated interned name.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"redComponent" => c.red,
        b"greenComponent" => c.green,
        b"blueComponent" => c.blue,
        _ => c.alpha,
    }
}

/// `-colorSpaceName`, the deprecated name of the space a colour is in.
///
/// Calibrated RGB, which is what every colour here is: four components in the
/// space the module note describes. A caller compares the result against
/// `NSDeviceCMYKColorSpace` before reaching for the CMYK accessors, and this sends
/// it down the RGB branch - which is the one that can be answered.
unsafe extern "C" fn color_space_name(_receiver: Id, _sel: Sel) -> Id {
    // The same interned object the `_NSCalibratedRGBColorSpace` global holds, so a
    // caller comparing the two with `==` - which is what a space-name check is
    // written as - finds them identical as well as equal.
    foundation::notification::name_object("NSCalibratedRGBColorSpace") as Id
}

/// `-colorUsingColorSpaceName:`
///
/// The receiver when the space asked for is an RGB one, and nil otherwise.
///
/// This is not the `self`-returning conversion the module note refuses. The note's
/// objection is to a conversion that silently did not happen; a colour here *is*
/// calibrated RGB, so answering it to a caller that asked for RGB is the
/// conversion, complete. A caller that asked for CMYK or grey gets nil, which is
/// the documented answer for a colour that cannot be converted and the branch it
/// already has - rather than four RGB components relabelled as something else.
///
/// # Safety
///
/// `name` is an `NSString` or nil.
unsafe extern "C" fn color_using_color_space_name(receiver: Id, _sel: Sel, name: Id) -> Id {
    let name = unsafe { foundation::string::bytes(name) };
    // A null or empty name means "the most appropriate space", which is this one.
    if name.is_empty() {
        return receiver;
    }
    const RGB: [&str; 4] = [
        "NSCalibratedRGBColorSpace",
        "NSDeviceRGBColorSpace",
        "NSCustomColorSpace",
        "kCGColorSpaceSRGB",
    ];
    match RGB.iter().any(|space| space.as_bytes() == name.as_slice()) {
        true => receiver,
        false => std::ptr::null_mut(),
    }
}

/// `-colorUsingColorSpace:`
///
/// The receiver. There is one colour space in this runtime and every colour is
/// already in it - see the module note - and the `NSColorSpace` a caller would pass
/// to ask for another is a class this tree does not have, so the argument is
/// always the space the receiver is in or nil.
unsafe extern "C" fn color_using_color_space(receiver: Id, _sel: Sel, _space: Id) -> Id {
    receiver
}

/// `-catalogNameComponent` and `-colorNameComponent`.
///
/// Nil, because a colour here is not a catalog colour: it is four components, and
/// `+controlTextColor` resolved them when it was sent rather than handing back a
/// named reference into a catalog that is resolved later.
///
/// The distinction is one a caller acts on. Qt asks whether the catalog is
/// `System`, and renders a colour that says yes into a pattern image instead of
/// reading its components - so claiming a catalog would send it down a drawing
/// path to represent a colour that is already a plain one.
unsafe extern "C" fn name_component(_receiver: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

/// `-numberOfComponents`, which is four: red, green, blue and alpha.
unsafe extern "C" fn number_of_components(_receiver: Id, _sel: Sel) -> isize {
    4
}

/// `-whiteComponent`, the brightness of a grey colour.
///
/// The Rec. 601 luma of the three components, which is what AppKit answers for a
/// colour that is not already grey - and for one that is, the three are equal and
/// any weighting gives the same number back.
unsafe extern "C" fn white_component(receiver: Id, _sel: Sel) -> CGFloat {
    // SAFETY: a colour of this crate's.
    let c = unsafe { &*(receiver as *const Color) };
    0.299 * c.red + 0.587 * c.green + 0.114 * c.blue
}

/// `-getWhite:alpha:`
///
/// # Safety
///
/// Each out-parameter is null or writable for one `CGFloat`.
unsafe extern "C" fn get_white(receiver: Id, sel: Sel, white: *mut CGFloat, alpha: *mut CGFloat) {
    // SAFETY: a colour of this crate's.
    let c = unsafe { &*(receiver as *const Color) };
    // SAFETY: the caller states each is null or writable.
    unsafe {
        if !white.is_null() {
            *white = white_component(receiver, sel);
        }
        if !alpha.is_null() {
            *alpha = c.alpha;
        }
    }
}

unsafe extern "C" fn get_components(
    receiver: Id,
    _sel: Sel,
    red: *mut CGFloat,
    green: *mut CGFloat,
    blue: *mut CGFloat,
    alpha: *mut CGFloat,
) {
    // SAFETY: a colour of this crate's.
    let c = unsafe { &*(receiver as *const Color) };
    // SAFETY: four out-parameters, each null or writable, as the method's
    // contract says.
    unsafe {
        for (slot, value) in [
            (red, c.red),
            (green, c.green),
            (blue, c.blue),
            (alpha, c.alpha),
        ] {
            if !slot.is_null() {
                *slot = value;
            }
        }
    }
}

/// `-CGColor`: the same components as a `CGColorRef`, autoreleased, which is
/// as long as a caller that does not retain it may keep it.
unsafe extern "C" fn cg_color(receiver: Id, _sel: Sel) -> *const c_void {
    // SAFETY: a colour of this crate's.
    let (red, green, blue, alpha) = unsafe { components(receiver) };
    // SAFETY: a fresh colour this call owns.
    unsafe {
        let color = crate::cgcontext::CGColorCreateGenericRGB(red, green, blue, alpha);
        corefoundation::api::CFAutorelease(color)
    }
}

// FIXME (ACCURACY): WRONG?
thread_local! {
    static FILL: Cell<[f64; 4]> = const { Cell::new([0.0, 0.0, 0.0, 1.0]) };
    static STROKE: Cell<[f64; 4]> = const { Cell::new([0.0, 0.0, 0.0, 1.0]) };
}

/// The current fill and stroke colours, as straight RGBA.
pub(crate) fn current() -> ([f64; 4], [f64; 4]) {
    (FILL.with(Cell::get), STROKE.with(Cell::get))
}

/// Put back colours [`current`] answered, as restoring a graphics state does.
pub(crate) fn restore(fill: [f64; 4], stroke: [f64; 4]) {
    FILL.with(|cell| cell.set(fill));
    STROKE.with(|cell| cell.set(stroke));
}

unsafe extern "C" fn set_fill(receiver: Id, _sel: Sel) {
    let (red, green, blue, alpha) = unsafe { components(receiver) };
    FILL.with(|fill| fill.set([red, green, blue, alpha]));
}

unsafe extern "C" fn set_stroke(receiver: Id, _sel: Sel) {
    let (red, green, blue, alpha) = unsafe { components(receiver) };
    STROKE.with(|stroke| stroke.set([red, green, blue, alpha]));
}

unsafe extern "C" fn set_both(receiver: Id, sel: Sel) {
    unsafe {
        set_fill(receiver, sel);
        set_stroke(receiver, sel);
    }
}

#[allow(non_snake_case)]
pub extern "C" fn NSRectFill(rect: NSRect) {
    if crate::bezier::fill_rect_through_state(rect, FILL.with(Cell::get)) {
        return;
    }
    let [red, green, blue, alpha] = FILL.with(Cell::get);
    let channel = |value: f64| (value.clamp(0.0, 1.0) * alpha.clamp(0.0, 1.0) * 255.0) as u8;
    let pixel = [
        channel(blue),
        channel(green),
        channel(red),
        (alpha.clamp(0.0, 1.0) * 255.0) as u8,
    ];
    crate::backing::draw_image(
        &pixel,
        1,
        1,
        NSSize {
            width: 1.0,
            height: 1.0,
        },
        rect,
        NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSSize {
                width: 1.0,
                height: 1.0,
            },
        },
        1,
        1.0,
        true,
    );
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSColor").expect("declared before its methods");
    let meta = class.metaclass();
    for (name, ..) in NAMED {
        meta.replace_method(objc::sel::get(name), named as *const c_void, c"@16@0:8");
    }
    class_methods!(class, {
        "controlAlternatingRowBackgroundColors" => alternating_row_colors, c"@16@0:8";
        "alternatingContentBackgroundColors" => alternating_row_colors, c"@16@0:8";
        "colorWithCalibratedRed:green:blue:alpha:" => color_with_rgba, c"@48@0:8d16d24d32d40";
        "colorWithDeviceRed:green:blue:alpha:" => color_with_rgba, c"@48@0:8d16d24d32d40";
        "colorWithSRGBRed:green:blue:alpha:" => color_with_rgba, c"@48@0:8d16d24d32d40";
        "colorWithRed:green:blue:alpha:" => color_with_rgba, c"@48@0:8d16d24d32d40";
        "colorWithCalibratedWhite:alpha:" => color_with_white, c"@32@0:8d16d24";
        "colorWithDeviceWhite:alpha:" => color_with_white, c"@32@0:8d16d24";
        "colorWithWhite:alpha:" => color_with_white, c"@32@0:8d16d24";
    });
    methods!(class, {
        "highlightWithLevel:" => highlight_with_level, c"@24@0:8d16";
        "setFill" => set_fill, c"v16@0:8";
        "setStroke" => set_stroke, c"v16@0:8";
        "set" => set_both, c"v16@0:8";
        "CGColor" => cg_color, c"^{CGColor=}16@0:8";
        "redComponent" => component, c"d16@0:8";
        "greenComponent" => component, c"d16@0:8";
        "blueComponent" => component, c"d16@0:8";
        "alphaComponent" => component, c"d16@0:8";
        "getRed:green:blue:alpha:" => get_components, c"v48@0:8^d16^d24^d32^d40";
        "whiteComponent" => white_component, c"d16@0:8";
        "getWhite:alpha:" => get_white, c"v32@0:8^d16^d24";
        "numberOfComponents" => number_of_components, c"q16@0:8";
        "colorSpaceName" => color_space_name, c"@16@0:8";
        "colorUsingColorSpaceName:" => color_using_color_space_name, c"@24@0:8@16";
        "colorUsingColorSpaceName:device:" => color_using_color_space_name, c"@32@0:8@16@24";
        "colorUsingColorSpace:" => color_using_color_space, c"@24@0:8@16";
        "catalogNameComponent" => name_component, c"@16@0:8";
        "colorNameComponent" => name_component, c"@16@0:8";
        "localizedCatalogNameComponent" => name_component, c"@16@0:8";
        "localizedColorNameComponent" => name_component, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every named colour answers, and answers with its own components - which
    /// is the claim one shared implementation has to earn.
    #[test]
    fn each_named_colour_is_its_own() {
        crate::bootstrap();
        crate::appearance::use_aqua_on_this_thread();
        let class = objc::class::ptr(objc::class::lookup(c"NSColor").expect("NSColor")) as Id;
        for (name, r, g, b, a) in NAMED {
            // SAFETY: `+namedColor` is `id (Class, SEL)`.
            let color = unsafe { named(class, objc::sel::get(name)) };
            assert!(!color.is_null(), "{name}");
            // SAFETY: a colour this crate just made.
            assert_eq!(unsafe { components(color) }, (*r, *g, *b, *a), "{name}");
        }
    }

    /// A named colour is the *same object* every time and is not autoreleased.
    ///
    /// The case this protects: a caller that puts one in a `static` during a module
    /// initializer and reads it back after the pool has drained. An autoreleased
    /// colour there is freed, and the read is a message to freed memory.
    #[test]
    fn a_named_colour_is_one_kept_object() {
        crate::bootstrap();
        crate::appearance::use_aqua_on_this_thread();
        let class = objc::class::ptr(objc::class::lookup(c"NSColor").expect("NSColor")) as Id;
        let sel = objc::sel::get("controlTextColor");
        // SAFETY: `+namedColor` is `id (Class, SEL)`.
        let (first, second) = unsafe { (named(class, sel), named(class, sel)) };
        assert_eq!(first, second, "a named colour is a singleton");
        // Drain a pool around a read: an autoreleased colour would not survive it.
        let pool = objc::arc::pool_push();
        // SAFETY: the colour just obtained.
        let before = unsafe { components(first) };
        objc::arc::pool_pop(pool);
        // SAFETY: still live, because nothing autoreleased it.
        assert_eq!(unsafe { components(first) }, before);
        assert_eq!(before, (0.0, 0.0, 0.0, 1.0));
    }

    /// `+alternatingContentBackgroundColors` is the same kept pair as the older
    /// `+controlAlternatingRowBackgroundColors`, so a caller using either gets
    /// matching row shades.
    #[test]
    fn alternating_content_colours_match_the_row_colours() {
        crate::bootstrap();
        let class = objc::class::ptr(objc::class::lookup(c"NSColor").expect("NSColor")) as Id;
        // SAFETY: both are `id (Class, SEL)` messages to a class this crate
        // installed.
        let (old, new) = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            (
                f(
                    class,
                    objc::sel::get("controlAlternatingRowBackgroundColors"),
                ),
                f(class, objc::sel::get("alternatingContentBackgroundColors")),
            )
        };
        assert!(!new.is_null());
        assert_eq!(old, new);
    }

    /// A highlight moves each component toward white by the level and keeps alpha.
    #[test]
    fn highlight_with_level_blends_toward_white() {
        crate::bootstrap();
        let color = make(0.2, 0.4, 0.0, 0.5);
        // SAFETY: `-highlightWithLevel:` is `id (id, SEL, double)`.
        let f: unsafe extern "C" fn(Id, Sel, f64) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let sel = objc::sel::get("highlightWithLevel:");
        // SAFETY: colours this crate just made.
        let (r, g, b, a) = unsafe { components(f(color, sel, 0.5)) };
        assert!((r - 0.6).abs() < 1e-9 && (g - 0.7).abs() < 1e-9 && (b - 0.5).abs() < 1e-9);
        assert_eq!(a, 0.5);
        // SAFETY: as above.
        assert_eq!(
            unsafe { components(f(color, sel, 7.0)) },
            (1.0, 1.0, 1.0, 0.5)
        );
        // SAFETY: as above.
        assert_eq!(
            unsafe { components(f(color, sel, 0.0)) },
            (0.2, 0.4, 0.0, 0.5)
        );
    }

    /// And something that is not a colour reads as opaque black rather than as
    /// whatever its second word happens to be.
    #[test]
    fn a_non_colour_reads_as_black() {
        crate::bootstrap();
        let string = foundation::string::from_str("not a colour");
        // SAFETY: a live object.
        assert_eq!(unsafe { components(string) }, (0.0, 0.0, 0.0, 1.0));
        // SAFETY: null is allowed.
        assert_eq!(
            unsafe { components(std::ptr::null_mut()) },
            (0.0, 0.0, 0.0, 1.0)
        );
    }
}
