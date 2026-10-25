//! The path type and glyph outlines, from `coregraphics`, and the `NSRect`
//! bridge AppKit's C signatures still need.

pub use coregraphics::cgpath::{Elements, elements_of, glyph_outline, path_from};

use coregraphics::{CGPoint, CGRect, CGSize};
use foundation::geometry::{NSPoint, NSRect, NSSize};

/// `NSRect` as the `CGRect` the path builder takes. AppKit's C signatures keep
/// the `NS` spelling, and the two are the same layout.
pub(crate) fn cg_rect(r: NSRect) -> CGRect {
    CGRect {
        origin: CGPoint {
            x: r.origin.x,
            y: r.origin.y,
        },
        size: CGSize {
            width: r.size.width,
            height: r.size.height,
        },
    }
}

/// `CGRect` as the `NSRect` AppKit's C signatures answer with.
pub(crate) fn ns_rect(r: CGRect) -> NSRect {
    NSRect {
        origin: NSPoint {
            x: r.origin.x,
            y: r.origin.y,
        },
        size: NSSize {
            width: r.size.width,
            height: r.size.height,
        },
    }
}

/// `NSPoint` as the `CGPoint` CoreGraphics takes.
pub(crate) fn cg_point(p: NSPoint) -> CGPoint {
    CGPoint { x: p.x, y: p.y }
}

/// `CGPoint` as the `NSPoint` AppKit's C signatures answer with.
pub(crate) fn ns_point(p: CGPoint) -> NSPoint {
    NSPoint { x: p.x, y: p.y }
}
