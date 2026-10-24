use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

use foundation::geometry::{NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

pub struct Store {
    pub width: u32,
    pub height: u32,
    /// Premultiplied B, G, R, A, top row first.
    pub pixels: Vec<u8>,
    dirty: bool,
    /// Bumped by every change to what the store shows, so that a presenter
    /// can tell whether a copy it took is still current without comparing it.
    generation: u64,
}

impl Store {
    #[cfg(test)]
    pub(crate) fn for_tests(width: u32, height: u32) -> Self {
        Store::new(width, height)
    }

    /// Allocate an initially clean premultiplied-BGRA backing store.
    pub fn new(width: u32, height: u32) -> Self {
        Store {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
            dirty: false,
            generation: 0,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// What the store shows changed, and has to be presented again.
    fn touch(&mut self) {
        self.dirty = true;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Change the size, keeping what the top-left corner held.
    fn resize(&mut self, width: u32, height: u32) {
        if (self.width, self.height) == (width, height) {
            return;
        }
        let mut pixels = vec![0; width as usize * height as usize * 4];
        let columns = self.width.min(width) as usize;
        for row in 0..self.height.min(height) as usize {
            let from = row * self.width as usize * 4;
            let to = row * width as usize * 4;
            pixels[to..to + columns * 4].copy_from_slice(&self.pixels[from..from + columns * 4]);
        }
        *self = Store {
            width,
            height,
            pixels,
            dirty: true,
            generation: self.generation.wrapping_add(1),
        };
    }
}

/// A rectangle of pixels, top-left origin; empty when either extent is not
/// positive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

impl Rect {
    pub(crate) fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    pub(crate) fn intersection(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        Rect {
            x,
            y,
            width: (right - x).max(0),
            height: (bottom - y).max(0),
        }
    }

    /// `self` less every rectangle in `cuts`, as rectangles that do not
    /// overlap.
    fn subtract(&self, cuts: &[Rect]) -> Vec<Rect> {
        let mut pieces = vec![*self];
        for cut in cuts {
            let mut next = Vec::with_capacity(pieces.len() + 4);
            for piece in pieces {
                let inside = piece.intersection(cut);
                if inside.is_empty() {
                    next.push(piece);
                    continue;
                }
                let (right, bottom) = (inside.x + inside.width, inside.y + inside.height);
                let bands = [
                    (piece.x, piece.y, piece.width, inside.y - piece.y),
                    (
                        piece.x,
                        bottom,
                        piece.width,
                        piece.y + piece.height - bottom,
                    ),
                    (piece.x, inside.y, inside.x - piece.x, inside.height),
                    (
                        right,
                        inside.y,
                        piece.x + piece.width - right,
                        inside.height,
                    ),
                ];
                next.extend(
                    bands
                        .into_iter()
                        .map(|(x, y, width, height)| Rect {
                            x,
                            y,
                            width,
                            height,
                        })
                        .filter(|band| !band.is_empty()),
                );
            }
            pieces = next;
        }
        pieces
    }

    fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect {
            x,
            y,
            width: (self.x + self.width).max(other.x + other.width) - x,
            height: (self.y + self.height).max(other.y + other.height) - y,
        }
    }
}

/// A view's layer as the window's store leaves room for it: the presenter
/// keeps what the layer last presented there instead of the store's pixels.
#[derive(Clone, Copy, Debug)]
pub struct Hole {
    pub layer: usize,
    pub rect: Rect,
}

/// Each window's composite, keyed by the SDL window's address.
static STORES: Mutex<Option<HashMap<usize, Store>>> = Mutex::new(None);

/// Each drawn view's own pixels, keyed by the view's address.
static VIEW_STORES: Mutex<Option<HashMap<usize, Store>>> = Mutex::new(None);

/// What a window's composite is waiting on: the part of it views have drawn
/// over since it was last composed, and where each view was then, so that a
/// view which moved, hid or went away is recomposed without drawing.
#[derive(Default)]
struct WindowState {
    damage: Rect,
    /// Each view's `(view, clip, rect)`: the rectangle too, since a scrolled
    /// document moves under a clip that stays where it is.
    placed: Vec<(usize, Rect, Rect)>,
    holes: Vec<Hole>,
    /// For each layer with an overlay stacked above it, the overlays'
    /// rectangles, which its own presents must leave to the store.
    covered: Vec<(usize, Vec<Rect>)>,
    /// The generation of `crate::decor`'s chrome last copied in, zero for
    /// none.
    chrome: u64,
}

static WINDOWS: Mutex<Option<HashMap<usize, WindowState>>> = Mutex::new(None);

/// Views sent `-setNeedsDisplay:`, by address, drawn at the next pump.
static NEEDS_DISPLAY: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// The batches [`pump`] is drawing, innermost last, so a view freed by another
/// view's `-drawRect:` is taken out of them too. A stack because a `-drawRect:`
/// can run a nested event loop that pumps again. Zero marks a forgotten entry.
static DRAWING: Mutex<Vec<Vec<usize>>> = Mutex::new(Vec::new());

/// Distinguish an unavailable GPU presenter from a busy window. The latter
/// must not force a CPU fallback over content still being presented through
/// Vulkan: retain the backing store's dirty state for the next event pump.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentOutcome {
    Displayed,
    /// An earlier copy of the store reached the screen through the GPU, and
    /// this one is still on its way there: keep the store dirty.
    DisplayedEarlier,
    Fallback,
    Deferred,
}

pub type Presenter = fn(sdl3::Window, &Store, &[Hole], &[GpuOverlay]) -> PresentOutcome;

static PRESENTER: Mutex<Option<Presenter>> = Mutex::new(None);

/// A picture on the GPU, shared as a Linux dma-buf: premultiplied, top row
/// first.
pub struct Dmabuf {
    pub width: u32,
    pub height: u32,
    /// The DRM fourcc and format modifier.
    pub fourcc: u32,
    pub modifier: u64,
    pub planes: Vec<DmabufPlane>,
}

pub struct DmabufPlane {
    pub fd: std::os::fd::OwnedFd,
    pub offset: u32,
    pub stride: u32,
}

/// An overlay the presenter draws from a dma-buf, over `rect` of the store
/// in window pixels, source-over.
pub struct GpuOverlay {
    pub picture: std::sync::Arc<Dmabuf>,
    pub rect: Rect,
}

/// The GPU a presenter shows windows with, and what it can show from a
/// dma-buf without a copy.
#[derive(Clone, Debug)]
pub struct Importer {
    /// The GPU's DRM render node, as `st_rdev`.
    pub device: u64,
    /// DRM fourcc and modifier pairs it can sample.
    pub formats: Vec<(u32, u64)>,
}

/// Asks the presenter for its [`Importer`].
pub type ImporterQuery = fn() -> Option<Importer>;

static IMPORTER: Mutex<Option<ImporterQuery>> = Mutex::new(None);

/// Let the presenter say it can draw [`GpuOverlay`]s. `query` is asked
/// only when someone wants to know, so a GPU is not opened for nothing.
pub fn set_importer(query: ImporterQuery) {
    *IMPORTER.lock().unwrap_or_else(|e| e.into_inner()) = Some(query);
}

/// The presenter's GPU, if it draws dma-bufs at all.
pub fn importer() -> Option<Importer> {
    let query = (*IMPORTER.lock().unwrap_or_else(|e| e.into_inner()))?;
    query()
}

/// Windows whose last present went through the presenter, and so would draw
/// a dma-buf overlay; and those whose presenter could not import one.
static GPU_WINDOWS: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());

/// Whether an overlay on `sdl`'s window can be a dma-buf: the presenter
/// showed its last frame and has not failed to import one there. Otherwise
/// it has to be pixels, which every present can draw.
pub fn shows_dmabufs(sdl: sdl3::Window) -> bool {
    GPU_WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|&(window, refused)| window == sdl as usize && !refused)
}

/// The presenter could not draw a dma-buf overlay on `sdl`'s window; from
/// now on it is given pixels there.
pub fn refuse_dmabufs(sdl: sdl3::Window) {
    let mut windows = GPU_WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    match windows
        .iter_mut()
        .find(|(window, _)| *window == sdl as usize)
    {
        Some(entry) => entry.1 = true,
        None => windows.push((sdl as usize, true)),
    }
}

fn presented_through_gpu(sdl: sdl3::Window, gpu: bool) {
    let mut windows = GPU_WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    let at = windows
        .iter()
        .position(|(window, _)| *window == sdl as usize);
    match (at, gpu) {
        (None, true) => windows.push((sdl as usize, false)),
        (Some(at), false) if !windows[at].1 => {
            windows.remove(at);
        }
        _ => {}
    }
}

/// Let a GPU backend present stores, so that a window it also presents layers
/// to has one owner of its surface.
pub fn set_presenter(presenter: Presenter) {
    *PRESENTER.lock().unwrap_or_else(|e| e.into_inner()) = Some(presenter);
}

#[derive(Clone, Copy)]
struct Focus {
    view: Id,
    window: sdl3::Window,
    /// The view's top-left corner in window pixels, and its size in pixels.
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    /// Window pixels per point.
    density: f64,
    flipped: bool,
    /// Drawing into [`OFFSCREEN`]'s store keyed by `view`, an image being
    /// rendered, rather than into a view in a window.
    offscreen: bool,
}

thread_local! {
    static FOCUS: RefCell<Vec<Focus>> = const { RefCell::new(Vec::new()) };
}

/// Images being rendered by [`begin_offscreen`], keyed by the caller's key.
static OFFSCREEN: Mutex<Option<HashMap<usize, Store>>> = Mutex::new(None);

/// Send drawing to a transparent `width` by `height` pixel image, at
/// `density` pixels per point, until [`end_offscreen`] with the same key.
pub(crate) fn begin_offscreen(key: usize, width: u32, height: u32, density: f64, flipped: bool) {
    OFFSCREEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(key, Store::new(width.max(1), height.max(1)));
    FOCUS.with(|stack| {
        stack.borrow_mut().push(Focus {
            view: key as Id,
            window: std::ptr::null_mut(),
            x: 0.0,
            y: 0.0,
            width: f64::from(width.max(1)),
            height: f64::from(height.max(1)),
            density: if density > 0.0 { density } else { 1.0 },
            flipped,
            offscreen: true,
        })
    });
}

/// The premultiplied B, G, R, A pixels, top row first, drawn since
/// [`begin_offscreen`], and that focus undone.
pub(crate) fn end_offscreen(key: usize) -> Option<Store> {
    FOCUS.with(|stack| {
        let mut stack = stack.borrow_mut();
        if let Some(at) = stack
            .iter()
            .rposition(|focus| focus.offscreen && focus.view as usize == key)
        {
            stack.truncate(at);
        }
    });
    release_context_override_above(focus_depth());
    OFFSCREEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()?
        .remove(&key)
}

fn focus_on(view: Id) -> Option<Focus> {
    // SAFETY: null or an object this runtime allocated.
    let frame = unsafe { crate::view::presentation_frame(view) }?;
    let (points, _) = sdl3::video::window_size(frame.window)?;
    let density = match points > 0 {
        true => f64::from(frame.window_width) / f64::from(points),
        false => 1.0,
    };
    Some(Focus {
        view,
        window: frame.window,
        x: f64::from(frame.x),
        y: f64::from(frame.y),
        width: f64::from(frame.width),
        height: f64::from(frame.height),
        density: if density > 0.0 { density } else { 1.0 },
        flipped: crate::view::is_flipped(view),
        offscreen: false,
    })
}

