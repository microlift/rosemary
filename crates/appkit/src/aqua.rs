//! Aqua-like controls painted with Quartz, for the callers that ask the
//! system to draw a widget into their own `CGContext`: the `HIThemeDraw...`
//! calls and a standalone `-[NSCell drawWithFrame:inView:]`.
//!
//! FIXME (ACCURACY): these are flat shapes in Aqua's proportions and colours,
//! not Apple's artwork: there is no copy of the CoreUI assets to draw from.
//! Sizes and placement follow the regular, small and mini controls so a
//! toolkit's layout and hit testing still line up with what is drawn.

use corefoundation::base::CFTypeRef;
use foundation::geometry::{NSPoint, NSRect, NSSize};

use crate::cgcontext as cg;
use crate::quartz::{self, Shading};

type Rgba = [f64; 4];

const ACCENT_TOP: Rgba = [0.33, 0.62, 0.98, 1.0];
const ACCENT_BOTTOM: Rgba = [0.07, 0.43, 0.92, 1.0];
const ACCENT_BORDER: Rgba = [0.0, 0.25, 0.65, 0.6];
const FOCUS: Rgba = [0.23, 0.53, 0.96, 0.55];
const TRACK: Rgba = [0.98, 0.98, 0.98, 1.0];
const TRACK_EDGE: Rgba = [0.0, 0.0, 0.0, 0.12];
const THUMB: Rgba = [0.0, 0.0, 0.0, 0.38];
const DARK_TRACK: Rgba = [0.17, 0.17, 0.17, 1.0];
const DARK_TRACK_EDGE: Rgba = [1.0, 1.0, 1.0, 0.08];
const LIGHT_THUMB: Rgba = [1.0, 1.0, 1.0, 0.38];
const CLEAR: Rgba = [0.0; 4];
const WHITE: Rgba = [1.0; 4];

/// The colours of the controls in one appearance.
pub(crate) struct Palette {
    face_top: Rgba,
    face_bottom: Rgba,
    pressed_top: Rgba,
    pressed_bottom: Rgba,
    /// A line along the top of a face, the light catching its edge.
    highlight: Rgba,
    border: Rgba,
    border_disabled: Rgba,
    /// The drop shadow a point under a face.
    shadow: Rgba,
    mark: Rgba,
    mark_secondary: Rgba,
    mark_disabled: Rgba,
    field_fill: Rgba,
    field_top: Rgba,
    field_side: Rgba,
    /// A hairline between parts: a header's divider, a group box's edge.
    edge: Rgba,
    panel: Rgba,
    menu: Rgba,
    trough_top: Rgba,
    trough_bottom: Rgba,
    slider_track: Rgba,
    tick: Rgba,
}

const AQUA: Palette = Palette {
    face_top: WHITE,
    face_bottom: [0.94, 0.94, 0.94, 1.0],
    pressed_top: [0.80, 0.80, 0.80, 1.0],
    pressed_bottom: [0.74, 0.74, 0.74, 1.0],
    highlight: CLEAR,
    border: [0.0, 0.0, 0.0, 0.32],
    border_disabled: [0.0, 0.0, 0.0, 0.16],
    shadow: [0.0, 0.0, 0.0, 0.08],
    mark: [0.0, 0.0, 0.0, 0.85],
    mark_secondary: [0.0, 0.0, 0.0, 0.6],
    mark_disabled: [0.0, 0.0, 0.0, 0.3],
    field_fill: WHITE,
    field_top: [0.0, 0.0, 0.0, 0.42],
    field_side: [0.0, 0.0, 0.0, 0.25],
    edge: TRACK_EDGE,
    panel: [0.0, 0.0, 0.0, 0.04],
    menu: [0.97, 0.97, 0.97, 1.0],
    trough_top: [0.88, 0.88, 0.88, 1.0],
    trough_bottom: [0.94, 0.94, 0.94, 1.0],
    slider_track: [0.0, 0.0, 0.0, 0.2],
    tick: [0.0, 0.0, 0.0, 0.5],
};

/// Dark Aqua as Catalina draws it: lifted grey faces with a lit top edge and
/// a shadow beneath. The borders are stronger than Apple's so that a control
/// still reads against the window's grey.
const DARK_AQUA: Palette = Palette {
    face_top: [0.42, 0.42, 0.42, 1.0],
    face_bottom: [0.38, 0.38, 0.38, 1.0],
    pressed_top: [0.52, 0.52, 0.52, 1.0],
    pressed_bottom: [0.48, 0.48, 0.48, 1.0],
    highlight: [1.0, 1.0, 1.0, 0.2],
    border: [0.0, 0.0, 0.0, 0.35],
    border_disabled: [0.0, 0.0, 0.0, 0.2],
    shadow: [0.0, 0.0, 0.0, 0.3],
    mark: [1.0, 1.0, 1.0, 0.88],
    mark_secondary: [1.0, 1.0, 1.0, 0.6],
    mark_disabled: [1.0, 1.0, 1.0, 0.3],
    field_fill: [1.0, 1.0, 1.0, 0.05],
    field_top: [1.0, 1.0, 1.0, 0.3],
    field_side: [1.0, 1.0, 1.0, 0.2],
    edge: [1.0, 1.0, 1.0, 0.12],
    panel: [1.0, 1.0, 1.0, 0.03],
    menu: [0.16, 0.16, 0.16, 1.0],
    trough_top: [0.20, 0.20, 0.20, 1.0],
    trough_bottom: [0.25, 0.25, 0.25, 1.0],
    slider_track: [1.0, 1.0, 1.0, 0.2],
    tick: [1.0, 1.0, 1.0, 0.5],
};

