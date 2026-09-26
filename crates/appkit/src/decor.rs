//! Window chrome the runtime draws itself: the Mac-style titlebar a
//! client-side decorated window gets, the application's menu bar, and the
//! menus it opens.
//!
//! Drawing is `chrome`'s; this module decides which windows get what, keeps
//! the pixels each window's presentation path composites, answers the window
//! manager's hit tests, and runs menu tracking.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use chrome::menu::{Item, Panel, PanelLayout};
use chrome::titlebar::{Bar, BarLayout, Lights, MenuTitle, Part};
use chrome::{Canvas, Point, Rect, Theme, View};
use objc::object::Id;
use objc::sel::Sel;

/// `ROSETTA3_DECORATIONS`: `client` (the default) draws the titlebar here;
/// `server` leaves it to the host's window manager.
pub const DECORATIONS_ENV: &str = "ROSETTA3_DECORATIONS";
/// `ROSETTA3_MENU`: `titlebar`, `strip` or `none`.
pub const MENU_ENV: &str = "ROSETTA3_MENU";
/// `ROSETTA3_MENU_CTRL=0` stops Control standing in for Command in menu key
/// equivalents.
pub const MENU_CTRL_ENV: &str = "ROSETTA3_MENU_CTRL";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decorations {
    Client,
    Server,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuPlacement {
    /// In the titlebar, after the window buttons.
    Titlebar,
    /// In a row of its own under the titlebar.
    Strip,
    Hidden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub decorations: Decorations,
    pub menu: MenuPlacement,
    /// Whether Control+key reaches a menu item whose equivalent is
    /// Command+key: Linux users reach for Control, and the host's desktop
    /// often keeps the Super key for itself.
    pub control_shortcuts: bool,
}

impl Config {
    /// The configuration the three variables' values ask for; anything
    /// unrecognised is the default, with a warning.
    pub fn parse(decorations: Option<&str>, menu: Option<&str>, control: Option<&str>) -> Config {
        let warn = |name: &str, value: &str| {
            eprintln!("rosetta3: appkit: {name}={value} is not recognised; using the default");
        };
        let decorations = match decorations.map(|v| v.trim().to_ascii_lowercase()) {
            None => Decorations::Client,
            Some(v) if v == "client" || v.is_empty() => Decorations::Client,
            Some(v) if v == "server" => Decorations::Server,
            Some(v) => {
                warn(DECORATIONS_ENV, &v);
                Decorations::Client
            }
        };
        let default_menu = match decorations {
            Decorations::Client => MenuPlacement::Titlebar,
            // There is no titlebar of this runtime's to put them in.
            Decorations::Server => MenuPlacement::Strip,
        };
        let menu = match menu.map(|v| v.trim().to_ascii_lowercase()) {
            None => default_menu,
            Some(v) if v.is_empty() => default_menu,
            Some(v) if v == "titlebar" => default_menu,
            Some(v) if v == "strip" => MenuPlacement::Strip,
            Some(v) if v == "none" => MenuPlacement::Hidden,
            Some(v) => {
                warn(MENU_ENV, &v);
                default_menu
            }
        };
        let control_shortcuts = !matches!(
            control.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
            Some("0" | "false" | "off" | "no")
        );
        Config {
            decorations,
            menu,
            control_shortcuts,
        }
    }
}

pub fn config() -> Config {
    static CONFIG: OnceLock<Config> = OnceLock::new();
    *CONFIG.get_or_init(|| {
        let read = |name| std::env::var(name).ok();
        Config::parse(
            read(DECORATIONS_ENV).as_deref(),
            read(MENU_ENV).as_deref(),
            read(MENU_CTRL_ENV).as_deref(),
        )
    })
}

/// Whether the chrome is drawn dark. The chrome is the host's, not the
/// guest's, so it follows the desktop even though AppKit here answers Aqua.
static DARK: AtomicBool = AtomicBool::new(false);

fn theme() -> &'static Theme {
    static LIGHT: OnceLock<Theme> = OnceLock::new();
    static DARK_THEME: OnceLock<Theme> = OnceLock::new();
    match DARK.load(Ordering::Relaxed) {
        true => DARK_THEME.get_or_init(Theme::dark),
        false => LIGHT.get_or_init(Theme::light),
    }
}

/// Follow the desktop's theme; true when it flipped. An unknown theme keeps
/// the light chrome.
fn follow_system_theme() -> bool {
    let dark = sdl3::video::system_theme() == sdl3::video::SystemTheme::Dark;
    DARK.swap(dark, Ordering::Relaxed) != dark
}

/// Set what has to be decided before SDL's video subsystem starts.
pub(crate) fn install() {
    if config().decorations == Decorations::Client {
        // libdecor's GTK plugin loads GTK 3 into the process, which cannot
        // share it with the GTK 4 that WebKit loads. Nothing of libdecor's is
        // needed once the chrome is drawn here.
        sdl3::video::set_hint(sdl3::video::HINT_WAYLAND_ALLOW_LIBDECOR, c"0");
        sdl3::video::set_hint(sdl3::video::HINT_WAYLAND_PREFER_LIBDECOR, c"0");
    }
}

/// What a window's chrome is drawn from, read off the window.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Facts {
    pub sdl: usize,
    pub titled: bool,
    pub closable: bool,
    pub miniaturizable: bool,
    pub resizable: bool,
    pub panel: bool,
    pub fullscreen: bool,
    pub shown: bool,
    pub key: bool,
    pub title: String,
}

/// The height of chrome above a window's content, in points.
pub(crate) fn inset_for(config: Config, titled: bool, panel: bool, fullscreen: bool) -> f64 {
    if !titled || fullscreen {
        return 0.0;
    }
    let theme = theme();
    let titlebar = match config.decorations {
        Decorations::Client => theme.titlebar_height,
        Decorations::Server => 0.0,
    };
    let strip = match config.menu == MenuPlacement::Strip && !panel {
        true => theme.strip_height,
        false => 0.0,
    };
    f64::from(titlebar + strip)
}

