//! The raster engine behind `CGContext`: where a context's pixels are, its
//! graphics state, and filling, stroking, clipping and compositing on them.
//!
//! Two kinds of target. A bitmap context draws into the caller's memory, in
//! the layout `CGBitmapContextCreate` was given; a focus context draws into
//! the backing store of the view `-drawRect:` focused. Both are drawn by
//! tiny-skia, which works in premultiplied R, G, B, A: a B, G, R, A target is
//! drawn in place with red and blue swapped in every source, which composites
//! identically because no blend mode mixes one colour channel into another.
//!
//! Coordinates follow Quartz. The CTM maps user space to the context's base
//! space, whose origin is the bottom left; the frame's base transform maps
//! base space to device pixels, top row first. The current path is kept in
//! base space, because Quartz applies the CTM as each element is added.

use std::ffi::c_void;
use std::sync::Arc;

use corefoundation::base::CFTypeRef;
use foundation::geometry::{NSPoint, NSRect, NSSize};
use tiny_skia as sk;

use crate::cgcontext::{CGAffineTransform, IDENTITY};
use crate::cgpath::Elements;

/// A Core Foundation object a graphics state holds a reference to.
pub(crate) struct Retained(pub CFTypeRef);

impl Retained {
    pub(crate) fn new(object: CFTypeRef) -> Option<Retained> {
        // SAFETY: a live object the caller holds.
        (!object.is_null()).then(|| Retained(unsafe { corefoundation::api::CFRetain(object) }))
    }
}

impl Clone for Retained {
    fn clone(&self) -> Self {
        // SAFETY: an object this value holds a reference to.
        Retained(unsafe { corefoundation::api::CFRetain(self.0) })
    }
}

impl Drop for Retained {
    fn drop(&mut self) {
        // SAFETY: the reference `new` or `clone` took.
        unsafe { corefoundation::api::CFRelease(self.0) };
    }
}

/// What a fill or stroke paints with.
#[derive(Clone)]
pub(crate) enum Ink {
    /// Straight R, G, B, A.
    Color([f64; 4]),
    /// A `CGPattern`, and the colour an uncoloured one is painted in.
    Pattern(Retained, [f64; 4]),
}

#[derive(Clone)]
pub(crate) struct Line {
    pub width: f64,
    pub cap: sk::LineCap,
    pub join: sk::LineJoin,
    pub miter: f64,
    /// Lengths and phase.
    pub dash: Option<(Vec<f64>, f64)>,
}

#[derive(Clone)]
pub(crate) struct GState {
    pub ctm: CGAffineTransform,
    pub fill: Ink,
    pub stroke: Ink,
    /// The components a colour set with `CGContextSetFillColor` has, from the
    /// last colour space set: 3 for RGB, 1 for gray, 0 for patterns.
    pub fill_components: usize,
    pub stroke_components: usize,
    pub alpha: f64,
    pub blend: sk::BlendMode,
    pub quality: sk::FilterQuality,
    pub antialias: bool,
    pub line: Line,
    /// Coverage in device pixels, the size of the target it was made for.
    pub clip: Option<Arc<sk::Mask>>,
    pub text_matrix: CGAffineTransform,
    pub font: Option<Retained>,
    pub font_size: f64,
    pub text_mode: i32,
    pub pattern_phase: (f64, f64),
}

impl Default for GState {
    fn default() -> Self {
        GState {
            ctm: IDENTITY,
            fill: Ink::Color([0.0, 0.0, 0.0, 1.0]),
            stroke: Ink::Color([0.0, 0.0, 0.0, 1.0]),
            fill_components: 3,
            stroke_components: 3,
            alpha: 1.0,
            blend: sk::BlendMode::SourceOver,
            quality: sk::FilterQuality::Bilinear,
            antialias: true,
            line: Line {
                width: 1.0,
                cap: sk::LineCap::Butt,
                join: sk::LineJoin::Miter,
                miter: 10.0,
                dash: None,
            },
            clip: None,
            text_matrix: IDENTITY,
            font: None,
            font_size: 0.0,
            text_mode: 0,
            pattern_phase: (0.0, 0.0),
        }
    }
}

pub(crate) struct State {
    pub current: GState,
    pub saved: Vec<GState>,
    /// The current path, in base space.
    pub path: Elements,
    /// Storage the context allocated because the caller passed none, which
    /// `data` points into; held only to keep it alive.
    _storage: Option<Vec<u8>>,
}

impl State {
    pub(crate) fn new(storage: Option<Vec<u8>>) -> State {
        State {
            current: GState::default(),
            saved: Vec::new(),
            path: Elements::default(),
            _storage: storage,
        }
    }
}

#[repr(C)]
pub(crate) struct BitmapContext {
    pub isa: *const c_void,
    pub data: *mut u8,
    pub width: usize,
    pub height: usize,
    pub bytes_per_row: usize,
    pub bits_per_component: usize,
    pub bitmap_info: u32,
    pub state: *mut State,
    /// Drawing goes to the view `-drawRect:` focused rather than to `data`,
    /// which is null: the context `-[NSGraphicsContext graphicsPort]` answers.
    pub focus: bool,
    /// Bytes per pixel: 4, or 1 for gray and alpha-only.
    pub channels: usize,
    /// For a 1-channel context, whether it is `kCGImageAlphaOnly`.
    pub alpha_only: bool,
}

