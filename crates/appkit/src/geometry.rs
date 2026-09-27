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
/// `CGRectGet…` accessor answers about.
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

/// `bool NSEqualRects(NSRect, NSRect)`, compared field by field.
///
/// The rows that used to reach this function were `CGRectEqualToRect` and
/// `NSEqualRects`, and it compared **standardized**, so `{0,0,10,10}` equalled
/// `{10,10,-10,-10}`. Neither CoreGraphics nor Foundation says that. Apple's
/// documentation for `CGRectEqualToRect` is "true if the two specified
/// rectangles have equal size and origin values", and it deprecates the
/// function in favour of `==` on `CGRect`; `origin` and `size` are the stored
/// fields. Standardization belongs to the accessors - `CGRectGetMinX` of a
/// rectangle running leftwards is its right edge - and to [`contains`], which
/// CoreGraphics does normalize. Equality is not one of them, and a negative
/// extent is a rectangle, not an error.
///
/// Only the `NSEqualRects` row still reaches this; `CGRectEqualToRect` is
/// `darwin::cg_unavailable`'s, a separate provider that sits ahead of this one
/// in the loader's chain. The comment on that row in `lib.rs` is the long
/// version.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGRectEqualToRect(a: NSRect, b: NSRect) -> u8 {
    u8::from(
        a.origin.x == b.origin.x
            && a.origin.y == b.origin.y
            && a.size.width == b.size.width
            && a.size.height == b.size.height,
    )
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

/// `BOOL NSContainsRect(NSRect rect1, NSRect rect2)`: whether `rect1` lies wholly
/// inside `rect2`.
///
/// A rectangle contains itself, and the answer is `false` for an empty one - the
/// empty rectangle's own extent admits no point, not even its own origin, because
/// the test is half-open on the far edges. Both rectangles are standardized first,
/// so one that runs backwards is measured as the rectangle it describes.
///
/// The two arguments go **on the stack**, not in vector registers. Verified at
/// `0x3a8e170`: the caller writes the first rectangle at `[rsp]`/`[rsp+0x10]` and
/// the second at `[rsp+0x20]`/`[rsp+0x30]`, then calls. A 32-byte aggregate is
/// `MEMORY` class on this ABI, and `NSRect` is a `#[repr(C)]` pair of pairs of
/// doubles, so an `extern "C"` declaration of exactly this shape gets it right
/// without a shim.
#[allow(non_snake_case)]
pub unsafe extern "C" fn NSContainsRect(rect1: NSRect, rect2: NSRect) -> u8 {
    let outer = standardized(rect2);
    let inner = standardized(rect1);
    let width = inner.2;
    let height = inner.3;
    if width <= 0.0 || height <= 0.0 {
        return 0;
    }
    let inside = inner.0 >= outer.0
        && inner.1 >= outer.1
        && inner.0 + width <= outer.0 + outer.2
        && inner.1 + height <= outer.1 + outer.3;
    inside as u8
}