fn push_focus(view: Id) -> bool {
    let Some(focus) = focus_on(view) else {
        return false;
    };
    FOCUS.with(|stack| stack.borrow_mut().push(focus));
    true
}

fn pop_focus() {
    let depth = FOCUS.with(|stack| {
        let mut stack = stack.borrow_mut();
        stack.pop();
        stack.len()
    });
    release_context_override_above(depth);
}

fn current_focus() -> Option<Focus> {
    FOCUS.with(|stack| stack.borrow().last().copied())
}

/// What vector drawing needs to know about the focused view: where its pixels
/// are, and which focus it is.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Device {
    /// The view (or offscreen key) and how deep in the focus stack it is,
    /// which together tell one `-drawRect:` from the next.
    pub key: usize,
    pub depth: usize,
    pub width: u32,
    pub height: u32,
    pub density: f64,
    pub flipped: bool,
}

impl Device {
    /// A point in the view's coordinates, in pixels from its top left.
    pub fn to_pixels(self, x: f64, y: f64) -> [f64; 2] {
        let y = match self.flipped {
            true => y * self.density,
            false => f64::from(self.height) - y * self.density,
        };
        [x * self.density, y]
    }
}

pub(crate) fn focused_device() -> Option<Device> {
    FOCUS.with(|stack| {
        let stack = stack.borrow();
        let focus = stack.last()?;
        Some(Device {
            key: focus.view as usize,
            depth: stack.len(),
            width: focus.width.max(1.0) as u32,
            height: focus.height.max(1.0) as u32,
            density: focus.density,
            flipped: focus.flipped,
        })
    })
}

/// Blend straight-alpha `color` over the focused view through `coverage`, a
/// `width`-wide mask of the view's pixels whose top-left is `(left, top)`.
pub(crate) fn paint_coverage(left: i64, top: i64, width: usize, coverage: &[f32], color: [f64; 4]) {
    let Some(focus) = current_focus() else {
        return;
    };
    if width == 0 || coverage.is_empty() {
        return;
    }
    let (store_width, store_height) = (focus.width.max(1.0) as u32, focus.height.max(1.0) as u32);
    let [red, green, blue, alpha] = color.map(|c| c.clamp(0.0, 1.0));
    let drawn = {
        let stores = match focus.offscreen {
            true => &OFFSCREEN,
            false => &VIEW_STORES,
        };
        let mut stores = stores.lock().unwrap_or_else(|e| e.into_inner());
        let store = stores
            .get_or_insert_with(HashMap::new)
            .entry(focus.view as usize)
            .or_insert_with(|| Store::new(store_width, store_height));
        store.resize(store_width, store_height);
        let rows = coverage.len() / width;
        let x0 = left.max(0);
        let y0 = top.max(0);
        let x1 = (left + width as i64).min(i64::from(store.width));
        let y1 = (top + rows as i64).min(i64::from(store.height));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for y in y0..y1 {
            let mask = &coverage[(y - top) as usize * width..][..width];
            let base = y as usize * store.width as usize * 4;
            for x in x0..x1 {
                let a = f64::from(mask[(x - left) as usize].clamp(0.0, 1.0)) * alpha;
                if a <= 0.0 {
                    continue;
                }
                let pixel = &mut store.pixels[base + x as usize * 4..][..4];
                for (channel, source) in [blue, green, red, 1.0].into_iter().enumerate() {
                    let over = source * a + f64::from(pixel[channel]) / 255.0 * (1.0 - a);
                    pixel[channel] = (over * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        store.touch();
        Rect {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    };
    if focus.offscreen {
        return;
    }
    damage(
        focus.window,
        Rect {
            x: drawn.x + focus.x as i64,
            y: drawn.y + focus.y as i64,
            ..drawn
        },
    );
}

/// Pixels per point of what is being drawn into, 1 with nothing focused.
pub(crate) fn focus_density() -> f64 {
    current_focus().map_or(1.0, |focus| focus.density)
}

fn damage(window: sdl3::Window, rect: Rect) {
    if rect.is_empty() {
        return;
    }
    let mut windows = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    let state = windows
        .get_or_insert_with(HashMap::new)
        .entry(window as usize)
        .or_default();
    state.damage = state.damage.union(&rect);
}

/// `NSCompositingOperation` values this draws with; any other is drawn as
/// source-over.
const COMPOSITE_COPY: usize = 1;
/// Keeps the destination where the source is opaque: how a template image is
/// tinted, by filling with the colour and then drawing the image this way.
const COMPOSITE_DESTINATION_IN: usize = 7;

/// Draw premultiplied BGRA `source` pixels into the focused view.
///
/// `destination` is in the view's coordinates and `from` in the image's,
/// whose origin is its bottom-left corner. `points` is the image's size in
/// points, which scales `from` into the source's pixels.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_image(
    source: &[u8],
    source_width: usize,
    source_height: usize,
    points: NSSize,
    destination: NSRect,
    from: NSRect,
    operation: usize,
    fraction: f64,
    respect_flipped: bool,
) {
    let Some(focus) = current_focus() else {
        return;
    };
    let destination = crate::bezier::transform_rect(destination);
    let (width, height) = (focus.width.max(1.0) as u32, focus.height.max(1.0) as u32);
    let drawn = {
        let stores = match focus.offscreen {
            true => &OFFSCREEN,
            false => &VIEW_STORES,
        };
        let mut stores = stores.lock().unwrap_or_else(|e| e.into_inner());
        let store = stores
            .get_or_insert_with(HashMap::new)
            .entry(focus.view as usize)
            .or_insert_with(|| Store::new(width, height));
        store.resize(width, height);
        draw_into(
            store,
            source,
            source_width,
            source_height,
            points,
            destination,
            from,
            operation,
            fraction,
            respect_flipped,
        )
    };
    if focus.offscreen {
        return;
    }
    damage(
        focus.window,
        Rect {
            x: drawn.x + focus.x as i64,
            y: drawn.y + focus.y as i64,
            ..drawn
        },
    );
}

/// The focused view's store as a canvas: `draw` gets its premultiplied B, G,
/// R, A pixels, top row first, with their width and height, and answers the
/// rectangle of them it changed. False with nothing focused.
pub(crate) fn draw_focused(draw: impl FnOnce(&mut [u8], u32, u32) -> Option<Rect>) -> bool {
    let Some(focus) = current_focus() else {
        return false;
    };
    let (width, height) = (focus.width.max(1.0) as u32, focus.height.max(1.0) as u32);
    let drawn = {
        let stores = match focus.offscreen {
            true => &OFFSCREEN,
            false => &VIEW_STORES,
        };
        let mut stores = stores.lock().unwrap_or_else(|e| e.into_inner());
        let store = stores
            .get_or_insert_with(HashMap::new)
            .entry(focus.view as usize)
            .or_insert_with(|| Store::new(width, height));
        store.resize(width, height);
        let drawn = draw(&mut store.pixels, store.width, store.height);
        if drawn.is_some_and(|r| !r.is_empty()) {
            store.touch();
        }
        drawn
    };
    if let Some(drawn) = drawn
        && !focus.offscreen
    {
        damage(
            focus.window,
            Rect {
                x: drawn.x + focus.x as i64,
                y: drawn.y + focus.y as i64,
                ..drawn
            },
        );
    }
    true
}

/// [`draw_image`], into the focused view's store; answers the rectangle of
/// it that changed.
#[allow(clippy::too_many_arguments)]
fn draw_into(
    store: &mut Store,
    source: &[u8],
    source_width: usize,
    source_height: usize,
    points: NSSize,
    destination: NSRect,
    from: NSRect,
    operation: usize,
    fraction: f64,
    respect_flipped: bool,
) -> Rect {
    let Some(focus) = current_focus() else {
        return Rect::default();
    };
    if source_width == 0
        || source_height == 0
        || fraction <= 0.0
        || source.len() < source_width * source_height * 4
    {
        return Rect::default();
    }
    let from = match from.size.width <= 0.0 || from.size.height <= 0.0 {
        true => NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: points,
        },
        false => from,
    };
    let scale_x = source_width as f64 / points.width.max(f64::MIN_POSITIVE);
    let scale_y = source_height as f64 / points.height.max(f64::MIN_POSITIVE);
    let source_left = from.origin.x * scale_x;
    let source_top = source_height as f64 - (from.origin.y + from.size.height) * scale_y;
    let source_w = from.size.width * scale_x;
    let source_h = from.size.height * scale_y;

    let view_height = f64::from(store.height) / focus.density;
    let top = match focus.flipped {
        true => destination.origin.y,
        false => view_height - (destination.origin.y + destination.size.height),
    };
    let left = destination.origin.x * focus.density;
    let top = top * focus.density;
    let width = destination.size.width * focus.density;
    let height = destination.size.height * focus.density;
    if width <= 0.0 || height <= 0.0 {
        return Rect::default();
    }
    // A flipped context draws an image upside down unless asked not to.
    let mirrored = focus.flipped && !respect_flipped;
    let opacity = (fraction.min(1.0) * 256.0) as u32;

    let x0 = left.max(0.0).round() as i64;
    let x1 = (left + width).min(f64::from(store.width)).round() as i64;
    let y0 = top.max(0.0).round() as i64;
    let y1 = (top + height).min(f64::from(store.height)).round() as i64;
    if x0 >= x1 || y0 >= y1 {
        return Rect::default();
    }
    let columns: Vec<usize> = (x0..x1)
        .map(|x| {
            let u = (x as f64 + 0.5 - left) / width;
            ((source_left + u * source_w).floor().max(0.0) as usize).min(source_width - 1)
        })
        .collect();
    for y in y0..y1 {
        let mut v = (y as f64 + 0.5 - top) / height;
        if mirrored {
            v = 1.0 - v;
        }
        let row = ((source_top + v * source_h).floor().max(0.0) as usize).min(source_height - 1);
        let from_row = &source[row * source_width * 4..(row + 1) * source_width * 4];
        let base = y as usize * store.width as usize * 4;
        let to_row = &mut store.pixels[base + x0 as usize * 4..base + x1 as usize * 4];
        for (target, &column) in to_row.as_chunks_mut::<4>().0.iter_mut().zip(&columns) {
            let texel = &from_row[column * 4..column * 4 + 4];
            if operation == COMPOSITE_COPY && opacity >= 256 {
                target.copy_from_slice(texel);
                continue;
            }
            let alpha = texel[3] as u32 * opacity / 256;
            if operation == COMPOSITE_DESTINATION_IN {
                for channel in target.iter_mut() {
                    *channel = (u32::from(*channel) * alpha / 255) as u8;
                }
                continue;
            }
            for channel in 0..4 {
                let s = texel[channel] as u32 * opacity / 256;
                let d = target[channel] as u32;
                target[channel] = match operation == COMPOSITE_COPY {
                    true => s as u8,
                    false => (s + d * (255 - alpha) / 255).min(255) as u8,
                };
            }
        }
    }
    store.touch();
    Rect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

/// Mark a view for `-drawRect:` at the next pump.
pub(crate) fn needs_display(view: Id) {
    let mut pending = NEEDS_DISPLAY.lock().unwrap_or_else(|e| e.into_inner());
    if !pending.contains(&(view as usize)) {
        pending.push(view as usize);
    }
}

/// Whether a view of `window` is waiting to be displayed.
pub(crate) fn window_needs_display(window: Id) -> bool {
    NEEDS_DISPLAY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        // SAFETY: a view recorded while live and forgotten on deallocation.
        .any(|view| unsafe { crate::view::window_of(*view as Id) } == window)
}

/// Mark a view and everything under it, as a window that has nothing valid
/// to show needs, and as a subtree moved into a window does: none of it has
/// been drawn there.
pub(crate) fn needs_display_in_tree(view: Id, depth: usize) {
    if view.is_null() || depth > 64 {
        return;
    }
    needs_display(view);
    // SAFETY: a view this crate made.
    for child in unsafe { crate::view::children(view) } {
        needs_display_in_tree(child, depth + 1);
    }
}

/// Forget a window whose SDL window is about to be destroyed.
///
/// The store is keyed by the SDL window's address and SDL reuses addresses, so
/// a store left behind would be handed to the next window made - at the size the
/// old one was.
pub fn forget_window(sdl: sdl3::Window) {
    if let Some(stores) = STORES.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        stores.remove(&(sdl as usize));
    }
    GPU_WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(window, _)| *window != sdl as usize);
    SURFACE_LAYERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(window, _)| *window != sdl as usize);
}

/// Layers that have written their frames into a window's SDL surface
/// themselves, as `(window, layer)`.
static SURFACE_LAYERS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

/// Record that `layer` has drawn its frame into `sdl`'s window surface, which
/// the store's own presents through that surface then leave alone.
///
/// A layer's first frame there can land before the window's layout settles,
/// over rows that are now the chrome's, so the store is presented again then.
pub fn layer_drew_on_surface(sdl: sdl3::Window, layer: usize) {
    let mut layers = SURFACE_LAYERS.lock().unwrap_or_else(|e| e.into_inner());
    if !layers.contains(&(sdl as usize, layer)) {
        layers.push((sdl as usize, layer));
        drop(layers);
        present_again(sdl);
    }
}

/// Forget a view that is being deallocated, so no pump draws freed memory.
pub(crate) fn forget(view: Id) {
    let key = view as usize;
    NEEDS_DISPLAY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|v| *v != key);
    ANIMATING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(_, v)| *v != key);
    for batch in DRAWING.lock().unwrap_or_else(|e| e.into_inner()).iter_mut() {
        for entry in batch.iter_mut().filter(|entry| **entry == key) {
            *entry = 0;
        }
    }
    if let Some(stores) = VIEW_STORES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        stores.remove(&key);
    }
}