impl Palette {
    /// The palette of the current drawing appearance.
    fn current() -> &'static Palette {
        match crate::appearance::is_dark() {
            true => &DARK_AQUA,
            false => &AQUA,
        }
    }
}

/// The size class a control is drawn at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Size {
    Regular,
    Small,
    Mini,
}

impl Size {
    /// From an `NSControlSize`.
    pub(crate) fn from_control_size(size: usize) -> Size {
        match size {
            1 => Size::Small,
            2 => Size::Mini,
            _ => Size::Regular,
        }
    }
}

/// How a control is drawn: its interaction state.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Look {
    pub pressed: bool,
    pub disabled: bool,
    pub focused: bool,
    /// The default button, drawn in the accent colour.
    pub default: bool,
}

/// What a check box or radio button shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Value {
    Off,
    On,
    Mixed,
}

/// A context with its graphics state saved and user space turned y-down over
/// `bounds`, whichever way the caller's runs, so every painter below draws
/// with the top edge at `bounds.origin.y`. The state is restored on drop.
pub(crate) struct Painter {
    context: CFTypeRef,
    palette: &'static Palette,
}

impl Painter {
    /// `y_down` is whether the caller's user space already grows downward,
    /// as a flipped view's or `kHIThemeOrientationNormal` does.
    pub(crate) fn new(context: CFTypeRef, bounds: NSRect, y_down: bool) -> Option<Painter> {
        quartz::state_of(context)?;
        // SAFETY: a live context, checked above.
        unsafe {
            cg::CGContextSaveGState(context);
            if !y_down {
                let middle = 2.0 * bounds.origin.y + bounds.size.height;
                cg::CGContextTranslateCTM(context, 0.0, middle);
                cg::CGContextScaleCTM(context, 1.0, -1.0);
            }
            cg::CGContextSetShouldAntialias(context, 1);
            cg::CGContextBeginPath(context);
        }
        Some(Painter {
            context,
            palette: Palette::current(),
        })
    }

    fn rounded(&self, r: NSRect, radius: f64) {
        let c = self.context;
        let radius = radius
            .min(r.size.width / 2.0)
            .min(r.size.height / 2.0)
            .max(0.0);
        let (x0, y0) = (r.origin.x, r.origin.y);
        let (x1, y1) = (x0 + r.size.width, y0 + r.size.height);
        // SAFETY: the painter's live context.
        unsafe {
            cg::CGContextMoveToPoint(c, x0 + radius, y0);
            cg::CGContextAddArcToPoint(c, x1, y0, x1, y1, radius);
            cg::CGContextAddArcToPoint(c, x1, y1, x0, y1, radius);
            cg::CGContextAddArcToPoint(c, x0, y1, x0, y0, radius);
            cg::CGContextAddArcToPoint(c, x0, y0, x1, y0, radius);
            cg::CGContextClosePath(c);
        }
    }

    fn fill_rounded(&self, r: NSRect, radius: f64, color: Rgba) {
        self.rounded(r, radius);
        // SAFETY: the painter's live context.
        unsafe {
            cg::CGContextSetRGBFillColor(self.context, color[0], color[1], color[2], color[3]);
            cg::CGContextFillPath(self.context);
        }
    }

    /// A vertical gradient from `top` to `bottom` over a rounded rectangle.
    fn gradient_rounded(&self, r: NSRect, radius: f64, top: Rgba, bottom: Rgba) {
        let c = self.context;
        // SAFETY: the painter's live context.
        unsafe { cg::CGContextSaveGState(c) };
        self.rounded(r, radius);
        // SAFETY: as above.
        unsafe { cg::CGContextClip(c) };
        quartz::draw_shading(
            c,
            Shading {
                radial: None,
                start: (r.origin.x, r.origin.y),
                end: (r.origin.x, r.origin.y + r.size.height),
                extend: (true, true),
                stops: vec![(0.0, top), (1.0, bottom)],
            },
        );
        // SAFETY: as above.
        unsafe { cg::CGContextRestoreGState(c) };
    }

    /// A hairline just inside a rounded rectangle.
    fn stroke_rounded(&self, r: NSRect, radius: f64, width: f64, color: Rgba) {
        self.rounded(inset(r, width / 2.0, width / 2.0), radius - width / 2.0);
        self.stroke(width, color);
    }

    fn stroke(&self, width: f64, color: Rgba) {
        // SAFETY: the painter's live context.
        unsafe {
            cg::CGContextSetLineWidth(self.context, width);
            cg::CGContextSetRGBStrokeColor(self.context, color[0], color[1], color[2], color[3]);
            cg::CGContextStrokePath(self.context);
        }
    }

    fn fill(&self, color: Rgba) {
        // SAFETY: the painter's live context.
        unsafe {
            cg::CGContextSetRGBFillColor(self.context, color[0], color[1], color[2], color[3]);
            cg::CGContextFillPath(self.context);
        }
    }

    fn rect(&self, r: NSRect, color: Rgba) {
        // SAFETY: the painter's live context.
        unsafe {
            cg::CGContextSetRGBFillColor(self.context, color[0], color[1], color[2], color[3]);
            cg::CGContextFillRect(self.context, r);
        }
    }

    fn polyline(&self, points: &[(f64, f64)], close: bool) {
        let c = self.context;
        let Some((first, rest)) = points.split_first() else {
            return;
        };
        // SAFETY: the painter's live context.
        unsafe {
            cg::CGContextMoveToPoint(c, first.0, first.1);
            for p in rest {
                cg::CGContextAddLineToPoint(c, p.0, p.1);
            }
            if close {
                cg::CGContextClosePath(c);
            }
        }
    }

