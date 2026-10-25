//! `CGPath` and the path-building geometry a `CGContext`'s current path shares.
//!
//! A path is its elements, in the coordinates they were added in after the
//! transform passed with each. A context keeps its current path the same way,
//! with the CTM as that transform, which is when Quartz applies it.

#![allow(non_snake_case)]

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use i_overlay::float::single::SingleFloatOverlay;

use crate::{CGAffineTransform, CGAffineTransformIdentity, CGFloat, CGPoint, CGRect, CGSize};

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CGPathElementType {
    kCGPathElementMoveToPoint(CGPoint),
    kCGPathElementAddLineToPoint(CGPoint),
    kCGPathElementAddQuadCurveToPoint(CGPoint, CGPoint),
    kCGPathElementAddCurveToPoint(CGPoint, CGPoint, CGPoint),
    kCGPathElementCloseSubpath,
}

#[repr(C)]
pub struct CGPath {
    isa: CFTypeRef,
    elements: *mut Elements,
}

/// `CGPathElement`: a type and a pointer to its points, as `CGPathApply`
/// hands one to an applier.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CGPathElement {
    pub element_type: CGPathElementType,
    pub points: *const CGPoint,
}

pub type CGPathRef = *const c_void;
pub type CGMutablePathRef = *mut c_void;

/// A sequence of elements, with the point the next element starts from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Elements {
    pub list: Vec<CGPathElementType>,
    /// The current point, if there is one.
    pub current: Option<CGPoint>,
    /// Where the current subpath began, which a close returns to.
    start: Option<CGPoint>,
}

/// `4/3 * (sqrt(2) - 1)`: the control distance that makes a cubic a quarter
/// circle to within 0.03%.
const KAPPA: f64 = 0.552_284_749_830_793_4;

impl Elements {
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn clear(&mut self) {
        *self = Elements::default();
    }

    pub fn move_to(&mut self, m: CGAffineTransform, x: CGFloat, y: CGFloat) {
        let p = m.apply(x, y);
        self.list
            .push(CGPathElementType::kCGPathElementMoveToPoint(p));
        self.current = Some(p);
        self.start = Some(p);
    }

    /// A line, or a move when there is nowhere to draw from - which is what
    /// Quartz does rather than refuse.
    pub fn line_to(&mut self, m: CGAffineTransform, x: CGFloat, y: CGFloat) {
        if self.current.is_none() {
            return self.move_to(m, x, y);
        }
        let p = m.apply(x, y);
        self.list
            .push(CGPathElementType::kCGPathElementAddLineToPoint(p));
        self.current = Some(p);
    }