/// Draw `view` now, as `-display` does.
///
/// A layer-backed view is displayed through its layer, as AppKit does it: a
/// layer whose delegate implements `-displayLayer:` is sent `-display`, which
/// hands the whole job to the delegate (Qt answers with an expose event and
/// then sets the layer's `contents`); a view that answers `-wantsUpdateLayer`
/// with YES is sent `-updateLayer`. Every other view has `-drawRect:` sent with
/// its focus locked, and what it draws is its layer's content.
pub(crate) fn display_view(view: Id) {
    if crate::view::is_layer_backed(view) && focus_on(view).is_some() {
        let layer = crate::view::layer_of(view);
        let delegate = crate::layer::delegate_of(layer);
        // SAFETY: null or a live object, which the weak slot guarantees.
        let displays = !delegate.is_null()
            && unsafe { objc::object::class_of(delegate) }
                .is_some_and(|c| c.method(objc::sel::get("displayLayer:")).is_some());
        if displays {
            send(layer, "display");
            return;
        }
        // SAFETY: `-wantsUpdateLayer` is `BOOL (id, SEL)`.
        let updates = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
            f(view, objc::sel::get("wantsUpdateLayer"))
        } != 0;
        if updates {
            send(view, "updateLayer");
            return;
        }
    }
    let sel_draw = objc::sel::get("drawRect:");
    // SAFETY: a live object this runtime allocated.
    let responds =
        unsafe { objc::object::class_of(view) }.is_some_and(|c| c.method(sel_draw).is_some());
    if !responds || !push_focus(view) {
        return;
    }
    clear_focused_store();
    let bounds = current_focus().map_or(NSRect::default(), |focus| NSRect {
        origin: NSPoint { x: 0.0, y: 0.0 },
        size: NSSize {
            width: focus.width / focus.density,
            height: focus.height / focus.density,
        },
    });
    // SAFETY: `-drawRect:` is `void (id, SEL, NSRect)`.
    unsafe {
        let send: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
        send(view, sel_draw, bounds);
    }
    pop_focus();
}

/// Start a `-drawRect:` from a transparent store. What the view drew last time
/// is gone on a Mac too: the window behind a view that is not opaque is drawn
/// again first, so only this draw's pixels end up over it.
fn clear_focused_store() {
    let Some(focus) = current_focus() else {
        return;
    };
    let cleared = VIEW_STORES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|stores| stores.get_mut(&(focus.view as usize)))
        .map(|store| {
            store.pixels.fill(0);
            store.touch();
            Rect {
                x: focus.x as i64,
                y: focus.y as i64,
                width: i64::from(store.width),
                height: i64::from(store.height),
            }
        });
    if let Some(area) = cleared {
        damage(focus.window, area);
    }
}

fn send(receiver: Id, name: &str) {
    // SAFETY: a `void (id, SEL)` method on a live object.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(name));
    }
}

/// Draw everything marked for display and present every window that changed.
pub fn pump() {
    crate::decor::pump();
    crate::layout::prepare();
    for window in crate::window::all() {
        if !crate::window::is_shown(window) {
            continue;
        }
        // SAFETY: a window this crate made.
        let sdl = unsafe { crate::window::sdl_window(window) };
        let Some((width, height)) = sdl3::video::window_size_in_pixels(sdl) else {
            continue;
        };
        let current = STORES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|stores| stores.get(&(sdl as usize)))
            .map(|store| (store.width as i32, store.height as i32));
        // A window newly shown, or resized, has nothing valid in it: AppKit
        // redraws the whole window then.
        if current != Some((width.max(1), height.max(1))) {
            needs_display_in_tree(crate::window::content_view_of(window), 0);
        }
    }
    let pending = std::mem::take(&mut *NEEDS_DISPLAY.lock().unwrap_or_else(|e| e.into_inner()));
    let count = pending.len();
    let depth = {
        let mut drawing = DRAWING.lock().unwrap_or_else(|e| e.into_inner());
        drawing.push(pending);
        drawing.len() - 1
    };
    for at in 0..count {
        let view = DRAWING.lock().unwrap_or_else(|e| e.into_inner())[depth][at] as Id;
        if view.is_null() {
            continue;
        }
        // SAFETY: a view this crate recorded while it was live.
        let window = unsafe { crate::view::window_of(view) };
        if crate::window::is_shown(window) && !crate::view::is_hidden_or_has_hidden_ancestor(view) {
            display_view(view);
        }
    }
    DRAWING.lock().unwrap_or_else(|e| e.into_inner()).pop();
    damage_animating();
    for window in crate::window::all() {
        if crate::window::is_shown(window) {
            compose_window(window);
        }
    }
    present_dirty();
}

/// Views whose layer trees were still animating when last composed, by window.
static ANIMATING: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

/// Recompose each animating view, at most once a display frame: the pump runs
/// far more often than that, and every recompose is a software fill.
fn damage_animating() {
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(16)) {
        return;
    }
    let views: Vec<usize> = ANIMATING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(_, view)| *view)
        .collect();
    if views.is_empty() {
        return;
    }
    *last = Some(std::time::Instant::now());
    drop(last);
    for view in views {
        damage_view(view as Id);
    }
}

/// Have the part of the window `view` covers recomposed at the next pump,
/// without asking the view to draw again: its layer tree changed, not it.
pub(crate) fn damage_view(view: Id) {
    // SAFETY: a view this crate made.
    let Some(frame) = (unsafe { crate::view::presentation_frame(view) }) else {
        return;
    };
    damage(
        frame.window,
        Rect {
            x: i64::from(frame.x),
            y: i64::from(frame.y),
            width: i64::from(frame.width),
            height: i64::from(frame.height),
        },
    );
}

/// One view's place in the composite.
struct Placed {
    view: Id,
    /// Where it is, in window pixels, and the part of that its ancestors let
    /// show.
    rect: Rect,
    clip: Rect,
    /// `-alphaValue` with its ancestors' folded in.
    alpha: f64,
}

/// The single rounded rectangle accepted by backing, software overlay,
/// OpenGL and Metal presentation. Avoid using point-density to reconstruct
/// widths: physical X/Y scales and half-point edge rounding can differ.
fn logical_pixel_bounds(sdl: sdl3::Window, whole: Rect) -> Rect {
    crate::window::presentation_clip(sdl)
        .map(|clip| Rect {
            x: i64::from(clip.x),
            y: i64::from(clip.y),
            width: i64::from(clip.w),
            height: i64::from(clip.h),
        })
        .unwrap_or(whole)
        .intersection(&whole)
}

/// The shown views of a window, back to front, placed.
fn place_tree(window: Id, sdl: sdl3::Window) -> Vec<Placed> {
    let mut out = Vec::new();
    let Some((points_high, pixels)) = sdl3::video::window_size(sdl)
        .zip(sdl3::video::window_size_in_pixels(sdl))
        .map(|((w, h), (pw, ph))| (h, (w, pw, ph)))
    else {
        return out;
    };
    let (points_wide, pixel_width, pixel_height) = pixels;
    let whole = Rect {
        x: 0,
        y: 0,
        width: i64::from(pixel_width),
        height: i64::from(pixel_height),
    };
    let content = crate::window::content_view_of(window);
    let Some(surface) = crate::window::surface_frame(window) else {
        return out;
    };
    // Use the exact edge rounding of every presentation backend. Rounding
    // an origin and a size independently can admit one pixel past the logical
    // right/bottom edge when the origin is fractional.
    let logical_clip = logical_pixel_bounds(sdl, whole);
    // The window's base coordinates: not flipped, origin at the bottom, and
    // taller than the content by the chrome above it.
    let base = NSRect {
        origin: NSPoint {
            x: surface.origin.x,
            y: f64::from(points_high) - surface.origin.y - surface.size.height,
        },
        size: NSSize {
            width: surface.size.width,
            height: (surface.size.height - crate::window::content_inset(window)).max(0.0),
        },
    };
    let geometry = Geometry {
        window_height: f64::from(points_high),
        host_points: (points_wide, points_high),
        host_pixels: (pixel_width, pixel_height),
    };
    place_view(content, base, false, logical_clip, geometry, 0, &mut out);
    out
}

#[cfg(test)]
pub(crate) fn placed_rect_for_tests(window: Id, view: Id) -> Option<Rect> {
    let sdl = unsafe { crate::window::sdl_window(window) };
    place_tree(window, sdl)
        .into_iter()
        .find(|placed| placed.view == view)
        .map(|placed| placed.rect)
}

#[derive(Clone, Copy)]
struct Geometry {
    /// The window's height in points, which base coordinates count up from.
    window_height: f64,
    host_points: (i32, i32),
    host_pixels: (i32, i32),
}