    fn ellipse(&self, r: NSRect) {
        // SAFETY: the painter's live context.
        unsafe { cg::CGContextAddEllipseInRect(self.context, r) };
    }

    /// A button face: the gradient and its border, or the accent colour.
    fn face(&self, r: NSRect, radius: f64, look: Look, accent: bool) {
        let palette = self.palette;
        let accent = accent && !look.disabled;
        let (top, bottom, border) = match (look.pressed, accent) {
            (true, false) => (palette.pressed_top, palette.pressed_bottom, palette.border),
            (false, true) => (ACCENT_TOP, ACCENT_BOTTOM, ACCENT_BORDER),
            (true, true) => (ACCENT_BOTTOM, ACCENT_BOTTOM, ACCENT_BORDER),
            (false, false) => (palette.face_top, palette.face_bottom, palette.border),
        };
        let border = if look.disabled {
            palette.border_disabled
        } else {
            border
        };
        // A one-point shadow under the face.
        self.fill_rounded(offset(r, 0.0, 0.5), radius, palette.shadow);
        if palette.highlight[3] > 0.0 && !accent {
            // The face drawn a point lower leaves the highlight showing
            // along the top, following its corners.
            self.fill_rounded(r, radius, palette.highlight);
            self.gradient_rounded(
                rect(
                    r.origin.x,
                    r.origin.y + 1.0,
                    r.size.width,
                    r.size.height - 1.0,
                ),
                radius,
                top,
                bottom,
            );
        } else {
            self.gradient_rounded(r, radius, top, bottom);
        }
        self.stroke_rounded(r, radius, 1.0, border);
        if look.focused {
            self.focus_ring_rounded(r, radius);
        }
    }

    fn focus_ring_rounded(&self, r: NSRect, radius: f64) {
        self.rounded(inset(r, -1.5, -1.5), radius + 1.5);
        self.stroke(3.0, FOCUS);
    }
}

impl Drop for Painter {
    fn drop(&mut self) {
        // SAFETY: the state `new` saved on this live context.
        unsafe { cg::CGContextRestoreGState(self.context) };
    }
}

pub(crate) fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect {
        origin: NSPoint { x, y },
        size: NSSize {
            width: w.max(0.0),
            height: h.max(0.0),
        },
    }
}

fn inset(r: NSRect, dx: f64, dy: f64) -> NSRect {
    rect(
        r.origin.x + dx,
        r.origin.y + dy,
        r.size.width - 2.0 * dx,
        r.size.height - 2.0 * dy,
    )
}

fn offset(r: NSRect, dx: f64, dy: f64) -> NSRect {
    rect(
        r.origin.x + dx,
        r.origin.y + dy,
        r.size.width,
        r.size.height,
    )
}

/// `r`'s height cut to `height`, centred.
fn centred_height(r: NSRect, height: f64) -> NSRect {
    let height = height.min(r.size.height);
    rect(
        r.origin.x,
        r.origin.y + (r.size.height - height) / 2.0,
        r.size.width,
        height,
    )
}

/// A square of `side` centred in `r`, on whole points so its edges are crisp.
fn centred_square(r: NSRect, side: f64) -> NSRect {
    let side = side.min(r.size.width).min(r.size.height);
    rect(
        (r.origin.x + (r.size.width - side) / 2.0).round(),
        (r.origin.y + (r.size.height - side) / 2.0).round(),
        side,
        side,
    )
}

/// A push button's rounded bezel, which sits inside the frame AppKit sizes
/// for it, clear of the shadow and focus ring.
pub(crate) fn push_button(p: &Painter, frame: NSRect, size: Size, look: Look) {
    let (height, side) = match size {
        Size::Regular => (21.0, 6.0),
        Size::Small => (18.0, 5.0),
        Size::Mini => (15.0, 1.0),
    };
    // A frame narrower than the margins the bezel keeps is all bezel.
    let side = if frame.size.width > 4.0 * side {
        side
    } else {
        1.0
    };
    let bezel = centred_height(inset(frame, side, 0.0), height);
    let bezel = offset(bezel, 0.0, -0.5);
    p.face(bezel, 4.0, look, look.default);
}

/// A bevel or square button: the whole frame, barely rounded.
pub(crate) fn bevel_button(p: &Painter, frame: NSRect, look: Look) {
    p.face(inset(frame, 0.5, 0.5), 2.0, look, false);
}

/// A rounded rectangle in the frame, as a textured or round-rect button.
pub(crate) fn round_button(p: &Painter, frame: NSRect, look: Look) {
    p.face(inset(frame, 0.5, 0.5), frame.size.height / 2.0, look, false);
}

fn check_side(size: Size) -> f64 {
    match size {
        Size::Regular => 14.0,
        Size::Small => 12.0,
        Size::Mini => 10.0,
    }
}

/// A check box: its box at the leading edge of `frame`, vertically centred.
pub(crate) fn check_box(p: &Painter, frame: NSRect, size: Size, value: Value, look: Look) {
    let side = check_side(size);
    let column = rect(
        frame.origin.x,
        frame.origin.y,
        side.max(frame.size.width.min(side + 4.0)),
        frame.size.height,
    );
    let r = centred_square(column, side);
    let on = value != Value::Off;
    p.face(r, 3.0, look, on);
    let mark = match (on, look.disabled) {
        (_, true) => p.palette.mark_disabled,
        (true, false) => WHITE,
        (false, false) => p.palette.mark,
    };
    let (x, y, s) = (r.origin.x, r.origin.y, side);
    match value {
        Value::Off => {}
        Value::On => {
            p.polyline(
                &[
                    (x + s * 0.24, y + s * 0.52),
                    (x + s * 0.43, y + s * 0.72),
                    (x + s * 0.78, y + s * 0.28),
                ],
                false,
            );
            // SAFETY: the painter's live context.
            unsafe {
                cg::CGContextSetLineCap(p.context, 1);
                cg::CGContextSetLineJoin(p.context, 1);
            }
            p.stroke((s / 7.0).max(1.5), mark);
        }
        Value::Mixed => {
            p.rect(rect(x + s * 0.25, y + s * 0.44, s * 0.5, s * 0.12), mark);
        }
    }
}