    pub fn quad_to(
        &mut self,
        m: CGAffineTransform,
        cx: CGFloat,
        cy: CGFloat,
        x: CGFloat,
        y: CGFloat,
    ) {
        if self.current.is_none() {
            self.move_to(m, cx, cy);
        }
        let p = m.apply(x, y);
        self.list
            .push(CGPathElementType::kCGPathElementAddQuadCurveToPoint(
                m.apply(cx, cy),
                p,
            ));
        self.current = Some(p);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn cubic_to(
        &mut self,
        m: CGAffineTransform,
        c1x: CGFloat,
        c1y: CGFloat,
        c2x: CGFloat,
        c2y: CGFloat,
        x: CGFloat,
        y: CGFloat,
    ) {
        if self.current.is_none() {
            self.move_to(m, c1x, c1y);
        }
        let p = m.apply(x, y);
        self.list
            .push(CGPathElementType::kCGPathElementAddCurveToPoint(
                m.apply(c1x, c1y),
                m.apply(c2x, c2y),
                p,
            ));
        self.current = Some(p);
    }

    pub fn close(&mut self) {
        if self.current.is_none()
            || matches!(
                self.list.last(),
                Some(CGPathElementType::kCGPathElementCloseSubpath) | None
            )
        {
            return;
        }
        self.list
            .push(CGPathElementType::kCGPathElementCloseSubpath);
        self.current = self.start;
    }

    pub fn rect(&mut self, m: CGAffineTransform, r: CGRect) {
        let (x0, y0) = (r.origin.x, r.origin.y);
        let (x1, y1) = (x0 + r.size.width, y0 + r.size.height);
        self.move_to(m, x0, y0);
        self.line_to(m, x1, y0);
        self.line_to(m, x1, y1);
        self.line_to(m, x0, y1);
        self.close();
    }

    /// `CGPathAddRoundedRect`: a rectangle with elliptical corners, each
    /// radius clamped to half the side it sits on.
    pub fn rounded_rect(
        &mut self,
        m: CGAffineTransform,
        r: CGRect,
        corner_width: CGFloat,
        corner_height: CGFloat,
    ) {
        let (x0, y0) = (r.origin.x, r.origin.y);
        let (x1, y1) = (x0 + r.size.width, y0 + r.size.height);
        let rx = corner_width.clamp(0.0, r.size.width.abs() / 2.0);
        let ry = corner_height.clamp(0.0, r.size.height.abs() / 2.0);
        let (kx, ky) = (rx * KAPPA, ry * KAPPA);
        // Quartz starts at the middle of the right edge and goes
        // counterclockwise, which is what `CGPathApply` reports and what a
        // dashed or stroked copy inherits.
        self.move_to(m, x1, (y0 + y1) / 2.0);
        self.line_to(m, x1, y1 - ry);
        self.cubic_to(m, x1, y1 - ry + ky, x1 - rx + kx, y1, x1 - rx, y1);
        self.line_to(m, x0 + rx, y1);
        self.cubic_to(m, x0 + rx - kx, y1, x0, y1 - ry + ky, x0, y1 - ry);
        self.line_to(m, x0, y0 + ry);
        self.cubic_to(m, x0, y0 + ry - ky, x0 + rx - kx, y0, x0 + rx, y0);
        self.line_to(m, x1 - rx, y0);
        self.cubic_to(m, x1 - rx + kx, y0, x1, y0 + ry - ky, x1, y0 + ry);
        self.close();
    }

    /// The rectangle this path is, if it is exactly one in the order
    /// `CGPathCreateWithRect` emits: the minimum corner, then +x, +y, -x.
    pub fn as_rect(&self) -> Option<CGRect> {
        let [
            CGPathElementType::kCGPathElementMoveToPoint(a),
            CGPathElementType::kCGPathElementAddLineToPoint(b),
            CGPathElementType::kCGPathElementAddLineToPoint(c),
            CGPathElementType::kCGPathElementAddLineToPoint(d),
            CGPathElementType::kCGPathElementCloseSubpath,
        ] = self.list.as_slice()
        else {
            return None;
        };
        if !(a.y == b.y && b.x == c.x && c.y == d.y && d.x == a.x && a.x <= b.x && a.y <= d.y) {
            return None;
        }
        Some(CGRect {
            origin: CGPoint { x: a.x, y: a.y },
            size: CGSize {
                width: c.x - a.x,
                height: c.y - a.y,
            },
        })
    }

    pub fn ellipse(&mut self, m: CGAffineTransform, r: CGRect) {
        let (cx, cy) = (
            r.origin.x + r.size.width / 2.0,
            r.origin.y + r.size.height / 2.0,
        );
        let (rx, ry) = (r.size.width / 2.0, r.size.height / 2.0);
        let (kx, ky) = (rx * KAPPA, ry * KAPPA);
        self.move_to(m, cx + rx, cy);
        self.cubic_to(m, cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry);
        self.cubic_to(m, cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy);
        self.cubic_to(m, cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry);
        self.cubic_to(m, cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy);
        self.close();
    }

    /// `CGPathAddArc`: a line to the arc's start when there is a current point,
    /// then the arc, in at most quarter turns.
    ///
    /// `clockwise` is in Quartz's y-up sense: the angle decreases.
    #[allow(clippy::too_many_arguments)]
    pub fn arc(
        &mut self,
        m: CGAffineTransform,
        x: CGFloat,
        y: CGFloat,
        radius: CGFloat,
        start: CGFloat,
        end: CGFloat,
        clockwise: bool,
    ) {
        use std::f64::consts::TAU;
        let mut sweep = end - start;
        match clockwise {
            true => {
                if sweep > 0.0 {
                    sweep -= TAU * ((sweep / TAU).floor() + 1.0);
                }
                sweep = sweep.max(-TAU);
            }
            false => {
                if sweep < 0.0 {
                    sweep += TAU * ((-sweep / TAU).floor() + 1.0);
                }
                sweep = sweep.min(TAU);
            }
        }
        let (sx, sy) = (x + radius * start.cos(), y + radius * start.sin());
        match self.current {
            Some(_) => self.line_to(m, sx, sy),
            None => self.move_to(m, sx, sy),
        }
        // A sweep that is a quarter turn to within rounding is one piece, not
        // two: `arc_to` derives its angles from `atan2`, so an exact 90 degrees
        // arrives a few ulps over.
        let quarters = sweep.abs() / (TAU / 4.0);
        let pieces = (quarters - 1e-9).ceil().max(1.0) as usize;
        let step = sweep / pieces as CGFloat;
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        let mut a = start;
        for _ in 0..pieces {
            let b = a + step;
            let (ca, sa, cb, sb) = (a.cos(), a.sin(), b.cos(), b.sin());
            self.cubic_to(
                m,
                x + radius * (ca - k * sa),
                y + radius * (sa + k * ca),
                x + radius * (cb + k * sb),
                y + radius * (sb - k * cb),
                x + radius * cb,
                y + radius * sb,
            );
            a = b;
        }
    }

    /// `CGPathAddArcToPoint`: the arc of `radius` tangent to the line from the
    /// current point to `(x1, y1)` and from there to `(x2, y2)`.
    ///
    /// The tangent geometry is worked in the untransformed space the points
    /// are given in, so the current point is mapped back through `m`.
    #[allow(clippy::too_many_arguments)]
    pub fn arc_to(
        &mut self,
        m: CGAffineTransform,
        x1: CGFloat,
        y1: CGFloat,
        x2: CGFloat,
        y2: CGFloat,
        radius: CGFloat,
    ) {
        let Some(current) = self.current else {
            return self.move_to(m, x1, y1);
        };
        let Some(inverse) = m.inverse() else {
            return;
        };
        let CGPoint { x: x0, y: y0 } = inverse.apply(current.x, current.y);
        let (v0x, v0y) = (x0 - x1, y0 - y1);
        let (v1x, v1y) = (x2 - x1, y2 - y1);
        let (l0, l1) = (v0x.hypot(v0y), v1x.hypot(v1y));
        let cross = v0x * v1y - v0y * v1x;
        if l0 == 0.0 || l1 == 0.0 || cross.abs() < 1e-9 || radius <= 0.0 {
            return self.line_to(m, x1, y1);
        }
        let cos = (v0x * v1x + v0y * v1y) / (l0 * l1);
        let half = cos.clamp(-1.0, 1.0).acos() / 2.0;
        let distance = radius / half.tan();
        let (t0x, t0y) = (x1 + v0x / l0 * distance, y1 + v0y / l0 * distance);
        let (t1x, t1y) = (x1 + v1x / l1 * distance, y1 + v1y / l1 * distance);
        // The centre is along the bisector, `radius / sin(half)` from the
        // corner.
        let (bx, by) = (v0x / l0 + v1x / l1, v0y / l0 + v1y / l1);
        let bl = bx.hypot(by);
        let reach = radius / half.sin();
        let (cx, cy) = (x1 + bx / bl * reach, y1 + by / bl * reach);
        let start = (t0y - cy).atan2(t0x - cx);
        let end = (t1y - cy).atan2(t1x - cx);
        self.arc(m, cx, cy, radius, start, end, cross > 0.0);
    }

    /// Append `other`'s elements through `m`.
    pub fn append(&mut self, m: CGAffineTransform, other: &Elements) {
        for element in &other.list {
            match *element {
                CGPathElementType::kCGPathElementMoveToPoint(p) => self.move_to(m, p.x, p.y),
                CGPathElementType::kCGPathElementAddLineToPoint(p) => self.line_to(m, p.x, p.y),
                CGPathElementType::kCGPathElementAddQuadCurveToPoint(c, p) => {
                    self.quad_to(m, c.x, c.y, p.x, p.y)
                }
                CGPathElementType::kCGPathElementAddCurveToPoint(c1, c2, p) => {
                    self.cubic_to(m, c1.x, c1.y, c2.x, c2.y, p.x, p.y)
                }
                CGPathElementType::kCGPathElementCloseSubpath => self.close(),
            }
        }
    }

    /// The bounding box of every point, control points included, as
    /// `CGPathGetBoundingBox` answers.
    pub fn bounds(&self) -> Option<CGRect> {
        let mut points = self.list.iter().flat_map(|e| match *e {
            CGPathElementType::kCGPathElementMoveToPoint(p)
            | CGPathElementType::kCGPathElementAddLineToPoint(p) => vec![p],
            CGPathElementType::kCGPathElementAddQuadCurveToPoint(c, p) => vec![c, p],
            CGPathElementType::kCGPathElementAddCurveToPoint(c1, c2, p) => vec![c1, c2, p],
            CGPathElementType::kCGPathElementCloseSubpath => vec![],
        });
        let first = points.next()?;
        let (l, b, r, t) = points.fold((first.x, first.y, first.x, first.y), |(l, b, r, t), p| {
            (l.min(p.x), b.min(p.y), r.max(p.x), t.max(p.y))
        });
        Some(CGRect {
            origin: CGPoint { x: l, y: b },
            size: CGSize {
                width: r - l,
                height: t - b,
            },
        })
    }

    /// The path for the rasterizer, or `None` when it encloses nothing.
    pub fn to_skia(&self) -> Option<tiny_skia::Path> {
        let mut builder = tiny_skia::PathBuilder::new();
        let f = |p: CGPoint| (p.x as f32, p.y as f32);
        for element in &self.list {
            match *element {
                CGPathElementType::kCGPathElementMoveToPoint(p) => {
                    let (x, y) = f(p);
                    builder.move_to(x, y);
                }
                CGPathElementType::kCGPathElementAddLineToPoint(p) => {
                    let (x, y) = f(p);
                    builder.line_to(x, y);
                }
                CGPathElementType::kCGPathElementAddQuadCurveToPoint(c, p) => {
                    let ((cx, cy), (x, y)) = (f(c), f(p));
                    builder.quad_to(cx, cy, x, y);
                }
                CGPathElementType::kCGPathElementAddCurveToPoint(c1, c2, p) => {
                    let ((ax, ay), (bx, by), (x, y)) = (f(c1), f(c2), f(p));
                    builder.cubic_to(ax, ay, bx, by, x, y);
                }
                CGPathElementType::kCGPathElementCloseSubpath => builder.close(),
            }
        }
        builder.finish()
    }

    /// The elements of a rasterizer path, as a stroke outline comes back.
    pub fn from_skia(path: &tiny_skia::Path, m: CGAffineTransform) -> Elements {
        use tiny_skia::PathSegment;
        let mut out = Elements::default();
        let g = |p: tiny_skia::Point| (CGFloat::from(p.x), CGFloat::from(p.y));
        for segment in path.segments() {
            match segment {
                PathSegment::MoveTo(p) => {
                    let (x, y) = g(p);
                    out.move_to(m, x, y);
                }
                PathSegment::LineTo(p) => {
                    let (x, y) = g(p);
                    out.line_to(m, x, y);
                }
                PathSegment::QuadTo(c, p) => {
                    let ((cx, cy), (x, y)) = (g(c), g(p));
                    out.quad_to(m, cx, cy, x, y);
                }
                PathSegment::CubicTo(c1, c2, p) => {
                    let ((ax, ay), (bx, by), (x, y)) = (g(c1), g(c2), g(p));
                    out.cubic_to(m, ax, ay, bx, by, x, y);
                }
                PathSegment::Close => out.close(),
            }
        }
        out
    }

    /// `CGPathCreateCopyByFlattening`: every curve replaced by line segments
    /// no further than `threshold` from it.
    pub fn flattened(&self, threshold: CGFloat) -> Elements {
        let mut out = Elements::default();
        let mut pen: Option<CGPoint> = None;
        let mut subpath: Option<CGPoint> = None;
        for element in &self.list {
            match *element {
                CGPathElementType::kCGPathElementMoveToPoint(p) => {
                    out.move_to(CGAffineTransformIdentity, p.x, p.y);
                    pen = Some(p);
                    subpath = Some(p);
                }
                CGPathElementType::kCGPathElementAddLineToPoint(p) => {
                    out.line_to(CGAffineTransformIdentity, p.x, p.y);
                    pen = Some(p);
                }
                CGPathElementType::kCGPathElementAddQuadCurveToPoint(c, p) => {
                    if let Some(start) = pen {
                        flatten_quad(&mut out, start, c, p, threshold);
                    }
                    pen = Some(p);
                }
                CGPathElementType::kCGPathElementAddCurveToPoint(c1, c2, p) => {
                    if let Some(start) = pen {
                        flatten_cubic(&mut out, start, c1, c2, p, threshold, 0);
                    }
                    pen = Some(p);
                }
                CGPathElementType::kCGPathElementCloseSubpath => {
                    out.close();
                    pen = subpath;
                }
            }
        }
        out
    }

    /// The path as closed contours of line segments, which is what the boolean
    /// engine takes. Curves are flattened at `threshold`, and an open subpath
    /// is closed because a boolean operation treats a path as a filled region.
    pub fn contours(&self, threshold: CGFloat) -> Vec<Vec<[CGFloat; 2]>> {
        let flat = self.flattened(threshold);
        let mut out: Vec<Vec<[CGFloat; 2]>> = Vec::new();
        let mut current: Vec<[CGFloat; 2]> = Vec::new();
        for element in &flat.list {
            match *element {
                CGPathElementType::kCGPathElementMoveToPoint(p) => {
                    if current.len() > 2 {
                        out.push(std::mem::take(&mut current));
                    } else {
                        current.clear();
                    }
                    current.push([p.x, p.y]);
                }
                CGPathElementType::kCGPathElementAddLineToPoint(p) => current.push([p.x, p.y]),
                CGPathElementType::kCGPathElementCloseSubpath => {
                    if current.len() > 2 {
                        out.push(std::mem::take(&mut current));
                    } else {
                        current.clear();
                    }
                }
                // `flattened` emits no curves.
                CGPathElementType::kCGPathElementAddQuadCurveToPoint(..)
                | CGPathElementType::kCGPathElementAddCurveToPoint(..) => {}
            }
        }
        if current.len() > 2 {
            out.push(current);
        }
        out
    }

    /// The elements of a boolean result: one closed contour per shape contour.
    pub fn from_contours(shapes: &[Vec<Vec<[CGFloat; 2]>>]) -> Elements {
        let mut out = Elements::default();
        for shape in shapes {
            for contour in shape {
                let Some((first, rest)) = contour.split_first() else {
                    continue;
                };
                out.move_to(CGAffineTransformIdentity, first[0], first[1]);
                for p in rest {
                    out.line_to(CGAffineTransformIdentity, p[0], p[1]);
                }
                out.close();
            }
        }
        out
    }
}

/// The most subdivisions a curve may take before it is accepted as flat.
///
/// Totality: a curve whose control points never converge would otherwise
/// recurse until the stack runs out.
const MAX_FLATTEN_DEPTH: u32 = 24;

fn midpoint(a: CGPoint, b: CGPoint) -> CGPoint {
    CGPoint {
        x: (a.x + b.x) / 2.0,
        y: (a.y + b.y) / 2.0,
    }
}

/// The distance from `p` to the line through `a` and `b`, or to `a` when the
/// line has no length.
fn distance_to_line(p: CGPoint, a: CGPoint, b: CGPoint) -> CGFloat {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length = dx.hypot(dy);
    if length == 0.0 {
        return (p.x - a.x).hypot(p.y - a.y);
    }
    ((p.x - a.x) * dy - (p.y - a.y) * dx).abs() / length
}

fn flatten_cubic(
    out: &mut Elements,
    p0: CGPoint,
    p1: CGPoint,
    p2: CGPoint,
    p3: CGPoint,
    threshold: CGFloat,
    depth: u32,
) {
    let flat =
        distance_to_line(p1, p0, p3) <= threshold && distance_to_line(p2, p0, p3) <= threshold;
    if flat || depth >= MAX_FLATTEN_DEPTH {
        out.line_to(CGAffineTransformIdentity, p3.x, p3.y);
        return;
    }
    let p01 = midpoint(p0, p1);
    let p12 = midpoint(p1, p2);
    let p23 = midpoint(p2, p3);
    let p012 = midpoint(p01, p12);
    let p123 = midpoint(p12, p23);
    let p0123 = midpoint(p012, p123);
    flatten_cubic(out, p0, p01, p012, p0123, threshold, depth + 1);
    flatten_cubic(out, p0123, p123, p23, p3, threshold, depth + 1);
}

fn flatten_quad(out: &mut Elements, p0: CGPoint, c: CGPoint, p2: CGPoint, threshold: CGFloat) {
    // Degree elevation: the same curve as a cubic.
    let c1 = CGPoint {
        x: p0.x + 2.0 / 3.0 * (c.x - p0.x),
        y: p0.y + 2.0 / 3.0 * (c.y - p0.y),
    };
    let c2 = CGPoint {
        x: p2.x + 2.0 / 3.0 * (c.x - p2.x),
        y: p2.y + 2.0 / 3.0 * (c.y - p2.y),
    };
    flatten_cubic(out, p0, c1, c2, p2, threshold, 0);
}

/// The distance from `p` to the segment `a`-`b`.
fn distance_to_segment(p: CGPoint, a: CGPoint, b: CGPoint) -> CGFloat {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length_squared = dx * dx + dy * dy;
    if length_squared == 0.0 {
        return (p.x - a.x).hypot(p.y - a.y);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / length_squared).clamp(0.0, 1.0);
    (p.x - (a.x + t * dx)).hypot(p.y - (a.y + t * dy))
}

/// Whether `point` lies on the path itself, within `tolerance`.
///
/// A fill test cannot see a subpath with no area - a dashed copy is a run of
/// zero-width lines - and Quartz counts a point on the path as inside.
fn on_path(elements: &Elements, point: CGPoint, tolerance: CGFloat) -> bool {
    let flat = elements.flattened(tolerance);
    let mut pen: Option<CGPoint> = None;
    let mut start: Option<CGPoint> = None;
    for element in &flat.list {
        match *element {
            CGPathElementType::kCGPathElementMoveToPoint(p) => {
                pen = Some(p);
                start = Some(p);
            }
            CGPathElementType::kCGPathElementAddLineToPoint(p) => {
                if let Some(from) = pen
                    && distance_to_segment(point, from, p) <= tolerance
                {
                    return true;
                }
                pen = Some(p);
            }
            CGPathElementType::kCGPathElementCloseSubpath => {
                if let (Some(from), Some(to)) = (pen, start)
                    && distance_to_segment(point, from, to) <= tolerance
                {
                    return true;
                }
                pen = start;
            }
            CGPathElementType::kCGPathElementAddQuadCurveToPoint(..)
            | CGPathElementType::kCGPathElementAddCurveToPoint(..) => {}
        }
    }
    false
}

/// The winding number of `point` against the path's flattened outline.
///
/// An open subpath is not closed: Quartz counts the segments as given, so a
/// zero-width dash still crosses the ray.
fn winding(elements: &Elements, point: CGPoint) -> i32 {
    let flat = elements.flattened(1e-6);
    let mut total = 0;
    let mut pen: Option<CGPoint> = None;
    let mut start: Option<CGPoint> = None;
    for element in &flat.list {
        match *element {
            CGPathElementType::kCGPathElementMoveToPoint(p) => {
                pen = Some(p);
                start = Some(p);
            }
            CGPathElementType::kCGPathElementAddLineToPoint(p) => {
                if let Some(from) = pen {
                    total += crossing(point, from, p);
                }
                pen = Some(p);
            }
            CGPathElementType::kCGPathElementCloseSubpath => {
                if let (Some(from), Some(to)) = (pen, start) {
                    total += crossing(point, from, to);
                }
                pen = start;
            }
            CGPathElementType::kCGPathElementAddQuadCurveToPoint(..)
            | CGPathElementType::kCGPathElementAddCurveToPoint(..) => {}
        }
    }
    total
}

/// `+1` or `-1` when the segment crosses the ray from `p` towards +x, else `0`.
fn crossing(p: CGPoint, a: CGPoint, b: CGPoint) -> i32 {
    if (a.y > p.y) == (b.y > p.y) {
        return 0;
    }
    let t = (p.y - a.y) / (b.y - a.y);
    if a.x + t * (b.x - a.x) <= p.x {
        return 0;
    }
    match b.y > a.y {
        true => 1,
        false => -1,
    }
}

/// Append a glyph's outline, in font units, through `m`. Every contour is
/// closed, as a glyph's are.
pub fn glyph_outline(
    face: &owned_ttf_parser::Face<'_>,
    glyph: u16,
    m: CGAffineTransform,
    out: &mut Elements,
) {
    use crate::fonts::Segment;
    let p = |(x, y): (f32, f32)| (f64::from(x), f64::from(y));
    let mut pen: Option<(f32, f32)> = None;
    for segment in crate::fonts::outline(face, glyph) {
        let start = match segment {
            Segment::Line(a, _) | Segment::Quad(a, _, _) | Segment::Cubic(a, _, _, _) => a,
        };
        if pen != Some(start) {
            out.close();
            let (x, y) = p(start);
            out.move_to(m, x, y);
        }
        let end = match segment {
            Segment::Line(_, b) => {
                let (x, y) = p(b);
                out.line_to(m, x, y);
                b
            }
            Segment::Quad(_, c, b) => {
                let ((cx, cy), (x, y)) = (p(c), p(b));
                out.quad_to(m, cx, cy, x, y);
                b
            }
            Segment::Cubic(_, c1, c2, b) => {
                let ((ax, ay), (bx, by), (x, y)) = (p(c1), p(c2), p(b));
                out.cubic_to(m, ax, ay, bx, by, x, y);
                b
            }
        };
        pen = Some(end);
    }
    out.close();
}

/// A new path object holding `elements`, which the caller owns.
pub fn path_from(elements: Elements) -> CGPathRef {
    new_path(elements)
}

pub fn install() {
    let path = corefoundation::base::register(c"__CGPath", None, size_of::<CGPath>(), true);
    path.replace_method(
        objc::sel::get("dealloc"),
        path_dealloc as *const c_void,
        c"v16@0:8",
    );
}

unsafe extern "C" fn path_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `CGPath`, whose elements are this runtime's.
    unsafe {
        let path = &mut *(receiver as *mut CGPath);
        std::ptr::drop_in_place(&mut path.elements);
        objc::arc::finalize(receiver);
    }
}