fn rows(config: Config, facts: &Facts) -> (f32, f32) {
    let theme = theme();
    let inset = inset_for(config, facts.titled, facts.panel, facts.fullscreen) as f32;
    let titlebar = match config.decorations == Decorations::Client && inset > 0.0 {
        true => theme.titlebar_height,
        false => 0.0,
    };
    (titlebar, inset - titlebar)
}

/// A window's chrome, painted: the top rows of its presentation.
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    /// Premultiplied B, G, R, A, top row first.
    pub pixels: Vec<u8>,
    /// Changes whenever the pixels do.
    pub generation: u64,
    /// The radius of the window's rounded corners in pixels, or 0 for square
    /// ones. The top corners are already cut out of `pixels`; the bottom ones
    /// are the presenter's to cut.
    pub radius: f32,
}

/// Keyed by SDL window address, for whichever thread presents it.
static PIXELS: Mutex<Option<HashMap<usize, Arc<Pixels>>>> = Mutex::new(None);

/// The chrome to composite over the top of `sdl`, if it has any.
pub fn pixels(sdl: sdl3::Window) -> Option<Arc<Pixels>> {
    PIXELS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()?
        .get(&(sdl as usize))
        .cloned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Titlebar,
    Strip,
}

/// Where the window manager may move or resize a window from, for SDL's hit
/// test, in window points.
#[derive(Clone, Debug, Default)]
struct HitMap {
    width: f32,
    height: f32,
    /// Everything above this that is not a control drags the window.
    drag_bottom: f32,
    controls: Vec<Rect>,
    resizable: bool,
    /// The window's `wl_surface`, on Wayland.
    surface: usize,
}

static HIT_MAPS: Mutex<Option<HashMap<usize, HitMap>>> = Mutex::new(None);

/// How far in from a window's edge a drag resizes it.
const RESIZE_EDGE: f32 = 5.0;

unsafe extern "C" fn hit_test(
    window: sdl3::Window,
    point: *const sdl3::video::Point,
    _data: *mut std::ffi::c_void,
) -> sdl3::video::HitTestResult {
    use sdl3::video::HitTestResult as R;
    // SAFETY: SDL passes a live point for the duration of the call.
    let Some(point) = (unsafe { point.as_ref() }) else {
        return R::Normal;
    };
    let maps = HIT_MAPS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = maps.as_ref().and_then(|maps| maps.get(&(window as usize))) else {
        return R::Normal;
    };
    classify(map, window, point.x as f32, point.y as f32)
}

fn classify(map: &HitMap, window: sdl3::Window, x: f32, y: f32) -> sdl3::video::HitTestResult {
    use sdl3::video::HitTestResult as R;
    let maximized = sdl3::video::window_flags(window) & sdl3::video::flags::MAXIMIZED != 0;
    if map.resizable && !maximized {
        let left = x < RESIZE_EDGE;
        let right = x >= map.width - RESIZE_EDGE;
        let top = y < RESIZE_EDGE;
        let bottom = y >= map.height - RESIZE_EDGE;
        match (top, bottom, left, right) {
            (true, _, true, _) => return R::ResizeTopLeft,
            (true, _, _, true) => return R::ResizeTopRight,
            (_, true, true, _) => return R::ResizeBottomLeft,
            (_, true, _, true) => return R::ResizeBottomRight,
            (true, ..) => return R::ResizeTop,
            (_, true, ..) => return R::ResizeBottom,
            (_, _, true, _) => return R::ResizeLeft,
            (_, _, _, true) => return R::ResizeRight,
            _ => {}
        }
    }
    let at = Point::new(x, y);
    if y < map.drag_bottom && !map.controls.iter().any(|rect| rect.contains(at)) {
        return R::Draggable;
    }
    R::Normal
}

/// SDL windows a presenter shows opaque whatever their alpha, whose corners
/// therefore stay square.
static OPAQUE: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// `sdl` is presented through a path that ignores alpha from now on. Its
/// corners are drawn square: cut out, they would show black.
pub fn presented_opaque(sdl: sdl3::Window) {
    let mut opaque = OPAQUE.lock().unwrap_or_else(|e| e.into_inner());
    if !opaque.contains(&(sdl as usize)) {
        opaque.push(sdl as usize);
    }
}

/// Drop everything kept for an SDL window that is being destroyed: its
/// presenter goes with it, and its address may be reused.
pub(crate) fn destroyed(sdl: sdl3::Window) {
    forget_sdl(sdl);
    OPAQUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|opaque| *opaque != sdl as usize);
}

/// Whether a decorated window's corners are rounded: it has to be see-through
/// where they are cut, and a maximized window fills its space edge to edge.
fn rounds(sdl: sdl3::Window) -> bool {
    let flags = sdl3::video::window_flags(sdl);
    flags & sdl3::video::flags::TRANSPARENT != 0
        && flags & (sdl3::video::flags::MAXIMIZED | sdl3::video::flags::FULLSCREEN) == 0
        && !OPAQUE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&(sdl as usize))
}

/// The last press on a titlebar outside its controls, which the window
/// manager took for a drag, as (SDL window, x, y, milliseconds).
static LAST_TITLEBAR_PRESS: Mutex<Option<(usize, f32, f32, u64)>> = Mutex::new(None);

/// SDL's defaults for what makes two presses one double click.
const DOUBLE_CLICK_MILLISECONDS: u64 = 500;
const DOUBLE_CLICK_RADIUS: f32 = 32.0;

/// A press the window manager was handed at `x`, `y` of `sdl`; true when it
/// is the second of a double click, which zooms the window as a Mac's
/// titlebar does.
fn titlebar_pressed(sdl: sdl3::Window, x: f32, y: f32, time: u64) -> bool {
    let draggable = HIT_MAPS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|maps| maps.get(&(sdl as usize)))
        .is_some_and(|map| classify(map, sdl, x, y) == sdl3::video::HitTestResult::Draggable);
    let mut last = LAST_TITLEBAR_PRESS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let double = draggable
        && last.is_some_and(|(window, last_x, last_y, last_time)| {
            window == sdl as usize
                && time.wrapping_sub(last_time) <= DOUBLE_CLICK_MILLISECONDS
                && (x - last_x).hypot(y - last_y) <= DOUBLE_CLICK_RADIUS
        });
    *last = match draggable && !double {
        true => Some((sdl as usize, x, y, time)),
        false => None,
    };
    double
}