/// A radio button: a circle at the leading edge of `frame`, with a dot when
/// it is on.
pub(crate) fn radio_button(p: &Painter, frame: NSRect, size: Size, value: Value, look: Look) {
    let side = check_side(size) + 1.0;
    let column = rect(
        frame.origin.x,
        frame.origin.y,
        side.max(frame.size.width.min(side + 4.0)),
        frame.size.height,
    );
    let r = centred_square(column, side);
    let on = value != Value::Off;
    p.face(r, side / 2.0, look, on);
    if on {
        let dot = centred_square(r, (side * 0.4).round());
        p.ellipse(dot);
        p.fill(if look.disabled {
            p.palette.mark_disabled
        } else {
            WHITE
        });
    }
}

/// The two arrows of a pop-up button's indicator, centred on `(x, y)`.
fn popup_arrows(p: &Painter, x: f64, y: f64, s: f64, color: Rgba) {
    p.polyline(
        &[(x - s, y - 1.5), (x, y - 1.5 - s * 1.2), (x + s, y - 1.5)],
        true,
    );
    p.polyline(
        &[(x - s, y + 1.5), (x, y + 1.5 + s * 1.2), (x + s, y + 1.5)],
        true,
    );
    p.fill(color);
}

/// A pop-up button: a push button with the accent-coloured arrow segment at
/// its trailing edge.
pub(crate) fn popup_button(p: &Painter, frame: NSRect, size: Size, look: Look, pulls_down: bool) {
    let (height, side) = match size {
        Size::Regular => (21.0, 3.0),
        Size::Small => (18.0, 3.0),
        Size::Mini => (15.0, 1.0),
    };
    let bezel = offset(centred_height(inset(frame, side, 0.0), height), 0.0, -0.5);
    p.face(bezel, 4.0, look, false);
    let segment = (bezel.size.height - 4.0).round();
    let seg = rect(
        bezel.origin.x + bezel.size.width - segment - 2.0,
        bezel.origin.y + 2.0,
        segment,
        bezel.size.height - 4.0,
    );
    let disabled = look.disabled;
    if !disabled {
        p.gradient_rounded(seg, 3.0, ACCENT_TOP, ACCENT_BOTTOM);
    }
    let color = if disabled {
        p.palette.mark_disabled
    } else {
        WHITE
    };
    let (cx, cy) = (
        seg.origin.x + seg.size.width / 2.0,
        seg.origin.y + seg.size.height / 2.0,
    );
    let s = (segment / 6.0).max(2.0);
    match pulls_down {
        false => popup_arrows(p, cx, cy, s, color),
        true => {
            p.polyline(
                &[
                    (cx - s, cy - s / 2.0),
                    (cx + s, cy - s / 2.0),
                    (cx, cy + s * 0.8),
                ],
                true,
            );
            p.fill(color);
        }
    }
}

/// An editable combo box: a field with the arrow button at its trailing edge.
pub(crate) fn combo_box(p: &Painter, frame: NSRect, size: Size, look: Look) {
    let height = match size {
        Size::Regular => 22.0,
        Size::Small => 19.0,
        Size::Mini => 16.0,
    };
    let r = centred_height(frame, height);
    let button = height - 1.0;
    text_field(
        p,
        rect(
            r.origin.x,
            r.origin.y,
            r.size.width - button + 2.0,
            r.size.height,
        ),
        look.focused,
    );
    let b = rect(
        r.origin.x + r.size.width - button,
        r.origin.y,
        button,
        r.size.height,
    );
    p.face(
        b,
        4.0,
        Look {
            focused: false,
            ..look
        },
        !look.disabled,
    );
    let color = if look.disabled {
        p.palette.mark_disabled
    } else {
        WHITE
    };
    let (cx, cy) = (
        b.origin.x + b.size.width / 2.0,
        b.origin.y + b.size.height / 2.0,
    );
    let s = (button / 6.0).max(2.0);
    p.polyline(
        &[
            (cx - s, cy - s / 2.0),
            (cx + s, cy - s / 2.0),
            (cx, cy + s * 0.8),
        ],
        true,
    );
    p.fill(color);
}

/// A square text field's frame: a darker top edge than the others, inside
/// `r`, which the caller has filled itself.
pub(crate) fn text_field(p: &Painter, r: NSRect, focused: bool) {
    let (x, y, w, h) = (r.origin.x, r.origin.y, r.size.width, r.size.height);
    p.rect(rect(x, y, w, 1.0), p.palette.field_top);
    p.rect(rect(x, y + 1.0, 1.0, h - 1.0), p.palette.field_side);
    p.rect(
        rect(x + w - 1.0, y + 1.0, 1.0, h - 1.0),
        p.palette.field_side,
    );
    p.rect(
        rect(x + 1.0, y + h - 1.0, w - 2.0, 1.0),
        p.palette.field_side,
    );
    if focused {
        focus_ring(p, r);
    }
}

/// A bezeled text field's interior, which the bezel artwork fills whether or
/// not the field draws its own background.
pub(crate) fn text_field_fill(p: &Painter, r: NSRect) {
    p.rect(r, p.palette.field_fill);
}

