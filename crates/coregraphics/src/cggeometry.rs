//! Rectangle and point geometry: the `CGRect` accessors and the conversions
//! between CoreGraphics' bottom-left space and SDL's top-left one.

#![allow(non_snake_case)]

use crate::{CGFloat, CGPoint, CGRect, CGSize};

/// The height of the primary display, which is the origin AppKit measures from.
///
/// Zero when there is no display - a headless host, or SDL absent - and a zero
/// here makes the conversion the identity, which is the only answer that cannot
/// be mistaken for a real position.
pub fn primary_height() -> f64 {
    sdl3::video::display_bounds(sdl3::video::primary_display()).map_or(0.0, |b| b.h as f64)
}

/// An SDL rectangle, top-left and y down, as a CoreGraphics one.
pub fn rect_from_sdl(x: i32, y: i32, width: i32, height: i32) -> CGRect {
    CGRect {
        origin: CGPoint {
            x: x as f64,
            y: primary_height() - (y as f64) - (height as f64),
        },
        size: CGSize {
            width: width as f64,
            height: height as f64,
        },
    }
}

/// The reverse: a CoreGraphics rectangle as SDL's `(x, y, width, height)`.
///
/// The same expression as [`rect_from_sdl`], which is not a coincidence - the
/// transform is its own inverse - and is written out rather than shared so that
/// each direction reads as what it does.
pub fn rect_to_sdl(rect: CGRect) -> (i32, i32, i32, i32) {
    let y = primary_height() - rect.origin.y - rect.size.height;
    (
        rect.origin.x as i32,
        y as i32,
        rect.size.width as i32,
        rect.size.height as i32,
    )
}

/// A desktop point as a CoreGraphics screen point.
pub fn point_from_sdl(x: f64, y: f64) -> CGPoint {
    CGPoint {
        x,
        y: primary_height() - y,
    }
}

/// A point inside a window, from SDL's top-left frame to CoreGraphics'
/// bottom-left.
///
/// `height` is the window's, in points: this is the conversion
/// `-locationInWindow` needs, and it is relative to the window rather than to
/// the screen.
pub fn point_in_window(x: f64, y: f64, height: f64) -> CGPoint {
    CGPoint { x, y: height - y }
}