/// Place `view`, whose superview's rectangle in window base coordinates is
/// `outer` and whose flippedness is `outer_flipped`, then its subviews.
fn place_view(
    view: Id,
    outer: NSRect,
    outer_flipped: bool,
    clip: Rect,
    geometry: Geometry,
    depth: usize,
    out: &mut Vec<Placed>,
) {
    if view.is_null() || depth > 64 || crate::view::is_hidden_itself(view) {
        return;
    }
    let frame = crate::view::frame_of(view);
    let y = match outer_flipped {
        true => outer.origin.y + outer.size.height - frame.origin.y - frame.size.height,
        false => outer.origin.y + frame.origin.y,
    };
    let in_window = NSRect {
        origin: NSPoint {
            x: outer.origin.x + frame.origin.x,
            y,
        },
        size: frame.size,
    };
    let pixels = crate::window::pixel_rect_from_surface(
        NSRect {
            origin: NSPoint {
                x: in_window.origin.x,
                y: geometry.window_height - (y + frame.size.height),
            },
            size: frame.size,
        },
        geometry.host_points,
        geometry.host_pixels,
    );
    let rect = Rect {
        x: i64::from(pixels.x),
        y: i64::from(pixels.y),
        width: i64::from(pixels.w),
        height: i64::from(pixels.h),
    };
    let clip = clip.intersection(&rect);
    let alpha = crate::view::effective_alpha(view);
    out.push(Placed {
        view,
        rect,
        clip,
        alpha,
    });
    let flipped = crate::view::is_flipped(view);
    // Subviews' frames are measured from the bounds origin, so a scrolled
    // clip view moves its document the other way.
    let shift = crate::view::bounds_origin_of(view);
    let inner = NSRect {
        origin: NSPoint {
            x: in_window.origin.x - shift.x,
            y: match flipped {
                true => in_window.origin.y + shift.y,
                false => in_window.origin.y - shift.y,
            },
        },
        size: in_window.size,
    };
    // SAFETY: a view this crate made.
    for child in unsafe { crate::view::children(view) } {
        place_view(child, inner, flipped, clip, geometry, depth + 1, out);
    }
}

/// The views placed above a Metal layer whose own drawing covers all of their
/// rectangle.
fn opaque_views(placed: &[Placed]) -> Vec<usize> {
    let Some(first) = placed
        .iter()
        .position(|p| crate::layer::is_metal(crate::view::layer_of(p.view)))
    else {
        return Vec::new();
    };
    let stores = VIEW_STORES.lock().unwrap_or_else(|e| e.into_inner());
    let Some(stores) = stores.as_ref() else {
        return Vec::new();
    };
    placed[first + 1..]
        .iter()
        .filter(|p| !p.clip.is_empty() && p.alpha >= 1.0)
        .filter(|p| {
            stores.get(&(p.view as usize)).is_some_and(|store| {
                i64::from(store.width) >= p.rect.width
                    && i64::from(store.height) >= p.rect.height
                    && store.pixels.as_chunks::<4>().0.iter().all(|t| t[3] == 255)
            })
        })
        .map(|p| p.view as usize)
        .collect()
}

/// Bring a window's composite up to date with its views: recompose what was
/// drawn over, and everywhere a view appeared, moved or went away.
fn compose_window(window: Id) {
    // First, so it drops last: after every lock below is released.
    let _owed = crate::coreanimation::defer_releases();
    // SAFETY: a window this crate made.
    let sdl = unsafe { crate::window::sdl_window(window) };
    let Some((width, height)) = sdl3::video::window_size_in_pixels(sdl) else {
        return;
    };
    let (width, height) = (width.max(1) as u32, height.max(1) as u32);
    let placed = place_tree(window, sdl);
    let surface_damage: Vec<Rect> = placed
        .iter()
        .flat_map(|p| crate::coreanimation::surface_contents_damage(p.view, p.rect, p.clip))
        .collect();
    let now: Vec<(usize, Rect, Rect)> = placed
        .iter()
        .map(|p| (p.view as usize, p.clip, p.rect))
        .collect();
    let overlaid: Vec<usize> = OVERLAYS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(view, _)| *view)
        .collect();
    let opaque = opaque_views(&placed);
    let mut holes = Vec::new();
    let mut covered = Vec::new();
    for (index, p) in placed.iter().enumerate() {
        let layer = crate::view::layer_of(p.view);
        if !crate::layer::is_metal(layer) || p.clip.is_empty() {
            continue;
        }
        // An overlay is stacked above the layers of the views placed before
        // it - a web view inside a Metal-drawn panel - so there the store,
        // which carries the overlay, shows instead of the layer. So is a view
        // that draws: a web view's title bar over a game. Only where it drew
        // opaquely, since the store holds nothing of the layer to show
        // through a label's transparent pixels.
        let above: Vec<Rect> = placed[index + 1..]
            .iter()
            .filter(|q| {
                overlaid.contains(&(q.view as usize)) || opaque.contains(&(q.view as usize))
            })
            .map(|q| q.clip.intersection(&p.clip))
            .filter(|rect| !rect.is_empty())
            .collect();
        holes.extend(p.clip.subtract(&above).into_iter().map(|rect| Hole {
            layer: layer as usize,
            rect,
        }));
        if !above.is_empty() {
            covered.push((layer as usize, above));
        }
    }

    let mut stores = STORES.lock().unwrap_or_else(|e| e.into_inner());
    // A window no view has drawn into has nothing to compose. Composing it
    // anyway makes a store of transparent black out of the content view's own
    // rectangle - a view that appeared is a region to recompose, and there is
    // nothing to recompose it from - and presenting that paints over whatever
    // else owns the window's pixels. For a guest drawing through an
    // `NSOpenGLContext` that is the whole of its frame, so the window goes
    // black. Once a view has drawn, the composite exists and is kept up to
    // date from then on, including as content goes away again.
    let composed_before = stores
        .as_ref()
        .is_some_and(|stores| stores.contains_key(&(sdl as usize)));
    // A window with chrome always has something of its own to show.
    let chrome = crate::decor::pixels(sdl);
    if !composed_before && holes.is_empty() && chrome.is_none() {
        let views = VIEW_STORES.lock().unwrap_or_else(|e| e.into_inner());
        let drawn = views.as_ref().is_some_and(|views| {
            placed
                .iter()
                .any(|p| views.contains_key(&(p.view as usize)))
        }) || placed
            .iter()
            .any(|p| crate::coreanimation::has_tree(crate::view::layer_of(p.view)));
        if !drawn {
            return;
        }
    }
    let store = stores
        .get_or_insert_with(HashMap::new)
        .entry(sdl as usize)
        .or_insert_with(|| Store::new(width, height));
    let whole = Rect {
        x: 0,
        y: 0,
        width: i64::from(width),
        height: i64::from(height),
    };
    let resized = (store.width, store.height) != (width, height);
    if resized {
        *store = Store::new(width, height);
    }

    let mut windows = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    let state = windows
        .get_or_insert_with(HashMap::new)
        .entry(sdl as usize)
        .or_default();
    let mut region = std::mem::take(&mut state.damage);
    for rect in &surface_damage {
        region = region.union(rect);
    }
    if resized {
        region = whole;
    }
    if state.placed != now {
        // Where each view was and is, for every one that changed.
        for (_, rect, _) in state.placed.iter().filter(|entry| !now.contains(entry)) {
            region = region.union(rect);
        }
        for (_, rect, _) in now.iter().filter(|entry| !state.placed.contains(entry)) {
            region = region.union(rect);
        }
        state.placed = now;
    }
    let holes_changed = state.holes.len() != holes.len()
        || state
            .holes
            .iter()
            .zip(&holes)
            .any(|(a, b)| a.layer != b.layer || a.rect != b.rect);
    state.holes = holes;
    state.covered = covered;
    let chrome_generation = chrome.as_ref().map_or(0, |chrome| chrome.generation);
    let chrome_changed =
        std::mem::replace(&mut state.chrome, chrome_generation) != chrome_generation;
    drop(windows);
    let region = region.intersection(&whole);
    if region.is_empty() {
        if let Some(chrome) = chrome.as_ref().filter(|_| chrome_changed || resized) {
            copy_chrome(store, chrome);
            store.touch();
        }
        if holes_changed {
            store.touch();
        }
        return;
    }

    // What no view covers shows the window's `backgroundColor`, which a
    // program that draws only a logo and a bar relies on for everything else.
    // With none set it is `windowBackgroundColor`, AppKit's default; a layer
    // view's hole is the layer's either way.
    let background = crate::window::background_pixel(window).unwrap_or(DEFAULT_BACKGROUND);
    let logical = logical_pixel_bounds(sdl, whole);
    for row in region.y..region.y + region.height {
        let base = (row as usize * width as usize + region.x as usize) * 4;
        let pixels = store.pixels[base..base + region.width as usize * 4]
            .as_chunks_mut::<4>()
            .0;
        // Outside AppKit's logical window is the fullscreen container,
        // not more of the window's default background.
        // FIXME (ACCURACY): the native probe did not measure this area's color.
        if row < logical.y || row >= logical.y + logical.height {
            pixels.fill([0, 0, 0, 255]);
        } else {
            let left = (logical.x - region.x).clamp(0, region.width) as usize;
            let right = (logical.x + logical.width - region.x).clamp(0, region.width) as usize;
            pixels[..left].fill([0, 0, 0, 255]);
            pixels[left..right].fill(background);
            pixels[right..].fill([0, 0, 0, 255]);
        }
    }
    let views = VIEW_STORES.lock().unwrap_or_else(|e| e.into_inner());
    let mut animating = Vec::new();
    for p in &placed {
        let clip = p.clip.intersection(&region);
        use crate::coreanimation::{Pass, render};
        render(p.view, store, p.rect, clip, Pass::Background);
        if let Some(source) = views
            .as_ref()
            .and_then(|views| views.get(&(p.view as usize)))
        {
            blend_at(store, source, p.rect.x, p.rect.y, clip, p.alpha);
        }
        if render(p.view, store, p.rect, clip, Pass::Sublayers) {
            animating.push(p.view as usize);
        }
    }
    drop(views);
    ANIMATING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(window, _)| *window != sdl as usize);
    ANIMATING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .extend(animating.into_iter().map(|view| (sdl as usize, view)));
    if let Some(chrome) = &chrome {
        copy_chrome(store, chrome);
    }
    store.touch();
}

/// Put a window's chrome over the top rows of its store.
fn copy_chrome(store: &mut Store, chrome: &crate::decor::Pixels) {
    let columns = store.width.min(chrome.width) as usize;
    for row in 0..store.height.min(chrome.height) as usize {
        let from = row * chrome.width as usize * 4;
        let to = row * store.width as usize * 4;
        store.pixels[to..to + columns * 4]
            .copy_from_slice(&chrome.pixels[from..from + columns * 4]);
    }
}

/// Present a window's store again at the next pump, without recomposing it:
/// for a presenter that lost what it last showed.
pub fn present_again(sdl: sdl3::Window) {
    let mut stores = STORES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(store) = stores
        .as_mut()
        .and_then(|stores| stores.get_mut(&(sdl as usize)))
    {
        store.touch();
    }
}

/// Recompose the whole of a window, whose background changed.
/// `windowBackgroundColor` (0.93 grey, opaque) as premultiplied BGRA.
const DEFAULT_BACKGROUND: [u8; 4] = [237, 237, 237, 255];

pub(crate) fn window_background_changed(window: Id) {
    // SAFETY: a window this crate made.
    let sdl = unsafe { crate::window::sdl_window(window) };
    if sdl.is_null() {
        return;
    }
    if let Some((width, height)) = sdl3::video::window_size_in_pixels(sdl) {
        damage(
            sdl,
            Rect {
                x: 0,
                y: 0,
                width: i64::from(width.max(0)),
                height: i64::from(height.max(0)),
            },
        );
    }
}

/// Have a window's chrome composited again, from `crate::decor` when it
/// repaints it.
pub(crate) fn chrome_changed(sdl: sdl3::Window) {
    let mut windows = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = windows
        .as_mut()
        .and_then(|windows| windows.get_mut(&(sdl as usize)))
    {
        // Zero is "none", so any real generation differs from it.
        state.chrome = 0;
    }
}