pub(crate) fn context_of<'a>(context: CFTypeRef) -> Option<&'a mut BitmapContext> {
    let class = objc::class::lookup(c"__CGContext")?;
    // SAFETY: null or an object a guest received from this runtime.
    if context.is_null() || !unsafe { corefoundation::base::is_kind_of(context, class) } {
        return None;
    }
    // SAFETY: a `__CGContext`, whose body is `BitmapContext`.
    Some(unsafe { &mut *(context as *mut BitmapContext) })
}

pub(crate) fn state_of<'a>(context: CFTypeRef) -> Option<&'a mut State> {
    let context = context_of(context)?;
    // SAFETY: every context is created with a state.
    unsafe { context.state.as_mut() }
}

pub(crate) const ALPHA_INFO_MASK: u32 = 0x1f;
pub(crate) const BYTE_ORDER_MASK: u32 = 0x7000;
pub(crate) const BYTE_ORDER_32_LITTLE: u32 = 0x2000;

/// Where a context's base space lands in its target, and how big that is.
#[derive(Clone, Copy)]
pub(crate) struct Frame {
    /// Base space to device pixels.
    pub base: CGAffineTransform,
    pub width: u32,
    pub height: u32,
    /// Whether the target is B, G, R, A, so sources are drawn with red and
    /// blue exchanged.
    pub swap: bool,
    /// Device pixels per base unit: the view's density, or 1.
    pub scale: f64,
}

/// The frame a context draws in, without touching its pixels.
pub(crate) fn frame_of(context: CFTypeRef) -> Option<Frame> {
    let ctx = context_of(context)?;
    if ctx.focus {
        let device = crate::backing::focused_device()?;
        let d = device.density;
        let base = match device.flipped {
            true => CGAffineTransform {
                a: d,
                d,
                ..IDENTITY
            },
            false => CGAffineTransform {
                a: d,
                d: -d,
                ty: f64::from(device.height),
                ..IDENTITY
            },
        };
        return Some(Frame {
            base,
            width: device.width,
            height: device.height,
            swap: true,
            scale: d,
        });
    }
    if ctx.data.is_null() {
        return None;
    }
    Some(Frame {
        base: CGAffineTransform {
            d: -1.0,
            ty: ctx.height as f64,
            ..IDENTITY
        },
        width: ctx.width as u32,
        height: ctx.height as u32,
        // Only a context drawn in place keeps its bytes' order; any other is
        // drawn on an R, G, B, A copy, see `render`.
        swap: direct(ctx) && layout(ctx) == Layout::Bgra,
        scale: 1.0,
    })
}

/// How a bitmap context's bytes are laid out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Rgba,
    Bgra,
    Argb,
    Abgr,
    Gray,
    Alpha,
}

fn layout(ctx: &BitmapContext) -> Layout {
    if ctx.channels == 1 {
        return match ctx.alpha_only {
            true => Layout::Alpha,
            false => Layout::Gray,
        };
    }
    let alpha = ctx.bitmap_info & ALPHA_INFO_MASK;
    let little = ctx.bitmap_info & BYTE_ORDER_MASK == BYTE_ORDER_32_LITTLE;
    let first = matches!(alpha, 2 | 4 | 6);
    match (first, little) {
        (true, true) => Layout::Bgra,
        (false, false) => Layout::Rgba,
        (true, false) => Layout::Argb,
        (false, true) => Layout::Abgr,
    }
}

/// Whether a context's bytes are drawn on in place: four channels in an order
/// tiny-skia's pixmap takes, with no padding at the end of a row. Any other
/// layout is converted to R, G, B, A and back around each draw.
fn direct(ctx: &BitmapContext) -> bool {
    matches!(layout(ctx), Layout::Rgba | Layout::Bgra) && ctx.bytes_per_row == ctx.width * 4
}

/// Whether a context's fourth byte is padding Quartz keeps opaque.
fn skips_alpha(ctx: &BitmapContext) -> bool {
    ctx.channels == 4 && matches!(ctx.bitmap_info & ALPHA_INFO_MASK, 0 | 5 | 6)
}

/// Run `op` over a context's pixels, as premultiplied R, G, B, A (or B, G, R, A
/// when the frame says to swap), and write back what it changed.
///
/// `op` answers the device rectangle it touched, or `None` for nothing.
pub(crate) fn render(
    context: CFTypeRef,
    op: impl FnOnce(&mut sk::PixmapMut, &Frame) -> Option<sk::IntRect>,
) {
    let Some(ctx) = context_of(context) else {
        return;
    };
    let Some(frame) = frame_of(context) else {
        return;
    };
    if ctx.focus {
        crate::backing::draw_focused(|pixels, width, height| {
            let mut pixmap = sk::PixmapMut::from_bytes(pixels, width, height)?;
            let touched = op(&mut pixmap, &frame)?;
            Some(crate::backing::Rect {
                x: i64::from(touched.x()),
                y: i64::from(touched.y()),
                width: i64::from(touched.width()),
                height: i64::from(touched.height()),
            })
        });
        return;
    }
    let (width, height) = (ctx.width, ctx.height);
    let kind = layout(ctx);
    // SAFETY: a bitmap context's storage is `bytes_per_row * height` bytes.
    let bytes = unsafe { std::slice::from_raw_parts_mut(ctx.data, ctx.bytes_per_row * ctx.height) };
    let touched = match direct(ctx) {
        true => {
            let Some(mut pixmap) = sk::PixmapMut::from_bytes(bytes, width as u32, height as u32)
            else {
                return;
            };
            op(&mut pixmap, &frame)
        }
        false => {
            let Some(mut owned) = sk::Pixmap::new(width as u32, height as u32) else {
                return;
            };
            read_in(bytes, ctx, kind, owned.data_mut());
            let touched = op(&mut owned.as_mut(), &frame);
            if let Some(rect) = touched {
                write_out(owned.data(), ctx, kind, rect, bytes);
            }
            touched
        }
    };
    if let Some(rect) = touched
        && skips_alpha(ctx)
    {
        let alpha_at = match kind {
            Layout::Argb | Layout::Abgr => 0,
            _ => 3,
        };
        // Only where the draw wrote: a pixel still at zero alpha is one no
        // drawing has touched, and stays as the caller left it.
        for y in rect.top()..rect.bottom() {
            let row = y as usize * ctx.bytes_per_row;
            for x in rect.left()..rect.right() {
                let at = row + x as usize * 4 + alpha_at;
                if bytes[at] != 0 {
                    bytes[at] = 255;
                }
            }
        }
    }
}