/// `BOOL NSIntersectsRect(NSRect rect1, NSRect rect2)`: whether the two share any
/// point at all.
///
/// Touching edges do not count: the test is the same half-open interval
/// [`contains`] uses, so two rectangles that meet along an edge are reported as
/// disjoint - which is the answer `NSIntersection` gives, since their
/// intersection is empty.
///
/// **A rectangle with no extent intersects nothing**, including itself. This is
/// the one case the interval rule alone does not decide - a zero-width rectangle
/// is a line, and a line can be inside another rectangle - and it is decided here
/// rather than left to the arithmetic for two reasons. First, consistency:
/// [`NSContainsRect`] above already answers "no" for a degenerate rectangle,
/// because the test is half-open on the far edges and an empty rectangle admits
/// no point, and a pair of rectangles that cannot contain each other but are
/// reported as overlapping is not a shape anything downstream can use. Second, and
/// load-bearing: both of the guest's call sites read the answer as "then reuse
/// this, otherwise build a new one" -
///
/// ```text
/// 8b6ed8f  call _NSIntersectsRect
/// 8b6ed94  test al, al
/// 8b6ed96  je   0x8b6edcf           ; no: build
/// 8b6ed98  ...                      ; yes: reuse
/// ```
///
/// - so "no" costs the guest a redundant object and "yes" costs it a wrong one.
/// The arithmetic error is the one that cannot be undone.
///
/// The arguments are on the stack at `[rsp]` and `[rsp+0x10]`, as for
/// [`NSContainsRect`]; verified at `0x8b6ed8f`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn NSIntersectsRect(rect1: NSRect, rect2: NSRect) -> u8 {
    let a = standardized(rect1);
    let b = standardized(rect2);
    if a.2 <= 0.0 || a.3 <= 0.0 || b.2 <= 0.0 || b.3 <= 0.0 {
        return 0;
    }
    let overlaps = a.0 < b.0 + b.2
        && b.0 < a.0 + a.2
        && a.1 < b.1 + b.3
        && b.1 < a.1 + a.3;
    overlaps as u8
}

/// `BOOL NSEqualSizes(NSSize, NSSize)`: whether the two extents are the same pair
/// of doubles.
///
/// Compared as bits rather than by value, because a `CGFloat` is a `double` and
/// `0.0` and `-0.0` are the same extent. The two sizes go in four vector
/// registers - `xmm0`/`xmm1` for the first, `xmm2`/`xmm3` for the second - which
/// is what a `#[repr(C)]` pair of doubles gets on this ABI; verified at
/// `0x8b6e584` and `0xbc8ba41`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn NSEqualSizes(a: NSSize, b: NSSize) -> u8 {
    u8::from(a.width == b.width && a.height == b.height)
}

/// `NSRect NSOffsetRect(NSRect rect, CGFloat dx, CGFloat dy)`: the same rectangle
/// moved by `dx` and `dy`, its extent untouched.
///
/// The extent really is untouched - a negative amount does not shrink anything,
/// which is what distinguishes this from [`NSInsetRect`] and is why a caller
/// padding a frame reaches for the other one.
///
/// **The return goes through a hidden pointer.** Verified at both call sites,
/// `0x81633f2` and `0x8541db0`: the rectangle goes on the stack at `[rsp]` and
/// `[rsp+0x10]`, `dx` and `dy` arrive in `xmm0` and `xmm1`, `lea rdi, [rbp-…]`
/// supplies the destination, and the caller reads the answer back as 32 bytes out
/// of that frame slot. A 32-byte aggregate is returned in memory on this ABI, so
/// an `extern "C"` declaration returning `NSRect` gets the same shape - and
/// because there is no pointer parameter after the hidden one, nothing shifts.
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

/// `NSString *NSStringFromPoint(NSPoint point)` - refused, and answering nil.
///
/// The function's whole result is *the textual form of a point*, and that form is
/// a format string inside AppKit. It is not derivable: a byte search of the
/// 244 MB Framework for `%g, %g` and `{%g` finds **nothing**, because the guest
/// does not format points itself - it asks AppKit for the string and prints that.
/// So any string produced here would be a plausible invention, which is the one
/// outcome this project treats as worse than a refusal.
///
/// Nil is safe *and* is an answer the guest carries. The one call site,
/// `0x8058fa9`, is inside an accessibility diagnostic that formats `point=%s`:
///
/// ```text
/// 8058fa9  call _NSStringFromPoint
/// 8058fae  mov  rdi, rax
/// 8058fb1  call _objc_retainAutoreleasedReturnValue
/// 8058fb9  test rax, rax
/// 8058fbc  je   0x8058fcc            ; nil: the buffer is left empty
/// 8058fbe  lea  rdi, [rbp-0x40]
/// 8058fc5  call 0x5d99b80            ; and the log line still gets written
/// ```
///
/// so the result only ever reaches a log line, and the guest has a path for its
/// absence.
///
/// The point arrives in `xmm0`/`xmm1` (verified: `movsd xmm0, [rax]` then
/// `movsd xmm1, [rax+8]` at `0x8058fa0`), which is what a `#[repr(C)]` pair of
/// doubles gets on this ABI.
#[allow(non_snake_case)]
pub extern "C" fn NSStringFromPoint(_point: NSPoint) -> objc::object::Id {
    say_once(
        "NSStringFromPoint: refusing - the textual form of a point is a format string \
         inside AppKit and is nowhere in the guest, so there is nothing to derive it \
         from and nothing to check an answer against; answering nil, which the one call \
         site (0x8058fbc) already handles",
    );
    std::ptr::null_mut()
}

