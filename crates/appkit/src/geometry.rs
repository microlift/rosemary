use foundation::geometry::{CGFloat, NSPoint, NSRect, NSSize};

/// The height of the primary display, which is the origin AppKit measures from.
///
/// Zero when there is no display - a headless host, or SDL absent - and a zero
/// here makes the conversion the identity, which is the only answer that cannot
/// be mistaken for a real position.
pub fn primary_height() -> f64 {
    sdl3::video::display_bounds(sdl3::video::primary_display()).map_or(0.0, |b| b.h as f64)
}

/// An SDL rectangle, top-left and y down, as an AppKit one.
pub fn rect_from_sdl(x: i32, y: i32, width: i32, height: i32) -> NSRect {
    NSRect {
        origin: NSPoint {
            x: x as f64,
            y: primary_height() - (y as f64) - (height as f64),
        },
        size: NSSize {
            width: width as f64,
            height: height as f64,
        },
    }
}

/// The reverse: an AppKit rectangle as SDL's `(x, y, width, height)`.
///
/// The same expression as [`rect_from_sdl`], which is not a coincidence - the
/// transform is its own inverse - and is written out rather than shared so that
/// each direction reads as what it does.
pub fn rect_to_sdl(rect: NSRect) -> (i32, i32, i32, i32) {
    let y = primary_height() - rect.origin.y - rect.size.height;
    (
        rect.origin.x as i32,
        y as i32,
        rect.size.width as i32,
        rect.size.height as i32,
    )
}

/// A desktop point as an AppKit screen point.
pub fn point_from_sdl(x: f64, y: f64) -> NSPoint {
    NSPoint {
        x,
        y: primary_height() - y,
    }
}

/// A point inside a window, from SDL's top-left frame to AppKit's bottom-left.
///
/// `height` is the window's, in points: this is the conversion `-locationInWindow`
/// needs, and it is relative to the window rather than to the screen.
pub fn point_in_window(x: f64, y: f64, height: f64) -> NSPoint {
    NSPoint { x, y: height - y }
}

/// A rectangle with a non-negative width and height, which is what every
/// `CGRectGet...` accessor answers about.
///
/// CoreGraphics allows a negative extent and standardizes before measuring, so
/// `CGRectGetMinX` of a rectangle that runs leftwards is its *right* edge.
fn standardized(rect: NSRect) -> (f64, f64, f64, f64) {
    let (x, width) = match rect.size.width < 0.0 {
        true => (rect.origin.x + rect.size.width, -rect.size.width),
        false => (rect.origin.x, rect.size.width),
    };
    let (y, height) = match rect.size.height < 0.0 {
        true => (rect.origin.y + rect.size.height, -rect.size.height),
        false => (rect.origin.y, rect.size.height),
    };
    (x, y, width, height)
}

/// Whether `point` is inside `rect`, half-open on the far edges as
/// `CGRectContainsPoint` is.
pub fn contains(rect: NSRect, point: NSPoint) -> bool {
    let (x, y, width, height) = standardized(rect);
    point.x >= x && point.x < x + width && point.y >= y && point.y < y + height
}

/// `void NSDisableScreenUpdates(void)` and `NSEnableScreenUpdates`: they hold
/// back the window server's flushes so several windows change at once. Each
/// window here presents on its own, so there is nothing to hold back.
#[allow(non_snake_case)]
pub extern "C" fn NSDisableScreenUpdates() {}

#[allow(non_snake_case)]
pub extern "C" fn NSEnableScreenUpdates() {}

/// `bool CGRectEqualToRect(CGRect, CGRect)`, compared standardized.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGRectEqualToRect(a: NSRect, b: NSRect) -> u8 {
    u8::from(standardized(a) == standardized(b))
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn NSIsEmptyRect(rect: NSRect) -> u8 {
    u8::from(rect.size.width <= 0.0 || rect.size.height <= 0.0)
}

#[allow(non_snake_case)]
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

/// `BOOL NSPointInRect(NSPoint, NSRect)`, which AppKit defines as
/// `NSMouseInRect` with a flipped rectangle.
#[allow(non_snake_case)]
pub unsafe extern "C" fn NSPointInRect(point: NSPoint, rect: NSRect) -> u8 {
    mouse_in_rect(point, rect, true) as u8
}

/// `BOOL NSMouseInRect(NSPoint, NSRect, BOOL)`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn NSMouseInRect(point: NSPoint, rect: NSRect, flipped: u8) -> u8 {
    mouse_in_rect(point, rect, flipped != 0) as u8
}