fn zoom(sdl: sdl3::Window) {
    let window = crate::event::object_for(sdl3::video::window_id(sdl));
    if !window.is_null() {
        send(window, "zoom:");
    }
}

/// SDL reported a press its hit test gave the window manager, which only X11
/// does; the pointer has not moved since.
pub(crate) fn hit_test_pressed(id: u32) {
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    let window = crate::event::object_for(id);
    let Some((x, y, _)) = sdl3::video::mouse_state() else {
        return;
    };
    if window.is_null() {
        return;
    }
    // SAFETY: a window this crate made.
    let sdl = unsafe { crate::window::sdl_window(window) };
    let time = START.get_or_init(Instant::now).elapsed().as_millis() as u64;
    if titlebar_pressed(sdl, x, y, time) {
        zoom(sdl);
    }
}

/// A double click on a titlebar whose zoom waits for the compositor to
/// finish the move its second press began, as (SDL window, when).
static PENDING_ZOOM: Mutex<Option<(usize, std::time::Instant)>> = Mutex::new(None);

/// How long after a double click the move it began may take to end for the
/// zoom still to happen. Longer, and the second press was the start of a drag.
const PENDING_ZOOM_LIMIT: std::time::Duration = std::time::Duration::from_millis(500);

/// The pointer events a Wayland compositor delivered since the last pump. SDL
/// never reports a press on a titlebar, so they are watched for separately.
///
/// The second press of a double click has already started an interactive
/// move by the time it is seen here, and a compositor asked to maximize a
/// window it is moving resizes it without maximizing it. So the zoom waits
/// for the move to end: the release, or the pointer coming back.
fn wayland_pointer_events() {
    use sdl3::wayland::PointerEvent;
    let sdl_for = |surface: usize| {
        HIT_MAPS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|maps| {
                maps.iter()
                    .find(|(_, map)| map.surface == surface)
                    .map(|(sdl, _)| *sdl)
            })
    };
    let mut pending = PENDING_ZOOM.lock().unwrap_or_else(|e| e.into_inner());
    for event in sdl3::wayland::take_events() {
        match event {
            PointerEvent::Press {
                surface,
                x,
                y,
                time,
            } => {
                *pending = sdl_for(surface).and_then(|sdl| {
                    titlebar_pressed(sdl as sdl3::Window, x, y, u64::from(time))
                        .then(|| (sdl, std::time::Instant::now()))
                });
            }
            PointerEvent::Release | PointerEvent::Enter { .. } => {
                if let Some((sdl, since)) = pending.take()
                    && since.elapsed() <= PENDING_ZOOM_LIMIT
                {
                    zoom(sdl as sdl3::Window);
                }
            }
        }
    }
    if pending.is_some_and(|(_, since)| since.elapsed() > PENDING_ZOOM_LIMIT) {
        *pending = None;
    }
}

/// One decorated window's chrome, as last painted.
struct Chrome {
    sdl: usize,
    size: (u32, u32),
    titlebar: Option<Bar>,
    strip: Option<Bar>,
    titlebar_layout: BarLayout,
    strip_layout: BarLayout,
    /// Where the strip starts, in points.
    strip_top: f32,
    hover: Option<(Row, Part)>,
    pressed: Option<(Row, Part)>,
    dark: bool,
    rounded: bool,
    generation: u64,
}

/// Keyed by `NSWindow` address. Touched only from the event pump.
static CHROMES: Mutex<Option<HashMap<usize, Chrome>>> = Mutex::new(None);

fn with_chromes<T>(f: impl FnOnce(&mut HashMap<usize, Chrome>) -> T) -> T {
    let mut chromes = CHROMES.lock().unwrap_or_else(|e| e.into_inner());
    f(chromes.get_or_insert_with(HashMap::new))
}

/// The main menu's items that show in the bar, and the titles they show.
///
/// The first is the application menu, which AppKit titles with the
/// application's name whatever the item itself says.
fn menu_titles() -> Vec<(Id, MenuTitle)> {
    let main = crate::menu::main_menu();
    if main.is_null() {
        return Vec::new();
    }
    crate::menu::shown_items(main)
        .into_iter()
        .enumerate()
        .map(|(at, item)| {
            let title = match at {
                0 => crate::metadata::name().to_string_lossy().into_owned(),
                _ => crate::menu::display_title(item),
            };
            let enabled = crate::menu::is_enabled_flag(item);
            (item, MenuTitle { title, enabled })
        })
        .collect()
}

fn models(config: Config, facts: &Facts, window: Id) -> (Option<Bar>, Option<Bar>) {
    let (titlebar_height, strip_height) = rows(config, facts);
    let menus: Vec<MenuTitle> = match config.menu == MenuPlacement::Hidden || facts.panel {
        true => Vec::new(),
        false => menu_titles().into_iter().map(|(_, title)| title).collect(),
    };
    let open = TRACKING.with(|tracking| {
        tracking
            .borrow()
            .as_ref()
            .filter(|t| t.window == window)
            .map(|t| (t.row, t.title))
    });
    // A window whose menu is open is still the one being worked in, even
    // when the host gave the menu the keyboard.
    let active = facts.key || open.is_some();
    let titlebar = (titlebar_height > 0.0).then(|| Bar {
        title: facts.title.clone(),
        show_title: true,
        lights: Some(Lights {
            closable: facts.closable,
            minimizable: facts.miniaturizable,
            zoomable: facts.resizable,
        }),
        menus: match strip_height > 0.0 {
            true => Vec::new(),
            false => menus.clone(),
        },
        active,
        open: open.and_then(|(row, title)| (row == Row::Titlebar).then_some(title)),
        separator: strip_height <= 0.0,
        ..Bar::default()
    });
    let strip = (strip_height > 0.0).then(|| Bar {
        menus,
        active,
        open: open.and_then(|(row, title)| (row == Row::Strip).then_some(title)),
        separator: true,
        ..Bar::default()
    });
    (titlebar, strip)
}

