//! `CGPath` and the path-building geometry a `CGContext`'s current path shares.
//!
//! A path is its elements, in the coordinates they were added in after the
//! transform passed with each. A context keeps its current path the same way,
//! with the CTM as that transform, which is when Quartz applies it.

#![allow(non_snake_case)]

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use foundation::geometry::{NSPoint, NSRect, NSSize};

use crate::cgcontext::{CGAffineTransform, IDENTITY};

pub type Point = (f64, f64);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Element {
    Move(Point),
    Line(Point),
    Quad(Point, Point),
    Cubic(Point, Point, Point),
    Close,
}

/// A sequence of elements, with the point the next element starts from.
#[derive(Clone, Debug, Default)]
pub struct Elements {
    pub list: Vec<Element>,
    /// The current point, if there is one.
    pub current: Option<Point>,
    /// Where the current subpath began, which a close returns to.
    start: Option<Point>,
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

    pub fn move_to(&mut self, m: CGAffineTransform, x: f64, y: f64) {
        let p = m.apply(x, y);
        self.list.push(Element::Move(p));
        self.current = Some(p);
        self.start = Some(p);
    }

    /// A line, or a move when there is nowhere to draw from - which is what
    /// Quartz does rather than refuse.
    pub fn line_to(&mut self, m: CGAffineTransform, x: f64, y: f64) {
        if self.current.is_none() {
            return self.move_to(m, x, y);
        }
        let p = m.apply(x, y);
        self.list.push(Element::Line(p));
        self.current = Some(p);
    }