/// Whether a mouse region holds `point`, on `NSMouseInRect`'s rule.
///
/// Cocoa assigns a point on a shared edge to only one adjacent rectangle: the
/// visual top and left edges are inside, the bottom and right ones are not, so
/// an unflipped rectangle owns its maximum-y edge. Unlike `CGRectContainsPoint`,
/// a negative extent is not standardized and holds nothing.
pub fn mouse_in_rect(point: NSPoint, rect: NSRect, flipped: bool) -> bool {
    let (min_x, max_x) = (rect.origin.x, rect.origin.x + rect.size.width);
    let (min_y, max_y) = (rect.origin.y, rect.origin.y + rect.size.height);
    let horizontal = point.x >= min_x && point.x < max_x;
    let vertical = match flipped {
        true => point.y >= min_y && point.y < max_y,
        false => point.y > min_y && point.y <= max_y,
    };
    horizontal && vertical
}

/// `NSRect NSInsetRect(NSRect, CGFloat, CGFloat)`: the rectangle shrunk by `dx`
/// on each side and `dy` on each end. A negative amount grows it, which is how
/// callers use it to make a margin.
#[allow(non_snake_case)]
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

/// `NSRect NSIntersectionRect(NSRect, NSRect)`, which is also
/// `CGRectIntersection` for the empty case's purposes: no overlap is the zero
/// rectangle.
#[allow(non_snake_case)]
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
#[allow(non_snake_case)]
pub unsafe extern "C" fn NSOffsetRect(rect: NSRect, dx: CGFloat, dy: CGFloat) -> NSRect {
    NSRect {
        origin: NSPoint {
            x: rect.origin.x + dx,
            y: rect.origin.y + dy,
        },
        size: rect.size,
    }
}

/// [`standardized`] as a rectangle again.
fn standard_rect(rect: NSRect) -> NSRect {
    let (x, y, width, height) = standardized(rect);
    NSRect {
        origin: NSPoint { x, y },
        size: NSSize { width, height },
    }
}

/// `CGRect CGRectInset(CGRect, CGFloat, CGFloat)`: standardized first, and
/// the null rectangle when the inset swallows it.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGRectInset(rect: NSRect, dx: CGFloat, dy: CGFloat) -> NSRect {
    let rect = standard_rect(rect);
    let out = unsafe { NSInsetRect(rect, dx, dy) };
    if out.size.width < 0.0 || out.size.height < 0.0 {
        return CG_RECT_NULL;
    }
    out
}

/// `CGRect CGRectOffset(CGRect, CGFloat, CGFloat)`
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGRectOffset(rect: NSRect, dx: CGFloat, dy: CGFloat) -> NSRect {
    unsafe { NSOffsetRect(standard_rect(rect), dx, dy) }
}

/// `CGRect CGRectIntegral(CGRect)`: the smallest integral rectangle that
/// contains it.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGRectIntegral(rect: NSRect) -> NSRect {
    if rect.origin.x.is_infinite() || rect.origin.y.is_infinite() {
        return rect;
    }
    let rect = standard_rect(rect);
    let (x, y) = (rect.origin.x.floor(), rect.origin.y.floor());
    let right = (rect.origin.x + rect.size.width).ceil();
    let top = (rect.origin.y + rect.size.height).ceil();
    NSRect {
        origin: NSPoint { x, y },
        size: NSSize {
            width: right - x,
            height: top - y,
        },
    }
}

/// `bool CGRectIsEmpty(CGRect)`: zero area, or the null rectangle.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGRectIsEmpty(rect: NSRect) -> u8 {
    u8::from(
        rect.size.width == 0.0
            || rect.size.height == 0.0
            || rect.origin.x.is_infinite()
            || rect.origin.y.is_infinite(),
    )
}

/// `CGRectNull`: an infinite origin and a zero size.
const CG_RECT_NULL: NSRect = NSRect {
    origin: NSPoint {
        x: f64::INFINITY,
        y: f64::INFINITY,
    },
    size: NSSize {
        width: 0.0,
        height: 0.0,
    },
};