/// The elements of a path object, or `None` for anything else.
pub fn elements_of<'a>(path: CGPathRef) -> Option<&'a mut Elements> {
    let class = objc::class::lookup(c"__CGPath")?;
    // SAFETY: null or an object a guest received from this runtime.
    if path.is_null() || !unsafe { corefoundation::base::is_kind_of(path.cast(), class) } {
        return None;
    }
    // SAFETY: a `CGPath`, whose elements this runtime owns.
    unsafe { (*(path as *mut CGPath)).elements.as_mut() }
}

/// The elements a mutating call may write, or `None` for an immutable path.
///
/// Apple's headers make writing an immutable path undefined behaviour;
/// refusing is the answer that cannot corrupt the object.
// fn mutable_elements(path: CGMutablePathRef) -> Option<&'static mut Elements> {
//     let elements = elements_of(path)?;
//     // SAFETY: the object is a `CGPath` this runtime made.
//     match unsafe { (*path.cast::<CGPath>()) } {
//         true => Some(elements),
//         false => None,
//     }
// }

fn new_path(elements: Elements) -> CGPathRef {
    let class = objc::class::lookup(c"__CGPath").expect("installed during bootstrap");
    // SAFETY: `CGPath` is the registered class layout.
    let path = unsafe { corefoundation::base::allocate::<CGPath>(class) };
    // SAFETY: a fresh allocation.
    unsafe { (*path).elements = Box::into_raw(Box::new(elements)) };
    path as CGPathRef
}