/// A round (search) field's frame: a capsule outline.
pub(crate) fn round_field(p: &Painter, r: NSRect, focused: bool) {
    let radius = r.size.height / 2.0;
    p.stroke_rounded(r, radius, 1.0, p.palette.field_side);
    if focused {
        p.focus_ring_rounded(r, radius);
    }
}

/// A list box's frame: one even line.
pub(crate) fn list_box(p: &Painter, r: NSRect, focused: bool) {
    let (x, y, w, h) = (r.origin.x, r.origin.y, r.size.width, r.size.height);
    p.rect(rect(x, y, w, 1.0), p.palette.field_side);
    p.rect(rect(x, y + 1.0, 1.0, h - 1.0), p.palette.field_side);
    p.rect(
        rect(x + w - 1.0, y + 1.0, 1.0, h - 1.0),
        p.palette.field_side,
    );
    p.rect(
        rect(x + 1.0, y + h - 1.0, w - 2.0, 1.0),
        p.palette.field_side,
    );
    if focused {
        focus_ring(p, r);
    }
}

/// The keyboard focus ring outside `r`.
pub(crate) fn focus_ring(p: &Painter, r: NSRect) {
    p.rounded(inset(r, -1.5, -1.5), 2.0);
    p.stroke(3.0, FOCUS);
}

/// A list or table column header: a light gradient with a separator on the
/// trailing edge and a line beneath.
pub(crate) fn list_header(p: &Painter, r: NSRect, look: Look, sorted: bool) {
    let (top, bottom) = match (look.pressed, sorted) {
        (true, _) => (p.palette.pressed_top, p.palette.pressed_bottom),
        (false, true) => ([0.93, 0.95, 0.99, 1.0], [0.86, 0.89, 0.96, 1.0]),
        (false, false) => (p.palette.face_top, p.palette.face_bottom),
    };
    p.gradient_rounded(r, 0.0, top, bottom);
    let (x, y, w, h) = (r.origin.x, r.origin.y, r.size.width, r.size.height);
    p.rect(rect(x, y + h - 1.0, w, 1.0), p.palette.edge);
    p.rect(rect(x + w - 1.0, y + 3.0, 1.0, h - 6.0), p.palette.edge);
}

/// A disclosure triangle pointing right, down or left.
pub(crate) fn disclosure(p: &Painter, r: NSRect, direction: u16, look: Look) {
    let s = (r.size.width.min(r.size.height) * 0.28).max(3.0);
    let (cx, cy) = (
        r.origin.x + r.size.width / 2.0,
        r.origin.y + r.size.height / 2.0,
    );
    let points = match direction {
        1 => [
            (cx - s, cy - s * 0.6),
            (cx + s, cy - s * 0.6),
            (cx, cy + s * 0.8),
        ],
        2 => [
            (cx + s * 0.6, cy - s),
            (cx + s * 0.6, cy + s),
            (cx - s * 0.8, cy),
        ],
        _ => [
            (cx - s * 0.6, cy - s),
            (cx - s * 0.6, cy + s),
            (cx + s * 0.8, cy),
        ],
    };
    p.polyline(&points, true);
    p.fill(match (look.disabled, look.pressed) {
        (true, _) => p.palette.mark_disabled,
        (false, true) => p.palette.mark,
        (false, false) => p.palette.mark_secondary,
    });
}

/// Stepper arrows: two stacked buttons, the pressed half shaded.
pub(crate) fn inc_dec(
    p: &Painter,
    frame: NSRect,
    size: Size,
    look: Look,
    pressed_half: Option<bool>,
) {
    let (w, h) = match size {
        Size::Regular => (13.0, 23.0),
        Size::Small => (11.0, 20.0),
        Size::Mini => (9.0, 15.0),
    };
    let r = rect(
        (frame.origin.x + (frame.size.width - w) / 2.0).round(),
        (frame.origin.y + (frame.size.height - h) / 2.0).round(),
        w,
        h,
    );
    p.face(
        r,
        4.0,
        Look {
            pressed: false,
            ..look
        },
        false,
    );
    let half = h / 2.0;
    if let Some(up) = pressed_half {
        let y = if up {
            r.origin.y + 1.0
        } else {
            r.origin.y + half
        };
        p.rect(
            rect(r.origin.x + 1.0, y, w - 2.0, half - 1.0),
            [0.0, 0.0, 0.0, 0.15],
        );
    }
    p.rect(
        rect(r.origin.x + 1.0, r.origin.y + half - 0.5, w - 2.0, 1.0),
        p.palette.edge,
    );
    let color = if look.disabled {
        p.palette.mark_disabled
    } else {
        p.palette.mark
    };
    let cx = r.origin.x + w / 2.0;
    let s = (w / 4.5).max(1.5);
    let up_y = r.origin.y + half / 2.0;
    let down_y = r.origin.y + half * 1.5;
    p.polyline(
        &[
            (cx - s, up_y + s * 0.5),
            (cx + s, up_y + s * 0.5),
            (cx, up_y - s * 0.6),
        ],
        true,
    );
    p.polyline(
        &[
            (cx - s, down_y - s * 0.5),
            (cx + s, down_y - s * 0.5),
            (cx, down_y + s * 0.6),
        ],
        true,
    );
    p.fill(color);
}

