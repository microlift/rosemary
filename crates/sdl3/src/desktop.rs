//! The virtual desktop: every window a program makes, drawn into one host
//! window that stands for the screen. Between the windows the host window is
//! clear and takes no input, where the host can be told so, and what the
//! host has behind it is seen and used as ever.
//!
//! A Mac program places its windows itself, in one coordinate space shared
//! with every other window, and reads the pointer in that space. A window
//! system that keeps placement to itself - Wayland - cannot honour that for
//! separate host windows, and a program that docks one window into another
//! depends on it. So here the desktop is the window system instead, as a
//! Mac's window server is: it owns each window's frame, level and stacking,
//! composites their pictures, and sends input to the one under the pointer.
//!
//! [`crate::video`] asks this module first about every window. A window made
//! while the desktop is on is a real but never shown SDL window - it keeps
//! its id, title and place in the tables keyed by its address - whose
//! geometry, visibility, focus and picture are kept here.
//!
//! [`scene`] is the window list and [`compose`] the drawing; this file is the
//! host window, and SDL's events and calls turned into theirs.

use std::collections::HashMap;
use std::ffi::CStr;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::video::{self, Event, HitTest, HitTestResult, Point, Presentation, flags};
use crate::{Rect, Window};

mod compose;
mod scene;

use compose::{Buffer, Layer, Target};
use scene::{Scene, WindowId};

/// `ROSEMARY_DESKTOP`: `fullscreen` for a desktop that takes the screen at
/// its resolution, or `<width>x<height>` for one in a window of that size.
pub const ENV: &str = "ROSEMARY_DESKTOP";

const TITLE: &CStr = c"Rosemary desktop";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Extent {
    Fullscreen,
    Sized(i32, i32),
}

fn parse(value: &str) -> Option<Extent> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("fullscreen") {
        return Some(Extent::Fullscreen);
    }
    let (width, height) = value.split_once(['x', 'X'])?;
    let width = width.trim().parse::<i32>().ok()?;
    let height = height.trim().parse::<i32>().ok()?;
    (width > 0 && height > 0).then_some(Extent::Sized(width, height))
}

fn extent() -> Option<Extent> {
    static EXTENT: OnceLock<Option<Extent>> = OnceLock::new();
    *EXTENT.get_or_init(|| {
        let value = std::env::var(ENV).ok()?;
        if value.trim().is_empty() {
            return None;
        }
        let extent = parse(&value);
        if extent.is_none() {
            eprintln!("rosemary: sdl3: {ENV}={value} is not recognised; running without a desktop");
        }
        extent
    })
}

/// Whether windows are made on the virtual desktop.
pub fn active() -> bool {
    extent().is_some()
}

/// What the desktop keeps for one window besides its place in the scene.
struct Surface {
    handle: usize,
    buffer: Buffer,
    /// Whether the program has drawn it. Until then there is nothing to show.
    drawn: bool,
    hit_test: Option<HitTest>,
}

struct Desktop {
    host: usize,
    host_id: u32,
    /// Host pixels per desktop point.
    density: f32,
    scene: Scene,
    surfaces: HashMap<WindowId, Surface>,
    /// Host pixels to redraw besides what the scene reports.
    damage: Option<Rect>,
    /// The host surface's size when last drawn: SDL makes another when it
    /// changes, with none of the old one's pixels.
    drawn_size: (i32, i32),
    /// The window that asked for text input, which is the host's to give.
    text_input: Option<WindowId>,
    /// Likewise relative mouse mode, and whether the host is in it.
    relative: Option<WindowId>,
    host_relative: bool,
    /// Likewise a pointer rectangle, in the window's coordinates, a window
    /// that keeps the pointer in the whole of itself, and the rectangle the
    /// host has.
    confined: Option<(WindowId, Rect)>,
    grabbed: Option<WindowId>,
    host_confined: Option<Rect>,
    /// Whether the host shows its own desktop between the windows and takes
    /// the clicks made there, and the region it was last told to leave to
    /// this one.
    see_through: bool,
    input: Option<Vec<Rect>>,
}