/// Convert a bitmap context's bytes into premultiplied R, G, B, A.
fn read_in(bytes: &[u8], ctx: &BitmapContext, kind: Layout, out: &mut [u8]) {
    for y in 0..ctx.height {
        let row = &bytes[y * ctx.bytes_per_row..];
        for x in 0..ctx.width {
            let rgba = match kind {
                Layout::Rgba => [row[x * 4], row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3]],
                Layout::Bgra => [row[x * 4 + 2], row[x * 4 + 1], row[x * 4], row[x * 4 + 3]],
                Layout::Argb => [row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3], row[x * 4]],
                Layout::Abgr => [row[x * 4 + 3], row[x * 4 + 2], row[x * 4 + 1], row[x * 4]],
                Layout::Gray => [row[x], row[x], row[x], 255],
                Layout::Alpha => [0, 0, 0, row[x]],
            };
            let rgba = match skips_alpha(ctx) {
                true => [rgba[0], rgba[1], rgba[2], 255],
                false => rgba,
            };
            out[(y * ctx.width + x) * 4..][..4].copy_from_slice(&rgba);
        }
    }
}

/// The inverse of [`read_in`], over the rectangle that changed.
fn write_out(
    pixels: &[u8],
    ctx: &BitmapContext,
    kind: Layout,
    rect: sk::IntRect,
    bytes: &mut [u8],
) {
    for y in rect.top().max(0) as usize..(rect.bottom().max(0) as usize).min(ctx.height) {
        let row = y * ctx.bytes_per_row;
        for x in rect.left().max(0) as usize..(rect.right().max(0) as usize).min(ctx.width) {
            let p = &pixels[(y * ctx.width + x) * 4..][..4];
            let [r, g, b, a] = [p[0], p[1], p[2], p[3]];
            match kind {
                Layout::Rgba => bytes[row + x * 4..][..4].copy_from_slice(&[r, g, b, a]),
                Layout::Bgra => bytes[row + x * 4..][..4].copy_from_slice(&[b, g, r, a]),
                Layout::Argb => bytes[row + x * 4..][..4].copy_from_slice(&[a, r, g, b]),
                Layout::Abgr => bytes[row + x * 4..][..4].copy_from_slice(&[a, b, g, r]),
                Layout::Gray => {
                    bytes[row + x] =
                        ((u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000)
                            as u8
                }
                Layout::Alpha => bytes[row + x] = a,
            }
        }
    }
}

impl CGAffineTransform {
    pub(crate) fn sk(self) -> sk::Transform {
        sk::Transform::from_row(
            self.a as f32,
            self.b as f32,
            self.c as f32,
            self.d as f32,
            self.tx as f32,
            self.ty as f32,
        )
    }

    pub(crate) fn inverse(self) -> Option<CGAffineTransform> {
        let det = self.a * self.d - self.b * self.c;
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(CGAffineTransform {
            a,
            b,
            c,
            d,
            tx: -(self.tx * a + self.ty * c),
            ty: -(self.tx * b + self.ty * d),
        })
    }

    /// The factor lengths grow by, for sizing a raster that will be drawn
    /// through this transform.
    pub(crate) fn scale_factor(self) -> f64 {
        (self.a * self.d - self.b * self.c).abs().sqrt()
    }
}

/// A paint source, made before the geometry it will fill is known.
pub(crate) enum Source {
    /// Straight R, G, B, A, the global alpha already applied.
    Solid([f64; 4]),
    /// A raster, already in the target's channel order, and the transform from
    /// its pixels to device pixels.
    Raster {
        pixmap: sk::Pixmap,
        to_device: CGAffineTransform,
        spread: sk::SpreadMode,
        quality: sk::FilterQuality,
        opacity: f32,
    },
    /// A gradient's stops (straight R, G, B, A, global alpha applied) in the
    /// space `to_device` maps to device pixels.
    Gradient {
        radial: Option<(f64, f64)>,
        start: (f64, f64),
        end: (f64, f64),
        stops: Vec<(f32, [f64; 4])>,
        to_device: CGAffineTransform,
    },
}

fn color(rgba: [f64; 4], swap: bool) -> sk::Color {
    let c = |v: f64| v.clamp(0.0, 1.0) as f32;
    let [r, g, b, a] = rgba;
    let (r, b) = if swap { (b, r) } else { (r, b) };
    sk::Color::from_rgba(c(r), c(g), c(b), c(a)).unwrap_or(sk::Color::TRANSPARENT)
}

impl Source {
    /// The paint for this source when the geometry is drawn through
    /// `geometry`, which tiny-skia also applies to the shader.
    pub(crate) fn paint<'a>(
        &'a self,
        geometry: CGAffineTransform,
        state: &GState,
        swap: bool,
    ) -> Option<sk::Paint<'a>> {
        let local = |to_device: CGAffineTransform| -> Option<sk::Transform> {
            Some(to_device.then(geometry.inverse()?).sk())
        };
        let shader = match self {
            Source::Solid(rgba) => sk::Shader::SolidColor(color(*rgba, swap)),
            Source::Raster {
                pixmap,
                to_device,
                spread,
                quality,
                opacity,
            } => sk::Pattern::new(
                pixmap.as_ref(),
                *spread,
                *quality,
                *opacity,
                local(*to_device)?,
            ),
            Source::Gradient {
                radial,
                start,
                end,
                stops,
                to_device,
            } => {
                let stops: Vec<sk::GradientStop> = stops
                    .iter()
                    .map(|(at, rgba)| sk::GradientStop::new(*at, color(*rgba, swap)))
                    .collect();
                let p = |(x, y): (f64, f64)| sk::Point::from_xy(x as f32, y as f32);
                let transform = local(*to_device)?;
                match radial {
                    None => sk::LinearGradient::new(
                        p(*start),
                        p(*end),
                        stops,
                        sk::SpreadMode::Pad,
                        transform,
                    )?,
                    Some((_, radius)) => sk::RadialGradient::new(
                        p(*start),
                        p(*end),
                        *radius as f32,
                        stops,
                        sk::SpreadMode::Pad,
                        transform,
                    )?,
                }
            }
        };
        Some(sk::Paint {
            shader,
            blend_mode: state.blend,
            anti_alias: state.antialias,
            force_hq_pipeline: false,
        })
    }
}

