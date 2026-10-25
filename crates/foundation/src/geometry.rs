#![allow(clippy::missing_safety_doc, non_snake_case)]

use std::ffi::{CStr, CString, c_char};

use coregraphics::cggeometry;

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

pub use coregraphics::CGFloat;

pub type NSPoint = coregraphics::CGPoint;
pub type NSSize = coregraphics::CGSize;
pub type NSRect = coregraphics::CGRect;

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct NSRange {
    pub location: usize,
    pub length: usize,
}

/// `void NSDisableScreenUpdates(void)` and `NSEnableScreenUpdates`: they hold
/// back the window server's flushes so several windows change at once. Each
/// window here presents on its own, so there is nothing to hold back.
pub extern "C" fn NSDisableScreenUpdates() {}

pub extern "C" fn NSEnableScreenUpdates() {}

pub unsafe extern "C" fn NSEqualRects(a: NSRect, b: NSRect) -> u8 {
    unsafe { cggeometry::CGRectEqualToRect(a, b) }
}

pub unsafe extern "C" fn NSIsEmptyRect(rect: NSRect) -> u8 {
    u8::from(rect.size.width <= 0.0 || rect.size.height <= 0.0)
}

pub unsafe extern "C" fn NSUnionRect(a: NSRect, b: NSRect) -> NSRect {
    match unsafe { (NSIsEmptyRect(a) != 0, NSIsEmptyRect(b) != 0) } {
        (true, true) => NSRect::default(),
        (true, false) => b,
        (false, true) => a,
        (false, false) => {
            let x = a.origin.x.min(b.origin.x);
            let y = a.origin.y.min(b.origin.y);
            let right = (a.origin.x + a.size.width).max(b.origin.x + b.size.width);
            let top = (a.origin.y + a.size.height).max(b.origin.y + b.size.height);
            NSRect {
                origin: NSPoint { x, y },
                size: NSSize {
                    width: right - x,
                    height: top - y,
                },
            }
        }
    }
}

/// `BOOL NSPointInRect(NSPoint, NSRect)`.
pub unsafe extern "C" fn NSPointInRect(point: NSPoint, rect: NSRect) -> u8 {
    cggeometry::contains(rect, point) as u8
}

/// `BOOL NSMouseInRect(NSPoint, NSRect, BOOL)`.
///
/// Cocoa assigns a point on a shared edge to only one adjacent rectangle. The
/// horizontal interval is always half-open. The vertical interval follows the
/// view orientation: a flipped view owns its minimum-y edge, while an
/// unflipped view owns its maximum-y edge.
pub unsafe extern "C" fn NSMouseInRect(point: NSPoint, rect: NSRect, flipped: u8) -> u8 {
    // SAFETY: the accessors take a rectangle by value and standardize its extents.
    let (x, y, width, height) = unsafe {
        (
            cggeometry::CGRectGetMinX(rect),
            cggeometry::CGRectGetMinY(rect),
            cggeometry::CGRectGetWidth(rect),
            cggeometry::CGRectGetHeight(rect),
        )
    };
    let horizontal = point.x >= x && point.x < x + width;
    let vertical = if flipped != 0 {
        point.y >= y && point.y < y + height
    } else {
        point.y > y && point.y <= y + height
    };
    (horizontal && vertical) as u8
}

/// `NSRect NSInsetRect(NSRect, CGFloat, CGFloat)`: the rectangle shrunk by `dx`
/// on each side and `dy` on each end. A negative amount grows it, which is how
/// callers use it to make a margin.
pub unsafe extern "C" fn NSInsetRect(rect: NSRect, dx: CGFloat, dy: CGFloat) -> NSRect {
    NSRect {
        origin: NSPoint {
            x: rect.origin.x + dx,
            y: rect.origin.y + dy,
        },
        size: NSSize {
            width: rect.size.width - 2.0 * dx,
            height: rect.size.height - 2.0 * dy,
        },
    }
}