/// The transform a path call was given, or the identity for none.
fn transform(m: *const CGAffineTransform) -> CGAffineTransform {
    match m.is_null() {
        true => CGAffineTransformIdentity,
        // SAFETY: the caller's transform.
        false => unsafe { *m },
    }
}

pub unsafe extern "C" fn CGPathCreateMutable() -> CGMutablePathRef {
    new_path(Elements::default()).cast_mut()
}

pub unsafe extern "C" fn CGPathCreateCopy(path: CGPathRef) -> CGPathRef {
    
    path
}

pub unsafe extern "C" fn CGPathCreateMutableCopy(path: CGPathRef) -> CGMutablePathRef {
    
    path.cast_mut()
}

pub unsafe extern "C" fn CGPathCreateCopyByTransformingPath(
    path: CGPathRef,
    m: *const CGAffineTransform,
) -> CGPathRef {
    let Some(source) = elements_of(path) else {
        return std::ptr::null();
    };
    let mut out = Elements::default();
    out.append(transform(m), source);
    new_path(out)
}

pub unsafe extern "C" fn CGPathCreateMutableCopyByTransformingPath(
    path: CGPathRef,
    m: *const CGAffineTransform,
) -> CGMutablePathRef {
    let Some(source) = elements_of(path) else {
        return std::ptr::null_mut();
    };
    let mut out = Elements::default();
    out.append(transform(m), source);
    new_path(out).cast_mut()
}

pub unsafe extern "C" fn CGPathCreateWithRect(
    rect: CGRect,
    m: *const CGAffineTransform,
) -> CGPathRef {
    let mut out = Elements::default();
    out.rect(transform(m), rect);
    new_path(out)
}

pub unsafe extern "C" fn CGPathCreateWithEllipseInRect(
    rect: CGRect,
    m: *const CGAffineTransform,
) -> CGPathRef {
    let mut out = Elements::default();
    out.ellipse(transform(m), rect);
    new_path(out)
}

/// `CGPathRef CGPathCreateWithRoundedRect(CGRect rect, CGFloat cornerWidth,
/// CGFloat cornerHeight, const CGAffineTransform *m)`
pub unsafe extern "C" fn CGPathCreateWithRoundedRect(
    rect: CGRect,
    corner_width: CGFloat,
    corner_height: CGFloat,
    m: *const CGAffineTransform,
) -> CGPathRef {
    let mut out = Elements::default();
    out.rounded_rect(transform(m), rect, corner_width, corner_height);
    new_path(out)
}

/// `CGPathRef CGPathCreateCopyByDashingPath(CGPathRef path,
/// const CGAffineTransform *m, CGFloat phase, const CGFloat *lengths,
/// size_t count)`
pub unsafe extern "C" fn CGPathCreateCopyByDashingPath(
    path: CGPathRef,
    m: *const CGAffineTransform,
    phase: CGFloat,
    lengths: *const CGFloat,
    count: usize,
) -> CGPathRef {
    let Some(source) = elements_of(path) else {
        return std::ptr::null();
    };
    if lengths.is_null() || count == 0 {
        return std::ptr::null();
    }
    // SAFETY: the caller supplies `count` lengths.
    let lengths = unsafe { std::slice::from_raw_parts(lengths, count) };
    let widths: Vec<f32> = lengths.iter().map(|l| *l as f32).collect();
    let Some(dash) = tiny_skia::StrokeDash::new(widths, phase as f32) else {
        return std::ptr::null();
    };
    // The transform is applied before the dashing, as the header says.
    let mut moved = Elements::default();
    moved.append(transform(m), source);
    let Some(skia) = moved.to_skia() else {
        return std::ptr::null();
    };
    let Some(dashed) = skia.dash(&dash, 1.0) else {
        return std::ptr::null();
    };
    new_path(Elements::from_skia(&dashed, CGAffineTransformIdentity))
}

/// `CGPathRef CGPathCreateCopyByStrokingPath(CGPathRef path,
/// const CGAffineTransform *m, CGFloat lineWidth, CGLineCap lineCap,
/// CGLineJoin lineJoin, CGFloat miterLimit)`
pub unsafe extern "C" fn CGPathCreateCopyByStrokingPath(
    path: CGPathRef,
    m: *const CGAffineTransform,
    line_width: CGFloat,
    line_cap: i32,
    line_join: i32,
    miter_limit: CGFloat,
) -> CGPathRef {
    let Some(source) = elements_of(path) else {
        return std::ptr::null();
    };
    let mut moved = Elements::default();
    moved.append(transform(m), source);
    let Some(skia) = moved.to_skia() else {
        return std::ptr::null();
    };
    let style = tiny_skia::Stroke {
        width: line_width as f32,
        miter_limit: miter_limit as f32,
        line_cap: match line_cap {
            1 => tiny_skia::LineCap::Round,
            2 => tiny_skia::LineCap::Square,
            _ => tiny_skia::LineCap::Butt,
        },
        line_join: match line_join {
            1 => tiny_skia::LineJoin::Round,
            2 => tiny_skia::LineJoin::Bevel,
            _ => tiny_skia::LineJoin::Miter,
        },
        ..tiny_skia::Stroke::default()
    };
    let Some(outline) = skia.stroke(&style, 1.0) else {
        return std::ptr::null();
    };
    new_path(Elements::from_skia(&outline, CGAffineTransformIdentity))
}