/// The source an ink paints with in a context whose frame is `frame`.
pub(crate) fn source_for(ink: &Ink, state: &GState, frame: &Frame) -> Option<Source> {
    match ink {
        Ink::Color([r, g, b, a]) => Some(Source::Solid([*r, *g, *b, a * state.alpha])),
        Ink::Pattern(pattern, rgba) => {
            let (phase_x, phase_y) = state.pattern_phase;
            let placed = crate::cgpaint::pattern_matrix(pattern.0)?.then(CGAffineTransform {
                tx: phase_x,
                ty: phase_y,
                ..IDENTITY
            });
            let pattern_to_device = placed.then(frame.base);
            let tile = crate::cgpaint::render_tile(
                pattern.0,
                pattern_to_device.scale_factor().max(0.01),
                *rgba,
                frame.swap,
            )?;
            Some(Source::Raster {
                pixmap: tile.pixmap,
                to_device: tile.to_pattern.then(pattern_to_device),
                spread: sk::SpreadMode::Repeat,
                quality: state.quality,
                opacity: state.alpha as f32,
            })
        }
    }
}

/// The clip, if it was made for a target this size.
fn clip_for(state: &GState, width: u32, height: u32) -> Option<&sk::Mask> {
    state
        .clip
        .as_deref()
        .filter(|mask| mask.width() == width && mask.height() == height)
}

/// The device rectangle `path` covers through `transform`, grown by `outset`
/// pixels and cut to the target.
fn touched(
    path: &sk::Path,
    transform: sk::Transform,
    outset: f32,
    width: u32,
    height: u32,
) -> Option<sk::IntRect> {
    let bounds = path.clone().transform(transform)?.bounds();
    let left = (bounds.left() - outset).floor().max(0.0) as i32;
    let top = (bounds.top() - outset).floor().max(0.0) as i32;
    let right = ((bounds.right() + outset).ceil() as i32).min(width as i32);
    let bottom = ((bounds.bottom() + outset).ceil() as i32).min(height as i32);
    sk::IntRect::from_ltrb(left, top, right, bottom)
}

/// Whether tiny-skia clips a fill in `mode` correctly by scaling the source
/// by coverage - its own `should_pre_scale_coverage`, which is private. In
/// any other mode its rectangle blitter writes through the clip: it scales
/// the source by the mask but never moves the result back toward the
/// destination, so a clipped `Clear` empties the whole rectangle.
fn clips_by_scaling_source(mode: sk::BlendMode) -> bool {
    use sk::BlendMode::*;
    matches!(
        mode,
        Destination | DestinationOver | Plus | DestinationOut | SourceAtop | SourceOver | Xor
    )
}

/// `fill_path` with `clip` honoured in every blend mode: where tiny-skia
/// would not, the fill is made unclipped on a copy of the area it reaches,
/// which is then laid back over the target by the clip's coverage.
fn fill_clipped(
    pixmap: &mut sk::PixmapMut,
    path: &sk::Path,
    paint: &sk::Paint,
    rule: sk::FillRule,
    transform: sk::Transform,
    clip: Option<&sk::Mask>,
) {
    let Some(clip) = clip.filter(|_| !clips_by_scaling_source(paint.blend_mode)) else {
        pixmap.fill_path(path, paint, rule, transform, clip);
        return;
    };
    // One pixel more than the bounds, for anti-aliased edges.
    let Some(area) = touched(path, transform, 1.0, pixmap.width(), pixmap.height()) else {
        return;
    };
    let Some(mut scratch) = sk::Pixmap::new(area.width(), area.height()) else {
        return;
    };
    let stride = pixmap.width() as usize * 4;
    let (x0, y0) = (area.x() as usize, area.y() as usize);
    let (w, h) = (area.width() as usize, area.height() as usize);
    for row in 0..h {
        let from = (y0 + row) * stride + x0 * 4;
        scratch.data_mut()[row * w * 4..(row + 1) * w * 4]
            .copy_from_slice(&pixmap.data_mut()[from..from + w * 4]);
    }
    let shifted = transform.post_translate(-(x0 as f32), -(y0 as f32));
    scratch.fill_path(path, paint, rule, shifted, None);
    let coverage = clip.data();
    let mask_width = clip.width() as usize;
    let target = pixmap.data_mut();
    for row in 0..h {
        for col in 0..w {
            let m = u16::from(coverage[(y0 + row) * mask_width + x0 + col]);
            if m == 0 {
                continue;
            }
            let at = (y0 + row) * stride + (x0 + col) * 4;
            let made = &scratch.data()[(row * w + col) * 4..][..4];
            for (d, s) in target[at..at + 4].iter_mut().zip(made) {
                *d = ((u16::from(*d) * (255 - m) + u16::from(*s) * m + 127) / 255) as u8;
            }
        }
    }
}