static DESKTOP: Mutex<Option<Desktop>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<Desktop>> {
    DESKTOP.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run `f` on the desktop and the window `window` is on it, if it is one.
/// `None` is every other window, the host's own included.
fn with_window<T>(window: Window, f: impl FnOnce(&mut Desktop, WindowId) -> T) -> Option<T> {
    let mut guard = lock();
    let desktop = guard.as_mut()?;
    let id = desktop.id_of(window)?;
    Some(f(desktop, id))
}

/// What the desktop will not show, each named once.
fn refuse(what: &'static str) {
    static REFUSED: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut refused = REFUSED.lock().unwrap_or_else(|e| e.into_inner());
    if !refused.contains(&what) {
        refused.push(what);
        eprintln!("rosemary: sdl3: {what} cannot be shown on the virtual desktop ({ENV}); refused");
    }
}

fn union(damage: Option<Rect>, rect: Rect) -> Option<Rect> {
    match damage {
        Some(damage) => Some(damage.bounding(rect)),
        None => (!rect.is_empty()).then_some(rect),
    }
}

impl Desktop {
    fn id_of(&self, window: Window) -> Option<WindowId> {
        self.surfaces
            .iter()
            .find(|(_, surface)| surface.handle == window as usize)
            .map(|(id, _)| *id)
    }

    fn handle_of(&self, id: WindowId) -> Window {
        self.surfaces
            .get(&id)
            .map_or(std::ptr::null_mut(), |surface| surface.handle as Window)
    }

    fn pixels(&self, points: i32) -> i32 {
        (points as f32 * self.density).round() as i32
    }

    /// The host pixels a rectangle of the desktop covers, and their
    /// neighbours a rounded edge may have touched.
    fn pixel_rect(&self, rect: Rect) -> Rect {
        let left = (rect.x as f32 * self.density).floor() as i32;
        let top = (rect.y as f32 * self.density).floor() as i32;
        let right = ((rect.x + rect.w) as f32 * self.density).ceil() as i32;
        let bottom = ((rect.y + rect.h) as f32 * self.density).ceil() as i32;
        Rect {
            x: left,
            y: top,
            w: right - left,
            h: bottom - top,
        }
    }

    /// Give a window's buffer the size its frame now has.
    fn fit_buffer(&mut self, id: WindowId) {
        let Some(frame) = self.scene.frame(id) else {
            return;
        };
        let (width, height) = (self.pixels(frame.w), self.pixels(frame.h));
        if let Some(surface) = self.surfaces.get_mut(&id) {
            surface.buffer.resize(width, height);
        }
    }

    fn fit_buffers(&mut self) {
        let ids: Vec<WindowId> = self.surfaces.keys().copied().collect();
        for id in ids {
            self.fit_buffer(id);
        }
    }

    fn damage_all(&mut self) {
        self.damage = Some(self.pixel_rect(self.scene.bounds()));
    }

    /// The pointer rectangle the host should have: the one a window asked
    /// for, for as long as that window has the keyboard.
    fn wanted_confinement(&self) -> Option<Rect> {
        let focus = self.scene.keyboard();
        let frame = self.scene.frame(focus)?;
        match self.confined {
            Some((id, rect)) if id == focus => Some(Rect {
                x: frame.x + rect.x,
                y: frame.y + rect.y,
                ..rect
            }),
            _ => (self.grabbed == Some(focus)).then_some(frame),
        }
    }
}

/// The desktop's size before or after the host window exists.
fn size() -> Option<(i32, i32)> {
    if let Some(desktop) = lock().as_ref() {
        return Some(desktop.scene.size());
    }
    match extent()? {
        Extent::Sized(width, height) => Some((width, height)),
        Extent::Fullscreen => video::native_display_bounds(video::primary_display())
            .map(|bounds| (bounds.w, bounds.h)),
    }
}

/// The desktop as the one display there is, or `None` without a desktop.
pub(crate) fn bounds() -> Option<Rect> {
    let (w, h) = size()?;
    Some(Rect { x: 0, y: 0, w, h })
}

/// Make the host window, once.
fn start() -> bool {
    if lock().is_some() {
        return true;
    }
    let (Some(extent), Some((width, height))) = (extent(), size()) else {
        return false;
    };
    // A desktop that takes the clicks made between its windows has to be
    // seen, so it is clear only where the host can be told not to.
    let see_through = video::has_window_input_regions();
    let mut host_flags = flags::HIGH_PIXEL_DENSITY;
    if see_through {
        host_flags |= flags::TRANSPARENT;
    }
    if extent == Extent::Fullscreen {
        host_flags |= flags::FULLSCREEN;
    }
    let Some(host) = video::create_native_window(TITLE, width, height, host_flags) else {
        return false;
    };
    let mut desktop = Desktop {
        host: host as usize,
        host_id: video::window_id(host),
        density: video::window_pixel_density(host),
        scene: Scene::new((width, height)),
        surfaces: HashMap::new(),
        damage: None,
        drawn_size: (0, 0),
        text_input: None,
        relative: None,
        host_relative: false,
        confined: None,
        grabbed: None,
        host_confined: None,
        see_through,
        input: None,
    };
    desktop.damage_all();
    *lock() = Some(desktop);
    true
}

/// A window on the desktop. `parent` and an offset from it make a popup,
/// placed as SDL places one.
pub(crate) fn create_window(
    title: &CStr,
    width: i32,
    height: i32,
    window_flags: u64,
    parent: Option<(Window, i32, i32)>,
) -> Option<Window> {
    if window_flags & flags::OPENGL != 0 {
        refuse("an OpenGL window");
        return None;
    }
    if !start() {
        return None;
    }
    let window =
        video::create_native_window(title, width, height, flags::HIDDEN | flags::BORDERLESS)?;
    // SDL destroys a popup with its parent, and the caller counts on it.
    if let Some((parent, ..)) = parent
        && !video::set_native_parent(window, parent)
    {
        video::destroy_native_window(window);
        return None;
    }
    let id = video::window_id(window);
    let mut guard = lock();
    let desktop = guard.as_mut()?;
    let (parent, offset) = match parent {
        Some((parent, x, y)) => (desktop.id_of(parent), (x, y)),
        // Centred, where SDL puts a window that was not told otherwise.
        None => {
            let (desktop_width, desktop_height) = desktop.scene.size();
            (
                None,
                ((desktop_width - width) / 2, (desktop_height - height) / 2),
            )
        }
    };
    desktop.surfaces.insert(
        id,
        Surface {
            handle: window as usize,
            buffer: Buffer::default(),
            drawn: false,
            hit_test: None,
        },
    );
    desktop
        .scene
        .add(id, parent, window_flags, offset, (width, height));
    desktop.fit_buffer(id);
    Some(window)
}

/// `window` is being destroyed.
pub(crate) fn destroyed(window: Window) {
    with_window(window, |desktop, id| {
        desktop.scene.remove(id);
        desktop.surfaces.remove(&id);
        for holder in [
            &mut desktop.text_input,
            &mut desktop.relative,
            &mut desktop.grabbed,
        ] {
            if *holder == Some(id) {
                *holder = None;
            }
        }
        if desktop.confined.is_some_and(|(holder, _)| holder == id) {
            desktop.confined = None;
        }
    });
}

/// Whether `window` is one of the desktop's.
pub(crate) fn owns(window: Window) -> bool {
    with_window(window, |_, _| ()).is_some()
}

pub(crate) fn window_size(window: Window) -> Option<(i32, i32)> {
    with_window(window, |desktop, id| desktop.scene.frame(id))?.map(|frame| (frame.w, frame.h))
}

pub(crate) fn set_window_size(window: Window, width: i32, height: i32) -> Option<bool> {
    with_window(window, |desktop, id| {
        desktop.scene.set_size(id, width, height);
        desktop.fit_buffer(id);
        true
    })
}

pub(crate) fn window_size_in_pixels(window: Window) -> Option<(i32, i32)> {
    with_window(window, |desktop, id| {
        desktop
            .scene
            .frame(id)
            .map(|frame| (desktop.pixels(frame.w), desktop.pixels(frame.h)))
    })?
}

pub(crate) fn window_position(window: Window) -> Option<(i32, i32)> {
    with_window(window, |desktop, id| desktop.scene.position(id))?
}

pub(crate) fn set_window_position(window: Window, x: i32, y: i32) -> Option<bool> {
    with_window(window, |desktop, id| {
        desktop.scene.set_position(id, x, y);
        true
    })
}

pub(crate) fn window_flags(window: Window) -> Option<u64> {
    with_window(window, |desktop, id| desktop.scene.flags(id))?
}

pub(crate) fn window_pixel_density(window: Window) -> Option<f32> {
    with_window(window, |desktop, _| desktop.density)
}

/// The window a popup hangs off, or null; `None` for a window not here.
pub(crate) fn window_parent(window: Window) -> Option<Window> {
    with_window(window, |desktop, id| match desktop.scene.parent(id) {
        Some(parent) => desktop.handle_of(parent),
        None => std::ptr::null_mut(),
    })
}

pub(crate) fn show_window(window: Window) -> Option<bool> {
    with_window(window, |desktop, id| {
        desktop.scene.show(id);
        true
    })
}

pub(crate) fn hide_window(window: Window) -> Option<bool> {
    with_window(window, |desktop, id| {
        desktop.scene.hide(id);
        true
    })
}

/// Bring a window to the front of its level, with the keyboard for `focus`.
pub(crate) fn raise_window(window: Window, focus: bool) -> Option<bool> {
    with_window(window, |desktop, id| {
        desktop.scene.raise(id, focus);
        true
    })
}

/// Enter or leave `state`, which is `FULLSCREEN` or `MAXIMIZED`.
pub(crate) fn set_window_covering(window: Window, state: u64, on: bool) -> Option<bool> {
    with_window(window, |desktop, id| {
        desktop.scene.set_covering(id, state, on);
        desktop.fit_buffer(id);
        true
    })
}

pub(crate) fn set_window_opacity(window: Window, opacity: f32) -> Option<bool> {
    with_window(window, |desktop, id| {
        desktop.scene.set_opacity(id, opacity);
        true
    })
}

pub(crate) fn window_opacity(window: Window) -> Option<f32> {
    with_window(window, |desktop, id| desktop.scene.opacity(id))?
}

pub(crate) fn set_window_flag(window: Window, flag: u64, on: bool) -> Option<bool> {
    with_window(window, |desktop, id| {
        desktop.scene.set_flag(id, flag, on);
        true
    })
}

pub(crate) fn set_window_minimum_size(window: Window, width: i32, height: i32) -> Option<bool> {
    with_window(window, |desktop, id| {
        desktop.scene.set_minimum(id, width, height);
        true
    })
}

pub(crate) fn set_window_hit_test(window: Window, callback: Option<HitTest>) -> Option<bool> {
    with_window(window, |desktop, id| {
        if let Some(surface) = desktop.surfaces.get_mut(&id) {
            surface.hit_test = callback;
        }
        true
    })
}

/// Make `window` modal for `parent`, both on the desktop.
pub(crate) fn set_window_modal_for(window: Window, parent: Window) -> Option<bool> {
    with_window(window, |desktop, id| match desktop.id_of(parent) {
        Some(parent) => {
            desktop.scene.set_modal_for(id, parent);
            true
        }
        None => false,
    })
}

/// Whether `window` is one a Vulkan surface is refused for, saying so.
pub(crate) fn refuses_surface(window: Window) -> bool {
    let refused = owns(window);
    if refused {
        refuse("a Vulkan surface");
    }
    refused
}

/// The level a window stacks at, as a Mac's window server is told it: an
/// `NSWindowLevel`, which is a `CGWindowLevel`. Without a desktop the host's
/// window manager stacks windows and has no such thing to be told.
pub fn set_window_level(window: Window, level: i32) {
    with_window(window, |desktop, id| desktop.scene.set_level(id, level));
}

/// Let `body` draw `window`'s picture, as [`video::present_window`] does on
/// the window's surface. `Err` hands `body` back for a window not here.
pub(crate) fn draw<F>(window: Window, body: F) -> Result<bool, F>
where
    F: FnOnce(&Presentation) -> bool,
{
    // Out of the table while `body` runs: it asks about the window, which
    // takes the lock held here.
    let taken = with_window(window, |desktop, id| {
        let format = match desktop.scene.flags(id).unwrap_or(0) & flags::TRANSPARENT != 0 {
            true => video::pixel_format::ARGB8888,
            false => video::pixel_format::XRGB8888,
        };
        let surface = desktop.surfaces.get_mut(&id);
        surface.map(|surface| (id, std::mem::take(&mut surface.buffer), format))
    });
    let Some(taken) = taken else {
        return Err(body);
    };
    let Some((id, mut buffer, format)) = taken else {
        return Ok(false);
    };
    let drew = body(&Presentation {
        width: buffer.width,
        height: buffer.height,
        pitch: buffer.pitch() as i32,
        format,
        pixels: buffer.pixels.as_mut_ptr(),
    });
    let mut guard = lock();
    let Some(desktop) = guard.as_mut() else {
        return Ok(false);
    };
    let Some(frame) = desktop.scene.frame(id) else {
        return Ok(false);
    };
    let damage = desktop.pixel_rect(frame);
    if let Some(surface) = desktop.surfaces.get_mut(&id) {
        surface.buffer = buffer;
        surface.drawn |= drew;
        if drew {
            desktop.damage = union(desktop.damage, damage);
        }
    }
    Ok(drew)
}

/// A frame's worth of the desktop, taken out of it so that drawing holds no
/// lock a window query needs.
struct Frame {
    host: Window,
    damage: Rect,
    background: [u8; 4],
    /// Back to front: the window, its picture, where it goes and how.
    layers: Vec<(WindowId, Buffer, (i32, i32), bool, f32)>,
}

fn take_frame(desktop: &mut Desktop) -> Option<Frame> {
    let changed = desktop
        .scene
        .take_damage()
        .map(|rect| desktop.pixel_rect(rect));
    let damage = match (desktop.damage.take(), changed) {
        (Some(damage), Some(changed)) => damage.bounding(changed),
        (damage, changed) => damage.or(changed)?,
    };
    let mut layers = Vec::new();
    for id in desktop.scene.stack() {
        let (Some(frame), Some(window_flags), Some(opacity)) = (
            desktop.scene.frame(id),
            desktop.scene.flags(id),
            desktop.scene.opacity(id),
        ) else {
            continue;
        };
        let origin = (desktop.pixels(frame.x), desktop.pixels(frame.y));
        let Some(surface) = desktop
            .surfaces
            .get_mut(&id)
            .filter(|surface| surface.drawn)
        else {
            continue;
        };
        layers.push((
            id,
            std::mem::take(&mut surface.buffer),
            origin,
            window_flags & flags::TRANSPARENT != 0,
            opacity,
        ));
    }
    Some(Frame {
        host: desktop.host as Window,
        damage,
        background: match desktop.see_through {
            true => compose::CLEAR,
            false => compose::BACKGROUND,
        },
        layers,
    })
}

/// Show what changed since the last call. On the video thread, once the
/// windows have drawn: the end of a turn of the event pump.
pub fn present() {
    if !video::is_video_thread() {
        return;
    }
    sync_host_pointer();
    sync_input_region();
    let Some(frame) = lock().as_mut().and_then(take_frame) else {
        return;
    };
    let drawn_size = lock().as_ref().map_or((0, 0), |desktop| desktop.drawn_size);
    let mut size = drawn_size;
    video::present_native(frame.host, Some(frame.damage), |surface| {
        size = (surface.width, surface.height);
        let whole = Rect {
            x: 0,
            y: 0,
            w: surface.width,
            h: surface.height,
        };
        let damage = match size == drawn_size {
            true => frame.damage,
            false => whole,
        };
        // SAFETY: SDL owns `pitch * height` bytes for the length of the call.
        let pixels = unsafe {
            std::slice::from_raw_parts_mut(
                surface.pixels,
                surface.pitch.max(0) as usize * surface.height.max(0) as usize,
            )
        };
        let layers: Vec<Layer> = frame
            .layers
            .iter()
            .map(|(_, buffer, origin, alpha, opacity)| Layer {
                buffer,
                x: origin.0,
                y: origin.1,
                alpha: *alpha,
                opacity: *opacity,
            })
            .collect();
        let mut target = Target {
            pixels,
            pitch: surface.pitch.max(0) as usize,
            width: surface.width,
            height: surface.height,
        };
        compose::compose(&mut target, frame.background, &layers, damage);
        true
    });
    let mut guard = lock();
    let Some(desktop) = guard.as_mut() else {
        return;
    };
    desktop.drawn_size = size;
    for (id, buffer, ..) in frame.layers {
        // A window resized meanwhile already has a buffer of its new size.
        if let Some(surface) = desktop.surfaces.get_mut(&id)
            && surface.buffer.pixels.is_empty()
        {
            surface.buffer = buffer;
        }
    }
}

/// Leave the host everything between the windows now showing: a click there
/// is for whatever it has behind the desktop.
fn sync_input_region() {
    let wanted = lock().as_ref().and_then(|desktop| {
        // While a button is held the host sends the pointer here wherever it
        // goes, so a window being dragged is followed once it is let go.
        if !desktop.see_through || desktop.scene.buttons() != 0 {
            return None;
        }
        let region = desktop.scene.pointer_region();
        (desktop.input.as_ref() != Some(&region)).then_some((desktop.host as Window, region))
    });
    let Some((host, region)) = wanted else {
        return;
    };
    if video::set_window_input_region(host, &region)
        && let Some(desktop) = lock().as_mut()
    {
        desktop.input = Some(region);
    }
}

/// Bring the host's relative mode and pointer rectangle to what the window
/// with the keyboard asked for: SDL holds both for a window only while it
/// has the focus, and here the host window always has it.
fn sync_host_pointer() {
    let changes = lock().as_mut().map(|desktop| {
        if desktop
            .relative
            .is_some_and(|id| desktop.scene.keyboard() != id)
        {
            desktop.relative = None;
        }
        let relative = desktop.relative.is_some();
        let confined = desktop.wanted_confinement();
        let changes = (
            desktop.host as Window,
            (relative != desktop.host_relative).then_some(relative),
            (confined != desktop.host_confined).then_some(confined),
        );
        desktop.host_relative = relative;
        desktop.host_confined = confined;
        changes
    });
    let Some((host, relative, confined)) = changes else {
        return;
    };
    if let Some(relative) = relative {
        video::set_relative_mouse_mode(host, relative);
    }
    if let Some(confined) = confined {
        video::set_mouse_rect(host, confined);
    }
}

/// The host window changed size or scale: the desktop's are the host's.
fn resync() {
    let Some(host) = lock().as_ref().map(|desktop| desktop.host as Window) else {
        return;
    };
    let size = video::window_size(host);
    let density = video::window_pixel_density(host);
    let mut guard = lock();
    let Some(desktop) = guard.as_mut() else {
        return;
    };
    if let Some(size) = size.filter(|size| *size != desktop.scene.size()) {
        desktop.scene.resize(size);
        desktop.scene.push(Event::DisplayModeChanged {
            display: video::primary_display(),
        });
    }
    if density != desktop.density {
        desktop.density = density;
        let windows: Vec<WindowId> = desktop.surfaces.keys().copied().collect();
        for window in windows {
            desktop.scene.push(Event::WindowBackingChanged { window });
        }
    }
    desktop.fit_buffers();
    desktop.damage_all();
}

/// The window an event is about, where it is about one.
fn window_of(event: &Event) -> Option<u32> {
    match event {
        Event::WindowMoved { window }
        | Event::WindowDisplayChanged { window }
        | Event::WindowResized { window, .. }
        | Event::WindowBackingChanged { window }
        | Event::WindowClosed { window }
        | Event::WindowFocus { window, .. }
        | Event::WindowMouse { window, .. }
        | Event::WindowHitTest { window }
        | Event::WindowState { window }
        | Event::Key { window, .. }
        | Event::Text { window, .. }
        | Event::TextEditing { window, .. }
        | Event::MouseMotion { window, .. }
        | Event::MouseButton { window, .. }
        | Event::MouseWheel { window, .. }
        | Event::Pinch { window, .. } => Some(*window),
        Event::Quit | Event::DisplayModeChanged { .. } | Event::Other(_) => None,
    }
}

/// The next event the desktop has for one of its windows.
pub(crate) fn take_event() -> Option<Event> {
    lock().as_mut()?.scene.take_event()
}

/// Take an event SDL delivered. One about the host window becomes events for
/// the windows on the desktop, read back with [`take_event`]; `Some` is an
/// event that is not the desktop's to handle.
pub(crate) fn route(event: Event) -> Option<Event> {
    let mut guard = lock();
    let Some(desktop) = guard.as_mut() else {
        return Some(event);
    };
    let Some(window) = window_of(&event) else {
        return Some(event);
    };
    // The desktop is the only one with anything to say about its windows.
    if desktop.surfaces.contains_key(&window) {
        return None;
    }
    if window != desktop.host_id {
        return Some(event);
    }
    let scene = &mut desktop.scene;
    match event {
        Event::MouseMotion { x, y, dx, dy, .. } => match desktop.relative {
            // The pointer stays where it is and only its deltas arrive.
            Some(window) if desktop.host_relative => {
                let (pointer, origin) = (scene.pointer(), scene.frame(window).unwrap_or_default());
                scene.push(Event::MouseMotion {
                    window,
                    x: pointer.0 - origin.x as f32,
                    y: pointer.1 - origin.y as f32,
                    dx,
                    dy,
                    buttons: scene.buttons(),
                });
            }
            _ => scene.pointer_moved(x, y, dx, dy),
        },
        Event::MouseButton {
            button,
            down,
            clicks,
            x,
            y,
            ..
        } => {
            if !desktop.host_relative && scene.pointer() != (x, y) {
                scene.pointer_moved(x, y, 0.0, 0.0);
            }
            let probe = scene
                .press_point()
                .filter(|_| down)
                .and_then(|(window, point)| {
                    let surface = desktop.surfaces.get(&window)?;
                    Some((surface.hit_test?, surface.handle, point))
                });
            // The hit test asks about its window, which takes the lock.
            drop(guard);
            let hit = probe.map_or(HitTestResult::Normal, |(hit_test, handle, (x, y))| {
                // SAFETY: the callback the window's owner registered, called
                // as SDL calls it: with its window and a live point.
                unsafe { hit_test(handle as Window, &Point { x, y }, std::ptr::null_mut()) }
            });
            if let Some(desktop) = lock().as_mut() {
                desktop.scene.button(button, down, clicks, hit);
            }
        }
        Event::MouseWheel { x, y, .. } => {
            if let Some(window) = scene.pointer_window() {
                scene.push(Event::MouseWheel { window, x, y });
            }
        }
        Event::Pinch { scale, phase, .. } => {
            if let Some(window) = scene.pointer_window() {
                scene.push(Event::Pinch {
                    window,
                    scale,
                    phase,
                });
            }
        }
        Event::Key {
            scancode,
            key,
            modifiers,
            down,
            repeat,
            ..
        } => {
            let window = scene.keyboard();
            scene.push(Event::Key {
                window,
                scancode,
                key,
                modifiers,
                down,
                repeat,
            });
        }
        Event::Text { text, .. } => {
            let window = scene.keyboard();
            scene.push(Event::Text { window, text });
        }
        Event::TextEditing {
            text,
            start,
            length,
            ..
        } => {
            let window = scene.keyboard();
            scene.push(Event::TextEditing {
                window,
                text,
                start,
                length,
            });
        }
        Event::WindowFocus { gained, .. } => scene.host_focus(gained),
        Event::WindowMouse { entered: false, .. } => scene.pointer_left(),
        Event::WindowResized { .. }
        | Event::WindowBackingChanged { .. }
        | Event::WindowDisplayChanged { .. } => {
            drop(guard);
            resync();
        }
        // Closing the desktop closes the session, as closing a screen would.
        Event::WindowClosed { .. } => return Some(Event::Quit),
        _ => {}
    }
    None
}

/// The pointer in the coordinates of the window it is in, or the desktop's
/// when it is in none, and the held buttons.
pub(crate) fn mouse_state() -> Option<(f32, f32, u32)> {
    let guard = lock();
    let scene = &guard.as_ref()?.scene;
    let origin = scene
        .pointer_window()
        .and_then(|window| scene.frame(window))
        .map_or((0, 0), |frame| (frame.x, frame.y));
    let (x, y) = scene.pointer();
    Some((x - origin.0 as f32, y - origin.1 as f32, scene.buttons()))
}

pub(crate) fn global_mouse_state() -> Option<(f32, f32, u32)> {
    let guard = lock();
    let scene = &guard.as_ref()?.scene;
    let (x, y) = scene.pointer();
    Some((x, y, scene.buttons()))
}

/// Move the pointer to a desktop point, which is a point of the host window.
pub(crate) fn warp_mouse_global(x: f32, y: f32) -> Option<bool> {
    let host = lock().as_ref()?.host as Window;
    Some(match video::host_is_wayland() {
        // What SDL can do only where the compositor has a protocol for it.
        true => crate::pointer::warp(host, x, y).is_some(),
        false => video::warp_mouse_in_window(host, x, y),
    })
}

pub(crate) fn warp_mouse_in_window(window: Window, x: f32, y: f32) -> Option<bool> {
    let frame = with_window(window, |desktop, id| desktop.scene.frame(id))??;
    warp_mouse_global(frame.x as f32 + x, frame.y as f32 + y)
}

pub(crate) fn set_relative_mouse_mode(window: Window, enabled: bool) -> Option<bool> {
    with_window(window, |desktop, id| match enabled {
        true => desktop.relative = Some(id),
        false if desktop.relative == Some(id) => desktop.relative = None,
        false => {}
    })?;
    sync_host_pointer();
    Some(true)
}

pub(crate) fn relative_mouse_mode(window: Window) -> Option<bool> {
    with_window(window, |desktop, id| {
        desktop.host_relative && desktop.relative == Some(id)
    })
}

pub(crate) fn set_mouse_rect(window: Window, rect: Option<Rect>) -> Option<bool> {
    with_window(window, |desktop, id| match rect {
        Some(rect) => desktop.confined = Some((id, rect)),
        None if desktop.confined.is_some_and(|(holder, _)| holder == id) => {
            desktop.confined = None;
        }
        None => {}
    })?;
    sync_host_pointer();
    Some(true)
}

pub(crate) fn set_mouse_grab(window: Window, grabbed: bool) -> Option<bool> {
    with_window(window, |desktop, id| match grabbed {
        true => desktop.grabbed = Some(id),
        false if desktop.grabbed == Some(id) => desktop.grabbed = None,
        false => {}
    })?;
    sync_host_pointer();
    Some(true)
}

/// `Some(None)` is a window of the desktop's with no rectangle.
pub(crate) fn get_mouse_rect(window: Window) -> Option<Option<Rect>> {
    with_window(window, |desktop, id| {
        desktop
            .confined
            .filter(|(holder, _)| *holder == id)
            .map(|(_, rect)| rect)
    })
}

/// The host window, for a call on one of the desktop's windows that only a
/// window on the screen can carry out.
fn host_for(window: Window) -> Option<(Window, WindowId)> {
    with_window(window, |desktop, id| (desktop.host as Window, id))
}

pub(crate) fn start_text_input(window: Window) -> Option<bool> {
    let (host, id) = host_for(window)?;
    let started = video::start_text_input(host);
    if started && let Some(desktop) = lock().as_mut() {
        desktop.text_input = Some(id);
    }
    Some(started)
}

pub(crate) fn stop_text_input(window: Window) -> Option<bool> {
    let (host, id) = host_for(window)?;
    let mine = lock()
        .as_mut()
        .is_some_and(|desktop| desktop.text_input.take_if(|holder| *holder == id).is_some());
    Some(mine && video::stop_text_input(host))
}

pub(crate) fn text_input_active(window: Window) -> Option<bool> {
    let (host, id) = host_for(window)?;
    let mine = lock()
        .as_ref()
        .is_some_and(|desktop| desktop.text_input == Some(id));
    Some(mine && video::text_input_active(host))
}

pub(crate) fn set_text_input_area(window: Window, area: Rect, cursor: i32) -> Option<bool> {
    let (host, frame) = with_window(window, |desktop, id| {
        (desktop.host as Window, desktop.scene.frame(id))
    })?;
    let frame = frame?;
    let area = Rect {
        x: frame.x + area.x,
        y: frame.y + area.y,
        ..area
    };
    Some(video::set_text_input_area(host, area, cursor))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [u8; 4] = [0, 0, 0xff, 0xff];
    const BLUE: [u8; 4] = [0xff, 0, 0, 0xff];

    fn window(at: (i32, i32), colour: [u8; 4]) -> Window {
        let window = video::create_window(c"window", 100, 50, flags::HIDDEN).expect("a window");
        assert!(video::set_window_position(window, at.0, at.1));
        assert!(video::present_window(window, |surface| {
            // SAFETY: the buffer the desktop lent for the length of the call.
            let pixels = unsafe {
                std::slice::from_raw_parts_mut(
                    surface.pixels,
                    surface.pitch as usize * surface.height as usize,
                )
            };
            for pixel in pixels.as_chunks_mut::<4>().0 {
                *pixel = colour;
            }
            true
        }));
        assert!(video::show_window(window));
        window
    }

    /// What the host window shows at a desktop point.
    fn shown_at(x: i32, y: i32) -> [u8; 4] {
        present();
        let host = lock().as_ref().expect("a desktop").host as Window;
        let mut pixel = [0; 4];
        video::present_native(host, None, |surface| {
            let at = y as usize * surface.pitch as usize + x as usize * 4;
            // SAFETY: a point inside the surface SDL lent for the call.
            pixel.copy_from_slice(unsafe { std::slice::from_raw_parts(surface.pixels.add(at), 4) });
            false
        });
        pixel
    }

    fn events() -> Vec<Event> {
        std::iter::from_fn(take_event).collect()
    }

    #[test]
    fn windows_share_one_host_window_and_stack_in_it() {
        if !crate::isolated::run_here_on_dummy_desktop("320x200") {
            return;
        }
        assert_eq!(
            video::display_bounds(video::primary_display()),
            Some(Rect {
                x: 0,
                y: 0,
                w: 320,
                h: 200
            })
        );
        let back = window((10, 10), RED);
        let front = window((60, 30), BLUE);
        assert_eq!(video::window_position(front), Some((60, 30)));
        assert_eq!(video::window_flags(front) & flags::HIDDEN, 0);
        assert_eq!(shown_at(15, 15), RED);
        assert_eq!(shown_at(70, 40), BLUE);
        assert_eq!(shown_at(5, 5), compose::BACKGROUND);

        assert!(video::raise_window(back));
        assert_eq!(shown_at(70, 40), RED);

        assert!(video::set_window_position(back, 200, 100));
        assert_eq!(shown_at(70, 40), BLUE);
        assert_eq!(shown_at(15, 15), compose::BACKGROUND);
        assert_eq!(shown_at(210, 110), RED);

        video::destroy_window(front);
        assert_eq!(shown_at(70, 40), compose::BACKGROUND);
    }

    unsafe extern "C" fn everywhere_draggable(
        _window: Window,
        _point: *const Point,
        _data: *mut std::ffi::c_void,
    ) -> HitTestResult {
        HitTestResult::Draggable
    }

    #[test]
    fn host_input_reaches_the_window_under_the_pointer() {
        if !crate::isolated::run_here_on_dummy_desktop("320x200") {
            return;
        }
        let plain = window((10, 10), RED);
        let titled = window((150, 100), BLUE);
        assert!(video::set_window_hit_test(
            titled,
            Some(everywhere_draggable)
        ));
        let host = lock().as_ref().expect("a desktop").host_id;
        // The dummy driver gives no window the keyboard by itself.
        let focused = Event::WindowFocus {
            window: host,
            gained: true,
        };
        assert_eq!(route(focused), None);
        events();

        let motion = |x, y| Event::MouseMotion {
            window: host,
            x,
            y,
            dx: 0.0,
            dy: 0.0,
            buttons: 0,
        };
        let button = |down, x, y| Event::MouseButton {
            window: host,
            button: 1,
            down,
            clicks: 1,
            x,
            y,
        };
        assert_eq!(route(motion(20.0, 15.0)), None);
        assert_eq!(route(button(true, 20.0, 15.0)), None);
        assert_eq!(route(button(false, 20.0, 15.0)), None);
        let id = video::window_id(plain);
        assert!(events().contains(&Event::MouseButton {
            window: id,
            button: 1,
            down: true,
            clicks: 1,
            x: 10.0,
            y: 5.0,
        }));
        assert_eq!(video::mouse_state(), Some((10.0, 5.0, 0)));
        assert_eq!(video::global_mouse_state(), Some((20.0, 15.0, 0)));
        assert_ne!(video::window_flags(plain) & flags::INPUT_FOCUS, 0);

        // A press its hit test calls draggable moves the window instead.
        assert_eq!(route(button(true, 160.0, 110.0)), None);
        assert_eq!(route(motion(170.0, 130.0)), None);
        assert_eq!(route(button(false, 170.0, 130.0)), None);
        assert_eq!(video::window_position(titled), Some((160, 120)));
        assert!(events().contains(&Event::WindowMoved {
            window: video::window_id(titled)
        }));

        // What SDL says about a window the desktop keeps is not passed on.
        assert_eq!(route(Event::WindowMoved { window: id }), None);
        assert_eq!(
            route(Event::WindowClosed { window: host }),
            Some(Event::Quit)
        );
    }

    #[test]
    fn what_the_desktop_cannot_show_is_refused() {
        if !crate::isolated::run_here_on_dummy_desktop("320x200") {
            return;
        }
        assert!(video::create_window(c"gl", 10, 10, flags::HIDDEN | flags::OPENGL).is_none());
        let plain = video::create_window(c"plain", 10, 10, flags::HIDDEN).expect("a window");
        assert!(video::gl_create_context(plain).is_none());
        assert!(!video::minimize_window(plain));
    }

    #[test]
    fn the_extent_is_fullscreen_or_a_size() {
        assert_eq!(parse("fullscreen"), Some(Extent::Fullscreen));
        assert_eq!(parse(" Fullscreen "), Some(Extent::Fullscreen));
        assert_eq!(parse("1280x800"), Some(Extent::Sized(1280, 800)));
        assert_eq!(parse("1280X800"), Some(Extent::Sized(1280, 800)));
        assert_eq!(parse("0x800"), None);
        assert_eq!(parse("1280"), None);
        assert_eq!(parse("on"), None);
    }
}