/// `NSRect NSIntersectionRect(NSRect, NSRect)`: no overlap is the zero rectangle.
pub unsafe extern "C" fn NSIntersectionRect(a: NSRect, b: NSRect) -> NSRect {
    let x = a.origin.x.max(b.origin.x);
    let y = a.origin.y.max(b.origin.y);
    let right = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let top = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    if right <= x || top <= y {
        return NSRect::default();
    }
    NSRect {
        origin: NSPoint { x, y },
        size: NSSize {
            width: right - x,
            height: top - y,
        },
    }
}

/// `NSRect NSOffsetRect(NSRect, CGFloat, CGFloat)`, the same as
/// `CGRectOffset`.
pub unsafe extern "C" fn NSOffsetRect(rect: NSRect, dx: CGFloat, dy: CGFloat) -> NSRect {
    NSRect {
        origin: NSPoint {
            x: rect.origin.x + dx,
            y: rect.origin.y + dy,
        },
        size: rect.size,
    }
}

/// `BOOL NSContainsRect(NSRect a, NSRect b)`: AppKit's, which refuses an
/// empty `b`.
pub unsafe extern "C" fn NSContainsRect(a: NSRect, b: NSRect) -> u8 {
    let empty = unsafe { NSIsEmptyRect(b) } != 0;
    (!empty
        && a.origin.x <= b.origin.x
        && a.origin.y <= b.origin.y
        && a.origin.x + a.size.width >= b.origin.x + b.size.width
        && a.origin.y + a.size.height >= b.origin.y + b.size.height) as u8
}

/// `BOOL NSIntersectsRect(NSRect, NSRect)`.
pub unsafe extern "C" fn NSIntersectsRect(a: NSRect, b: NSRect) -> u8 {
    let empty = unsafe { NSIsEmptyRect(a) != 0 || NSIsEmptyRect(b) != 0 };
    (!empty
        && a.origin.x < b.origin.x + b.size.width
        && b.origin.x < a.origin.x + a.size.width
        && a.origin.y < b.origin.y + b.size.height
        && b.origin.y < a.origin.y + a.size.height) as u8
}

/// `NSRect NSIntegralRect(NSRect)`: outward to whole numbers, and the zero
/// rectangle for an empty one.
pub unsafe extern "C" fn NSIntegralRect(rect: NSRect) -> NSRect {
    if unsafe { NSIsEmptyRect(rect) } != 0 {
        return NSRect::default();
    }
    unsafe { cggeometry::CGRectIntegral(rect) }
}

pub unsafe extern "C" fn NSEqualPoints(a: NSPoint, b: NSPoint) -> u8 {
    (a.x == b.x && a.y == b.y) as u8
}

pub unsafe extern "C" fn NSEqualSizes(a: NSSize, b: NSSize) -> u8 {
    (a.width == b.width && a.height == b.height) as u8
}

/// `NSRange NSIntersectionRange(NSRange, NSRange)`: the zero range when they
/// do not overlap.
pub unsafe extern "C" fn NSIntersectionRange(a: NSRange, b: NSRange) -> NSRange {
    let start = a.location.max(b.location);
    let end = (a.location + a.length).min(b.location + b.length);
    match end > start {
        true => NSRange {
            location: start,
            length: end - start,
        },
        false => NSRange::default(),
    }
}

/// A number as `%g` writes it, which is how Foundation prints geometry: six
/// significant digits, no trailing zeros, and an exponent outside 1e-4..1e6.
fn g(value: f64) -> String {
    if value == 0.0 || !value.is_finite() {
        return match value {
            v if v.is_nan() => "nan".into(),
            v if v.is_infinite() && v > 0.0 => "inf".into(),
            v if v.is_infinite() => "-inf".into(),
            _ => "0".into(),
        };
    }
    let exponent = value.abs().log10().floor() as i32;
    let trim = |text: String| {
        if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            text
        }
    };
    if !(-4..6).contains(&exponent) {
        let text = format!("{:.5e}", value);
        let (mantissa, power) = text.split_once('e').unwrap_or((&text, "0"));
        let power: i32 = power.parse().unwrap_or(0);
        let sign = if power < 0 { '-' } else { '+' };
        return format!("{}e{sign}{:02}", trim(mantissa.to_string()), power.abs());
    }
    let decimals = (5 - exponent).max(0) as usize;
    trim(format!("{value:.decimals$}"))
}

