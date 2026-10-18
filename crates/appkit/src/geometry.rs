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

/// `BOOL NSPointInRect(NSPoint, NSRect)`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn NSPointInRect(point: NSPoint, rect: NSRect) -> u8 {
    contains(rect, point) as u8
}

/// `BOOL NSMouseInRect(NSPoint, NSRect, BOOL)`.
///
/// Cocoa assigns a point on a shared edge to only one adjacent rectangle. The
/// horizontal interval is always half-open. The vertical interval follows the
/// view orientation: a flipped view owns its minimum-y edge, while an
/// unflipped view owns its maximum-y edge.
#[allow(non_snake_case)]
pub unsafe extern "C" fn NSMouseInRect(point: NSPoint, rect: NSRect, flipped: u8) -> u8 {
    let (x, y, width, height) = standardized(rect);
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
}