/// Bring every decorated window's chrome up to date, before the stores are
/// composed.
pub(crate) fn pump() {
    let config = config();
    wayland_pointer_events();
    if follow_system_theme() {
        repaint_menus();
    }
    let dark = DARK.load(Ordering::Relaxed);
    let mut live = Vec::new();
    for window in crate::window::all() {
        let Some(facts) = crate::window::facts(window) else {
            continue;
        };
        let (titlebar_height, strip_height) = rows(config, &facts);
        if !facts.shown || titlebar_height + strip_height <= 0.0 {
            continue;
        }
        let sdl = facts.sdl as sdl3::Window;
        let (Some((width, height)), Some((pixel_width, _))) = (
            sdl3::video::window_size(sdl),
            sdl3::video::window_size_in_pixels(sdl),
        ) else {
            continue;
        };
        live.push(window as usize);
        let (mut titlebar, mut strip) = models(config, &facts, window);
        let scale = match width > 0 {
            true => pixel_width as f32 / width as f32,
            false => 1.0,
        };
        let inset = titlebar_height + strip_height;
        let rounded = config.decorations == Decorations::Client && rounds(sdl);
        let size = (
            pixel_width.max(1) as u32,
            (inset * scale).round().max(1.0) as u32,
        );
        let painted = with_chromes(|chromes| {
            let existing = chromes.get(&(window as usize));
            let (hover, pressed) = existing.map_or((None, None), |c| (c.hover, c.pressed));
            for (row, bar) in [(Row::Titlebar, &mut titlebar), (Row::Strip, &mut strip)] {
                if let Some(bar) = bar {
                    bar.hover = hover.filter(|(r, _)| *r == row).map(|(_, p)| p);
                    bar.pressed = pressed.filter(|(r, _)| *r == row).map(|(_, p)| p);
                }
            }
            let unchanged = existing.is_some_and(|c| {
                c.sdl == sdl as usize
                    && c.dark == dark
                    && c.rounded == rounded
                    && c.size == size
                    && c.titlebar == titlebar
                    && c.strip == strip
            });
            if unchanged {
                return None;
            }
            let theme = theme();
            let points = width as f32;
            let titlebar_layout = titlebar.as_ref().map_or_else(BarLayout::default, |bar| {
                bar.layout(Rect::new(0.0, 0.0, points, titlebar_height), theme)
            });
            let strip_layout = strip.as_ref().map_or_else(BarLayout::default, |bar| {
                bar.layout(Rect::new(0.0, titlebar_height, points, strip_height), theme)
            });
            let mut pixels = vec![0u8; size.0 as usize * size.1 as usize * 4];
            {
                let mut canvas = Canvas::new(
                    &mut pixels,
                    size.0 as usize,
                    size.1 as usize,
                    size.0 as usize * 4,
                    scale,
                );
                if let Some(bar) = &titlebar {
                    bar.paint(&mut canvas, &titlebar_layout, theme);
                }
                if let Some(bar) = &strip {
                    bar.paint(&mut canvas, &strip_layout, theme);
                }
            }
            let radius = match rounded {
                true => theme.window_corner_radius * scale,
                false => 0.0,
            };
            chrome::corners::clip(
                &mut pixels,
                size.0 as usize,
                size.1 as usize,
                size.0 as usize * 4,
                radius,
                true,
                false,
            );
            let generation = existing.map_or(1, |c| c.generation + 1);
            let controls: Vec<Rect> = titlebar_layout
                .parts
                .iter()
                .chain(&strip_layout.parts)
                .map(|(_, rect)| *rect)
                .collect();
            chromes.insert(
                window as usize,
                Chrome {
                    sdl: sdl as usize,
                    size,
                    titlebar,
                    strip,
                    titlebar_layout,
                    strip_layout,
                    strip_top: titlebar_height,
                    hover,
                    pressed,
                    dark,
                    rounded,
                    generation,
                },
            );
            Some((
                Pixels {
                    width: size.0,
                    height: size.1,
                    pixels,
                    generation,
                    radius,
                },
                controls,
            ))
        });
        let Some((pixels, controls)) = painted else {
            continue;
        };
        PIXELS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(sdl as usize, Arc::new(pixels));
        if config.decorations == Decorations::Client {
            let wayland = sdl3::video::wayland_surface(sdl);
            if let Some((display, _)) = wayland {
                sdl3::wayland::watch(display);
            }
            let map = HitMap {
                width: width as f32,
                height: height as f32,
                drag_bottom: inset,
                controls,
                resizable: facts.resizable,
                surface: wayland.map_or(0, |(_, surface)| surface as usize),
            };
            let fresh = HIT_MAPS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_insert_with(HashMap::new)
                .insert(sdl as usize, map)
                .is_none();
            if fresh {
                sdl3::video::set_window_hit_test(sdl, Some(hit_test));
            }
        }
        crate::backing::chrome_changed(sdl);
    }
    // Windows that closed, lost their titlebar or went full screen.
    let gone: Vec<(usize, usize)> = with_chromes(|chromes| {
        let gone: Vec<(usize, usize)> = chromes
            .iter()
            .filter(|(window, _)| !live.contains(window))
            .map(|(window, chrome)| (*window, chrome.sdl))
            .collect();
        for (window, _) in &gone {
            chromes.remove(window);
        }
        gone
    });
    for (_, sdl) in gone {
        forget_sdl(sdl as sdl3::Window);
    }
}

/// Drop everything kept for an SDL window, before it is destroyed or when it
/// no longer has chrome.
pub(crate) fn forget_sdl(sdl: sdl3::Window) {
    let had_pixels = PIXELS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|pixels| pixels.remove(&(sdl as usize)))
        .is_some();
    let had_map = HIT_MAPS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|maps| maps.remove(&(sdl as usize)))
        .is_some();
    if had_map {
        sdl3::video::set_window_hit_test(sdl, None);
    }
    if had_pixels {
        crate::backing::chrome_changed(sdl);
    }
}