/// A plain arrow button pointing down, as a pop-up's arrow alone.
pub(crate) fn arrow_button(p: &Painter, frame: NSRect, look: Look) {
    p.face(inset(frame, 0.5, 0.5), 3.0, look, false);
    let (cx, cy) = (
        frame.origin.x + frame.size.width / 2.0,
        frame.origin.y + frame.size.height / 2.0,
    );
    let s = (frame.size.width.min(frame.size.height) / 5.0).max(2.0);
    p.polyline(
        &[
            (cx - s, cy - s / 2.0),
            (cx + s, cy - s / 2.0),
            (cx, cy + s * 0.8),
        ],
        true,
    );
    p.fill(if look.disabled {
        p.palette.mark_disabled
    } else {
        p.palette.mark
    });
}

/// Where a scroll bar's thumb lies along its track, from the track's range,
/// value and the visible size: offset and length, in points.
pub(crate) fn thumb_span(
    length: f64,
    min: i32,
    max: i32,
    value: i32,
    view: i32,
) -> Option<(f64, f64)> {
    let range = f64::from(max) - f64::from(min);
    if range <= 0.0 || length <= 0.0 {
        return None;
    }
    let view = f64::from(view.max(0));
    let minimum = 18.0f64.min(length);
    let thumb = (length * view / (range + view)).clamp(minimum, length);
    let at = ((f64::from(value) - f64::from(min)) / range).clamp(0.0, 1.0) * (length - thumb);
    Some((at, thumb))
}

/// A legacy (always shown) scroll bar: a pale track with a capsule thumb.
pub(crate) fn scroll_bar(
    p: &Painter,
    r: NSRect,
    horizontal: bool,
    thumb: Option<(f64, f64)>,
    pressed: bool,
) {
    scroll_track(p, r, horizontal, Shade::Light);
    let Some((at, length)) = thumb else {
        return;
    };
    let (x, y, w, h) = (r.origin.x, r.origin.y, r.size.width, r.size.height);
    let knob = match horizontal {
        true => rect(x + at, y, length, h),
        false => rect(x, y + at, w, length),
    };
    scroll_thumb(p, knob, horizontal, pressed, Shade::Light);
}

/// What a scroll bar is drawn to sit on: a light background, with a pale
/// track and a dark thumb, or a dark one with the reverse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shade {
    Light,
    Dark,
}

/// A scroll bar's track, edged on the side facing the content.
pub(crate) fn scroll_track(p: &Painter, r: NSRect, horizontal: bool, shade: Shade) {
    let (fill, edge) = match shade {
        Shade::Light => (TRACK, TRACK_EDGE),
        Shade::Dark => (DARK_TRACK, DARK_TRACK_EDGE),
    };
    p.rect(r, fill);
    let (x, y, w, h) = (r.origin.x, r.origin.y, r.size.width, r.size.height);
    match horizontal {
        true => p.rect(rect(x, y, w, 1.0), edge),
        false => p.rect(rect(x, y, 1.0, h), edge),
    }
}

/// A scroll bar's capsule thumb, inset within `knob` - the span it occupies
/// along the track, the track's full breadth across.
pub(crate) fn scroll_thumb(
    p: &Painter,
    knob: NSRect,
    horizontal: bool,
    pressed: bool,
    shade: Shade,
) {
    let (x, y, w, h) = (
        knob.origin.x,
        knob.origin.y,
        knob.size.width,
        knob.size.height,
    );
    let girth = if horizontal { h } else { w };
    let pad = (girth * 0.2).round().max(2.0);
    let thumb = match horizontal {
        true => rect(x + 2.0, y + pad, w - 4.0, h - 2.0 * pad + 1.0),
        false => rect(x + pad, y + 2.0, w - 2.0 * pad + 1.0, h - 4.0),
    };
    let color = match (shade, pressed) {
        (Shade::Light, false) => THUMB,
        (Shade::Light, true) => [0.0, 0.0, 0.0, 0.55],
        (Shade::Dark, false) => LIGHT_THUMB,
        (Shade::Dark, true) => [1.0, 1.0, 1.0, 0.55],
    };
    let radius = (girth - 2.0 * pad + 1.0) / 2.0;
    p.fill_rounded(thumb, radius, color);
}

/// A determinate progress bar, filled to `fraction`; an indeterminate one is
/// drawn half filled, without its animation.
pub(crate) fn progress_bar(
    p: &Painter,
    r: NSRect,
    horizontal: bool,
    fraction: Option<f64>,
    size: Size,
) {
    let girth = match size {
        Size::Regular => 12.0,
        Size::Small | Size::Mini => 8.0,
    };
    let bar = match horizontal {
        true => centred_height(r, girth),
        false => rect(
            r.origin.x + ((r.size.width - girth) / 2.0).max(0.0),
            r.origin.y,
            girth.min(r.size.width),
            r.size.height,
        ),
    };
    let radius = girth / 2.0;
    p.gradient_rounded(bar, radius, p.palette.trough_top, p.palette.trough_bottom);
    p.stroke_rounded(bar, radius, 1.0, p.palette.edge);
    let fraction = fraction.unwrap_or(0.5).clamp(0.0, 1.0);
    if fraction > 0.0 {
        let filled = match horizontal {
            true => rect(
                bar.origin.x,
                bar.origin.y,
                bar.size.width * fraction,
                bar.size.height,
            ),
            false => {
                let h = bar.size.height * fraction;
                rect(
                    bar.origin.x,
                    bar.origin.y + bar.size.height - h,
                    bar.size.width,
                    h,
                )
            }
        };
        p.gradient_rounded(filled, radius, ACCENT_TOP, ACCENT_BOTTOM);
    }
}

/// Which side of a slider's bar its tick marks are on: above or leading, or
/// below or trailing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TickSide {
    Before,
    After,
}

/// Where a linear slider's bar, knob and tick marks lie within its frame, in
/// y-down coordinates. The minimum is at the left, or at the bottom when the
/// slider is vertical.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SliderLayout {
    pub frame: NSRect,
    pub horizontal: bool,
    pub size: Size,
    pub ticks: Option<TickSide>,
}