/// `CGPathRef CGPathCreateCopyByFlattening(CGPathRef path, CGFloat threshold)`
pub unsafe extern "C" fn CGPathCreateCopyByFlattening(
    path: CGPathRef,
    threshold: CGFloat,
) -> CGPathRef {
    let Some(source) = elements_of(path) else {
        return std::ptr::null();
    };
    new_path(source.flattened(threshold))
}

/// The tolerance the boolean forms flatten curves at.
///
/// FIXME (ACCURACY): Apple intersects the curves exactly; this flattens them
/// first, so a result can differ by up to this much.
const BOOLEAN_FLATTENING: CGFloat = 0.05;

/// The fill rule a boolean call names.
fn fill_rule(even_odd: bool) -> i_overlay::core::fill_rule::FillRule {
    match even_odd {
        true => i_overlay::core::fill_rule::FillRule::EvenOdd,
        false => i_overlay::core::fill_rule::FillRule::NonZero,
    }
}

/// `CGPathCreateCopyBy{Unioning,Intersecting,Subtracting,SymmetricDifference}Path`
fn boolean(
    path: CGPathRef,
    mask: CGPathRef,
    even_odd: bool,
    rule: i_overlay::core::overlay_rule::OverlayRule,
) -> CGPathRef {
    let Some(subject) = elements_of(path) else {
        return std::ptr::null();
    };
    let Some(clip) = elements_of(mask) else {
        return std::ptr::null();
    };
    let subject = subject.contours(BOOLEAN_FLATTENING);
    let clip = clip.contours(BOOLEAN_FLATTENING);
    let shapes = subject.overlay(&clip, rule, fill_rule(even_odd));
    new_path(Elements::from_contours(&shapes))
}

/// `CGPathRef CGPathCreateCopyByNormalizing(CGPathRef path, bool evenOddFillRule)`
pub unsafe extern "C" fn CGPathCreateCopyByNormalizing(
    path: CGPathRef,
    even_odd: bool,
) -> CGPathRef {
    let Some(subject) = elements_of(path) else {
        return std::ptr::null();
    };
    let contours = subject.contours(BOOLEAN_FLATTENING);
    let empty: Vec<Vec<[CGFloat; 2]>> = Vec::new();
    let shapes = contours.overlay(
        &empty,
        i_overlay::core::overlay_rule::OverlayRule::Subject,
        fill_rule(even_odd),
    );
    new_path(Elements::from_contours(&shapes))
}

/// `CGPathRef CGPathCreateCopyByUnioningPath(CGPathRef path, CGPathRef mask,
/// bool evenOddFillRule)`
pub unsafe extern "C" fn CGPathCreateCopyByUnioningPath(
    path: CGPathRef,
    mask: CGPathRef,
    even_odd: bool,
) -> CGPathRef {
    boolean(
        path,
        mask,
        even_odd,
        i_overlay::core::overlay_rule::OverlayRule::Union,
    )
}

/// `CGPathRef CGPathCreateCopyByIntersectingPath(CGPathRef path,
/// CGPathRef mask, bool evenOddFillRule)`
pub unsafe extern "C" fn CGPathCreateCopyByIntersectingPath(
    path: CGPathRef,
    mask: CGPathRef,
    even_odd: bool,
) -> CGPathRef {
    boolean(
        path,
        mask,
        even_odd,
        i_overlay::core::overlay_rule::OverlayRule::Intersect,
    )
}

/// `CGPathRef CGPathCreateCopyBySubtractingPath(CGPathRef path,
/// CGPathRef mask, bool evenOddFillRule)`
pub unsafe extern "C" fn CGPathCreateCopyBySubtractingPath(
    path: CGPathRef,
    mask: CGPathRef,
    even_odd: bool,
) -> CGPathRef {
    boolean(
        path,
        mask,
        even_odd,
        i_overlay::core::overlay_rule::OverlayRule::Difference,
    )
}

/// `CGPathRef CGPathCreateCopyBySymmetricDifferenceOfPath(CGPathRef path,
/// CGPathRef mask, bool evenOddFillRule)`
pub unsafe extern "C" fn CGPathCreateCopyBySymmetricDifferenceOfPath(
    path: CGPathRef,
    mask: CGPathRef,
    even_odd: bool,
) -> CGPathRef {
    boolean(
        path,
        mask,
        even_odd,
        i_overlay::core::overlay_rule::OverlayRule::Xor,
    )
}

pub unsafe extern "C" fn CGPathRetain(path: CGPathRef) -> CGPathRef {
    if !path.is_null() {
        // SAFETY: a path the caller holds.
        unsafe { corefoundation::api::CFRetain(path.cast()) };
    }
    path
}

pub unsafe extern "C" fn CGPathRelease(path: CGPathRef) {
    if !path.is_null() {
        // SAFETY: a reference the caller owns.
        unsafe { corefoundation::api::CFRelease(path.cast()) };
    }
}

pub unsafe extern "C" fn CGPathMoveToPoint(
    path: CGMutablePathRef,
    m: *const CGAffineTransform,
    x: CGFloat,
    y: CGFloat,
) {
    if let Some(e) = elements_of(path) {
        e.move_to(transform(m), x, y);
    }
}

pub unsafe extern "C" fn CGPathAddLineToPoint(
    path: CGMutablePathRef,
    m: *const CGAffineTransform,
    x: CGFloat,
    y: CGFloat,
) {
    if let Some(e) = elements_of(path) {
        e.line_to(transform(m), x, y);
    }
}

pub unsafe extern "C" fn CGPathAddQuadCurveToPoint(
    path: CGMutablePathRef,
    m: *const CGAffineTransform,
    cx: CGFloat,
    cy: CGFloat,
    x: CGFloat,
    y: CGFloat,
) {
    if let Some(e) = elements_of(path) {
        e.quad_to(transform(m), cx, cy, x, y);
    }
}

#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CGPathAddCurveToPoint(
    path: CGMutablePathRef,
    m: *const CGAffineTransform,
    c1x: CGFloat,
    c1y: CGFloat,
    c2x: CGFloat,
    c2y: CGFloat,
    x: CGFloat,
    y: CGFloat,
) {
    if let Some(e) = elements_of(path) {
        e.cubic_to(transform(m), c1x, c1y, c2x, c2y, x, y);
    }
}

pub unsafe extern "C" fn CGPathCloseSubpath(path: CGMutablePathRef) {
    if let Some(e) = elements_of(path) {
        e.close();
    }
}

pub unsafe extern "C" fn CGPathAddRect(
    path: CGMutablePathRef,
    m: *const CGAffineTransform,
    rect: CGRect,
) {
    if let Some(e) = elements_of(path) {
        e.rect(transform(m), rect);
    }
}

pub unsafe extern "C" fn CGPathAddRects(
    path: CGMutablePathRef,
    m: *const CGAffineTransform,
    rects: *const CGRect,
    count: usize,
) {
    let Some(e) = elements_of(path) else {
        return;
    };
    if rects.is_null() {
        return;
    }
    // SAFETY: the caller supplies `count` rectangles.
    for rect in unsafe { std::slice::from_raw_parts(rects, count) } {
        e.rect(transform(m), *rect);
    }
}

pub unsafe extern "C" fn CGPathAddEllipseInRect(
    path: CGMutablePathRef,
    m: *const CGAffineTransform,
    rect: CGRect,
) {
    if let Some(e) = elements_of(path) {
        e.ellipse(transform(m), rect);
    }
}

/// `void CGPathAddRoundedRect(CGMutablePathRef path, const CGAffineTransform
/// *m, CGRect rect, CGFloat cornerWidth, CGFloat cornerHeight)`
pub unsafe extern "C" fn CGPathAddRoundedRect(
    path: CGMutablePathRef,
    m: *const CGAffineTransform,
    rect: CGRect,
    corner_width: CGFloat,
    corner_height: CGFloat,
) {
    if let Some(e) = elements_of(path) {
        e.rounded_rect(transform(m), rect, corner_width, corner_height);
    }
}

#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CGPathAddArc(
    path: CGMutablePathRef,
    m: *const CGAffineTransform,
    x: CGFloat,
    y: CGFloat,
    radius: CGFloat,
    start: CGFloat,
    end: CGFloat,
    clockwise: bool,
) {
    if let Some(e) = elements_of(path) {
        e.arc(transform(m), x, y, radius, start, end, clockwise);
    }
}