/// A diagnostic said once, so a refusal is legible rather than silent.
fn say_once(what: &str) {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if ONCE.set(()).is_ok() {
        eprintln!("rosetta3: {what}");
    }
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

/// The eight `CGFloat CGRectGet…(CGRect)` accessors.
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

    /// The rectangle the tests below build, and the one inside it.
    fn pair() -> (NSRect, NSRect) {
        let outer = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSSize {
                width: 100.0,
                height: 80.0,
            },
        };
        let inner = NSRect {
            origin: NSPoint { x: 10.0, y: 20.0 },
            size: NSSize {
                width: 30.0,
                height: 40.0,
            },
        };
        (outer, inner)
    }

    /// A rectangle contains itself, and contains one that shares its edges.
    ///
    /// Both halves matter: `NSContainsRect(x, x)` answering `false` would send the
    /// guest's loop at `0x3a8e175` down the "not found" arm for every candidate
    /// rectangle, and answering `true` for a rectangle that overhangs would send it
    /// the other way.
    #[test]
    fn a_rectangle_is_contained_by_itself_and_by_one_that_encloses_it() {
        let (outer, inner) = pair();
        // SAFETY: each takes two `CGRect`s by value and returns a `BOOL`.
        unsafe {
            assert_eq!(NSContainsRect(outer, outer), 1);
            assert_eq!(NSContainsRect(inner, outer), 1);
            // Sharing the far edges still counts: the containment test is closed.
            let flush = NSRect {
                origin: NSPoint { x: 0.0, y: 0.0 },
                size: outer.size,
            };
            assert_eq!(NSContainsRect(flush, outer), 1);
            // One point over on either axis is not contained.
            let over = NSRect {
                origin: NSPoint { x: 10.0, y: 20.0 },
                size: NSSize {
                    width: 91.0,
                    height: 40.0,
                },
            };
            assert_eq!(NSContainsRect(over, outer), 0);
            let left = NSRect {
                origin: NSPoint { x: -1.0, y: 20.0 },
                size: inner.size,
            };
            assert_eq!(NSContainsRect(left, outer), 0);
            // An empty rectangle contains nothing, not even itself.
            let empty = NSRect {
                origin: inner.origin,
                size: NSSize::default(),
            };
            assert_eq!(NSContainsRect(empty, outer), 0);
            assert_eq!(NSContainsRect(empty, empty), 0);
        }
    }

    /// The empty rectangle is a real answer here and not a rounding accident: the
    /// call site at `0x3a8e175` is a loop that increments a counter on `false`, so
    /// a rectangle the guest built as a filter box must be reported as containing
    /// nothing.
    #[test]
    fn an_empty_rectangle_contains_nothing() {
        let (outer, _) = pair();
        let empty = NSRect {
            origin: NSPoint { x: 50.0, y: 50.0 },
            size: NSSize::default(),
        };
        // SAFETY: as above.
        unsafe { assert_eq!(NSContainsRect(empty, outer), 0) };
    }

    /// Touching edges do not intersect.
    ///
    /// The same half-open rule `contains` uses, and the reason is the answer
    /// `NSIntersection` would give: the shared edge is in neither rectangle's
    /// interior, so the intersection is empty. Getting this wrong in the other
    /// direction would make the guest's `0x8b6ed8f` build a new object for two
    /// rectangles that are merely adjacent.
    #[test]
    fn rectangles_that_only_touch_do_not_intersect() {
        let (outer, inner) = pair();
        let abutting = NSRect {
            origin: NSPoint { x: 100.0, y: 0.0 },
            size: NSSize {
                width: 10.0,
                height: 10.0,
            },
        };
        // SAFETY: as above.
        unsafe {
            assert_eq!(NSIntersectsRect(inner, outer), 1);
            assert_eq!(NSIntersectsRect(outer, inner), 1);
            assert_eq!(NSIntersectsRect(outer, outer), 1, "a rectangle meets itself");
            assert_eq!(NSIntersectsRect(abutting, outer), 0);
            let above = NSRect {
                origin: NSPoint { x: 0.0, y: 80.0 },
                size: NSSize {
                    width: 10.0,
                    height: 10.0,
                },
            };
            assert_eq!(NSIntersectsRect(above, outer), 0);
            // A zero-extent rectangle has no interior, so it meets nothing.
            let flat = NSRect {
                origin: NSPoint { x: 10.0, y: 20.0 },
                size: NSSize::default(),
            };
            assert_eq!(NSIntersectsRect(flat, outer), 0);
        }
    }

    /// The two extent tests the guest's two call sites branch on, and the negative
    /// extents that make them worth having.
    #[test]
    fn equal_sizes_compares_both_extents() {
        let a = NSSize {
            width: 3.5,
            height: 4.5,
        };
        // SAFETY: each takes two `CGSize`s by value and returns a `BOOL`.
        unsafe {
            assert_eq!(NSEqualSizes(a, a), 1);
            assert_eq!(
                NSEqualSizes(
                    a,
                    NSSize {
                        width: 3.5,
                        height: 4.5
                    }
                ),
                1
            );
            assert_eq!(
                NSEqualSizes(
                    a,
                    NSSize {
                        width: 3.5,
                        height: 4.5000001
                    }
                ),
                0
            );
            assert_eq!(
                NSEqualSizes(
                    a,
                    NSSize {
                        width: 3.5000001,
                        height: 4.5
                    }
                ),
                0
            );
        }
    }

    /// Moving a rectangle moves its origin and leaves its extent alone, which is
    /// what tells this apart from `NSInsetRect`.
    #[test]
    fn offset_rect_moves_the_origin_and_keeps_the_extent() {
        let rect = NSRect {
            origin: NSPoint { x: 10.0, y: 20.0 },
            size: NSSize {
                width: 30.0,
                height: 40.0,
            },
        };
        // SAFETY: a `CGRect` in and a `CGRect` out, both by value.
        unsafe {
            let moved = NSOffsetRect(rect, 5.0, -7.5);
            assert_eq!(
                moved,
                NSRect {
                    origin: NSPoint { x: 15.0, y: 12.5 },
                    size: rect.size
                }
            );
            // And the moved rectangle still contains what the original did, which
            // is what a caller scrolling content by an offset is relying on.
            let outer = NSRect {
                origin: NSPoint { x: 0.0, y: 0.0 },
                size: NSSize {
                    width: 100.0,
                    height: 100.0,
                },
            };
            assert_eq!(NSContainsRect(NSOffsetRect(rect, 1.0, 1.0), outer), 1);
        }
    }

    /// `NSStringFromPoint` refuses and answers nil, and the nil is what the one
    /// call site handles.
    ///
    /// Asserted on the *value*: a test that only checked the function was callable
    /// would pass on a plausible point format, which is the hazard - `"(1, 2)"`
    /// looks right in a log line and is Apple's string, not this runtime's.
    #[test]
    fn a_points_textual_form_is_refused_rather_than_invented() {
        crate::bootstrap();
        let point = NSPoint { x: 1.5, y: -2.5 };
        let text = NSStringFromPoint(point);
        assert!(
            text.is_null(),
            "nil is the answer the call site at 0x8058fbc already handles"
        );
    }
}