/// Draw `source` with its top-left corner at `left`, `top` of `store`,
/// source-over, within `clip`, faded to `opacity`.
fn blend_at(store: &mut Store, source: &Store, left: i64, top: i64, clip: Rect, opacity: f64) {
    if opacity <= 0.0 {
        return;
    }
    // 256 is exact: a scaled channel is then `texel * scale >> 8` with no
    // rounding error at full opacity.
    let scale = (opacity.min(1.0) * 256.0).round() as u32;
    let source_rect = Rect {
        x: left,
        y: top,
        width: i64::from(source.width),
        height: i64::from(source.height),
    };
    let area = clip.intersection(&source_rect);
    if area.is_empty() {
        return;
    }
    let columns = area.width as usize;
    for row in area.y..area.y + area.height {
        let from = ((row - top) as usize * source.width as usize + (area.x - left) as usize) * 4;
        let to = (row as usize * store.width as usize + area.x as usize) * 4;
        let from_row = &source.pixels[from..from + columns * 4];
        let to_row = &mut store.pixels[to..to + columns * 4];
        for (target, texel) in to_row
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(from_row.as_chunks::<4>().0)
        {
            let texel = match scale {
                256 => *texel,
                _ => texel.map(|channel| ((u32::from(channel) * scale) >> 8) as u8),
            };
            match texel[3] {
                255 => target.copy_from_slice(&texel),
                0 => {}
                alpha => {
                    let keep = 255 - u32::from(alpha);
                    for channel in 0..4 {
                        let blended =
                            u32::from(texel[channel]) + u32::from(target[channel]) * keep / 255;
                        target[channel] = blended.min(255) as u8;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
static TEST_CPU_SURFACE_FALLBACKS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn cpu_fallback_calls_for_tests(window: sdl3::Window) -> usize {
    TEST_CPU_SURFACE_FALLBACKS
        .lock()
        .unwrap()
        .iter()
        .filter(|&&key| key == window as usize)
        .count()
}

#[cfg(test)]
pub(crate) fn queue_dirty_store_for_tests(window: sdl3::Window) {
    let (width, height) = sdl3::video::window_size_in_pixels(window).unwrap();
    let mut store = Store::new(width.max(1) as u32, height.max(1) as u32);
    store.touch();
    STORES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(window as usize, store);
}

#[cfg(test)]
pub(crate) fn has_store_for_tests(window: sdl3::Window) -> bool {
    STORES
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|stores| stores.contains_key(&(window as usize)))
}

/// The part of the surface to keep when a logical frame excludes the rest, or
/// `None` for a window without one: a windowed surface that outgrew its store
/// during a resize is left as it was, not painted black.
fn software_exterior(clip: Rect, whole: Rect, shown: Rect) -> Option<Rect> {
    (clip != whole).then(|| clip.intersection(&shown))
}

/// Black out obsolete host pixels outside the logical AppKit bounds without
/// changing content/Metal layers within it. `pitch` includes SDL row padding.
fn blackout_software_exterior(
    pixels: &mut [u8],
    pitch: usize,
    width: usize,
    height: usize,
    allowed: Rect,
) {
    let whole = Rect {
        x: 0,
        y: 0,
        width: width as i64,
        height: height as i64,
    };
    let allowed = whole.intersection(&allowed);
    for band in whole.subtract(&[allowed]) {
        for y in band.y as usize..(band.y + band.height) as usize {
            let start = y * pitch + band.x as usize * 4;
            for pixel in pixels[start..start + band.width as usize * 4]
                .as_chunks_mut::<4>()
                .0
            {
                pixel.copy_from_slice(&[0, 0, 0, 255]);
            }
        }
    }
}

fn present_dirty() {
    let presenter = *PRESENTER.lock().unwrap_or_else(|e| e.into_inner());
    let mut stores = STORES.lock().unwrap_or_else(|e| e.into_inner());
    let Some(stores) = stores.as_mut() else {
        return;
    };
    for (&window, store) in stores
        .iter_mut()
        .filter(|(window, store)| store.dirty || dump_overdue(**window as sdl3::Window))
    {
        let window = window as sdl3::Window;
        store.dirty = false;
        let backing = store;
        let (composed, gpu_overlays) = compose(window, backing);
        let store = composed.as_ref().unwrap_or(backing);
        dump(window, store);
        let holes = WINDOWS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|windows| windows.get(&(window as usize)))
            .map(|state| state.holes.clone())
            .unwrap_or_default();
        let outcome = presenter.map_or(PresentOutcome::Fallback, |present| {
            present(window, store, &holes, &gpu_overlays)
        });
        if outcome == PresentOutcome::Deferred {
            // Preserve the original store and its overlays for a later turn;
            // a slow FIFO acquisition must not hold up input for other windows.
            backing.dirty = true;
            continue;
        }
        if outcome == PresentOutcome::DisplayedEarlier {
            presented_through_gpu(window, true);
            backing.dirty = true;
            continue;
        }
        let presented = outcome == PresentOutcome::Displayed;
        presented_through_gpu(window, presented);
        if presented {
            continue;
        }
        // The dma-buf overlays are left out of this frame; `shows_dmabufs`
        // now says no, and their views send pixels from the next one.
        // The guest's own GL context owns this window's surface; see
        // [`crate::window::draws_with_opengl`]. There is nothing to fall back
        // to, so the store is left unshown rather than taking the context away.
        if crate::window::draws_with_opengl(window) {
            continue;
        }
        let transparent = sdl3::video::window_flags(window) & sdl3::video::flags::TRANSPARENT != 0;
        let chrome = crate::decor::pixels(window).filter(|_| transparent);
        // A layer that draws into the surface itself keeps its last frame
        // there; the store holds only background where it is.
        let kept: Vec<Rect> = {
            let layers = SURFACE_LAYERS.lock().unwrap_or_else(|e| e.into_inner());
            holes
                .iter()
                .filter(|hole| layers.contains(&(window as usize, hole.layer)))
                .map(|hole| hole.rect)
                .collect()
        };
        #[cfg(test)]
        TEST_CPU_SURFACE_FALLBACKS
            .lock()
            .unwrap()
            .push(window as usize);
        sdl3::video::present_window(window, |surface| {
            let rows = store.height.min(surface.height.max(0) as u32) as usize;
            let columns = store.width.min(surface.width.max(0) as u32) as usize;
            let shown = Rect {
                x: 0,
                y: 0,
                width: columns as i64,
                height: rows as i64,
            };
            let whole = Rect {
                x: 0,
                y: 0,
                width: i64::from(surface.width.max(0)),
                height: i64::from(surface.height.max(0)),
            };
            let clip = logical_pixel_bounds(window, whole);
            let logical = clip.intersection(&shown);
            if let Some(allowed) = software_exterior(clip, whole, shown) {
                // SAFETY: SDL owns pitch * height bytes during this callback.
                let pixels = unsafe {
                    std::slice::from_raw_parts_mut(
                        surface.pixels,
                        surface.pitch as usize * surface.height.max(0) as usize,
                    )
                };
                blackout_software_exterior(
                    pixels,
                    surface.pitch as usize,
                    surface.width.max(0) as usize,
                    surface.height.max(0) as usize,
                    allowed,
                );
            }
            for piece in logical.subtract(&kept) {
                let (left, width) = (piece.x as usize, piece.width as usize);
                for row in piece.y as usize..(piece.y + piece.height) as usize {
                    let from = (row * store.width as usize + left) * 4;
                    // SAFETY: SDL reported `pitch` bytes per row for `height`
                    // rows, and the piece lies inside `rows` by `columns`.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            store.pixels[from..from + width * 4].as_ptr(),
                            surface.pixels.add(row * surface.pitch as usize + left * 4),
                            width * 4,
                        )
                    };
                }
            }
            if transparent && rows > 0 && columns > 0 {
                // SAFETY: SDL reported `pitch` bytes per row for `height` rows.
                let pixels = unsafe {
                    std::slice::from_raw_parts_mut(surface.pixels, surface.pitch as usize * rows)
                };
                let pitch = surface.pitch as usize;
                let chrome_rows = chrome.as_ref().map_or(0, |chrome| chrome.height as usize);
                make_opaque(pixels, columns, rows, pitch, chrome_rows);
                if let Some(chrome) = chrome.as_ref().filter(|chrome| chrome.radius > 0.0) {
                    chrome::corners::clip(pixels, columns, rows, pitch, chrome.radius, false, true);
                }
            }
            rows > 0 && columns > 0
        });
    }
}

/// Give every pixel from row `from` down full alpha.
///
/// A window with room for rounded corners shows its alpha, which a Mac's
/// window content never does: what no view drew is transparent in the
/// store, and there it would be a hole through to the desktop. Premultiplied
/// pixels given full alpha are as if drawn over black, which is what an
/// opaque window shows for them too.
fn make_opaque(pixels: &mut [u8], width: usize, height: usize, pitch: usize, from: usize) {
    for row in from..height {
        let start = row * pitch;
        for pixel in pixels[start..start + width * 4].as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
    }
}

/// Pixels a view shows above its window's store.
///
/// A view whose content comes from outside the window's drawing - a web view,
/// whose page another toolkit renders - is a layer the window server stacks
/// above the views that draw into the store. Writing it into the store instead
/// would lose it at the next flush of the view underneath, which repaints its
/// whole rectangle without knowing a subview is there.
pub struct Overlay {
    pub width: u32,
    pub height: u32,
    pub picture: Picture,
}

pub enum Picture {
    /// Premultiplied B, G, R, A, top row first.
    Pixels(Vec<u8>),
    /// Drawn by the presenter; see [`shows_dmabufs`].
    Dmabuf(std::sync::Arc<Dmabuf>),
}

/// Keyed by the view's address, in the order the overlays were first set.
static OVERLAYS: Mutex<Vec<(usize, Overlay)>> = Mutex::new(Vec::new());

/// Show `overlay` over `view`'s rectangle from the next present on.
///
/// The view must call [`remove_overlay`] before it is deallocated.
pub fn set_overlay(view: Id, overlay: Overlay) {
    {
        let mut overlays = OVERLAYS.lock().unwrap_or_else(|e| e.into_inner());
        match overlays.iter_mut().find(|(key, _)| *key == view as usize) {
            Some((_, existing)) => *existing = overlay,
            None => overlays.push((view as usize, overlay)),
        }
    }
    mark_window_of(view);
}

pub fn remove_overlay(view: Id) {
    let removed = {
        let mut overlays = OVERLAYS.lock().unwrap_or_else(|e| e.into_inner());
        let before = overlays.len();
        overlays.retain(|(key, _)| *key != view as usize);
        overlays.len() != before
    };
    if removed {
        mark_window_of(view);
    }
}

/// The rectangles, in window pixels, of the overlays stacked above `layer`
/// in `sdl`'s window, which that layer's presents must not draw over.
pub fn covered(sdl: sdl3::Window, layer: Id) -> Vec<Rect> {
    let windows = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    windows
        .as_ref()
        .and_then(|windows| windows.get(&(sdl as usize)))
        .and_then(|state| state.covered.iter().find(|(key, _)| *key == layer as usize))
        .map(|(_, rects)| rects.clone())
        .unwrap_or_default()
}

/// Have `view`'s window presented again, composing it if nothing has been:
/// an overlay on a window nothing else draws into still has to reach the
/// screen.
fn mark_window_of(view: Id) {
    // SAFETY: a live view, which both callers are passed.
    let Some(frame) = (unsafe { crate::view::presentation_frame(view) }) else {
        return;
    };
    damage(
        frame.window,
        Rect {
            x: i64::from(frame.x),
            y: i64::from(frame.y),
            width: i64::from(frame.width),
            height: i64::from(frame.height),
        },
    );
}

/// The store with every visible pixel overlay of `window` drawn over it, or
/// `None` when there is nothing to draw and the store can be presented as it
/// is; and the dma-buf overlays, which only the presenter can draw.
fn compose(window: sdl3::Window, store: &Store) -> (Option<Store>, Vec<GpuOverlay>) {
    let overlays = OVERLAYS.lock().unwrap_or_else(|e| e.into_inner());
    let mut composed: Option<Store> = None;
    let mut gpu = Vec::new();
    let allowed = logical_pixel_bounds(
        window,
        Rect {
            x: 0,
            y: 0,
            width: i64::from(store.width),
            height: i64::from(store.height),
        },
    );
    for (view, overlay) in overlays.iter() {
        let view = *view as Id;
        // SAFETY: an overlay is removed before its view is deallocated.
        let shown = unsafe { crate::view::window_of(view) };
        if !crate::window::is_shown(shown) || crate::view::is_hidden_or_has_hidden_ancestor(view) {
            continue;
        }
        let Some(frame) = (unsafe { crate::view::presentation_frame(view) }) else {
            continue;
        };
        if frame.window != window {
            continue;
        }
        let original = Rect {
            x: i64::from(frame.x),
            y: i64::from(frame.y),
            width: i64::from(frame.width),
            height: i64::from(frame.height),
        };
        if original.intersection(&allowed).is_empty() {
            continue;
        }
        if let Picture::Dmabuf(picture) = &overlay.picture {
            gpu.push(GpuOverlay {
                picture: picture.clone(),
                // Keep original placement for texture sampling; the final
                // Vulkan presentation masks pixels beyond the logical window.
                rect: original,
            });
            continue;
        }
        let target = composed.get_or_insert_with(|| Store {
            width: store.width,
            height: store.height,
            pixels: store.pixels.clone(),
            dirty: false,
            // An overlay that changes damages the store under it.
            generation: store.generation,
        });
        blend_overlay_clipped(
            target,
            overlay,
            frame.x,
            frame.y,
            frame.width,
            frame.height,
            allowed,
        );
    }
    (composed, gpu)
}

/// Draw `overlay` scaled into the rectangle at `left`, `top` of `store`,
/// source-over, nearest-neighbour.
#[cfg(test)]
fn blend_overlay(
    store: &mut Store,
    overlay: &Overlay,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
) {
    blend_overlay_clipped(
        store,
        overlay,
        left,
        top,
        width,
        height,
        Rect {
            x: 0,
            y: 0,
            width: i64::from(store.width),
            height: i64::from(store.height),
        },
    );
}

fn blend_overlay_clipped(
    store: &mut Store,
    overlay: &Overlay,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    allowed: Rect,
) {
    let Picture::Pixels(pixels) = &overlay.picture else {
        return;
    };
    let (source_width, source_height) = (overlay.width as usize, overlay.height as usize);
    if source_width == 0
        || source_height == 0
        || width == 0
        || height == 0
        || pixels.len() < source_width * source_height * 4
    {
        return;
    }
    let clipped = Rect {
        x: i64::from(left),
        y: i64::from(top),
        width: i64::from(width),
        height: i64::from(height),
    }
    .intersection(&allowed)
    .intersection(&Rect {
        x: 0,
        y: 0,
        width: i64::from(store.width),
        height: i64::from(store.height),
    });
    let x0 = clipped.x as usize;
    let y0 = clipped.y as usize;
    let x1 = (clipped.x + clipped.width) as usize;
    let y1 = (clipped.y + clipped.height) as usize;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let columns: Vec<usize> = (x0..x1)
        .map(|x| {
            ((x as i64 - i64::from(left)) as usize * source_width / width as usize)
                .min(source_width - 1)
        })
        .collect();
    for y in y0..y1 {
        let row = ((y as i64 - i64::from(top)) as usize * source_height / height as usize)
            .min(source_height - 1);
        let from_row = &pixels[row * source_width * 4..(row + 1) * source_width * 4];
        let base = y * store.width as usize * 4;
        let to_row = &mut store.pixels[base + x0 * 4..base + x1 * 4];
        for (target, &column) in to_row.as_chunks_mut::<4>().0.iter_mut().zip(&columns) {
            let texel = &from_row[column * 4..column * 4 + 4];
            let alpha = u32::from(texel[3]);
            if alpha == 255 {
                target.copy_from_slice(texel);
                continue;
            }
            for channel in 0..4 {
                let blended =
                    u32::from(texel[channel]) + u32::from(target[channel]) * (255 - alpha) / 255;
                target[channel] = blended.min(255) as u8;
            }
        }
    }
}

/// `ROSEMARY_STORE_OUT=DIR`: write each window's store as a binary PPM when it
/// is presented, at most once a second per window, overwriting the last.
///
/// The Metal frame dump sees only layers; this is the half of a window that
/// views drew on the CPU, for a session with no screen to look at.
/// When each window's store was last written, and whether a present since
/// then went unwritten.
static DUMPED: Mutex<Vec<(usize, std::time::Instant, bool)>> = Mutex::new(Vec::new());

/// Whether a store should be presented again only so that its last frame is
/// written: without this, a window that stops changing within a second of
/// its last dump leaves a stale picture of itself behind.
fn dump_overdue(window: sdl3::Window) -> bool {
    std::env::var_os("ROSEMARY_STORE_OUT").is_some()
        && DUMPED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|(key, at, skipped)| {
                *key == window as usize && *skipped && at.elapsed().as_secs_f64() >= 1.0
            })
}

fn dump(window: sdl3::Window, store: &Store) {
    use std::time::Instant;
    let Some(directory) = std::env::var_os("ROSEMARY_STORE_OUT") else {
        return;
    };
    let mut last = DUMPED.lock().unwrap_or_else(|e| e.into_inner());
    match last.iter_mut().find(|(key, _, _)| *key == window as usize) {
        Some((_, at, skipped)) if at.elapsed().as_secs_f64() < 1.0 => {
            *skipped = true;
            return;
        }
        Some((_, at, skipped)) => {
            *at = Instant::now();
            *skipped = false;
        }
        None => last.push((window as usize, Instant::now(), false)),
    }
    let mut out = format!("P6\n{} {}\n255\n", store.width, store.height).into_bytes();
    for texel in store.pixels.as_chunks::<4>().0 {
        out.extend_from_slice(&[texel[2], texel[1], texel[0]]);
    }
    let name = format!("store-{:x}.ppm", window as usize);
    let _ = std::fs::write(std::path::Path::new(&directory).join(name), out);
}

unsafe extern "C" fn lock_focus_if_can_draw(receiver: Id, _sel: Sel) -> u8 {
    if crate::view::is_hidden_or_has_hidden_ancestor(receiver) {
        return 0;
    }
    push_focus(receiver) as u8
}

unsafe extern "C" fn lock_focus(receiver: Id, _sel: Sel) {
    push_focus(receiver);
}

unsafe extern "C" fn unlock_focus(_receiver: Id, _sel: Sel) {
    pop_focus();
}

unsafe extern "C" fn focus_view(_class: Id, _sel: Sel) -> Id {
    current_focus().map_or(std::ptr::null_mut(), |focus| focus.view)
}

/// `-getRectsBeingDrawn:count:`: the whole view, which is what [`display_view`]
/// draws.
unsafe extern "C" fn get_rects_being_drawn(
    receiver: Id,
    _sel: Sel,
    rects: *mut *const NSRect,
    count: *mut isize,
) {
    thread_local! {
        static DRAWN: RefCell<NSRect> = const { RefCell::new(NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSSize { width: 0.0, height: 0.0 },
        }) };
    }
    let size = current_focus()
        .filter(|focus| focus.view == receiver)
        .map_or(NSSize::default(), |focus| NSSize {
            width: focus.width / focus.density,
            height: focus.height / focus.density,
        });
    DRAWN.with(|drawn| {
        *drawn.borrow_mut() = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size,
        };
        if !rects.is_null() {
            // SAFETY: the caller's out-pointer; the rect lives as long as the
            // thread, past the `-drawRect:` it is read in.
            unsafe { *rects = drawn.as_ptr() };
        }
    });
    if !count.is_null() {
        // SAFETY: the caller's out-pointer.
        unsafe { *count = 1 };
    }
}