/// Fill `path`, given in base space, with `ink`.
pub(crate) fn fill(context: CFTypeRef, path: &Elements, rule: sk::FillRule, stroke_ink: bool) {
    let Some(state) = state_of(context) else {
        return;
    };
    let state = state.current.clone();
    let Some(skia) = path.to_skia() else {
        return;
    };
    render(context, |pixmap, frame| {
        let ink = if stroke_ink {
            &state.stroke
        } else {
            &state.fill
        };
        let source = source_for(ink, &state, frame)?;
        let paint = source.paint(frame.base, &state, frame.swap)?;
        let transform = frame.base.sk();
        let clip = clip_for(&state, pixmap.width(), pixmap.height());
        fill_clipped(pixmap, &skia, &paint, rule, transform, clip);
        touched(&skia, transform, 1.0, pixmap.width(), pixmap.height())
    });
}

/// The stroke a graphics state's line style describes.
fn stroke_style(line: &Line) -> sk::Stroke {
    let dash = line.dash.as_ref().and_then(|(lengths, phase)| {
        let mut lengths: Vec<f32> = lengths.iter().map(|l| *l as f32).collect();
        if lengths.len() % 2 == 1 {
            lengths.extend_from_within(..);
        }
        sk::StrokeDash::new(lengths, *phase as f32)
    });
    sk::Stroke {
        width: line.width.max(0.0) as f32,
        miter_limit: line.miter as f32,
        line_cap: line.cap,
        line_join: line.join,
        dash,
    }
}

/// `path` in base space, carried back into user space for stroking.
fn in_user_space(path: &Elements, ctm: CGAffineTransform) -> Option<Elements> {
    let inverse = ctm.inverse()?;
    let mut user = Elements::default();
    user.append(inverse, path);
    Some(user)
}

/// Stroke `path`, given in base space, with the state's stroke ink and line.
///
/// The stroke is made in user space and the whole drawn through the CTM, so a
/// line width scales as Quartz's does.
pub(crate) fn stroke(context: CFTypeRef, path: &Elements) {
    let Some(state) = state_of(context) else {
        return;
    };
    let state = state.current.clone();
    let Some(user) = in_user_space(path, state.ctm) else {
        return;
    };
    let Some(skia) = user.to_skia() else {
        return;
    };
    render(context, |pixmap, frame| {
        let source = source_for(&state.stroke, &state, frame)?;
        let geometry = state.ctm.then(frame.base);
        let paint = source.paint(geometry, &state, frame.swap)?;
        let style = stroke_style(&state.line);
        let transform = geometry.sk();
        let clip = clip_for(&state, pixmap.width(), pixmap.height());
        match clip.is_some() && !clips_by_scaling_source(paint.blend_mode) {
            // The outline, filled, so `fill_clipped` can honour the clip.
            true => {
                let scale = sk::PathStroker::compute_resolution_scale(&transform);
                let dashed = match &style.dash {
                    Some(dash) => skia.dash(dash, scale)?,
                    None => skia.clone(),
                };
                let outline = dashed.stroke(&style, scale)?;
                fill_clipped(
                    pixmap,
                    &outline,
                    &paint,
                    sk::FillRule::Winding,
                    transform,
                    clip,
                );
            }
            false => pixmap.stroke_path(&skia, &paint, &style, transform, clip),
        }
        let reach = (state.line.width.max(1.0)
            * geometry.scale_factor()
            * state.line.miter.max(1.0)) as f32;
        touched(
            &skia,
            transform,
            reach + 1.0,
            pixmap.width(),
            pixmap.height(),
        )
    });
}

/// The outline a stroke of `path` (base space) would fill, in base space.
pub(crate) fn stroked_outline(context: CFTypeRef, path: &Elements) -> Option<Elements> {
    let state = &state_of(context)?.current;
    let user = in_user_space(path, state.ctm)?;
    let outline = user.to_skia()?.stroke(&stroke_style(&state.line), 1.0)?;
    Some(Elements::from_skia(&outline, state.ctm))
}

/// Intersect the clip with `path`, given in base space.
pub(crate) fn clip(context: CFTypeRef, path: &Elements, rule: sk::FillRule) {
    let Some(frame) = frame_of(context) else {
        return;
    };
    let Some(state) = state_of(context) else {
        return;
    };
    let gs = &mut state.current;
    let antialias = gs.antialias;
    let Some(skia) = path.to_skia() else {
        // An empty path clips everything away.
        gs.clip = sk::Mask::new(frame.width, frame.height).map(Arc::new);
        return;
    };
    let mask = match clip_for(gs, frame.width, frame.height) {
        Some(existing) => {
            let mut mask = existing.clone();
            mask.intersect_path(&skia, rule, antialias, frame.base.sk());
            mask
        }
        None => {
            let Some(mut mask) = sk::Mask::new(frame.width, frame.height) else {
                return;
            };
            mask.fill_path(&skia, rule, antialias, frame.base.sk());
            mask
        }
    };
    gs.clip = Some(Arc::new(mask));
}