    pub fn quad_to(&mut self, m: CGAffineTransform, cx: f64, cy: f64, x: f64, y: f64) {
        if self.current.is_none() {
            self.move_to(m, cx, cy);
        }
        let p = m.apply(x, y);
        self.list.push(Element::Quad(m.apply(cx, cy), p));
        self.current = Some(p);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn cubic_to(
        &mut self,
        m: CGAffineTransform,
        c1x: f64,
        c1y: f64,
        c2x: f64,
        c2y: f64,
        x: f64,
        y: f64,
    ) {
        if self.current.is_none() {
            self.move_to(m, c1x, c1y);
        }
        let p = m.apply(x, y);
        self.list
            .push(Element::Cubic(m.apply(c1x, c1y), m.apply(c2x, c2y), p));
        self.current = Some(p);
    }

    pub fn close(&mut self) {
        if self.current.is_none() || matches!(self.list.last(), Some(Element::Close) | None) {
            return;
        }
        self.list.push(Element::Close);
        self.current = self.start;
    }

    pub fn rect(&mut self, m: CGAffineTransform, r: NSRect) {
        let (x0, y0) = (r.origin.x, r.origin.y);
        let (x1, y1) = (x0 + r.size.width, y0 + r.size.height);
        self.move_to(m, x0, y0);
        self.line_to(m, x1, y0);
        self.line_to(m, x1, y1);
        self.line_to(m, x0, y1);
        self.close();
    }

    /// A rectangle whose corners are quarter ellipses `cw` wide and `ch` tall,
    /// counterclockwise from the middle of its right edge. A zero radius is a
    /// square corner.
    pub fn rounded_rect(&mut self, m: CGAffineTransform, r: NSRect, cw: f64, ch: f64) {
        let (x0, y0) = (r.origin.x, r.origin.y);
        let (x1, y1) = (x0 + r.size.width, y0 + r.size.height);
        let (kx, ky) = (cw * KAPPA, ch * KAPPA);
        self.move_to(m, x1, y0 + r.size.height / 2.0);
        self.line_to(m, x1, y1 - ch);
        self.cubic_to(m, x1, y1 - ch + ky, x1 - cw + kx, y1, x1 - cw, y1);
        self.line_to(m, x0 + cw, y1);
        self.cubic_to(m, x0 + cw - kx, y1, x0, y1 - ch + ky, x0, y1 - ch);
        self.line_to(m, x0, y0 + ch);
        self.cubic_to(m, x0, y0 + ch - ky, x0 + cw - kx, y0, x0 + cw, y0);
        self.line_to(m, x1 - cw, y0);
        self.cubic_to(m, x1 - cw + kx, y0, x1, y0 + ch - ky, x1, y0 + ch);
        self.close();
    }

    pub fn ellipse(&mut self, m: CGAffineTransform, r: NSRect) {
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
        x: f64,
        y: f64,
        radius: f64,
        start: f64,
        end: f64,
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
        let pieces = (sweep.abs() / (TAU / 4.0)).ceil().max(1.0) as usize;
        let step = sweep / pieces as f64;
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
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        radius: f64,
    ) {
        let Some(current) = self.current else {
            return self.move_to(m, x1, y1);
        };
        let Some(inverse) = m.inverse() else {
            return;
        };
        let (x0, y0) = inverse.apply(current.0, current.1);
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
        self.line_to(m, t0x, t0y);
        self.arc(m, cx, cy, radius, start, end, cross > 0.0);
    }

    /// Append `other`'s elements through `m`.
    pub fn append(&mut self, m: CGAffineTransform, other: &Elements) {
        for element in &other.list {
            match *element {
                Element::Move(p) => self.move_to(m, p.0, p.1),
                Element::Line(p) => self.line_to(m, p.0, p.1),
                Element::Quad(c, p) => self.quad_to(m, c.0, c.1, p.0, p.1),
                Element::Cubic(c1, c2, p) => self.cubic_to(m, c1.0, c1.1, c2.0, c2.1, p.0, p.1),
                Element::Close => self.close(),
            }
        }
    }

    /// The bounding box of every point, control points included, as
    /// `CGPathGetBoundingBox` answers.
    pub fn bounds(&self) -> Option<NSRect> {
        let mut points = self.list.iter().flat_map(|e| match *e {
            Element::Move(p) | Element::Line(p) => vec![p],
            Element::Quad(c, p) => vec![c, p],
            Element::Cubic(c1, c2, p) => vec![c1, c2, p],
            Element::Close => vec![],
        });
        let first = points.next()?;
        let (l, b, r, t) = points.fold((first.0, first.1, first.0, first.1), |(l, b, r, t), p| {
            (l.min(p.0), b.min(p.1), r.max(p.0), t.max(p.1))
        });
        Some(NSRect {
            origin: NSPoint { x: l, y: b },
            size: NSSize {
                width: r - l,
                height: t - b,
            },
        })
    }

    /// The path for the rasterizer, or `None` when it encloses nothing.
    pub fn to_skia(&self) -> Option<tiny_skia::Path> {
        let mut builder = tiny_skia::PathBuilder::new();
        let f = |p: Point| (p.0 as f32, p.1 as f32);
        for element in &self.list {
            match *element {
                Element::Move(p) => {
                    let (x, y) = f(p);
                    builder.move_to(x, y);
                }
                Element::Line(p) => {
                    let (x, y) = f(p);
                    builder.line_to(x, y);
                }
                Element::Quad(c, p) => {
                    let ((cx, cy), (x, y)) = (f(c), f(p));
                    builder.quad_to(cx, cy, x, y);
                }
                Element::Cubic(c1, c2, p) => {
                    let ((ax, ay), (bx, by), (x, y)) = (f(c1), f(c2), f(p));
                    builder.cubic_to(ax, ay, bx, by, x, y);
                }
                Element::Close => builder.close(),
            }
        }
        builder.finish()
    }

    /// The elements of a rasterizer path, as a stroke outline comes back.
    pub fn from_skia(path: &tiny_skia::Path, m: CGAffineTransform) -> Elements {
        use tiny_skia::PathSegment;
        let mut out = Elements::default();
        let g = |p: tiny_skia::Point| (f64::from(p.x), f64::from(p.y));
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
pub fn path_from(elements: Elements) -> CFTypeRef {
    new_path(elements)
}

/// A `__CGPath`: its elements, boxed so the object stays a fixed size.
#[repr(C)]
struct Path {
    isa: *const c_void,
    elements: *mut Elements,
}

pub fn install() {
    let path = corefoundation::base::register(c"__CGPath", None, size_of::<Path>(), true);
    path.replace_method(
        objc::sel::get("dealloc"),
        path_dealloc as *const c_void,
        c"v16@0:8",
    );
}

unsafe extern "C" fn path_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__CGPath`, whose elements came from `Box::into_raw`.
    unsafe {
        let path = &mut *(receiver as *mut Path);
        if !path.elements.is_null() {
            drop(Box::from_raw(path.elements));
            path.elements = std::ptr::null_mut();
        }
        objc::arc::finalize(receiver);
    }
}

/// The elements of a path object, or `None` for anything else.
pub fn elements_of<'a>(path: CFTypeRef) -> Option<&'a mut Elements> {
    let class = objc::class::lookup(c"__CGPath")?;
    // SAFETY: null or an object a guest received from this runtime.
    if path.is_null() || !unsafe { corefoundation::base::is_kind_of(path, class) } {
        return None;
    }
    // SAFETY: a `__CGPath`, created with elements.
    unsafe { (*(path as *mut Path)).elements.as_mut() }
}

fn new_path(elements: Elements) -> CFTypeRef {
    let class = objc::class::lookup(c"__CGPath").expect("installed during bootstrap");
    // SAFETY: `Path` is the registered class layout.
    let path = unsafe { corefoundation::base::allocate::<Path>(class) };
    // SAFETY: a fresh allocation.
    unsafe { (*path).elements = Box::into_raw(Box::new(elements)) };
    path as CFTypeRef
}

/// The transform a path call was given, or the identity for none.
fn transform(m: *const CGAffineTransform) -> CGAffineTransform {
    match m.is_null() {
        true => IDENTITY,
        // SAFETY: the caller's transform.
        false => unsafe { *m },
    }
}

pub unsafe extern "C" fn CGPathCreateMutable() -> CFTypeRef {
    new_path(Elements::default())
}

pub unsafe extern "C" fn CGPathCreateCopy(path: CFTypeRef) -> CFTypeRef {
    elements_of(path).map_or(std::ptr::null(), |e| new_path(e.clone()))
}

pub unsafe extern "C" fn CGPathCreateCopyByTransformingPath(
    path: CFTypeRef,
    m: *const CGAffineTransform,
) -> CFTypeRef {
    let Some(source) = elements_of(path) else {
        return std::ptr::null();
    };
    let mut out = Elements::default();
    out.append(transform(m), source);
    new_path(out)
}

pub unsafe extern "C" fn CGPathCreateWithRect(
    rect: NSRect,
    m: *const CGAffineTransform,
) -> CFTypeRef {
    let mut out = Elements::default();
    out.rect(transform(m), rect);
    new_path(out)
}

pub unsafe extern "C" fn CGPathCreateWithEllipseInRect(
    rect: NSRect,
    m: *const CGAffineTransform,
) -> CFTypeRef {
    let mut out = Elements::default();
    out.ellipse(transform(m), rect);
    new_path(out)
}

/// `rect` with its size made positive, as `CGRectStandardize` does.
fn standardized(rect: NSRect) -> NSRect {
    let (w, h) = (rect.size.width, rect.size.height);
    NSRect {
        origin: NSPoint {
            x: rect.origin.x + w.min(0.0),
            y: rect.origin.y + h.min(0.0),
        },
        size: NSSize {
            width: w.abs(),
            height: h.abs(),
        },
    }
}

/// Add a rounded rectangle, refusing the corner sizes Quartz asserts on: a
/// negative one, or one more than half the side it rounds.
fn add_rounded_rect(out: &mut Elements, m: CGAffineTransform, rect: NSRect, cw: f64, ch: f64) {
    let rect = standardized(rect);
    if !(0.0..=rect.size.width / 2.0).contains(&cw) || !(0.0..=rect.size.height / 2.0).contains(&ch)
    {
        objc::fail!(
            "CGPathAddRoundedRect: corner {cw}x{ch} does not fit {}x{}",
            rect.size.width,
            rect.size.height
        );
    }
    out.rounded_rect(m, rect, cw, ch);
}

/// `CGPathRef CGPathCreateWithRoundedRect(CGRect, CGFloat, CGFloat, const CGAffineTransform *)`
pub unsafe extern "C" fn CGPathCreateWithRoundedRect(
    rect: NSRect,
    corner_width: f64,
    corner_height: f64,
    m: *const CGAffineTransform,
) -> CFTypeRef {
    let mut out = Elements::default();
    add_rounded_rect(&mut out, transform(m), rect, corner_width, corner_height);
    new_path(out)
}

/// `void CGPathAddRoundedRect(CGMutablePathRef, const CGAffineTransform *, CGRect, CGFloat, CGFloat)`
pub unsafe extern "C" fn CGPathAddRoundedRect(
    path: CFTypeRef,
    m: *const CGAffineTransform,
    rect: NSRect,
    corner_width: f64,
    corner_height: f64,
) {
    if let Some(e) = elements_of(path) {
        add_rounded_rect(e, transform(m), rect, corner_width, corner_height);
    }
}

pub unsafe extern "C" fn CGPathRetain(path: CFTypeRef) -> CFTypeRef {
    if !path.is_null() {
        // SAFETY: a path the caller holds.
        unsafe { corefoundation::api::CFRetain(path) };
    }
    path
}

pub unsafe extern "C" fn CGPathRelease(path: CFTypeRef) {
    if !path.is_null() {
        // SAFETY: a reference the caller owns.
        unsafe { corefoundation::api::CFRelease(path) };
    }
}

pub unsafe extern "C" fn CGPathMoveToPoint(
    path: CFTypeRef,
    m: *const CGAffineTransform,
    x: f64,
    y: f64,
) {
    if let Some(e) = elements_of(path) {
        e.move_to(transform(m), x, y);
    }
}

pub unsafe extern "C" fn CGPathAddLineToPoint(
    path: CFTypeRef,
    m: *const CGAffineTransform,
    x: f64,
    y: f64,
) {
    if let Some(e) = elements_of(path) {
        e.line_to(transform(m), x, y);
    }
}

pub unsafe extern "C" fn CGPathAddQuadCurveToPoint(
    path: CFTypeRef,
    m: *const CGAffineTransform,
    cx: f64,
    cy: f64,
    x: f64,
    y: f64,
) {
    if let Some(e) = elements_of(path) {
        e.quad_to(transform(m), cx, cy, x, y);
    }
}

#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CGPathAddCurveToPoint(
    path: CFTypeRef,
    m: *const CGAffineTransform,
    c1x: f64,
    c1y: f64,
    c2x: f64,
    c2y: f64,
    x: f64,
    y: f64,
) {
    if let Some(e) = elements_of(path) {
        e.cubic_to(transform(m), c1x, c1y, c2x, c2y, x, y);
    }
}

pub unsafe extern "C" fn CGPathCloseSubpath(path: CFTypeRef) {
    if let Some(e) = elements_of(path) {
        e.close();
    }
}

pub unsafe extern "C" fn CGPathAddRect(path: CFTypeRef, m: *const CGAffineTransform, rect: NSRect) {
    if let Some(e) = elements_of(path) {
        e.rect(transform(m), rect);
    }
}

pub unsafe extern "C" fn CGPathAddRects(
    path: CFTypeRef,
    m: *const CGAffineTransform,
    rects: *const NSRect,
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
    path: CFTypeRef,
    m: *const CGAffineTransform,
    rect: NSRect,
) {
    if let Some(e) = elements_of(path) {
        e.ellipse(transform(m), rect);
    }
}

#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CGPathAddArc(
    path: CFTypeRef,
    m: *const CGAffineTransform,
    x: f64,
    y: f64,
    radius: f64,
    start: f64,
    end: f64,
    clockwise: u8,
) {
    if let Some(e) = elements_of(path) {
        e.arc(transform(m), x, y, radius, start, end, clockwise != 0);
    }
}

#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CGPathAddArcToPoint(
    path: CFTypeRef,
    m: *const CGAffineTransform,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    radius: f64,
) {
    if let Some(e) = elements_of(path) {
        e.arc_to(transform(m), x1, y1, x2, y2, radius);
    }
}

pub unsafe extern "C" fn CGPathAddLines(
    path: CFTypeRef,
    m: *const CGAffineTransform,
    points: *const NSPoint,
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
    path: CFTypeRef,
    m: *const CGAffineTransform,
    other: CFTypeRef,
) {
    let Some(source) = elements_of(other).map(|e| e.clone()) else {
        return;
    };
    if let Some(e) = elements_of(path) {
        e.append(transform(m), &source);
    }
}

pub unsafe extern "C" fn CGPathIsEmpty(path: CFTypeRef) -> bool {
    elements_of(path).is_none_or(|e| e.is_empty())
}

pub unsafe extern "C" fn CGPathGetBoundingBox(path: CFTypeRef) -> NSRect {
    elements_of(path)
        .and_then(|e| e.bounds())
        .unwrap_or(NULL_RECT)
}

/// `CGPathGetPathBoundingBox`, which leaves the control points out. The
/// rasterizer's tight bounds are exactly that.
pub unsafe extern "C" fn CGPathGetPathBoundingBox(path: CFTypeRef) -> NSRect {
    let Some(skia) = elements_of(path).and_then(|e| e.to_skia()) else {
        return NULL_RECT;
    };
    let tight = skia.compute_tight_bounds().unwrap_or(skia.bounds());
    NSRect {
        origin: NSPoint {
            x: f64::from(tight.left()),
            y: f64::from(tight.top()),
        },
        size: NSSize {
            width: f64::from(tight.width()),
            height: f64::from(tight.height()),
        },
    }
}

pub unsafe extern "C" fn CGPathGetCurrentPoint(path: CFTypeRef) -> NSPoint {
    elements_of(path)
        .and_then(|e| e.current)
        .map_or(NSPoint::default(), |(x, y)| NSPoint { x, y })
}

/// `bool CGPathContainsPoint(CGPathRef, const CGAffineTransform *, CGPoint,
/// bool eoFill)`: whether the point's pixel would be filled.
pub unsafe extern "C" fn CGPathContainsPoint(
    path: CFTypeRef,
    m: *const CGAffineTransform,
    point: NSPoint,
    even_odd: bool,
) -> bool {
    let Some(source) = elements_of(path) else {
        return false;
    };
    let mut moved = Elements::default();
    moved.append(transform(m), source);
    let Some(skia) = moved.to_skia() else {
        return false;
    };
    // Rasterize one pixel centred on the point, without antialiasing.
    let Some(mut mask) = tiny_skia::Mask::new(1, 1) else {
        return false;
    };
    let rule = match even_odd {
        true => tiny_skia::FillRule::EvenOdd,
        false => tiny_skia::FillRule::Winding,
    };
    let shift =
        tiny_skia::Transform::from_translate((0.5 - point.x) as f32, (0.5 - point.y) as f32);
    mask.fill_path(&skia, rule, false, shift);
    mask.data()[0] != 0
}

/// `CGPathElement`: a type and its points.
#[repr(C)]
pub struct PathElement {
    kind: i32,
    points: *const NSPoint,
}

/// `void CGPathApply(CGPathRef, void *info, CGPathApplierFunction)`
pub unsafe extern "C" fn CGPathApply(
    path: CFTypeRef,
    info: *mut c_void,
    function: Option<unsafe extern "C" fn(*mut c_void, *const PathElement)>,
) {
    let (Some(elements), Some(function)) = (elements_of(path), function) else {
        return;
    };
    let list = elements.list.clone();
    let p = |(x, y): Point| NSPoint { x, y };
    for element in list {
        let (kind, points): (i32, Vec<NSPoint>) = match element {
            Element::Move(a) => (0, vec![p(a)]),
            Element::Line(a) => (1, vec![p(a)]),
            Element::Quad(c, a) => (2, vec![p(c), p(a)]),
            Element::Cubic(c1, c2, a) => (3, vec![p(c1), p(c2), p(a)]),
            Element::Close => (4, vec![]),
        };
        let element = PathElement {
            kind,
            points: points.as_ptr(),
        };
        // SAFETY: the applier's declared signature, and an element that lives
        // across the call.
        unsafe { function(info, &element) };
    }
}

/// `CGRectNull`.
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

exports::exports! {
    functions {
        "CGPathCreateMutable" => CGPathCreateMutable,
        "CGPathCreateCopy" | "CGPathCreateMutableCopy" => CGPathCreateCopy,
        "CGPathCreateCopyByTransformingPath" | "CGPathCreateMutableCopyByTransformingPath" => CGPathCreateCopyByTransformingPath,
        "CGPathCreateWithRect" => CGPathCreateWithRect,
        "CGPathCreateWithEllipseInRect" => CGPathCreateWithEllipseInRect,
        "CGPathCreateWithRoundedRect" => CGPathCreateWithRoundedRect,
        "CGPathAddRoundedRect" => CGPathAddRoundedRect,
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
        "CGPathAddArc" => CGPathAddArc,
        "CGPathAddArcToPoint" => CGPathAddArcToPoint,
        "CGPathAddLines" => CGPathAddLines,
        "CGPathAddPath" => CGPathAddPath,
        "CGPathIsEmpty" => CGPathIsEmpty,
        "CGPathGetBoundingBox" => CGPathGetBoundingBox,
        "CGPathGetPathBoundingBox" => CGPathGetPathBoundingBox,
        "CGPathGetCurrentPoint" => CGPathGetCurrentPoint,
        "CGPathContainsPoint" => CGPathContainsPoint,
        "CGPathApply" => CGPathApply,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn count(info: *mut c_void, element: *const PathElement) {
        // SAFETY: a `Vec<i32>` the test passed, and a live element.
        unsafe { (*(info as *mut Vec<i32>)).push((*element).kind) };
    }

    /// A rounded rectangle fills its rectangle except the corners it rounds.
    #[test]
    fn a_rounded_rect_leaves_its_corners_out() {
        crate::bootstrap();
        let rect = NSRect {
            origin: NSPoint { x: 10.0, y: 20.0 },
            size: NSSize {
                width: 40.0,
                height: 30.0,
            },
        };
        let at = |x, y| NSPoint { x, y };
        // SAFETY: a path this test made and releases.
        unsafe {
            let path = CGPathCreateWithRoundedRect(rect, 8.0, 6.0, std::ptr::null());
            let bounds = CGPathGetBoundingBox(path);
            assert_eq!(bounds, rect);
            let inside = |p| CGPathContainsPoint(path, std::ptr::null(), p, false);
            assert!(inside(at(30.0, 35.0)), "the middle");
            assert!(inside(at(10.5, 35.0)), "the left edge between corners");
            assert!(inside(at(30.0, 20.5)), "the bottom edge between corners");
            for corner in [
                at(10.5, 20.5),
                at(49.5, 20.5),
                at(10.5, 49.5),
                at(49.5, 49.5),
            ] {
                assert!(!inside(corner), "{corner:?} is rounded off");
            }
            CGPathRelease(path);
        }
    }

    /// A rectangle is a move, three lines and a close, which is what a program
    /// walking it with `CGPathApply` expects.
    #[test]
    fn a_rect_applies_as_move_three_lines_and_close() {
        crate::bootstrap();
        let mut kinds: Vec<i32> = Vec::new();
        // SAFETY: a path this test made, and an applier for `kinds`.
        unsafe {
            let path = CGPathCreateMutable();
            CGPathAddRect(
                path,
                std::ptr::null(),
                NSRect {
                    origin: NSPoint { x: 1.0, y: 2.0 },
                    size: NSSize {
                        width: 3.0,
                        height: 4.0,
                    },
                },
            );
            CGPathApply(path, (&raw mut kinds).cast(), Some(count));
            assert_eq!(
                CGPathGetBoundingBox(path),
                NSRect {
                    origin: NSPoint { x: 1.0, y: 2.0 },
                    size: NSSize {
                        width: 3.0,
                        height: 4.0
                    },
                }
            );
            assert!(CGPathContainsPoint(
                path,
                std::ptr::null(),
                NSPoint { x: 2.0, y: 3.0 },
                false
            ));
            assert!(!CGPathContainsPoint(
                path,
                std::ptr::null(),
                NSPoint { x: 9.0, y: 3.0 },
                false
            ));
            CGPathRelease(path);
        }
        assert_eq!(kinds, [0, 1, 1, 1, 4]);
    }

    /// A quarter arc ends where the angles say, whichever way it turns.
    #[test]
    fn an_arc_ends_at_its_end_angle() {
        let mut e = Elements::default();
        e.arc(
            IDENTITY,
            0.0,
            0.0,
            10.0,
            0.0,
            std::f64::consts::FRAC_PI_2,
            false,
        );
        let end = e.current.expect("a current point");
        assert!((end.0).abs() < 1e-9 && (end.1 - 10.0).abs() < 1e-9);
    }
}