/// A context explicitly selected on this thread, and the focus depth it was
/// selected at. AppKit installs its own context for each `-drawRect:`, so an
/// override only applies at the depth it was set; `None` leaves the focused
/// view's context available.
#[derive(Clone, Copy)]
struct ContextOverride {
    context: Id,
    depth: usize,
}

struct CurrentGraphicsContext(Cell<Option<ContextOverride>>);

impl Drop for CurrentGraphicsContext {
    fn drop(&mut self) {
        if let Some(held) = self.0.get().filter(|held| !held.context.is_null()) {
            objc::arc::release(held.context);
        }
    }
}

thread_local! {
    static CURRENT_GRAPHICS_CONTEXT: CurrentGraphicsContext =
        const { CurrentGraphicsContext(Cell::new(None)) };
}

fn focus_depth() -> usize {
    FOCUS.with(|stack| stack.borrow().len())
}

fn set_thread_current_context(context: Id) {
    let depth = focus_depth();
    let _ = CURRENT_GRAPHICS_CONTEXT.try_with(|current| {
        let retained = if context.is_null() {
            context
        } else {
            objc::arc::retain(context)
        };
        let previous = current.0.replace(Some(ContextOverride {
            context: retained,
            depth,
        }));
        if let Some(previous) = previous.filter(|previous| !previous.context.is_null()) {
            objc::arc::release(previous.context);
        }
    });
}

/// Drop an override whose focus has ended, so a stale context neither shadows
/// later drawing nor outlives the window it came from.
fn release_context_override_above(depth: usize) {
    let _ = CURRENT_GRAPHICS_CONTEXT.try_with(|current| {
        if let Some(held) = current.0.get().filter(|held| held.depth > depth) {
            current.0.set(None);
            if !held.context.is_null() {
                objc::arc::release(held.context);
            }
        }
    });
}

unsafe extern "C" fn set_current_context(_class: Id, _sel: Sel, context: Id) {
    set_thread_current_context(context);
}

unsafe extern "C" fn current_context(_class: Id, _sel: Sel) -> Id {
    let depth = focus_depth();
    match CURRENT_GRAPHICS_CONTEXT.try_with(|current| current.0.get()) {
        Ok(Some(held)) if held.depth == depth => return held.context,
        Ok(_) => {}
        // Avoid panicking through Objective-C if Cocoa calls back as this
        // thread's local graphics context is being destroyed.
        Err(_) => return std::ptr::null_mut(),
    }
    if current_focus().is_none() {
        return std::ptr::null_mut();
    }
    shared_instance(c"NSGraphicsContext")
}

fn shared_instance(name: &'static std::ffi::CStr) -> Id {
    static INSTANCES: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
    let class = objc::class::lookup(name).expect("declared during bootstrap");
    let key = objc::class::ptr(class) as usize;
    let mut instances = INSTANCES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, object)) = instances.iter().find(|(k, _)| *k == key) {
        return *object as Id;
    }
    let object = objc::object::create_instance(class, 0);
    objc::arc::retain(object);
    instances.push((key, object as usize));
    object
}

/// Graphics contexts made over a caller's `CGContext`, by object: the port,
/// retained, and whether the context is flipped.
static WRAPPED: Mutex<Vec<(usize, usize, bool)>> = Mutex::new(Vec::new());