/// Which row and part of a window's chrome a point in window points is on.
fn locate(window: Id, x: f32, y: f32) -> Option<(Row, Option<Part>)> {
    with_chromes(|chromes| {
        let chrome = chromes.get(&(window as usize))?;
        let point = Point::new(x, y);
        let in_titlebar = chrome.titlebar.is_some() && y < chrome.strip_top;
        let in_strip = chrome.strip.is_some()
            && y >= chrome.strip_top
            && y < chrome.strip_layout.bounds.bottom();
        match (in_titlebar, in_strip) {
            (true, _) => Some((
                Row::Titlebar,
                chrome
                    .titlebar
                    .as_ref()
                    .and_then(|bar| bar.hit(&chrome.titlebar_layout, point)),
            )),
            (_, true) => Some((
                Row::Strip,
                chrome
                    .strip
                    .as_ref()
                    .and_then(|bar| bar.hit(&chrome.strip_layout, point)),
            )),
            _ => None,
        }
    })
}

fn set_hover(window: Id, hover: Option<(Row, Part)>) {
    with_chromes(|chromes| {
        if let Some(chrome) = chromes.get_mut(&(window as usize)) {
            chrome.hover = hover;
        }
    });
}

fn set_pressed(window: Id, pressed: Option<(Row, Part)>) -> Option<(Row, Part)> {
    with_chromes(|chromes| {
        chromes
            .get_mut(&(window as usize))
            .and_then(|chrome| std::mem::replace(&mut chrome.pressed, pressed))
    })
}

fn part_rect(window: Id, row: Row, part: Part) -> Option<Rect> {
    with_chromes(|chromes| {
        let chrome = chromes.get(&(window as usize))?;
        match row {
            Row::Titlebar => chrome.titlebar_layout.rect_of(part),
            Row::Strip => chrome.strip_layout.rect_of(part),
        }
    })
}

/// Pointer motion; true when the chrome took it and the guest must not see
/// it.
pub(crate) fn pointer_moved(id: u32, x: f32, y: f32) -> bool {
    if tracking_popup(id) {
        tracking_motion(id, x, y);
        return true;
    }
    let window = crate::event::object_for(id);
    if window.is_null() {
        return false;
    }
    let Some((row, part)) = locate(window, x, y) else {
        set_hover(window, None);
        return false;
    };
    let hover = part.map(|part| (row, part));
    set_hover(window, hover);
    // With a menu open, moving across the other titles opens theirs.
    if let Some((row, Part::Menu(title))) = hover
        && tracking_window() == Some(window)
        && tracking_title() != Some((row, title))
    {
        open_menu(window, row, title);
    }
    true
}

/// The pointer left a window.
pub(crate) fn pointer_left(id: u32) {
    let window = crate::event::object_for(id);
    if !window.is_null() {
        set_hover(window, None);
    }
}

