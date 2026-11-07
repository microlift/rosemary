use std::ffi::c_void;

use foundation::geometry::{CGFloat, NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
pub struct Window {
    isa: *const c_void,
    appearance: Id,
    /// The SDL window, or null until `-initWithContentRect:` has run.
    sdl: sdl3::Window,
    content_view: Id,
    delegate: Id,
    background: Id,
    /// `NSWindowStyleMask`, kept because `-styleMask` is asked and because the
    /// resizable bit is the one SDL needs told separately.
    style_mask: usize,
    level: isize,
    accepts_mouse_moved: usize,
    shown: usize,
    /// `NSWindowCollectionBehavior`, which a program reads back to decide
    /// whether it may go full screen.
    collection_behavior: usize,
    /// Weak: a responder freed while it has focus must not leave this dangling.
    first_responder: Id,
    /// `-initialFirstResponder`, a nib outlet and so weak, not retained: made
    /// the first responder when the window is first ordered front without one.
    initial_first_responder: Id,
    /// `-setNextResponder:`, unretained; a window has none until given one.
    next_responder: Id,
    minimum: NSSize,
    maximum: NSSize,
    fullscreen: usize,
    /// The full-screen state the guest was last told of through
    /// `-windowDidEnterFullScreen:` or `-windowDidExitFullScreen:`. Kept apart
    /// from [`Window::fullscreen`], which follows requests at once, because the
    /// compositor answers later and a program that counts its own transitions
    /// by these notifications must hear of every one.
    reported_fullscreen: usize,
    /// AppKit accepts a smaller frame inside a fullscreen Space; SDL keeps
    /// the host container at display size. Zero width means no override.
    fullscreen_frame: NSRect,
    /// Fullscreen size requests must not change the restored window frame.
    fullscreen_restore: NSRect,
    /// Suppress reentrant toggles from fullscreen lifecycle callbacks: when
    /// the pending transition began, from [`transition_stamp`], or zero.
    fullscreen_transition: usize,
    /// Expected SDL host geometry after a fullscreen transition. Some window
    /// managers queue its move/resize events after the state notification.
    fullscreen_announced_frame: NSRect,
    fullscreen_announced_move: usize,
    fullscreen_announced_resize: usize,
    /// Guest confinement is measured in AppKit coordinates, not SDL coordinates.
    mouse_confinement: NSRect,
    miniaturized: usize,
    /// `-representedFilename`, an owned `NSString` or null for the empty
    /// string AppKit answers before one is set.
    represented_filename: Id,
    /// `NSPanel`'s `-becomesKeyOnlyIfNeeded`. A panel's field, kept in the
    /// window body because `NSPanel` adds no storage of its own here - every
    /// panel *is* one of these - and one word costs less than a side table
    /// keyed by address.
    becomes_key_only_if_needed: usize,
    /// `-setAlphaValue:`, with [`Window::alpha_set`] saying whether it was ever
    /// given one - a fresh window is fully opaque, and zero is a legal alpha, so
    /// the field cannot carry its own default.
    alpha: CGFloat,
    alpha_set: usize,
    /// `-setMovable:`, stored inverted so that zero is AppKit's `YES`.
    immovable: usize,
    /// `-setShowsToolbarButton:`.
    shows_toolbar_button: usize,
    /// The chrome above the content the SDL window was last sized for, in
    /// points; see [`content_inset`].
    inset: CGFloat,
    /// The frame the guest last asked for, in AppKit's screen coordinates.
    ///
    /// Kept as well as asked of SDL because the two need not agree: a Wayland
    /// compositor places a toplevel where it likes and ignores every later
    /// request, so what a guest asked for is not readable back from the window
    /// system - and it is what a window remade as a popup has to be placed at.
    requested: NSRect,
    /// Nonzero from a `-setFrame:` until SDL reports the window's new size.
    /// SDL answers the old size in between, and AppKit's frame and content
    /// view already have the new one: a program reads them back at once.
    size_pending: usize,
    /// `-setTitlebarAppearsTransparent:`.
    titlebar_transparent: usize,
    /// `-setTitleVisibility:`, `NSWindowTitleHidden` being 1.
    title_visibility: isize,
    /// `-windowController`, not retained: the controller owns the window.
    window_controller: Id,
    /// `-setHidesOnDeactivate:`, kept so a program reads back what it set.
    hides_on_deactivate: usize,
    /// `-setExcludedFromWindowsMenu:`, likewise; no Windows menu lists windows.
    excluded_from_windows_menu: usize,
    /// When this window was last ordered front, from [`ORDERED`]: the stacking
    /// order, which creation order is not.
    ordered: u64,
    /// The view the content view and the titlebar accessories share, retained,
    /// made when the first accessory is added; null until then. It is what
    /// AppKit's `NSThemeFrame` is to the content view: its superview.
    frame_view: Id,
    /// `-titlebarAccessoryViewControllers`, an owned mutable array or null.
    accessories: Id,
}

pub const INSTANCE_SIZE: usize = size_of::<Window>();

/// The notification names a window posts.
///
/// Named here rather than where they are posted, because publishing the symbol
/// and posting the notification are two different jobs and the first has to
/// happen whether or not the second does yet.
pub const DID_RESIZE: &str = "NSWindowDidResizeNotification";
pub const DID_MOVE: &str = "NSWindowDidMoveNotification";
pub const DID_CHANGE_SCREEN: &str = "NSWindowDidChangeScreenNotification";
pub const DID_CHANGE_SCREEN_PROFILE: &str = "NSWindowDidChangeScreenProfileNotification";
pub const DID_EXPOSE: &str = "NSWindowDidExposeNotification";
pub const WILL_CLOSE: &str = "NSWindowWillCloseNotification";
pub const DID_BECOME_KEY: &str = "NSWindowDidBecomeKeyNotification";
pub const DID_RESIGN_KEY: &str = "NSWindowDidResignKeyNotification";
pub const DID_BECOME_MAIN: &str = "NSWindowDidBecomeMainNotification";
pub const DID_RESIGN_MAIN: &str = "NSWindowDidResignMainNotification";
pub const DID_MINIATURIZE: &str = "NSWindowDidMiniaturizeNotification";
pub const DID_DEMINIATURIZE: &str = "NSWindowDidDeminiaturizeNotification";
pub const WILL_ENTER_FULL_SCREEN: &str = "NSWindowWillEnterFullScreenNotification";
pub const WILL_EXIT_FULL_SCREEN: &str = "NSWindowWillExitFullScreenNotification";
pub const DID_ENTER_FULL_SCREEN: &str = "NSWindowDidEnterFullScreenNotification";
pub const DID_EXIT_FULL_SCREEN: &str = "NSWindowDidExitFullScreenNotification";
/// Posted when the window moves to a screen of a different backing scale, which
/// is what a guest recomputes its drawable size from.
pub const DID_CHANGE_BACKING_PROPERTIES: &str = "NSWindowDidChangeBackingPropertiesNotification";
/// The pair that brackets a user-driven resize. Published and not yet posted:
/// nothing here distinguishes a drag of the window's edge from a programmatic
/// resize, so a toolkit that batches work between the two does it for every resize
/// instead - which is the slower path and not the wrong one.
pub const WILL_START_LIVE_RESIZE: &str = "NSWindowWillStartLiveResizeNotification";
pub const DID_END_LIVE_RESIZE: &str = "NSWindowDidEndLiveResizeNotification";

/// `NSWindowStyleMask`.
mod style {
    pub const TITLED: usize = 1;
    pub const CLOSABLE: usize = 1 << 1;
    pub const MINIATURIZABLE: usize = 1 << 2;
    pub const RESIZABLE: usize = 1 << 3;
    /// `NSWindowStyleMaskFullScreen`, which AppKit sets and clears itself;
    /// see `style_mask`.
    pub const FULL_SCREEN: usize = 1 << 14;
}

/// The `NSWindowLevel`s that say what a window is *for*.
///
/// A level is the only thing that distinguishes a menu from a frameless window
/// a program drew a title bar into: both are untitled, and AppKit needs no
/// further distinction because it places and stacks every window the same way.
/// A Wayland compositor does not - a menu has to be a popup surface, positioned
/// against its parent - so the level is what this tree classifies by.
mod window_level {
    /// `NSFloatingWindowLevel`: a tool palette, above its document.
    pub const FLOATING: isize = 3;
    /// `NSPopUpMenuWindowLevel`: a menu, a combo box's list, a popover.
    pub const POPUP_MENU: isize = 101;
    /// `NSScreenSaverWindowLevel`, which is where Qt puts a tooltip - above
    /// everything, and never taking the pointer or the keyboard.
    pub const SCREEN_SAVER: isize = 1000;
}

/// How the host window system presents a window.
///
/// AppKit has one kind of window and stacks it by level. Wayland has two that
/// matter here, and the difference is not cosmetic: a toplevel is placed,
/// decorated and focused by the compositor and **cannot be positioned by the
/// program at all**, while a popup is placed exactly where its parent says,
/// carries no decoration and leaves the keyboard where it was. A menu made as a
/// toplevel therefore appears wherever the compositor likes, which is the
/// defect this distinction exists to remove.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Presentation {
    /// An ordinary window, decorated when the style mask says titled.
    Toplevel,
    /// A menu or dropdown: positioned against its parent, undecorated, and not
    /// taking the keyboard - which is what AppKit does with a borderless panel
    /// at menu level, since `QNSPanel` refuses to become key.
    Menu,
    /// A tooltip: as a menu, and never taking the pointer either.
    Tooltip,
}

impl Presentation {
    /// The SDL flags that distinguish the two popup kinds, or none.
    fn popup_flags(self) -> Option<u64> {
        match self {
            Presentation::Toplevel => None,
            // Not focusable, so the keyboard stays with the window the toolkit
            // thinks is active: `-[QNSPanel canBecomeKeyWindow]` answers no for
            // a popup, and Qt routes keys to the popup through the window that
            // *is* key. A popup that took focus would make that window resign,
            // and the menu would answer nothing.
            Presentation::Menu => {
                Some(sdl3::video::flags::POPUP_MENU | sdl3::video::flags::NOT_FOCUSABLE)
            }
            Presentation::Tooltip => {
                Some(sdl3::video::flags::TOOLTIP | sdl3::video::flags::NOT_FOCUSABLE)
            }
        }
    }
}

/// What the window is, from what the guest set on it.
///
/// Read at every ordering rather than remembered from creation, because a
/// toolkit reuses its windows: Qt hides one, changes its style mask and level,
/// and shows it again as something else entirely - a tooltip where a dialog was.
fn presentation(window: &Window) -> Presentation {
    if window.style_mask & style::TITLED != 0 {
        return Presentation::Toplevel;
    }
    match window.level {
        level if level >= window_level::SCREEN_SAVER => Presentation::Tooltip,
        level if level >= window_level::POPUP_MENU => Presentation::Menu,
        _ => Presentation::Toplevel,
    }
}

/// The window a popup is positioned against: the key window, else the largest
/// window on the screen.
///
/// AppKit needs no such thing - a menu is placed in screen coordinates - and
/// this Qt does not call `-addChildWindow:ordered:`, so there is nothing to
/// read the answer off. The largest shown window is the program's own interface,
/// which is what a menu of that program belongs to.
fn popup_parent(receiver: Id) -> sdl3::Window {
    let candidates = || {
        all()
            .into_iter()
            .filter(|window| *window != receiver && is_shown(*window))
            // A popup of a popup is legal and is what a submenu is, but the
            // parent has to be one that is still on the screen.
            .filter_map(|window| {
                // SAFETY: a window this crate made.
                let sdl = unsafe { sdl_window(window) };
                (!sdl.is_null()).then_some(sdl)
            })
    };
    // SAFETY: null or a window this crate made.
    let key = unsafe { sdl_window(key_window()) };
    if !key.is_null() && candidates().any(|sdl| sdl == key) {
        return key;
    }
    candidates()
        .max_by_key(|sdl| {
            sdl3::video::window_size(*sdl).map_or(0, |(w, h)| i64::from(w) * i64::from(h))
        })
        .unwrap_or(std::ptr::null_mut())
}

/// The presentation an SDL window was actually made with.
fn presentation_of_sdl(sdl: sdl3::Window) -> Presentation {
    let flags = sdl3::video::window_flags(sdl);
    if flags & sdl3::video::flags::TOOLTIP != 0 {
        return Presentation::Tooltip;
    }
    match flags & sdl3::video::flags::POPUP_MENU != 0 {
        true => Presentation::Menu,
        false => Presentation::Toplevel,
    }
}

/// Where a window is on the desktop, in SDL's coordinates.
///
/// A popup's own position is measured from its parent's origin, which is what
/// SDL both takes and answers for one; every caller here wants the desktop.
pub(crate) fn global_position(sdl: sdl3::Window) -> Option<(i32, i32)> {
    let parent = sdl3::video::window_parent(sdl);
    if parent.is_null() {
        return placed_origin(sdl).or_else(|| sdl3::video::window_position(sdl));
    }
    let (x, y) = sdl3::video::window_position(sdl)?;
    let (parent_x, parent_y) = global_position(parent)?;
    Some((x + parent_x, y + parent_y))
}

/// Where a Wayland toplevel is on the desktop: where the guest put it.
///
/// A Wayland client is never told where its toplevels are, and SDL answers
/// with a placeholder origin once the compositor has mapped one. On a Mac the
/// window is where it was asked to be, and a program may keep its own copy of
/// that origin without ever asking again - Feral's Win32 layer does, and maps
/// every pointer position against it. So the desktop here is a virtual one in
/// which each toplevel sits at its requested origin; every desktop position
/// (the frame, the pointer, a warp) is measured from it, so they agree.
///
/// Not for a full-screen or maximized window, which the compositor has placed
/// somewhere the guest did not ask for and SDL does report.
fn placed_origin(sdl: sdl3::Window) -> Option<(i32, i32)> {
    if !sdl3::video::is_wayland() {
        return None;
    }
    let placed_by_host = sdl3::video::flags::FULLSCREEN | sdl3::video::flags::MAXIMIZED;
    if sdl3::video::window_flags(sdl) & placed_by_host != 0 {
        return None;
    }
    let object = window_for(sdl);
    if object.is_null() {
        return None;
    }
    let requested = window_of(object).requested;
    if requested.size.width <= 0.0 || requested.size.height <= 0.0 {
        return None;
    }
    let (x, y, _, _) = crate::geometry::rect_to_sdl(requested);
    Some((x, y))
}

/// The pointer on the desktop, in SDL's coordinates, and the held buttons.
///
/// Measured from the window the pointer is in rather than asked of SDL, whose
/// global answer on Wayland adds its own placeholder origin instead of the one
/// [`global_position`] reports.
pub(crate) fn desktop_mouse_state() -> Option<(f32, f32, u32)> {
    if !sdl3::video::is_wayland() {
        return sdl3::video::global_mouse_state();
    }
    let (x, y, buttons) = sdl3::video::mouse_state()?;
    let origin = crate::event::pointer_window()
        .and_then(global_position)
        .unwrap_or((0, 0));
    Some((x + origin.0 as f32, y + origin.1 as f32, buttons))
}

/// The reverse: a desktop position as the offset SDL wants for this window.
fn local_position(sdl: sdl3::Window, x: i32, y: i32) -> (i32, i32) {
    let parent = sdl3::video::window_parent(sdl);
    if parent.is_null() {
        return (x, y);
    }
    match global_position(parent) {
        Some((parent_x, parent_y)) => (x - parent_x, y - parent_y),
        None => (x, y),
    }
}

/// `ROSEMARY_TRACE_WINDOW=1`: report every change to a window's identity - its
/// creation, style mask, level, frame and ordering.
///
/// A toolkit says what kind of window it wants through those five and nothing
/// else, and the answer to "why did this appear undecorated, in the wrong place,
/// deaf to the pointer" is which of them arrived, in what order, and with what.
/// Written to the event log as well as to stderr, because a guest that replaces
/// its own stderr takes every later line with it.
pub const TRACE_ENV: &str = "ROSEMARY_TRACE_WINDOW";

pub(crate) fn tracing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os(TRACE_ENV).is_some())
}

pub(crate) fn trace(args: std::fmt::Arguments) {
    if !tracing() {
        return;
    }
    let line = format!("rosemary: appkit: window: {args}");
    eprintln!("{line}");
    crate::event::debug_log(&line);
}

macro_rules! trace {
    ($($arg:tt)*) => { crate::window::trace(format_args!($($arg)*)) };
}

fn window_of<'a>(receiver: Id) -> &'a mut Window {
    // SAFETY: a window of this crate's, which is what every method here is sent
    // to.
    unsafe { &mut *(receiver as *mut Window) }
}

/// What to run before an SDL window is destroyed.
///
/// Anything keyed by the SDL window's *address* has to drop its entry then, or
/// it will be handed to the next window made - SDL reuses addresses - at the
/// wrong size and against the wrong surface. `appkit::backing` clears its own
/// store directly; `metal` registers here, because nothing in this crate may
/// call into it.
static DESTROY_HOOKS: std::sync::Mutex<Vec<fn(sdl3::Window)>> = std::sync::Mutex::new(Vec::new());

/// Run `hook` on every SDL window this crate is about to destroy.
pub fn add_destroy_hook(hook: fn(sdl3::Window)) {
    let mut hooks = DESTROY_HOOKS.lock().unwrap_or_else(|e| e.into_inner());
    if !hooks
        .iter()
        .any(|existing| std::ptr::fn_addr_eq(*existing, hook))
    {
        hooks.push(hook);
    }
}

/// Preserve guest NSWindow popup objects when their SDL parent is remade.
/// SDL attaches native popups to a particular parent allocation, so they
/// cannot outlive that parent without being recreated themselves.
fn rehome_popup_descendants(old: sdl3::Window, replacement: sdl3::Window) {
    let mut replacements = std::collections::HashMap::from([(old as usize, replacement)]);
    for child in sdl3::video::window_family(old).into_iter().skip(1) {
        if sdl3::video::live_window_id(child).is_none() {
            continue;
        }
        let parent = sdl3::video::window_parent(child);
        let Some(&new_parent) = replacements.get(&(parent as usize)) else {
            continue;
        };
        let Some(popup) = presentation_of_sdl(child).popup_flags() else {
            continue;
        };
        let Some(object) = (!window_for(child).is_null()).then(|| window_for(child)) else {
            continue;
        };
        // SDL makes a new popup visible when its parent is shown, even if
        // the old popup was explicitly ordered out by the guest.
        let shown = window_of(object).shown != 0;
        let (x, y) = sdl3::video::window_position(child).unwrap_or((0, 0));
        let (width, height) = sdl3::video::window_size(child).unwrap_or((1, 1));
        let api = if draws_with_opengl(child) {
            Api::OpenGl
        } else {
            Api::Undecided
        };
        let flags = (creation_flags(window_of(object).style_mask, api)
            & !sdl3::video::flags::BORDERLESS)
            | popup;
        let Some(new) =
            sdl3::video::create_popup_window(new_parent, x, y, width.max(1), height.max(1), flags)
        else {
            continue; // old mapping is invalidated by the destroy path.
        };
        // Hide before the replacement parent is shown so SDL does not
        // resurrect a popup that AppKit still considers ordered out.
        if !shown {
            sdl3::video::hide_window(new);
        }
        // SDL's GL flag is not enough: AppKit separately tracks which window
        // surface belongs to a guest context. Transfer that ownership before
        // the original popup's teardown hook removes the old registration.
        if api == Api::OpenGl {
            OPENGL_WINDOWS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(new as usize);
        }
        window_of(object).sdl = new;
        // Rebind before SDL recursively destroys the old popup. Never try
        // to move a context that belongs to another rendering thread.
        crate::opengl::popup_rehomed(child, new);
        WINDOWS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter_mut()
            .filter(|entry| entry.0 == child as usize)
            .for_each(|entry| entry.0 = new as usize);
        replacements.insert(child as usize, new);
        let content = window_of(object).content_view;
        if !content.is_null() {
            crate::backing::needs_display(content);
        }
    }
}

/// Destroy an SDL window, telling everything keyed on it first.
fn destroy_sdl_window(sdl: sdl3::Window) {
    // SDL_DestroyWindow recursively frees popups. Reconcile every descendant
    // NSWindow and retire every Metal/CPU owner *before* native destruction.
    let family = sdl3::video::begin_window_family_destroy(sdl);
    let hooks = DESTROY_HOOKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    for member in family.into_iter().rev() {
        // Remove fullscreen native overlays before retiring each member,
        // including SDL popups which SDL otherwise destroys recursively.
        #[cfg(target_os = "linux")]
        sdl3::x11_mask::forget(member);
        #[cfg(target_os = "linux")]
        sdl3::wayland_mask::forget(member);
        // A separately retired child has already received its own cleanup.
        if sdl3::video::live_window_id(member).is_none() {
            continue;
        }
        if member != sdl {
            let owned = {
                let mut windows = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
                windows
                    .iter()
                    .position(|(key, _)| *key == member as usize)
                    .map(|at| windows.remove(at).1 as Id)
            };
            if let Some(object) = owned {
                let child = window_of(object);
                child.sdl = std::ptr::null_mut();
                child.shown = 0;
                if KEY.load(std::sync::atomic::Ordering::Acquire) == object as usize {
                    KEY.store(0, std::sync::atomic::Ordering::Release);
                    crate::event::announce_resign(object);
                }
                crate::event::forget_announced(object);
            }
        }
        OPENGL_WINDOWS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|window| *window != member as usize);
        crate::backing::forget_window(member);
        crate::view::forget_metal_scene(member);
        crate::screen::forget_relative_pointer(member);
        for hook in &hooks {
            hook(member);
        }
        // After the hooks, which cancel a Metal swapchain initializer that
        // would otherwise mark the window opaque once this had forgotten it.
        crate::decor::destroyed(member);
    }
    sdl3::video::destroy_window_after_hooks(sdl);
}

/// Every live window, so an SDL event's window id can find the object it belongs
/// to.
///
/// SDL identifies a window by a small integer in every event; AppKit identifies
/// one by the object. The table is the join, and it is here rather than in the
/// event module because this is what creates and destroys the entries.
/// Both halves are addresses rather than pointers, because the table is shared
/// between threads and neither is dereferenced while it is in it.
static WINDOWS: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());

/// The `NSWindow` for an SDL window, or nil.
pub fn window_for(sdl: sdl3::Window) -> Id {
    WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(w, _)| *w == sdl as usize)
        .map_or(std::ptr::null_mut(), |(_, object)| *object as Id)
}

/// Every live SDL window, for a change that applies to all of them.
pub fn sdl_windows() -> Vec<sdl3::Window> {
    WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(sdl, _)| *sdl as sdl3::Window)
        .collect()
}

/// The last value handed to [`Window::ordered`].
static ORDERED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Every shown window, frontmost first.
pub(crate) fn front_to_back() -> Vec<Id> {
    let mut windows: Vec<Id> = all().into_iter().filter(|w| is_shown(*w)).collect();
    windows.sort_by_key(|w| std::cmp::Reverse(window_of(*w).ordered));
    windows
}

/// Every window this crate has made, most recent last.
pub fn all() -> Vec<Id> {
    WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(_, object)| *object as Id)
        .collect()
}

/// The SDL window behind an `NSWindow`, or null.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
pub unsafe fn sdl_window(object: Id) -> sdl3::Window {
    if object.is_null() {
        return std::ptr::null_mut();
    }
    let is_window = unsafe { objc::object::class_of(object) }.is_some_and(|c| {
        objc::class::lookup(c"NSWindow").is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    });
    match is_window {
        true => window_of(object).sdl,
        false => std::ptr::null_mut(),
    }
}

/// Whether the window has been ordered onto the screen.
pub fn is_shown(object: Id) -> bool {
    // SAFETY: null or an object this runtime allocated.
    !unsafe { sdl_window(object) }.is_null() && window_of(object).shown != 0
}

/// The root of a window's views, for the event pump and drawing: the content
/// view, or the view it shares with the titlebar accessories.
pub fn content_view_of(object: Id) -> Id {
    if object.is_null() {
        return std::ptr::null_mut();
    }
    let window = window_of(object);
    match window.frame_view.is_null() {
        true => window.content_view,
        false => window.frame_view,
    }
}