#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CGPathAddArcToPoint(
    path: CGMutablePathRef,
    m: *const CGAffineTransform,
    x1: CGFloat,
    y1: CGFloat,
    x2: CGFloat,
    y2: CGFloat,
    radius: CGFloat,
) {
    if let Some(e) = elements_of(path) {
        e.arc_to(transform(m), x1, y1, x2, y2, radius);
    }
}

/// `void CGPathAddRelativeArc(CGMutablePathRef path, const CGAffineTransform
/// *m, CGFloat x, CGFloat y, CGFloat radius, CGFloat startAngle, CGFloat delta)`
pub unsafe extern "C" fn CGPathAddRelativeArc(
    path: CGMutablePathRef,
    m: *const CGAffineTransform,
    x: CGFloat,
    y: CGFloat,
    radius: CGFloat,
    start: CGFloat,
    delta: CGFloat,
) {
    if let Some(e) = elements_of(path) {
        e.arc(
            transform(m),
            x,
            y,
            radius,
            start,
            start + delta,
            delta < 0.0,
        );
    }
}

pub unsafe extern "C" fn CGPathAddLines(
    path: CGMutablePathRef,
    m: *const CGAffineTransform,
    points: *const CGPoint,
    count: usize,
) {
    let Some(e) = elements_of(path) else {
        return;
    };
    if points.is_null() || count == 0 {
        return;
    }
    // SAFETY: the caller supplies `count` points.
    let points = unsafe { std::slice::from_raw_parts(points, count) };
    e.move_to(transform(m), points[0].x, points[0].y);
    for p in &points[1..] {
        e.line_to(transform(m), p.x, p.y);
    }
}

pub unsafe extern "C" fn CGPathAddPath(
    path: CGMutablePathRef,
    m: *const CGAffineTransform,
    other: CGPathRef,
) {
    let Some(source) = elements_of(other).map(|e| e.clone()) else {
        return;
    };
    if let Some(e) = elements_of(path) {
        e.append(transform(m), &source);
    }
}

pub unsafe extern "C" fn CGPathIsEmpty(path: CGPathRef) -> bool {
    elements_of(path).is_none_or(|e| e.is_empty())
}

/// `bool CGPathIsRect(CGPathRef path, CGRect *rect)`
pub unsafe extern "C" fn CGPathIsRect(path: CGPathRef, rect: *mut CGRect) -> bool {
    let Some(elements) = elements_of(path) else {
        return false;
    };
    let Some(found) = elements.as_rect() else {
        return false;
    };
    if !rect.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *rect = found };
    }
    true
}

pub unsafe extern "C" fn CGPathGetBoundingBox(path: CGPathRef) -> CGRect {
    elements_of(path)
        .and_then(|e| e.bounds())
        .unwrap_or(NULL_RECT)
}

/// `CGPathGetPathBoundingBox`, which leaves the control points out. The
/// rasterizer's tight bounds are exactly that.
pub unsafe extern "C" fn CGPathGetPathBoundingBox(path: CGPathRef) -> CGRect {
    let Some(skia) = elements_of(path).and_then(|e| e.to_skia()) else {
        return NULL_RECT;
    };
    let tight = skia.compute_tight_bounds().unwrap_or(skia.bounds());
    CGRect {
        origin: CGPoint {
            x: CGFloat::from(tight.left()),
            y: CGFloat::from(tight.top()),
        },
        size: CGSize {
            width: CGFloat::from(tight.width()),
            height: CGFloat::from(tight.height()),
        },
    }
}

pub unsafe extern "C" fn CGPathGetCurrentPoint(path: CGPathRef) -> CGPoint {
    elements_of(path)
        .and_then(|e| e.current)
        .unwrap_or_default()
}

/// `bool CGPathContainsPoint(CGPathRef, const CGAffineTransform *, CGPoint,
/// bool eoFill)`: whether the point is inside the path under the given rule.
pub unsafe extern "C" fn CGPathContainsPoint(
    path: CGPathRef,
    m: *const CGAffineTransform,
    point: CGPoint,
    even_odd: bool,
) -> bool {
    let Some(source) = elements_of(path) else {
        return false;
    };
    let mut moved = Elements::default();
    moved.append(transform(m), source);
    let p = point;
    // A crossing count rather than a rasterized pixel: the question is about
    // the point, and a subpath with no area - a dashed copy is a run of
    // zero-width lines - has no coverage for a fill to find.
    let w = winding(&moved, p);
    let inside = match even_odd {
        true => w % 2 != 0,
        false => w != 0,
    };
    // A point on the boundary is inside, which a crossing count alone cannot
    // say.
    inside || on_path(&moved, p, 1e-6)
}

/// `bool CGPathEqualToPath(CGPathRef path1, CGPathRef path2)`
pub unsafe extern "C" fn CGPathEqualToPath(path1: CGPathRef, path2: CGPathRef) -> bool {
    match (elements_of(path1), elements_of(path2)) {
        (Some(a), Some(b)) => a == b,
        (None, None) => true,
        _ => false,
    }
}

/// `void CGPathApply(CGPathRef, void *info, CGPathApplierFunction)`
pub unsafe extern "C" fn CGPathApply(
    path: CGPathRef,
    info: *mut c_void,
    function: Option<unsafe extern "C" fn(*mut c_void, *const CGPathElement)>,
) {
    let (Some(elements), Some(function)) = (elements_of(path), function) else {
        return;
    };
    let list = elements.list.clone();
    for element in list {
        let (element_type, points): (CGPathElementType, Vec<CGPoint>) = match element {
            CGPathElementType::kCGPathElementMoveToPoint(a) => {
                (CGPathElementType::kCGPathElementMoveToPoint(a), vec![a])
            }
            CGPathElementType::kCGPathElementAddLineToPoint(a) => {
                (CGPathElementType::kCGPathElementAddLineToPoint(a), vec![a])
            }
            CGPathElementType::kCGPathElementAddQuadCurveToPoint(c, a) => (
                CGPathElementType::kCGPathElementAddQuadCurveToPoint(c, a),
                vec![c, a],
            ),
            CGPathElementType::kCGPathElementAddCurveToPoint(c1, c2, a) => (
                CGPathElementType::kCGPathElementAddCurveToPoint(c1, c2, a),
                vec![c1, c2, a],
            ),
            CGPathElementType::kCGPathElementCloseSubpath => {
                (CGPathElementType::kCGPathElementCloseSubpath, vec![])
            }
        };
        let element = CGPathElement {
            element_type,
            points: points.as_ptr(),
        };
        // SAFETY: the applier's declared signature, and an element that lives
        // across the call.
        unsafe { function(info, &element) };
    }
}

/// `void CGPathApplyWithBlock(CGPathRef path, CGPathApplierBlock block)`
///
/// The block's invoke pointer must already be host-callable; a guest block
/// reaches that state through the runtime's block substitution.
pub unsafe extern "C" fn CGPathApplyWithBlock(path: CGPathRef, block: *const c_void) {
    let Some(elements) = elements_of(path) else {
        return;
    };
    let Some(invoke) = (unsafe { objc::block::invoke(block.cast_mut()) }) else {
        return;
    };
    // SAFETY: the block's declared signature is `void (^)(const CGPathElement *)`.
    let call: unsafe extern "C" fn(*mut c_void, *const CGPathElement) =
        unsafe { std::mem::transmute(invoke) };
    let list = elements.list.clone();
    for element in list {
        let (element_type, points): (CGPathElementType, Vec<CGPoint>) = match element {
            CGPathElementType::kCGPathElementMoveToPoint(a) => {
                (CGPathElementType::kCGPathElementMoveToPoint(a), vec![a])
            }
            CGPathElementType::kCGPathElementAddLineToPoint(a) => {
                (CGPathElementType::kCGPathElementAddLineToPoint(a), vec![a])
            }
            CGPathElementType::kCGPathElementAddQuadCurveToPoint(c, a) => (
                CGPathElementType::kCGPathElementAddQuadCurveToPoint(c, a),
                vec![c, a],
            ),
            CGPathElementType::kCGPathElementAddCurveToPoint(c1, c2, a) => (
                CGPathElementType::kCGPathElementAddCurveToPoint(c1, c2, a),
                vec![c1, c2, a],
            ),
            CGPathElementType::kCGPathElementCloseSubpath => {
                (CGPathElementType::kCGPathElementCloseSubpath, vec![])
            }
        };
        let element = CGPathElement {
            element_type,
            points: points.as_ptr(),
        };
        // SAFETY: the block, and an element that lives across the call.
        unsafe { call(block.cast_mut(), &element) };
    }
}