fn autoreleased_string(text: &str) -> Id {
    objc::arc::autorelease(crate::string::from_str(text))
}

/// The numbers in an `NSString`, in order, as Foundation's parsers scan them:
/// anything that is not part of a number separates.
unsafe fn numbers(string: Id) -> Vec<f64> {
    if string.is_null() {
        return Vec::new();
    }
    // SAFETY: an NSString, by the parsers' prototypes.
    let text = String::from_utf8_lossy(&unsafe { crate::string::bytes(string) }).into_owned();
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E') {
            current.push(c);
        } else if !current.is_empty() {
            if let Ok(value) = current.parse() {
                out.push(value);
            }
            current.clear();
        }
    }
    out
}

pub unsafe extern "C" fn NSStringFromPoint(point: NSPoint) -> Id {
    autoreleased_string(&format!("{{{}, {}}}", g(point.x), g(point.y)))
}

pub unsafe extern "C" fn NSStringFromSize(size: NSSize) -> Id {
    autoreleased_string(&format!("{{{}, {}}}", g(size.width), g(size.height)))
}

pub unsafe extern "C" fn NSStringFromRect(rect: NSRect) -> Id {
    autoreleased_string(&format!(
        "{{{{{}, {}}}, {{{}, {}}}}}",
        g(rect.origin.x),
        g(rect.origin.y),
        g(rect.size.width),
        g(rect.size.height)
    ))
}

pub unsafe extern "C" fn NSStringFromRange(range: NSRange) -> Id {
    autoreleased_string(&format!("{{{}, {}}}", range.location, range.length))
}

pub unsafe extern "C" fn NSPointFromString(string: Id) -> NSPoint {
    let n = unsafe { numbers(string) };
    let at = |i: usize| n.get(i).copied().unwrap_or(0.0);
    NSPoint { x: at(0), y: at(1) }
}

pub unsafe extern "C" fn NSSizeFromString(string: Id) -> NSSize {
    let n = unsafe { numbers(string) };
    let at = |i: usize| n.get(i).copied().unwrap_or(0.0);
    NSSize {
        width: at(0),
        height: at(1),
    }
}

pub unsafe extern "C" fn NSRectFromString(string: Id) -> NSRect {
    let n = unsafe { numbers(string) };
    let at = |i: usize| n.get(i).copied().unwrap_or(0.0);
    NSRect {
        origin: NSPoint { x: at(0), y: at(1) },
        size: NSSize {
            width: at(2),
            height: at(3),
        },
    }
}

exports::exports! {
    functions {
        "NSDisableScreenUpdates" => NSDisableScreenUpdates,
        "NSEnableScreenUpdates" => NSEnableScreenUpdates,
        "NSEqualRects" => NSEqualRects,
        "NSIsEmptyRect" => NSIsEmptyRect,
        "NSUnionRect" => NSUnionRect,
        "NSPointInRect" => NSPointInRect,
        "NSMouseInRect" => NSMouseInRect,
        "NSInsetRect" => NSInsetRect,
        "NSIntersectionRect" => NSIntersectionRect,
        "NSOffsetRect" => NSOffsetRect,
        "NSContainsRect" => NSContainsRect,
        "NSIntersectsRect" => NSIntersectsRect,
        "NSIntegralRect" => NSIntegralRect,
        "NSEqualPoints" => NSEqualPoints,
        "NSEqualSizes" => NSEqualSizes,
        "NSIntersectionRange" => NSIntersectionRange,
        "NSStringFromPoint" => NSStringFromPoint,
        "NSStringFromSize" => NSStringFromSize,
        "NSStringFromRect" => NSStringFromRect,
        "NSStringFromRange" => NSStringFromRange,
        "NSPointFromString" => NSPointFromString,
        "NSSizeFromString" => NSSizeFromString,
        "NSRectFromString" => NSRectFromString,
    }
}