fn send_view(receiver: Id, sel: &str, argument: Id) {
    // SAFETY: `void (id, SEL, id)`, which every selector this is used with is.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel), argument);
    }
}

fn send_frame(receiver: Id, frame: NSRect) {
    // SAFETY: `-setFrame:` is `void (id, SEL, NSRect)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("setFrame:"), frame);
    }
}

/// The accessory controllers of a window, in order.
fn accessories_of(object: Id) -> Vec<Id> {
    let accessories = window_of(object).accessories;
    // SAFETY: null or the array `add_titlebar_accessory` made.
    unsafe { corefoundation::collections::array(accessories as corefoundation::base::CFTypeRef) }
        .map(|a| a.values.iter().map(|v| *v as Id).collect())
        .unwrap_or_default()
}

/// The controller's view, loading it if need be.
fn accessory_view(controller: Id) -> Id {
    // SAFETY: an `NSViewController`, sent `-view`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(controller, objc::sel::get("view"))
    }
}

/// Place the accessories along the top of the frame view, each its own
/// height, and the content view in what is left.
///
/// FIXME (ACCURACY): an accessory placed at the titlebar's leading or
/// trailing edge goes under it with the rest, since the titlebar here is not
/// made of views it could join.
fn layout_titlebar(object: Id, size: NSSize) {
    let window = window_of(object);
    if window.frame_view.is_null() {
        return;
    }
    send_frame(
        window.frame_view,
        NSRect {
            origin: NSPoint::default(),
            size,
        },
    );
    let mut top = size.height;
    for controller in accessories_of(object) {
        let view = accessory_view(controller);
        if view.is_null() || crate::props::integer(controller, "hidden") != 0 {
            continue;
        }
        let height = match crate::layout::own_size(view)[1] {
            (Some(exact), _) => exact,
            (None, Some(least)) => least.max(crate::view::frame_of(view).size.height),
            (None, None) => crate::view::frame_of(view).size.height,
        };
        top -= height;
        send_frame(
            view,
            NSRect {
                origin: NSPoint { x: 0.0, y: top },
                size: NSSize {
                    width: size.width,
                    height,
                },
            },
        );
    }
    if !window.content_view.is_null() {
        send_frame(
            window.content_view,
            NSRect {
                origin: NSPoint::default(),
                size: NSSize {
                    width: size.width,
                    height: top.max(0.0),
                },
            },
        );
    }
}

/// `-addTitlebarAccessoryViewController:`.
unsafe extern "C" fn add_titlebar_accessory(receiver: Id, sel: Sel, controller: Id) {
    let count = accessories_of(receiver).len();
    unsafe { insert_titlebar_accessory(receiver, sel, controller, count) };
}

/// `-insertTitlebarAccessoryViewController:atIndex:`.
unsafe extern "C" fn insert_titlebar_accessory(
    receiver: Id,
    _sel: Sel,
    controller: Id,
    index: usize,
) {
    if controller.is_null() {
        return;
    }
    let window = window_of(receiver);
    if window.accessories.is_null() {
        window.accessories = corefoundation::collections::new_array(
            &[],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            true,
        ) as Id;
    }
    if window.frame_view.is_null() {
        let class = objc::class::lookup(c"NSView").expect("NSView");
        let frame_view = objc::object::create_instance(class, 0);
        // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)`.
        let frame_view = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, NSRect) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                frame_view,
                objc::sel::get("initWithFrame:"),
                NSRect::default(),
            )
        };
        window.frame_view = frame_view;
        if !window.content_view.is_null() {
            send_view(frame_view, "addSubview:", window.content_view);
        }
        // SAFETY: a view of this crate's, now this window's root.
        unsafe { crate::view::set_window(frame_view, receiver) };
    }
    // SAFETY: the array made above, and an object to put in it.
    if let Some(array) = unsafe {
        corefoundation::collections::array(window.accessories as corefoundation::base::CFTypeRef)
    } {
        let at = index.min(array.values.len());
        array.values.insert(
            at,
            objc::arc::retain(controller) as corefoundation::base::CFTypeRef,
        );
    }
    crate::view_controller::set_accessory_window(controller, receiver);
    let view = accessory_view(controller);
    if !view.is_null() {
        send_view(window.frame_view, "addSubview:", view);
        if crate::props::integer(controller, "hidden") != 0 {
            // SAFETY: `-setHidden:` is `void (id, SEL, BOOL)`.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel, u8) = std::mem::transmute(objc::msg::entry());
                f(view, objc::sel::get("setHidden:"), 1);
            }
        }
    }
    fit_content_view(receiver);
}

/// `-removeTitlebarAccessoryViewControllerAtIndex:`.
unsafe extern "C" fn remove_titlebar_accessory(receiver: Id, _sel: Sel, index: usize) {
    let window = window_of(receiver);
    // SAFETY: null or the array `insert_titlebar_accessory` made.
    let Some(array) = (unsafe {
        corefoundation::collections::array(window.accessories as corefoundation::base::CFTypeRef)
    }) else {
        return;
    };
    if index >= array.values.len() {
        return;
    }
    let controller = array.values.remove(index) as Id;
    let view = accessory_view(controller);
    if !view.is_null() {
        // SAFETY: `-removeFromSuperview` is `void (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
            f(view, objc::sel::get("removeFromSuperview"));
        }
    }
    crate::view_controller::set_accessory_window(controller, std::ptr::null_mut());
    objc::arc::release(controller);
    fit_content_view(receiver);
}

/// `-titlebarAccessoryViewControllers`.
unsafe extern "C" fn titlebar_accessories(receiver: Id, _sel: Sel) -> Id {
    let values: Vec<corefoundation::base::CFTypeRef> = accessories_of(receiver)
        .into_iter()
        .map(|c| c as corefoundation::base::CFTypeRef)
        .collect();
    let array = corefoundation::collections::new_array(
        &values,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    objc::arc::autorelease(array as Id)
}

/// Whether `object` is an `NSPanel`, which gets a titlebar and no menu bar.
fn is_panel(object: Id) -> bool {
    // SAFETY: an object this runtime allocated, or a class.
    unsafe { objc::object::class_of(object) }.is_some_and(|c| {
        objc::class::lookup(c"NSPanel").is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    })
}

/// The height of the chrome this runtime draws above a window's content, in
/// points: the SDL window is the frame, and this much of its top is not the
/// content view's.
pub(crate) fn content_inset(object: Id) -> CGFloat {
    // SAFETY: null or an object this runtime allocated.
    if unsafe { sdl_window(object) }.is_null() {
        return 0.0;
    }
    let window = window_of(object);
    let fullscreen = if sdl3::video::is_video_thread() {
        is_fullscreen(window.sdl)
    } else {
        sdl3::video::presentation_window(window.sdl)
            .is_some_and(|snapshot| snapshot.flags & sdl3::video::flags::FULLSCREEN != 0)
    };
    inset_of(object, window.style_mask, fullscreen)
}

fn inset_of(object: Id, style_mask: usize, fullscreen: bool) -> CGFloat {
    crate::decor::inset_for(
        crate::decor::config(),
        style_mask & style::TITLED != 0,
        is_panel(object),
        fullscreen,
    )
}

fn is_fullscreen(sdl: sdl3::Window) -> bool {
    !sdl.is_null() && sdl3::video::window_flags(sdl) & sdl3::video::flags::FULLSCREEN != 0
}

/// Whether a shown fullscreen window covers `display`. Hidden and minimized
/// ones leave the desktop panels visible.
pub(crate) fn fullscreen_on(display: u32) -> bool {
    use sdl3::video::flags::{FULLSCREEN, HIDDEN, MINIMIZED};
    all().into_iter().any(|object| {
        let window = window_of(object);
        if window.shown == 0 || window.sdl.is_null() {
            return false;
        }
        let flags = sdl3::video::window_flags(window.sdl);
        flags & FULLSCREEN != 0
            && flags & (HIDDEN | MINIMIZED) == 0
            && sdl3::video::display_for_window(window.sdl) == Some(display)
    })
}

fn host_frame(object: Id) -> Option<NSRect> {
    if object.is_null() {
        return None;
    }
    let sdl = window_of(object).sdl;
    if sdl.is_null() {
        return None;
    }
    // Metal presentation can resolve this frame from a worker, which reads
    // the SDL owner's published snapshot, never SDL window properties. The
    // owner asks SDL itself: -frame straight after -setFrame: must not wait
    // for the resize event, and popup-relative and Wayland virtual-desktop
    // positions come from global_position.
    let ((width, height), (x, y)) = if sdl3::video::is_video_thread() {
        (sdl3::video::window_size(sdl)?, global_position(sdl)?)
    } else {
        let snapshot = sdl3::video::presentation_window(sdl)?;
        (snapshot.logical_size, snapshot.position)
    };
    Some(crate::geometry::rect_from_sdl(x, y, width, height))
}

pub(crate) fn fullscreen_frame(object: Id) -> Option<NSRect> {
    if object.is_null() {
        return None;
    }
    let window = window_of(object);
    let flags = if sdl3::video::is_video_thread() {
        sdl3::video::window_flags(window.sdl)
    } else {
        sdl3::video::presentation_window(window.sdl)?.flags
    };
    (flags & sdl3::video::flags::FULLSCREEN != 0 && window.fullscreen_frame.size.width > 0.0)
        .then_some(window.fullscreen_frame)
}

/// The logical AppKit frame in host surface points, with a top-left origin.
/// The physical surface remains display-sized even when AppKit's frame shrinks.
pub(crate) fn surface_frame(object: Id) -> Option<NSRect> {
    let host = host_frame(object)?;
    let logical = fullscreen_frame(object).unwrap_or(host);
    Some(NSRect {
        origin: NSPoint {
            x: logical.origin.x - host.origin.x,
            y: host.origin.y + host.size.height - logical.origin.y - logical.size.height,
        },
        size: logical.size,
    })
}

/// Pixel bounds of the guest's logical NSWindow inside its SDL host surface.
/// The host may stay display-sized in fullscreen. Round each edge separately
/// to preserve half-point boundaries and use the same extent for every
/// presentation backend, including those outside AppKit.
pub fn presentation_clip(sdl: sdl3::Window) -> Option<sdl3::Rect> {
    // The clip is consumed by Metal's FIFO worker. Use dimensions already
    // published by the SDL video owner, never query the native SDL window.
    let snapshot = sdl3::video::presentation_window(sdl)?;
    let (width, height) = snapshot.logical_size;
    let (pixels_w, pixels_h) = snapshot.pixel_size;
    if width <= 0 || height <= 0 || pixels_w <= 0 || pixels_h <= 0 {
        return None;
    }
    let window = window_for(sdl);
    let surface = (!window.is_null()).then(|| surface_frame(window)).flatten();
    let Some(surface) = surface else {
        return Some(sdl3::Rect {
            x: 0,
            y: 0,
            w: pixels_w,
            h: pixels_h,
        });
    };
    Some(pixel_clip_from_surface(
        surface,
        (width, height),
        (pixels_w, pixels_h),
    ))
}

/// Convert a top-origin AppKit rectangle to a half-open *unclipped* pixel
/// rectangle. Every drawable and backing presenter uses this conversion so
/// fractional origins and extents cannot disagree about the final edge.
///
/// A view can extend outside the physical window. Do not clamp here: Metal
/// needs its full destination frame to map a clipped region back to the right
/// source texels. Only `pixel_clip_from_surface` clamps to the host surface.
pub(crate) fn pixel_rect_from_surface(
    surface: NSRect,
    (width, height): (i32, i32),
    (pixels_w, pixels_h): (i32, i32),
) -> sdl3::Rect {
    let sx = f64::from(pixels_w) / f64::from(width.max(1));
    let sy = f64::from(pixels_h) / f64::from(height.max(1));
    let x0 = (surface.origin.x * sx).round() as i32;
    let y0 = (surface.origin.y * sy).round() as i32;
    let x1 = ((surface.origin.x + surface.size.width) * sx).round() as i32;
    let y1 = ((surface.origin.y + surface.size.height) * sy).round() as i32;
    sdl3::Rect {
        x: x0,
        y: y0,
        w: x1.saturating_sub(x0).max(0),
        h: y1.saturating_sub(y0).max(0),
    }
}

/// The canonical host-clipped pixel rectangle: identical rounded endpoints
/// to an unclipped drawable, intersected with SDL's physical pixel bounds.
pub(crate) fn pixel_clip_from_surface(
    surface: NSRect,
    (width, height): (i32, i32),
    (pixels_w, pixels_h): (i32, i32),
) -> sdl3::Rect {
    let pixels = pixel_rect_from_surface(surface, (width, height), (pixels_w, pixels_h));
    let x0 = pixels.x.clamp(0, pixels_w);
    let y0 = pixels.y.clamp(0, pixels_h);
    let x1 = pixels.x.saturating_add(pixels.w).clamp(x0, pixels_w);
    let y1 = pixels.y.saturating_add(pixels.h).clamp(y0, pixels_h);
    sdl3::Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

#[cfg(test)]
#[test]
fn physical_pixel_rects_share_independent_edge_rounding_at_one_and_two_x() {
    let bounds = NSRect {
        origin: NSPoint { x: 0.5, y: 0.5 },
        size: NSSize {
            width: 1438.5,
            height: 898.5,
        },
    };
    let uncut = pixel_rect_from_surface(bounds, (1440, 900), (1440, 900));
    assert_eq!(
        uncut,
        sdl3::Rect {
            x: 1,
            y: 1,
            w: 1438,
            h: 898
        }
    );
    assert_eq!(
        pixel_clip_from_surface(bounds, (1440, 900), (1440, 900)),
        uncut
    );
    let retina = pixel_rect_from_surface(bounds, (1440, 900), (2880, 1800));
    assert_eq!(
        retina,
        sdl3::Rect {
            x: 1,
            y: 1,
            w: 2877,
            h: 1797
        }
    );
    assert_eq!(
        pixel_clip_from_surface(bounds, (1440, 900), (2880, 1800)),
        retina
    );
    // An overhanging Metal child must keep its negative origin, even when
    // its visible destination will subsequently be clipped to the window.
    let outside = NSRect {
        origin: NSPoint { x: -0.5, y: -0.5 },
        size: NSSize {
            width: 1441.0,
            height: 901.0,
        },
    };
    let raw = pixel_rect_from_surface(outside, (1440, 900), (1440, 900));
    assert_eq!(
        raw,
        sdl3::Rect {
            x: -1,
            y: -1,
            w: 1442,
            h: 902
        }
    );
    let visible = pixel_clip_from_surface(outside, (1440, 900), (1440, 900));
    assert_eq!(
        visible,
        sdl3::Rect {
            x: 0,
            y: 0,
            w: 1440,
            h: 900
        }
    );
}

/// Map a rectangle in AppKit window base coordinates onto the host surface.
pub(crate) fn rect_to_surface(object: Id, rect: NSRect) -> Option<NSRect> {
    let surface = surface_frame(object)?;
    Some(NSRect {
        origin: NSPoint {
            x: surface.origin.x + rect.origin.x,
            y: surface.origin.y + surface.size.height - rect.origin.y - rect.size.height,
        },
        size: rect.size,
    })
}

/// Whether a host-SDL point actually belongs to the guest's logical window.
/// SDL's fullscreen container may be larger than the NSWindow. The same
/// half-open edges are used for hit testing at windowed and fullscreen sizes.
pub(crate) fn accepts_surface_point(object: Id, x: f32, y: f32) -> bool {
    if fullscreen_frame(object).is_none() {
        return true;
    }
    surface_frame(object).is_some_and(|surface| {
        let x = f64::from(x);
        let y = f64::from(y);
        x >= surface.origin.x
            && y >= surface.origin.y
            && x < surface.origin.x + surface.size.width
            && y < surface.origin.y + surface.size.height
    })
}

/// The reverse mapping used by mouse events and cursor tracking.
pub(crate) fn point_from_surface(object: Id, x: f64, y: f64) -> Option<NSPoint> {
    let surface = surface_frame(object)?;
    Some(NSPoint {
        x: x - surface.origin.x,
        y: surface.size.height - (y - surface.origin.y),
    })
}

fn constrain_fullscreen_frame(mut rect: NSRect, host: NSRect) -> NSRect {
    rect.size.width = rect.size.width.clamp(1.0, host.size.width.max(1.0));
    rect.size.height = rect.size.height.clamp(1.0, host.size.height.max(1.0));
    rect.origin.x = rect.origin.x.clamp(
        host.origin.x,
        host.origin.x + host.size.width - rect.size.width,
    );
    rect.origin.y = rect.origin.y.clamp(
        host.origin.y,
        host.origin.y + host.size.height - rect.size.height,
    );
    rect
}

/// Refresh the *already displayed* OpenGL surface's host-side boundary.
/// A guest may stop swapping immediately after changing its NSWindow frame;
/// its GL context can be current on another thread. Never issue GL commands
/// here. On X11 an input-transparent shaped black child above the host window
/// obscures out-of-bounds pixels immediately, including old front buffers.
pub(crate) fn refresh_idle_opengl_clip(sdl: sdl3::Window) {
    if sdl.is_null() || !draws_with_opengl(sdl) {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        if !sdl3::video::is_x11() && !sdl3::video::is_wayland() {
            return;
        }
        let logical = if is_fullscreen(sdl) {
            let clip = presentation_clip(sdl);
            let points = sdl3::video::window_size(sdl);
            let pixels = sdl3::video::window_size_in_pixels(sdl);
            match (clip, points, pixels) {
                (Some(clip), Some((w, h)), Some((pw, ph))) if pw > 0 && ph > 0 => {
                    // X11 child coordinates are window points. Map *edges*
                    // from the canonical pixel clip rather than rounding
                    // origin and size independently.
                    let x0 = (f64::from(clip.x) * f64::from(w) / f64::from(pw)).round() as i32;
                    let x1 =
                        (f64::from(clip.x + clip.w) * f64::from(w) / f64::from(pw)).round() as i32;
                    let y0 = (f64::from(clip.y) * f64::from(h) / f64::from(ph)).round() as i32;
                    let y1 =
                        (f64::from(clip.y + clip.h) * f64::from(h) / f64::from(ph)).round() as i32;
                    Some(sdl3::Rect {
                        x: x0,
                        y: y0,
                        w: x1 - x0,
                        h: y1 - y0,
                    })
                }
                _ => None,
            }
        } else {
            None
        };
        // If fullscreen ended, unmap the masking child immediately; the SDL
        // fullscreen transition restores the physical window geometry.
        if sdl3::video::is_x11() {
            let _ = sdl3::x11_mask::update(sdl, logical);
        } else {
            let _ = sdl3::wayland_mask::update(sdl, logical);
        }
    }
}

fn set_fullscreen_frame(object: Id, rect: NSRect) {
    let Some(host) = host_frame(object) else {
        return;
    };
    let before = fullscreen_frame(object).unwrap_or(host);
    // Will-exit callbacks still expose fullscreen style, but native AppKit
    // has already lifted the settled Space's frame constraints.
    let rect = if !in_fullscreen_transition(object) {
        constrain_fullscreen_frame(rect, host)
    } else {
        rect
    };
    let window = window_of(object);
    window.fullscreen_frame = rect;
    window.requested = rect;
    window.size_pending = 0;
    trace!("{object:p} fullscreen logical frame {rect:?}; host {host:?}");
    // Refresh the backing even when SDL's physical fullscreen extent stays
    // unchanged; background and view clips may have changed in place.
    crate::backing::window_background_changed(object);
    refresh_idle_opengl_clip(window_of(object).sdl);
    fit_content_view(object);
    crate::screen::confine_pointer(object);
    apply_mouse_confinement(object);
    let content = window_of(object).content_view;
    unsafe {
        crate::view::update_tracking_areas(content);
        crate::cursor::reset_rects_in_tree(content);
    }
    // Native AppKit posts resize synchronously, after fitting the content.
    if before.size != rect.size {
        crate::event::post_for(object, DID_RESIZE);
    }
    if before.origin.x != rect.origin.x
        || before.origin.y + before.size.height != rect.origin.y + rect.size.height
    {
        crate::event::post_for(object, DID_MOVE);
    }
}

/// Whether a titled toplevel is in full screen, where its chrome is hidden
/// until the pointer asks for it. Cheaper than [`facts`], for pointer motion.
pub(crate) fn is_full_screen_titled(object: Id) -> bool {
    // SAFETY: null or an object this runtime allocated.
    let sdl = unsafe { sdl_window(object) };
    !sdl.is_null()
        && window_of(object).style_mask & style::TITLED != 0
        && presentation_of_sdl(sdl) == Presentation::Toplevel
        && is_fullscreen(sdl)
}

/// What `crate::decor` draws a window's chrome from.
pub(crate) fn facts(object: Id) -> Option<crate::decor::Facts> {
    // SAFETY: null or an object this runtime allocated.
    let sdl = unsafe { sdl_window(object) };
    if sdl.is_null() || presentation_of_sdl(sdl) != Presentation::Toplevel {
        return None;
    }
    let window = window_of(object);
    let mask = window.style_mask;
    Some(crate::decor::Facts {
        sdl: sdl as usize,
        titled: mask & style::TITLED != 0,
        closable: mask & style::CLOSABLE != 0,
        miniaturizable: mask & style::MINIATURIZABLE != 0,
        resizable: mask & style::RESIZABLE != 0,
        panel: is_panel(object),
        fullscreen: is_fullscreen(sdl),
        shown: window.shown != 0 && window.miniaturized == 0,
        key: key_window() == object,
        title: sdl3::video::window_title(sdl)
            .to_string_lossy()
            .into_owned(),
        title_hidden: window.title_visibility == 1,
        // A transparent titlebar shows the window behind it.
        titlebar_color: (window.titlebar_transparent != 0).then(|| {
            let (r, g, b, a) = background_rgba(object);
            let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            [byte(r), byte(g), byte(b), byte(a)]
        }),
    })
}

/// Give the content view the part of the window below the chrome, after the
/// window or its chrome changed size.
pub(crate) fn fit_content_view(object: Id) {
    let window = window_of(object);
    if window.sdl.is_null() || window.content_view.is_null() {
        return;
    }
    let size = fullscreen_frame(object).map(|rect| rect.size).or_else(|| {
        sdl3::video::window_size(window.sdl).map(|(width, height)| NSSize {
            width: f64::from(width),
            height: f64::from(height),
        })
    });
    let Some(size) = size else {
        return;
    };
    fit_content_view_to(object, size);
}

/// SDL has reported the window's size, so it is the one to answer with again.
pub(crate) fn size_settled(object: Id) {
    if !object.is_null() {
        window_of(object).size_pending = 0;
    }
}

/// The content view sized to a window `width` by `height`, titlebar included.
fn fit_content_view_to(object: Id, size: NSSize) {
    let window = window_of(object);
    if window.content_view.is_null() {
        return;
    }
    let frame = NSRect {
        origin: NSPoint::default(),
        size: NSSize {
            width: size.width,
            height: (size.height - content_inset(object)).max(0.0),
        },
    };
    if !window.frame_view.is_null() {
        layout_titlebar(object, frame.size);
        return;
    }
    // Sent rather than written, so a view that tracks its own size hears
    // about it.
    // SAFETY: `-setFrame:` is `void (id, SEL, NSRect)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
        f(window.content_view, objc::sel::get("setFrame:"), frame);
    }
}

/// A window's own first responder, nil when it has none - not the window
/// itself, which is what `-firstResponder` answers then.
pub fn first_responder_of(object: Id) -> Id {
    match object.is_null() {
        true => std::ptr::null_mut(),
        false => window_of(object).first_responder,
    }
}

/// Whether `-mouseMoved:` should be delivered to this window, for the event
/// pump: AppKit drops those unless a window asked for them, and a game that
/// never asked would otherwise get a stream it does not expect.
pub fn accepts_mouse_moved_events(object: Id) -> bool {
    !object.is_null() && window_of(object).accepts_mouse_moved != 0
}

/// The window an SDL window id names, for the event pump.
pub fn delegate_of(object: Id) -> Id {
    match object.is_null() {
        true => std::ptr::null_mut(),
        false => window_of(object).delegate,
    }
}

/// `ROSEMARY_WINDOW_SIZE=WIDTHxHEIGHT`, the size every window is created at.
fn forced_size() -> Option<(i32, i32)> {
    static SIZE: std::sync::OnceLock<Option<(i32, i32)>> = std::sync::OnceLock::new();
    *SIZE.get_or_init(|| {
        let value = std::env::var("ROSEMARY_WINDOW_SIZE").ok()?;
        let (width, height) = value.split_once(['x', 'X'])?;
        Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
    })
}

unsafe extern "C" fn init_with_content_rect(
    receiver: Id,
    _sel: Sel,
    rect: NSRect,
    style_mask: usize,
    _backing: usize,
    _defer: u8,
) -> Id {
    let window = window_of(receiver);
    window.style_mask = style_mask;
    // The content rectangle grows upwards by the chrome, as a Mac's frame
    // rectangle does: the SDL window is the whole frame.
    let inset = inset_of(receiver, style_mask, false);
    window.inset = inset;
    let content_size = rect.size;
    let rect = frame_for_content(rect, inset);
    window.requested = rect;

    let flags = creation_flags(style_mask, Api::Undecided);
    let (x, y, width, height) = crate::geometry::rect_to_sdl(rect);
    let (width, height) = match forced_size() {
        // A guest which sizes its first window to a fraction of the screen is
        // unusable in a headless or scripted session, where nothing can drag
        // it larger. This says how large every window it makes must be.
        Some((forced_width, forced_height)) => (forced_width, forced_height + inset.round() as i32),
        None => (width, height),
    };
    let title = crate::metadata::name();
    let Some(sdl) = sdl3::video::create_window(title, width.max(1), height.max(1), flags) else {
        // A window this runtime cannot make is not something to continue past:
        // every later method would be sent to a window with no SDL window
        // behind it, and each would answer plausibly and do nothing.
        objc::fail!(
            "-[NSWindow initWithContentRect:...]: SDL could not create a window: {}",
            sdl3::error()
        );
    };
    sdl3::video::set_window_position(sdl, x, y);
    crate::icon::apply(sdl);
    window.sdl = sdl;
    trace!(
        "{receiver:p} created: style {style_mask:#x} frame ({:.0},{:.0} {:.0}x{:.0}) sdl \
         ({x},{y} {width}x{height}) id {}",
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
        sdl3::video::window_id(sdl),
    );
    WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((sdl as usize, receiver as usize));
    // AppKit gives every window a plain content view, and a toolkit adds its
    // own view to it rather than replacing it - Gecko does.
    if window.content_view.is_null() {
        install_default_content_view(receiver, content_size);
    }
    receiver
}

/// An `NSView` the size of the content rectangle, as the window's content view.
fn install_default_content_view(window: Id, size: NSSize) {
    let Some(class) = objc::class::lookup(c"NSView") else {
        return;
    };
    let frame = NSRect {
        origin: NSPoint::default(),
        size,
    };
    // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)` and `-setContentView:`
    // takes the view, which it retains; this call's reference is then spent.
    unsafe {
        let init: unsafe extern "C" fn(Id, Sel, NSRect) -> Id =
            std::mem::transmute(objc::msg::entry());
        let view = init(
            objc::object::create_instance(class, 0),
            objc::sel::get("initWithFrame:"),
            frame,
        );
        if view.is_null() {
            return;
        }
        set_content_view(window, objc::sel::get("setContentView:"), view);
        objc::arc::release(view);
    }
}

/// The designated initializer's screen-aware spelling. SDL chooses the host
/// display from the rectangle, so the explicit `NSScreen` needs no separate
/// translation here.
unsafe extern "C" fn init_with_content_rect_screen(
    receiver: Id,
    sel: Sel,
    rect: NSRect,
    style_mask: usize,
    backing: usize,
    defer: u8,
    _screen: Id,
) -> Id {
    unsafe { init_with_content_rect(receiver, sel, rect, style_mask, backing, defer) }
}

/// Run `-initWithContentRect:...` for a test, answering whether SDL could
/// make the window at all - a host with no display cannot, and a test which
/// needs one skips rather than fails.
///
/// # Safety
///
/// `window` is a fresh `NSWindow` this runtime allocated.
#[cfg(test)]
pub unsafe fn init_with_content_rect_for_tests(window: Id, rect: NSRect) -> bool {
    if sdl3::video::create_window(c"", 1, 1, sdl3::video::flags::HIDDEN).is_none() {
        return false;
    }
    // SAFETY: the caller's obligation; the selector's signature is
    // `id (id, SEL, NSRect, NSUInteger, NSUInteger, BOOL)`.
    unsafe {
        init_with_content_rect(
            window,
            objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
            rect,
            style::TITLED,
            0,
            0,
        )
    };
    // SAFETY: a window this call just initialised.
    !unsafe { sdl_window(window) }.is_null()
}

/// The SDL flags for a window of `style_mask`, drawn with `api`.
///
/// Windows start without OpenGL or Vulkan. On Cocoa, the Metal flag prevents
/// SDL's default OpenGL backend from claiming the window.
/// SDL commits a window to an API at creation and mostly cannot change its
/// mind afterwards: it will convert an OpenGL window into a Vulkan one in
/// place, but it converts the flags and the loaded libraries and not the
/// backend's own state, so on Wayland the `EGLSurface`
/// `Wayland_CreateWindow` made for an `SDL_WINDOW_OPENGL` window stays alive
/// on the `wl_surface`, both drivers claim it, and the compositor answers
/// `wp_linux_drm_syncobj_manager_v1: surface already exists` and closes the
/// connection.
///
/// A window with neither flag has no `EGLSurface`, so nothing has claimed its
/// surface: `SDL_Vulkan_CreateSurface` converts it safely and Metal
/// presentation needs nothing decided in advance. OpenGL is the direction SDL
/// will not convert - `SDL_GL_CreateContext` refuses a non-OpenGL window
/// outright - so a guest which asks for an `NSOpenGLContext` has its window
/// rebuilt as an OpenGL one; see [`rebuild_for_opengl`].
fn creation_flags(style_mask: usize, api: Api) -> u64 {
    let mut flags = sdl3::video::flags::HIDDEN | sdl3::video::flags::HIGH_PIXEL_DENSITY;
    if matches!(api, Api::OpenGl) {
        flags |= sdl3::video::flags::OPENGL;
    }
    // Cocoa defaults a window with no graphics flag to OpenGL. Opt into Metal
    // so SDL can later give MoltenVK a surface without claiming it for GL.
    #[cfg(target_os = "macos")]
    if matches!(api, Api::Undecided) {
        flags |= sdl3::video::flags::METAL;
    }
    if style_mask & style::RESIZABLE != 0 {
        flags |= sdl3::video::flags::RESIZABLE;
    }
    if style_mask & style::TITLED == 0 || draws_own_titlebar() {
        flags |= sdl3::video::flags::BORDERLESS;
    }
    // Room for the chrome's rounded corners. Only on Wayland: an X11 window
    // with alpha needs a compositing manager to be anything but black there,
    // and its GL and Vulkan visuals would pass the guest's alpha through.
    if style_mask & style::TITLED != 0 && draws_own_titlebar() && sdl3::video::is_wayland() {
        flags |= sdl3::video::flags::TRANSPARENT;
    }
    flags
}

/// Whether a titled window's titlebar is this runtime's rather than the
/// window manager's.
fn draws_own_titlebar() -> bool {
    crate::decor::config().decorations == crate::decor::Decorations::Client
}

/// The graphics API a window is created for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Api {
    /// Neither OpenGL nor Vulkan: the window a guest gets until it says which
    /// it draws with, and the one a Vulkan surface can be made for.
    Undecided,
    OpenGl,
}