/// Multiply the clip by `coverage`, a device-sized mask.
fn clip_by(gs: &mut GState, coverage: sk::Mask) {
    let mask = match clip_for(gs, coverage.width(), coverage.height()) {
        Some(existing) => {
            let mut mask = existing.clone();
            for (m, c) in mask.data_mut().iter_mut().zip(coverage.data()) {
                *m = ((u16::from(*m) * u16::from(*c) + 127) / 255) as u8;
            }
            mask
        }
        None => coverage,
    };
    gs.clip = Some(Arc::new(mask));
}

/// Run `body` with a context's clip as coverage bytes, row by row, and the
/// row width; `None` when the context is not clipped.
pub(crate) fn with_clip<R>(context: CFTypeRef, body: impl FnOnce(&[u8], u32) -> R) -> Option<R> {
    let frame = frame_of(context)?;
    let state = state_of(context)?;
    let mask = clip_for(&state.current, frame.width, frame.height)?;
    Some(body(mask.data(), mask.width()))
}

/// The device rectangle the clip leaves drawable, as left, top, right,
/// bottom.
pub(crate) fn clip_device_bounds(context: CFTypeRef) -> Option<[f64; 4]> {
    let frame = frame_of(context)?;
    let state = state_of(context)?;
    let Some(mask) = clip_for(&state.current, frame.width, frame.height) else {
        return Some([0.0, 0.0, f64::from(frame.width), f64::from(frame.height)]);
    };
    let (width, height) = (mask.width() as usize, mask.height() as usize);
    let mut bounds: Option<[usize; 4]> = None;
    for (at, value) in mask.data().iter().enumerate() {
        if *value == 0 {
            continue;
        }
        let (x, y) = (at % width, at / width);
        bounds = Some(match bounds {
            None => [x, y, x + 1, y + 1],
            Some([l, t, r, b]) => [l.min(x), t.min(y), r.max(x + 1), b.max(y + 1)],
        });
    }
    let _ = height;
    let [l, t, r, b] = bounds.unwrap_or([0, 0, 0, 0]);
    Some([l as f64, t as f64, r as f64, b as f64])
}

/// Coverage of `pixels` drawn into `rect` (user space), as a device-sized
/// mask. `coverage` is one byte per pixel, top row first.
fn coverage_mask(
    state: &GState,
    frame: &Frame,
    rect: NSRect,
    width: usize,
    height: usize,
    coverage: &[u8],
) -> Option<sk::Mask> {
    let mut alpha = sk::Pixmap::new(width as u32, height as u32)?;
    for (pixel, value) in alpha.data_mut().chunks_exact_mut(4).zip(coverage) {
        pixel.copy_from_slice(&[0, 0, 0, *value]);
    }
    let mut device = sk::Pixmap::new(frame.width, frame.height)?;
    let to_device = image_to_user(rect, width, height)
        .then(state.ctm)
        .then(frame.base);
    let paint = sk::PixmapPaint {
        opacity: 1.0,
        blend_mode: sk::BlendMode::Source,
        quality: state.quality,
    };
    device.draw_pixmap(0, 0, alpha.as_ref(), &paint, to_device.sk(), None);
    Some(sk::Mask::from_pixmap(device.as_ref(), sk::MaskType::Alpha))
}

/// `CGContextClipToMask`: the clip multiplied by an image's coverage over
/// `rect`.
pub(crate) fn clip_to_mask(context: CFTypeRef, rect: NSRect, image: CFTypeRef) {
    let Some(frame) = frame_of(context) else {
        return;
    };
    let Some((width, height, coverage)) = crate::graphics::image_coverage(image) else {
        return;
    };
    let Some(state) = state_of(context) else {
        return;
    };
    let Some(mask) = coverage_mask(&state.current, &frame, rect, width, height, &coverage) else {
        return;
    };
    clip_by(&mut state.current, mask);
}

/// Image pixels (top row first) to the user-space rectangle they fill.
fn image_to_user(rect: NSRect, width: usize, height: usize) -> CGAffineTransform {
    CGAffineTransform {
        a: rect.size.width / width as f64,
        b: 0.0,
        c: 0.0,
        d: -rect.size.height / height as f64,
        tx: rect.origin.x,
        ty: rect.origin.y + rect.size.height,
    }
}

/// A raster of premultiplied B, G, R, A pixels in the frame's channel order.
fn raster(bgra: Vec<u8>, width: usize, height: usize, swap: bool) -> Option<sk::Pixmap> {
    let mut pixels = bgra;
    if !swap {
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
    }
    sk::Pixmap::from_vec(pixels, sk::IntSize::from_wh(width as u32, height as u32)?)
}

fn rect_path(rect: NSRect) -> Option<sk::Path> {
    let r = sk::Rect::from_xywh(
        rect.origin.x as f32,
        rect.origin.y as f32,
        rect.size.width as f32,
        rect.size.height as f32,
    )?;
    Some(sk::PathBuilder::from_rect(r))
}