const SLIDER_BAR_END_INSET: f64 = 2.0;
const SLIDER_TICK_GAP: f64 = 2.0;
const SLIDER_TICK_WIDTH: f64 = 1.0;

// FIXME (ACCURACY): the knob, bar and tick sizes are Aqua's proportions, not
// measured from AppKit; with tick marks AppKit's knob is pointed, not round.
impl SliderLayout {
    fn knob_metric(&self) -> f64 {
        match self.size {
            Size::Regular => 18.0,
            Size::Small => 14.0,
            Size::Mini => 11.0,
        }
    }

    fn bar_thickness(&self) -> f64 {
        match self.size {
            Size::Regular => 4.0,
            Size::Small => 3.0,
            Size::Mini => 2.0,
        }
    }

    fn tick_length(&self) -> f64 {
        match self.size {
            Size::Regular => 4.0,
            Size::Small | Size::Mini => 3.0,
        }
    }

    /// The frame's extent along the bar, and across it.
    fn extents(&self) -> (f64, f64) {
        let NSSize { width, height } = self.frame.size;
        match self.horizontal {
            true => (width, height),
            false => (height, width),
        }
    }

    /// A rectangle given along and across the bar, from the frame's top left.
    fn place(&self, along: f64, across: f64, length: f64, breadth: f64) -> NSRect {
        let NSPoint { x, y } = self.frame.origin;
        match self.horizontal {
            true => rect(x + along, y + across, length, breadth),
            false => rect(x + across, y + along, breadth, length),
        }
    }

    /// Where the knob and bar sit across the frame: what is left beside the
    /// tick marks, as an offset and a breadth.
    fn band(&self) -> (f64, f64) {
        let (_, breadth) = self.extents();
        let reserve = match self.ticks {
            Some(_) => (SLIDER_TICK_GAP + self.tick_length()).min(breadth),
            None => 0.0,
        };
        match self.ticks {
            Some(TickSide::Before) => (reserve, breadth - reserve),
            _ => (0.0, breadth - reserve),
        }
    }

    /// The knob's side: its metric, or less in a frame too small for it.
    pub(crate) fn knob_side(&self) -> f64 {
        let (length, _) = self.extents();
        self.knob_metric().min(self.band().1).min(length).max(0.0)
    }

    /// How far along the frame the centre of a knob at `fraction` is.
    fn centre_along(&self, fraction: f64) -> f64 {
        let (length, _) = self.extents();
        let k = self.knob_side();
        let fraction = fraction.clamp(0.0, 1.0);
        let from_start = match self.horizontal {
            true => fraction,
            false => 1.0 - fraction,
        };
        k / 2.0 + from_start * (length - k)
    }

    pub(crate) fn knob(&self, fraction: f64) -> NSRect {
        let k = self.knob_side();
        let (offset, breadth) = self.band();
        let along = (self.centre_along(fraction) - k / 2.0).round();
        let across = (offset + (breadth - k) / 2.0).round();
        self.place(along, across, k, k)
    }

    pub(crate) fn bar(&self) -> NSRect {
        let (length, _) = self.extents();
        let (offset, breadth) = self.band();
        let t = self.bar_thickness().min(breadth);
        let across = (offset + (breadth - t) / 2.0).round();
        self.place(
            SLIDER_BAR_END_INSET,
            across,
            length - 2.0 * SLIDER_BAR_END_INSET,
            t,
        )
    }

    /// Tick mark `index` of `count`, the first at the minimum; `None` for a
    /// slider without tick marks.
    pub(crate) fn tick(&self, index: usize, count: usize) -> Option<NSRect> {
        let side = self.ticks?;
        let fraction = match count {
            0 | 1 => 0.5,
            n => index as f64 / (n - 1) as f64,
        };
        let length = self.tick_length();
        let (offset, breadth) = self.band();
        let across = match side {
            TickSide::Before => offset - SLIDER_TICK_GAP - length,
            TickSide::After => offset + breadth + SLIDER_TICK_GAP,
        };
        let along = (self.centre_along(fraction) - SLIDER_TICK_WIDTH / 2.0).floor();
        Some(self.place(along, across, SLIDER_TICK_WIDTH, length))
    }
}

/// A slider's bar, filled with the accent colour from the minimum to
/// `fraction` unless disabled.
pub(crate) fn slider_bar(
    p: &Painter,
    bar: NSRect,
    horizontal: bool,
    fraction: f64,
    disabled: bool,
) {
    let track = p.palette.slider_track;
    let radius = bar.size.width.min(bar.size.height) / 2.0;
    p.fill_rounded(bar, radius, track);
    if disabled {
        return;
    }
    let fraction = fraction.clamp(0.0, 1.0);
    let filled = match horizontal {
        true => rect(
            bar.origin.x,
            bar.origin.y,
            bar.size.width * fraction,
            bar.size.height,
        ),
        false => {
            let h = bar.size.height * fraction;
            rect(
                bar.origin.x,
                bar.origin.y + bar.size.height - h,
                bar.size.width,
                h,
            )
        }
    };
    p.fill_rounded(filled, radius, ACCENT_BOTTOM);
}

pub(crate) fn slider_knob(p: &Painter, knob: NSRect, look: Look) {
    p.face(
        knob,
        knob.size.width.min(knob.size.height) / 2.0,
        look,
        false,
    );
}

pub(crate) fn slider_tick(p: &Painter, tick: NSRect) {
    p.rect(tick, p.palette.tick);
}