/// Whether `sdl` is a window the guest draws into with OpenGL.
///
/// Such a window belongs to the guest's GL context: it is presented by
/// `-[NSOpenGLContext flushBuffer]`, and nothing else may ask SDL for its
/// surface. `SDL_GetWindowSurface` on a GL window makes SDL build a framebuffer
/// of its own, which means a second GL context that SDL leaves current - after
/// which every call the guest makes lands in a context where none of its
/// objects exist.
///
/// Asked of the windows remade for the guest's GL, not of `SDL_WINDOW_OPENGL`:
/// SDL sets that flag itself when the window surface it builds is drawn by a
/// GL renderer (Wayland's is), and the store must still be presented there.
pub fn draws_with_opengl(sdl: sdl3::Window) -> bool {
    !sdl.is_null()
        && OPENGL_WINDOWS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&(sdl as usize))
}

/// SDL windows made for [`Api::OpenGl`], by address.
static OPENGL_WINDOWS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// Rebuild `receiver`'s SDL window as an OpenGL one, keeping the `NSWindow`.
///
/// `SDL_GL_CreateContext` refuses a window that was not created with
/// `SDL_WINDOW_OPENGL`, and SDL has no public way to convert one, so the
/// window has to be made again.
///
/// A guest asks for a GL context before it draws anything, which is while the
/// window is usually still hidden, so this is nearly always invisible.
/// Answers whether the window is an OpenGL window afterwards.
pub fn rebuild_for_opengl(receiver: Id) -> bool {
    let window = window_of(receiver);
    if window.sdl.is_null() {
        return false;
    }
    if draws_with_opengl(window.sdl) {
        return true;
    }
    let want = presentation(window);
    remake(receiver, Api::OpenGl, want)
}

/// Replace `receiver`'s SDL window with one made for `api` and `want`, keeping
/// the `NSWindow`.
///
/// SDL decides at creation what a window *is* - which graphics API it carries,
/// and whether it is a toplevel or a popup - and can change neither afterwards.
/// A guest changes its mind about both: it asks for a GL context on a window it
/// already made, and it hides a window, gives it a menu's level and shows it
/// again as a menu. So the window is made again.
///
/// The `NSWindow` is what the guest holds and it does not change; only the SDL
/// window behind it does, so everything keyed on that - the event table, the
/// backing store, the object's own handle - is moved over or dropped.
fn remake(receiver: Id, api: Api, want: Presentation) -> bool {
    let window = window_of(receiver);
    let old = window.sdl;
    if old.is_null() {
        return false;
    }
    // What the replacement has to come back as. The size is asked of SDL - a
    // user may have resized the window since it was made - but the position is
    // the one the guest asked for, because a compositor that places toplevels
    // itself never gave the guest's request effect in the first place.
    let (width, height) = sdl3::video::window_size(old).unwrap_or((1, 1));
    let title = sdl3::video::window_title(old);
    let shown = window.shown != 0;
    let (x, y, _, _) = crate::geometry::rect_to_sdl(window.requested);

    let flags = creation_flags(window.style_mask, api);
    let new = match want.popup_flags() {
        Some(popup) => {
            let parent = popup_parent(receiver);
            let (offset_x, offset_y) = match global_position(parent) {
                Some((parent_x, parent_y)) => (x - parent_x, y - parent_y),
                None => (x, y),
            };
            // Neither hidden nor borderless is legal on a popup, and a popup is
            // both without being told: SDL shows it with its parent.
            let flags = (flags & !sdl3::video::flags::BORDERLESS) | popup;
            sdl3::video::create_popup_window(
                parent,
                offset_x,
                offset_y,
                width.max(1),
                height.max(1),
                flags,
            )
        }
        None => sdl3::video::create_window(&title, width.max(1), height.max(1), flags),
    };
    let Some(new) = new else {
        trace!(
            "{receiver:p} could not be remade as {want:?}: {}",
            sdl3::error()
        );
        return false;
    };
    if want == Presentation::Toplevel {
        sdl3::video::set_window_position(new, x, y);
        crate::icon::apply(new);
    }
    window.sdl = new;
    if api == Api::OpenGl {
        OPENGL_WINDOWS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(new as usize);
    }
    {
        let mut windows = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
        for entry in windows.iter_mut() {
            if entry.0 == old as usize {
                entry.0 = new as usize;
            }
        }
    }
    // The old window goes only once nothing points at it any more: its SDL
    // window id appears in events already queued, and the table above is what
    // those are resolved through. Its backing store goes with it - SDL reuses
    // addresses, and a store left behind would be the next window's, at the
    // wrong size.
    // Native GL contexts on render threads may remain current across raw GL
    // calls. Retain the old parent and its popup family until those threads
    // unbind or bind their replacement drawables.
    crate::opengl::protect_remade_family(old);
    rehome_popup_descendants(old, new);
    destroy_sdl_window(old);
    update_text_input(receiver);
    trace!("{receiver:p} remade as {want:?} ({api:?})");
    if shown {
        sdl3::video::show_window(new);
        if want == Presentation::Toplevel {
            sdl3::video::raise_window(new);
        }
    }
    apply_mouse_confinement(receiver);
    true
}

/// The window's frame, asked of SDL rather than remembered.
///
/// A user can drag or resize a window, and a remembered rectangle would then be
/// where the program last put it rather than where it is.
unsafe extern "C" fn frame(receiver: Id, _sel: Sel) -> NSRect {
    if let Some(rect) = fullscreen_frame(receiver) {
        return rect;
    }
    let window = window_of(receiver);
    if window.sdl.is_null() {
        return NSRect::default();
    }
    let (w, h) = match window.size_pending != 0 {
        true => {
            let (_, _, w, h) = crate::geometry::rect_to_sdl(window.requested);
            (w, h)
        }
        false => sdl3::video::window_size(window.sdl).unwrap_or((0, 0)),
    };
    let (x, y) = match global_position(window.sdl) {
        Some(position) => position,
        None => return window.requested,
    };
    crate::geometry::rect_from_sdl(x, y, w, h)
}

/// `+windowNumberAtPoint:belowWindowWithWindowNumber:`, over this runtime's own
/// windows.
///
/// Apple's answers about every window on the screen, this process's or not.
/// Nothing here can: the windows of other applications belong to the window
/// server and SDL does not report them. A program that asks whether the pointer
/// is over one of *its* windows - which is what this is for - gets the right
/// answer; one asking whether some other application is in front is told no.
unsafe extern "C" fn window_number_at_point(
    _class: Id,
    _sel: Sel,
    point: NSPoint,
    below: isize,
) -> isize {
    // Front to back, which is the order the answer is defined in and the
    // reverse of the order windows were made in.
    let windows: Vec<Id> = all().into_iter().rev().collect();
    let start = match below {
        0 => 0,
        // SAFETY: `-windowNumber` is `NSInteger (id, SEL)`.
        _ => match windows
            .iter()
            .position(|w| unsafe { window_number(*w, objc::sel::get("windowNumber")) == below })
        {
            Some(at) => at + 1,
            // A number naming no window of this process's orders nothing.
            None => return 0,
        },
    };
    for window in &windows[start.min(windows.len())..] {
        // SAFETY: `-isVisible` and `-frame` are this module's own, called with
        // the receivers they were installed for.
        unsafe {
            if is_visible(*window, objc::sel::get("isVisible")) == 0 {
                continue;
            }
            let rect = frame(*window, objc::sel::get("frame"));
            if crate::geometry::contains(rect, point) {
                return window_number(*window, objc::sel::get("windowNumber"));
            }
        }
    }
    0
}

/// The window under a screen point the pointer is at, its events reported
/// against `reporting`: the frontmost shown window whose frame holds it, or
/// null.
///
/// Wayland's toplevels sit in the virtual desktop `placed_origin` describes,
/// which says nothing of how they overlap on the real one; there the window
/// whose surface the pointer is reported in is known to be under it.
pub(crate) fn window_at_screen_point(reporting: Id, point: NSPoint) -> Id {
    let sel = objc::sel::get("frame");
    // SAFETY: `-frame` is this module's own, sent to a window of this crate's.
    let holds = |window: Id| crate::geometry::contains(unsafe { frame(window, sel) }, point);
    if sdl3::video::is_wayland() && is_shown(reporting) && holds(reporting) {
        return reporting;
    }
    front_to_back()
        .into_iter()
        .find(|window| holds(*window))
        .unwrap_or(std::ptr::null_mut())
}

/// A point in a window's base coordinates, in screen coordinates.
pub(crate) fn to_screen(window: Id, point: NSPoint) -> NSPoint {
    // SAFETY: a window of this crate's, with the selector the method is for.
    unsafe { convert_base_to_screen(window, objc::sel::get("frame"), point) }
}

/// The inverse of [`to_screen`].
pub(crate) fn from_screen(window: Id, point: NSPoint) -> NSPoint {
    // SAFETY: as above.
    unsafe { convert_screen_to_base(window, objc::sel::get("frame"), point) }
}

/// `-sendEvent:`, the default routing.
unsafe extern "C" fn send_event(receiver: Id, _sel: Sel, event: Id) {
    crate::event::route(receiver, event);
}

/// `-mouseLocationOutsideOfEventStream`: where the pointer is now, in this
/// window's coordinates.
unsafe extern "C" fn mouse_location_outside_of_event_stream(receiver: Id, _sel: Sel) -> NSPoint {
    pointer_location(receiver)
}

#[cfg(test)]
thread_local! {
    /// A pointer in base coordinates that wins over every real source, so a
    /// test does not depend on where the host's mouse happens to be.
    pub(crate) static TEST_POINTER: std::cell::Cell<Option<NSPoint>> =
        const { std::cell::Cell::new(None) };
}

/// Where the pointer is in a window's base coordinates, for the cursor
/// rectangles as well as for the method above: [`crate::cursor`] has to decide
/// which rectangle holds the pointer, and asking through `objc_msgSend` would
/// reach a program's own override of a method it never called.
pub fn pointer_location(window: Id) -> NSPoint {
    if window.is_null() {
        return NSPoint::default();
    }
    #[cfg(test)]
    if let Some(point) = TEST_POINTER.get() {
        return point;
    }
    let point = match crate::script::pointer().or_else(crate::event::frozen_pointer) {
        Some((x, y)) => crate::geometry::point_from_sdl(x as f64, y as f64),
        None => match desktop_mouse_state() {
            Some((x, y, _)) => crate::geometry::point_from_sdl(x as f64, y as f64),
            None => return NSPoint::default(),
        },
    };
    let sel = objc::sel::get("frame");
    // SAFETY: the receiver is a window of this crate's, and both are its own
    // methods called with the receiver they were installed for.
    unsafe { convert_screen_to_base(window, sel, point) }
}

/// `-convertBaseToScreen:`, and its inverse below.
///
/// A window's base coordinates have their origin at its own bottom-left, so the
/// conversion is the frame origin - which is already in screen coordinates,
/// flipped the way AppKit wants by `geometry::rect_from_sdl`. The two are the
/// deprecated spellings of `-convertRectToScreen:`; a program that positions a
/// popover or reads the mouse in screen space still uses them.
unsafe extern "C" fn convert_base_to_screen(receiver: Id, sel: Sel, point: NSPoint) -> NSPoint {
    // SAFETY: the receiver is a window of this crate's.
    let origin = unsafe { frame(receiver, sel) }.origin;
    NSPoint {
        x: point.x + origin.x,
        y: point.y + origin.y,
    }
}

unsafe extern "C" fn convert_screen_to_base(receiver: Id, sel: Sel, point: NSPoint) -> NSPoint {
    // SAFETY: the receiver is a window of this crate's.
    let origin = unsafe { frame(receiver, sel) }.origin;
    NSPoint {
        x: point.x - origin.x,
        y: point.y - origin.y,
    }
}

/// The rectangle forms of the two above: only the origin moves, because the two
/// coordinate systems differ by a translation and nothing else.
unsafe extern "C" fn convert_rect_from_screen(receiver: Id, sel: Sel, rect: NSRect) -> NSRect {
    NSRect {
        // SAFETY: the receiver is a window of this crate's.
        origin: unsafe { convert_screen_to_base(receiver, sel, rect.origin) },
        size: rect.size,
    }
}

unsafe extern "C" fn convert_rect_to_screen(receiver: Id, sel: Sel, rect: NSRect) -> NSRect {
    NSRect {
        origin: unsafe { convert_base_to_screen(receiver, sel, rect.origin) },
        size: rect.size,
    }
}

/// The frame that holds a content rectangle under `inset` of chrome.
fn frame_for_content(rect: NSRect, inset: CGFloat) -> NSRect {
    NSRect {
        origin: rect.origin,
        size: NSSize {
            width: rect.size.width,
            height: rect.size.height + inset,
        },
    }
}

fn content_for_frame(rect: NSRect, inset: CGFloat) -> NSRect {
    NSRect {
        origin: rect.origin,
        size: NSSize {
            width: rect.size.width,
            height: (rect.size.height - inset).max(0.0),
        },
    }
}

/// `-contentRectForFrameRect:`: the frame less the chrome this runtime draws
/// above the content. With the host's decorations that is nothing, since the
/// host's title bar is outside SDL's window.
unsafe extern "C" fn content_rect_for_frame(receiver: Id, _sel: Sel, rect: NSRect) -> NSRect {
    content_for_frame(rect, content_inset(receiver))
}

unsafe extern "C" fn frame_rect_for_content(receiver: Id, _sel: Sel, rect: NSRect) -> NSRect {
    frame_for_content(rect, content_inset(receiver))
}

/// `-contentLayoutRect`: in window coordinates, the part of the frame the chrome
/// this runtime draws does not cover.
unsafe extern "C" fn content_layout_rect(receiver: Id, _sel: Sel) -> NSRect {
    // SAFETY: the receiver is a window of this crate's.
    let size = unsafe { frame(receiver, objc::sel::get("frame")) }.size;
    let whole = NSRect {
        origin: NSPoint { x: 0.0, y: 0.0 },
        size,
    };
    content_for_frame(whole, content_inset(receiver))
}

/// `-setMouseConfinementRect:` (10.13): keep the pointer inside `rect`, in window
/// coordinates, while the window has focus. An empty rectangle lifts it.
///
/// SDL2 sends it unasked-about whenever `NSAppKitVersionNumber` says 10.13, to
/// implement a grabbed or confined window.
/// Recompute guest confinement whenever the AppKit frame moves within SDL's surface.
fn mouse_confinement_in_surface(receiver: Id) -> Option<sdl3::Rect> {
    let rect = window_of(receiver).mouse_confinement;
    if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
        return None;
    }
    let mapped = rect_to_surface(receiver, rect)?;
    let points = sdl3::video::window_size(window_of(receiver).sdl)?;
    Some(pixel_rect_from_surface(mapped, points, points))
}

fn apply_mouse_confinement(receiver: Id) {
    let sdl = window_of(receiver).sdl;
    if sdl.is_null() {
        return;
    }
    if let Some(rect) = mouse_confinement_in_surface(receiver) {
        crate::screen::set_pointer_confinement(sdl, Some(rect));
    }
}

/// `-setMouseConfinementRect:` (10.13): keep guest coordinates across resizing.
unsafe extern "C" fn set_mouse_confinement_rect(receiver: Id, _sel: Sel, rect: NSRect) {
    window_of(receiver).mouse_confinement = rect;
    let sdl = window_of(receiver).sdl;
    if sdl.is_null() {
        return;
    }
    if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
        crate::screen::set_pointer_confinement(sdl, None);
    } else {
        apply_mouse_confinement(receiver);
    }
}

/// The spellings that take the style explicitly, on the class and - for some
/// older programs - on an instance. A class has no fullscreen state to ask.
unsafe extern "C" fn content_rect_for_frame_style(
    receiver: Id,
    _sel: Sel,
    rect: NSRect,
    style_mask: usize,
) -> NSRect {
    content_for_frame(rect, inset_of(receiver, style_mask, false))
}

unsafe extern "C" fn frame_rect_for_content_style(
    receiver: Id,
    _sel: Sel,
    rect: NSRect,
    style_mask: usize,
) -> NSRect {
    frame_for_content(rect, inset_of(receiver, style_mask, false))
}

unsafe extern "C" fn set_frame(receiver: Id, _sel: Sel, rect: NSRect) {
    if is_fullscreen(window_of(receiver).sdl) {
        set_fullscreen_frame(receiver, rect);
        return;
    }
    let window = window_of(receiver);
    window.requested = rect;
    let sdl = window.sdl;
    if sdl.is_null() {
        return;
    }
    let (x, y, width, height) = crate::geometry::rect_to_sdl(rect);
    let (local_x, local_y) = local_position(sdl, x, y);
    trace!(
        "{receiver:p} frame ({:.0},{:.0} {:.0}x{:.0}) -> sdl ({local_x},{local_y} \
         {width}x{height})",
        rect.origin.x, rect.origin.y, rect.size.width, rect.size.height,
    );
    sdl3::video::set_window_size(sdl, width.max(1), height.max(1));
    sdl3::video::set_window_position(sdl, local_x, local_y);
    // A hidden window takes the size at once and reports no resize, and a
    // shown one reports it later: the content view follows now either way.
    if sdl3::video::window_size(sdl) != Some((width.max(1), height.max(1))) {
        window.size_pending = 1;
    }
    fit_content_view_to(
        receiver,
        NSSize {
            width: f64::from(width.max(1)),
            height: f64::from(height.max(1)),
        },
    );
}

