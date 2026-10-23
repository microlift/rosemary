use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;

use foundation::geometry::{NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::backing::Device;
use crate::define::{class_methods, methods};
use crate::transform::Matrix;

/// One entry of the graphics-state stack. The top entry for the focused view
/// is the live state; the ones under it are what `-saveGraphicsState` kept.
#[derive(Clone)]
struct State {
    /// The focus this state belongs to; see [`Device`].
    key: usize,
    depth: usize,
    ctm: Matrix,
    /// The colours when this state was saved over; the live ones are
    /// `crate::color`'s.
    colors: ([f64; 4], [f64; 4]),
    /// Coverage of every pixel of the focused view, row by row, or `None` for
    /// no clip.
    clip: Option<Rc<Vec<f32>>>,
}

thread_local! {
    static STATES: RefCell<Vec<State>> = const { RefCell::new(Vec::new()) };
}

/// Run `body` on the live state of the focused view, making a fresh one when
/// the view has none. A state left by a view whose drawing has finished is
/// dropped on the way: each `-drawRect:` starts from the identity and no clip,
/// as each does on a Mac.
fn with_state<R>(body: impl FnOnce(&mut Vec<State>, Option<Device>) -> R) -> R {
    let device = crate::backing::focused_device();
    let (key, depth) = device.map_or((0, 0), |d| (d.key, d.depth));
    STATES.with(|states| {
        let mut states = states.borrow_mut();
        while let Some(top) = states.last() {
            let stale = top.depth > depth || (top.depth == depth && top.key != key);
            if !stale {
                break;
            }
            states.pop();
        }
        if states
            .last()
            .is_none_or(|top| top.key != key || top.depth != depth)
        {
            states.push(State {
                key,
                depth,
                ctm: Matrix::IDENTITY,
                colors: crate::color::current(),
                clip: None,
            });
        }
        body(&mut states, device)
    })
}

/// Replace the current transform with `update` of it.
pub(crate) fn update_ctm(update: impl FnOnce(&Matrix) -> Matrix) {
    with_state(|states, _| {
        let top = states.last_mut().expect("with_state leaves a state");
        top.ctm = update(&top.ctm);
    });
}

fn current() -> (Matrix, Option<Rc<Vec<f32>>>, Option<Device>) {
    with_state(|states, device| {
        let top = states.last().expect("with_state leaves a state");
        (top.ctm, top.clip.clone(), device)
    })
}

unsafe extern "C" fn save_graphics_state(_receiver: Id, _sel: Sel) {
    with_state(|states, _| {
        let top = states.last_mut().expect("with_state leaves a state");
        top.colors = crate::color::current();
        let copy = top.clone();
        states.push(copy);
    });
}

unsafe extern "C" fn restore_graphics_state(_receiver: Id, _sel: Sel) {
    with_state(|states, _| {
        let n = states.len();
        // Only a state this focus saved: restoring past the start of a
        // `-drawRect:` would hand it the enclosing view's state.
        if n < 2
            || states[n - 2].key != states[n - 1].key
            || states[n - 2].depth != states[n - 1].depth
        {
            return;
        }
        states.pop();
        let (fill, stroke) = states[n - 2].colors;
        crate::color::restore(fill, stroke);
    });
}

/// Vertical samples per pixel row. Horizontal coverage is exact.
const SUBSAMPLES: usize = 4;

/// A coverage mask over part of the device: `width` columns from `left`,
/// rows from `top`.
#[derive(Debug, PartialEq)]
struct Mask {
    left: i64,
    top: i64,
    width: usize,
    coverage: Vec<f32>,
}

/// The pixels of a `width` by `height` device that `polygons` (each closed
/// implicitly) cover, by the nonzero or even-odd rule.
fn rasterize(polygons: &[Vec<[f64; 2]>], even_odd: bool, width: u32, height: u32) -> Option<Mask> {
    let points = polygons.iter().flatten();
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for &[x, y] in points {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    let x0 = min_x.floor().max(0.0) as i64;
    let y0 = min_y.floor().max(0.0) as i64;
    let x1 = (max_x.ceil() as i64).min(i64::from(width));
    let y1 = (max_y.ceil() as i64).min(i64::from(height));
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let columns = (x1 - x0) as usize;
    let mut coverage = vec![0f32; columns * (y1 - y0) as usize];

    // Every edge that is not horizontal, as (top, bottom, x at top, dx/dy,
    // winding direction).
    let mut edges = Vec::new();
    for polygon in polygons {
        for (i, &a) in polygon.iter().enumerate() {
            let b = polygon[(i + 1) % polygon.len()];
            if a[1] == b[1] {
                continue;
            }
            let (top, bottom, direction) = match a[1] < b[1] {
                true => (a, b, 1),
                false => (b, a, -1),
            };
            let slope = (bottom[0] - top[0]) / (bottom[1] - top[1]);
            edges.push((top[1], bottom[1], top[0], slope, direction));
        }
    }

    let weight = 1.0 / SUBSAMPLES as f32;
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    for y in y0..y1 {
        let row = &mut coverage[(y - y0) as usize * columns..][..columns];
        for sample in 0..SUBSAMPLES {
            let sy = y as f64 + (sample as f64 + 0.5) / SUBSAMPLES as f64;
            crossings.clear();
            for &(top, bottom, x, slope, direction) in &edges {
                if sy >= top && sy < bottom {
                    crossings.push((x + (sy - top) * slope, direction));
                }
            }
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut winding = 0;
            for pair in crossings.windows(2) {
                winding += pair[0].1;
                let inside = match even_odd {
                    true => winding % 2 != 0,
                    false => winding != 0,
                };
                if inside {
                    add_span(row, pair[0].0 - x0 as f64, pair[1].0 - x0 as f64, weight);
                }
            }
        }
    }
    Some(Mask {
        left: x0,
        top: y0,
        width: columns,
        coverage,
    })
}

/// Add `weight` times how much of each pixel of `row` lies in `[from, to)`.
fn add_span(row: &mut [f32], from: f64, to: f64, weight: f32) {
    let from = from.max(0.0);
    let to = to.min(row.len() as f64);
    if from >= to {
        return;
    }
    let first = from.floor() as usize;
    let last = (to.ceil() as usize).min(row.len());
    for (x, pixel) in row.iter_mut().enumerate().take(last).skip(first) {
        let overlap = (to.min(x as f64 + 1.0) - from.max(x as f64)).max(0.0);
        *pixel += overlap as f32 * weight;
    }
}

/// Paint `polygons`, in the view's coordinates, in `color` through the
/// current transform and clip.
fn paint(polygons: Vec<Vec<[f64; 2]>>, even_odd: bool, color: [f64; 4]) {
    let (ctm, clip, Some(device)) = current() else {
        return;
    };
    let polygons: Vec<Vec<[f64; 2]>> = polygons
        .into_iter()
        .map(|polygon| {
            polygon
                .into_iter()
                .map(|point| {
                    let [x, y] = ctm.apply(point);
                    device.to_pixels(x, y)
                })
                .collect()
        })
        .collect();
    let Some(mut mask) = rasterize(&polygons, even_odd, device.width, device.height) else {
        return;
    };
    if let Some(clip) = clip {
        apply_clip(&mut mask, &clip, device.width as usize);
    }
    crate::backing::paint_coverage(mask.left, mask.top, mask.width, &mask.coverage, color);
}

fn apply_clip(mask: &mut Mask, clip: &[f32], device_width: usize) {
    for (at, value) in mask.coverage.iter_mut().enumerate() {
        let x = mask.left as usize + at % mask.width;
        let y = mask.top as usize + at / mask.width;
        *value *= clip.get(y * device_width + x).copied().unwrap_or(0.0);
    }
}

/// Intersect the clip with `polygons`, in the view's coordinates.
fn clip_to(polygons: Vec<Vec<[f64; 2]>>, even_odd: bool) {
    with_state(|states, device| {
        let Some(device) = device else {
            return;
        };
        let top = states.last_mut().expect("with_state leaves a state");
        let ctm = top.ctm;
        let polygons: Vec<Vec<[f64; 2]>> = polygons
            .into_iter()
            .map(|polygon| {
                polygon
                    .into_iter()
                    .map(|point| {
                        let [x, y] = ctm.apply(point);
                        device.to_pixels(x, y)
                    })
                    .collect()
            })
            .collect();
        let (width, height) = (device.width as usize, device.height as usize);
        let mut clip = vec![0f32; width * height];
        if let Some(mask) = rasterize(&polygons, even_odd, device.width, device.height) {
            for (at, &value) in mask.coverage.iter().enumerate() {
                let x = mask.left as usize + at % mask.width;
                let y = mask.top as usize + at / mask.width;
                clip[y * width + x] = value.min(1.0);
            }
        }
        if let Some(previous) = &top.clip {
            for (value, old) in clip.iter_mut().zip(previous.iter()) {
                *value *= old;
            }
        }
        top.clip = Some(Rc::new(clip));
    });
}

/// `NSRectFill` under a transform or a clip, which the plain blit it is
/// otherwise cannot honour. False when there is neither, for the blit.
pub(crate) fn fill_rect_through_state(rect: NSRect, color: [f64; 4]) -> bool {
    let (ctm, clip, device) = current();
    if device.is_none() || (ctm == Matrix::IDENTITY && clip.is_none()) {
        return false;
    }
    paint(vec![rect_points(rect)], false, color);
    true
}

/// `NSRectClip`: intersect the clip with `rect`.
pub(crate) fn clip_rect(rect: NSRect) {
    clip_to(vec![rect_points(rect)], false);
}

/// Where `rect` lands in the view under the current transform, for drawing
/// that can only place axis-aligned images.
///
/// FIXME (ACCURACY): a rotated or sheared transform places the image at the
/// bounding box of where it would land, unrotated, and the clip is not
/// applied to images.
pub(crate) fn transform_rect(rect: NSRect) -> NSRect {
    let (ctm, _, _) = current();
    if ctm == Matrix::IDENTITY {
        return rect;
    }
    let corners = rect_points(rect).into_iter().map(|p| ctm.apply(p));
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for [x, y] in corners {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    NSRect {
        origin: NSPoint { x: x0, y: y0 },
        size: NSSize {
            width: x1 - x0,
            height: y1 - y0,
        },
    }
}

/// Paths are drawn with miter joins and butt caps, NSBezierPath's defaults,
/// and the miter limit it starts with.
const MITER_LIMIT: f64 = 10.0;

/// The polygons that cover a stroke `width` wide along `subpath`, each wound
/// the same way so the nonzero rule paints their union.
fn stroke_polygons(points: &[[f64; 2]], closed: bool, width: f64) -> Vec<Vec<[f64; 2]>> {
    let mut points: Vec<[f64; 2]> = points.to_vec();
    points.dedup();
    if closed && points.len() > 1 && points.first() == points.last() {
        points.pop();
    }
    let mut pieces = Vec::new();
    if points.len() < 2 {
        return pieces;
    }
    let half = width / 2.0;
    let segments: Vec<([f64; 2], [f64; 2])> = match closed {
        true => (0..points.len())
            .map(|i| (points[i], points[(i + 1) % points.len()]))
            .collect(),
        false => points.windows(2).map(|w| (w[0], w[1])).collect(),
    };
    let unit = |(a, b): ([f64; 2], [f64; 2])| {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let length = dx.hypot(dy);
        [dx / length, dy / length]
    };
    for &(a, b) in &segments {
        let [dx, dy] = unit((a, b));
        let n = [-dy * half, dx * half];
        pieces.push(vec![
            [a[0] + n[0], a[1] + n[1]],
            [b[0] + n[0], b[1] + n[1]],
            [b[0] - n[0], b[1] - n[1]],
            [a[0] - n[0], a[1] - n[1]],
        ]);
    }
    let joins = match closed {
        true => segments.len(),
        false => segments.len() - 1,
    };
    for i in 0..joins {
        let incoming = segments[i];
        let outgoing = segments[(i + 1) % segments.len()];
        let v = incoming.1;
        let d1 = unit(incoming);
        let d2 = unit(outgoing);
        let cross = d1[0] * d2[1] - d1[1] * d2[0];
        if cross.abs() < 1e-9 {
            continue;
        }
        // The outside of the turn, where the two segments leave a notch.
        let side = if cross > 0.0 { -1.0 } else { 1.0 };
        let p1 = [v[0] - d1[1] * half * side, v[1] + d1[0] * half * side];
        let p2 = [v[0] - d2[1] * half * side, v[1] + d2[0] * half * side];
        let dot = (d1[0] * d2[0] + d1[1] * d2[1]).clamp(-1.0, 1.0);
        let half_turn_cos = ((1.0 + dot) / 2.0).sqrt();
        let mut join = vec![v, p1];
        if half_turn_cos > 0.0 && 1.0 / half_turn_cos <= MITER_LIMIT {
            let bisector = [p1[0] + p2[0] - 2.0 * v[0], p1[1] + p2[1] - 2.0 * v[1]];
            let length = bisector[0].hypot(bisector[1]);
            let reach = half / half_turn_cos;
            join.push([
                v[0] + bisector[0] / length * reach,
                v[1] + bisector[1] / length * reach,
            ]);
        }
        join.push(p2);
        pieces.push(join);
    }
    for piece in &mut pieces {
        if signed_area(piece) < 0.0 {
            piece.reverse();
        }
    }
    pieces
}

fn signed_area(polygon: &[[f64; 2]]) -> f64 {
    let mut sum = 0.0;
    for (i, a) in polygon.iter().enumerate() {
        let b = polygon[(i + 1) % polygon.len()];
        sum += a[0] * b[1] - b[0] * a[1];
    }
    sum / 2.0
}

fn rect_points(rect: NSRect) -> Vec<[f64; 2]> {
    let (x, y) = (rect.origin.x, rect.origin.y);
    let (w, h) = (rect.size.width, rect.size.height);
    vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]]
}

#[derive(Clone)]
struct Subpath {
    points: Vec<[f64; 2]>,
    closed: bool,
}

#[derive(Clone)]
struct PathData {
    subpaths: Vec<Subpath>,
    line_width: f64,
    even_odd: bool,
}

impl Default for PathData {
    fn default() -> Self {
        PathData {
            subpaths: Vec::new(),
            line_width: 1.0,
            even_odd: false,
        }
    }
}

impl PathData {
    fn current_point(&self) -> Option<[f64; 2]> {
        self.subpaths.last()?.points.last().copied()
    }

    fn move_to(&mut self, point: [f64; 2]) {
        self.subpaths.push(Subpath {
            points: vec![point],
            closed: false,
        });
    }

    fn line_to(&mut self, point: [f64; 2]) {
        match self.subpaths.last_mut() {
            Some(subpath) if !subpath.closed => subpath.points.push(point),
            // A line with no open subpath starts one where the last ended, as
            // Apple's does after `-closePath`.
            _ => {
                let from = self.current_point().unwrap_or(point);
                self.move_to(from);
                self.line_to(point);
            }
        }
    }

    fn curve_to(&mut self, to: [f64; 2], c1: [f64; 2], c2: [f64; 2]) {
        let Some(from) = self.current_point() else {
            return;
        };
        const STEPS: usize = 16;
        for step in 1..=STEPS {
            let t = step as f64 / STEPS as f64;
            let u = 1.0 - t;
            let point = [0, 1].map(|axis| {
                u * u * u * from[axis]
                    + 3.0 * u * u * t * c1[axis]
                    + 3.0 * u * t * t * c2[axis]
                    + t * t * t * to[axis]
            });
            self.line_to(point);
        }
    }

    fn close(&mut self) {
        if let Some(subpath) = self.subpaths.last_mut() {
            subpath.closed = true;
        }
    }

    fn append_rect(&mut self, rect: NSRect) {
        let mut points = rect_points(rect).into_iter();
        self.move_to(points.next().expect("four corners"));
        for point in points {
            self.line_to(point);
        }
        self.close();
    }

    fn append_oval(&mut self, rect: NSRect) {
        let (cx, cy) = (
            rect.origin.x + rect.size.width / 2.0,
            rect.origin.y + rect.size.height / 2.0,
        );
        let (rx, ry) = (rect.size.width / 2.0, rect.size.height / 2.0);
        const STEPS: usize = 64;
        for step in 0..STEPS {
            let angle = std::f64::consts::TAU * step as f64 / STEPS as f64;
            let point = [cx + rx * angle.cos(), cy + ry * angle.sin()];
            match step {
                0 => self.move_to(point),
                _ => self.line_to(point),
            }
        }
        self.close();
    }

    fn fill_polygons(&self) -> Vec<Vec<[f64; 2]>> {
        self.subpaths
            .iter()
            .filter(|s| s.points.len() > 2)
            .map(|s| s.points.clone())
            .collect()
    }

    fn stroke_polygons(&self) -> Vec<Vec<[f64; 2]>> {
        let (ctm, _, device) = current();
        // Zero is the thinnest line the device can draw: one pixel.
        let width = match self.line_width > 0.0 {
            true => self.line_width,
            false => {
                let pixels = device.map_or(1.0, |d| d.density) * ctm.scale_factor();
                1.0 / pixels.max(f64::MIN_POSITIVE)
            }
        };
        self.subpaths
            .iter()
            .flat_map(|s| stroke_polygons(&s.points, s.closed, width))
            .collect()
    }

    fn bounds(&self) -> NSRect {
        let mut points = self.subpaths.iter().flat_map(|s| s.points.iter());
        let Some(&[x, y]) = points.next() else {
            return NSRect::default();
        };
        let (mut x0, mut y0, mut x1, mut y1) = (x, y, x, y);
        for &[x, y] in points {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        NSRect {
            origin: NSPoint { x: x0, y: y0 },
            size: NSSize {
                width: x1 - x0,
                height: y1 - y0,
            },
        }
    }
}

#[repr(C)]
struct Path {
    isa: *const c_void,
    data: *mut PathData,
}

pub const INSTANCE_SIZE: usize = size_of::<Path>();

/// The path's contents, made on first use: `+alloc` leaves the field zeroed
/// and `-init` is inherited from `NSObject`.
fn data<'a>(receiver: Id) -> &'a mut PathData {
    // SAFETY: an `NSBezierPath`, which this module laid out.
    let path = unsafe { &mut *(receiver as *mut Path) };
    if path.data.is_null() {
        path.data = Box::into_raw(Box::default());
    }
    // SAFETY: from `Box::into_raw`, owned by the path until `-dealloc`.
    unsafe { &mut *path.data }
}

fn new_path(contents: PathData) -> Id {
    let class = objc::class::lookup(c"NSBezierPath").expect("declared");
    let object = objc::object::create_instance(class, 0);
    *data(object) = contents;
    // SAFETY: a fresh convenience-constructor result.
    unsafe { corefoundation::api::CFAutorelease(object as corefoundation::base::CFTypeRef) as Id }
}

fn point(p: NSPoint) -> [f64; 2] {
    [p.x, p.y]
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: an `NSBezierPath`, whose data came from `Box::into_raw`.
    let path = unsafe { &mut *(receiver as *mut Path) };
    if !path.data.is_null() {
        drop(unsafe { Box::from_raw(path.data) });
        path.data = std::ptr::null_mut();
    }
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn bezier_path(_class: Id, _sel: Sel) -> Id {
    new_path(PathData::default())
}

unsafe extern "C" fn bezier_path_with_rect(_class: Id, _sel: Sel, rect: NSRect) -> Id {
    let mut contents = PathData::default();
    contents.append_rect(rect);
    new_path(contents)
}

unsafe extern "C" fn bezier_path_with_oval(_class: Id, _sel: Sel, rect: NSRect) -> Id {
    let mut contents = PathData::default();
    contents.append_oval(rect);
    new_path(contents)
}

unsafe extern "C" fn fill_rect(_class: Id, _sel: Sel, rect: NSRect) {
    let (fill, _) = crate::color::current();
    paint(vec![rect_points(rect)], false, fill);
}

unsafe extern "C" fn stroke_rect(_class: Id, _sel: Sel, rect: NSRect) {
    let mut contents = PathData::default();
    contents.append_rect(rect);
    let (_, stroke) = crate::color::current();
    paint(contents.stroke_polygons(), false, stroke);
}

unsafe extern "C" fn stroke_line(_class: Id, _sel: Sel, from: NSPoint, to: NSPoint) {
    let mut contents = PathData::default();
    contents.move_to(point(from));
    contents.line_to(point(to));
    let (_, stroke) = crate::color::current();
    paint(contents.stroke_polygons(), false, stroke);
}

unsafe extern "C" fn copy_with_zone(receiver: Id, _sel: Sel, _zone: *const c_void) -> Id {
    let class = objc::class::lookup(c"NSBezierPath").expect("declared");
    let object = objc::object::create_instance(class, 0);
    *data(object) = data(receiver).clone();
    object
}

unsafe extern "C" fn move_to_point(receiver: Id, _sel: Sel, to: NSPoint) {
    data(receiver).move_to(point(to));
}

unsafe extern "C" fn line_to_point(receiver: Id, _sel: Sel, to: NSPoint) {
    data(receiver).line_to(point(to));
}

unsafe extern "C" fn curve_to_point(
    receiver: Id,
    _sel: Sel,
    to: NSPoint,
    control1: NSPoint,
    control2: NSPoint,
) {
    data(receiver).curve_to(point(to), point(control1), point(control2));
}

unsafe extern "C" fn relative_move_to_point(receiver: Id, _sel: Sel, by: NSPoint) {
    let path = data(receiver);
    let [x, y] = path.current_point().unwrap_or([0.0, 0.0]);
    path.move_to([x + by.x, y + by.y]);
}

unsafe extern "C" fn relative_line_to_point(receiver: Id, _sel: Sel, by: NSPoint) {
    let path = data(receiver);
    let [x, y] = path.current_point().unwrap_or([0.0, 0.0]);
    path.line_to([x + by.x, y + by.y]);
}

unsafe extern "C" fn close_path(receiver: Id, _sel: Sel) {
    data(receiver).close();
}

unsafe extern "C" fn append_rect(receiver: Id, _sel: Sel, rect: NSRect) {
    data(receiver).append_rect(rect);
}

unsafe extern "C" fn append_oval(receiver: Id, _sel: Sel, rect: NSRect) {
    data(receiver).append_oval(rect);
}

unsafe extern "C" fn append_path(receiver: Id, _sel: Sel, other: Id) {
    if other.is_null() {
        return;
    }
    let subpaths = data(other).subpaths.clone();
    data(receiver).subpaths.extend(subpaths);
}

unsafe extern "C" fn remove_all_points(receiver: Id, _sel: Sel) {
    data(receiver).subpaths.clear();
}

unsafe extern "C" fn is_empty(receiver: Id, _sel: Sel) -> u8 {
    u8::from(data(receiver).subpaths.is_empty())
}

unsafe extern "C" fn current_point(receiver: Id, _sel: Sel) -> NSPoint {
    let [x, y] = data(receiver).current_point().unwrap_or([0.0, 0.0]);
    NSPoint { x, y }
}

unsafe extern "C" fn bounds(receiver: Id, _sel: Sel) -> NSRect {
    data(receiver).bounds()
}

unsafe extern "C" fn line_width(receiver: Id, _sel: Sel) -> f64 {
    data(receiver).line_width
}

unsafe extern "C" fn set_line_width(receiver: Id, _sel: Sel, width: f64) {
    data(receiver).line_width = width;
}

/// `NSWindingRule`: 0 nonzero, 1 even-odd.
unsafe extern "C" fn winding_rule(receiver: Id, _sel: Sel) -> usize {
    usize::from(data(receiver).even_odd)
}

unsafe extern "C" fn set_winding_rule(receiver: Id, _sel: Sel, rule: usize) {
    data(receiver).even_odd = rule == 1;
}

unsafe extern "C" fn transform_using(receiver: Id, _sel: Sel, transform: Id) {
    if transform.is_null() {
        return;
    }
    let matrix = crate::transform::matrix(transform);
    for subpath in &mut data(receiver).subpaths {
        for p in &mut subpath.points {
            *p = matrix.apply(*p);
        }
    }
}

unsafe extern "C" fn fill(receiver: Id, _sel: Sel) {
    let path = data(receiver);
    let (fill, _) = crate::color::current();
    paint(path.fill_polygons(), path.even_odd, fill);
}

unsafe extern "C" fn stroke(receiver: Id, _sel: Sel) {
    let (_, stroke) = crate::color::current();
    paint(data(receiver).stroke_polygons(), false, stroke);
}

unsafe extern "C" fn add_clip(receiver: Id, _sel: Sel) {
    let path = data(receiver);
    clip_to(path.fill_polygons(), path.even_odd);
}

unsafe extern "C" fn set_clip(receiver: Id, _sel: Sel) {
    with_state(|states, _| {
        states.last_mut().expect("with_state leaves a state").clip = None;
    });
    unsafe { add_clip(receiver, objc::sel::get("addClip")) };
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSBezierPath").expect("declared before its methods");
    class_methods!(class, {
        "bezierPath" => bezier_path, c"@16@0:8";
        "bezierPathWithRect:" => bezier_path_with_rect, c"@48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "bezierPathWithOvalInRect:" => bezier_path_with_oval, c"@48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "fillRect:" => fill_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "strokeRect:" => stroke_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "strokeLineFromPoint:toPoint:" => stroke_line, c"v48@0:8{CGPoint=dd}16{CGPoint=dd}32";
    });
    methods!(class, {
        "dealloc" => dealloc, c"v16@0:8";
        "copyWithZone:" => copy_with_zone, c"@24@0:8^v16";
        "moveToPoint:" => move_to_point, c"v32@0:8{CGPoint=dd}16";
        "lineToPoint:" => line_to_point, c"v32@0:8{CGPoint=dd}16";
        "curveToPoint:controlPoint1:controlPoint2:" => curve_to_point, c"v64@0:8{CGPoint=dd}16{CGPoint=dd}32{CGPoint=dd}48";
        "relativeMoveToPoint:" => relative_move_to_point, c"v32@0:8{CGPoint=dd}16";
        "relativeLineToPoint:" => relative_line_to_point, c"v32@0:8{CGPoint=dd}16";
        "closePath" => close_path, c"v16@0:8";
        "appendBezierPathWithRect:" => append_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "appendBezierPathWithOvalInRect:" => append_oval, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "appendBezierPath:" => append_path, c"v24@0:8@16";
        "removeAllPoints" => remove_all_points, c"v16@0:8";
        "isEmpty" => is_empty, c"c16@0:8";
        "currentPoint" => current_point, c"{CGPoint=dd}16@0:8";
        "bounds" => bounds, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "lineWidth" => line_width, c"d16@0:8";
        "setLineWidth:" => set_line_width, c"v24@0:8d16";
        "windingRule" => winding_rule, c"Q16@0:8";
        "setWindingRule:" => set_winding_rule, c"v24@0:8Q16";
        "transformUsingAffineTransform:" => transform_using, c"v24@0:8@16";
        "fill" => fill, c"v16@0:8";
        "stroke" => stroke, c"v16@0:8";
        "addClip" => add_clip, c"v16@0:8";
        "setClip" => set_clip, c"v16@0:8";
    });

    let context = objc::class::lookup(c"NSGraphicsContext").expect("declared");
    class_methods!(context, {
        "saveGraphicsState" => save_graphics_state, c"v16@0:8";
        "restoreGraphicsState" => restore_graphics_state, c"v16@0:8";
    });
    methods!(context, {
        "saveGraphicsState" => save_graphics_state, c"v16@0:8";
        "restoreGraphicsState" => restore_graphics_state, c"v16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasterizes_a_pixel_aligned_square_exactly() {
        let square = vec![vec![[1.0, 1.0], [3.0, 1.0], [3.0, 3.0], [1.0, 3.0]]];
        let mask = rasterize(&square, false, 8, 8).unwrap();
        assert_eq!((mask.left, mask.top, mask.width), (1, 1, 2));
        assert!(mask.coverage.iter().all(|&c| (c - 1.0).abs() < 1e-6));
    }

    #[test]
    fn half_covered_pixels_get_half_coverage() {
        let strip = vec![vec![[0.5, 0.0], [1.5, 0.0], [1.5, 1.0], [0.5, 1.0]]];
        let mask = rasterize(&strip, false, 4, 4).unwrap();
        assert_eq!(mask.width, 2);
        assert!(mask.coverage.iter().all(|&c| (c - 0.5).abs() < 1e-6));
    }

    #[test]
    fn even_odd_leaves_a_hole_and_nonzero_does_not() {
        let outer = vec![[0.0, 0.0], [6.0, 0.0], [6.0, 6.0], [0.0, 6.0]];
        let inner = vec![[2.0, 2.0], [4.0, 2.0], [4.0, 4.0], [2.0, 4.0]];
        let shapes = vec![outer, inner];
        let centre = |mask: &Mask| mask.coverage[3 * mask.width + 3];
        assert_eq!(centre(&rasterize(&shapes, true, 8, 8).unwrap()), 0.0);
        assert_eq!(centre(&rasterize(&shapes, false, 8, 8).unwrap()), 1.0);
    }

    #[test]
    fn a_stroked_corner_is_filled_by_its_miter() {
        // An L, two units wide: the outside corner at (-1, -1) is only
        // covered by the join.
        let pieces = stroke_polygons(&[[0.0, 4.0], [0.0, 0.0], [4.0, 0.0]], false, 2.0);
        assert!(pieces.iter().all(|p| signed_area(p) >= 0.0));
        let shifted: Vec<Vec<[f64; 2]>> = pieces
            .iter()
            .map(|p| p.iter().map(|&[x, y]| [x + 2.0, y + 2.0]).collect())
            .collect();
        let mask = rasterize(&shifted, false, 8, 8).unwrap();
        let at = |x: i64, y: i64| {
            mask.coverage[((y - mask.top) as usize) * mask.width + (x - mask.left) as usize]
        };
        assert_eq!(at(1, 1), 1.0, "the miter fills the outside corner");
        assert_eq!(at(2, 4), 1.0, "the vertical arm");
        assert_eq!(at(4, 2), 1.0, "the horizontal arm");
    }
}