/// The eight `CGFloat CGRectGet...(CGRect)` accessors.
///
/// Inline functions in the header on a Mac *and* exported symbols, and a guest
/// compiled against one SDK imports what another inlined - so these are real
/// imports for some programs and absent from others. A `CGRect` is four
/// doubles, which is four vector registers on this ABI rather than a pointer.
macro_rules! accessors {
    ($($name:ident => $measure:expr;)*) => {
        $(
            #[allow(non_snake_case)]
            pub unsafe extern "C" fn $name(rect: NSRect) -> f64 {
                let measure: fn(f64, f64, f64, f64) -> f64 = $measure;
                let (x, y, width, height) = standardized(rect);
                measure(x, y, width, height)
            }
        )*
    };
}

accessors! {
    CGRectGetWidth => |_x, _y, width, _height| width;
    CGRectGetHeight => |_x, _y, _width, height| height;
    CGRectGetMinX => |x, _y, _width, _height| x;
    CGRectGetMidX => |x, _y, width, _height| x + width / 2.0;
    CGRectGetMaxX => |x, _y, width, _height| x + width;
    CGRectGetMinY => |_x, y, _width, _height| y;
    CGRectGetMidY => |_x, y, _width, height| y + height / 2.0;
    CGRectGetMaxY => |_x, y, _width, height| y + height;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The accessors standardize first, which is what makes a rectangle with a
    /// negative extent measure the same as the one it describes.
    #[test]
    fn a_rectangle_that_runs_backwards_measures_as_the_one_it_describes() {
        let backwards = NSRect {
            origin: NSPoint { x: 30.0, y: 40.0 },
            size: NSSize {
                width: -10.0,
                height: -20.0,
            },
        };
        // SAFETY: each takes a `CGRect` by value and returns a `CGFloat`.
        unsafe {
            assert_eq!(CGRectGetWidth(backwards), 10.0);
            assert_eq!(CGRectGetHeight(backwards), 20.0);
            assert_eq!(CGRectGetMinX(backwards), 20.0);
            assert_eq!(CGRectGetMaxX(backwards), 30.0);
            assert_eq!(CGRectGetMidY(backwards), 30.0);
            assert_eq!(CGRectGetMaxY(backwards), 40.0);
        }
    }

    /// The conversion is its own inverse, whatever the display height is -
    /// which is the property that makes a round trip through SDL and back leave
    /// a window where it started.
    #[test]
    fn a_rectangle_survives_the_round_trip() {
        let rect = NSRect {
            origin: NSPoint { x: 100.0, y: 200.0 },
            size: NSSize {
                width: 640.0,
                height: 480.0,
            },
        };
        let (x, y, w, h) = rect_to_sdl(rect);
        assert_eq!(rect_from_sdl(x, y, w, h), rect);
    }

    /// And a point at the bottom of the screen is at the top of SDL's.
    #[test]
    fn the_origin_is_the_other_corner() {
        let height = primary_height();
        assert_eq!(point_from_sdl(0.0, height), NSPoint { x: 0.0, y: 0.0 });
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
        }
    }

    #[test]
    fn mouse_rects_with_a_negative_extent_hold_nothing() {
        let middle = NSPoint { x: 100.0, y: 50.0 };
        let rects = [
            (200.0, 100.0, -200.0, -100.0),
            (200.0, 0.0, -200.0, 100.0),
            (0.0, 100.0, 200.0, -100.0),
        ];
        for (x, y, width, height) in rects {
            let rect = NSRect {
                origin: NSPoint { x, y },
                size: NSSize { width, height },
            };
            // SAFETY: these calls pass the functions' plain value arguments.
            unsafe {
                assert_eq!(NSMouseInRect(middle, rect, 0), 0);
                assert_eq!(NSMouseInRect(middle, rect, 1), 0);
                assert_eq!(NSPointInRect(middle, rect), 0);
                assert_eq!(CGRectContainsPoint(rect, middle), 1);
            }
        }
    }
}

/// `CGRectNull`, which `CGRectIntersection` answers for no overlap.
const NULL_RECT: NSRect = NSRect {
    origin: NSPoint {
        x: f64::INFINITY,
        y: f64::INFINITY,
    },
    size: NSSize {
        width: 0.0,
        height: 0.0,
    },
};

/// `CGRect CGRectIntersection(CGRect, CGRect)`: standardized, and the null
/// rectangle rather than the zero one when they do not meet.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGRectIntersection(a: NSRect, b: NSRect) -> NSRect {
    let (a, b) = (standard_rect(a), standard_rect(b));
    let x = a.origin.x.max(b.origin.x);
    let y = a.origin.y.max(b.origin.y);
    let right = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let top = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    if right < x || top < y {
        return NULL_RECT;
    }
    NSRect {
        origin: NSPoint { x, y },
        size: NSSize {
            width: right - x,
            height: top - y,
        },
    }
}