unsafe extern "C" fn set_frame_display(receiver: Id, sel: Sel, rect: NSRect, _display: u8) {
    // SAFETY: `-setFrame:` is `void (id, SEL, NSRect)`.
    unsafe { set_frame(receiver, sel, rect) };
}

unsafe extern "C" fn set_frame_display_animate(
    receiver: Id,
    sel: Sel,
    rect: NSRect,
    _display: u8,
    _animate: u8,
) {
    // Animation has no SDL equivalent; applying the final frame preserves the
    // observable geometry and the existing resize notifications.
    unsafe { set_frame(receiver, sel, rect) };
}

unsafe extern "C" fn set_frame_origin(
    receiver: Id,
    _sel: Sel,
    origin: foundation::geometry::NSPoint,
) {
    // A standalone origin request is ignored in a settled fullscreen Space,
    // but accepted inside will-exit callbacks (before the style bit clears).
    if is_fullscreen(window_of(receiver).sdl) {
        if in_fullscreen_transition(receiver) {
            let mut rect = unsafe { frame(receiver, objc::sel::get("frame")) };
            rect.origin = origin;
            set_fullscreen_frame(receiver, rect);
        }
        return;
    }
    let window = window_of(receiver);
    let sdl = window.sdl;
    if sdl.is_null() {
        return;
    }
    let (w, h) = sdl3::video::window_size(sdl).unwrap_or((0, 0));
    let rect = NSRect {
        origin,
        size: NSSize {
            width: w as f64,
            height: h as f64,
        },
    };
    window.requested = rect;
    let (x, y, _, _) = crate::geometry::rect_to_sdl(rect);
    let (local_x, local_y) = local_position(sdl, x, y);
    trace!(
        "{receiver:p} origin ({:.0},{:.0}) -> sdl ({local_x},{local_y})",
        origin.x, origin.y
    );
    sdl3::video::set_window_position(sdl, local_x, local_y);
}

unsafe extern "C" fn set_content_size(receiver: Id, _sel: Sel, size: NSSize) {
    let inset = content_inset(receiver);
    if is_fullscreen(window_of(receiver).sdl) {
        // Native content-size changes keep the top edge, unlike assigning
        // just the size of a bottom-left-origin frame.
        let mut rect = unsafe { frame(receiver, objc::sel::get("frame")) };
        rect.origin.y += rect.size.height - (size.height + inset);
        rect.size = NSSize {
            width: size.width,
            height: size.height + inset,
        };
        set_fullscreen_frame(receiver, rect);
        return;
    }
    let window = window_of(receiver);
    let size = NSSize {
        width: size.width,
        height: size.height + inset,
    };
    window.requested.size = size;
    let sdl = window.sdl;
    if !sdl.is_null() {
        let (width, height) = ((size.width as i32).max(1), (size.height as i32).max(1));
        sdl3::video::set_window_size(sdl, width, height);
        if sdl3::video::window_size(sdl) != Some((width, height)) {
            window.size_pending = 1;
        }
        fit_content_view_to(receiver, size);
    }
}

unsafe extern "C" fn content_view(receiver: Id, _sel: Sel) -> Id {
    window_of(receiver).content_view
}

/// `-setContentView:`, which **resizes the view to the window**.
///
/// AppKit does that, and it is not a detail: a nib's view carries the size it
/// had in Interface Builder, and a window carries the size the document asked
/// for. Deltarune's are 640x480 and 966x620, so a view left at its own size
/// draws into a fraction of the window and reports a backing store that does not
/// match it.
unsafe extern "C" fn set_content_view(receiver: Id, _sel: Sel, view: Id) {
    let previous = window_of(receiver).content_view;
    if previous != view {
        crate::view::will_move_to_window(previous, receiver, std::ptr::null_mut());
        // SAFETY: null or a view of this crate's.
        let from = unsafe { crate::view::window_of(view) };
        crate::view::will_move_to_window(view, from, receiver);
    }
    let window = window_of(receiver);
    let old = std::mem::replace(&mut window.content_view, objc::arc::retain(view));
    // The view being replaced may outlive this window: Qt keeps its own reference
    // and moves the view elsewhere, and a stale link would answer `-window` with
    // this object after it is freed.
    if old != view {
        // SAFETY: null or an object this runtime allocated.
        unsafe { crate::view::detach_window(old, receiver) };
    }
    if !window.frame_view.is_null() && old != view {
        if !old.is_null() {
            // SAFETY: `-removeFromSuperview` is `void (id, SEL)`.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
                f(old, objc::sel::get("removeFromSuperview"));
            }
        }
        if !view.is_null() {
            send_view(window.frame_view, "addSubview:", view);
        }
    }
    let moved = old != view;
    if moved {
        crate::view::did_move_to_window(old, receiver, std::ptr::null_mut());
    }
    objc::arc::release(old);
    // SAFETY: `view` is null or an object this runtime allocated.
    unsafe { crate::view::set_window(view, receiver) };
    if moved {
        crate::view::did_move_to_window(view, std::ptr::null_mut(), receiver);
    }
    if view.is_null() || window.sdl.is_null() {
        return;
    }
    fit_content_view(receiver);
    // SAFETY: a view this crate made, now this window's.
    unsafe { crate::view::update_tracking_areas(view) };
}

/// `-init`: a titled window with an empty content rectangle, through the
/// designated initializer so a subclass's runs.
unsafe extern "C" fn init_plain(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: `id (id, SEL, NSRect, NSUInteger, NSUInteger, BOOL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect, usize, usize, u8) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            receiver,
            objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
            NSRect::default(),
            style::TITLED,
            2,
            0,
        )
    }
}

unsafe extern "C" fn window_controller(receiver: Id, _sel: Sel) -> Id {
    window_of(receiver).window_controller
}

unsafe extern "C" fn set_window_controller(receiver: Id, _sel: Sel, value: Id) {
    window_of(receiver).window_controller = value;
}

unsafe extern "C" fn delegate(receiver: Id, _sel: Sel) -> Id {
    window_of(receiver).delegate
}

/// As `-[NSApplication setDelegate:]`, this does not retain: Apple's rule, and
/// the reason a window delegate outliving its window is the program's business.
unsafe extern "C" fn set_delegate(receiver: Id, _sel: Sel, value: Id) {
    window_of(receiver).delegate = value;
}

unsafe extern "C" fn set_title(receiver: Id, _sel: Sel, title: Id) {
    let sdl = window_of(receiver).sdl;
    if sdl.is_null() || title.is_null() {
        return;
    }
    // SAFETY: an `NSString`.
    let bytes = unsafe { foundation::string::bytes(title) };
    let Ok(text) = std::ffi::CString::new(bytes) else {
        return;
    };
    sdl3::video::set_window_title(sdl, &text);
}

unsafe extern "C" fn title(receiver: Id, _sel: Sel) -> Id {
    let sdl = window_of(receiver).sdl;
    let title = match sdl.is_null() {
        true => c"".to_owned(),
        false => sdl3::video::window_title(sdl),
    };
    objc::arc::autorelease(foundation::string::from_cstr(&title))
}

/// `-setRepresentedFilename:`. AppKit shows the file's proxy icon in the title
/// bar; SDL has no such thing, so the path is only kept to be read back.
unsafe extern "C" fn set_represented_filename(receiver: Id, _sel: Sel, filename: Id) {
    let window = window_of(receiver);
    let copy = match filename.is_null() {
        true => std::ptr::null_mut(),
        false => {
            // SAFETY: an `NSString`; copied, as the property is declared `copy`.
            let bytes = unsafe { foundation::string::bytes(filename) };
            foundation::string::from_str(&String::from_utf8_lossy(&bytes))
        }
    };
    objc::arc::release(window.represented_filename);
    window.represented_filename = copy;
}

unsafe extern "C" fn represented_filename(receiver: Id, _sel: Sel) -> Id {
    match window_of(receiver).represented_filename {
        filename if filename.is_null() => objc::arc::autorelease(foundation::string::from_str("")),
        filename => filename,
    }
}

/// `-setTitleWithRepresentedFilename:`: the represented filename, and the
/// file's name as the title.
unsafe extern "C" fn set_title_with_represented_filename(receiver: Id, sel: Sel, filename: Id) {
    // SAFETY: forwarded with the same receiver and argument.
    unsafe { set_represented_filename(receiver, sel, filename) };
    if filename.is_null() {
        return;
    }
    // SAFETY: an `NSString`.
    let path = unsafe { foundation::string::bytes(filename) };
    let at = path.iter().rposition(|b| *b == b'/').map_or(0, |i| i + 1);
    let title = foundation::string::from_str(&String::from_utf8_lossy(&path[at..]));
    // SAFETY: the window this was sent to, and a string this function owns.
    unsafe { set_title(receiver, sel, title) };
    objc::arc::release(title);
}

/// `-display`: redraw the whole content view at the next pump.
unsafe extern "C" fn display_window(receiver: Id, _sel: Sel) {
    let content = content_view_of(receiver);
    if !content.is_null() {
        crate::backing::needs_display(content);
    }
}

/// A window coming onto the screen with no first responder gets its initial
/// first responder, or else the first view of its key view loop that takes
/// it, before it becomes key. Qt ignores becoming key unless its view is
/// already first responder by then.
///
/// FIXME (ACCURACY): the key view loop is only the one a program wired;
/// AppKit's automatic loop over every subview is not computed.
fn set_up_first_responder(receiver: Id) {
    let window = window_of(receiver);
    if !window.first_responder.is_null() {
        return;
    }
    let initial = match window.initial_first_responder {
        initial if !initial.is_null() => initial,
        _ => default_first_responder(receiver),
    };
    if !initial.is_null() {
        // SAFETY: this window, and a responder in it.
        unsafe { make_first_responder(receiver, objc::sel::get("makeFirstResponder:"), initial) };
    }
}

fn default_first_responder(receiver: Id) -> Id {
    let content = content_view_of(receiver);
    if content.is_null() || crate::event::accepts_first_responder(content) {
        return content;
    }
    // SAFETY: `-nextValidKeyView` is `NSView *(id, SEL)`.
    let next = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(content, objc::sel::get("nextValidKeyView"))
    };
    match crate::event::accepts_first_responder(next) {
        true => next,
        false => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn make_key_and_order_front(receiver: Id, _sel: Sel, _sender: Id) {
    order_front(receiver, true);
}

unsafe extern "C" fn order_front_regardless(receiver: Id, _sel: Sel) {
    order_front(receiver, false);
}

fn order_front(receiver: Id, make_key: bool) {
    // Its views are owed their first `-updateConstraints` before it draws.
    crate::layout::note_tree_changed();
    let window = window_of(receiver);
    if window.sdl.is_null() {
        return;
    }
    let want = presentation(window);
    trace!(
        "{receiver:p} ordered front as {want:?}: style {:#x} level {}",
        window.style_mask, window.level,
    );
    // What a window is, is decided here and not at creation: a toolkit gives a
    // window its style mask and its level after making it, and reuses one
    // window as a dialog, then as a tooltip. A popup's parent has to be on the
    // screen, so a popup with nothing to hang off stays a toplevel.
    if want != presentation_of_sdl(window.sdl)
        && !(want != Presentation::Toplevel && popup_parent(receiver).is_null())
    {
        let api = match draws_with_opengl(window.sdl) {
            true => Api::OpenGl,
            false => Api::Undecided,
        };
        remake(receiver, api, want);
    }
    let window = window_of(receiver);
    let sdl = window.sdl;
    apply_style(receiver);
    // The frame again, now that the window is the kind it is going to be: a
    // popup made a moment ago was placed against whatever its parent's position
    // was then, and a toplevel remade as a popup was never placed at all.
    let (x, y, width, height) = crate::geometry::rect_to_sdl(window.requested);
    if width > 0 && height > 0 {
        let (local_x, local_y) = local_position(sdl, x, y);
        sdl3::video::set_window_size(sdl, width.max(1), height.max(1));
        sdl3::video::set_window_position(sdl, local_x, local_y);
    }
    sdl3::video::show_window(sdl);
    if want == Presentation::Toplevel {
        // A popup cannot be raised - it is already above its parent, and SDL
        // refuses - and asking would only log a failure.
        sdl3::video::raise_window(sdl);
    }
    window_of(receiver).shown = 1;
    window_of(receiver).ordered = ORDERED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    trace!(
        "{receiver:p} shown at sdl {:?} size {:?}",
        sdl3::video::window_position(sdl),
        sdl3::video::window_size(sdl),
    );
    set_up_first_responder(receiver);
    if make_key && want != Presentation::Tooltip {
        KEY.store(receiver as usize, std::sync::atomic::Ordering::Release);
        crate::event::announce_key(receiver);
    }
    // Where the window actually ended up. On a Mac a window is placed exactly
    // where it was asked to be, so a toolkit has no reason to look again; here
    // the compositor chooses, and a toolkit which never hears about it maps
    // every later pointer position against a window that is somewhere else. It
    // is also the pair of notifications a window gets when it is ordered onto
    // the screen for real, and Qt does not treat its window as exposed - or act
    // on any of the input it is sent - until they arrive.
    crate::event::window_geometry_changed(receiver);
}

/// `-setIsVisible:`, which is `-orderFront:` or `-orderOut:` by its argument.
unsafe extern "C" fn set_is_visible(receiver: Id, _sel: Sel, visible: u8) {
    match visible {
        0 => unsafe { order_out(receiver, objc::sel::get("orderOut:"), std::ptr::null_mut()) },
        _ => unsafe {
            make_key_and_order_front(
                receiver,
                objc::sel::get("orderFront:"),
                std::ptr::null_mut(),
            )
        },
    }
}

unsafe extern "C" fn order_out(receiver: Id, _sel: Sel, _sender: Id) {
    trace!("{receiver:p} ordered out");
    let sdl = window_of(receiver).sdl;
    if !sdl.is_null() {
        sdl3::video::hide_window(sdl);
    }
    window_of(receiver).shown = 0;
    let _ = KEY.compare_exchange(
        receiver as usize,
        0,
        std::sync::atomic::Ordering::AcqRel,
        std::sync::atomic::Ordering::Acquire,
    );
    crate::event::announce_resign(receiver);
}

/// `-dealloc`. The content view is told it has no window before it is released,
/// since whoever else holds it would otherwise follow `-window` into freed memory,
/// and the window leaves every table that answers with it.
///
/// The SDL window is hidden rather than destroyed: a Metal swapchain may still
/// be keyed by it, and that table is not this crate's to clear.
pub(crate) unsafe extern "C" fn dealloc(receiver: Id, sel: Sel) {
    crate::event::forget_announced(receiver);
    crate::dragging::forget(receiver);
    crate::sheet::forget(receiver);
    children()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(p, c)| *p != receiver as usize && *c != receiver as usize);
    let window = window_of(receiver);
    if !window.sdl.is_null() {
        crate::view::forget_metal_scene(window.sdl);
    }
    objc::arc::release(std::mem::replace(
        &mut window.appearance,
        std::ptr::null_mut(),
    ));
    let content = std::mem::replace(&mut window.content_view, std::ptr::null_mut());
    // SAFETY: null or an object this runtime allocated.
    unsafe { crate::view::detach_window(content, receiver) };
    objc::arc::release(content);
    objc::arc::release(std::mem::replace(
        &mut window.background,
        std::ptr::null_mut(),
    ));
    objc::arc::release(std::mem::replace(
        &mut window.represented_filename,
        std::ptr::null_mut(),
    ));
    let frame_view = std::mem::replace(&mut window.frame_view, std::ptr::null_mut());
    // SAFETY: null or an object this runtime allocated.
    unsafe { crate::view::detach_window(frame_view, receiver) };
    objc::arc::release(frame_view);
    objc::arc::release(std::mem::replace(
        &mut window.accessories,
        std::ptr::null_mut(),
    ));
    // SAFETY: `-orderOut:` is `void (id, SEL, id)`; it also gives up key status.
    unsafe { order_out(receiver, sel, std::ptr::null_mut()) };
    objc::arc::destroy_weak(&mut window.first_responder);
    objc::arc::destroy_weak(&mut window.initial_first_responder);
    WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(_, object)| *object != receiver as usize);
    objc::arc::finalize(receiver);
}

/// `-close`: the delegate hears `-windowWillClose:` while the window is still
/// on screen, and the application then decides whether that was its last one.
unsafe extern "C" fn close(receiver: Id, sel: Sel) {
    crate::event::post_for(receiver, WILL_CLOSE);
    // SAFETY: `-orderOut:` is `void (id, SEL, id)`.
    unsafe { order_out(receiver, sel, std::ptr::null_mut()) };
    crate::application::window_closed();
}

/// Consult the delegate before closing, falling back to the window itself.
unsafe extern "C" fn perform_close(receiver: Id, _sel: Sel, _sender: Id) {
    if window_of(receiver).style_mask & style::CLOSABLE == 0 {
        return;
    }
    let should_close = objc::sel::get("windowShouldClose:");
    let target = [delegate_of(receiver), receiver]
        .into_iter()
        .find(|object| {
            // SAFETY: the window and its delegate are live runtime objects or nil.
            unsafe { objc::object::class_of(*object) }
                .is_some_and(|class| class.method(should_close).is_some())
        });
    if let Some(target) = target {
        // SAFETY: `-windowShouldClose:` is `BOOL (id, SEL, id)`.
        let agrees = unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id) -> u8 =
                std::mem::transmute(objc::msg::entry());
            send(target, should_close, receiver)
        };
        if agrees == 0 {
            return;
        }
    }
    // Send the message so a subclass's close override runs too.
    // SAFETY: `-close` is `void (id, SEL)`.
    unsafe {
        let send: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
        send(receiver, objc::sel::get("close"));
    }
}

/// The window a program's keystrokes go to, which is the last one ordered front.
static KEY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn key_window() -> Id {
    KEY.load(std::sync::atomic::Ordering::Acquire) as Id
}

/// Say which window is key, from the event pump when focus moves.
pub fn set_key_window(window: Id) {
    KEY.store(window as usize, std::sync::atomic::Ordering::Release);
}

/// `-setAlphaValue:`: the window's opacity, pushed to the compositor.
///
/// SDL's opacity is the whole window's, which is what AppKit's alpha is too. A
/// compositor that will not composite a translucent window refuses, and the
/// value is still recorded: `-alphaValue` is a property a program reads back to
/// decide how far a fade has got, and answering something other than what was
/// set would break that whether or not the pixels changed.
unsafe extern "C" fn set_alpha_value(receiver: Id, _sel: Sel, alpha: CGFloat) {
    let window = window_of(receiver);
    window.alpha = alpha.clamp(0.0, 1.0);
    window.alpha_set = 1;
    if !window.sdl.is_null() {
        sdl3::video::set_window_opacity(window.sdl, window.alpha as f32);
    }
}

unsafe extern "C" fn alpha_value(receiver: Id, _sel: Sel) -> CGFloat {
    let window = window_of(receiver);
    match window.alpha_set != 0 {
        true => window.alpha,
        false => 1.0,
    }
}

/// `-setShowsToolbarButton:`, the toggle at the right of a titled window's
/// title bar.
///
/// Stored and answered. The button belongs to a title bar this runtime does not
/// draw - SDL gives the host's decorations, which have no toolbar of AppKit's to
/// toggle - so there is nothing to show or hide, and a program that sets the
/// flag while building a window and reads it back later gets its own answer.
unsafe extern "C" fn set_shows_toolbar_button(receiver: Id, _sel: Sel, shows: u8) {
    window_of(receiver).shows_toolbar_button = shows as usize;
}

unsafe extern "C" fn shows_toolbar_button(receiver: Id, _sel: Sel) -> u8 {
    (window_of(receiver).shows_toolbar_button != 0) as u8
}

/// `-setMovable:`. A window that says no cannot be moved by the *program*
/// either, which is the part this runtime can honour: see
/// [`perform_window_drag_with_event`], which refuses to start a drag on one.
///
/// It does not reach the host's own title bar. A person dragging the
/// decorations SDL asked the window system for is moving the window whatever
/// AppKit thinks, and there is no SDL call that says otherwise.
unsafe extern "C" fn set_movable(receiver: Id, _sel: Sel, movable: u8) {
    window_of(receiver).immovable = (movable == 0) as usize;
}

unsafe extern "C" fn is_movable(receiver: Id, _sel: Sel) -> u8 {
    (window_of(receiver).immovable == 0) as u8
}

/// `-performWindowDragWithEvent:`: drag the window with the pointer until the
/// button comes up.
///
/// A program with a borderless window calls this from its own `-mouseDown:` to
/// make a region behave like a title bar. AppKit takes over the event stream
/// there and then; this starts a session the event pump carries on - see
/// [`drag_moved`] - because the pump is the only thing here that sees the
/// pointer move, and a loop of its own inside this call would starve everything
/// else the pump drives.
///
/// The pointer is tracked in the *desktop's* coordinates. A window-relative
/// position moves with the window it is measured against, which would feed each
/// move back into the next one.
unsafe extern "C" fn perform_window_drag_with_event(receiver: Id, _sel: Sel, _event: Id) {
    let window = window_of(receiver);
    if window.immovable != 0 || window.sdl.is_null() {
        return;
    }
    let (Some((pointer_x, pointer_y, _)), Some((origin_x, origin_y))) = (
        sdl3::video::global_mouse_state(),
        sdl3::video::window_position(window.sdl),
    ) else {
        return;
    };
    *drag().lock().unwrap_or_else(|e| e.into_inner()) = Some(Drag {
        window: receiver as usize,
        pointer: (pointer_x, pointer_y),
        origin: (origin_x, origin_y),
    });
}

/// A window drag in progress, and what it started from.
struct Drag {
    window: usize,
    /// The pointer in desktop coordinates when the drag began.
    pointer: (f32, f32),
    /// The window's position then.
    origin: (i32, i32),
}

fn drag() -> &'static std::sync::Mutex<Option<Drag>> {
    static DRAG: std::sync::Mutex<Option<Drag>> = std::sync::Mutex::new(None);
    &DRAG
}

/// Carry a drag started by `-performWindowDragWithEvent:` one pointer move
/// further; whether one is in progress and the move belongs to it.
///
/// True means the event pump has nothing more to do with the move: the window
/// moved under the pointer, and AppKit does not also send `-mouseDragged:` for
/// a move it consumed dragging a window.
pub(crate) fn drag_moved() -> bool {
    let session = drag().lock().unwrap_or_else(|e| e.into_inner());
    let Some(session) = session.as_ref() else {
        return false;
    };
    // Through the table rather than through the object: a window closed during
    // its own drag is gone from the table, and this must not be what reads a
    // freed body to find that out.
    let sdl = WINDOWS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(_, object)| *object == session.window)
        .map_or(std::ptr::null_mut(), |(sdl, _)| *sdl as sdl3::Window);
    if sdl.is_null() {
        return false;
    }
    let Some((x, y, _)) = sdl3::video::global_mouse_state() else {
        return false;
    };
    let at = |start: i32, from: f32, to: f32| start + (to - from).round() as i32;
    sdl3::video::set_window_position(
        sdl,
        at(session.origin.0, session.pointer.0, x),
        at(session.origin.1, session.pointer.1, y),
    );
    true
}

/// End a window drag, from the pump's mouse-up. Nothing to do when none is in
/// progress, which is every mouse-up but the one.
pub(crate) fn end_drag() {
    *drag().lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// `-[NSPanel becomesKeyOnlyIfNeeded]` and its setter.
///
/// A palette that says yes wants clicks to reach its controls without taking key
/// away from the document window - a tool palette whose buttons work while the
/// text caret stays where it is. This runtime does not choose which window is
/// key: SDL does, from the window the host's compositor focuses, and
/// [`set_key_window`] follows it. So the flag is stored and answered rather than
/// acted on, which keeps a guest's own `-becomesKeyOnlyIfNeeded` read - it sets
/// it while building a palette and asks later - agreeing with what it set.
unsafe extern "C" fn becomes_key_only_if_needed(receiver: Id, _sel: Sel) -> u8 {
    (window_of(receiver).becomes_key_only_if_needed != 0) as u8
}

unsafe extern "C" fn set_becomes_key_only_if_needed(receiver: Id, _sel: Sel, value: u8) {
    window_of(receiver).becomes_key_only_if_needed = value as usize;
}

unsafe extern "C" fn hides_on_deactivate(receiver: Id, _sel: Sel) -> u8 {
    (window_of(receiver).hides_on_deactivate != 0) as u8
}

unsafe extern "C" fn set_hides_on_deactivate(receiver: Id, _sel: Sel, value: u8) {
    window_of(receiver).hides_on_deactivate = value as usize;
}

unsafe extern "C" fn is_excluded_from_windows_menu(receiver: Id, _sel: Sel) -> u8 {
    (window_of(receiver).excluded_from_windows_menu != 0) as u8
}

unsafe extern "C" fn set_excluded_from_windows_menu(receiver: Id, _sel: Sel, value: u8) {
    window_of(receiver).excluded_from_windows_menu = value as usize;
}

unsafe extern "C" fn is_key_window(receiver: Id, _sel: Sel) -> u8 {
    (key_window() == receiver) as u8
}

/// AppKit's rule: a window with a title bar or a resize bar can be key (and
/// main); a borderless one cannot unless a subclass says so.
unsafe extern "C" fn can_become_key(receiver: Id, _sel: Sel) -> u8 {
    (window_of(receiver).style_mask & (style::TITLED | style::RESIZABLE) != 0) as u8
}

/// Keep a titled window's top edge on `screen`'s visible frame, shrinking a
/// resizable one that is taller than it. A nil screen leaves `rect` alone.
unsafe extern "C" fn constrain_frame_rect(
    receiver: Id,
    _sel: Sel,
    mut rect: NSRect,
    screen: Id,
) -> NSRect {
    let mask = window_of(receiver).style_mask;
    if screen.is_null() || mask & style::TITLED == 0 {
        return rect;
    }
    // SAFETY: `-visibleFrame` is `NSRect (id, SEL)`, returned through a buffer.
    let visible = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> NSRect =
            std::mem::transmute(objc::msg::entry_stret());
        f(screen, objc::sel::get("visibleFrame"))
    };
    let top = visible.origin.y + visible.size.height;
    if mask & style::RESIZABLE != 0 && rect.size.height > visible.size.height {
        rect.size.height = visible.size.height;
    }
    if rect.origin.y + rect.size.height > top {
        rect.origin.y = top - rect.size.height;
    }
    rect
}

/// An attached sheet is visible whether or not it has a window of its own:
/// an alert's is drawn by `crate::dialog`.
unsafe extern "C" fn is_visible(receiver: Id, _sel: Sel) -> u8 {
    (window_of(receiver).shown != 0 || crate::sheet::parent_of(receiver).is_some()) as u8
}

/// `-screen`: the display SDL puts the window on, so per-display answers such
/// as a fullscreen work area reach a window on a secondary monitor.
unsafe extern "C" fn screen(receiver: Id, _sel: Sel) -> Id {
    let sdl = window_of(receiver).sdl;
    if let Some(display) = (!sdl.is_null())
        .then(|| sdl3::video::display_for_window(sdl))
        .flatten()
    {
        return crate::screen::screen_object(c"NSScreen", display);
    }
    let class = objc::class::ptr(objc::class::lookup(c"NSScreen").expect("NSScreen")) as Id;
    // SAFETY: `+mainScreen` is `id (Class, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(class, objc::sel::get("mainScreen"))
    }
}

