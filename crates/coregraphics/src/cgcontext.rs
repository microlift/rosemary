#![allow(non_snake_case)]

//! `CGContext`: the graphics state and drawing calls, over the engine in
//! `crate::quartz`.

use std::ffi::{c_char, c_void};

use crate::font_attributes::{key, loaded, make_font, new_dictionary, release};
use crate::fonts::LoadedFace;
use crate::{CGAffineTransformIdentity, CGPoint, CGRect, CGRectApplyAffineTransform, CGSize};
use corefoundation::base::CFTypeRef;
use tiny_skia as sk;

use crate::cgpath::Elements;
use crate::quartz::{self, BitmapContext, Ink, Retained, State, state_of};
use crate::{CGAffineTransformMakeScale, CGAffineTransformMakeTranslation};

pub use crate::CGAffineTransform;

pub type CGContextRef = CFTypeRef;

pub fn install() {
    let context =
        corefoundation::base::register(c"__CGContext", None, size_of::<BitmapContext>(), true);
    context.replace_method(
        objc::sel::get("dealloc"),
        context_dealloc as *const c_void,
        c"v16@0:8",
    );
    crate::cgpaint::install();
}

unsafe extern "C" fn context_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__CGContext`, whose state came from `Box::into_raw`.
    unsafe {
        let context = &mut *(receiver as *mut BitmapContext);
        if !context.state.is_null() {
            drop(Box::from_raw(context.state));
            context.state = std::ptr::null_mut();
        }
        objc::arc::finalize(receiver);
    }
}

thread_local! {
    /// The focus context, and the focus it was last answered for.
    static FOCUS_CONTEXT: std::cell::Cell<(usize, Option<(usize, usize)>)> =
        const { std::cell::Cell::new((0, None)) };
}

/// The `CGContextRef` of the view being drawn, or null with nothing focused.
///
/// One per thread, its graphics state reset whenever the focus changes, since
/// AppKit hands each `-drawRect:` a fresh state. Its user space is the view's
/// own, in points, so drawing lands where the same rectangle would through
/// `NSImage`.
pub fn focus_context() -> CFTypeRef {
    let Some(device) = crate::focus::device() else {
        return std::ptr::null();
    };
    let focus = Some((device.key, device.depth));
    let (mut context, last) = FOCUS_CONTEXT.with(|cell| cell.get());
    if context == 0 {
        let class = objc::class::lookup(c"__CGContext").expect("installed during bootstrap");
        // SAFETY: `BitmapContext` is the registered class layout; the object
        // lives for the thread, so it is never released.
        let fresh = unsafe { corefoundation::base::allocate::<BitmapContext>(class) };
        unsafe {
            (*fresh).focus = true;
            (*fresh).channels = 4;
            (*fresh).state = Box::into_raw(Box::new(State::new(None)));
        }
        context = fresh as usize;
    } else if last != focus {
        // SAFETY: the context made above, whose state is never freed.
        let state = unsafe { &mut *(*(context as *mut BitmapContext)).state };
        *state = State::new(None);
    }
    FOCUS_CONTEXT.with(|cell| cell.set((context, focus)));
    context as CFTypeRef
}

/// Save the focus context's state alongside AppKit's graphics state.
pub fn focus_save() {
    let context = focus_context();
    // SAFETY: null or the focus context.
    unsafe { CGContextSaveGState(context) };
}

/// Restore the focus context's state alongside AppKit's graphics state.
pub fn focus_restore() {
    let context = focus_context();
    // SAFETY: null or the focus context.
    unsafe { CGContextRestoreGState(context) };
}

/// Apply an AppKit transform change to the focus context too.
pub fn focus_update_ctm(update: impl FnOnce(CGAffineTransform) -> CGAffineTransform) {
    if let Some(state) = gstate(focus_context()) {
        state.ctm = update(state.ctm);
    }
}

/// Run `body` with the focus context's clip, one coverage byte per device
/// pixel, when Quartz calls have clipped it.
pub fn with_focus_clip<R>(body: impl FnOnce(&[u8], u32) -> R) -> Option<R> {
    quartz::with_clip(focus_context(), body)
}

pub unsafe extern "C" fn CGContextRetain(context: CFTypeRef) -> CFTypeRef {
    if context.is_null() {
        return context;
    }
    unsafe { corefoundation::api::CFRetain(context) }
}

pub unsafe extern "C" fn CGContextRelease(context: CFTypeRef) {
    if !context.is_null() {
        unsafe { corefoundation::api::CFRelease(context) };
    }
}

pub unsafe extern "C" fn CGContextSaveGState(context: CFTypeRef) {
    if let Some(state) = state_of(context) {
        state.saved.push(state.current.clone());
    }
}

pub unsafe extern "C" fn CGContextRestoreGState(context: CFTypeRef) {
    if let Some(state) = state_of(context)
        && let Some(saved) = state.saved.pop()
    {
        state.current = saved;
    }
}

fn gstate<'a>(context: CFTypeRef) -> Option<&'a mut quartz::GState> {
    state_of(context).map(|state| &mut state.current)
}

pub unsafe extern "C" fn CGContextConcatCTM(context: CFTypeRef, transform: CGAffineTransform) {
    if let Some(state) = gstate(context) {
        state.ctm = transform.then(state.ctm);
    }
}

pub unsafe extern "C" fn CGContextGetCTM(context: CFTypeRef) -> CGAffineTransform {
    gstate(context).map_or(CGAffineTransformIdentity, |state| state.ctm)
}

/// `CGContextSetCTM`, private but exported, and used by Cairo to put back a
/// transform it read with `CGContextGetCTM`.
pub unsafe extern "C" fn CGContextSetCTM(context: CFTypeRef, transform: CGAffineTransform) {
    if let Some(state) = gstate(context) {
        state.ctm = transform;
    }
}

pub unsafe extern "C" fn CGContextTranslateCTM(context: CFTypeRef, x: f64, y: f64) {
    unsafe { CGContextConcatCTM(context, CGAffineTransformMakeTranslation(x, y)) };
}

pub unsafe extern "C" fn CGContextScaleCTM(context: CFTypeRef, x: f64, y: f64) {
    unsafe { CGContextConcatCTM(context, CGAffineTransformMakeScale(x, y)) };
}

pub unsafe extern "C" fn CGContextRotateCTM(context: CFTypeRef, angle: f64) {
    let (sin, cos) = angle.sin_cos();
    let rotation = CGAffineTransform {
        a: cos,
        b: sin,
        c: -sin,
        d: cos,
        ..CGAffineTransformIdentity
    };
    unsafe { CGContextConcatCTM(context, rotation) };
}

//
// Colours
//

pub unsafe extern "C" fn CGContextSetRGBFillColor(
    context: CFTypeRef,
    red: f64,
    green: f64,
    blue: f64,
    alpha: f64,
) {
    if let Some(state) = gstate(context) {
        state.fill = Ink::Color([red, green, blue, alpha]);
    }
}

pub unsafe extern "C" fn CGContextSetRGBStrokeColor(
    context: CFTypeRef,
    red: f64,
    green: f64,
    blue: f64,
    alpha: f64,
) {
    if let Some(state) = gstate(context) {
        state.stroke = Ink::Color([red, green, blue, alpha]);
    }
}

pub unsafe extern "C" fn CGContextSetGrayFillColor(context: CFTypeRef, gray: f64, alpha: f64) {
    unsafe { CGContextSetRGBFillColor(context, gray, gray, gray, alpha) };
}

pub unsafe extern "C" fn CGContextSetGrayStrokeColor(context: CFTypeRef, gray: f64, alpha: f64) {
    unsafe { CGContextSetRGBStrokeColor(context, gray, gray, gray, alpha) };
}

pub unsafe extern "C" fn CGContextSetFillColorWithColor(context: CFTypeRef, color: CFTypeRef) {
    if let (Some(state), Some(color)) = (gstate(context), crate::cgcolor::color_of(color)) {
        state.fill = Ink::Color(color.components);
    }
}

pub unsafe extern "C" fn CGContextSetStrokeColorWithColor(context: CFTypeRef, color: CFTypeRef) {
    if let (Some(state), Some(color)) = (gstate(context), crate::cgcolor::color_of(color)) {
        state.stroke = Ink::Color(color.components);
    }
}

pub unsafe extern "C" fn CGContextSetFillColorSpace(context: CFTypeRef, space: CFTypeRef) {
    if let Some(state) = gstate(context) {
        state.fill_components = crate::cgcolor::space_components(space).unwrap_or(3);
        // A new space resets the colour to its initial one, opaque black.
        state.fill = Ink::Color([0.0, 0.0, 0.0, 1.0]);
    }
}

pub unsafe extern "C" fn CGContextSetStrokeColorSpace(context: CFTypeRef, space: CFTypeRef) {
    if let Some(state) = gstate(context) {
        state.stroke_components = crate::cgcolor::space_components(space).unwrap_or(3);
        state.stroke = Ink::Color([0.0, 0.0, 0.0, 1.0]);
    }
}

pub unsafe extern "C" fn CGContextSetFillColor(context: CFTypeRef, components: *const f64) {
    if let Some(state) = gstate(context)
        && state.fill_components != 0
        && let Some(rgba) = unsafe { crate::cgcolor::rgba_from(components, state.fill_components) }
    {
        state.fill = Ink::Color(rgba);
    }
}

pub unsafe extern "C" fn CGContextSetStrokeColor(context: CFTypeRef, components: *const f64) {
    if let Some(state) = gstate(context)
        && state.stroke_components != 0
        && let Some(rgba) =
            unsafe { crate::cgcolor::rgba_from(components, state.stroke_components) }
    {
        state.stroke = Ink::Color(rgba);
    }
}

/// The ink for a pattern set with `components`: for an uncoloured pattern
/// its colour, read as RGB and alpha because every base space here is RGB.
///
/// # Safety
///
/// `components` is null or holds four values.
unsafe fn pattern_ink(pattern: CFTypeRef, components: *const f64) -> Option<Ink> {
    let pattern = Retained::new(pattern)?;
    let color = match components.is_null() {
        true => [0.0, 0.0, 0.0, 1.0],
        // SAFETY: the caller's obligation.
        false => {
            unsafe { crate::cgcolor::rgba_from(components, 3) }.unwrap_or([0.0, 0.0, 0.0, 1.0])
        }
    };
    Some(Ink::Pattern(pattern, color))
}

pub unsafe extern "C" fn CGContextSetFillPattern(
    context: CFTypeRef,
    pattern: CFTypeRef,
    components: *const f64,
) {
    if let Some(state) = gstate(context)
        && let Some(ink) = unsafe { pattern_ink(pattern, components) }
    {
        state.fill = ink;
    }
}

pub unsafe extern "C" fn CGContextSetStrokePattern(
    context: CFTypeRef,
    pattern: CFTypeRef,
    components: *const f64,
) {
    if let Some(state) = gstate(context)
        && let Some(ink) = unsafe { pattern_ink(pattern, components) }
    {
        state.stroke = ink;
    }
}

pub unsafe extern "C" fn CGContextSetPatternPhase(context: CFTypeRef, phase: CGSize) {
    if let Some(state) = gstate(context) {
        state.pattern_phase = (phase.width, phase.height);
    }
}

pub unsafe extern "C" fn CGContextSetAlpha(context: CFTypeRef, alpha: f64) {
    if let Some(state) = gstate(context) {
        state.alpha = alpha.clamp(0.0, 1.0);
    }
}

//
// Line style and compositing
//

pub unsafe extern "C" fn CGContextSetLineWidth(context: CFTypeRef, width: f64) {
    if let Some(state) = gstate(context) {
        state.line.width = width.max(0.0);
    }
}

pub unsafe extern "C" fn CGContextSetLineCap(context: CFTypeRef, cap: i32) {
    if let Some(state) = gstate(context) {
        state.line.cap = match cap {
            1 => sk::LineCap::Round,
            2 => sk::LineCap::Square,
            _ => sk::LineCap::Butt,
        };
    }
}

pub unsafe extern "C" fn CGContextSetLineJoin(context: CFTypeRef, join: i32) {
    if let Some(state) = gstate(context) {
        state.line.join = match join {
            1 => sk::LineJoin::Round,
            2 => sk::LineJoin::Bevel,
            _ => sk::LineJoin::Miter,
        };
    }
}

pub unsafe extern "C" fn CGContextSetMiterLimit(context: CFTypeRef, limit: f64) {
    if let Some(state) = gstate(context) {
        state.line.miter = limit.max(0.0);
    }
}

pub unsafe extern "C" fn CGContextSetLineDash(
    context: CFTypeRef,
    phase: f64,
    lengths: *const f64,
    count: usize,
) {
    let Some(state) = gstate(context) else {
        return;
    };
    state.line.dash = match lengths.is_null() || count == 0 {
        true => None,
        // SAFETY: the caller supplies `count` lengths.
        false => Some((
            unsafe { std::slice::from_raw_parts(lengths, count) }.to_vec(),
            phase,
        )),
    };
}

/// Quartz's flatness is how far a curve may stray from its line segments;
/// the rasterizer here flattens to its own tolerance.
unsafe extern "C" fn CGContextSetFlatness(_context: CFTypeRef, _flatness: f64) {}

/// `CGBlendMode` as the rasterizer's.
fn blend_mode(mode: i32) -> sk::BlendMode {
    use sk::BlendMode as B;
    match mode {
        1 => B::Multiply,
        2 => B::Screen,
        3 => B::Overlay,
        4 => B::Darken,
        5 => B::Lighten,
        6 => B::ColorDodge,
        7 => B::ColorBurn,
        8 => B::SoftLight,
        9 => B::HardLight,
        10 => B::Difference,
        11 => B::Exclusion,
        12 => B::Hue,
        13 => B::Saturation,
        14 => B::Color,
        15 => B::Luminosity,
        16 => B::Clear,
        17 => B::Source,
        18 => B::SourceIn,
        19 => B::SourceOut,
        20 => B::SourceAtop,
        21 => B::DestinationOver,
        22 => B::DestinationIn,
        23 => B::DestinationOut,
        24 => B::DestinationAtop,
        25 => B::Xor,
        // FIXME (ACCURACY): `kCGBlendModePlusDarker` has no counterpart in
        // the rasterizer and draws as normal.
        27 => B::Plus,
        _ => B::SourceOver,
    }
}

pub unsafe extern "C" fn CGContextSetBlendMode(context: CFTypeRef, mode: i32) {
    if let Some(state) = gstate(context) {
        state.blend = blend_mode(mode);
    }
}

/// `CGContextSetCompositeOperation`, private: an `NSCompositingOperation`,
/// which is how AppKit and Cairo ask for the Porter-Duff operators.
pub unsafe extern "C" fn CGContextSetCompositeOperation(context: CFTypeRef, operation: i32) {
    use sk::BlendMode as B;
    let mode = match operation {
        0 => B::Clear,
        1 => B::Source,
        3 => B::SourceIn,
        4 => B::SourceOut,
        5 => B::SourceAtop,
        6 => B::DestinationOver,
        7 => B::DestinationIn,
        8 => B::DestinationOut,
        9 => B::DestinationAtop,
        10 => B::Xor,
        13 => B::Plus,
        // Source-over, highlight (which is source-over), and plus-darker,
        // which has no counterpart - see `blend_mode`.
        _ => B::SourceOver,
    };
    if let Some(state) = gstate(context) {
        state.blend = mode;
    }
}

pub unsafe extern "C" fn CGContextSetInterpolationQuality(context: CFTypeRef, quality: i32) {
    if let Some(state) = gstate(context) {
        // `kCGInterpolationNone` and `Low` sample; the rest filter.
        state.quality = match quality {
            1 | 2 => sk::FilterQuality::Nearest,
            _ => sk::FilterQuality::Bilinear,
        };
    }
}

pub unsafe extern "C" fn CGContextSetShouldAntialias(context: CFTypeRef, antialias: u8) {
    if let Some(state) = gstate(context) {
        state.antialias = antialias != 0;
    }
}

/// The font-rendering switches, none of which change what an outline fills
/// here: there is no subpixel positioning or LCD smoothing to turn off.
unsafe extern "C" fn ignored_flag(_context: CFTypeRef, _value: u8) {}

//
// The current path
//

fn path<'a>(context: CFTypeRef) -> Option<(&'a mut Elements, CGAffineTransform)> {
    let state = state_of(context)?;
    let ctm = state.current.ctm;
    Some((&mut state.path, ctm))
}

pub unsafe extern "C" fn CGContextBeginPath(context: CFTypeRef) {
    if let Some((path, _)) = path(context) {
        path.clear();
    }
}

pub unsafe extern "C" fn CGContextMoveToPoint(context: CFTypeRef, x: f64, y: f64) {
    if let Some((path, ctm)) = path(context) {
        path.move_to(ctm, x, y);
    }
}

pub unsafe extern "C" fn CGContextAddLineToPoint(context: CFTypeRef, x: f64, y: f64) {
    if let Some((path, ctm)) = path(context) {
        path.line_to(ctm, x, y);
    }
}

pub unsafe extern "C" fn CGContextAddQuadCurveToPoint(
    context: CFTypeRef,
    cx: f64,
    cy: f64,
    x: f64,
    y: f64,
) {
    if let Some((path, ctm)) = path(context) {
        path.quad_to(ctm, cx, cy, x, y);
    }
}

#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CGContextAddCurveToPoint(
    context: CFTypeRef,
    c1x: f64,
    c1y: f64,
    c2x: f64,
    c2y: f64,
    x: f64,
    y: f64,
) {
    if let Some((path, ctm)) = path(context) {
        path.cubic_to(ctm, c1x, c1y, c2x, c2y, x, y);
    }
}

pub unsafe extern "C" fn CGContextClosePath(context: CFTypeRef) {
    if let Some((path, _)) = path(context) {
        path.close();
    }
}

pub unsafe extern "C" fn CGContextAddRect(context: CFTypeRef, rect: CGRect) {
    if let Some((path, ctm)) = path(context) {
        path.rect(ctm, rect);
    }
}

pub unsafe extern "C" fn CGContextAddRects(context: CFTypeRef, rects: *const CGRect, count: usize) {
    let Some((path, ctm)) = path(context) else {
        return;
    };
    if rects.is_null() {
        return;
    }
    // SAFETY: the caller supplies `count` rectangles.
    for rect in unsafe { std::slice::from_raw_parts(rects, count) } {
        path.rect(ctm, *rect);
    }
}

pub unsafe extern "C" fn CGContextAddEllipseInRect(context: CFTypeRef, rect: CGRect) {
    if let Some((path, ctm)) = path(context) {
        path.ellipse(ctm, rect);
    }
}

#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CGContextAddArc(
    context: CFTypeRef,
    x: f64,
    y: f64,
    radius: f64,
    start: f64,
    end: f64,
    clockwise: i32,
) {
    if let Some((path, ctm)) = path(context) {
        path.arc(ctm, x, y, radius, start, end, clockwise != 0);
    }
}

pub unsafe extern "C" fn CGContextAddArcToPoint(
    context: CFTypeRef,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    radius: f64,
) {
    if let Some((path, ctm)) = path(context) {
        path.arc_to(ctm, x1, y1, x2, y2, radius);
    }
}

pub unsafe extern "C" fn CGContextAddLines(
    context: CFTypeRef,
    points: *const CGPoint,
    count: usize,
) {
    let Some((path, ctm)) = path(context) else {
        return;
    };
    if points.is_null() || count == 0 {
        return;
    }
    // SAFETY: the caller supplies `count` points.
    let points = unsafe { std::slice::from_raw_parts(points, count) };
    path.move_to(ctm, points[0].x, points[0].y);
    for p in &points[1..] {
        path.line_to(ctm, p.x, p.y);
    }
}

pub unsafe extern "C" fn CGContextAddPath(context: CFTypeRef, other: CFTypeRef) {
    let Some(source) = crate::cgpath::elements_of(other.cast()).map(|e| e.clone()) else {
        return;
    };
    if let Some((path, ctm)) = path(context) {
        path.append(ctm, &source);
    }
}

pub unsafe extern "C" fn CGContextIsPathEmpty(context: CFTypeRef) -> bool {
    path(context).is_none_or(|(path, _)| path.is_empty())
}

pub unsafe extern "C" fn CGContextGetPathCurrentPoint(context: CFTypeRef) -> CGPoint {
    let Some((path, ctm)) = path(context) else {
        return CGPoint::default();
    };
    match (path.current, ctm.inverse()) {
        (Some(p), Some(inverse)) => {
            let p = inverse.apply(p.x, p.y);
            CGPoint { x: p.x, y: p.y }
        }
        _ => CGPoint::default(),
    }
}

/// `CGContextGetPathBoundingBox`, in user space.
pub unsafe extern "C" fn CGContextGetPathBoundingBox(context: CFTypeRef) -> CGRect {
    let Some((path, ctm)) = path(context) else {
        return NULL_RECT;
    };
    let Some(inverse) = ctm.inverse() else {
        return NULL_RECT;
    };
    let mut user = Elements::default();
    user.append(inverse, path);
    user.bounds().unwrap_or(NULL_RECT)
}

pub unsafe extern "C" fn CGContextReplacePathWithStrokedPath(context: CFTypeRef) {
    let Some(current) = path(context).map(|(path, _)| path.clone()) else {
        return;
    };
    let outline = quartz::stroked_outline(context, &current).unwrap_or_default();
    if let Some((path, _)) = path(context) {
        *path = outline;
    }
}

/// The current path, taken: painting and clipping both consume it.
fn take_path(context: CFTypeRef) -> Option<Elements> {
    path(context).map(|(path, _)| std::mem::take(path))
}

//
// Painting
//

pub unsafe extern "C" fn CGContextFillPath(context: CFTypeRef) {
    if let Some(path) = take_path(context) {
        quartz::fill(context, &path, sk::FillRule::Winding, false);
    }
}

pub unsafe extern "C" fn CGContextEOFillPath(context: CFTypeRef) {
    if let Some(path) = take_path(context) {
        quartz::fill(context, &path, sk::FillRule::EvenOdd, false);
    }
}

pub unsafe extern "C" fn CGContextStrokePath(context: CFTypeRef) {
    if let Some(path) = take_path(context) {
        quartz::stroke(context, &path);
    }
}

/// `CGContextDrawPath(CGContextRef, CGPathDrawingMode)`
pub unsafe extern "C" fn CGContextDrawPath(context: CFTypeRef, mode: i32) {
    let Some(path) = take_path(context) else {
        return;
    };
    // `kCGPathFill`, `EOFill`, `Stroke`, `FillStroke`, `EOFillStroke`.
    match mode {
        0 | 3 => quartz::fill(context, &path, sk::FillRule::Winding, false),
        1 | 4 => quartz::fill(context, &path, sk::FillRule::EvenOdd, false),
        _ => {}
    }
    if matches!(mode, 2..=4) {
        quartz::stroke(context, &path);
    }
}

/// A one-off path through the CTM, for the calls that paint a shape without
/// touching the current path.
fn shape(
    context: CFTypeRef,
    build: impl FnOnce(&mut Elements, CGAffineTransform),
) -> Option<Elements> {
    let ctm = gstate(context)?.ctm;
    let mut shape = Elements::default();
    build(&mut shape, ctm);
    Some(shape)
}

pub unsafe extern "C" fn CGContextFillRect(context: CFTypeRef, rect: CGRect) {
    if let Some(shape) = shape(context, |e, m| e.rect(m, rect)) {
        quartz::fill(context, &shape, sk::FillRule::Winding, false);
    }
}

pub unsafe extern "C" fn CGContextFillRects(
    context: CFTypeRef,
    rects: *const CGRect,
    count: usize,
) {
    if rects.is_null() {
        return;
    }
    // SAFETY: the caller supplies `count` rectangles.
    let rects = unsafe { std::slice::from_raw_parts(rects, count) };
    if let Some(shape) = shape(context, |e, m| rects.iter().for_each(|r| e.rect(m, *r))) {
        quartz::fill(context, &shape, sk::FillRule::Winding, false);
    }
}

pub unsafe extern "C" fn CGContextStrokeRect(context: CFTypeRef, rect: CGRect) {
    if let Some(shape) = shape(context, |e, m| e.rect(m, rect)) {
        quartz::stroke(context, &shape);
    }
}

pub unsafe extern "C" fn CGContextStrokeRectWithWidth(
    context: CFTypeRef,
    rect: CGRect,
    width: f64,
) {
    let Some(state) = gstate(context) else {
        return;
    };
    let saved = state.line.width;
    state.line.width = width.max(0.0);
    unsafe { CGContextStrokeRect(context, rect) };
    if let Some(state) = gstate(context) {
        state.line.width = saved;
    }
}

pub unsafe extern "C" fn CGContextFillEllipseInRect(context: CFTypeRef, rect: CGRect) {
    if let Some(shape) = shape(context, |e, m| e.ellipse(m, rect)) {
        quartz::fill(context, &shape, sk::FillRule::Winding, false);
    }
}

pub unsafe extern "C" fn CGContextStrokeEllipseInRect(context: CFTypeRef, rect: CGRect) {
    if let Some(shape) = shape(context, |e, m| e.ellipse(m, rect)) {
        quartz::stroke(context, &shape);
    }
}

/// `CGContextStrokeLineSegments(CGContextRef, const CGPoint *, size_t)`: each
/// pair of points is a line of its own.
pub unsafe extern "C" fn CGContextStrokeLineSegments(
    context: CFTypeRef,
    points: *const CGPoint,
    count: usize,
) {
    if points.is_null() {
        return;
    }
    // SAFETY: the caller supplies `count` points.
    let points = unsafe { std::slice::from_raw_parts(points, count) };
    if let Some(shape) = shape(context, |e, m| {
        for pair in points.as_chunks::<2>().0 {
            e.move_to(m, pair[0].x, pair[0].y);
            e.line_to(m, pair[1].x, pair[1].y);
        }
    }) {
        quartz::stroke(context, &shape);
    }
}

pub unsafe extern "C" fn CGContextClearRect(context: CFTypeRef, rect: CGRect) {
    quartz::clear(context, rect);
}

//
// Clipping
//

pub unsafe extern "C" fn CGContextClip(context: CFTypeRef) {
    if let Some(path) = take_path(context) {
        quartz::clip(context, &path, sk::FillRule::Winding);
    }
}

pub unsafe extern "C" fn CGContextEOClip(context: CFTypeRef) {
    if let Some(path) = take_path(context) {
        quartz::clip(context, &path, sk::FillRule::EvenOdd);
    }
}

pub unsafe extern "C" fn CGContextClipToRect(context: CFTypeRef, rect: CGRect) {
    if let Some(shape) = shape(context, |e, m| e.rect(m, rect)) {
        quartz::clip(context, &shape, sk::FillRule::Winding);
    }
}

pub unsafe extern "C" fn CGContextClipToRects(
    context: CFTypeRef,
    rects: *const CGRect,
    count: usize,
) {
    if rects.is_null() {
        return;
    }
    // SAFETY: the caller supplies `count` rectangles.
    let rects = unsafe { std::slice::from_raw_parts(rects, count) };
    if let Some(shape) = shape(context, |e, m| rects.iter().for_each(|r| e.rect(m, *r))) {
        quartz::clip(context, &shape, sk::FillRule::Winding);
    }
}

pub unsafe extern "C" fn CGContextClipToMask(context: CFTypeRef, rect: CGRect, mask: CFTypeRef) {
    quartz::clip_to_mask(context, rect, mask);
}

/// `CGContextGetClipBoundingBox`, in user space: the device rectangle the clip
/// leaves, carried back through the CTM.
pub unsafe extern "C" fn CGContextGetClipBoundingBox(context: CFTypeRef) -> CGRect {
    let (Some([left, top, right, bottom]), Some(frame), Some(state)) = (
        quartz::clip_device_bounds(context),
        quartz::frame_of(context),
        gstate(context),
    ) else {
        return NULL_RECT;
    };
    if right <= left || bottom <= top {
        return NULL_RECT;
    }
    let Some(device_to_user) = state.ctm.then(frame.base).inverse() else {
        return NULL_RECT;
    };
    let rect = CGRect {
        origin: CGPoint { x: left, y: top },
        size: CGSize {
            width: right - left,
            height: bottom - top,
        },
    };
    unsafe { CGRectApplyAffineTransform(rect, device_to_user) }
}

//
// Images and shadings
//

pub unsafe extern "C" fn CGContextDrawImage(context: CFTypeRef, rect: CGRect, image: CFTypeRef) {
    if crate::cgimage::is_image_mask(image) {
        if let Some((width, height, coverage)) = crate::cgimage::image_coverage(image) {
            quartz::draw_through_mask(context, rect, width, height, coverage);
        }
        return;
    }
    if let Some((width, height, pixels)) = crate::cgimage::image_bgra(image) {
        quartz::draw_pixels(context, rect, width, height, pixels);
    }
}

pub unsafe extern "C" fn CGContextDrawShading(context: CFTypeRef, shading: CFTypeRef) {
    if let Some(paint) = crate::cgpaint::shading_paint(shading) {
        quartz::draw_shading(context, paint);
    }
}

//
// Text
//

pub unsafe extern "C" fn CGContextSetFont(context: CFTypeRef, font: CFTypeRef) {
    if let Some(state) = gstate(context) {
        state.font = Retained::new(font);
    }
}

pub unsafe extern "C" fn CGContextSetFontSize(context: CFTypeRef, size: f64) {
    if let Some(state) = gstate(context) {
        state.font_size = size;
    }
}

/// `CGContextSelectFont(CGContextRef, const char *name, CGFloat size,
/// CGTextEncoding)`: a font by PostScript name, and its size.
pub unsafe extern "C" fn CGContextSelectFont(
    context: CFTypeRef,
    name: *const c_char,
    size: f64,
    _encoding: i32,
) {
    if name.is_null() {
        return;
    }
    // SAFETY: a NUL-terminated name.
    let name = unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy();
    let name = corefoundation::string::from_bytes(name.as_bytes(), false);
    let descriptor = new_dictionary(&[
        (key("CTFontNameAttribute"), name),
        (key("CTFontFamilyNameAttribute"), name),
    ]);
    let font = make_font(descriptor, size);
    release(descriptor);
    // SAFETY: the reference this call made.
    unsafe { corefoundation::api::CFRelease(name) };
    if let Some(state) = gstate(context) {
        state.font = Retained::new(font);
        state.font_size = size;
    }
    if !font.is_null() {
        // SAFETY: the reference this call made; the state holds its own.
        unsafe { corefoundation::api::CFRelease(font) };
    }
}

pub unsafe extern "C" fn CGContextSetTextMatrix(context: CFTypeRef, matrix: CGAffineTransform) {
    if let Some(state) = gstate(context) {
        state.text_matrix = matrix;
    }
}

pub unsafe extern "C" fn CGContextGetTextMatrix(context: CFTypeRef) -> CGAffineTransform {
    gstate(context).map_or(CGAffineTransformIdentity, |state| state.text_matrix)
}

pub unsafe extern "C" fn CGContextSetTextPosition(context: CFTypeRef, x: f64, y: f64) {
    if let Some(state) = gstate(context) {
        state.text_matrix.tx = x;
        state.text_matrix.ty = y;
    }
}

pub unsafe extern "C" fn CGContextGetTextPosition(context: CFTypeRef) -> CGPoint {
    gstate(context).map_or(CGPoint::default(), |state| CGPoint {
        x: state.text_matrix.tx,
        y: state.text_matrix.ty,
    })
}

pub unsafe extern "C" fn CGContextSetTextDrawingMode(context: CFTypeRef, mode: i32) {
    if let Some(state) = gstate(context) {
        state.text_mode = mode;
    }
}

/// The face the state's font draws with.
fn state_face(context: CFTypeRef) -> Option<(std::sync::Arc<LoadedFace>, f64)> {
    let state = gstate(context)?;
    let face = loaded(state.font.as_ref()?.0)?;
    Some((face, state.font_size))
}

/// Draw `glyphs` from the text position, each moved on by the advances
/// before it (in text space), and move the text position past the last.
fn show_glyphs(context: CFTypeRef, glyphs: &[u16], advances: &[(f64, f64)]) {
    let Some((face, size)) = state_face(context) else {
        return;
    };
    let mut positions = Vec::with_capacity(glyphs.len());
    let mut pen = (0.0, 0.0);
    for advance in advances.iter().take(glyphs.len()) {
        positions.push(pen);
        pen = (pen.0 + advance.0, pen.1 + advance.1);
    }
    quartz::draw_glyphs(context, &face, size, glyphs, &positions);
    if let Some(state) = gstate(context) {
        let p = state.text_matrix.apply(pen.0, pen.1);
        state.text_matrix.tx = p.x;
        state.text_matrix.ty = p.y;
    }
}

/// Each glyph's advance from the font, in text space.
fn font_advances(context: CFTypeRef, glyphs: &[u16]) -> Vec<(f64, f64)> {
    let Some((face, size)) = state_face(context) else {
        return Vec::new();
    };
    let face = face.face();
    let s = size / f64::from(face.units_per_em().max(1));
    glyphs
        .iter()
        .map(|g| {
            let units = face
                .glyph_hor_advance(owned_ttf_parser::GlyphId(*g))
                .unwrap_or(0);
            (f64::from(units) * s, 0.0)
        })
        .collect()
}

pub unsafe extern "C" fn CGContextShowGlyphsWithAdvances(
    context: CFTypeRef,
    glyphs: *const u16,
    advances: *const CGSize,
    count: usize,
) {
    if glyphs.is_null() || advances.is_null() || count == 0 {
        return;
    }
    // SAFETY: the caller supplies `count` of each.
    let (glyphs, advances) = unsafe {
        (
            std::slice::from_raw_parts(glyphs, count),
            std::slice::from_raw_parts(advances, count),
        )
    };
    let advances: Vec<(f64, f64)> = advances.iter().map(|a| (a.width, a.height)).collect();
    show_glyphs(context, glyphs, &advances);
}

pub unsafe extern "C" fn CGContextShowGlyphs(context: CFTypeRef, glyphs: *const u16, count: usize) {
    if glyphs.is_null() || count == 0 {
        return;
    }
    // SAFETY: the caller supplies `count` glyphs.
    let glyphs = unsafe { std::slice::from_raw_parts(glyphs, count) };
    let advances = font_advances(context, glyphs);
    show_glyphs(context, glyphs, &advances);
}

pub unsafe extern "C" fn CGContextShowGlyphsAtPoint(
    context: CFTypeRef,
    x: f64,
    y: f64,
    glyphs: *const u16,
    count: usize,
) {
    unsafe {
        CGContextSetTextPosition(context, x, y);
        CGContextShowGlyphs(context, glyphs, count);
    }
}

/// `CGContextShowText(CGContextRef, const char *, size_t)`, in the font
/// `CGContextSelectFont` chose.
///
/// FIXME (ACCURACY): the bytes are read as Latin-1, which is MacRoman only
/// below 128.
pub unsafe extern "C" fn CGContextShowText(context: CFTypeRef, text: *const c_char, length: usize) {
    if text.is_null() || length == 0 {
        return;
    }
    // SAFETY: the caller supplies `length` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length) };
    let Some((face, _)) = state_face(context) else {
        return;
    };
    let glyphs: Vec<u16> = bytes
        .iter()
        .map(|b| face.face().glyph_index(char::from(*b)).map_or(0, |g| g.0))
        .collect();
    let advances = font_advances(context, &glyphs);
    show_glyphs(context, &glyphs, &advances);
}

pub unsafe extern "C" fn CGContextShowTextAtPoint(
    context: CFTypeRef,
    x: f64,
    y: f64,
    text: *const c_char,
    length: usize,
) {
    unsafe {
        CGContextSetTextPosition(context, x, y);
        CGContextShowText(context, text, length);
    }
}

//
// Pages and flushing
//

/// Every context here draws immediately, so there is nothing to flush.
unsafe extern "C" fn ignored_context(_context: CFTypeRef) {}

/// `CGContextBeginPage(CGContextRef, const CGRect *)`: a bitmap context has
/// one page and nothing here makes a PDF context.
unsafe extern "C" fn CGContextBeginPage(_context: CFTypeRef, _media: *const CGRect) {}

/// `CGRectNull`.
const NULL_RECT: CGRect = CGRect {
    origin: CGPoint {
        x: f64::INFINITY,
        y: f64::INFINITY,
    },
    size: CGSize {
        width: 0.0,
        height: 0.0,
    },
};

exports::exports! {
    functions {
        "CGContextRetain" => CGContextRetain,
        "CGContextRelease" => CGContextRelease,
        "CGContextSaveGState" => CGContextSaveGState,
        "CGContextRestoreGState" => CGContextRestoreGState,
        "CGContextConcatCTM" => CGContextConcatCTM,
        "CGContextGetCTM" => CGContextGetCTM,
        "CGContextSetCTM" => CGContextSetCTM,
        "CGContextTranslateCTM" => CGContextTranslateCTM,
        "CGContextScaleCTM" => CGContextScaleCTM,
        "CGContextRotateCTM" => CGContextRotateCTM,
        "CGContextSetRGBFillColor" => CGContextSetRGBFillColor,
        "CGContextSetRGBStrokeColor" => CGContextSetRGBStrokeColor,
        "CGContextSetGrayFillColor" => CGContextSetGrayFillColor,
        "CGContextSetGrayStrokeColor" => CGContextSetGrayStrokeColor,
        "CGContextSetFillColorWithColor" => CGContextSetFillColorWithColor,
        "CGContextSetStrokeColorWithColor" => CGContextSetStrokeColorWithColor,
        "CGContextSetFillColorSpace" => CGContextSetFillColorSpace,
        "CGContextSetStrokeColorSpace" => CGContextSetStrokeColorSpace,
        "CGContextSetFillColor" => CGContextSetFillColor,
        "CGContextSetStrokeColor" => CGContextSetStrokeColor,
        "CGContextSetFillPattern" => CGContextSetFillPattern,
        "CGContextSetStrokePattern" => CGContextSetStrokePattern,
        "CGContextSetPatternPhase" => CGContextSetPatternPhase,
        "CGContextSetAlpha" => CGContextSetAlpha,
        "CGContextSetLineWidth" => CGContextSetLineWidth,
        "CGContextSetLineCap" => CGContextSetLineCap,
        "CGContextSetLineJoin" => CGContextSetLineJoin,
        "CGContextSetMiterLimit" => CGContextSetMiterLimit,
        "CGContextSetLineDash" => CGContextSetLineDash,
        "CGContextSetFlatness" => CGContextSetFlatness,
        "CGContextSetBlendMode" => CGContextSetBlendMode,
        "CGContextSetCompositeOperation" => CGContextSetCompositeOperation,
        "CGContextSetInterpolationQuality" => CGContextSetInterpolationQuality,
        "CGContextSetShouldAntialias" => CGContextSetShouldAntialias,
        "CGContextSetAllowsAntialiasing" | "CGContextSetShouldSmoothFonts" | "CGContextSetAllowsFontSmoothing" | "CGContextSetShouldSubpixelPositionFonts" | "CGContextSetAllowsFontSubpixelPositioning" | "CGContextSetShouldSubpixelQuantizeFonts" | "CGContextSetAllowsFontSubpixelQuantization" => ignored_flag,
        "CGContextBeginPath" => CGContextBeginPath,
        "CGContextMoveToPoint" => CGContextMoveToPoint,
        "CGContextAddLineToPoint" => CGContextAddLineToPoint,
        "CGContextAddQuadCurveToPoint" => CGContextAddQuadCurveToPoint,
        "CGContextAddCurveToPoint" => CGContextAddCurveToPoint,
        "CGContextClosePath" => CGContextClosePath,
        "CGContextAddRect" => CGContextAddRect,
        "CGContextAddRects" => CGContextAddRects,
        "CGContextAddEllipseInRect" => CGContextAddEllipseInRect,
        "CGContextAddArc" => CGContextAddArc,
        "CGContextAddArcToPoint" => CGContextAddArcToPoint,
        "CGContextAddLines" => CGContextAddLines,
        "CGContextAddPath" => CGContextAddPath,
        "CGContextIsPathEmpty" => CGContextIsPathEmpty,
        "CGContextGetPathCurrentPoint" => CGContextGetPathCurrentPoint,
        "CGContextGetPathBoundingBox" => CGContextGetPathBoundingBox,
        "CGContextReplacePathWithStrokedPath" => CGContextReplacePathWithStrokedPath,
        "CGContextFillPath" => CGContextFillPath,
        "CGContextEOFillPath" => CGContextEOFillPath,
        "CGContextStrokePath" => CGContextStrokePath,
        "CGContextDrawPath" => CGContextDrawPath,
        "CGContextFillRect" => CGContextFillRect,
        "CGContextFillRects" => CGContextFillRects,
        "CGContextStrokeRect" => CGContextStrokeRect,
        "CGContextStrokeRectWithWidth" => CGContextStrokeRectWithWidth,
        "CGContextFillEllipseInRect" => CGContextFillEllipseInRect,
        "CGContextStrokeEllipseInRect" => CGContextStrokeEllipseInRect,
        "CGContextStrokeLineSegments" => CGContextStrokeLineSegments,
        "CGContextClearRect" => CGContextClearRect,
        "CGContextClip" => CGContextClip,
        "CGContextEOClip" => CGContextEOClip,
        "CGContextClipToRect" => CGContextClipToRect,
        "CGContextClipToRects" => CGContextClipToRects,
        "CGContextClipToMask" => CGContextClipToMask,
        "CGContextGetClipBoundingBox" => CGContextGetClipBoundingBox,
        "CGContextDrawImage" => CGContextDrawImage,
        "CGContextDrawShading" => CGContextDrawShading,
        "CGContextSetFont" => CGContextSetFont,
        "CGContextSetFontSize" => CGContextSetFontSize,
        "CGContextSelectFont" => CGContextSelectFont,
        "CGContextSetTextMatrix" => CGContextSetTextMatrix,
        "CGContextGetTextMatrix" => CGContextGetTextMatrix,
        "CGContextSetTextPosition" => CGContextSetTextPosition,
        "CGContextGetTextPosition" => CGContextGetTextPosition,
        "CGContextSetTextDrawingMode" => CGContextSetTextDrawingMode,
        "CGContextShowGlyphsWithAdvances" => CGContextShowGlyphsWithAdvances,
        "CGContextShowGlyphs" => CGContextShowGlyphs,
        "CGContextShowGlyphsAtPoint" => CGContextShowGlyphsAtPoint,
        "CGContextShowText" => CGContextShowText,
        "CGContextShowTextAtPoint" => CGContextShowTextAtPoint,
        "CGContextFlush" | "CGContextSynchronize" | "CGContextEndPage" => ignored_context,
        "CGContextBeginPage" => CGContextBeginPage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cgbitmap::CGBitmapContextCreate;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
        CGRect {
            origin: CGPoint { x, y },
            size: CGSize { width, height },
        }
    }

    #[test]
    fn draws_an_image_through_a_vertical_context_flip() {
        crate::bootstrap();
        let image = crate::cgimage::image_from_bitmap(crate::raster::Bitmap {
            width: 1,
            height: 2,
            pixels: vec![255, 0, 0, 255, 0, 255, 0, 255],
        });
        let mut pixels = [0u8; 8];
        // SAFETY: `pixels` holds two writable RGBA rows and `image` is live.
        unsafe {
            let context =
                CGBitmapContextCreate(pixels.as_mut_ptr().cast(), 1, 2, 8, 4, std::ptr::null(), 3);
            CGContextTranslateCTM(context, 0.0, 2.0);
            CGContextScaleCTM(context, 1.0, -1.0);
            CGContextDrawImage(context, rect(0.0, 0.0, 1.0, 2.0), image);
            CGContextRelease(context);
            crate::cgimage::CGImageRelease(image);
        }
        assert_eq!(pixels, [0, 255, 0, 255, 255, 0, 0, 255]);
    }

    #[test]
    fn fills_a_translated_rect_in_a_little_endian_bitmap() {
        crate::bootstrap();
        let mut pixels = [0u32; 4];
        // SAFETY: `pixels` is two writable rows of two pixels.
        unsafe {
            let context = CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                2,
                2,
                8,
                8,
                std::ptr::null(),
                0x2006,
            );
            CGContextSetRGBFillColor(context, 1.0, 0.0, 0.0, 1.0);
            CGContextTranslateCTM(context, 1.0, 1.0);
            CGContextFillRect(context, rect(0.0, 0.0, 1.0, 1.0));
            CGContextRelease(context);
        }
        // Device (1, 1) is the top row's second pixel, 0xAARRGGBB with the
        // skipped byte opaque as Qt's RGB32 images need it.
        assert_eq!(pixels, [0, 0xffff_0000, 0, 0]);
    }

    /// A little-endian context whose rows are padded is drawn on a copy, and
    /// its colours still land in the right bytes.
    #[test]
    fn fills_a_padded_little_endian_bitmap_in_its_own_order() {
        crate::bootstrap();
        // One pixel per row, padded to two.
        let mut pixels = [0u32; 4];
        // SAFETY: `pixels` is two writable rows of eight bytes.
        unsafe {
            let context = CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                1,
                2,
                8,
                8,
                std::ptr::null(),
                0x2002,
            );
            CGContextSetRGBFillColor(context, 1.0, 0.0, 0.0, 1.0);
            CGContextFillRect(context, rect(0.0, 0.0, 1.0, 2.0));
            CGContextRelease(context);
        }
        assert_eq!(pixels, [0xffff_0000, 0, 0xffff_0000, 0]);
    }

    #[test]
    fn saved_state_restores_the_transform() {
        crate::bootstrap();
        // SAFETY: a context over its own storage.
        unsafe {
            let context =
                CGBitmapContextCreate(std::ptr::null_mut(), 4, 4, 8, 16, std::ptr::null(), 1);
            CGContextSaveGState(context);
            CGContextScaleCTM(context, 2.0, 3.0);
            assert_eq!(CGContextGetCTM(context).d, 3.0);
            CGContextRestoreGState(context);
            assert_eq!(CGContextGetCTM(context), CGAffineTransformIdentity);
            let inverse = crate::CGAffineTransformInvert(CGAffineTransformMakeScale(2.0, 4.0));
            assert_eq!(inverse.a, 0.5);
            assert_eq!(inverse.d, 0.25);
            CGContextRelease(context);
        }
    }

    /// The four pixels of a 2x2 premultiplied RGBA context, top row first.
    fn pixels_after(draw: impl FnOnce(CFTypeRef)) -> [[u8; 4]; 4] {
        crate::bootstrap();
        let mut pixels = [[0u8; 4]; 4];
        // SAFETY: two writable rows of two RGBA pixels.
        unsafe {
            let context =
                CGBitmapContextCreate(pixels.as_mut_ptr().cast(), 2, 2, 8, 8, std::ptr::null(), 1);
            draw(context);
            CGContextRelease(context);
        }
        pixels
    }

    /// A path filled through the current path, which a fill consumes.
    #[test]
    fn a_filled_path_covers_its_pixels_and_is_consumed() {
        let pixels = pixels_after(|context| unsafe {
            CGContextSetRGBFillColor(context, 0.0, 0.0, 1.0, 1.0);
            CGContextMoveToPoint(context, 0.0, 0.0);
            CGContextAddLineToPoint(context, 2.0, 0.0);
            CGContextAddLineToPoint(context, 2.0, 1.0);
            CGContextAddLineToPoint(context, 0.0, 1.0);
            CGContextClosePath(context);
            CGContextFillPath(context);
            assert!(CGContextIsPathEmpty(context));
        });
        // The bottom row in user space is the second row in memory.
        assert_eq!(pixels, [[0; 4], [0; 4], [0, 0, 255, 255], [0, 0, 255, 255]]);
    }

    /// A clipped fill in a mode that ignores the source, or replaces the
    /// destination, still leaves everything outside the clip alone.
    #[test]
    fn a_clip_bounds_clear_and_copy_fills() {
        for (mode, inside) in [(16, [0u8; 4]), (17, [0, 0, 255, 255])] {
            let pixels = pixels_after(|context| unsafe {
                CGContextSetRGBFillColor(context, 1.0, 1.0, 1.0, 1.0);
                CGContextFillRect(context, rect(0.0, 0.0, 2.0, 2.0));
                CGContextClipToRect(context, rect(1.0, 0.0, 1.0, 2.0));
                CGContextSetBlendMode(context, mode);
                CGContextSetRGBFillColor(context, 0.0, 0.0, 1.0, 1.0);
                CGContextFillRect(context, rect(0.0, 0.0, 2.0, 2.0));
            });
            let white = [255u8; 4];
            assert_eq!(pixels, [white, inside, white, inside], "blend mode {mode}");
        }
    }

    /// A clip keeps drawing inside it, and restoring the state lifts it.
    #[test]
    fn a_clip_bounds_a_fill_until_restored() {
        let pixels = pixels_after(|context| unsafe {
            CGContextSaveGState(context);
            CGContextClipToRect(context, rect(1.0, 0.0, 1.0, 2.0));
            CGContextSetRGBFillColor(context, 1.0, 1.0, 1.0, 1.0);
            CGContextFillRect(context, rect(0.0, 0.0, 2.0, 2.0));
            assert_eq!(
                CGContextGetClipBoundingBox(context),
                rect(1.0, 0.0, 1.0, 2.0)
            );
            CGContextRestoreGState(context);
            CGContextSetRGBFillColor(context, 1.0, 0.0, 0.0, 1.0);
            CGContextFillRect(context, rect(0.0, 1.0, 1.0, 1.0));
        });
        assert_eq!(pixels, [[255, 0, 0, 255], [255; 4], [0; 4], [255; 4]]);
    }

    /// A line one unit wide, under a CTM that doubles it, covers a pixel either
    /// side of its centre.
    #[test]
    fn a_stroke_scales_its_width_with_the_ctm() {
        let pixels = pixels_after(|context| unsafe {
            CGContextScaleCTM(context, 2.0, 2.0);
            CGContextSetLineWidth(context, 1.0);
            CGContextSetRGBStrokeColor(context, 0.0, 1.0, 0.0, 1.0);
            CGContextMoveToPoint(context, 0.5, 0.0);
            CGContextAddLineToPoint(context, 0.5, 1.0);
            CGContextStrokePath(context);
        });
        assert_eq!(pixels, [[0, 255, 0, 255]; 4]);
    }

    /// Clearing leaves transparent black, whatever was under it.
    #[test]
    fn clear_rect_makes_pixels_transparent() {
        let pixels = pixels_after(|context| unsafe {
            CGContextSetRGBFillColor(context, 1.0, 1.0, 1.0, 1.0);
            CGContextFillRect(context, rect(0.0, 0.0, 2.0, 2.0));
            CGContextClearRect(context, rect(0.0, 0.0, 1.0, 2.0));
        });
        assert_eq!(pixels, [[0; 4], [255; 4], [0; 4], [255; 4]]);
    }

    /// A layer drawn back at its own size reproduces what was drawn into it.
    #[test]
    fn a_layer_draws_back_what_it_holds() {
        let pixels = pixels_after(|context| unsafe {
            let layer = crate::cgpaint::CGLayerCreateWithContext(
                context,
                CGSize {
                    width: 2.0,
                    height: 2.0,
                },
                std::ptr::null(),
            );
            let inner = crate::cgpaint::CGLayerGetContext(layer);
            CGContextSetRGBFillColor(inner, 0.0, 0.0, 1.0, 1.0);
            CGContextFillRect(inner, rect(0.0, 1.0, 2.0, 1.0));
            crate::cgpaint::CGContextDrawLayerAtPoint(context, CGPoint::default(), layer);
            crate::cgpaint::CGLayerRelease(layer);
        });
        assert_eq!(pixels, [[0, 0, 255, 255], [0, 0, 255, 255], [0; 4], [0; 4]]);
    }
}
