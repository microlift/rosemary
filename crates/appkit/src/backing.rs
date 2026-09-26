use std::cell::RefCell;
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
}

impl Store {
    fn new(width: u32, height: u32) -> Self {
        Store {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
            dirty: false,
        }
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
    fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    fn intersection(&self, other: &Rect) -> Rect {
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
    placed: Vec<(usize, Rect)>,
    holes: Vec<Hole>,
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

/// Presents a window's store, leaving `holes` to the layers named in them;
/// `false` where it cannot, so that SDL's window surface is used instead.
pub type Presenter = fn(sdl3::Window, &Store, &[Hole]) -> bool;

static PRESENTER: Mutex<Option<Presenter>> = Mutex::new(None);

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
}

thread_local! {
    static FOCUS: RefCell<Vec<Focus>> = const { RefCell::new(Vec::new()) };
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
    FOCUS.with(|stack| stack.borrow_mut().pop());
}

fn current_focus() -> Option<Focus> {
    FOCUS.with(|stack| stack.borrow().last().copied())
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
    let (width, height) = (focus.width.max(1.0) as u32, focus.height.max(1.0) as u32);
    let drawn = {
        let mut stores = VIEW_STORES.lock().unwrap_or_else(|e| e.into_inner());
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
    damage(
        focus.window,
        Rect {
            x: drawn.x + focus.x as i64,
            y: drawn.y + focus.y as i64,
            ..drawn
        },
    );
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
    store.dirty = true;
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

/// Mark a view and everything under it, as a window that has nothing valid
/// to show needs.
fn needs_display_in_tree(view: Id, depth: usize) {
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
}

/// Forget a view that is being deallocated, so no pump draws freed memory.
pub(crate) fn forget(view: Id) {
    let key = view as usize;
    NEEDS_DISPLAY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|v| *v != key);
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

/// Draw `view` now, with its focus locked, as `-display` does.
pub(crate) fn display_view(view: Id) {
    let sel_draw = objc::sel::get("drawRect:");
    // SAFETY: a live object this runtime allocated.
    let responds =
        unsafe { objc::object::class_of(view) }.is_some_and(|c| c.method(sel_draw).is_some());
    if !responds || !push_focus(view) {
        return;
    }
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

/// Draw everything marked for display and present every window that changed.
pub fn pump() {
    crate::decor::pump();
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
    for window in crate::window::all() {
        if crate::window::is_shown(window) {
            compose_window(window);
        }
    }
    present_dirty();
}

/// One view's place in the composite.
struct Placed {
    view: Id,
    /// Where it is, in window pixels, and the part of that its ancestors let
    /// show.
    rect: Rect,
    clip: Rect,
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
    let density = match points_wide > 0 {
        true => f64::from(pixel_width) / f64::from(points_wide),
        false => 1.0,
    };
    let whole = Rect {
        x: 0,
        y: 0,
        width: i64::from(pixel_width),
        height: i64::from(pixel_height),
    };
    let content = crate::window::content_view_of(window);
    // The window's base coordinates: not flipped, origin at the bottom, and
    // taller than the content by the chrome above it.
    let base = NSRect {
        origin: NSPoint::default(),
        size: NSSize {
            width: f64::from(points_wide),
            height: (f64::from(points_high) - crate::window::content_inset(window)).max(0.0),
        },
    };
    let geometry = Geometry {
        window_height: f64::from(points_high),
        density,
    };
    place_view(content, base, false, whole, geometry, 0, &mut out);
    out
}

#[derive(Clone, Copy)]
struct Geometry {
    /// The window's height in points, which base coordinates count up from.
    window_height: f64,
    /// Window pixels per point.
    density: f64,
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
    let density = geometry.density;
    let rect = Rect {
        x: (in_window.origin.x * density).round() as i64,
        y: ((geometry.window_height - (y + frame.size.height)) * density).round() as i64,
        width: (frame.size.width * density).round() as i64,
        height: (frame.size.height * density).round() as i64,
    };
    let clip = clip.intersection(&rect);
    out.push(Placed { view, rect, clip });
    let flipped = crate::view::is_flipped(view);
    // SAFETY: a view this crate made.
    for child in unsafe { crate::view::children(view) } {
        place_view(child, in_window, flipped, clip, geometry, depth + 1, out);
    }
}

/// Bring a window's composite up to date with its views: recompose what was
/// drawn over, and everywhere a view appeared, moved or went away.
fn compose_window(window: Id) {
    // SAFETY: a window this crate made.
    let sdl = unsafe { crate::window::sdl_window(window) };
    let Some((width, height)) = sdl3::video::window_size_in_pixels(sdl) else {
        return;
    };
    let (width, height) = (width.max(1) as u32, height.max(1) as u32);
    let placed = place_tree(window, sdl);
    let now: Vec<(usize, Rect)> = placed.iter().map(|p| (p.view as usize, p.clip)).collect();
    let holes: Vec<Hole> = placed
        .iter()
        .filter_map(|p| {
            let layer = crate::view::layer_of(p.view);
            (!layer.is_null() && !p.clip.is_empty()).then_some(Hole {
                layer: layer as usize,
                rect: p.clip,
            })
        })
        .collect();

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
        });
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
    if resized {
        region = whole;
    }
    if state.placed != now {
        // Where each view was and is, for every one that changed.
        for (_, rect) in state.placed.iter().filter(|entry| !now.contains(entry)) {
            region = region.union(rect);
        }
        for (_, rect) in now.iter().filter(|entry| !state.placed.contains(entry)) {
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
    let chrome_generation = chrome.as_ref().map_or(0, |chrome| chrome.generation);
    let chrome_changed =
        std::mem::replace(&mut state.chrome, chrome_generation) != chrome_generation;
    drop(windows);
    let region = region.intersection(&whole);
    if region.is_empty() {
        if let Some(chrome) = chrome.as_ref().filter(|_| chrome_changed || resized) {
            copy_chrome(store, chrome);
            store.dirty = true;
        }
        if holes_changed {
            store.dirty = true;
        }
        return;
    }

    for row in region.y..region.y + region.height {
        let base = (row as usize * width as usize + region.x as usize) * 4;
        store.pixels[base..base + region.width as usize * 4].fill(0);
    }
    let views = VIEW_STORES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(views) = views.as_ref() {
        for p in &placed {
            let Some(source) = views.get(&(p.view as usize)) else {
                continue;
            };
            blend_at(
                store,
                source,
                p.rect.x,
                p.rect.y,
                p.clip.intersection(&region),
            );
        }
    }
    if let Some(chrome) = &chrome {
        copy_chrome(store, chrome);
    }
    store.dirty = true;
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
        store.dirty = true;
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
/// source-over, within `clip`.
fn blend_at(store: &mut Store, source: &Store, left: i64, top: i64, clip: Rect) {
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
            match texel[3] {
                255 => target.copy_from_slice(texel),
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
        let composed = compose(window, store);
        let store = composed.as_ref().unwrap_or(store);
        dump(window, store);
        let holes = WINDOWS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|windows| windows.get(&(window as usize)))
            .map(|state| state.holes.clone())
            .unwrap_or_default();
        if presenter.is_some_and(|present| present(window, store, &holes)) {
            continue;
        }
        // The guest's own GL context owns this window's surface; see
        // [`crate::window::draws_with_opengl`]. There is nothing to fall back
        // to, so the store is left unshown rather than taking the context away.
        if crate::window::draws_with_opengl(window) {
            continue;
        }
        let transparent = sdl3::video::window_flags(window) & sdl3::video::flags::TRANSPARENT != 0;
        let chrome = crate::decor::pixels(window).filter(|_| transparent);
        sdl3::video::present_window(window, |surface| {
            let rows = store.height.min(surface.height.max(0) as u32) as usize;
            let columns = store.width.min(surface.width.max(0) as u32) as usize;
            for row in 0..rows {
                let from = row * store.width as usize * 4;
                // SAFETY: SDL reported `pitch` bytes per row for `height` rows.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        store.pixels[from..from + columns * 4].as_ptr(),
                        surface.pixels.add(row * surface.pitch as usize),
                        columns * 4,
                    )
                };
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
    /// Premultiplied B, G, R, A, top row first.
    pub pixels: Vec<u8>,
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

/// The store with every visible overlay of `window` drawn over it, or `None`
/// when there is nothing to draw and the store can be presented as it is.
fn compose(window: sdl3::Window, store: &Store) -> Option<Store> {
    let overlays = OVERLAYS.lock().unwrap_or_else(|e| e.into_inner());
    let mut composed: Option<Store> = None;
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
        let target = composed.get_or_insert_with(|| Store {
            width: store.width,
            height: store.height,
            pixels: store.pixels.clone(),
            dirty: false,
        });
        blend_overlay(target, overlay, frame.x, frame.y, frame.width, frame.height);
    }
    composed
}

/// Draw `overlay` scaled into the rectangle at `left`, `top` of `store`,
/// source-over, nearest-neighbour.
fn blend_overlay(
    store: &mut Store,
    overlay: &Overlay,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
) {
    let (source_width, source_height) = (overlay.width as usize, overlay.height as usize);
    if source_width == 0
        || source_height == 0
        || width == 0
        || height == 0
        || overlay.pixels.len() < source_width * source_height * 4
    {
        return;
    }
    let x0 = left.max(0) as usize;
    let y0 = top.max(0) as usize;
    let x1 = (i64::from(left) + i64::from(width)).clamp(0, i64::from(store.width)) as usize;
    let y1 = (i64::from(top) + i64::from(height)).clamp(0, i64::from(store.height)) as usize;
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
        let from_row = &overlay.pixels[row * source_width * 4..(row + 1) * source_width * 4];
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

/// `ROSETTA3_STORE_OUT=DIR`: write each window's store as a binary PPM when it
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
    std::env::var_os("ROSETTA3_STORE_OUT").is_some()
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
    let Some(directory) = std::env::var_os("ROSETTA3_STORE_OUT") else {
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

unsafe extern "C" fn current_context(_class: Id, _sel: Sel) -> Id {
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

unsafe extern "C" fn context_is_flipped(_receiver: Id, _sel: Sel) -> u8 {
    current_focus().is_some_and(|focus| focus.flipped) as u8
}

unsafe extern "C" fn nil(_receiver: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
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
    class_methods!(context, {
        "currentContext" => current_context, c"@16@0:8";
        "saveGraphicsState" => ignored, c"v16@0:8";
        "restoreGraphicsState" => ignored, c"v16@0:8";
    });
    methods!(context, {
        "isFlipped" => context_is_flipped, c"c16@0:8";
        "CGContext" => nil, c"^v16@0:8";
        "graphicsPort" => nil, c"^v16@0:8";
        "saveGraphicsState" => ignored, c"v16@0:8";
        "restoreGraphicsState" => ignored, c"v16@0:8";
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

    /// A view of `NSView`'s, and one of a flipped subclass, so a tree can be
    /// placed without a window.
    fn view(frame: NSRect, flipped: bool) -> Id {
        crate::bootstrap();
        let name = match flipped {
            true => c"R3BackingFlippedView",
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
                density: 1.0,
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
                density: 1.0,
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
        blend_at(&mut window, &parent, 0, 0, whole);
        blend_at(&mut window, &child, 1, 0, whole);
        assert_eq!(window.pixels, [1, 1, 1, 255, 9, 9, 9, 255]);
    }

    fn store_of(width: u32, height: u32) -> Store {
        Store {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
            dirty: false,
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
    fn an_overlay_is_scaled_into_its_rectangle_over_the_store() {
        let mut store = store_of(4, 2);
        store.pixels = vec![9; 4 * 2 * 4];
        // One opaque texel and one fully transparent one, stretched over two
        // store pixels each.
        let overlay = Overlay {
            width: 2,
            height: 1,
            pixels: vec![1, 2, 3, 255, 0, 0, 0, 0],
        };
        blend_overlay(&mut store, &overlay, 0, 1, 4, 1);
        assert_eq!(&store.pixels[..16], &[9; 16]);
        assert_eq!(
            &store.pixels[16..],
            &[1, 2, 3, 255, 1, 2, 3, 255, 9, 9, 9, 9, 9, 9, 9, 9]
        );
    }

    #[test]
    fn an_overlay_past_the_store_edge_is_clipped() {
        let mut store = store_of(2, 2);
        let overlay = Overlay {
            width: 1,
            height: 1,
            pixels: vec![7, 7, 7, 255],
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