/// `-userSpaceScaleFactor`: the backing factor, as `NSScreen`'s answers, so a
/// window and its screen agree.
unsafe extern "C" fn user_space_scale_factor(receiver: Id, sel: Sel) -> f64 {
    // SAFETY: `-backingScaleFactor` is `CGFloat (id, SEL)`.
    unsafe { backing_scale_factor(receiver, sel) }
}

unsafe extern "C" fn backing_scale_factor(receiver: Id, _sel: Sel) -> CGFloat {
    let sdl = window_of(receiver).sdl;
    match sdl.is_null() {
        true => 1.0,
        false => sdl3::video::window_pixel_density(sdl) as f64,
    }
}

fn scale_rect(rect: NSRect, scale: CGFloat) -> NSRect {
    NSRect {
        origin: NSPoint {
            x: rect.origin.x * scale,
            y: rect.origin.y * scale,
        },
        size: NSSize {
            width: rect.size.width * scale,
            height: rect.size.height * scale,
        },
    }
}

/// `-convertRectToBacking:`, the window's own spelling of the view's.
unsafe extern "C" fn convert_rect_to_backing(receiver: Id, sel: Sel, rect: NSRect) -> NSRect {
    // SAFETY: the receiver is a window.
    scale_rect(rect, unsafe { backing_scale_factor(receiver, sel) })
}

unsafe extern "C" fn convert_rect_from_backing(receiver: Id, sel: Sel, rect: NSRect) -> NSRect {
    // SAFETY: the receiver is a window.
    scale_rect(rect, 1.0 / unsafe { backing_scale_factor(receiver, sel) })
}

/// `-animator`: the window itself, as a view's is, so what a program animates
/// is set at once, which is where the animation would have ended.
unsafe extern "C" fn animator(receiver: Id, _sel: Sel) -> Id {
    receiver
}

unsafe extern "C" fn set_background_color(receiver: Id, _sel: Sel, color: Id) {
    let window = window_of(receiver);
    objc::arc::release(window.background);
    window.background = objc::arc::retain(color);
    crate::backing::window_background_changed(receiver);
}

/// The window's `backgroundColor` as straight RGBA: `windowBackgroundColor`
/// in the current appearance when the program never set one.
fn background_rgba(window: Id) -> (f64, f64, f64, f64) {
    let color = window_of(window).background;
    match color.is_null() {
        true => crate::color::named_components(WINDOW_BACKGROUND)
            .expect("windowBackgroundColor is a named colour"),
        // SAFETY: an object the window retained.
        false => unsafe { crate::color::components(color) },
    }
}

/// The window's `backgroundColor` as one premultiplied BGRA pixel.
pub(crate) fn background_pixel(window: Id) -> [u8; 4] {
    let (red, green, blue, alpha) = background_rgba(window);
    let alpha = alpha.clamp(0.0, 1.0);
    let channel = |value: f64| (value.clamp(0.0, 1.0) * alpha * 255.0).round() as u8;
    [
        channel(blue),
        channel(green),
        channel(red),
        (alpha * 255.0).round() as u8,
    ]
}

/// The colour a window has until the program sets its own.
const WINDOW_BACKGROUND: &str = "windowBackgroundColor";

unsafe extern "C" fn background_color(receiver: Id, _sel: Sel) -> Id {
    let color = window_of(receiver).background;
    match color.is_null() {
        true => crate::color::named_color(WINDOW_BACKGROUND).unwrap_or(color),
        false => color,
    }
}

unsafe extern "C" fn set_accepts_mouse_moved(receiver: Id, _sel: Sel, accepts: u8) {
    window_of(receiver).accepts_mouse_moved = accepts as usize;
}

unsafe extern "C" fn accepts_mouse_moved(receiver: Id, _sel: Sel) -> u8 {
    (window_of(receiver).accepts_mouse_moved != 0) as u8
}

unsafe extern "C" fn set_level(receiver: Id, _sel: Sel, level: isize) {
    trace!(
        "{receiver:p} level {} -> {level}",
        window_of(receiver).level
    );
    window_of(receiver).level = level;
}

unsafe extern "C" fn level(receiver: Id, _sel: Sel) -> isize {
    window_of(receiver).level
}

/// `-styleMask`, with the full-screen bit AppKit adds for the length of a
/// full-screen session. Programs ask for that bit rather than keeping their own
/// flag, and one that finds it clear will not leave full screen.
unsafe extern "C" fn style_mask(receiver: Id, _sel: Sel) -> usize {
    let window = window_of(receiver);
    match window.fullscreen != 0 {
        true => window.style_mask | style::FULL_SCREEN,
        false => window.style_mask,
    }
}

unsafe extern "C" fn set_style_mask(receiver: Id, _sel: Sel, mask: usize) {
    let window = window_of(receiver);
    // A program that edits the mask it read back hands the full-screen bit
    // along with it; that bit is the window's state, not a style to keep.
    let mask = mask & !style::FULL_SCREEN;
    if window.style_mask != mask {
        trace!("{receiver:p} style {:#x} -> {mask:#x}", window.style_mask);
    }
    window.style_mask = mask;
    apply_style(receiver);
}

/// The content size the guest last asked this window for, in the units
/// `SDL_GetWindowSize` answers in.
///
/// What a resize is measured against: a window the compositor sized exactly as
/// asked has nothing to report, and reporting it anyway is not free - see
/// [`crate::event::window_geometry_changed`].
pub(crate) fn requested_size(receiver: Id) -> Option<(i32, i32)> {
    let (_, _, width, height) = crate::geometry::rect_to_sdl(window_of(receiver).requested);
    (width > 0 && height > 0).then_some((width, height))
}

/// Give the host window what the style mask and level ask for.
///
/// The three that reach a window system: whether it is decorated, whether the
/// user may resize it, and whether it floats above ordinary windows. A popup has
/// none of them - it is undecorated, sized by its parent and already on top.
fn apply_style(receiver: Id) {
    let window = window_of(receiver);
    let sdl = window.sdl;
    if sdl.is_null() || presentation_of_sdl(sdl) != Presentation::Toplevel {
        return;
    }
    let titled = window.style_mask & style::TITLED != 0;
    sdl3::video::set_window_bordered(sdl, titled && !draws_own_titlebar());
    sdl3::video::set_window_always_on_top(sdl, window.level >= window_level::FLOATING);
    refit_inset(receiver);
    // The resizable bit is the one that is not simply followed once the
    // window is on the screen. Qt clears it on a window for as long as a popup
    // of that window is open - QTBUG-30266, "a window should not be resizable
    // while a transient popup is open" - and restores it afterwards. On a Mac
    // that greys out the zoom button while a menu is down and costs nothing.
    // Here it makes SDL pin the toplevel's minimum and maximum size to its
    // current one, so the compositor re-decorates the window - its maximize
    // button disappears - and may resize it, every time a menu opens. So the
    // bit reaches the window system while the window is off the screen, which
    // is where a program that means it sets it, and a visible window keeps the
    // decorations it has. `-styleMask` still answers with whatever was set.
    if window.shown == 0 {
        sdl3::video::set_window_resizable(sdl, window.style_mask & style::RESIZABLE != 0);
    }
}

/// Keep the content the size it was when the chrome above it changes height
/// - a window that gains or loses its titlebar, or goes full screen - by
/// growing or shrinking the frame.
pub(crate) fn refit_inset(receiver: Id) {
    let inset = content_inset(receiver);
    let window = window_of(receiver);
    let (sdl, before) = (window.sdl, window.inset);
    if sdl.is_null() || inset == before {
        return;
    }
    window.inset = inset;
    // A full-screen window's size is the screen's: nothing to keep.
    if !is_fullscreen(sdl)
        && let Some((width, height)) = sdl3::video::window_size(sdl)
    {
        let height = (f64::from(height) + inset - before).round() as i32;
        window.requested.size.height += inset - before;
        sdl3::video::set_window_size(sdl, width, height.max(1));
    }
    fit_content_view(receiver);
}

unsafe extern "C" fn collection_behavior(receiver: Id, _sel: Sel) -> usize {
    window_of(receiver).collection_behavior
}

unsafe extern "C" fn set_collection_behavior(receiver: Id, _sel: Sel, behavior: usize) {
    window_of(receiver).collection_behavior = behavior;
}

unsafe extern "C" fn first_responder(receiver: Id, _sel: Sel) -> Id {
    let window = window_of(receiver);
    match window.first_responder.is_null() {
        // A window with no first responder of its own answers with itself, as
        // AppKit's does - it is the end of its own responder chain.
        true => receiver,
        false => window.first_responder,
    }
}

pub(crate) fn is_window(object: Id) -> bool {
    // SAFETY: null or an object this runtime allocated, which every caller has.
    unsafe { objc::object::class_of(object) }.is_some_and(|c| {
        objc::class::lookup(c"NSWindow").is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    })
}

unsafe extern "C" fn next_responder(receiver: Id, _sel: Sel) -> Id {
    window_of(receiver).next_responder
}

unsafe extern "C" fn set_next_responder(receiver: Id, _sel: Sel, responder: Id) {
    window_of(receiver).next_responder = responder;
}

unsafe extern "C" fn set_initial_first_responder(receiver: Id, _sel: Sel, responder: Id) {
    objc::arc::store_weak(&mut window_of(receiver).initial_first_responder, responder);
}

unsafe extern "C" fn initial_first_responder(receiver: Id, _sel: Sel) -> Id {
    window_of(receiver).initial_first_responder
}

unsafe extern "C" fn make_first_responder(receiver: Id, _sel: Sel, responder: Id) -> u8 {
    if crate::event::tracing() {
        // SAFETY: null or a live object this runtime allocated.
        let class = unsafe { objc::object::class_of(responder) };
        eprintln!(
            "rosemary: appkit: -makeFirstResponder: {responder:p} ({})",
            class.map_or("nil", |c| c.name_str())
        );
    }
    let responder = crate::text::editing_responder(receiver, responder);
    let current = window_of(receiver).first_responder;
    if current == responder {
        update_text_input(receiver);
        return 1;
    }
    // SAFETY: null or a live object this runtime allocated.
    if !unsafe { responder_agrees(current, "resignFirstResponder") } {
        return 0;
    }
    objc::arc::store_weak(&mut window_of(receiver).first_responder, responder);
    if unsafe { responder_agrees(responder, "becomeFirstResponder") } {
        update_text_input(receiver);
        return 1;
    }
    // A refusal leaves the window itself as the responder, which is the null
    // this file spells it with - unless the refusal already moved it on.
    if window_of(receiver).first_responder == responder {
        objc::arc::store_weak(
            &mut window_of(receiver).first_responder,
            std::ptr::null_mut(),
        );
    }
    update_text_input(receiver);
    0
}

/// Keep SDL's text-input stream paired with AppKit's first responder.
///
/// SDL leaves it disabled by default. Key events still arrive in that state,
/// which made text appear to work at the engine layer while every committed
/// character and IME composition was silently absent from the text client.
fn update_text_input(receiver: Id) {
    let window = window_of(receiver);
    if window.sdl.is_null() {
        return;
    }
    match crate::text::is_input_client(window.first_responder) {
        true => {
            set_text_input_area(window);
            sdl3::video::start_text_input(window.sdl);
        }
        false => {
            sdl3::video::stop_text_input(window.sdl);
        }
    }
}

/// Keep the host input method on only while the first responder sends its keys
/// through the text system, once a key down has reached it.
///
/// AppKit's input method sees only the keys a view hands to its input context.
/// A view may stay first responder and stop doing that - Roblox keeps a hidden
/// text view focused and sends game keys around it - and a host input method
/// left on would keep composing and offering accents for those keys.
pub(crate) fn follow_key_interpretation(receiver: Id) {
    let window = window_of(receiver);
    if window.sdl.is_null() || !crate::text::is_input_client(window.first_responder) {
        return;
    }
    let interpreting = crate::text::accepts_host_text(window.first_responder);
    if interpreting == sdl3::video::text_input_active(window.sdl) {
        return;
    }
    match interpreting {
        true => {
            set_text_input_area(window);
            sdl3::video::start_text_input(window.sdl);
        }
        false => {
            sdl3::video::stop_text_input(window.sdl);
        }
    }
}

/// Whether the host input method is resolving this window's keys into text.
pub(crate) fn host_text_input_active(receiver: Id) -> bool {
    let window = window_of(receiver);
    !window.sdl.is_null() && sdl3::video::text_input_active(window.sdl)
}

fn set_text_input_area(window: &Window) {
    let area = crate::text::input_area(window.first_responder);
    // SAFETY: an input client or the text field whose editor it is; both are
    // live views from this runtime.
    if let Some((_, area)) = unsafe { crate::view::input_frame(area) } {
        sdl3::video::set_text_input_area(window.sdl, area, 0);
    }
}

/// Refresh SDL's IME rectangle after the visible editor changes geometry.
///
/// # Safety
///
/// `view` must be a live view from this runtime.
pub(crate) unsafe fn refresh_text_input_area_for(view: Id) {
    let receiver = unsafe { crate::view::window_of(view) };
    if receiver.is_null() {
        return;
    }
    let window = window_of(receiver);
    if !window.sdl.is_null()
        && crate::text::is_input_client(window.first_responder)
        && crate::text::input_area(window.first_responder) == view
    {
        set_text_input_area(window);
    }
}

/// Send one of the two first-responder messages, or take silence for consent.
///
/// `NSResponder`'s own answer to both is yes, so a responder that implements
/// neither is not refusing - and neither is the window at the end of the chain,
/// which this file spells as a null responder.
///
/// # Safety
///
/// `responder` must be null or an object this runtime allocated.
unsafe fn responder_agrees(responder: Id, selector: &str) -> bool {
    if responder.is_null() {
        return true;
    }
    let sel = objc::sel::get(selector);
    let Some(method) = unsafe { objc::object::class_of(responder) }.and_then(|c| c.method(sel))
    else {
        return true;
    };
    // SAFETY: both messages are `BOOL (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(method.implementation());
        f(responder, sel) != 0
    }
}

unsafe extern "C" fn accepts_responder_change(_receiver: Id, _sel: Sel) -> u8 {
    1
}

#[cfg(test)]
thread_local! {
    static REJECT_NEXT_FULLSCREEN_REQUEST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn request_host_fullscreen(sdl: sdl3::Window, wanted: bool) -> bool {
    #[cfg(test)]
    if REJECT_NEXT_FULLSCREEN_REQUEST.with(|reject| reject.replace(false)) {
        return false;
    }
    sdl3::video::set_window_fullscreen(sdl, wanted)
}

/// Failure is a delegate method taking NSWindow*, not an NSNotification.
fn report_fullscreen_failure(receiver: Id, entering: bool) {
    let delegate = window_of(receiver).delegate;
    if delegate.is_null() {
        return;
    }
    let sel = objc::sel::get(if entering {
        "windowDidFailToEnterFullScreen:"
    } else {
        "windowDidFailToExitFullScreen:"
    });
    let method = unsafe { objc::object::class_of(delegate) }.and_then(|c| c.method(sel));
    if let Some(method) = method {
        // SAFETY: these NSWindowDelegate callbacks take the window itself.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(method.implementation());
            f(delegate, sel, receiver);
        }
    }
}

/// Longer than any compositor's fullscreen animation. A request still
/// unanswered after this was refused or deferred, not in progress.
const FULLSCREEN_TRANSITION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Milliseconds since the first transition, plus one: the nonzero value of
/// `fullscreen_transition` is when it began.
fn transition_stamp() -> usize {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as usize
        + 1
}

/// Whether a fullscreen transition is still in progress. One the host never
/// answered ends here as a failure, or every later toggle would be ignored.
fn in_fullscreen_transition(receiver: Id) -> bool {
    let window = window_of(receiver);
    let began = window.fullscreen_transition;
    if began == 0 {
        return false;
    }
    if transition_stamp().saturating_sub(began) < FULLSCREEN_TRANSITION_TIMEOUT.as_millis() as usize
    {
        return true;
    }
    let entering = window.fullscreen != 0;
    window.fullscreen_transition = 0;
    window.fullscreen = window.reported_fullscreen;
    if entering {
        window.fullscreen_restore = NSRect::default();
    }
    report_fullscreen_failure(receiver, entering);
    false
}

/// `-toggleFullScreen:` requests an SDL transition. Keep the window alive
/// across its synchronous AppKit will-notification and handle host rejection.
unsafe extern "C" fn toggle_full_screen(receiver: Id, _sel: Sel, _sender: Id) {
    if window_of(receiver).sdl.is_null() || in_fullscreen_transition(receiver) {
        return;
    }
    let window = window_of(receiver);
    let wanted = window.fullscreen == 0;
    let was_shown = window.shown != 0;
    if wanted {
        window.fullscreen_restore = unsafe { frame(receiver, objc::sel::get("frame")) };
    }
    window.fullscreen_transition = transition_stamp();
    // Lifecycle observers may release the window or replace its SDL surface.
    // Retain AppKit ownership across callbacks and reacquire the live host handle.
    objc::arc::retain(receiver);
    crate::event::post_for(
        receiver,
        if wanted {
            WILL_ENTER_FULL_SCREEN
        } else {
            WILL_EXIT_FULL_SCREEN
        },
    );
    let live = window_of(receiver).sdl;
    let cancelled = live.is_null() || (was_shown && window_of(receiver).shown == 0);
    if !cancelled && request_host_fullscreen(live, wanted) {
        window_of(receiver).fullscreen = wanted as usize;
        // SDL only records a hidden window's request; its state event comes
        // when the window is shown, and a toggle before then is a new request.
        if sdl3::video::window_flags(live) & sdl3::video::flags::HIDDEN != 0 {
            window_of(receiver).fullscreen_transition = 0;
        }
    } else {
        window_of(receiver).fullscreen_transition = 0;
        if wanted {
            window_of(receiver).fullscreen_restore = NSRect::default();
        }
        report_fullscreen_failure(receiver, wanted);
    }
    objc::arc::release(receiver);
}

/// The `NSWindowCollectionBehavior` bits that decide full screen.
mod behavior {
    pub const FULL_SCREEN_PRIMARY: usize = 1 << 7;
    pub const FULL_SCREEN_AUXILIARY: usize = 1 << 8;
    pub const FULL_SCREEN_NONE: usize = 1 << 9;
}

/// Whether the window may go full screen: one that opted in, or, as AppKit has
/// defaulted to since 10.11, any resizable window that did not opt out.
fn can_go_full_screen(window: &Window) -> bool {
    use behavior::*;
    let behavior = window.collection_behavior;
    behavior & (FULL_SCREEN_NONE | FULL_SCREEN_AUXILIARY) == 0
        && (behavior & FULL_SCREEN_PRIMARY != 0 || window.style_mask & style::RESIZABLE != 0)
}

/// `-validateMenuItem:`: AppKit's window retitles the full-screen item for the
/// way it would go, and enables it only for a window that can go full screen.
/// Every other item the window is sent stays enabled.
unsafe extern "C" fn validate_menu_item(receiver: Id, _sel: Sel, item: Id) -> u8 {
    // SAFETY: a menu item answers `-action`, which is `SEL (id, SEL)`.
    let action = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Sel = std::mem::transmute(objc::msg::entry());
        f(item, objc::sel::get("action"))
    };
    if !std::ptr::eq(action, objc::sel::get("toggleFullScreen:")) {
        return 1;
    }
    let window = window_of(receiver);
    let title = foundation::string::from_str(match window.fullscreen != 0 {
        true => "Exit Full Screen",
        false => "Enter Full Screen",
    });
    // SAFETY: `-setTitle:` is `void (id, SEL, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(item, objc::sel::get("setTitle:"), title);
    }
    objc::arc::release(title);
    (window.fullscreen != 0 || can_go_full_screen(window)) as u8
}

unsafe extern "C" fn is_full_screen(receiver: Id, _sel: Sel) -> u8 {
    (window_of(receiver).fullscreen != 0) as u8
}

unsafe extern "C" fn is_miniaturized(receiver: Id, _sel: Sel) -> u8 {
    (window_of(receiver).miniaturized != 0) as u8
}

/// `-miniaturize:`: the window manager's minimize, where it has one, and
/// otherwise off the screen.
unsafe extern "C" fn miniaturize(receiver: Id, sel: Sel, sender: Id) {
    let window = window_of(receiver);
    window.miniaturized = 1;
    if window.sdl.is_null() || !sdl3::video::minimize_window(window.sdl) {
        // SAFETY: `-orderOut:` is `void (id, SEL, id)`.
        unsafe { order_out(receiver, sel, sender) };
    }
}