/// Draw premultiplied B, G, R, A pixels, top row first, into `rect` in user
/// space - the whole of `CGContextDrawImage` once the image is decoded.
pub(crate) fn draw_pixels(
    context: CFTypeRef,
    rect: NSRect,
    width: usize,
    height: usize,
    bgra: Vec<u8>,
) {
    if width == 0 || height == 0 || rect.size.width == 0.0 || rect.size.height == 0.0 {
        return;
    }
    let Some(state) = state_of(context) else {
        return;
    };
    let state = state.current.clone();
    let rect = standard(rect);
    let Some(path) = rect_path(rect) else {
        return;
    };
    render(context, |pixmap, frame| {
        let source = Source::Raster {
            pixmap: raster(bgra, width, height, frame.swap)?,
            to_device: image_to_user(rect, width, height)
                .then(state.ctm)
                .then(frame.base),
            spread: sk::SpreadMode::Pad,
            quality: state.quality,
            opacity: state.alpha as f32,
        };
        let geometry = state.ctm.then(frame.base);
        let paint = source.paint(geometry, &state, frame.swap)?;
        let transform = geometry.sk();
        let clip = clip_for(&state, pixmap.width(), pixmap.height());
        fill_clipped(
            pixmap,
            &path,
            &paint,
            sk::FillRule::Winding,
            transform,
            clip,
        );
        touched(&path, transform, 1.0, pixmap.width(), pixmap.height())
    });
}

/// Paint the fill through an image mask's coverage over `rect`.
pub(crate) fn draw_through_mask(
    context: CFTypeRef,
    rect: NSRect,
    width: usize,
    height: usize,
    coverage: Vec<u8>,
) {
    let Some(state) = state_of(context) else {
        return;
    };
    let mut state = state.current.clone();
    let rect = standard(rect);
    let Some(path) = rect_path(rect) else {
        return;
    };
    render(context, |pixmap, frame| {
        let mask = coverage_mask(&state, frame, rect, width, height, &coverage)?;
        clip_by(&mut state, mask);
        let source = source_for(&state.fill, &state, frame)?;
        let geometry = state.ctm.then(frame.base);
        let paint = source.paint(geometry, &state, frame.swap)?;
        let transform = geometry.sk();
        let clip = clip_for(&state, pixmap.width(), pixmap.height());
        fill_clipped(
            pixmap,
            &path,
            &paint,
            sk::FillRule::Winding,
            transform,
            clip,
        );
        touched(&path, transform, 1.0, pixmap.width(), pixmap.height())
    });
}

/// A rectangle with a non-negative size.
fn standard(rect: NSRect) -> NSRect {
    let (x, w) = match rect.size.width < 0.0 {
        true => (rect.origin.x + rect.size.width, -rect.size.width),
        false => (rect.origin.x, rect.size.width),
    };
    let (y, h) = match rect.size.height < 0.0 {
        true => (rect.origin.y + rect.size.height, -rect.size.height),
        false => (rect.origin.y, rect.size.height),
    };
    NSRect {
        origin: NSPoint { x, y },
        size: NSSize {
            width: w,
            height: h,
        },
    }
}

/// `CGContextClearRect`: the rectangle made transparent, within the clip.
pub(crate) fn clear(context: CFTypeRef, rect: NSRect) {
    let Some(state) = state_of(context) else {
        return;
    };
    let state = state.current.clone();
    let Some(path) = rect_path(standard(rect)) else {
        return;
    };
    render(context, |pixmap, frame| {
        let paint = sk::Paint {
            shader: sk::Shader::SolidColor(sk::Color::TRANSPARENT),
            blend_mode: sk::BlendMode::Clear,
            anti_alias: state.antialias,
            force_hq_pipeline: false,
        };
        let transform = state.ctm.then(frame.base).sk();
        let clip = clip_for(&state, pixmap.width(), pixmap.height());
        fill_clipped(
            pixmap,
            &path,
            &paint,
            sk::FillRule::Winding,
            transform,
            clip,
        );
        touched(&path, transform, 1.0, pixmap.width(), pixmap.height())
    });
}

/// A gradient to paint, from `cgpaint`'s shading.
pub(crate) struct Shading {
    /// The two circles' radii, for a radial shading.
    pub radial: Option<(f64, f64)>,
    pub start: (f64, f64),
    pub end: (f64, f64),
    pub extend: (bool, bool),
    pub stops: Vec<(f32, [f64; 4])>,
}