/// A button press or release; true when the chrome took it.
pub(crate) fn pointer_button(id: u32, button: u8, down: bool, clicks: u8, x: f32, y: f32) -> bool {
    const LEFT: u8 = 1;
    const RIGHT: u8 = 3;
    if tracking_popup(id) {
        if !down && button == LEFT {
            tracking_release(id, x, y);
        }
        return true;
    }
    let window = crate::event::object_for(id);
    let located = match window.is_null() {
        true => None,
        false => locate(window, x, y),
    };
    // A press anywhere but on a menu title ends menu tracking, and does
    // nothing else: it is the click that dismisses the menu.
    if down && tracking_window().is_some() {
        let on_title = matches!(located, Some((_, Some(Part::Menu(_) | Part::Overflow(_)))));
        if !on_title {
            close_menus();
            return true;
        }
    }
    let Some((row, part)) = located else {
        // A release that started in the chrome still belongs to it.
        return !down && set_pressed(window, None).is_some();
    };
    match (down, part) {
        (true, Some(part @ (Part::Close | Part::Minimize | Part::Zoom))) if button == LEFT => {
            set_pressed(window, Some((row, part)));
        }
        (true, Some(Part::Menu(title))) if button == LEFT => {
            match tracking_title() == Some((row, title)) && tracking_window() == Some(window) {
                true => close_menus(),
                false => open_menu(window, row, title),
            }
        }
        (true, Some(Part::Overflow(first))) if button == LEFT => {
            match tracking_window() == Some(window) {
                true => close_menus(),
                false => open_overflow(window, row, first),
            }
        }
        (true, None) if button == LEFT && clicks >= 2 && row == Row::Titlebar => {
            send(window, "zoom:");
        }
        (true, None) if button == RIGHT && row == Row::Titlebar => {
            // SAFETY: a window this crate made.
            let sdl = unsafe { crate::window::sdl_window(window) };
            sdl3::video::show_window_system_menu(sdl, x as i32, y as i32);
        }
        (false, part) => {
            let pressed = set_pressed(window, None);
            if let (Some((_, pressed)), Some(part)) = (pressed, part)
                && pressed == part
            {
                match part {
                    Part::Close => send(window, "performClose:"),
                    Part::Minimize => send(window, "performMiniaturize:"),
                    Part::Zoom => send(window, "performZoom:"),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    true
}

/// Send `-selector:` with the window as sender, through the real dispatch
/// so a subclass's override runs.
fn send(window: Id, selector: &str) {
    // SAFETY: every selector sent here is `void (id, SEL, id)` on NSWindow.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(window, objc::sel::get(selector), window);
    }
}

/// A key while a menu is open; true when the menu took it.
pub(crate) fn key(scancode: u32, down: bool) -> bool {
    use sdl3::keycodes::scancode as sc;
    if tracking_window().is_none() {
        return false;
    }
    if !down {
        return true;
    }
    match scancode {
        sc::ESCAPE => close_menus(),
        sc::UP => move_highlight(-1),
        sc::DOWN => move_highlight(1),
        sc::RIGHT => enter_or_next(1),
        sc::LEFT => leave_or_next(-1),
        sc::RETURN | sc::KP_ENTER | sc::SPACE => choose_highlighted(),
        _ => {}
    }
    true
}

/// A window was asked to close; true when it was one of the menus'.
pub(crate) fn window_closed(id: u32) -> bool {
    if tracking_popup(id) {
        close_menus();
        return true;
    }
    false
}

/// Focus moved to a window of the program's other than the one tracking a
/// menu, which is the program being used elsewhere.
pub(crate) fn focus_changed(id: u32, gained: bool) {
    if !gained || tracking_popup(id) {
        return;
    }
    let window = crate::event::object_for(id);
    if !window.is_null() && tracking_window().is_some_and(|tracking| tracking != window) {
        close_menus();
    }
}

// Menu tracking.

/// One open menu panel.
struct Level {
    /// The items shown, retained, in panel order.
    items: Vec<Id>,
    /// The menu each item was read from, for its delegate.
    menu: Id,
    panel: Panel,
    layout: PanelLayout,
    popup: sdl3::Window,
    popup_id: u32,
}

struct Tracking {
    window: Id,
    row: Row,
    /// The bar title the menus hang from.
    title: usize,
    levels: Vec<Level>,
    /// The panel the arrow keys move in: the one the pointer was last in,
    /// or the one the keyboard last entered.
    focus: usize,
}

thread_local! {
    static TRACKING: RefCell<Option<Tracking>> = const { RefCell::new(None) };
}

/// The innermost open menu's popup, for a script to drive.
pub(crate) fn menu_popup() -> Option<sdl3::Window> {
    TRACKING.with(|t| t.borrow().as_ref()?.levels.last().map(|level| level.popup))
}

fn tracking_window() -> Option<Id> {
    TRACKING.with(|t| t.borrow().as_ref().map(|t| t.window))
}

fn tracking_title() -> Option<(Row, usize)> {
    TRACKING.with(|t| t.borrow().as_ref().map(|t| (t.row, t.title)))
}

fn tracking_popup(id: u32) -> bool {
    TRACKING.with(|t| {
        t.borrow()
            .as_ref()
            .is_some_and(|t| t.levels.iter().any(|level| level.popup_id == id))
    })
}

/// Open the menu under a bar title, closing whichever was open.
fn open_menu(window: Id, row: Row, title: usize) {
    close_menus();
    let titles = menu_titles();
    let Some((item, _)) = titles.get(title) else {
        return;
    };
    let submenu = crate::menu::submenu_of(*item);
    if submenu.is_null() {
        return;
    }
    let Some(anchor) = part_rect(window, row, Part::Menu(title)) else {
        return;
    };
    let items = crate::menu::prepare(submenu);
    begin(window, row, title, anchor, submenu, items);
}

/// Open the chevron's panel: the bar titles that did not fit, each leading to
/// its menu.
fn open_overflow(window: Id, row: Row, first: usize) {
    close_menus();
    let main = crate::menu::main_menu();
    let Some(anchor) = part_rect(window, row, Part::Overflow(first)) else {
        return;
    };
    let items: Vec<Id> = crate::menu::shown_items(main)
        .into_iter()
        .skip(first)
        .collect();
    begin(window, row, first, anchor, main, items);
}

fn begin(window: Id, row: Row, title: usize, anchor: Rect, menu: Id, items: Vec<Id>) {
    // SAFETY: a window this crate made.
    let parent = unsafe { crate::window::sdl_window(window) };
    let Some(level) = make_level(parent, anchor.x, anchor.bottom(), menu, items) else {
        return;
    };
    TRACKING.with(|t| {
        *t.borrow_mut() = Some(Tracking {
            window,
            row,
            title,
            levels: vec![level],
            focus: 0,
        })
    });
}

/// Build a panel for `items` and show it in a popup of `parent` at the given
/// offset.
fn make_level(parent: sdl3::Window, x: f32, y: f32, menu: Id, items: Vec<Id>) -> Option<Level> {
    if parent.is_null() {
        crate::menu::release_items(items);
        return None;
    }
    let panel = Panel {
        items: items.iter().map(|item| panel_item(*item)).collect(),
        highlighted: None,
    };
    let theme = theme();
    let layout = panel.layout(Rect::new(0.0, 0.0, 0.0, 0.0), theme);
    let flags = sdl3::video::flags::POPUP_MENU | sdl3::video::flags::HIGH_PIXEL_DENSITY;
    let Some(popup) = sdl3::video::create_popup_window(
        parent,
        x.round() as i32,
        y.round() as i32,
        layout.bounds.width.ceil() as i32,
        layout.bounds.height.ceil() as i32,
        flags,
    ) else {
        crate::window::trace(format_args!(
            "a menu's popup could not be made: {}",
            sdl3::error()
        ));
        crate::menu::release_items(items);
        return None;
    };
    crate::menu::will_open(menu);
    let level = Level {
        items,
        menu,
        panel,
        layout,
        popup,
        popup_id: sdl3::video::window_id(popup),
    };
    paint_level(&level);
    Some(level)
}

fn panel_item(item: Id) -> Item {
    if crate::menu::is_separator(item) {
        return Item::separator();
    }
    let submenu = !crate::menu::submenu_of(item).is_null();
    Item {
        title: crate::menu::display_title(item),
        shortcut: crate::menu::shortcut_label(item),
        enabled: crate::menu::is_item_enabled(item),
        state: match crate::menu::state_of(item) {
            1 => chrome::menu::State::On,
            -1 => chrome::menu::State::Mixed,
            _ => chrome::menu::State::Off,
        },
        submenu,
        separator: false,
        indent: crate::menu::indentation(item).min(15) as u8,
    }
}

fn paint_level(level: &Level) {
    let theme = theme();
    let Some((width, _)) = sdl3::video::window_size(level.popup) else {
        return;
    };
    sdl3::video::present_window(level.popup, |surface| {
        let scale = match width > 0 {
            true => surface.width as f32 / width as f32,
            false => 1.0,
        };
        let length = surface.pitch as usize * surface.height as usize;
        // SAFETY: SDL reported `pitch` bytes per row for `height` rows, and
        // the surface is locked for the duration of this call.
        let pixels = unsafe { std::slice::from_raw_parts_mut(surface.pixels, length) };
        let mut canvas = Canvas::new(
            pixels,
            surface.width as usize,
            surface.height as usize,
            surface.pitch as usize,
            scale,
        );
        level.panel.paint(&mut canvas, &level.layout, theme);
        dump_panel(level.popup_id, surface, pixels);
        true
    });
}

/// Repaint the open menus, after the theme changed under them.
fn repaint_menus() {
    TRACKING.with(|tracking| {
        if let Some(tracking) = tracking.borrow().as_ref() {
            tracking.levels.iter().for_each(paint_level);
        }
    });
}

/// `ROSETTA3_STORE_OUT=DIR` writes each menu panel as it is painted, beside
/// the window stores, for a session with no screen to look at.
fn dump_panel(id: u32, surface: &sdl3::video::Presentation, pixels: &[u8]) {
    let Some(directory) = std::env::var_os("ROSETTA3_STORE_OUT") else {
        return;
    };
    let (width, height) = (surface.width as usize, surface.height as usize);
    let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
    for row in 0..height {
        let start = row * surface.pitch as usize;
        for texel in pixels[start..start + width * 4].as_chunks::<4>().0 {
            out.extend_from_slice(&[texel[2], texel[1], texel[0]]);
        }
    }
    let _ = std::fs::write(
        std::path::Path::new(&directory).join(format!("menu-{id}.ppm")),
        out,
    );
}

/// Tear down every open panel, telling each menu's delegate.
pub(crate) fn close_menus() {
    let Some(tracking) = TRACKING.with(|t| t.borrow_mut().take()) else {
        return;
    };
    for level in tracking.levels.into_iter().rev() {
        close_level(level);
    }
}

fn close_level(level: Level) {
    sdl3::video::destroy_window(level.popup);
    crate::menu::did_close(level.menu);
    crate::menu::release_items(level.items);
}

/// Close every panel deeper than `depth`.
fn truncate_levels(depth: usize) {
    let closed: Vec<Level> = TRACKING.with(|t| {
        t.borrow_mut()
            .as_mut()
            .map(|t| {
                t.focus = t.focus.min(depth.saturating_sub(1));
                t.levels.split_off(depth.min(t.levels.len()))
            })
            .unwrap_or_default()
    });
    for level in closed.into_iter().rev() {
        close_level(level);
    }
}

/// Change the highlighted row of the panel at `depth`, and open or close the
/// submenu that goes with it.
fn highlight(depth: usize, row: Option<usize>) {
    let changed = TRACKING.with(|t| {
        let mut t = t.borrow_mut();
        let level = t.as_mut()?.levels.get_mut(depth)?;
        if level.panel.highlighted == row {
            return Some(false);
        }
        level.panel.highlighted = row;
        paint_level(level);
        Some(true)
    });
    if changed != Some(true) {
        return;
    }
    truncate_levels(depth + 1);
    let Some(row) = row else {
        return;
    };
    // The highlighted row's submenu opens beside it.
    let child = TRACKING.with(|t| {
        let t = t.borrow();
        let level = t.as_ref()?.levels.get(depth)?;
        let item = *level.items.get(row)?;
        let submenu = crate::menu::submenu_of(item);
        (!submenu.is_null() && level.panel.selectable(row)).then(|| {
            let rect = level.layout.rows[row];
            let theme = theme();
            (
                level.popup,
                level.layout.bounds.width - 4.0,
                rect.y - theme.panel_padding,
                submenu,
            )
        })
    });
    let Some((parent, x, y, submenu)) = child else {
        return;
    };
    let items = crate::menu::prepare(submenu);
    if let Some(level) = make_level(parent, x, y, submenu, items) {
        TRACKING.with(|t| {
            if let Some(t) = t.borrow_mut().as_mut() {
                t.levels.push(level);
            }
        });
    }
}

fn level_of_popup(id: u32) -> Option<usize> {
    TRACKING.with(|t| {
        t.borrow()
            .as_ref()?
            .levels
            .iter()
            .position(|level| level.popup_id == id)
    })
}

fn row_at(depth: usize, x: f32, y: f32) -> Option<usize> {
    TRACKING.with(|t| {
        let t = t.borrow();
        let level = t.as_ref()?.levels.get(depth)?;
        level.panel.hit(&level.layout, Point::new(x, y))
    })
}

fn tracking_motion(id: u32, x: f32, y: f32) {
    let Some(depth) = level_of_popup(id) else {
        return;
    };
    TRACKING.with(|t| {
        if let Some(t) = t.borrow_mut().as_mut() {
            t.focus = depth;
        }
    });
    let row = row_at(depth, x, y);
    // Leaving a panel for its submenu keeps the row that opened it lit.
    if row.is_none()
        && depth + 1 < TRACKING.with(|t| t.borrow().as_ref().map_or(0, |t| t.levels.len()))
    {
        return;
    }
    highlight(depth, row);
}

fn tracking_release(id: u32, x: f32, y: f32) {
    let Some(depth) = level_of_popup(id) else {
        return;
    };
    if let Some(row) = row_at(depth, x, y) {
        choose(depth, row);
    }
}

/// Choose the item at `row` of the panel at `depth`, if it can be chosen.
fn choose(depth: usize, row: usize) {
    let item = TRACKING.with(|t| {
        let t = t.borrow();
        let level = t.as_ref()?.levels.get(depth)?;
        let item = *level.items.get(row)?;
        (level.panel.selectable(row) && crate::menu::submenu_of(item).is_null())
            .then(|| objc::arc::retain(item))
    });
    let Some(item) = item else {
        return;
    };
    // Closed before the action runs: an action may run a modal loop, which
    // pumps events, and the menu must not be open underneath it.
    close_menus();
    crate::menu::perform(item);
    objc::arc::release(item);
}

fn choose_highlighted() {
    let highlighted = TRACKING.with(|t| {
        let t = t.borrow();
        let t = t.as_ref()?;
        Some((t.focus, t.levels.get(t.focus)?.panel.highlighted?))
    });
    if let Some((depth, row)) = highlighted {
        choose(depth, row);
    }
}

/// Move the focused panel's highlight by one row that can be highlighted.
fn move_highlight(step: isize) {
    let target = TRACKING.with(|t| {
        let t = t.borrow();
        let t = t.as_ref()?;
        let panel = &t.levels.get(t.focus)?.panel;
        let count = panel.items.len() as isize;
        if count == 0 {
            return None;
        }
        let start = panel
            .highlighted
            .map_or(if step > 0 { -1 } else { count }, |at| at as isize);
        (1..=count)
            .map(|n| (start + step * n).rem_euclid(count) as usize)
            .find(|at| panel.selectable(*at) || panel.items[*at].submenu)
            .map(|at| (t.focus, at))
    });
    if let Some((depth, row)) = target {
        highlight(depth, Some(row));
    }
}

/// Right arrow: into the open submenu, or on to the next bar title.
fn enter_or_next(step: isize) {
    let child = TRACKING.with(|t| {
        let mut t = t.borrow_mut();
        let t = t.as_mut()?;
        (t.focus + 1 < t.levels.len()).then(|| {
            t.focus += 1;
            t.focus
        })
    });
    match child {
        Some(depth) => highlight_first(depth),
        None => next_title(step),
    }
}

/// Left arrow: out of a submenu, or back to the previous bar title.
fn leave_or_next(step: isize) {
    let focus = TRACKING.with(|t| t.borrow().as_ref().map_or(0, |t| t.focus));
    match focus > 0 {
        true => truncate_levels(focus),
        false => next_title(step),
    }
}

fn highlight_first(depth: usize) {
    let row = TRACKING.with(|t| {
        let t = t.borrow();
        let panel = &t.as_ref()?.levels.get(depth)?.panel;
        (0..panel.items.len()).find(|at| panel.selectable(*at) || panel.items[*at].submenu)
    });
    if row.is_some() {
        highlight(depth, row);
    }
}

/// Open the bar title `step` along from the open one.
fn next_title(step: isize) {
    let Some((window, (row, title))) = tracking_window().zip(tracking_title()) else {
        return;
    };
    let count = with_chromes(|chromes| {
        let chrome = chromes.get(&(window as usize))?;
        let layout = match row {
            Row::Titlebar => &chrome.titlebar_layout,
            Row::Strip => &chrome.strip_layout,
        };
        Some(
            layout
                .parts
                .iter()
                .filter(|(part, _)| matches!(part, Part::Menu(_)))
                .count(),
        )
    });
    let Some(count) = count.filter(|count| *count > 0) else {
        return;
    };
    let next = (title as isize + step).rem_euclid(count as isize) as usize;
    open_menu(window, row, next);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_decorations_are_the_default() {
        let config = Config::parse(None, None, None);
        assert_eq!(config.decorations, Decorations::Client);
        assert_eq!(config.menu, MenuPlacement::Titlebar);
        assert!(config.control_shortcuts);
    }

    #[test]
    fn server_decorations_put_the_menus_in_a_strip() {
        let config = Config::parse(Some("server"), None, None);
        assert_eq!(config.menu, MenuPlacement::Strip);
        // The titlebar is the host's, so there is nowhere else to put them.
        let config = Config::parse(Some("server"), Some("titlebar"), None);
        assert_eq!(config.menu, MenuPlacement::Strip);
        let config = Config::parse(Some("Server"), Some("none"), Some("0"));
        assert_eq!(config.menu, MenuPlacement::Hidden);
        assert!(!config.control_shortcuts);
    }

    #[test]
    fn an_unknown_value_is_the_default() {
        let config = Config::parse(Some("sideways"), Some("everywhere"), Some("maybe"));
        assert_eq!(config, Config::parse(None, None, None));
    }

    #[test]
    fn the_inset_is_the_rows_a_window_draws() {
        let theme = theme();
        let client = Config::parse(None, None, None);
        let titlebar = f64::from(theme.titlebar_height);
        let strip = f64::from(theme.strip_height);
        assert_eq!(inset_for(client, true, false, false), titlebar);
        assert_eq!(inset_for(client, false, false, false), 0.0);
        assert_eq!(inset_for(client, true, false, true), 0.0, "full screen");

        let client_strip = Config::parse(None, Some("strip"), None);
        assert_eq!(
            inset_for(client_strip, true, false, false),
            titlebar + strip
        );
        // A panel has no menu bar of its own.
        assert_eq!(inset_for(client_strip, true, true, false), titlebar);

        let server = Config::parse(Some("server"), None, None);
        assert_eq!(inset_for(server, true, false, false), strip);
        assert_eq!(inset_for(server, true, true, false), 0.0);
        let server_bare = Config::parse(Some("server"), Some("none"), None);
        assert_eq!(inset_for(server_bare, true, false, false), 0.0);
    }

    #[test]
    fn the_hit_test_drags_from_the_titlebar_and_resizes_from_the_edges() {
        use sdl3::video::HitTestResult as R;
        let key = 0x5eed_usize;
        HIT_MAPS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(
                key,
                HitMap {
                    width: 400.0,
                    height: 300.0,
                    drag_bottom: 28.0,
                    controls: vec![Rect::new(8.0, 8.0, 12.0, 12.0)],
                    resizable: true,
                    surface: 0,
                },
            );
        let at = |x, y| {
            let point = sdl3::video::Point { x, y };
            // SAFETY: a live point; the window is only used as a key and to
            // read flags, which SDL answers as zero for one it does not know.
            unsafe { hit_test(key as sdl3::Window, &point, std::ptr::null_mut()) }
        };
        assert_eq!(at(200, 14), R::Draggable);
        assert_eq!(at(12, 12), R::Normal, "a window button is a control");
        assert_eq!(at(200, 100), R::Normal, "the content is the guest's");
        assert_eq!(at(0, 150), R::ResizeLeft);
        assert_eq!(at(399, 299), R::ResizeBottomRight);
        assert_eq!(at(200, 1), R::ResizeTop);
        HIT_MAPS.lock().unwrap().as_mut().unwrap().remove(&key);
    }
}