/// The type encodings these have, which `-objCType` answers with and which a
/// guest compares against to decide how to unpack a value.
mod encoding {
    pub const POINT: &std::ffi::CStr = c"{CGPoint=dd}";
    pub const SIZE: &std::ffi::CStr = c"{CGSize=dd}";
    pub const RECT: &std::ffi::CStr = c"{CGRect={CGPoint=dd}{CGSize=dd}}";
    pub const RANGE: &std::ffi::CStr = c"{_NSRange=QQ}";
}

/// A boxed value: the bytes, and what they are.
///
/// Bytes rather than an enum of the four shapes above, because `+valueWithBytes:objCType:`
/// boxes anything a guest hands it and `-getValue:` hands it back. The named
/// constructors are the same thing with the encoding filled in.
struct Boxed {
    bytes: Vec<u8>,
    encoding: CString,
}

#[repr(C)]
struct Body {
    isa: *const Class,
    boxed: *mut Boxed,
}

fn make(bytes: &[u8], encoding: &CStr) -> Id {
    let class = objc::class::lookup(c"NSValue").expect("NSValue is registered");
    let object = objc::object::create_instance(class, 0);
    if object.is_null() {
        return object;
    }
    // SAFETY: a live `NSValue`, whose second word is this crate's to fill in.
    unsafe {
        (*(object as *mut Body)).boxed = Box::into_raw(Box::new(Boxed {
            bytes: bytes.to_vec(),
            encoding: CString::from(encoding),
        }));
    }
    // SAFETY: a fresh object; the caller is a `+value...` method, which returns
    // an autoreleased one.
    unsafe { corefoundation::api::CFAutorelease(object as corefoundation::base::CFTypeRef) as Id }
}

/// The contents of a boxed value.
///
/// # Safety
///
/// `receiver` must be null or an object this runtime allocated.
unsafe fn boxed<'a>(receiver: Id) -> Option<&'a Boxed> {
    let class = objc::class::lookup(c"NSValue")?;
    if !unsafe { objc::object::class_of(receiver) }
        .is_some_and(|c| c.chain().any(|k| std::ptr::eq(k, class)))
    {
        return None;
    }
    // SAFETY: an `NSValue` is a `Body` with a live `boxed`.
    unsafe {
        let body = receiver as *const Body;
        (!(*body).boxed.is_null()).then(|| &*(*body).boxed)
    }
}

/// Read a boxed value back as `T`, or the default if it is not one or is the
/// wrong size.
///
/// # Safety
///
/// As [`boxed`], and `T` must be plain data.
unsafe fn unbox<T: Default + Copy>(receiver: Id) -> T {
    let Some(boxed) = (unsafe { boxed(receiver) }) else {
        return T::default();
    };
    if boxed.bytes.len() != size_of::<T>() {
        return T::default();
    }
    let mut out = T::default();
    // SAFETY: the sizes match, and `T` is plain data.
    unsafe {
        std::ptr::copy_nonoverlapping(
            boxed.bytes.as_ptr(),
            &mut out as *mut T as *mut u8,
            size_of::<T>(),
        )
    };
    out
}