unsafe extern "C" fn deminiaturize(receiver: Id, sel: Sel, sender: Id) {
    let window = window_of(receiver);
    window.miniaturized = 0;
    if !window.sdl.is_null() {
        sdl3::video::restore_window(window.sdl);
    }
    // SAFETY: `-makeKeyAndOrderFront:` is `void (id, SEL, id)`.
    unsafe { make_key_and_order_front(receiver, sel, sender) };
}

/// A host resize/move can be queued before the matching window-state event.
/// The fullscreen transition itself posts the native-ordered geometry notices.
pub(crate) fn fullscreen_state_pending(receiver: Id) -> bool {
    if receiver.is_null() || window_of(receiver).sdl.is_null() {
        return false;
    }
    is_fullscreen(window_of(receiver).sdl) != (window_of(receiver).reported_fullscreen != 0)
}

/// Suppress the host's redundant post-transition event, not a later real move.
/// Keep the remembered frame while it matches: some WMs send multiple echoes.
pub(crate) fn redundant_fullscreen_geometry(receiver: Id, resize: Option<(i32, i32)>) -> bool {
    if receiver.is_null() {
        return false;
    }
    let window = window_of(receiver);
    let expected = window.fullscreen_announced_frame;
    let redundant = if let Some((width, height)) = resize {
        if window.fullscreen_announced_resize == 0 {
            return false;
        }
        expected.size.width == f64::from(width) && expected.size.height == f64::from(height)
    } else {
        if window.fullscreen_announced_move == 0 {
            return false;
        }
        host_frame(receiver).is_some_and(|host| host.origin == expected.origin)
    };
    if !redundant {
        if resize.is_some() {
            window_of(receiver).fullscreen_announced_resize = 0;
        } else {
            window_of(receiver).fullscreen_announced_move = 0;
        }
    }
    redundant
}

/// Follow the window manager minimizing or restoring a window, from the
/// event pump; answers the notification to post, if any.
pub(crate) fn window_state_changed(receiver: Id) -> Option<&'static str> {
    let window = window_of(receiver);
    if window.sdl.is_null() {
        return None;
    }
    let flags = sdl3::video::window_flags(window.sdl);
    let fullscreen = flags & sdl3::video::flags::FULLSCREEN != 0;
    // Against what the guest was told, not what was asked: `-toggleFullScreen:`
    // records its request before the compositor acts on it, and comparing with
    // that would swallow the notification for every change the guest made.
    let entered = fullscreen != (window.reported_fullscreen != 0);
    window.fullscreen = fullscreen as usize;
    window.reported_fullscreen = fullscreen as usize;
    let minimized = flags & sdl3::video::flags::MINIMIZED != 0;
    let changed = minimized != (window.miniaturized != 0);
    window.miniaturized = minimized as usize;
    if entered {
        window.fullscreen_frame = NSRect::default();
        window.fullscreen_transition = 0;
    }
    refit_inset(receiver);
    // SDL can report the new fullscreen state before it settles physical
    // window geometry. Recompute after clearing stale transition geometry.
    if entered {
        refresh_idle_opengl_clip(window_of(receiver).sdl);
    }
    if entered {
        crate::screen::confine_pointer(receiver);
        apply_mouse_confinement(receiver);
    }
    let restore_target = window_of(receiver).fullscreen_restore;
    if entered && !fullscreen && restore_target.size.width > 0.0 {
        let restore = std::mem::take(&mut window_of(receiver).fullscreen_restore);
        // SDL's restore state and AppKit's restore frame are separate: a
        // request from a will-enter callback must not overwrite the latter.
        unsafe { set_frame(receiver, objc::sel::get("setFrame:"), restore) };
    }
    if entered && !changed {
        // Native AppKit settles geometry before -windowDidEnter/ExitFullScreen:.
        // SDL can deliver its resize/move events only after its state event, so
        // announce the settled AppKit geometry now and discard their later echoes.
        let host_target = if !fullscreen && restore_target.size.width > 0.0 {
            restore_target
        } else {
            host_frame(receiver).unwrap_or_default()
        };
        let window = window_of(receiver);
        window.fullscreen_announced_frame = host_target;
        window.fullscreen_announced_resize = 1;
        window.fullscreen_announced_move = 1;
        fit_content_view(receiver);
        crate::event::post_for(receiver, DID_RESIZE);
        if !fullscreen {
            crate::event::post_for(receiver, DID_MOVE);
        }
    }
    match (changed, minimized, entered, fullscreen) {
        (true, true, ..) => Some(DID_MINIATURIZE),
        (true, false, ..) => Some(DID_DEMINIATURIZE),
        (_, _, true, true) => Some(DID_ENTER_FULL_SCREEN),
        (_, _, true, false) => Some(DID_EXIT_FULL_SCREEN),
        _ => None,
    }
}

/// `-zoom:`: the window manager's maximize, and back.
unsafe extern "C" fn zoom(receiver: Id, _sel: Sel, _sender: Id) {
    let window = window_of(receiver);
    if window.sdl.is_null() || window.style_mask & style::RESIZABLE == 0 {
        return;
    }
    match sdl3::video::window_flags(window.sdl) & sdl3::video::flags::MAXIMIZED != 0 {
        true => sdl3::video::restore_window(window.sdl),
        false => sdl3::video::maximize_window(window.sdl),
    };
}

/// `-isZoomed`: whether the window fills its screen.
///
/// Asked of the window's actual size rather than remembered, because a user can
/// zoom one by dragging it and a remembered flag would then be a claim about
/// what the program last did.
unsafe extern "C" fn is_zoomed(receiver: Id, sel: Sel) -> u8 {
    // SAFETY: `-frame` is `NSRect (id, SEL)`.
    let frame = unsafe { frame(receiver, sel) };
    let screen = crate::geometry::primary_height();
    (frame.size.height >= screen) as u8
}

/// The size limits, kept and reported.
///
/// SDL can be told a minimum and a maximum, and is not: this runtime does not
/// resize the window itself, so the limits only matter when the *user* drags an
/// edge - and a limit enforced here but not there would let the drag through and
/// then report a size the window does not have.
unsafe extern "C" fn min_size(receiver: Id, _sel: Sel) -> NSSize {
    window_of(receiver).minimum
}

unsafe extern "C" fn set_min_size(receiver: Id, _sel: Sel, size: NSSize) {
    window_of(receiver).minimum = size;
}

unsafe extern "C" fn max_size(receiver: Id, _sel: Sel) -> NSSize {
    window_of(receiver).maximum
}

unsafe extern "C" fn set_max_size(receiver: Id, _sel: Sel, size: NSSize) {
    window_of(receiver).maximum = size;
}

/// `-standardWindowButton:`: the close, minimise and zoom buttons.
///
/// Nil, which is what AppKit answers for a button the window's style mask does
/// not include - and an SDL window's decorations are the platform's, not
/// objects this runtime has. A program asking for one is usually about to hide
/// it, which is already the case.
unsafe extern "C" fn standard_window_button(_receiver: Id, _sel: Sel, _which: usize) -> Id {
    std::ptr::null_mut()
}

/// `-windowNumber`, which is SDL's own window id.
///
/// A number a program compares and passes around rather than one it interprets,
/// so any stable per-window integer will do - and SDL's is the one this runtime
/// already uses to route events.
unsafe extern "C" fn window_number(receiver: Id, _sel: Sel) -> isize {
    let sdl = window_of(receiver).sdl;
    match sdl.is_null() {
        true => 0,
        false => sdl3::video::window_id(sdl) as isize,
    }
}

/// Accepted and ignored, one line each.
///
/// Every one is a request about how a window *looks* that SDL either does by
/// default or cannot be told. Present rather than absent because a program calls
/// them in a row while setting a window up, and a refusal on any would stop it
/// on a line that changes nothing about whether it can draw.
/// `-setFrameTopLeftPoint:`: the origin that puts the top-left corner there,
/// keeping the height.
unsafe extern "C" fn set_frame_top_left_point(receiver: Id, sel: Sel, point: NSPoint) {
    // SAFETY: `-frame` and `-setFrameOrigin:` of the same window.
    unsafe {
        let height = frame(receiver, sel).size.height;
        set_frame_origin(
            receiver,
            sel,
            NSPoint {
                x: point.x,
                y: point.y - height,
            },
        );
    }
}

/// `-hasShadow`: AppKit's default, which is a shadow on every window with a
/// title bar. `-setHasShadow:` is not kept, as the compositor draws shadows.
unsafe extern "C" fn has_shadow(receiver: Id, _sel: Sel) -> u8 {
    u8::from(window_of(receiver).style_mask & style::TITLED != 0)
}

/// `-showsResizeIndicator`: whether the window can be resized, which is when
/// AppKit draws one.
unsafe extern "C" fn shows_resize_indicator(receiver: Id, _sel: Sel) -> u8 {
    u8::from(window_of(receiver).style_mask & style::RESIZABLE != 0)
}

/// Child windows by parent, and each child's parent.
///
/// FIXME (ACCURACY): the relation is kept and answered, but a child does not
/// move with its parent or stay ordered above it; each is its own toplevel to
/// the compositor.
fn children() -> &'static std::sync::Mutex<Vec<(usize, usize)>> {
    static TABLE: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());
    &TABLE
}

unsafe extern "C" fn add_child_window(receiver: Id, _sel: Sel, child: Id, _ordered: isize) {
    if child.is_null() {
        return;
    }
    let mut table = children().lock().unwrap_or_else(|e| e.into_inner());
    table.retain(|(_, c)| *c != child as usize);
    table.push((receiver as usize, child as usize));
}

unsafe extern "C" fn remove_child_window(receiver: Id, _sel: Sel, child: Id) {
    children()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(p, c)| !(*p == receiver as usize && *c == child as usize));
}

unsafe extern "C" fn child_windows(receiver: Id, _sel: Sel) -> Id {
    let list: Vec<corefoundation::base::CFTypeRef> = children()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(p, _)| *p == receiver as usize)
        .map(|(_, c)| *c as corefoundation::base::CFTypeRef)
        .collect();
    if list.is_empty() {
        return std::ptr::null_mut();
    }
    let array = corefoundation::collections::new_array(
        &list,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh array this call owns.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn parent_window(receiver: Id, _sel: Sel) -> Id {
    children()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(_, c)| *c == receiver as usize)
        .map_or(std::ptr::null_mut(), |(p, _)| *p as Id)
}

unsafe extern "C" fn ignored_flag(_receiver: Id, _sel: Sel, _flag: u8) {}

unsafe extern "C" fn ignored_object(_receiver: Id, _sel: Sel, _object: Id) {}

unsafe extern "C" fn set_titlebar_transparent(receiver: Id, _sel: Sel, flag: u8) {
    window_of(receiver).titlebar_transparent = usize::from(flag != 0);
}

unsafe extern "C" fn titlebar_transparent(receiver: Id, _sel: Sel) -> u8 {
    u8::from(window_of(receiver).titlebar_transparent != 0)
}

unsafe extern "C" fn set_title_visibility(receiver: Id, _sel: Sel, visibility: isize) {
    window_of(receiver).title_visibility = visibility;
}

unsafe extern "C" fn title_visibility(receiver: Id, _sel: Sel) -> isize {
    window_of(receiver).title_visibility
}

unsafe extern "C" fn ignored(_receiver: Id, _sel: Sel) {}

unsafe extern "C" fn ignored_size(_receiver: Id, _sel: Sel, _size: NSSize) {}

unsafe extern "C" fn ignored_integer(_receiver: Id, _sel: Sel, _value: isize) {}

unsafe extern "C" fn yes(_receiver: Id, _sel: Sel) -> u8 {
    1
}

unsafe extern "C" fn no(_receiver: Id, _sel: Sel) -> u8 {
    0
}

unsafe extern "C" fn no_with_object(_receiver: Id, _sel: Sel, _object: Id) -> u8 {
    0
}

unsafe extern "C" fn yes_with_object(_receiver: Id, _sel: Sel, _object: Id) -> u8 {
    1
}

/// `NSWindowOcclusionStateVisible`: no compositor here reports occlusion.
pub(crate) unsafe extern "C" fn visible_occlusion(_receiver: Id, _sel: Sel) -> u64 {
    1 << 1
}

unsafe extern "C" fn zero_border(_receiver: Id, _sel: Sel, _edge: usize) -> f64 {
    0.0
}

unsafe extern "C" fn nil(_receiver: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

unsafe extern "C" fn zero_depth(_receiver: Id, _sel: Sel) -> i32 {
    0
}

unsafe extern "C" fn ignored_border(_receiver: Id, _sel: Sel, _thickness: f64, _edge: usize) {}

unsafe extern "C" fn ignored_border_flag(_receiver: Id, _sel: Sel, _flag: u8, _edge: usize) {}

unsafe extern "C" fn ignored_depth(_receiver: Id, _sel: Sel, _depth: i32) {}

/// `-invalidateCursorRectsForView:`, which is the whole of a program's custom
/// cursor: AppKit answers it by discarding that view's rectangles and asking
/// the view to install them again through `-resetCursorRects`, and the cursor
/// a view hands to `-addCursorRect:cursor:` there is the one the pointer shows
/// over it. A runtime that ignored this left `-resetCursorRects` uncalled, so
/// the program's cursor was built and never reached the screen.
unsafe extern "C" fn invalidate_cursor_rects_for_view(_receiver: Id, _sel: Sel, view: Id) {
    // SAFETY: null or an object this runtime allocated.
    unsafe { crate::cursor::reset_rects(view) };
}

/// `-resetCursorRects` on the *window*, which rebuilds every view's.
unsafe extern "C" fn reset_cursor_rects(receiver: Id, _sel: Sel) {
    // SAFETY: null or the content view, which this crate made.
    unsafe { crate::cursor::reset_rects_in_tree(content_view_of(receiver)) };
}

unsafe extern "C" fn discard_cursor_rects(receiver: Id, _sel: Sel) {
    unsafe { crate::cursor::discard_rects_in_tree(content_view_of(receiver)) };
}

unsafe extern "C" fn enable_cursor_rects(_receiver: Id, _sel: Sel) {
    crate::cursor::set_rects_enabled(true);
}

/// `-disableCursorRects`, which a program uses while it is drawing its own
/// pointer: the rectangles stay, and none of them applies until it enables them
/// again.
unsafe extern "C" fn disable_cursor_rects(_receiver: Id, _sel: Sel) {
    crate::cursor::set_rects_enabled(false);
}

unsafe extern "C" fn are_cursor_rects_enabled(_receiver: Id, _sel: Sel) -> u8 {
    crate::cursor::rects_enabled() as u8
}

/// `-viewsNeedDisplay`: whether a view of this window is waiting for the next
/// display pass.
unsafe extern "C" fn views_need_display(receiver: Id, _sel: Sel) -> u8 {
    u8::from(crate::backing::window_needs_display(receiver))
}

/// `-setViewsNeedDisplay:`. Setting it asks for a display pass, which the event
/// pump runs on every turn anyway; clearing it is what that pass does.
unsafe extern "C" fn set_views_need_display(_receiver: Id, _sel: Sel, _flag: u8) {}

unsafe extern "C" fn appearance(receiver: Id, _sel: Sel) -> Id {
    unsafe { (*(receiver as *const Window)).appearance }
}

unsafe extern "C" fn set_appearance(receiver: Id, _sel: Sel, appearance: Id) {
    let appearance = objc::arc::retain(appearance);
    let old =
        unsafe { std::mem::replace(&mut (*(receiver as *mut Window)).appearance, appearance) };
    objc::arc::release(old);
}

unsafe extern "C" fn effective_appearance(receiver: Id, sel: Sel) -> Id {
    let own = unsafe { appearance(receiver, sel) };
    if !own.is_null() {
        return own;
    }
    crate::appearance::effective_of(crate::application::shared())
}

/// Send a getter answering an `NSRect`, which returns through `_stret`.
fn send_rect(receiver: Id, selector: &str) -> NSRect {
    if receiver.is_null() {
        return NSRect::default();
    }
    // SAFETY: `NSRect (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> NSRect =
            std::mem::transmute(objc::msg::entry_stret());
        f(receiver, objc::sel::get(selector))
    }
}

/// `-stringWithSavedFrame`: the frame and its screen's visible frame, as
/// AppKit spells them: eight numbers, each followed by a space.
unsafe extern "C" fn string_with_saved_frame(receiver: Id, _sel: Sel) -> Id {
    let frame = send_rect(receiver, "frame");
    // SAFETY: `NSScreen * (id, SEL)`.
    let screen = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("screen"))
    };
    let visible = send_rect(screen, "visibleFrame");
    let text: String = [frame, visible]
        .iter()
        .flat_map(|r| [r.origin.x, r.origin.y, r.size.width, r.size.height])
        .map(|n| format!("{} ", n.round() as i64))
        .collect();
    objc::arc::autorelease(foundation::string::from_str(&text))
}

/// `-setFrameFromString:`: the first four numbers of a saved frame.
unsafe extern "C" fn set_frame_from_string(receiver: Id, _sel: Sel, string: Id) {
    // SAFETY: an `NSString` or nil.
    let text = String::from_utf8_lossy(&unsafe { foundation::string::bytes(string) }).into_owned();
    let numbers: Vec<f64> = text
        .split_whitespace()
        .filter_map(|n| n.parse().ok())
        .collect();
    let [x, y, width, height, ..] = numbers[..] else {
        return;
    };
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let frame = NSRect {
        origin: NSPoint { x, y },
        size: NSSize { width, height },
    };
    // SAFETY: `void (id, SEL, NSRect, BOOL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect, u8) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("setFrame:display:"), frame, 1);
    }
}