fn wrapped(context: Id) -> Option<(usize, bool)> {
    WRAPPED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(object, _, _)| *object == context as usize)
        .map(|(_, port, flipped)| (*port, *flipped))
}

/// `+graphicsContextWithGraphicsPort:flipped:` and its `CGContext` spelling:
/// a context that draws into the port given rather than the focused view.
unsafe extern "C" fn context_with_port(
    class: Id,
    _sel: Sel,
    port: *const c_void,
    flipped: u8,
) -> Id {
    if port.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the class this method is installed on.
    let class = unsafe { objc::class::from_ptr(class as *const objc::class::Class) }
        .or_else(|| objc::class::lookup(c"NSGraphicsContext"))
        .expect("NSGraphicsContext is declared");
    let context = objc::object::create_instance(class, 0);
    // SAFETY: the caller's live `CGContext`, held for this object's life.
    let port = unsafe { crate::cgcontext::CGContextRetain(port) };
    WRAPPED.lock().unwrap_or_else(|e| e.into_inner()).push((
        context as usize,
        port as usize,
        flipped != 0,
    ));
    objc::arc::autorelease(context)
}

unsafe extern "C" fn context_dealloc(receiver: Id, _sel: Sel) {
    let port = {
        let mut table = WRAPPED.lock().unwrap_or_else(|e| e.into_inner());
        let at = table
            .iter()
            .position(|(object, _, _)| *object == receiver as usize);
        at.map(|at| table.remove(at).1)
    };
    if let Some(port) = port {
        // SAFETY: the reference `context_with_port` took.
        unsafe { crate::cgcontext::CGContextRelease(port as *const c_void) };
    }
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn context_is_flipped(receiver: Id, _sel: Sel) -> u8 {
    if let Some((_, flipped)) = wrapped(receiver) {
        return u8::from(flipped);
    }
    current_focus().is_some_and(|focus| focus.flipped) as u8
}

unsafe extern "C" fn cg_context(receiver: Id, _sel: Sel) -> *const c_void {
    if let Some((port, _)) = wrapped(receiver) {
        return port as *const c_void;
    }
    crate::cgcontext::focus_context()
}

unsafe extern "C" fn ignored(_receiver: Id, _sel: Sel) {}

unsafe extern "C" fn ignored_integer(_receiver: Id, _sel: Sel, _value: isize) {}

unsafe extern "C" fn zero(_receiver: Id, _sel: Sel) -> isize {
    0
}

unsafe extern "C" fn shared_color_space(_receiver: Id, _sel: Sel) -> Id {
    shared_instance(c"NSColorSpace")
}

/// `-[NSColorSpace CGColorSpace]`, which the caller does not own.
unsafe extern "C" fn cg_color_space(_receiver: Id, _sel: Sel) -> *const c_void {
    static SPACE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    // SAFETY: creates an object this runtime keeps for the process.
    *SPACE.get_or_init(|| unsafe { crate::graphics::CGColorSpaceCreateDeviceRGB() } as usize)
        as *const c_void
}

pub(crate) fn install() {
    let view = objc::class::lookup(c"NSView").expect("declared before its methods");
    methods!(view, {
        "lockFocusIfCanDraw" => lock_focus_if_can_draw, c"c16@0:8";
        "lockFocus" => lock_focus, c"v16@0:8";
        "unlockFocus" => unlock_focus, c"v16@0:8";
        "getRectsBeingDrawn:count:" => get_rects_being_drawn, c"v32@0:8^^{CGRect}16^q24";
    });
    class_methods!(view, {
        "focusView" => focus_view, c"@16@0:8";
    });

    let context = objc::class::lookup(c"NSGraphicsContext").expect("declared");
    // Saving and restoring the graphics state is `crate::bezier`'s.
    class_methods!(context, {
        "currentContext" => current_context, c"@16@0:8";
        "setCurrentContext:" => set_current_context, c"v24@0:8@16";
        "graphicsContextWithGraphicsPort:flipped:" => context_with_port, c"@28@0:8^v16c24";
        "graphicsContextWithCGContext:flipped:" => context_with_port, c"@28@0:8^v16c24";
    });
    methods!(context, {
        "dealloc" => context_dealloc, c"v16@0:8";
        "isFlipped" => context_is_flipped, c"c16@0:8";
        "CGContext" => cg_context, c"^v16@0:8";
        "graphicsPort" => cg_context, c"^v16@0:8";
        "flushGraphics" => ignored, c"v16@0:8";
        "compositingOperation" => zero, c"q16@0:8";
        "setCompositingOperation:" => ignored_integer, c"v24@0:8q16";
    });

    let space = objc::class::lookup(c"NSColorSpace").expect("declared");
    class_methods!(space, {
        "sRGBColorSpace" => shared_color_space, c"@16@0:8";
        "deviceRGBColorSpace" => shared_color_space, c"@16@0:8";
        "genericRGBColorSpace" => shared_color_space, c"@16@0:8";
    });
    methods!(space, {
        "CGColorSpace" => cg_color_space, c"^v16@0:8";
    });
    let window = objc::class::lookup(c"NSWindow").expect("declared");
    methods!(window, {
        "colorSpace" => shared_color_space, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_backing_store_survives_the_pump_while_another_window_progresses() {
        fn presenter(
            window: sdl3::Window,
            _: &Store,
            _: &[Hole],
            _: &[GpuOverlay],
        ) -> PresentOutcome {
            match window as usize {
                0x71a1 => PresentOutcome::Deferred,
                0x71c3 => PresentOutcome::DisplayedEarlier,
                _ => PresentOutcome::Displayed,
            }
        }
        let before_presenter = PRESENTER.lock().expect("presenter").replace(presenter);
        let mut deferred = Store::new(2, 2);
        deferred.dirty = true;
        let mut independent = Store::new(2, 2);
        independent.dirty = true;
        let mut uploading = Store::new(2, 2);
        uploading.dirty = true;
        let before_stores = {
            let mut stores = STORES.lock().expect("stores");
            std::mem::replace(
                &mut *stores,
                Some(HashMap::from([
                    (0x71a1usize, deferred),
                    (0x71b2usize, independent),
                    (0x71c3usize, uploading),
                ])),
            )
        };
        present_dirty();
        {
            let stores = STORES.lock().expect("stores");
            let stores = stores.as_ref().expect("stores");
            assert!(
                stores[&0x71a1].dirty,
                "busy A must stay dirty for another event turn"
            );
            assert!(
                !stores[&0x71b2].dirty,
                "ready B must be presented on this turn"
            );
            assert!(
                stores[&0x71c3].dirty,
                "C's current store has not reached the screen yet"
            );
        }
        assert!(
            shows_dmabufs(0x71c3 as sdl3::Window),
            "C's earlier store was shown through the GPU"
        );
        *STORES.lock().expect("stores") = before_stores;
        *PRESENTER.lock().expect("presenter") = before_presenter;
        GPU_WINDOWS
            .lock()
            .expect("gpu windows")
            .retain(|(key, _)| *key != 0x71b2 && *key != 0x71c3);
    }

    fn set_thread_context_from_class(context: Id) {
        let class = objc::class::lookup(c"NSGraphicsContext").expect("declared");
        // SAFETY: `msg::entry` has the registered class-method ABI below.
        let send: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        // SAFETY: the class and selector identify the registered setter.
        unsafe {
            send(
                objc::class::ptr(class).cast(),
                objc::sel::get("setCurrentContext:"),
                context,
            )
        }
    }

    fn thread_context_from_class() -> Id {
        let class = objc::class::lookup(c"NSGraphicsContext").expect("declared");
        // SAFETY: `msg::entry` has the registered class-method ABI below.
        let send: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        // SAFETY: the class and selector identify the registered getter.
        unsafe {
            send(
                objc::class::ptr(class).cast(),
                objc::sel::get("currentContext"),
            )
        }
    }

    #[test]
    fn setting_current_graphics_context_retains_it_until_cleared() {
        crate::bootstrap();
        let pool = unsafe { objc::api::objc_autoreleasePoolPush() };
        let class = objc::class::lookup(c"NSGraphicsContext").expect("declared");
        let context = objc::arc::autorelease(objc::object::create_instance(class, 0));

        set_thread_context_from_class(context);
        assert_eq!(thread_context_from_class(), context);

        // The current-context state owns a reference beyond the autorelease
        // pool that would otherwise release this object.
        unsafe { objc::api::objc_autoreleasePoolPop(pool) };
        assert_eq!(thread_context_from_class(), context);

        set_thread_context_from_class(std::ptr::null_mut());
        assert!(thread_context_from_class().is_null());
    }

    #[test]
    fn explicit_nil_does_not_disable_the_focused_context_later() {
        crate::bootstrap();
        set_thread_context_from_class(std::ptr::null_mut());
        assert!(thread_context_from_class().is_null());

        begin_offscreen(0x7e57, 4, 4, 1.0, false);
        assert!(!thread_context_from_class().is_null());
        let class = objc::class::lookup(c"NSGraphicsContext").expect("declared");
        let bitmap = objc::object::create_instance(class, 0);
        set_thread_context_from_class(bitmap);
        assert_eq!(thread_context_from_class(), bitmap);
        end_offscreen(0x7e57);

        // The override ended with its focus.
        assert!(thread_context_from_class().is_null());
        begin_offscreen(0x7e58, 4, 4, 1.0, false);
        assert!(!thread_context_from_class().is_null());
        assert_ne!(thread_context_from_class(), bitmap);
        end_offscreen(0x7e58);
    }

    #[test]
    fn current_graphics_context_is_per_thread() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSGraphicsContext").expect("declared");
        let context = objc::object::create_instance(class, 0);
        set_thread_context_from_class(context);
        std::thread::spawn(|| assert!(thread_context_from_class().is_null()))
            .join()
            .unwrap();
        assert_eq!(thread_context_from_class(), context);
        set_thread_context_from_class(std::ptr::null_mut());
    }

    /// A view of `NSView`'s, and one of a flipped subclass, so a tree can be
    /// placed without a window.
    fn view(frame: NSRect, flipped: bool) -> Id {
        crate::bootstrap();
        let name = match flipped {
            true => c"RosemaryBackingFlippedView",
            false => c"NSView",
        };
        let class = objc::class::lookup(name).unwrap_or_else(|| {
            let class = corefoundation::base::register(
                name,
                objc::class::lookup(c"NSView"),
                crate::view::INSTANCE_SIZE,
                false,
            );
            methods!(class, {
                "isFlipped" => flipped_yes, c"c16@0:8";
            });
            class
        });
        let object = objc::object::create_instance(class, 0);
        // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)` on a view.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, NSRect) -> Id =
                std::mem::transmute(objc::msg::entry());
            send(object, objc::sel::get("initWithFrame:"), frame)
        }
    }

    unsafe extern "C" fn flipped_yes(_receiver: Id, _sel: Sel) -> u8 {
        1
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect {
            origin: NSPoint { x, y },
            size: NSSize { width, height },
        }
    }

    fn add(parent: Id, child: Id) {
        // SAFETY: `-addSubview:` is `void (id, SEL, id)` on views.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            send(parent, objc::sel::get("addSubview:"), child);
        }
    }

    /// Where the compositor puts each view, from a flipped tree: a subview at
    /// the top of its superview is at the top of the window, and one that
    /// hangs over an edge shows only the part inside.
    #[test]
    fn places_a_flipped_tree_from_the_top_of_the_window() {
        let content = view(rect(0.0, 0.0, 200.0, 100.0), true);
        let ribbon = view(rect(0.0, 0.0, 200.0, 20.0), true);
        let overhanging = view(rect(150.0, 10.0, 100.0, 5.0), true);
        add(content, ribbon);
        add(ribbon, overhanging);

        let mut placed = Vec::new();
        let whole = Rect {
            x: 0,
            y: 0,
            width: 200,
            height: 100,
        };
        place_view(
            content,
            rect(0.0, 0.0, 200.0, 100.0),
            false,
            whole,
            Geometry {
                window_height: 100.0,
                host_points: (200, 100),
                host_pixels: (200, 100),
            },
            0,
            &mut placed,
        );
        let rects: Vec<Rect> = placed.iter().map(|p| p.rect).collect();
        assert_eq!(rects[0], whole);
        assert_eq!(
            rects[1],
            Rect {
                x: 0,
                y: 0,
                width: 200,
                height: 20
            }
        );
        assert_eq!(
            rects[2],
            Rect {
                x: 150,
                y: 10,
                width: 100,
                height: 5
            }
        );
        // Clipped to its superview, which is what keeps a scrolled view's
        // children inside it.
        assert_eq!(
            placed[2].clip,
            Rect {
                x: 150,
                y: 10,
                width: 50,
                height: 5
            }
        );
    }

    /// A hidden view is placed nowhere, and takes its subviews with it.
    #[test]
    fn a_hidden_view_is_not_placed() {
        let content = view(rect(0.0, 0.0, 10.0, 10.0), true);
        let hidden = view(rect(0.0, 0.0, 10.0, 5.0), true);
        let under = view(rect(0.0, 0.0, 10.0, 2.0), true);
        add(content, hidden);
        add(hidden, under);
        // SAFETY: `-setHidden:` is `void (id, SEL, BOOL)` on a view.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, u8) = std::mem::transmute(objc::msg::entry());
            send(hidden, objc::sel::get("setHidden:"), 1);
        }
        let mut placed = Vec::new();
        place_view(
            content,
            rect(0.0, 0.0, 10.0, 10.0),
            false,
            Rect {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
            Geometry {
                window_height: 10.0,
                host_points: (10, 10),
                host_pixels: (10, 10),
            },
            0,
            &mut placed,
        );
        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].view, content);
    }

    /// A subview's pixels are over its superview's, whichever drew last: one
    /// store per window would instead show whichever flushed last.
    #[test]
    fn a_subviews_pixels_are_over_its_superviews() {
        let mut window = store_of(2, 1);
        let mut parent = store_of(2, 1);
        parent.pixels = vec![1, 1, 1, 255, 1, 1, 1, 255];
        let mut child = store_of(1, 1);
        child.pixels = vec![9, 9, 9, 255];
        let whole = Rect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        blend_at(&mut window, &parent, 0, 0, whole, 1.0);
        blend_at(&mut window, &child, 1, 0, whole, 1.0);
        assert_eq!(window.pixels, [1, 1, 1, 255, 9, 9, 9, 255]);
    }

    /// `-alphaValue` fades a view's pixels into what is under them.
    #[test]
    fn a_translucent_views_pixels_are_faded_over_the_store() {
        let mut window = store_of(2, 1);
        window.pixels = vec![0, 0, 200, 255, 0, 0, 200, 255];
        let mut view = store_of(2, 1);
        view.pixels = vec![200, 0, 0, 255, 200, 0, 0, 255];
        let whole = Rect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        blend_at(&mut window, &view, 0, 0, whole, 0.5);
        assert_eq!(window.pixels[..4], [100, 0, 100, 255]);
        let before = window.pixels.clone();
        blend_at(&mut window, &view, 0, 0, whole, 0.0);
        assert_eq!(window.pixels, before);
    }

    fn store_of(width: u32, height: u32) -> Store {
        Store {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
            dirty: false,
            generation: 0,
        }
    }

    #[test]
    fn copies_a_flipped_view_upright() {
        // Two rows: red on top, blue below, as Qt's image holds them.
        let source = [0, 0, 255, 255, 255, 0, 0, 255];
        let focus = Focus {
            view: std::ptr::null_mut(),
            window: std::ptr::null_mut(),
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 2.0,
            density: 1.0,
            flipped: true,
            offscreen: false,
        };
        let size = NSSize {
            width: 1.0,
            height: 2.0,
        };
        let rect = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size,
        };
        FOCUS.with(|stack| stack.borrow_mut().push(focus));
        let mut store = store_of(1, 2);
        draw_into(&mut store, &source, 1, 2, size, rect, rect, 1, 1.0, true);
        assert_eq!(store.pixels, source);
        let mut mirrored = store_of(1, 2);
        draw_into(
            &mut mirrored,
            &source,
            1,
            2,
            size,
            rect,
            rect,
            1,
            1.0,
            false,
        );
        assert_eq!(mirrored.pixels, [255, 0, 0, 255, 0, 0, 255, 255]);
        FOCUS.with(|stack| stack.borrow_mut().clear());
    }

    #[test]
    fn fractional_backing_fallback_uses_the_same_right_and_bottom_pixel_edges() {
        let clip = crate::window::pixel_clip_from_surface(
            NSRect {
                origin: NSPoint { x: 0.5, y: 0.5 },
                size: NSSize {
                    width: 1438.5,
                    height: 868.5,
                },
            },
            (1440, 900),
            (1440, 900),
        );
        assert_eq!(
            clip,
            sdl3::Rect {
                x: 1,
                y: 1,
                w: 1438,
                h: 868
            }
        );
        let allowed = Rect {
            x: clip.x.into(),
            y: clip.y.into(),
            width: clip.w.into(),
            height: clip.h.into(),
        };
        let mut pixels = vec![0u8; 1440 * 900 * 4];
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[0, 0, 255, 255]);
        }
        blackout_software_exterior(&mut pixels, 1440 * 4, 1440, 900, allowed);
        let at = |x: usize, y: usize| &pixels[(y * 1440 + x) * 4..(y * 1440 + x + 1) * 4];
        assert_eq!(at(1, 1), &[0, 0, 255, 255]);
        assert_eq!(at(1438, 868), &[0, 0, 255, 255]);
        assert_eq!(at(1439, 868), &[0, 0, 0, 255], "rightmost pixel escaped");
        assert_eq!(at(1438, 869), &[0, 0, 0, 255], "bottom pixel escaped");
        assert_eq!(at(0, 1), &[0, 0, 0, 255]);
        assert_eq!(at(1, 0), &[0, 0, 0, 255]);
        let retina = crate::window::pixel_clip_from_surface(
            NSRect {
                origin: NSPoint { x: 0.25, y: 0.25 },
                size: NSSize {
                    width: 99.25,
                    height: 79.25,
                },
            },
            (100, 80),
            (200, 160),
        );
        assert_eq!(
            retina,
            sdl3::Rect {
                x: 1,
                y: 1,
                w: 198,
                h: 158
            }
        );
    }

    #[test]
    fn only_a_logical_frame_blacks_out_the_software_surface() {
        let whole = Rect {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        // A windowed store lagging a grow-resize.
        let shown = Rect {
            x: 0,
            y: 0,
            width: 700,
            height: 500,
        };
        assert_eq!(software_exterior(whole, whole, shown), None);
        let clip = Rect {
            x: 0,
            y: 31,
            width: 800,
            height: 569,
        };
        assert_eq!(
            software_exterior(clip, whole, shown),
            Some(Rect {
                x: 0,
                y: 31,
                width: 700,
                height: 469,
            })
        );
    }

    #[test]
    fn an_overlay_is_scaled_into_its_rectangle_over_the_store() {
        let mut store = store_of(4, 2);
        store.pixels = vec![9; 4 * 2 * 4];
        // One opaque texel and one fully transparent one, stretched over two
        // store pixels each.
        let overlay = Overlay {
            width: 2,
            height: 1,
            picture: Picture::Pixels(vec![1, 2, 3, 255, 0, 0, 0, 0]),
        };
        blend_overlay(&mut store, &overlay, 0, 1, 4, 1);
        assert_eq!(&store.pixels[..16], &[9; 16]);
        assert_eq!(
            &store.pixels[16..],
            &[1, 2, 3, 255, 1, 2, 3, 255, 9, 9, 9, 9, 9, 9, 9, 9]
        );
    }

    #[test]
    fn an_oversized_overlay_is_clipped_without_rescaling_its_source() {
        let mut store = store_of(6, 3);
        let mut source = Vec::new();
        for row in 0..3u8 {
            for column in 0..6u8 {
                source.extend_from_slice(&[column + 10, row + 20, 37, 255]);
            }
        }
        let overlay = Overlay {
            width: 6,
            height: 3,
            picture: Picture::Pixels(source),
        };
        let allowed = Rect {
            x: 2,
            y: 1,
            width: 3,
            height: 1,
        };
        blend_overlay_clipped(&mut store, &overlay, 0, 0, 6, 3, allowed);
        for y in 0..3 {
            for x in 0..6 {
                let pixel = &store.pixels[(y * 6 + x) * 4..(y * 6 + x + 1) * 4];
                if (2..5).contains(&x) && y == 1 {
                    assert_eq!(pixel, &[(x as u8) + 10, 21, 37, 255]);
                } else {
                    assert_eq!(pixel, &[0, 0, 0, 0]);
                }
            }
        }
    }

    #[test]
    fn a_layer_hole_leaves_out_the_overlay_above_it() {
        let rect = |x, y, width, height| Rect {
            x,
            y,
            width,
            height,
        };
        // A web view filling the lower part of a Metal-drawn panel.
        let hole = rect(1, 510, 278, 338);
        let pieces = hole.subtract(&[rect(1, 546, 278, 302)]);
        assert_eq!(pieces, [rect(1, 510, 278, 36)]);
        // One in the middle leaves a band on every side.
        let pieces = rect(0, 0, 10, 10).subtract(&[rect(2, 3, 4, 5)]);
        let area: i64 = pieces.iter().map(|p| p.width * p.height).sum();
        assert_eq!(area, 100 - 20);
        assert!(
            pieces
                .iter()
                .all(|p| p.intersection(&rect(2, 3, 4, 5)).is_empty())
        );
        // One elsewhere leaves the hole as it was.
        assert_eq!(hole.subtract(&[rect(400, 0, 5, 5)]), [hole]);
    }

    #[test]
    fn an_overlay_past_the_store_edge_is_clipped() {
        let mut store = store_of(2, 2);
        let overlay = Overlay {
            width: 1,
            height: 1,
            picture: Picture::Pixels(vec![7, 7, 7, 255]),
        };
        blend_overlay(&mut store, &overlay, 1, -1, 4, 2);
        assert_eq!(
            store.pixels,
            [0, 0, 0, 0, 7, 7, 7, 255, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn source_over_blends_premultiplied_pixels() {
        let focus = Focus {
            view: std::ptr::null_mut(),
            window: std::ptr::null_mut(),
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            density: 1.0,
            flipped: true,
            offscreen: false,
        };
        FOCUS.with(|stack| stack.borrow_mut().push(focus));
        let size = NSSize {
            width: 1.0,
            height: 1.0,
        };
        let rect = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size,
        };
        let mut store = store_of(1, 1);
        store.pixels = vec![200, 200, 200, 255];
        draw_into(
            &mut store,
            &[0, 0, 128, 128],
            1,
            1,
            size,
            rect,
            rect,
            2,
            1.0,
            true,
        );
        assert_eq!(store.pixels, [99, 99, 227, 255]);
        FOCUS.with(|stack| stack.borrow_mut().clear());
    }
}