/// `T` as its bytes.
fn bytes_of<T: Copy>(value: &T) -> &[u8] {
    // SAFETY: `T` is plain data, and this borrows it for no longer than it lives.
    unsafe { std::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: reached with the last reference gone; the receiver is an `NSValue`.
    unsafe {
        let body = receiver as *mut Body;
        if !(*body).boxed.is_null() {
            drop(Box::from_raw((*body).boxed));
            (*body).boxed = std::ptr::null_mut();
        }
        objc::arc::finalize(receiver);
    }
}

unsafe extern "C" fn obj_c_type(receiver: Id, _sel: Sel) -> *const c_char {
    // SAFETY: an `NSValue`.
    unsafe { boxed(receiver) }.map_or(std::ptr::null(), |b| b.encoding.as_ptr())
}

unsafe extern "C" fn get_value(receiver: Id, _sel: Sel, out: *mut u8) {
    // SAFETY: an `NSValue`.
    let Some(b) = (unsafe { boxed(receiver) }) else {
        return;
    };
    if out.is_null() {
        return;
    }
    // SAFETY: the caller says `out` is at least as big as what was boxed, which
    // is the contract of `-getValue:` and why Apple deprecated it.
    unsafe { std::ptr::copy_nonoverlapping(b.bytes.as_ptr(), out, b.bytes.len()) };
}

unsafe extern "C" fn value_with_bytes(
    _class: Id,
    _sel: Sel,
    bytes: *const u8,
    encoding: *const c_char,
) -> Id {
    if bytes.is_null() || encoding.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a NUL-terminated encoding the caller supplied.
    let encoding = unsafe { CStr::from_ptr(encoding) };
    // The size is what the encoding says, and this runtime only knows the four
    // it makes itself. Anything else is refused by name rather than boxed at a
    // guessed width, which would read past the caller's value.
    let size = match encoding.to_bytes() {
        b if b == encoding::POINT.to_bytes() || b == encoding::SIZE.to_bytes() => 16,
        b if b == encoding::RECT.to_bytes() => 32,
        b if b == encoding::RANGE.to_bytes() => 16,
        _ => objc::fail!(
            "+[NSValue valueWithBytes:objCType:]: this runtime does not know the width of {}",
            encoding.to_string_lossy()
        ),
    };
    // SAFETY: the caller says `bytes` points at a value of this encoding.
    make(unsafe { std::slice::from_raw_parts(bytes, size) }, encoding)
}

unsafe extern "C" fn value_with_point(_class: Id, _sel: Sel, point: NSPoint) -> Id {
    make(bytes_of(&point), encoding::POINT)
}

unsafe extern "C" fn value_with_size(_class: Id, _sel: Sel, size: NSSize) -> Id {
    boxed_size(size)
}

/// An autoreleased `NSValue` holding `size`, for this tree's own dictionaries.
///
/// `-[NSScreen deviceDescription]` is the caller: its values are boxed sizes,
/// and a guest reads one back with `-sizeValue`.
pub fn boxed_size(size: NSSize) -> Id {
    make(bytes_of(&size), encoding::SIZE)
}

unsafe extern "C" fn value_with_rect(_class: Id, _sel: Sel, rect: NSRect) -> Id {
    make(bytes_of(&rect), encoding::RECT)
}

unsafe extern "C" fn value_with_range(_class: Id, _sel: Sel, range: NSRange) -> Id {
    make(bytes_of(&range), encoding::RANGE)
}

unsafe extern "C" fn point_value(receiver: Id, _sel: Sel) -> NSPoint {
    // SAFETY: an `NSValue` holding plain data.
    unsafe { unbox(receiver) }
}

unsafe extern "C" fn size_value(receiver: Id, _sel: Sel) -> NSSize {
    unsafe { unbox(receiver) }
}

unsafe extern "C" fn rect_value(receiver: Id, _sel: Sel) -> NSRect {
    unsafe { unbox(receiver) }
}

unsafe extern "C" fn range_value(receiver: Id, _sel: Sel) -> NSRange {
    unsafe { unbox(receiver) }
}

/// Box one of the four shapes, for [`crate::kvc`].
pub(crate) fn box_point(value: NSPoint) -> Id {
    make(bytes_of(&value), encoding::POINT)
}

pub(crate) fn box_size(value: NSSize) -> Id {
    make(bytes_of(&value), encoding::SIZE)
}

pub(crate) fn box_rect(value: NSRect) -> Id {
    make(bytes_of(&value), encoding::RECT)
}

pub(crate) fn box_range(value: NSRange) -> Id {
    make(bytes_of(&value), encoding::RANGE)
}

/// The bytes a boxed value holds, whatever shape it is.
///
/// # Safety
///
/// `value` must be null or an object this runtime allocated.
pub(crate) unsafe fn unbox_bytes(value: Id) -> Option<Vec<u8>> {
    unsafe { boxed(value) }.map(|b| b.bytes.clone())
}

pub(crate) fn install() {
    // Register the storage-bearing class before `value::install` adds the
    // pointer-value constructor. A class's size is fixed at registration, so a
    // prior abstract registration would leave no room for `boxed`.
    let class =
        corefoundation::base::register(c"NSValue", None, std::mem::size_of::<Body>(), false);
    methods!(class, {
        "objCType" => obj_c_type, c"*16@0:8";
        "getValue:" => get_value, c"v24@0:8^v16";
        "getValue:size:" => get_value, c"v32@0:8^v16Q24";
        "pointValue" => point_value, c"{CGPoint=dd}16@0:8";
        "sizeValue" => size_value, c"{CGSize=dd}16@0:8";
        "rectValue" => rect_value, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "rangeValue" => range_value, c"{_NSRange=QQ}16@0:8";
        "dealloc" => dealloc, c"v16@0:8"
    });
    class_methods!(class, {
        "valueWithBytes:objCType:" => value_with_bytes, c"@32@0:8^v16*24";
        "value:withObjCType:" => value_with_bytes, c"@32@0:8^v16*24";
        "valueWithPoint:" => value_with_point, c"@32@0:8{CGPoint=dd}16";
        "valueWithSize:" => value_with_size, c"@32@0:8{CGSize=dd}16";
        "valueWithRect:" => value_with_rect, c"@48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "valueWithRange:" => value_with_range, c"@32@0:8{_NSRange=QQ}16"
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_functions_are_exported_by_foundation_not_coregraphics() {
        for name in names() {
            assert!(name.starts_with("NS"), "{name}");
            assert!(symbol(name).is_some(), "{name}");
            assert_eq!(crate::symbol(name), symbol(name), "{name}");
            assert_eq!(coregraphics::symbol(name), None, "{name}");
        }
    }

    #[test]
    fn mouse_rect_vertical_edge_tracks_view_orientation() {
        let rect = NSRect {
            origin: NSPoint { x: 10.0, y: 20.0 },
            size: NSSize {
                width: 30.0,
                height: 40.0,
            },
        };

        // SAFETY: these calls pass the function's plain value arguments.
        unsafe {
            assert_eq!(NSMouseInRect(NSPoint { x: 10.0, y: 20.0 }, rect, 1), 1);
            assert_eq!(NSMouseInRect(NSPoint { x: 10.0, y: 60.0 }, rect, 1), 0);
            assert_eq!(NSMouseInRect(NSPoint { x: 10.0, y: 20.0 }, rect, 0), 0);
            assert_eq!(NSMouseInRect(NSPoint { x: 10.0, y: 60.0 }, rect, 0), 1);
            assert_eq!(NSMouseInRect(NSPoint { x: 40.0, y: 30.0 }, rect, 0), 0);

            let backwards = NSRect {
                origin: NSPoint { x: 40.0, y: 60.0 },
                size: NSSize {
                    width: -30.0,
                    height: -40.0,
                },
            };
            assert_eq!(NSMouseInRect(NSPoint { x: 10.0, y: 20.0 }, backwards, 1), 1);
            assert_eq!(NSMouseInRect(NSPoint { x: 10.0, y: 60.0 }, backwards, 0), 1);
        }
    }

    #[test]
    fn ns_rect_operations_preserve_negative_extents() {
        let rect = coregraphics::CGRect {
            origin: coregraphics::CGPoint { x: 30.0, y: 40.0 },
            size: coregraphics::CGSize {
                width: -10.0,
                height: -20.0,
            },
        };
        // SAFETY: these calls pass the functions' plain value arguments.
        unsafe {
            let inset: NSRect = NSInsetRect(rect, 2.0, 3.0);
            assert_eq!(inset.origin, NSPoint { x: 32.0, y: 43.0 });
            assert_eq!(
                inset.size,
                NSSize {
                    width: -14.0,
                    height: -26.0,
                }
            );
            let offset: coregraphics::CGRect = NSOffsetRect(rect, 2.0, 3.0);
            assert_eq!(offset.origin, inset.origin);
            assert_eq!(offset.size, rect.size);
            assert_eq!(NSIsEmptyRect(rect), 1);
            assert_eq!(NSIntegralRect(rect), NSRect::default());
        }
    }

    #[test]
    fn geometry_strings_round_trip() {
        crate::bootstrap();
        let pool = objc::arc::pool_push();
        let point = NSPoint { x: 1.5, y: -2.5 };
        let size = NSSize {
            width: 3.5,
            height: 4.5,
        };
        let rect = NSRect {
            origin: point,
            size,
        };
        // SAFETY: the strings are live NSStrings made by this module.
        unsafe {
            assert_eq!(NSPointFromString(NSStringFromPoint(point)), point);
            assert_eq!(NSSizeFromString(NSStringFromSize(size)), size);
            let string = NSStringFromRect(rect);
            assert_eq!(crate::string::bytes(string), b"{{1.5, -2.5}, {3.5, 4.5}}");
            assert_eq!(NSRectFromString(string), rect);
            assert_eq!(
                crate::string::bytes(NSStringFromRange(NSRange {
                    location: 8,
                    length: 9,
                })),
                b"{8, 9}"
            );
            assert_eq!(NSRectFromString(std::ptr::null_mut()), NSRect::default());
            objc::arc::pool_pop(pool);
        }
    }

    #[test]
    fn range_intersection_is_zero_when_disjoint() {
        let range = NSRange {
            location: 8,
            length: 9,
        };
        // SAFETY: these calls pass ranges by value.
        unsafe {
            assert_eq!(
                NSIntersectionRange(
                    range,
                    NSRange {
                        location: 10,
                        length: 2
                    }
                ),
                NSRange {
                    location: 10,
                    length: 2
                }
            );
            assert_eq!(
                NSIntersectionRange(
                    range,
                    NSRange {
                        location: 17,
                        length: 2
                    }
                ),
                NSRange::default()
            );
        }
    }

    #[test]
    fn nsvalue_has_room_for_its_boxed_payload() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSValue").expect("registered");
        assert!(class.instance_size() >= size_of::<Body>());
    }

    /// Each of the three shapes survives being boxed and read back.
    ///
    /// Called directly rather than through `objc_msgSend`: what a *guest* send
    /// does to these is `rosemary`'s marshaller's business and is covered by
    /// `tests/msgsend.rs`. What this pins is that the storage is the right width
    /// and the encoding is the right string.
    #[test]
    fn the_three_aggregates_round_trip() {
        crate::bootstrap();
        // SAFETY: real class objects, and each prototype is the one declared
        // above.
        unsafe {
            let class = objc::class::ptr(objc::class::lookup(c"NSValue").unwrap()) as Id;
            let sel = objc::sel::get("x");

            let rect = NSRect {
                origin: NSPoint { x: 1.5, y: 2.5 },
                size: NSSize {
                    width: 3.5,
                    height: 4.5,
                },
            };
            let boxed = value_with_rect(class, sel, rect);
            assert_eq!(rect_value(boxed, sel), rect);
            assert_eq!(
                CStr::from_ptr(obj_c_type(boxed, sel)),
                encoding::RECT,
                "a guest switches on this string"
            );

            let point = NSPoint { x: 6.5, y: 7.5 };
            assert_eq!(point_value(value_with_point(class, sel, point), sel), point);

            let range = NSRange {
                location: 8,
                length: 9,
            };
            assert_eq!(range_value(value_with_range(class, sel, range), sel), range);

            // A value read back as the wrong shape is zero rather than the
            // neighbouring bytes: the widths disagree, and answering with
            // whatever happened to be there is the failure mode this checks for.
            assert_eq!(point_value(boxed, sel), NSPoint::default());
        }
    }
}