/// `CFTypeID CGPathGetTypeID(void)`
pub unsafe extern "C" fn CGPathGetTypeID() -> corefoundation::base::CFTypeID {
    let Some(class) = objc::class::lookup(c"__CGPath") else {
        return 0;
    };
    corefoundation::base::type_id(class)
}

/// `CGRectNull`.
const NULL_RECT: CGRect = CGRect {
    origin: CGPoint {
        x: CGFloat::INFINITY,
        y: CGFloat::INFINITY,
    },
    size: CGSize {
        width: 0.0,
        height: 0.0,
    },
};

exports::exports! {
    functions {
        "CGPathCreateMutable" => CGPathCreateMutable,
        "CGPathCreateCopy" => CGPathCreateCopy,
        "CGPathCreateMutableCopy" => CGPathCreateMutableCopy,
        "CGPathCreateCopyByTransformingPath" => CGPathCreateCopyByTransformingPath,
        "CGPathCreateMutableCopyByTransformingPath" => CGPathCreateMutableCopyByTransformingPath,
        "CGPathCreateWithRect" => CGPathCreateWithRect,
        "CGPathCreateWithEllipseInRect" => CGPathCreateWithEllipseInRect,
        "CGPathCreateWithRoundedRect" => CGPathCreateWithRoundedRect,
        "CGPathCreateCopyByDashingPath" => CGPathCreateCopyByDashingPath,
        "CGPathCreateCopyByStrokingPath" => CGPathCreateCopyByStrokingPath,
        "CGPathCreateCopyByFlattening" => CGPathCreateCopyByFlattening,
        "CGPathCreateCopyByNormalizing" => CGPathCreateCopyByNormalizing,
        "CGPathCreateCopyByUnioningPath" => CGPathCreateCopyByUnioningPath,
        "CGPathCreateCopyByIntersectingPath" => CGPathCreateCopyByIntersectingPath,
        "CGPathCreateCopyBySubtractingPath" => CGPathCreateCopyBySubtractingPath,
        "CGPathCreateCopyBySymmetricDifferenceOfPath" => CGPathCreateCopyBySymmetricDifferenceOfPath,
        "CGPathRetain" => CGPathRetain,
        "CGPathRelease" => CGPathRelease,
        "CGPathMoveToPoint" => CGPathMoveToPoint,
        "CGPathAddLineToPoint" => CGPathAddLineToPoint,
        "CGPathAddQuadCurveToPoint" => CGPathAddQuadCurveToPoint,
        "CGPathAddCurveToPoint" => CGPathAddCurveToPoint,
        "CGPathCloseSubpath" => CGPathCloseSubpath,
        "CGPathAddRect" => CGPathAddRect,
        "CGPathAddRects" => CGPathAddRects,
        "CGPathAddEllipseInRect" => CGPathAddEllipseInRect,
        "CGPathAddRoundedRect" => CGPathAddRoundedRect,
        "CGPathAddArc" => CGPathAddArc,
        "CGPathAddArcToPoint" => CGPathAddArcToPoint,
        "CGPathAddRelativeArc" => CGPathAddRelativeArc,
        "CGPathAddLines" => CGPathAddLines,
        "CGPathAddPath" => CGPathAddPath,
        "CGPathIsEmpty" => CGPathIsEmpty,
        "CGPathIsRect" => CGPathIsRect,
        "CGPathGetBoundingBox" => CGPathGetBoundingBox,
        "CGPathGetPathBoundingBox" => CGPathGetPathBoundingBox,
        "CGPathGetCurrentPoint" => CGPathGetCurrentPoint,
        "CGPathContainsPoint" => CGPathContainsPoint,
        "CGPathEqualToPath" => CGPathEqualToPath,
        "CGPathApply" => CGPathApply,
        "CGPathApplyWithBlock" => CGPathApplyWithBlock,
        "CGPathGetTypeID" => CGPathGetTypeID,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn count(info: *mut c_void, element: *const CGPathElement) {
        // SAFETY: a `Vec<CGPathElementType>` the test passed, and a live element.
        unsafe { (*(info as *mut Vec<CGPathElementType>)).push((*element).element_type) };
    }

    /// A rectangle is a move, three lines and a close, which is what a program
    /// walking it with `CGPathApply` expects.
    #[test]
    fn a_rect_applies_as_move_three_lines_and_close() {
        crate::bootstrap();
        let mut kinds: Vec<CGPathElementType> = Vec::new();
        // SAFETY: a path this test made, and an applier for `kinds`.
        unsafe {
            let path = CGPathCreateMutable();
            CGPathAddRect(
                path,
                std::ptr::null(),
                CGRect {
                    origin: CGPoint { x: 1.0, y: 2.0 },
                    size: CGSize {
                        width: 3.0,
                        height: 4.0,
                    },
                },
            );
            CGPathApply(path, (&raw mut kinds).cast(), Some(count));
            assert_eq!(
                CGPathGetBoundingBox(path),
                CGRect {
                    origin: CGPoint { x: 1.0, y: 2.0 },
                    size: CGSize {
                        width: 3.0,
                        height: 4.0
                    },
                }
            );
            assert!(CGPathContainsPoint(
                path,
                std::ptr::null(),
                CGPoint { x: 2.0, y: 3.0 },
                false
            ));
            assert!(!CGPathContainsPoint(
                path,
                std::ptr::null(),
                CGPoint { x: 9.0, y: 3.0 },
                false
            ));
            CGPathRelease(path);
        }
        assert_eq!(
            kinds,
            [
                CGPathElementType::kCGPathElementMoveToPoint(CGPoint { x: 1.0, y: 2.0 }),
                CGPathElementType::kCGPathElementAddLineToPoint(CGPoint { x: 4.0, y: 2.0 }),
                CGPathElementType::kCGPathElementAddLineToPoint(CGPoint { x: 4.0, y: 6.0 }),
                CGPathElementType::kCGPathElementAddLineToPoint(CGPoint { x: 1.0, y: 6.0 }),
                CGPathElementType::kCGPathElementCloseSubpath,
            ]
        );
    }

    /// A quarter arc ends where the angles say, whichever way it turns.
    #[test]
    fn an_arc_ends_at_its_end_angle() {
        let mut e = Elements::default();
        e.arc(
            CGAffineTransformIdentity,
            0.0,
            0.0,
            10.0,
            0.0,
            std::f64::consts::FRAC_PI_2,
            false,
        );
        let end = e.current.expect("a current point");
        assert!((end.x).abs() < 1e-9 && (end.y - 10.0).abs() < 1e-9);
    }

    /// A rounded rectangle covers the rectangle it was given, corners included.
    #[test]
    fn a_rounded_rect_has_the_rects_bounds() {
        let mut e = Elements::default();
        e.rounded_rect(
            CGAffineTransformIdentity,
            CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: 10.0,
                    height: 10.0,
                },
            },
            2.0,
            2.0,
        );
        assert!(!e.is_empty());
        assert_eq!(
            e.bounds(),
            Some(CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: 10.0,
                    height: 10.0,
                },
            })
        );
    }

    /// Only a path that is exactly one axis-aligned rectangle answers with it.
    #[test]
    fn only_a_rect_is_a_rect() {
        let mut rect = Elements::default();
        rect.rect(
            CGAffineTransformIdentity,
            CGRect {
                origin: CGPoint { x: 1.0, y: 2.0 },
                size: CGSize {
                    width: 3.0,
                    height: 4.0,
                },
            },
        );
        assert_eq!(
            rect.as_rect(),
            Some(CGRect {
                origin: CGPoint { x: 1.0, y: 2.0 },
                size: CGSize {
                    width: 3.0,
                    height: 4.0,
                },
            })
        );

        // Quartz only accepts the order `CGPathCreateWithRect` emits, so a
        // rectangle that starts at another corner is not one.
        let mut turned = Elements::default();
        turned.move_to(CGAffineTransformIdentity, 10.0, 10.0);
        turned.line_to(CGAffineTransformIdentity, 5.0, 10.0);
        turned.line_to(CGAffineTransformIdentity, 5.0, 5.0);
        turned.line_to(CGAffineTransformIdentity, 10.0, 5.0);
        turned.close();
        assert_eq!(turned.as_rect(), None);

        let mut triangle = Elements::default();
        triangle.move_to(CGAffineTransformIdentity, 0.0, 0.0);
        triangle.line_to(CGAffineTransformIdentity, 1.0, 0.0);
        triangle.line_to(CGAffineTransformIdentity, 0.0, 1.0);
        triangle.close();
        assert_eq!(triangle.as_rect(), None);
    }

    #[test]
    fn flattening_leaves_only_lines_and_keeps_the_endpoints() {
        let mut e = Elements::default();
        e.move_to(CGAffineTransformIdentity, 0.0, 0.0);
        e.cubic_to(CGAffineTransformIdentity, 0.0, 10.0, 10.0, 10.0, 10.0, 0.0);
        let flat = e.flattened(0.1);
        assert!(flat.list.iter().all(|element| matches!(
            element,
            CGPathElementType::kCGPathElementMoveToPoint(_)
                | CGPathElementType::kCGPathElementAddLineToPoint(_)
                | CGPathElementType::kCGPathElementCloseSubpath
        )));
        assert!(flat.list.len() > 2, "a curve should become several lines");
        assert_eq!(flat.current, Some(CGPoint { x: 10.0, y: 0.0 }));
    }

    #[test]
    fn a_rect_is_one_contour_of_four_points() {
        let mut e = Elements::default();
        e.rect(
            CGAffineTransformIdentity,
            crate::CGRect {
                origin: crate::CGPoint { x: 0.0, y: 0.0 },
                size: crate::CGSize {
                    width: 10.0,
                    height: 20.0,
                },
            },
        );
        let contours = e.contours(0.1);
        assert_eq!(contours.len(), 1);
        assert_eq!(contours[0].len(), 4);
    }

    #[test]
    fn intersecting_two_rects_is_their_overlap() {
        crate::bootstrap();
        let a = unsafe {
            CGPathCreateWithRect(
                crate::CGRect {
                    origin: crate::CGPoint { x: 0.0, y: 0.0 },
                    size: crate::CGSize {
                        width: 10.0,
                        height: 10.0,
                    },
                },
                std::ptr::null(),
            )
        };
        let b = unsafe {
            CGPathCreateWithRect(
                crate::CGRect {
                    origin: crate::CGPoint { x: 5.0, y: 5.0 },
                    size: crate::CGSize {
                        width: 10.0,
                        height: 10.0,
                    },
                },
                std::ptr::null(),
            )
        };
        let overlap = unsafe { CGPathCreateCopyByIntersectingPath(a, b, false) };
        let bounds = unsafe { CGPathGetBoundingBox(overlap) };
        assert!((bounds.origin.x - 5.0).abs() < 1e-6);
        assert!((bounds.origin.y - 5.0).abs() < 1e-6);
        assert!((bounds.size.width - 5.0).abs() < 1e-6);
        assert!((bounds.size.height - 5.0).abs() < 1e-6);
        unsafe {
            CGPathRelease(a);
            CGPathRelease(b);
            CGPathRelease(overlap);
        }
    }

    fn close_to(a: CGFloat, b: CGFloat) -> bool {
        (a - b).abs() < 1e-9
    }

    /// `arc_to` emits the tangent line once, through `arc`, and an exact
    /// quarter turn is a single cubic.
    #[test]
    fn an_arc_to_point_is_one_line_and_one_quarter_turn() {
        let mut e = Elements::default();
        e.move_to(CGAffineTransformIdentity, 0.0, 0.0);
        e.arc_to(CGAffineTransformIdentity, 100.0, 0.0, 100.0, 100.0, 20.0);
        assert_eq!(e.list.len(), 3, "{:?}", e.list);
        assert_eq!(
            e.list[0],
            CGPathElementType::kCGPathElementMoveToPoint(CGPoint { x: 0.0, y: 0.0 })
        );
        match e.list[1] {
            CGPathElementType::kCGPathElementAddLineToPoint(p) => {
                assert!(close_to(p.x, 80.0) && close_to(p.y, 0.0), "{:?}", e.list[1]);
            }
            ref other => panic!("expected a line, got {other:?}"),
        }
        match e.list[2] {
            CGPathElementType::kCGPathElementAddCurveToPoint(_, _, p) => {
                assert!(
                    close_to(p.x, 100.0) && close_to(p.y, 20.0),
                    "{:?}",
                    e.list[2]
                );
            }
            ref other => panic!("expected a cubic, got {other:?}"),
        }
    }

    /// Quartz starts a rounded rectangle at the middle of its right edge.
    #[test]
    fn a_rounded_rect_starts_at_the_middle_of_its_right_edge() {
        let mut e = Elements::default();
        e.rounded_rect(
            CGAffineTransformIdentity,
            CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: 10.0,
                    height: 8.0,
                },
            },
            2.0,
            2.0,
        );
        assert_eq!(
            e.list[0],
            CGPathElementType::kCGPathElementMoveToPoint(CGPoint { x: 10.0, y: 4.0 })
        );
        assert_eq!(e.current, Some(CGPoint { x: 10.0, y: 4.0 }));
    }

    /// A point on a zero-width subpath is inside, even though a fill test sees
    /// no coverage there.
    #[test]
    fn a_point_on_a_zero_width_subpath_is_inside() {
        let mut e = Elements::default();
        e.move_to(CGAffineTransformIdentity, 0.0, 0.0);
        e.line_to(CGAffineTransformIdentity, 10.0, 0.0);
        assert!(on_path(&e, CGPoint { x: 5.0, y: 0.0 }, 1e-6));
        assert!(!on_path(&e, CGPoint { x: 5.0, y: 1.0 }, 1e-6));
    }

    /// An open arc contains a point inside it, because the ray crosses it once.
    #[test]
    fn an_open_arc_contains_a_point_inside_it() {
        let mut e = Elements::default();
        e.move_to(CGAffineTransformIdentity, 9.0, 5.0);
        e.cubic_to(CGAffineTransformIdentity, 9.0, 7.2, 7.2, 9.0, 5.0, 9.0);
        e.cubic_to(CGAffineTransformIdentity, 2.8, 9.0, 1.0, 7.2, 1.0, 5.0);
        assert!(winding(&e, CGPoint { x: 5.0, y: 5.0 }) != 0);
        assert!(winding(&e, CGPoint { x: 5.0, y: 3.0 }) == 0);
    }

    /// An open subpath's chord is not part of the path.
    #[test]
    fn a_point_on_the_chord_of_an_open_subpath_is_outside() {
        let mut e = Elements::default();
        e.move_to(CGAffineTransformIdentity, 0.0, 0.0);
        e.line_to(CGAffineTransformIdentity, 0.0, 10.0);
        e.line_to(CGAffineTransformIdentity, 10.0, 10.0);
        assert!(winding(&e, CGPoint { x: 5.0, y: 5.0 }) == 0);
        assert!(!on_path(&e, CGPoint { x: 5.0, y: 5.0 }, 1e-6));
    }

    /// A subpath with no area still crosses the ray from a point beside it.
    #[test]
    fn a_zero_width_subpath_still_crosses_the_ray() {
        let mut e = Elements::default();
        e.move_to(CGAffineTransformIdentity, 10.0, 0.0);
        e.line_to(CGAffineTransformIdentity, 10.0, 10.0);
        assert!(winding(&e, CGPoint { x: 5.0, y: 5.0 }) != 0);
        assert!(winding(&e, CGPoint { x: 15.0, y: 5.0 }) == 0);
    }

    /// Only the order `CGPathCreateWithRect` emits is a rectangle.
    #[test]
    fn only_a_canonical_rect_is_a_rect() {
        let mut canonical = Elements::default();
        canonical.move_to(CGAffineTransformIdentity, 0.0, 0.0);
        canonical.line_to(CGAffineTransformIdentity, 10.0, 0.0);
        canonical.line_to(CGAffineTransformIdentity, 10.0, 10.0);
        canonical.line_to(CGAffineTransformIdentity, 0.0, 10.0);
        canonical.close();
        assert!(canonical.as_rect().is_some());

        let mut turned = Elements::default();
        turned.move_to(CGAffineTransformIdentity, 10.0, 10.0);
        turned.line_to(CGAffineTransformIdentity, 5.0, 10.0);
        turned.line_to(CGAffineTransformIdentity, 5.0, 5.0);
        turned.line_to(CGAffineTransformIdentity, 10.0, 5.0);
        turned.close();
        assert_eq!(turned.as_rect(), None);
    }
}