/// A slider without tick marks: the bar and a round knob at `fraction`.
pub(crate) fn slider(
    p: &Painter,
    r: NSRect,
    horizontal: bool,
    size: Size,
    fraction: f64,
    look: Look,
) {
    let layout = SliderLayout {
        frame: r,
        horizontal,
        size,
        ticks: None,
    };
    slider_bar(p, layout.bar(), horizontal, fraction, look.disabled);
    slider_knob(p, layout.knob(fraction), look);
}

/// A group box or tab pane: a faintly shaded, rounded panel.
pub(crate) fn group_box(p: &Painter, r: NSRect) {
    let r = inset(r, 0.5, 0.5);
    p.fill_rounded(r, 4.0, p.palette.panel);
    p.stroke_rounded(r, 4.0, 1.0, p.palette.edge);
}

/// A rounded panel in a program's own colours: `path` filled, and stroked
/// along its outline with the border's width.
pub(crate) fn rounded_box(
    p: &Painter,
    path: NSRect,
    radius: f64,
    fill: Option<Rgba>,
    border: Option<(Rgba, f64)>,
) {
    if let Some(fill) = fill {
        p.fill_rounded(path, radius, fill);
    }
    if let Some((color, width)) = border {
        p.rounded(path, radius);
        p.stroke(width, color);
    }
}

/// A menu's background.
pub(crate) fn menu_background(p: &Painter, r: NSRect) {
    p.rect(r, p.palette.menu);
}

/// A menu item's background: the accent colour when selected.
pub(crate) fn menu_item(p: &Painter, r: NSRect, selected: bool) {
    if selected {
        p.rect(r, ACCENT_BOTTOM);
    }
}

/// A horizontal separator through the middle of `r`.
pub(crate) fn separator(p: &Painter, r: NSRect) {
    let y = (r.origin.y + r.size.height / 2.0).floor();
    p.rect(rect(r.origin.x, y, r.size.width, 1.0), p.palette.edge);
}

/// The side of the round dimple in a thick split view divider.
const DIMPLE: f64 = 6.0;

/// A thick split view divider's dimple, centred in `r`.
pub(crate) fn split_dimple(p: &Painter, r: NSRect) {
    let side = DIMPLE.min(r.size.width).min(r.size.height);
    let x = r.origin.x + (r.size.width - side) / 2.0;
    let y = r.origin.y + (r.size.height - side) / 2.0;
    p.ellipse(rect(x, y, side, side));
    p.fill(p.palette.mark_disabled);
}

/// A window's grow box: three diagonal ridges in its corner.
pub(crate) fn grow_box(p: &Painter, r: NSRect) {
    let (x1, y1) = (
        r.origin.x + r.size.width - 3.0,
        r.origin.y + r.size.height - 3.0,
    );
    for step in [3.0, 6.5, 10.0] {
        p.polyline(&[(x1 - step, y1), (x1, y1 - step)], false);
    }
    p.stroke(1.0, p.palette.border);
}

/// A plain fill of `color`, for a theme brush.
pub(crate) fn fill(p: &Painter, r: NSRect, color: Rgba) {
    p.rect(r, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thumb_spans_the_visible_share_of_its_track() {
        assert_eq!(thumb_span(100.0, 0, 100, 0, 100), Some((0.0, 50.0)));
        assert_eq!(thumb_span(100.0, 0, 100, 100, 100), Some((50.0, 50.0)));
    }

    #[test]
    fn a_thumb_never_shrinks_below_its_minimum_or_outgrows_its_track() {
        assert_eq!(thumb_span(100.0, 0, 100_000, 0, 10), Some((0.0, 18.0)));
        assert_eq!(thumb_span(10.0, 0, 5, 0, 1000), Some((0.0, 10.0)));
    }

    fn horizontal(width: f64, height: f64, ticks: Option<TickSide>) -> SliderLayout {
        SliderLayout {
            frame: rect(0.0, 0.0, width, height),
            horizontal: true,
            size: Size::Regular,
            ticks,
        }
    }

    #[test]
    fn a_slider_knob_travels_the_frame_between_its_ends() {
        let layout = horizontal(100.0, 20.0, None);
        assert_eq!(layout.knob(0.0), rect(0.0, 1.0, 18.0, 18.0));
        assert_eq!(layout.knob(1.0), rect(82.0, 1.0, 18.0, 18.0));
        assert_eq!(layout.knob(0.5), rect(41.0, 1.0, 18.0, 18.0));
    }

    #[test]
    fn a_vertical_slider_has_its_minimum_at_the_bottom() {
        let layout = SliderLayout {
            frame: rect(0.0, 0.0, 20.0, 100.0),
            horizontal: false,
            size: Size::Regular,
            ticks: None,
        };
        assert_eq!(layout.knob(0.0), rect(1.0, 82.0, 18.0, 18.0));
        assert_eq!(layout.knob(1.0), rect(1.0, 0.0, 18.0, 18.0));
    }

    #[test]
    fn slider_tick_marks_sit_beside_the_bar_under_the_knob_centres() {
        let layout = horizontal(100.0, 26.0, Some(TickSide::After));
        let bar = layout.bar();
        let first = layout.tick(0, 3).expect("a tick mark");
        let last = layout.tick(2, 3).expect("a tick mark");
        assert!(first.origin.y >= bar.origin.y + bar.size.height);
        assert_eq!(first.origin.x, 8.0);
        assert_eq!(last.origin.x, 90.0);
        assert_eq!(horizontal(100.0, 26.0, None).tick(0, 3), None);
    }

    #[test]
    fn nothing_to_scroll_has_no_thumb() {
        assert_eq!(thumb_span(100.0, 0, 0, 0, 100), None);
    }
}