/// A rectangle with a non-negative width and height, which is what every
/// `CGRectGet...` accessor answers about.
///
/// CoreGraphics allows a negative extent and standardizes before measuring, so
/// `CGRectGetMinX` of a rectangle that runs leftwards is its *right* edge.
fn standardized(rect: CGRect) -> (f64, f64, f64, f64) {
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
pub fn contains(rect: CGRect, point: CGPoint) -> bool {
    let (x, y, width, height) = standardized(rect);
    point.x >= x && point.x < x + width && point.y >= y && point.y < y + height
}

/// `bool CGRectEqualToRect(CGRect, CGRect)`, compared standardized.
pub unsafe extern "C" fn CGRectEqualToRect(a: CGRect, b: CGRect) -> u8 {
    u8::from(standardized(a) == standardized(b))
}

/// [`standardized`] as a rectangle again.
fn standard_rect(rect: CGRect) -> CGRect {
    let (x, y, width, height) = standardized(rect);
    CGRect {
        origin: CGPoint { x, y },
        size: CGSize { width, height },
    }
}

/// `CGRect CGRectInset(CGRect, CGFloat, CGFloat)`: standardized first, and
/// the null rectangle when the inset swallows it.
pub unsafe extern "C" fn CGRectInset(rect: CGRect, dx: CGFloat, dy: CGFloat) -> CGRect {
    let rect = standard_rect(rect);
    let out = CGRect {
        origin: CGPoint {
            x: rect.origin.x + dx,
            y: rect.origin.y + dy,
        },
        size: CGSize {
            width: rect.size.width - 2.0 * dx,
            height: rect.size.height - 2.0 * dy,
        },
    };
    if out.size.width < 0.0 || out.size.height < 0.0 {
        return CG_RECT_NULL;
    }
    out
}

/// `CGRect CGRectOffset(CGRect, CGFloat, CGFloat)`
pub unsafe extern "C" fn CGRectOffset(rect: CGRect, dx: CGFloat, dy: CGFloat) -> CGRect {
    let rect = standard_rect(rect);
    CGRect {
        origin: CGPoint {
            x: rect.origin.x + dx,
            y: rect.origin.y + dy,
        },
        size: rect.size,
    }
}

/// `CGRect CGRectIntegral(CGRect)`: the smallest integral rectangle that
/// contains it.
pub unsafe extern "C" fn CGRectIntegral(rect: CGRect) -> CGRect {
    if rect.origin.x.is_infinite() || rect.origin.y.is_infinite() {
        return rect;
    }
    let rect = standard_rect(rect);
    let (x, y) = (rect.origin.x.floor(), rect.origin.y.floor());
    let right = (rect.origin.x + rect.size.width).ceil();
    let top = (rect.origin.y + rect.size.height).ceil();
    CGRect {
        origin: CGPoint { x, y },
        size: CGSize {
            width: right - x,
            height: top - y,
        },
    }
}

/// `bool CGRectIsEmpty(CGRect)`: zero area, or the null rectangle.
pub unsafe extern "C" fn CGRectIsEmpty(rect: CGRect) -> u8 {
    u8::from(
        rect.size.width == 0.0
            || rect.size.height == 0.0
            || rect.origin.x.is_infinite()
            || rect.origin.y.is_infinite(),
    )
}

/// `CGRectNull`: an infinite origin and a zero size.
const CG_RECT_NULL: CGRect = CGRect {
    origin: CGPoint {
        x: f64::INFINITY,
        y: f64::INFINITY,
    },
    size: CGSize {
        width: 0.0,
        height: 0.0,
    },
};

/// `CGRect CGRectIntersection(CGRect, CGRect)`: standardized, and the null
/// rectangle rather than the zero one when they do not meet.

pub unsafe extern "C" fn CGRectIntersection(a: CGRect, b: CGRect) -> CGRect {
    let (a, b) = (standard_rect(a), standard_rect(b));
    let x = a.origin.x.max(b.origin.x);
    let y = a.origin.y.max(b.origin.y);
    let right = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let top = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    if right < x || top < y {
        return CG_RECT_NULL;
    }
    CGRect {
        origin: CGPoint { x, y },
        size: CGSize {
            width: right - x,
            height: top - y,
        },
    }
}

/// `bool CGRectContainsPoint(CGRect, CGPoint)`.

pub unsafe extern "C" fn CGRectContainsPoint(rect: CGRect, point: CGPoint) -> u8 {
    contains(rect, point) as u8
}

/// `bool CGRectContainsRect(CGRect, CGRect)`: `b` lies wholly inside `a`.

pub unsafe extern "C" fn CGRectContainsRect(a: CGRect, b: CGRect) -> u8 {
    let (ax, ay, aw, ah) = standardized(a);
    let (bx, by, bw, bh) = standardized(b);
    (bx >= ax && by >= ay && bx + bw <= ax + aw && by + bh <= ay + ah) as u8
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
            pub unsafe extern "C" fn $name(rect: CGRect) -> f64 {
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

exports::exports! {
    functions {
        "CGRectEqualToRect" => CGRectEqualToRect,
        "CGRectInset" => CGRectInset,
        "CGRectOffset" => CGRectOffset,
        "CGRectIntegral" => CGRectIntegral,
        "CGRectIsEmpty" => CGRectIsEmpty,
        "CGRectIntersection" => CGRectIntersection,
        "CGRectContainsPoint" => CGRectContainsPoint,
        "CGRectContainsRect" => CGRectContainsRect,
        "CGRectGetWidth" => CGRectGetWidth,
        "CGRectGetHeight" => CGRectGetHeight,
        "CGRectGetMinX" => CGRectGetMinX,
        "CGRectGetMidX" => CGRectGetMidX,
        "CGRectGetMaxX" => CGRectGetMaxX,
        "CGRectGetMinY" => CGRectGetMinY,
        "CGRectGetMidY" => CGRectGetMidY,
        "CGRectGetMaxY" => CGRectGetMaxY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The accessors standardize first, which is what makes a rectangle with a
    /// negative extent measure the same as the one it describes.
    #[test]
    fn a_rectangle_that_runs_backwards_measures_as_the_one_it_describes() {
        let backwards = CGRect {
            origin: CGPoint { x: 30.0, y: 40.0 },
            size: CGSize {
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

    #[test]
    fn inset_and_offset_standardize_negative_extents() {
        let rect = CGRect {
            origin: CGPoint { x: 30.0, y: 40.0 },
            size: CGSize {
                width: -10.0,
                height: -20.0,
            },
        };
        // SAFETY: these calls pass the functions' plain value arguments.
        unsafe {
            assert_eq!(
                CGRectInset(rect, 2.0, 3.0),
                CGRect {
                    origin: CGPoint { x: 22.0, y: 23.0 },
                    size: CGSize {
                        width: 6.0,
                        height: 14.0,
                    },
                }
            );
            assert_eq!(
                CGRectOffset(rect, 2.0, 3.0),
                CGRect {
                    origin: CGPoint { x: 22.0, y: 23.0 },
                    size: CGSize {
                        width: 10.0,
                        height: 20.0,
                    },
                }
            );
            assert_eq!(CGRectInset(rect, 6.0, 0.0), CG_RECT_NULL);
        }
    }

    /// The conversion is its own inverse, whatever the display height is -
    /// which is the property that makes a round trip through SDL and back leave
    /// a window where it started.
    #[test]
    fn a_rectangle_survives_the_round_trip() {
        let rect = CGRect {
            origin: CGPoint { x: 100.0, y: 200.0 },
            size: CGSize {
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
        assert_eq!(point_from_sdl(0.0, height), CGPoint { x: 0.0, y: 0.0 });
    }
}