pub(crate) fn install() {
    let responder = objc::class::lookup(c"NSResponder").expect("declared before its methods");
    methods!(responder, {
        "becomeFirstResponder" => accepts_responder_change, c"c16@0:8";
        "resignFirstResponder" => accepts_responder_change, c"c16@0:8";
    });
    let panel = objc::class::lookup(c"NSPanel").expect("declared before its methods");
    methods!(panel, {
        "becomesKeyOnlyIfNeeded" => becomes_key_only_if_needed, c"c16@0:8";
        "setBecomesKeyOnlyIfNeeded:" => set_becomes_key_only_if_needed, c"v20@0:8c16";
    });
    let class = objc::class::lookup(c"NSWindow").expect("declared before its methods");
    class_methods!(class, {
        "windowNumberAtPoint:belowWindowWithWindowNumber:" => window_number_at_point,
            c"q40@0:8{CGPoint=dd}16q32";
        "contentRectForFrameRect:styleMask:" => content_rect_for_frame_style,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16Q48";
        "frameRectForContentRect:styleMask:" => frame_rect_for_content_style,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16Q48";
        // Windows never merge into tabs here, which is what turning it off asks.
        "setAllowsAutomaticWindowTabbing:" => ignored_flag, c"v20@0:8c16";
    });
    methods!(class, {
        "appearance" => appearance, c"@16@0:8";
        "setAppearance:" => set_appearance, c"v24@0:8@16";
        "effectiveAppearance" => effective_appearance, c"@16@0:8";
        "init" => init_plain, c"@16@0:8";
        "initWithContentRect:styleMask:backing:defer:" => init_with_content_rect,
            c"@56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16Q48Q56c64";
        "initWithContentRect:styleMask:backing:defer:screen:" => init_with_content_rect_screen,
            c"@80@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16Q48Q56c64@72";
        "frame" => frame, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "contentRectForFrameRect:" => content_rect_for_frame,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "frameRectForContentRect:" => frame_rect_for_content,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "contentLayoutRect" => content_layout_rect, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "setMouseConfinementRect:" => set_mouse_confinement_rect,
            c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "contentRectForFrameRect:styleMask:" => content_rect_for_frame_style,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16Q48";
        "frameRectForContentRect:styleMask:" => frame_rect_for_content_style,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16Q48";
        "setFrame:" => set_frame, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "setFrame:display:" => set_frame_display, c"v52@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16c48";
        "setFrame:display:animate:" => set_frame_display_animate, c"v52@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16c48c49";
        "setFrameOrigin:" => set_frame_origin, c"v32@0:8{CGPoint=dd}16";
        "setFrameTopLeftPoint:" => set_frame_top_left_point, c"v32@0:8{CGPoint=dd}16";
        "hasShadow" => has_shadow, c"c16@0:8";
        "showsResizeIndicator" => shows_resize_indicator, c"c16@0:8";
        "addChildWindow:ordered:" => add_child_window, c"v32@0:8@16q24";
        "removeChildWindow:" => remove_child_window, c"v24@0:8@16";
        "childWindows" => child_windows, c"@16@0:8";
        "parentWindow" => parent_window, c"@16@0:8";
        "convertBaseToScreen:" => convert_base_to_screen, c"{CGPoint=dd}32@0:8{CGPoint=dd}16";
        "convertScreenToBase:" => convert_screen_to_base, c"{CGPoint=dd}32@0:8{CGPoint=dd}16";
        "convertPointFromScreen:" => convert_screen_to_base, c"{CGPoint=dd}32@0:8{CGPoint=dd}16";
        "convertPointToScreen:" => convert_base_to_screen, c"{CGPoint=dd}32@0:8{CGPoint=dd}16";
        "sendEvent:" => send_event, c"v24@0:8@16";
        "mouseLocationOutsideOfEventStream" => mouse_location_outside_of_event_stream,
            c"{CGPoint=dd}16@0:8";
        "convertRectFromScreen:" => convert_rect_from_screen,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "convertRectToScreen:" => convert_rect_to_screen,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "setContentSize:" => set_content_size, c"v32@0:8{CGSize=dd}16";
        "contentView" => content_view, c"@16@0:8";
        "addTitlebarAccessoryViewController:" => add_titlebar_accessory, c"v24@0:8@16";
        "insertTitlebarAccessoryViewController:atIndex:" => insert_titlebar_accessory, c"v32@0:8@16q24";
        "removeTitlebarAccessoryViewControllerAtIndex:" => remove_titlebar_accessory, c"v24@0:8q16";
        "titlebarAccessoryViewControllers" => titlebar_accessories, c"@16@0:8";
        "setContentView:" => set_content_view, c"v24@0:8@16";
        "delegate" => delegate, c"@16@0:8";
        "windowController" => window_controller, c"@16@0:8";
        "setWindowController:" => set_window_controller, c"v24@0:8@16";
        "setDelegate:" => set_delegate, c"v24@0:8@16";
        "title" => title, c"@16@0:8";
        "setTitle:" => set_title, c"v24@0:8@16";
        "makeKeyAndOrderFront:" => make_key_and_order_front, c"v24@0:8@16";
        "makeKeyWindow" => ignored, c"v16@0:8";
        "makeMainWindow" => ignored, c"v16@0:8";
        // Private. The number is SDL's window id and cannot be reassigned;
        // Gecko sets -1 then the old number back to reset a reshown popup in
        // the window server's cache, which this runtime has no counterpart of.
        "_setWindowNumber:" => ignored_integer, c"v24@0:8q16";
        "orderFront:" => make_key_and_order_front, c"v24@0:8@16";
        "orderFrontRegardless" => order_front_regardless, c"v16@0:8";
        "orderOut:" => order_out, c"v24@0:8@16";
        "close" => close, c"v16@0:8";
        "dealloc" => dealloc, c"v16@0:8";
        "performClose:" => perform_close, c"v24@0:8@16";
        "isKeyWindow" => is_key_window, c"c16@0:8";
        // The application's: a window has no event of its own.
        "currentEvent" => crate::application::current_event, c"@16@0:8";
        "canBecomeKeyWindow" => can_become_key, c"c16@0:8";
        "canBecomeMainWindow" => can_become_key, c"c16@0:8";
        "constrainFrameRect:toScreen:" => constrain_frame_rect,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48";
        "isMainWindow" => is_key_window, c"c16@0:8";
        "isVisible" => is_visible, c"c16@0:8";
        "setIsVisible:" => set_is_visible, c"v20@0:8c16";
        // One space: ordering any window in puts it on the active one.
        "isOnActiveSpace" => yes, c"c16@0:8";
        "screen" => screen, c"@16@0:8";
        "backingScaleFactor" => backing_scale_factor, c"d16@0:8";
        "userSpaceScaleFactor" => user_space_scale_factor, c"d16@0:8";
        "convertRectToBacking:" => convert_rect_to_backing,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "convertRectFromBacking:" => convert_rect_from_backing,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "backgroundColor" => background_color, c"@16@0:8";
        "setBackgroundColor:" => set_background_color, c"v24@0:8@16";
        "acceptsMouseMovedEvents" => accepts_mouse_moved, c"c16@0:8";
        "setAcceptsMouseMovedEvents:" => set_accepts_mouse_moved, c"v20@0:8c16";
        "level" => level, c"q16@0:8";
        "setLevel:" => set_level, c"v24@0:8q16";
        "styleMask" => style_mask, c"Q16@0:8";
        "setStyleMask:" => set_style_mask, c"v24@0:8Q16";
        "setReleasedWhenClosed:" => ignored_flag, c"v20@0:8c16";
        "isOpaque" => yes, c"c16@0:8";
        "setOpaque:" => ignored_flag, c"v20@0:8c16";
        "setHidesOnDeactivate:" => set_hides_on_deactivate, c"v20@0:8c16";
        "hidesOnDeactivate" => hides_on_deactivate, c"c16@0:8";
        "setExcludedFromWindowsMenu:" => set_excluded_from_windows_menu, c"v20@0:8c16";
        "isExcludedFromWindowsMenu" => is_excluded_from_windows_menu, c"c16@0:8";
        "orderBack:" => ignored_object, c"v24@0:8@16";
        "occlusionState" => visible_occlusion, c"Q16@0:8";
        "setToolbar:" => ignored_object, c"v24@0:8@16";
        "contentBorderThicknessForEdge:" => zero_border, c"d24@0:8Q16";
        "setDefaultButtonCell:" => ignored_object, c"v24@0:8@16";
        "setRestorable:" => ignored_flag, c"v20@0:8c16";
        // No state restoration, so nothing ever asks the class to restore.
        "setRestorationClass:" => ignored_integer, c"v24@0:8#16";
        "restorationClass" => nil, c"#16@0:8";
        "invalidateRestorableState" => ignored, c"v16@0:8";
        // Frames are not persisted, so a saved one is never there to restore.
        "saveFrameUsingName:" => ignored_integer, c"v24@0:8@16";
        "setFrameUsingName:" => no_with_object, c"c24@0:8@16";
        "setFrameAutosaveName:" => yes_with_object, c"c24@0:8@16";
        "stringWithSavedFrame" => string_with_saved_frame, c"@16@0:8";
        "setFrameFromString:" => set_frame_from_string, c"v24@0:8@16";
        // The textured bottom bar: no such chrome is drawn here.
        "setContentBorderThickness:forEdge:" => ignored_border, c"v32@0:8d16Q24";
        "setAutorecalculatesContentBorderThickness:forEdge:" => ignored_border_flag, c"v28@0:8c16Q20";
        "setShowsResizeIndicator:" => ignored_flag, c"v20@0:8c16";
        "standardWindowButton:" => standard_window_button, c"@24@0:8Q16";
        "windowNumber" => window_number, c"q16@0:8";
        "setTitlebarAppearsTransparent:" => set_titlebar_transparent, c"v20@0:8c16";
        "titlebarAppearsTransparent" => titlebar_transparent, c"c16@0:8";
        "setTitleVisibility:" => set_title_visibility, c"v24@0:8q16";
        "titleVisibility" => title_visibility, c"q16@0:8";
        "setTabbingMode:" => ignored_integer, c"v24@0:8q16";
        "setAnimationBehavior:" => ignored_integer, c"v24@0:8q16";
        "setHasShadow:" => ignored_flag, c"v20@0:8c16";
        "setOneShot:" => ignored_flag, c"v20@0:8c16";
        // The window is always presented in the host's colour space; SDL 1.2 sets sRGB.
        "setColorSpace:" => ignored_object, c"v24@0:8@16";
        // No shadow is drawn, so there is none to recompute.
        "invalidateShadow" => ignored, c"v16@0:8";
        // A no-op on every macOS since 10.10; Unity 5 still sends it.
        "useOptimizedDrawing:" => ignored_flag, c"v20@0:8c16";
        "setDocumentEdited:" => ignored_flag, c"v20@0:8c16";
        "setWorksWhenModal:" => ignored_flag, c"v20@0:8c16";
        "setResizeIncrements:" => ignored_size, c"v32@0:8{CGSize=dd}16";
        "setTitleWithRepresentedFilename:" => set_title_with_represented_filename, c"v24@0:8@16";
        "representedFilename" => represented_filename, c"@16@0:8";
        "setRepresentedFilename:" => set_represented_filename, c"v24@0:8@16";
        "ignoresMouseEvents" => no, c"c16@0:8";
        "toolbar" => nil, c"@16@0:8";
        "depthLimit" => zero_depth, c"i16@0:8";
        "setDepthLimit:" => ignored_depth, c"v20@0:8i16";
        "setDynamicDepthLimit:" => ignored_flag, c"v20@0:8c16";
        "setMovableByWindowBackground:" => ignored_flag, c"v20@0:8c16";
        "setAlphaValue:" => set_alpha_value, c"v24@0:8d16";
        "animator" => animator, c"@16@0:8";
        "alphaValue" => alpha_value, c"d16@0:8";
        "setShowsToolbarButton:" => set_shows_toolbar_button, c"v20@0:8c16";
        "showsToolbarButton" => shows_toolbar_button, c"c16@0:8";
        "setMovable:" => set_movable, c"v20@0:8c16";
        "isMovable" => is_movable, c"c16@0:8";
        "performWindowDragWithEvent:" => perform_window_drag_with_event, c"v24@0:8@16";
        "setIgnoresMouseEvents:" => ignored_flag, c"v20@0:8c16";
        "collectionBehavior" => collection_behavior, c"Q16@0:8";
        "setCollectionBehavior:" => set_collection_behavior, c"v24@0:8Q16";
        "firstResponder" => first_responder, c"@16@0:8";
        "makeFirstResponder:" => make_first_responder, c"c24@0:8@16";
        "initialFirstResponder" => initial_first_responder, c"@16@0:8";
        "nextResponder" => next_responder, c"@16@0:8";
        "setNextResponder:" => set_next_responder, c"v24@0:8@16";
        "setInitialFirstResponder:" => set_initial_first_responder, c"v24@0:8@16";
        "toggleFullScreen:" => toggle_full_screen, c"v24@0:8@16";
        "validateMenuItem:" => validate_menu_item, c"c24@0:8@16";
        "isInFullScreenMode" => is_full_screen, c"c16@0:8";
        "minSize" => min_size, c"{CGSize=dd}16@0:8";
        "setMinSize:" => set_min_size, c"v32@0:8{CGSize=dd}16";
        "maxSize" => max_size, c"{CGSize=dd}16@0:8";
        "setMaxSize:" => set_max_size, c"v32@0:8{CGSize=dd}16";
        "contentMinSize" => min_size, c"{CGSize=dd}16@0:8";
        "setContentMinSize:" => set_min_size, c"v32@0:8{CGSize=dd}16";
        "contentMaxSize" => max_size, c"{CGSize=dd}16@0:8";
        "setContentMaxSize:" => set_max_size, c"v32@0:8{CGSize=dd}16";
        "setContentAspectRatio:" => ignored_size, c"v32@0:8{CGSize=dd}16";
        "setAspectRatio:" => ignored_size, c"v32@0:8{CGSize=dd}16";
        "setContentResizeIncrements:" => ignored_size, c"v32@0:8{CGSize=dd}16";
        "zoom:" => zoom, c"v24@0:8@16";
        "performZoom:" => zoom, c"v24@0:8@16";
        "miniaturize:" => miniaturize, c"v24@0:8@16";
        "performMiniaturize:" => miniaturize, c"v24@0:8@16";
        "deminiaturize:" => deminiaturize, c"v24@0:8@16";
        "isMiniaturized" => is_miniaturized, c"c16@0:8";
        "isZoomed" => is_zoomed, c"c16@0:8";
        "center" => ignored, c"v16@0:8";
        "display" => display_window, c"v16@0:8";
        "displayIfNeeded" => ignored, c"v16@0:8";
        "viewsNeedDisplay" => views_need_display, c"c16@0:8";
        "setViewsNeedDisplay:" => set_views_need_display, c"v20@0:8c16";
        "flushWindow" => ignored, c"v16@0:8";
        "flushWindowIfNeeded" => ignored, c"v16@0:8";
        "update" => ignored, c"v16@0:8";
        "invalidateCursorRectsForView:" => invalidate_cursor_rects_for_view, c"v24@0:8@16";
        "resetCursorRects" => reset_cursor_rects, c"v16@0:8";
        "enableCursorRects" => enable_cursor_rects, c"v16@0:8";
        "disableCursorRects" => disable_cursor_rects, c"v16@0:8";
        "areCursorRectsEnabled" => are_cursor_rects_enabled, c"c16@0:8";
        "discardCursorRects" => discard_cursor_rects, c"v16@0:8";
    });
}