/// `CGContextDrawShading`: the shading over the clip, in user space.
pub(crate) fn draw_shading(context: CFTypeRef, shading: Shading) {
    let Some(state) = state_of(context) else {
        return;
    };
    let state = state.current.clone();
    render(context, |pixmap, frame| {
        let user_to_device = state.ctm.then(frame.base);
        let device_to_user = user_to_device.inverse()?;
        // The whole target, in user space, is the most a shading can cover.
        let corners = [
            (0.0, 0.0),
            (f64::from(frame.width), 0.0),
            (0.0, f64::from(frame.height)),
            (f64::from(frame.width), f64::from(frame.height)),
        ]
        .map(|(x, y)| device_to_user.apply(x, y));
        let reach = corners
            .iter()
            .flat_map(|(x, y)| [x.abs(), y.abs()])
            .fold(0.0f64, f64::max)
            * 4.0
            + 1.0;
        let mut region = Elements::default();
        let (stops, start, end, radial) = match shading.radial {
            None => {
                // The band between the perpendiculars through the ends,
                // carried out to the far side of the target on an extended end.
                let (dx, dy) = (
                    shading.end.0 - shading.start.0,
                    shading.end.1 - shading.start.1,
                );
                let length = dx.hypot(dy);
                if length == 0.0 {
                    return None;
                }
                let (ux, uy) = (dx / length, dy / length);
                let (px, py) = (-uy * reach, ux * reach);
                let s0 = if shading.extend.0 { -reach } else { 0.0 };
                let s1 = if shading.extend.1 {
                    length + reach
                } else {
                    length
                };
                let at = |s: f64| (shading.start.0 + ux * s, shading.start.1 + uy * s);
                let (a, b) = (at(s0), at(s1));
                region.move_to(IDENTITY, a.0 + px, a.1 + py);
                region.line_to(IDENTITY, b.0 + px, b.1 + py);
                region.line_to(IDENTITY, b.0 - px, b.1 - py);
                region.line_to(IDENTITY, a.0 - px, a.1 - py);
                region.close();
                (shading.stops.clone(), shading.start, shading.end, None)
            }
            Some((r0, r1)) => {
                let concentric = shading.start == shading.end;
                match shading.extend.1 {
                    true => region.rect(
                        IDENTITY,
                        NSRect {
                            origin: NSPoint {
                                x: -reach,
                                y: -reach,
                            },
                            size: NSSize {
                                width: reach * 2.0,
                                height: reach * 2.0,
                            },
                        },
                    ),
                    false => region.ellipse(
                        IDENTITY,
                        NSRect {
                            origin: NSPoint {
                                x: shading.end.0 - r1,
                                y: shading.end.1 - r1,
                            },
                            size: NSSize {
                                width: r1 * 2.0,
                                height: r1 * 2.0,
                            },
                        },
                    ),
                }
                // Concentric circles are exact: the gradient runs from the
                // centre, so the stops move out to where the inner circle is.
                // FIXME (ACCURACY): two circles with distinct centres and a
                // nonzero inner radius are drawn as if the inner were a point.
                let stops = match concentric && r1 > 0.0 {
                    true => shading
                        .stops
                        .iter()
                        .map(|(at, c)| (((r0 + f64::from(*at) * (r1 - r0)) / r1) as f32, *c))
                        .collect(),
                    false => shading.stops.clone(),
                };
                (stops, shading.start, shading.end, Some((r0, r1)))
            }
        };
        let stops: Vec<(f32, [f64; 4])> = stops
            .into_iter()
            .map(|(at, [r, g, b, a])| (at.clamp(0.0, 1.0), [r, g, b, a * state.alpha]))
            .collect();
        let source = Source::Gradient {
            radial,
            start,
            end,
            stops,
            to_device: user_to_device,
        };
        let paint = source.paint(user_to_device, &state, frame.swap)?;
        let skia = region.to_skia()?;
        let transform = user_to_device.sk();
        let clip = clip_for(&state, pixmap.width(), pixmap.height());
        fill_clipped(
            pixmap,
            &skia,
            &paint,
            sk::FillRule::Winding,
            transform,
            clip,
        );
        touched(&skia, transform, 1.0, pixmap.width(), pixmap.height())
    });
}

/// Glyph outlines, at positions in text space, drawn as the text drawing mode
/// says: filled, stroked, added to the clip, or not at all.
pub(crate) fn draw_glyphs(
    context: CFTypeRef,
    face: &crate::fonts::LoadedFace,
    size: f64,
    glyphs: &[u16],
    positions: &[(f64, f64)],
) {
    let Some(state) = state_of(context) else {
        return;
    };
    let mode = state.current.text_mode;
    let units = f64::from(face.face().units_per_em().max(1));
    let s = size / units;
    let mut text = Elements::default();
    for (glyph, (x, y)) in glyphs.iter().zip(positions) {
        let at = CGAffineTransform {
            a: s,
            d: s,
            tx: *x,
            ty: *y,
            ..IDENTITY
        };
        crate::cgpath::glyph_outline(face.face(), *glyph, at, &mut text);
    }
    if text.is_empty() {
        return;
    }
    // Into base space, which is where a context's paths are drawn from.
    let to_base = state.current.text_matrix.then(state.current.ctm);
    let mut path = Elements::default();
    path.append(to_base, &text);
    // `kCGTextFill`, `Stroke`, `FillStroke`, `Invisible`, then the same four
    // with the glyphs also added to the clip.
    if matches!(mode, 0 | 2 | 4 | 6) {
        fill(context, &path, sk::FillRule::Winding, false);
    }
    if matches!(mode, 1 | 2 | 5 | 6) {
        stroke(context, &path);
    }
    if mode >= 4 {
        clip(context, &path, sk::FillRule::Winding);
    }
}

/// Write a pixel buffer's B, G, R, A copy for `CGBitmapContextCreateImage` -
/// the context's own bytes in whatever layout - as premultiplied B, G, R, A.
pub(crate) fn snapshot_bgra(ctx: &BitmapContext) -> Option<Vec<u8>> {
    if ctx.data.is_null() {
        return None;
    }
    // SAFETY: a bitmap context's storage is `bytes_per_row * height` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(ctx.data, ctx.bytes_per_row * ctx.height) };
    let mut rgba = vec![0u8; ctx.width * ctx.height * 4];
    read_in(bytes, ctx, layout(ctx), &mut rgba);
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inverse_undoes_its_transform() {
        let t = CGAffineTransform {
            a: 2.0,
            b: 0.5,
            c: -1.0,
            d: 3.0,
            tx: 4.0,
            ty: -7.0,
        };
        let back = t.then(t.inverse().expect("invertible"));
        let (x, y) = back.apply(1.25, -3.5);
        assert!((x - 1.25).abs() < 1e-9 && (y + 3.5).abs() < 1e-9);
    }
}