/// `bool CGRectContainsPoint(CGRect, CGPoint)`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGRectContainsPoint(rect: NSRect, point: NSPoint) -> u8 {
    contains(rect, point) as u8
}

/// `bool CGRectContainsRect(CGRect, CGRect)`: `b` lies wholly inside `a`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGRectContainsRect(a: NSRect, b: NSRect) -> u8 {
    let (ax, ay, aw, ah) = standardized(a);
    let (bx, by, bw, bh) = standardized(b);
    (bx >= ax && by >= ay && bx + bw <= ax + aw && by + bh <= ay + ah) as u8
}

/// `BOOL NSContainsRect(NSRect a, NSRect b)`: AppKit's, which refuses an
/// empty `b`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn NSContainsRect(a: NSRect, b: NSRect) -> u8 {
    let empty = unsafe { NSIsEmptyRect(b) } != 0;
    (!empty
        && a.origin.x <= b.origin.x
        && a.origin.y <= b.origin.y
        && a.origin.x + a.size.width >= b.origin.x + b.size.width
        && a.origin.y + a.size.height >= b.origin.y + b.size.height) as u8
}

/// `BOOL NSIntersectsRect(NSRect, NSRect)`.
#[allow(non_snake_case)]
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
#[allow(non_snake_case)]
pub unsafe extern "C" fn NSIntegralRect(rect: NSRect) -> NSRect {
    if unsafe { NSIsEmptyRect(rect) } != 0 {
        return NSRect::default();
    }
    unsafe { CGRectIntegral(rect) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn NSEqualPoints(a: NSPoint, b: NSPoint) -> u8 {
    (a.x == b.x && a.y == b.y) as u8
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn NSEqualSizes(a: NSSize, b: NSSize) -> u8 {
    (a.width == b.width && a.height == b.height) as u8
}

/// `NSRange NSIntersectionRange(NSRange, NSRange)`: the zero range when they
/// do not overlap.
#[allow(non_snake_case)]
pub unsafe extern "C" fn NSIntersectionRange(
    a: foundation::geometry::NSRange,
    b: foundation::geometry::NSRange,
) -> foundation::geometry::NSRange {
    let start = a.location.max(b.location);
    let end = (a.location + a.length).min(b.location + b.length);
    match end > start {
        true => foundation::geometry::NSRange {
            location: start,
            length: end - start,
        },
        false => foundation::geometry::NSRange::default(),
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

fn autoreleased_string(text: &str) -> objc::object::Id {
    objc::arc::autorelease(foundation::string::from_str(text))
}

/// The numbers in an `NSString`, in order, as Foundation's parsers scan them:
/// anything that is not part of a number separates.
unsafe fn numbers(string: objc::object::Id) -> Vec<f64> {
    if string.is_null() {
        return Vec::new();
    }
    // SAFETY: an NSString, by the parsers' prototypes.
    let text = String::from_utf8_lossy(&unsafe { foundation::string::bytes(string) }).into_owned();
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

#[allow(non_snake_case)]
pub unsafe extern "C" fn NSStringFromPoint(point: NSPoint) -> objc::object::Id {
    autoreleased_string(&format!("{{{}, {}}}", g(point.x), g(point.y)))
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn NSStringFromSize(size: NSSize) -> objc::object::Id {
    autoreleased_string(&format!("{{{}, {}}}", g(size.width), g(size.height)))
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn NSStringFromRect(rect: NSRect) -> objc::object::Id {
    autoreleased_string(&format!(
        "{{{{{}, {}}}, {{{}, {}}}}}",
        g(rect.origin.x),
        g(rect.origin.y),
        g(rect.size.width),
        g(rect.size.height)
    ))
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn NSStringFromRange(
    range: foundation::geometry::NSRange,
) -> objc::object::Id {
    autoreleased_string(&format!("{{{}, {}}}", range.location, range.length))
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn NSPointFromString(string: objc::object::Id) -> NSPoint {
    let n = unsafe { numbers(string) };
    let at = |i: usize| n.get(i).copied().unwrap_or(0.0);
    NSPoint { x: at(0), y: at(1) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn NSSizeFromString(string: objc::object::Id) -> NSSize {
    let n = unsafe { numbers(string) };
    let at = |i: usize| n.get(i).copied().unwrap_or(0.0);
    NSSize {
        width: at(0),
        height: at(1),
    }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn NSRectFromString(string: objc::object::Id) -> NSRect {
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