#[cfg(test)]
#[path = "window_fullscreen_tests.rs"]
mod fullscreen_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// What a window is, is read off its style mask and its level together -
    /// and is read again at every ordering, because a toolkit reuses one window
    /// as a dialog and then as a tooltip.
    #[test]
    fn a_windows_kind_follows_its_style_mask_and_level() {
        crate::bootstrap();
        let window =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        let body = window_of(window);

        // A titled window is a toplevel whatever its level says: a program that
        // floats its document window above the others still wants a real one.
        body.style_mask = style::TITLED | style::CLOSABLE | style::RESIZABLE;
        body.level = window_level::POPUP_MENU;
        assert_eq!(presentation(body), Presentation::Toplevel);

        // Borderless at an ordinary level is a window a program draws its own
        // frame into - a splash screen - and not a menu.
        body.style_mask = 0;
        body.level = 0;
        assert_eq!(presentation(body), Presentation::Toplevel);

        // Borderless at menu level and above are the two popups.
        body.level = window_level::POPUP_MENU;
        assert_eq!(presentation(body), Presentation::Menu);
        body.level = window_level::SCREEN_SAVER;
        assert_eq!(presentation(body), Presentation::Tooltip);

        // Neither takes the keyboard, which is what `-[QNSPanel
        // canBecomeKeyWindow]` answers for a popup.
        for kind in [Presentation::Menu, Presentation::Tooltip] {
            let flags = kind.popup_flags().expect("a popup");
            assert_ne!(flags & sdl3::video::flags::NOT_FOCUSABLE, 0, "{kind:?}");
        }
        assert_eq!(Presentation::Toplevel.popup_flags(), None);

        objc::arc::release(window);
    }

    /// The full-screen bit is the window's state: reported while full screen,
    /// and not kept as a style when a program writes back the mask it read.
    #[test]
    fn style_mask_reports_full_screen_without_keeping_it() {
        crate::bootstrap();
        let window =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        let get = objc::sel::get("styleMask");
        let set = objc::sel::get("setStyleMask:");
        let titled = style::TITLED | style::RESIZABLE;
        // SAFETY: `-styleMask` is `NSUInteger (id, SEL)` and `-setStyleMask:`
        // its setter, on a window this test made.
        unsafe {
            set_style_mask(window, set, titled);
            assert_eq!(style_mask(window, get), titled);

            window_of(window).fullscreen = 1;
            let reported = style_mask(window, get);
            assert_eq!(reported, titled | style::FULL_SCREEN);
            set_style_mask(window, set, reported);

            window_of(window).fullscreen = 0;
            assert_eq!(style_mask(window, get), titled);
        }
        objc::arc::release(window);
    }

    /// A window with no SDL window behind it still records its alpha: the
    /// property reads back what the program set, whether or not the host
    /// composites it.
    #[test]
    fn a_window_reports_the_alpha_it_was_set() {
        crate::bootstrap();
        let window =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        let get = objc::sel::get("alphaValue");
        let set = objc::sel::get("setAlphaValue:");
        // SAFETY: `-alphaValue` is `CGFloat (id, SEL)` and `-setAlphaValue:` its
        // setter, on a window this test made.
        unsafe {
            assert_eq!(alpha_value(window, get), 1.0, "a fresh window is opaque");
            set_alpha_value(window, set, 0.25);
            assert_eq!(alpha_value(window, get), 0.25);
            // Zero is a legal alpha, and has to be told apart from "never set".
            set_alpha_value(window, set, 0.0);
            assert_eq!(alpha_value(window, get), 0.0);
            // Out of range is clamped rather than passed on to the compositor.
            set_alpha_value(window, set, 4.0);
            assert_eq!(alpha_value(window, get), 1.0);
            set_alpha_value(window, set, -1.0);
            assert_eq!(alpha_value(window, get), 0.0);
        }
        objc::arc::release(window);
    }

    /// The two flags that are stored and answered, each defaulting the way
    /// AppKit's does: a window is movable and shows no toolbar button.
    #[test]
    fn a_window_keeps_its_movable_and_toolbar_flags() {
        crate::bootstrap();
        let window =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        // SAFETY: each is the accessor its selector declares.
        unsafe {
            let (get, set) = (objc::sel::get("isMovable"), objc::sel::get("setMovable:"));
            assert_eq!(is_movable(window, get), 1, "movable unless told otherwise");
            set_movable(window, set, 0);
            assert_eq!(is_movable(window, get), 0);
            set_movable(window, set, 1);
            assert_eq!(is_movable(window, get), 1);

            let (get, set) = (
                objc::sel::get("showsToolbarButton"),
                objc::sel::get("setShowsToolbarButton:"),
            );
            assert_eq!(shows_toolbar_button(window, get), 0);
            set_shows_toolbar_button(window, set, 1);
            assert_eq!(shows_toolbar_button(window, get), 1);
        }
        objc::arc::release(window);
    }

    /// An immovable window refuses the drag, and a window with no SDL window
    /// behind it starts none - neither may leave a session the pump would then
    /// carry on against nothing.
    #[test]
    fn a_drag_needs_a_movable_window() {
        crate::bootstrap();
        let window =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        let sel = objc::sel::get("performWindowDragWithEvent:");
        // SAFETY: `-performWindowDragWithEvent:` is `void (id, SEL, NSEvent *)`.
        unsafe {
            set_movable(window, objc::sel::get("setMovable:"), 0);
            perform_window_drag_with_event(window, sel, std::ptr::null_mut());
            assert!(
                drag().lock().unwrap_or_else(|e| e.into_inner()).is_none(),
                "an immovable window does not drag"
            );
            // Movable, but with no SDL window: there is nothing to position.
            set_movable(window, objc::sel::get("setMovable:"), 1);
            perform_window_drag_with_event(window, sel, std::ptr::null_mut());
            assert!(drag().lock().unwrap_or_else(|e| e.into_inner()).is_none());
        }
        assert!(!drag_moved(), "no session, nothing consumed");
        end_drag();
        objc::arc::release(window);
    }

    /// A panel answers the flag it was set, and only a panel is asked it: the
    /// property is `NSPanel`'s, and a plain window that inherited it would
    /// answer a question AppKit does not put to it.
    #[test]
    fn a_panel_keeps_its_becomes_key_only_if_needed() {
        crate::bootstrap();
        let panel =
            objc::object::create_instance(objc::class::lookup(c"NSPanel").expect("NSPanel"), 0);
        let get = objc::sel::get("becomesKeyOnlyIfNeeded");
        // SAFETY: the getter and setter of a panel this test made.
        unsafe {
            assert_eq!(becomes_key_only_if_needed(panel, get), 0, "off by default");
            set_becomes_key_only_if_needed(panel, objc::sel::get("setBecomesKeyOnlyIfNeeded:"), 1);
            assert_eq!(becomes_key_only_if_needed(panel, get), 1);
        }
        assert!(
            objc::class::lookup(c"NSWindow")
                .and_then(|class| class.method(get))
                .is_none(),
            "a window is not asked a panel's question"
        );
        objc::arc::release(panel);
    }

    /// A view that outlives its window must not answer `-window` with it:
    /// Studio's dock panels are freed while Qt still holds their views.
    #[test]
    fn a_freed_window_leaves_its_content_view_windowless() {
        crate::bootstrap();
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        let class = objc::class::lookup(c"NSView").unwrap();
        let view = objc::object::create_instance(class, 0);
        // SAFETY: a window and a view this test made; the selectors take an id.
        unsafe {
            set_content_view(window, objc::sel::get("setContentView:"), view);
            assert_eq!(crate::view::window_of(view), window);
            objc::arc::release(window);
            assert!(crate::view::window_of(view).is_null());
        }
        assert_eq!(objc::arc::retain_count(view), 1);
        objc::arc::release(view);
    }

    #[test]
    fn perform_close_respects_style_and_delegate_and_dispatches_close() {
        crate::bootstrap();
        let base = objc::class::lookup(c"NSWindow").unwrap();
        let subclass = objc::class::allocate_pair(Some(base), c"PerformCloseWindow", 0).unwrap();
        unsafe extern "C" fn veto(receiver: Id, sel: Sel, window: Id) -> u8 {
            assert_eq!(sel, objc::sel::get("windowShouldClose:"));
            assert_eq!(receiver, window);
            0
        }
        unsafe extern "C" fn allow(_receiver: Id, sel: Sel, window: Id) -> u8 {
            assert_eq!(sel, objc::sel::get("windowShouldClose:"));
            assert!(!window.is_null());
            1
        }
        unsafe extern "C" fn overridden_close(receiver: Id, sel: Sel) {
            assert_eq!(sel, objc::sel::get("close"));
            window_of(receiver).shown = 2;
        }
        subclass.add_method(
            objc::sel::get("windowShouldClose:"),
            veto as objc::class::Imp,
            c"c24@0:8@16",
        );
        subclass.add_method(
            objc::sel::get("close"),
            overridden_close as objc::class::Imp,
            c"v16@0:8",
        );
        objc::class::register_pair(subclass);
        let delegate_class = objc::class::allocate_pair(
            objc::class::lookup(c"NSObject"),
            c"PerformCloseDelegate",
            0,
        )
        .unwrap();
        delegate_class.add_method(
            objc::sel::get("windowShouldClose:"),
            allow as objc::class::Imp,
            c"c24@0:8@16",
        );
        objc::class::register_pair(delegate_class);
        let delegate = objc::object::create_instance(delegate_class, 0);
        let window = objc::object::create_instance(subclass, 0);
        let plain = objc::object::create_instance(base, 0);
        // SAFETY: live runtime windows; `-performClose:` is `void (id, SEL, id)`.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            let sel = objc::sel::get("performClose:");
            window_of(window).shown = 1;
            send(window, sel, delegate);
            assert_eq!(window_of(window).shown, 1);
            window_of(window).style_mask = style::TITLED | style::CLOSABLE;
            send(window, sel, delegate);
            assert_eq!(window_of(window).shown, 1);
            window_of(window).delegate = delegate;
            send(window, sel, std::ptr::null_mut());
            assert_eq!(window_of(window).shown, 2);
            window_of(plain).shown = 1;
            window_of(plain).style_mask = style::TITLED | style::CLOSABLE;
            send(plain, sel, delegate);
            assert_eq!(window_of(plain).shown, 0);
            window_of(plain).shown = 1;
            window_of(plain).delegate =
                objc::object::create_instance(objc::class::lookup(c"NSObject").unwrap(), 0);
            send(plain, sel, std::ptr::null_mut());
            assert_eq!(window_of(plain).shown, 0);
        }
    }

    #[test]
    fn close_tells_the_delegate_the_window_will_close() {
        crate::bootstrap();
        static TOLD: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        unsafe extern "C" fn will_close(_receiver: Id, _sel: Sel, notification: Id) {
            assert!(!notification.is_null());
            TOLD.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let class =
            objc::class::allocate_pair(objc::class::lookup(c"NSObject"), c"WillCloseDelegate", 0)
                .unwrap();
        class.add_method(
            objc::sel::get("windowWillClose:"),
            will_close as objc::class::Imp,
            c"v24@0:8@16",
        );
        objc::class::register_pair(class);
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        window_of(window).delegate = objc::object::create_instance(class, 0);
        window_of(window).shown = 1;
        // SAFETY: a live runtime window; `-close` is `void (id, SEL)`.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
            send(window, objc::sel::get("close"));
        }
        assert_eq!(TOLD.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(window_of(window).shown, 0);
    }

    static REDIRECT: std::sync::Mutex<(usize, usize)> = std::sync::Mutex::new((0, 0));

    unsafe extern "C" fn redirecting_become(_receiver: Id, _sel: Sel) -> u8 {
        let (window, to) = *REDIRECT.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the window and view the test made.
        unsafe { make_first_responder(window as Id, objc::sel::get("x"), to as Id) };
        1
    }

    #[test]
    fn a_window_can_be_given_a_vulkan_surface_until_opengl_is_requested() {
        use sdl3::video::flags;

        // The default. A window with neither flag has no `EGLSurface`, so
        // `SDL_Vulkan_CreateSurface` can convert it and Metal presentation
        // needs nothing known in advance about the guest.
        let plain = creation_flags(style::TITLED | style::RESIZABLE, Api::Undecided);
        assert_eq!(plain & flags::OPENGL, 0);
        assert_eq!(plain & flags::VULKAN, 0);
        assert_eq!(plain & flags::METAL != 0, cfg!(target_os = "macos"));

        // A guest that asks for an `NSOpenGLContext` gets the window rebuilt
        // as this, because SDL will not convert to OpenGL.
        let gl = creation_flags(style::TITLED | style::RESIZABLE, Api::OpenGl);
        assert_ne!(gl & flags::OPENGL, 0);
        assert_eq!(gl & flags::VULKAN, 0);
        assert_eq!(gl & flags::METAL, 0);

        // Never both, whatever the style: SDL refuses such a window, and the
        // whole point is that at most one driver owns the surface.
        for style in [0, style::TITLED, style::RESIZABLE, style::CLOSABLE] {
            for api in [Api::Undecided, Api::OpenGl] {
                let flags = creation_flags(style, api);
                assert!(
                    flags & flags::OPENGL == 0 || flags & flags::VULKAN == 0,
                    "a window was asked for both graphics APIs"
                );
            }
        }

        // The rest of the mapping is unchanged by any of it.
        let api = Api::Undecided;
        assert_ne!(creation_flags(style::RESIZABLE, api) & flags::RESIZABLE, 0);
        assert_eq!(
            creation_flags(style::TITLED, api) & flags::BORDERLESS != 0,
            draws_own_titlebar(),
            "a titled window is bordered exactly when the host draws its titlebar"
        );
        assert_ne!(creation_flags(0, api) & flags::BORDERLESS, 0);
        assert_ne!(creation_flags(0, api) & flags::HIDDEN, 0);
    }

    /// A window becomes an OpenGL window only when one is asked for, and
    /// keeps its identity and its place when it does.
    #[test]
    fn a_window_rebuilt_for_opengl_keeps_what_it_had() {
        if !sdl3::isolated::run_here() {
            return;
        }
        crate::bootstrap();
        let object =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        // SAFETY: a fresh window this test made.
        let made = unsafe {
            init_with_content_rect_for_tests(
                object,
                NSRect {
                    origin: NSPoint { x: 0.0, y: 0.0 },
                    size: NSSize {
                        width: 320.0,
                        height: 240.0,
                    },
                },
            )
        };
        if !made {
            return;
        }
        // SAFETY: a window this test made.
        let before = unsafe { sdl_window(object) };
        assert_eq!(
            sdl3::video::window_flags(before) & sdl3::video::flags::OPENGL,
            0,
            "a window claimed OpenGL before anything asked for it"
        );
        sdl3::video::set_window_title(before, c"a title");
        sdl3::video::set_window_position(before, 64, 48);
        let size = sdl3::video::window_size(before).expect("a window");
        assert!(!draws_with_opengl(before));

        assert!(rebuild_for_opengl(object));
        let after = unsafe { sdl_window(object) };
        assert_ne!(after, before, "the window was not rebuilt");
        assert_ne!(
            sdl3::video::window_flags(after) & sdl3::video::flags::OPENGL,
            0,
            "the rebuilt window is not an OpenGL window"
        );
        assert!(
            draws_with_opengl(after),
            "the rebuilt window does not own its surface, so the backing store \
             would ask SDL for it and take the guest's GL context away"
        );
        assert_eq!(sdl3::video::window_size(after), Some(size));
        let title = sdl3::video::window_title(after);
        assert_eq!(
            title.to_bytes(),
            b"a title",
            "the rebuilt window lost its title"
        );
        // The event table has to follow the window, or every event for it
        // stops resolving to this object.
        assert_eq!(window_for(after), object);
        assert!(sdl_windows().contains(&after));
        assert!(!sdl_windows().contains(&before));

        // Asking twice is not two rebuilds.
        assert!(rebuild_for_opengl(object));
        assert_eq!(unsafe { sdl_window(object) }, after);
    }

    /// An explicitly ordered-out popup must not reappear when its parent is
    /// remade, even though SDL shows native child popups with their parent.
    #[test]
    fn hidden_popup_stays_hidden_across_parent_remake_until_order_front() {
        if !sdl3::isolated::run_here() {
            return;
        }

        crate::bootstrap();
        let Some(probe) = sdl3::video::create_window(c"hidden popup preflight", 32, 32, 0) else {
            return; // no graphical SDL display available
        };
        sdl3::video::destroy_window(probe);
        let rect = NSRect {
            origin: NSPoint { x: 20.0, y: 20.0 },
            size: NSSize {
                width: 180.0,
                height: 120.0,
            },
        };
        let class = objc::class::lookup(c"NSWindow").unwrap();
        let make = || {
            let object = objc::object::create_instance(class, 0);
            unsafe {
                init_with_content_rect(
                    object,
                    objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
                    rect,
                    0,
                    0,
                    0,
                )
            }
        };
        // Both undecided (potential Vulkan) and OpenGL-owned popup paths
        // must retain the guest's ordered-out state.
        for gl_child in [false, true] {
            let parent = make();
            let child = make();
            order_front(parent, true);
            unsafe { set_level(child, objc::sel::get("setLevel:"), window_level::POPUP_MENU) };
            order_front(child, false);
            if gl_child {
                assert!(rebuild_for_opengl(child), "convert child to OpenGL");
            }
            let original_parent = unsafe { sdl_window(parent) };
            let original_child = unsafe { sdl_window(child) };
            assert_eq!(sdl3::video::window_parent(original_child), original_parent);
            assert!(is_shown(child));
            assert_eq!(
                sdl3::video::window_flags(original_child) & sdl3::video::flags::HIDDEN,
                0
            );

            // Exercise the public Objective-C guest operation, not simply
            // mutate AppKit's visibility marker.
            unsafe {
                let order: unsafe extern "C" fn(Id, Sel, Id) =
                    std::mem::transmute(objc::msg::entry());
                order(child, objc::sel::get("orderOut:"), std::ptr::null_mut());
            }
            assert!(!is_shown(child));
            assert_ne!(
                sdl3::video::window_flags(original_child) & sdl3::video::flags::HIDDEN,
                0
            );

            assert!(rebuild_for_opengl(parent), "parent remake");
            let replacement_parent = unsafe { sdl_window(parent) };
            let replacement_child = unsafe { sdl_window(child) };
            assert_ne!(replacement_parent, original_parent);
            assert_ne!(replacement_child, original_child);
            assert_eq!(
                sdl3::video::window_parent(replacement_child),
                replacement_parent
            );
            assert_eq!(window_for(replacement_child), child);
            assert!(is_shown(parent));
            assert!(!is_shown(child), "AppKit resurrected the ordered-out popup");
            assert_ne!(
                sdl3::video::window_flags(replacement_child) & sdl3::video::flags::HIDDEN,
                0,
                "SDL resurrected the ordered-out popup when its parent was shown"
            );
            assert_eq!(draws_with_opengl(replacement_child), gl_child);

            unsafe {
                let order: unsafe extern "C" fn(Id, Sel, Id) =
                    std::mem::transmute(objc::msg::entry());
                order(child, objc::sel::get("orderFront:"), std::ptr::null_mut());
            }
            assert!(is_shown(child));
            assert_eq!(
                sdl3::video::window_flags(replacement_child) & sdl3::video::flags::HIDDEN,
                0,
                "orderFront did not restore the popup's native visibility"
            );
            assert_eq!(
                unsafe { sdl_window(child) },
                replacement_child,
                "orderFront unnecessarily remade the popup"
            );
        }
    }

    /// A guest NSOpenGLContext must keep its native context when its SDL popup
    /// is reparented by an unrelated parent window recreation.
    #[test]
    fn gl_popup_rehome_preserves_guest_context_and_surface_ownership() {
        if !sdl3::isolated::run_here() {
            return;
        }

        crate::bootstrap();
        let Some(probe) = sdl3::video::create_window(c"GL test preflight", 32, 32, 0) else {
            return; // graphical display unavailable
        };
        sdl3::video::destroy_window(probe);
        let rect = NSRect {
            origin: NSPoint { x: 20.0, y: 20.0 },
            size: NSSize {
                width: 180.0,
                height: 120.0,
            },
        };
        let class = objc::class::lookup(c"NSWindow").unwrap();
        let make = || {
            let object = objc::object::create_instance(class, 0);
            // SAFETY: normal designated NSWindow initializer and a fresh object.
            unsafe {
                init_with_content_rect(
                    object,
                    objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
                    rect,
                    0,
                    0,
                    0,
                )
            }
        };
        let parent = make();
        let child = make();
        order_front(parent, true);
        unsafe { set_level(child, objc::sel::get("setLevel:"), window_level::POPUP_MENU) };
        order_front(child, false);
        let original_parent = unsafe { sdl_window(parent) };
        assert!(!original_parent.is_null());
        assert!(rebuild_for_opengl(child));
        let original_popup = unsafe { sdl_window(child) };
        assert_eq!(sdl3::video::window_parent(original_popup), original_parent);
        assert!(draws_with_opengl(original_popup));

        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        unsafe {
            crate::view::init_with_frame(view, objc::sel::get("initWithFrame:"), rect);
            set_content_view(child, objc::sel::get("setContentView:"), view);
        }
        let context =
            objc::object::create_instance(objc::class::lookup(c"NSOpenGLContext").unwrap(), 0);
        // Real Objective-C entrypoints of the guest NSOpenGLContext.
        unsafe {
            let init: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            assert_eq!(
                init(
                    context,
                    objc::sel::get("initWithFormat:shareContext:"),
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                context
            );
            let set_view: unsafe extern "C" fn(Id, Sel, Id) =
                std::mem::transmute(objc::msg::entry());
            set_view(context, objc::sel::get("setView:"), view);
            let make_current: unsafe extern "C" fn(Id, Sel) =
                std::mem::transmute(objc::msg::entry());
            make_current(context, objc::sel::get("makeCurrentContext"));
        }
        let native = sdl3::video::gl_current_context();
        assert!(!native.is_null(), "guest created no native GL context");
        assert_eq!(sdl3::video::gl_current_window(), original_popup);

        assert!(rebuild_for_opengl(parent));
        let new_parent = unsafe { sdl_window(parent) };
        let new_popup = unsafe { sdl_window(child) };
        assert_ne!(new_popup, original_popup);
        assert_eq!(sdl3::video::window_parent(new_popup), new_parent);
        assert_ne!(
            sdl3::video::window_flags(new_popup) & sdl3::video::flags::OPENGL,
            0
        );
        assert!(
            draws_with_opengl(new_popup),
            "GL popup lost OPENGL_WINDOWS ownership"
        );
        assert!(
            !draws_with_opengl(original_popup),
            "old popup ownership was not retired"
        );
        assert_eq!(window_for(new_popup), child);
        assert!(window_for(original_popup).is_null());

        // The guest kept its NSOpenGLContext current throughout the parent
        // remake. No second makeCurrentContext call is allowed here.
        assert_eq!(
            sdl3::video::gl_current_context(),
            native,
            "parent remake detached the guest's original GL context"
        );
        assert_eq!(
            sdl3::video::gl_current_window(),
            new_popup,
            "parent remake failed to rebind the already-current context"
        );
        // Exercise real driver drawing and readback against the replacement
        // drawable before the guest flushes it, not merely an SDL identity.
        unsafe {
            let clear_color: unsafe extern "C" fn(f32, f32, f32, f32) =
                std::mem::transmute(sdl3::video::gl_proc_address(c"glClearColor").unwrap());
            let clear: unsafe extern "C" fn(u32) =
                std::mem::transmute(sdl3::video::gl_proc_address(c"glClear").unwrap());
            let read: unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, *mut u8) =
                std::mem::transmute(sdl3::video::gl_proc_address(c"glReadPixels").unwrap());
            clear_color(0.1, 0.75, 0.25, 1.0);
            clear(0x4000); // GL_COLOR_BUFFER_BIT
            let mut pixel = [0_u8; 4];
            read(8, 8, 1, 1, 0x1908, 0x1401, pixel.as_mut_ptr()); // RGBA, UNSIGNED_BYTE
            assert!(
                pixel[1] > 160 && pixel[0] < 90 && pixel[2] < 130,
                "GL drew to the wrong or an invalid popup drawable: {pixel:?}"
            );
            let flush: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
            flush(context, objc::sel::get("flushBuffer"));
        }
        assert_eq!(sdl3::video::gl_current_context(), native);
        assert_eq!(sdl3::video::gl_current_window(), new_popup);

        // Exercise the actual backing-store pump: this child gets a composed
        // dirty store but must never try SDL_GetWindowSurface on the GL window.
        crate::backing::queue_dirty_store_for_tests(new_popup);
        crate::backing::pump();
        assert!(crate::backing::has_store_for_tests(new_popup));
        crate::backing::present_again(new_popup);
        crate::backing::pump();
        assert_eq!(
            crate::backing::cpu_fallback_calls_for_tests(new_popup),
            0,
            "backing store attempted to replace the guest's OpenGL surface"
        );
        assert_eq!(sdl3::video::gl_current_context(), native);
        assert_eq!(sdl3::video::gl_current_window(), new_popup);
        assert!(
            rebuild_for_opengl(child),
            "existing GL popup needlessly rebuilt"
        );
        assert_eq!(unsafe { sdl_window(child) }, new_popup);
    }

    /// Parent remake must observe a binding still between native make-current
    /// and its publication to the cross-thread drawable registry.
    #[test]
    fn gl_popup_rehome_waits_for_inflight_native_binding() {
        if !sdl3::isolated::run_here() {
            return;
        }
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};

        crate::bootstrap();
        let rect = NSRect {
            origin: NSPoint { x: 10.0, y: 10.0 },
            size: NSSize {
                width: 150.0,
                height: 100.0,
            },
        };
        let class = objc::class::lookup(c"NSWindow").unwrap();
        let make = || {
            let object = objc::object::create_instance(class, 0);
            unsafe {
                init_with_content_rect(
                    object,
                    objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
                    rect,
                    0,
                    0,
                    0,
                )
            }
        };
        let parent = make();
        let child = make();
        order_front(parent, true);
        unsafe { set_level(child, objc::sel::get("setLevel:"), window_level::POPUP_MENU) };
        order_front(child, false);
        assert!(rebuild_for_opengl(child));
        let old = unsafe { sdl_window(child) };
        let old_id = sdl3::video::window_id(old);
        let old_addr = old as usize;
        assert_ne!(old_id, 0);
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        unsafe {
            crate::view::init_with_frame(view, objc::sel::get("initWithFrame:"), rect);
            set_content_view(child, objc::sel::get("setContentView:"), view);
        }
        let context =
            objc::object::create_instance(objc::class::lookup(c"NSOpenGLContext").unwrap(), 0);
        unsafe {
            let init: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            assert_eq!(
                init(
                    context,
                    objc::sel::get("initWithFormat:shareContext:"),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ),
                context
            );
            let set_view: unsafe extern "C" fn(Id, Sel, Id) =
                std::mem::transmute(objc::msg::entry());
            set_view(context, objc::sel::get("setView:"), view);
        }

        crate::opengl::test_pause_binding(old);
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let context_addr = context as usize;
        let worker = std::thread::spawn(move || {
            let context = context_addr as Id;
            unsafe {
                let make_current: unsafe extern "C" fn(Id, Sel) =
                    std::mem::transmute(objc::msg::entry());
                make_current(context, objc::sel::get("makeCurrentContext"));
            }
            resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            unsafe {
                let flush: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
                flush(context, objc::sel::get("flushBuffer"));
            }
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !crate::opengl::test_binding_paused() {
            assert!(
                Instant::now() < deadline,
                "GL binding never reached test barrier"
            );
            std::thread::yield_now();
        }
        // The main thread tries to recreate the parent while the worker holds
        // the binding gate after the real SDL_GL_MakeCurrent succeeded.
        let protected = std::sync::Arc::new(AtomicBool::new(false));
        let released = std::sync::Arc::new(AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&protected);
        let release_flag = std::sync::Arc::clone(&released);
        let releaser = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(5);
            while !crate::opengl::test_protection_waiting() {
                if Instant::now() >= until {
                    crate::opengl::test_resume_binding();
                    return;
                }
                std::thread::yield_now();
            }
            flag.store(true, Ordering::Release);
            // The old drawable must still exist while registration is pending.
            assert_eq!(
                sdl3::video::live_window_id(old_addr as sdl3::Window),
                Some(old_id)
            );
            release_flag.store(true, Ordering::Release);
            crate::opengl::test_resume_binding();
        });

        assert!(rebuild_for_opengl(parent));
        releaser.join().unwrap();
        assert!(
            protected.load(Ordering::Acquire),
            "retirement missed binding gate"
        );
        assert!(
            released.load(Ordering::Acquire),
            "bind wasn't released at protection"
        );
        let new = unsafe { sdl_window(child) };
        assert_ne!(old, new);
        sdl3::video::vulkan::service_surface_requests();
        assert_eq!(
            sdl3::video::window_from_id(old_id),
            old,
            "native popup freed even though render thread has not rebound"
        );
        assert_eq!(crate::opengl::retired_drawable_count_for_tests(), 1);

        resume_tx.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(crate::opengl::retired_drawable_count_for_tests(), 0);
        sdl3::video::vulkan::service_surface_requests();
        assert!(sdl3::video::window_from_id(old_id).is_null());
    }

    /// A context bound on the guest render thread cannot be made current on
    /// the SDL event thread. The owning thread repairs its binding at the
    /// first AppKit GL boundary, with no guest makeCurrentContext call.
    #[test]
    fn gl_popup_rehome_restores_render_thread_before_flush() {
        if !sdl3::isolated::run_here() {
            return;
        }
        crate::bootstrap();
        let rect = NSRect {
            origin: NSPoint { x: 12.0, y: 12.0 },
            size: NSSize {
                width: 160.0,
                height: 100.0,
            },
        };
        let class = objc::class::lookup(c"NSWindow").unwrap();
        let make = || {
            let object = objc::object::create_instance(class, 0);
            unsafe {
                init_with_content_rect(
                    object,
                    objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
                    rect,
                    0,
                    0,
                    0,
                )
            }
        };
        let parent = make();
        let child = make();
        order_front(parent, true);
        unsafe { set_level(child, objc::sel::get("setLevel:"), window_level::POPUP_MENU) };
        order_front(child, false);
        assert!(rebuild_for_opengl(child));
        let old = unsafe { sdl_window(child) };
        let view = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        unsafe {
            crate::view::init_with_frame(view, objc::sel::get("initWithFrame:"), rect);
            set_content_view(child, objc::sel::get("setContentView:"), view);
        }
        let context =
            objc::object::create_instance(objc::class::lookup(c"NSOpenGLContext").unwrap(), 0);
        unsafe {
            let init: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            assert_eq!(
                init(
                    context,
                    objc::sel::get("initWithFormat:shareContext:"),
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                context
            );
            let set_view: unsafe extern "C" fn(Id, Sel, Id) =
                std::mem::transmute(objc::msg::entry());
            set_view(context, objc::sel::get("setView:"), view);
        }
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (continue_tx, continue_rx) = std::sync::mpsc::channel();
        let (raw_done_tx, raw_done_rx) = std::sync::mpsc::channel();
        let (flush_tx, flush_rx) = std::sync::mpsc::channel();
        let context_addr = context as usize;
        let old_addr = old as usize;
        let worker = std::thread::spawn(move || {
            let context = context_addr as Id;
            unsafe {
                let make_current: unsafe extern "C" fn(Id, Sel) =
                    std::mem::transmute(objc::msg::entry());
                make_current(context, objc::sel::get("makeCurrentContext"));
            }
            let native = sdl3::video::gl_current_context();
            assert!(!native.is_null());
            assert_eq!(sdl3::video::gl_current_window() as usize, old_addr);
            ready_tx.send(native as usize).unwrap();
            let replacement = continue_rx.recv().unwrap() as sdl3::Window;
            // No AppKit/CGL call or makeCurrent occurs here. The old popup
            // MUST still be a valid native drawable during raw driver work.
            assert_eq!(sdl3::video::gl_current_window() as usize, old_addr);
            unsafe {
                let color: unsafe extern "C" fn(f32, f32, f32, f32) =
                    std::mem::transmute(sdl3::video::gl_proc_address(c"glClearColor").unwrap());
                let clear: unsafe extern "C" fn(u32) =
                    std::mem::transmute(sdl3::video::gl_proc_address(c"glClear").unwrap());
                let read: unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, *mut u8) =
                    std::mem::transmute(sdl3::video::gl_proc_address(c"glReadPixels").unwrap());
                color(0.125, 0.75, 0.25, 1.0);
                clear(0x4000);
                let mut pixels = [0u8; 4];
                read(8, 8, 1, 1, 0x1908, 0x1401, pixels.as_mut_ptr());
                assert!(
                    pixels[1] > 160 && pixels[0] < 90,
                    "raw GL calls before flush rendered through invalid drawable: {pixels:?}"
                );
            }
            raw_done_tx.send(()).unwrap();
            flush_rx.recv().unwrap();
            // Only now is the guest entering an AppKit boundary. The owner
            // rebinds and acknowledges the native retirement barrier.
            unsafe {
                let flush: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
                flush(context, objc::sel::get("flushBuffer"));
            }
            assert_eq!(sdl3::video::gl_current_context() as usize, native as usize);
            assert_eq!(sdl3::video::gl_current_window(), replacement);
            unsafe {
                let clear: unsafe extern "C" fn(u32) =
                    std::mem::transmute(sdl3::video::gl_proc_address(c"glClear").unwrap());
                clear(0x4000);
                let flush: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
                flush(context, objc::sel::get("flushBuffer"));
            }
            assert_eq!(sdl3::video::gl_current_window(), replacement);
        });
        let native = ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let original_id = sdl3::video::window_id(old);
        assert_ne!(original_id, 0);
        assert!(rebuild_for_opengl(parent));
        let new = unsafe { sdl_window(child) };
        assert_ne!(new, old);
        assert!(
            sdl3::video::gl_current_context().is_null(),
            "SDL video thread incorrectly stole the render thread's GL context"
        );
        assert_eq!(crate::opengl::retired_drawable_count_for_tests(), 1);
        // Servicing SDL retirement BEFORE the render thread acknowledges
        // must leave the old popup's native SDL allocation intact.
        sdl3::video::vulkan::service_surface_requests();
        assert_eq!(sdl3::video::window_from_id(original_id), old);
        continue_tx.send(new as usize).unwrap();
        raw_done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        sdl3::video::vulkan::service_surface_requests();
        assert_eq!(
            sdl3::video::window_from_id(original_id),
            old,
            "old native GL drawable freed while raw rendering was in flight"
        );
        assert_eq!(crate::opengl::retired_drawable_count_for_tests(), 1);
        flush_tx.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(crate::opengl::retired_drawable_count_for_tests(), 0);
        sdl3::video::vulkan::service_surface_requests();
        assert!(
            sdl3::video::window_from_id(original_id).is_null(),
            "old SDL popup did not retire after its GL owner rebound"
        );
        assert_ne!(native, 0);
    }

    /// Under this runtime's own titlebar the SDL window is the frame, and the
    /// content rectangle a window was made with is what its content view
    /// gets - the two conversions agreeing on the difference.
    #[test]
    fn a_titled_window_is_its_content_plus_the_chrome() {
        if !sdl3::isolated::run_here() {
            return;
        }
        crate::bootstrap();
        let object =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        let content = NSRect {
            origin: NSPoint { x: 40.0, y: 60.0 },
            size: NSSize {
                width: 400.0,
                height: 300.0,
            },
        };
        // SAFETY: a fresh window this test made.
        if !unsafe { init_with_content_rect_for_tests(object, content) } {
            return;
        }
        let inset = content_inset(object);
        assert_eq!(
            inset,
            crate::decor::inset_for(crate::decor::config(), true, false, false)
        );
        // SAFETY: a window this test made.
        let sdl = unsafe { sdl_window(object) };
        let (_, height) = sdl3::video::window_size(sdl).expect("a window");
        assert_eq!(f64::from(height), 300.0 + inset);

        // SAFETY: both conversions are `NSRect (id, SEL, NSRect)`.
        let (frame, back) = unsafe {
            let frame =
                frame_rect_for_content(object, objc::sel::get("frameRectForContentRect:"), content);
            let back =
                content_rect_for_frame(object, objc::sel::get("contentRectForFrameRect:"), frame);
            (frame, back)
        };
        assert_eq!(frame.size.height, 300.0 + inset);
        assert_eq!(frame.origin, content.origin, "the frame grows upwards");
        assert_eq!(back, content);

        let view =
            objc::object::create_instance(objc::class::lookup(c"NSView").expect("NSView"), 0);
        // SAFETY: `-setContentView:` is `void (id, SEL, id)`.
        unsafe { set_content_view(object, objc::sel::get("setContentView:"), view) };
        assert_eq!(crate::view::frame_of(view).size.height, 300.0);
    }

    #[test]
    fn a_new_first_responder_may_hand_it_on() {
        crate::bootstrap();
        let window =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        let view = objc::class::lookup(c"NSView").expect("NSView");
        let handler = objc::object::create_instance(view, 0);
        let redirecting = objc::class::allocate_pair(Some(view), c"RedirectingView", 0)
            .expect("a class of this name is not declared elsewhere");
        redirecting.add_method(
            objc::sel::get("becomeFirstResponder"),
            redirecting_become as objc::class::Imp,
            c"c16@0:8",
        );
        objc::class::register_pair(redirecting);
        let engine = objc::object::create_instance(redirecting, 0);
        *REDIRECT.lock().unwrap() = (window as usize, handler as usize);

        // SAFETY: the window and view above, and `-makeFirstResponder:` is
        // `BOOL (id, SEL, id)`.
        let answered = unsafe { make_first_responder(window, objc::sel::get("x"), engine) };
        assert_eq!(answered, 1);
        assert_eq!(window_of(window).first_responder, handler);
    }

    unsafe extern "C" fn accepts(_receiver: Id, _sel: Sel) -> u8 {
        1
    }

    #[test]
    fn a_window_ordered_in_makes_a_content_view_that_accepts_it_first_responder() {
        crate::bootstrap();
        let view = objc::class::lookup(c"NSView").expect("NSView");
        let accepting = objc::class::allocate_pair(Some(view), c"AcceptingContentView", 0)
            .expect("a class of this name is not declared elsewhere");
        accepting.add_method(
            objc::sel::get("acceptsFirstResponder"),
            accepts as objc::class::Imp,
            c"c16@0:8",
        );
        objc::class::register_pair(accepting);
        let window_class = objc::class::lookup(c"NSWindow").expect("NSWindow");
        for (class, takes_it) in [(view, false), (accepting, true)] {
            let window = objc::object::create_instance(window_class, 0);
            let content = objc::object::create_instance(class, 0);
            // SAFETY: `-setContentView:` is `void (id, SEL, id)`.
            unsafe { set_content_view(window, objc::sel::get("setContentView:"), content) };
            set_up_first_responder(window);
            let expected = if takes_it {
                content
            } else {
                std::ptr::null_mut()
            };
            assert_eq!(window_of(window).first_responder, expected);
        }
    }

    #[test]
    fn a_freed_first_responder_is_forgotten() {
        crate::bootstrap();
        let window =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        let view =
            objc::object::create_instance(objc::class::lookup(c"NSView").expect("NSView"), 0);
        objc::arc::store_weak(&mut window_of(window).first_responder, view);
        objc::arc::release(view);
        assert!(window_of(window).first_responder.is_null());
        objc::arc::destroy_weak(&mut window_of(window).first_responder);
    }
}
