use foundation::geometry::NSPoint;
use objc::object::Id;

use crate::nsevent::{self, Fields, kind};
use crate::window;

/// Drain SDL's queue once, delivering what this tree can deliver.
///
/// `app` is the application, which is the object a window notification is posted
/// about when the event names no window this crate made.
pub fn pump(app: Id) {
    let _ = app;
    // Before anything else: a thread waiting for a GL context this one holds is
    // a thread that is not drawing, and reaching here is what says this one has
    // finished with it. See `opengl::release_for_other_threads`.
    crate::opengl::release_for_other_threads();
    gamecontroller::pump();
    debug_resize();
    debug_key();
    crate::script::run();
    trace_views();
    while let Some(event) = sdl3::video::poll() {
        deliver(event);
    }
    // Copied out first: a guard in the loop's head would be held across every
    // hook, and a hook that runs guest code can turn this pump again.
    let hooks = PUMP_HOOKS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for hook in hooks {
        hook();
    }
    crate::application::check_last_window_closed();
    crate::backing::pump();
}

static PUMP_HOOKS: std::sync::Mutex<Vec<fn()>> = std::sync::Mutex::new(Vec::new());

/// Run `hook` on every turn of the main thread's event pump, after input is
/// delivered and before stores are presented.
///
/// For another toolkit whose own main loop has to turn on this thread: WebKit's
/// GTK main context is serviced here, so its callbacks reach the guest on the
/// thread AppKit promises them on.
pub fn add_pump_hook(hook: fn()) {
    let mut hooks = PUMP_HOOKS.lock().unwrap_or_else(|e| e.into_inner());
    if !hooks
        .iter()
        .any(|existing| std::ptr::fn_addr_eq(*existing, hook))
    {
        hooks.push(hook);
    }
}

/// `ROSETTA3_DEBUG_RESIZE=WxH@SECONDS`: resize the program's window once, that
/// many seconds in.
///
/// A resize is the one window event that cannot be produced without a person
/// dragging a corner, which makes "does the program follow its window" the one
/// thing about a window that is untestable from a script. `SDL_SetWindowSize`
/// on a resizable window raises the same `SDL_EVENT_WINDOW_RESIZED` the
/// compositor would, so everything downstream of the pump - the content view's
/// frame, the subviews, the delegate's `-windowDidResize:` - is exercised for
/// real.
fn debug_resize() {
    use std::sync::OnceLock;
    use std::time::Instant;

    static REQUEST: OnceLock<Option<(i32, i32, f64)>> = OnceLock::new();
    static START: OnceLock<Instant> = OnceLock::new();
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    let request = *REQUEST.get_or_init(|| {
        let value = std::env::var("ROSETTA3_DEBUG_RESIZE").ok()?;
        let (size, after) = value.split_once('@').unwrap_or((value.as_str(), "5"));
        let (width, height) = size.split_once('x')?;
        Some((
            width.trim().parse().ok()?,
            height.trim().parse().ok()?,
            after.trim().parse().unwrap_or(5.0),
        ))
    });
    let Some((width, height, after)) = request else {
        return;
    };
    if DONE.load(std::sync::atomic::Ordering::Relaxed)
        || START.get_or_init(Instant::now).elapsed().as_secs_f64() < after
    {
        return;
    }
    DONE.store(true, std::sync::atomic::Ordering::Relaxed);
    let Some(sdl) = window::sdl_windows().into_iter().next() else {
        return;
    };
    eprintln!("rosetta3: appkit: ROSETTA3_DEBUG_RESIZE: resizing to {width}x{height}");
    sdl3::video::set_window_size(sdl, width, height);
}

/// `ROSETTA3_DEBUG_KEY=NAME[,NAME...]@SECONDS`: press and release each key in
/// turn, that many seconds in.
///
/// The companion to `ROSETTA3_DEBUG_RESIZE`, and there for the same reason: a
/// keystroke cannot be produced from a script under a compositor that does not
/// implement the virtual-keyboard protocol, so "does the program answer the
/// keyboard" is otherwise only testable by hand. The event goes through
/// [`deliver`], so the keymap, `-keyDown:` and the responder chain are exercised
/// exactly as a real press would exercise them.
fn debug_key() {
    use std::sync::OnceLock;
    use std::time::Instant;

    static REQUEST: OnceLock<Option<(Vec<u32>, f64)>> = OnceLock::new();
    static START: OnceLock<Instant> = OnceLock::new();
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    let request = REQUEST.get_or_init(|| {
        let value = std::env::var("ROSETTA3_DEBUG_KEY").ok()?;
        let (names, after) = value.split_once('@').unwrap_or((value.as_str(), "5"));
        let keys: Vec<u32> = names
            .split(',')
            .filter_map(|n| scancode(n.trim()))
            .collect();
        match keys.is_empty() {
            true => None,
            false => Some((keys, after.trim().parse().unwrap_or(5.0))),
        }
    });
    let Some((keys, after)) = request else {
        return;
    };
    let after = *after;
    if DONE.load(std::sync::atomic::Ordering::Relaxed)
        || START.get_or_init(Instant::now).elapsed().as_secs_f64() < after
    {
        return;
    }
    DONE.store(true, std::sync::atomic::Ordering::Relaxed);
    let Some(window) = window::sdl_windows()
        .into_iter()
        .map(sdl3::video::window_id)
        .next()
    else {
        return;
    };
    eprintln!("rosetta3: appkit: ROSETTA3_DEBUG_KEY: pressing scancodes {keys:?}");
    for scancode in keys {
        for down in [true, false] {
            deliver(sdl3::video::Event::Key {
                window,
                scancode: *scancode,
                key: 0,
                modifiers: 0,
                down,
                repeat: false,
            });
        }
    }
}

/// The USB HID usage id `ROSETTA3_DEBUG_KEY`'s value names, or a raw number.
pub(crate) fn scancode(name: &str) -> Option<u32> {
    use sdl3::keycodes::scancode;
    Some(match name.to_ascii_lowercase().as_str() {
        "return" | "enter" => scancode::RETURN,
        "escape" => scancode::ESCAPE,
        "tab" => scancode::TAB,
        "space" => scancode::SPACE,
        "right" => scancode::RIGHT,
        "left" => scancode::LEFT,
        "down" => scancode::DOWN,
        "up" => scancode::UP,
        other => other.parse().ok()?,
    })
}

/// `ROSETTA3_TRACE_VIEWS=SECONDS`: print each window and the tree of views under
/// it, once, that many seconds in.
///
/// Companion to `ROSETTA3_DEBUG_RESIZE`, and needed for the same reason: the
/// question a resize raises is which view ended up the wrong size, and the view
/// that matters is one the *guest* made - so it is in no debugger's symbol
/// table, and a release build has inlined every accessor that would name it.
/// Printed after the window's own size so the two can be compared on sight.
fn trace_views() {
    use std::sync::OnceLock;
    use std::time::Instant;

    static AFTER: OnceLock<Option<f64>> = OnceLock::new();
    static START: OnceLock<Instant> = OnceLock::new();
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    let Some(after) = *AFTER.get_or_init(|| {
        std::env::var("ROSETTA3_TRACE_VIEWS")
            .ok()
            .map(|value| value.trim().parse().unwrap_or(5.0))
    }) else {
        return;
    };
    if DONE.load(std::sync::atomic::Ordering::Relaxed)
        || START.get_or_init(Instant::now).elapsed().as_secs_f64() < after
    {
        return;
    }
    DONE.store(true, std::sync::atomic::Ordering::Relaxed);

    let mut out = String::new();
    for object in window::all() {
        // SAFETY: a window this crate made.
        let sdl = unsafe { window::sdl_window(object) };
        let size = sdl3::video::window_size(sdl).unwrap_or((0, 0));
        out.push_str(&format!(
            "rosetta3: appkit: window {object:p} sdl {}x{}\n",
            size.0, size.1
        ));
        // SAFETY: null or a view this runtime allocated.
        unsafe { crate::view::describe(window::content_view_of(object), 1, &mut out) };
    }
    eprint!("{out}");
}

pub(crate) fn deliver(event: sdl3::video::Event) {
    if !crate::dialog::deliver_modal(&event) {
        deliver_past_modal(event);
    }
}

/// [`deliver`] for the modal dialog's own loop, which has already had its
/// turn at `event`: queueing it for the dialog again would hand it straight
/// back, and the loop would never wait for input again.
pub(crate) fn deliver_past_modal(event: sdl3::video::Event) {
    use sdl3::video::Event;
    if crate::dialog::deliver(&event) {
        return;
    }
    match event {
        // A quit is the whole application, and is the one event that is not
        // about a window: it is what a program's `-applicationShouldTerminate:`
        // would be asked about, and until there is one it ends the run loop the
        // way `-terminate:` does.
        Event::Quit => {
            let app = crate::application::shared();
            if !app.is_null() {
                // SAFETY: `-terminate:` is `void (id, SEL, id)` on the shared
                // application, which this crate installed.
                unsafe {
                    let f: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
                        std::mem::transmute(objc::msg::entry());
                    f(app, objc::sel::get("terminate:"), std::ptr::null_mut());
                }
            }
        }
        // The compositor's close button, or its shortcut, which is what the
        // title bar's close button is on a Mac: `-performClose:`, so the
        // delegate may refuse and `-close` does the rest.
        Event::WindowClosed { window } => {
            if crate::decor::window_closed(window) {
                return;
            }
            let object = object_for(window);
            if !object.is_null() {
                // SAFETY: `-performClose:` is `void (id, SEL, id)` on a window
                // this crate made.
                unsafe {
                    let f: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
                        std::mem::transmute(objc::msg::entry());
                    f(object, objc::sel::get("performClose:"), object);
                }
            }
        }
        Event::WindowHitTest { window } => crate::decor::hit_test_pressed(window),
        Event::WindowState { window } => {
            let object = object_for(window);
            if !object.is_null()
                && let Some(name) = window::window_state_changed(object)
            {
                post(window, name);
            }
        }
        // A program that keeps its own copy of where its window is - a Win32
        // layer mapping client rectangles to the desktop does - learns of a move
        // from this alone. On Wayland SDL first reports the position it was
        // asked for and corrects it once the compositor has placed the window;
        // a missed correction leaves that copy out by the difference, and every
        // pointer position the program checks against it with it.
        Event::WindowMoved { window } => post(window, window::DID_MOVE),
        Event::WindowResized { window, .. } => {
            // The content view follows the window, as it does on
            // `-setContentView:` and for the same reason: a view that kept its
            // old size after a resize would draw into part of the window and
            // report a backing store the window does not have.
            let object = object_for(window);
            let content = window::content_view_of(object);
            if !object.is_null() {
                window::fit_content_view(object);
            }
            // A resized view's tracking rectangle is the wrong shape, and
            // AppKit asks for a new one here rather than resizing the old.
            // SAFETY: null or a view this crate made.
            unsafe { crate::view::update_tracking_areas(content) };
            // Cursor rectangles are in their view's coordinates too. A
            // reset lets a view rebuild rectangles derived from its old bounds
            // before the next cursor hit test chooses the wrong cursor.
            // SAFETY: null or the content view of a window this crate made.
            unsafe { crate::cursor::reset_rects_in_tree(content) };
            post(window, window::DID_RESIZE);
        }
        // The frame remains in points when a window crosses onto a display at
        // another scale, but its backing store does not. AppKit tells guests to
        // recompute pixel-sized viewports with this notification.
        Event::WindowBackingChanged { window } => {
            post(window, window::DID_CHANGE_BACKING_PROPERTIES);
        }
        Event::WindowMouse { window, entered } => {
            if !entered {
                crate::decor::pointer_left(window);
            }
            // A menu's popup is chrome from its first pixel; anything else
            // is decided by the moves that follow.
            if entered {
                crate::cursor::set_over_chrome(crate::decor::is_chrome_popup(window));
            }
            // A resize is performed from outside the client area. SDL reports
            // the resulting leave and re-entry as window events rather than
            // mouse-motion events; dropping them leaves an NSTrackingArea
            // owner believing the pointer is outside forever.
            if entered && let Some((x, y, _)) = sdl3::video::mouse_state() {
                track_pointer(window, x, y);
            }
            crate::cursor::apply();
            match entered {
                true => crate::cursor::update_tracking(object_for(window), false),
                false => crate::cursor::forget_tracking(object_for(window)),
            }
        }
        Event::WindowFocus { window, gained } => {
            crate::decor::focus_changed(window, gained);
            let object = object_for(window);
            if !object.is_null() && gained {
                window::set_key_window(object);
                // SDL drops relative mouse mode when focus goes, and a window
                // made while the guest was disassociated never entered it.
                crate::screen::apply_mouse_association();
                // SAFETY: a window this crate made.
                let sdl = unsafe { window::sdl_window(object) };
                if !sdl.is_null() {
                    crate::screen::confine_pointer(sdl);
                }
            }
            post(
                window,
                match gained {
                    true => window::DID_BECOME_KEY,
                    false => window::DID_RESIGN_KEY,
                },
            );
        }
        Event::Key {
            window: id,
            scancode,
            key,
            modifiers,
            down,
            repeat,
        } => {
            if crate::decor::key(scancode, down) {
                return;
            }
            let text = crate::keymap::characters(
                scancode,
                crate::keymap::typed_key(scancode, key, modifiers),
            );
            let modifiers = crate::keymap::modifiers(modifiers);
            let code = crate::keymap::virtual_key(scancode);
            if down && !crate::keymap::is_modifier(scancode) {
                let window = object_for(id);
                crate::text::begin_key_interpretation(responder_of(window));
            }
            let fields = |window| Fields {
                kind: match down {
                    true => kind::KEY_DOWN,
                    false => kind::KEY_UP,
                },
                modifiers,
                key_code: code,
                characters: objc::arc::autorelease(foundation::string::from_str(&text)),
                repeat,
                window,
                ..Fields::default()
            };
            // A key equivalent is the menu's, offered before the key reaches
            // the first responder, as `-[NSApplication sendEvent:]` does.
            if down
                && !crate::keymap::is_modifier(scancode)
                && key_equivalent(object_for(id), fields(object_for(id)), &text)
            {
                return;
            }
            // A modifier is `-flagsChanged:`, not a key down: AppKit has no
            // key-up for one, and a guest that treated it as a key would see
            // shift as a character.
            match crate::keymap::is_modifier(scancode) {
                true => send(id, None, |w, _| Fields {
                    kind: kind::FLAGS_CHANGED,
                    ..fields(w)
                }),
                false => send(id, None, |w, _| fields(w)),
            }
        }
        // A key event describes a physical key. Text input is the separate,
        // layout- and IME-resolved commit, and must go to the focused
        // `NSTextInputClient`; `-characters` is not a substitute for it.
        Event::Text { window, text } => {
            let window = object_for(window);
            let responder = responder_of(window);
            if crate::text::accepts_host_text(responder) {
                crate::text::commit(responder, &text);
            }
        }
        Event::TextEditing {
            window,
            text,
            start,
            length,
        } => {
            let window = object_for(window);
            let responder = responder_of(window);
            if crate::text::accepts_host_text(responder) {
                crate::text::compose(responder, &text, start, length);
            }
        }
        Event::MouseMotion {
            window: id,
            x,
            y,
            dx,
            dy,
            buttons,
        } => {
            if take_warp_echo(x, y) {
                return;
            }
            let on_chrome = crate::decor::pointer_moved(id, x, y);
            crate::cursor::set_over_chrome(on_chrome);
            if on_chrome {
                return;
            }
            // A window being dragged by `-performWindowDragWithEvent:` consumes
            // the move: it moved the window, and AppKit does not also report it
            // as a drag inside the window it just moved.
            if crate::window::drag_moved() {
                return;
            }
            let held = crate::button::held(buttons);
            // A disassociated pointer reports the deltas and none of the
            // movement: the location a guest reads back is where the cursor was
            // when it broke the link, which is what CoreGraphics does and what
            // SDL, still tracking a position inside the window, does not.
            let (x, y) = track_pointer(id, x, y);
            // Before the drop below: a cursor rectangle applies whether or not
            // anything asked for `-mouseMoved:`, and AppKit re-decides the
            // pointer on every move rather than only when a rectangle changes.
            crate::cursor::apply();
            crate::cursor::update_tracking(object_for(id), held.is_some());
            let fields = |window, location, area| Fields {
                kind: crate::button::dragged_kind(held),
                modifiers: current_modifiers(),
                location,
                delta: NSPoint {
                    x: dx as f64,
                    y: dy as f64,
                },
                button: held.map_or(0, crate::button::number),
                window,
                tracking_area: area,
                ..Fields::default()
            };
            if held.is_none() {
                // An area made with `NSTrackingMouseMoved` has its owner sent
                // the move, and that is the whole of the delivery: the view
                // under the pointer is not sent it a second time.
                let window = object_for(id);
                let areas = crate::cursor::mouse_moved_areas(window);
                if !areas.is_empty() {
                    let location = location(window, x, y);
                    for area in &areas {
                        dispatch(window, nsevent::make(fields(window, location, *area)));
                    }
                    areas.into_iter().for_each(objc::arc::release);
                    return;
                }
            }
            if held.is_none() && !wants_mouse_moved(object_for(id)) {
                if tracing() {
                    eprintln!(
                        "rosetta3: appkit: -mouseMoved: dropped: the window did not ask for it \
                         and no view is tracking"
                    );
                }
                return;
            }
            send(id, Some((x, y)), |window, location| {
                fields(window, location, std::ptr::null_mut())
            });
        }
        Event::MouseButton {
            window: id,
            button,
            down,
            clicks,
            x,
            y,
        } => {
            if crate::decor::pointer_button(id, button, down, clicks, x, y) {
                return;
            }
            let kind = crate::button::event_kind(button, down);
            // The release ends a window drag. The event is still delivered: a
            // guest that started the drag from its own `-mouseDown:` is holding
            // a button as far as it knows, and swallowing the up would leave it
            // there.
            if !down {
                crate::window::end_drag();
            }
            let (x, y) = track_pointer(id, x, y);
            send(id, Some((x, y)), |window, location| Fields {
                kind,
                modifiers: current_modifiers(),
                location,
                button: crate::button::number(button),
                clicks: i64::from(clicks),
                window,
                ..Fields::default()
            });
        }
        // A wheel event carries no position, so it goes to the view the pointer
        // was last over - which is where the wheel turned.
        Event::MouseWheel { window: id, x, y } => {
            let at = *POINTER.lock().unwrap_or_else(|e| e.into_inner());
            send(id, Some(at), |window, location| Fields {
                kind: kind::SCROLL_WHEEL,
                modifiers: current_modifiers(),
                location,
                delta: NSPoint {
                    x: x as f64,
                    y: y as f64,
                },
                window,
                ..Fields::default()
            });
        }
        Event::Other(_) => {}
    }
}

/// Tell a window's guest where the window actually is and how large it actually
/// is, by posting the pair of notifications a real move and resize would.
///
/// On a Mac a window is placed and sized exactly as the program asked, so the
/// program never has to look again. A Wayland compositor places a toplevel
/// itself and may hand back a different size, and a toolkit that never hears
/// about it keeps mapping pointer positions against a window that is elsewhere.
/// The same pair is what a toolkit waits for before it treats a window as
/// on-screen at all.
pub fn window_geometry_changed(object: Id) {
    // SAFETY: a window this crate made.
    let sdl = unsafe { window::sdl_window(object) };
    if sdl.is_null() {
        return;
    }
    let id = sdl3::video::window_id(sdl);
    // Only when the compositor actually chose a different size. `-windowDidResize:`
    // is not a notification a program merely notes: Unreal rebuilds its viewport
    // from it, which means dropping the GL context on the thread that draws and
    // taking it on the thread that runs this - and a context handed back and
    // forth is the one thing the GL under this runtime cannot do. A window that
    // came back the size it asked for has nothing to say.
    if let Some((width, height)) = sdl3::video::window_size(sdl)
        && window::requested_size(object) != Some((width, height))
    {
        deliver(sdl3::video::Event::WindowResized {
            window: id,
            width,
            height,
        });
    }
    post(id, window::DID_MOVE);
}

fn current_modifiers() -> u64 {
    crate::keymap::modifiers(sdl3::video::keyboard_modifiers())
}

/// Offer a key press to whatever takes it as a key equivalent: the window's
/// views, then the main menu. True when something did.
///
/// With [`crate::decor::Config::control_shortcuts`], Control also reaches a
/// menu item whose equivalent is Command, once nothing took it as it was.
fn key_equivalent(window: Id, fields: Fields, text: &str) -> bool {
    use crate::keymap::flags;
    let modifiers = fields.modifiers;
    let function_key = text
        .chars()
        .next()
        .is_some_and(|c| ('\u{F700}'..='\u{F8FF}').contains(&c));
    if modifiers & (flags::COMMAND | flags::CONTROL) == 0 && !function_key {
        return false;
    }
    let sel = objc::sel::get("performKeyEquivalent:");
    let offer = |receiver: Id, event: Id| {
        if receiver.is_null() {
            return false;
        }
        // SAFETY: `-performKeyEquivalent:` is `BOOL (id, SEL, NSEvent *)` on
        // windows and menus, both of which this crate installs it on.
        unsafe {
            let f: unsafe extern "C" fn(Id, objc::sel::Sel, Id) -> u8 =
                std::mem::transmute(objc::msg::entry());
            f(receiver, sel, event) != 0
        }
    };
    let menu = crate::menu::main_menu();
    let event = nsevent::make(fields.clone());
    if offer(window, event) || offer(menu, event) {
        return true;
    }
    let control_only = modifiers & flags::CONTROL != 0 && modifiers & flags::COMMAND == 0;
    if !(control_only && crate::decor::config().control_shortcuts) {
        return false;
    }
    let aliased = nsevent::make(Fields {
        modifiers: (modifiers & !flags::CONTROL) | flags::COMMAND,
        ..fields
    });
    offer(menu, aliased)
}

/// A pointer position SDL reports against the window's top-left, in the
/// bottom-left frame `-locationInWindow` promises.
///
/// The base coordinate system is the whole frame, so a titlebar this runtime
/// draws is above the content view's top.
fn location(window: Id, x: f32, y: f32) -> NSPoint {
    let content = window::content_view_of(window);
    let height = match content.is_null() {
        true => 0.0,
        // SAFETY: `-bounds` is `NSRect (id, SEL)` on a view this crate made.
        // `NSRect` returns through a buffer, so this is the `_stret` entry:
        // through the other one the receiver would be the return slot.
        false => unsafe {
            let f: unsafe extern "C" fn(Id, objc::sel::Sel) -> foundation::geometry::NSRect =
                std::mem::transmute(objc::msg::entry_stret());
            f(content, objc::sel::get("bounds")).size.height
        },
    };
    let height = height + window::content_inset(window);
    crate::geometry::point_in_window(x as f64, y as f64, height)
}

/// Hand one input event to its window, as `-[NSApplication sendEvent:]` does.
fn send(id: u32, at: Option<(f32, f32)>, fields: impl FnOnce(Id, NSPoint) -> Fields) {
    let window = object_for(id);
    if window.is_null() {
        return;
    }
    let point = at.map(|(x, y)| location(window, x, y)).unwrap_or_default();
    dispatch(window, nsevent::make(fields(window, point)));
}

/// Hand a built event to its window's `-sendEvent:`, after the local monitors.
pub(crate) fn dispatch(window: Id, event: Id) {
    // Before the window sees it, which is where AppKit runs a local monitor: a
    // monitor exists to swallow an event, and one that ran afterwards could
    // only take it back.
    if !crate::monitor::allows(event) {
        if tracing() {
            eprintln!("rosetta3: appkit: a local event monitor swallowed the event");
        }
        return;
    }
    let sel = objc::sel::get("sendEvent:");
    // SAFETY: `-sendEvent:` is `void (id, SEL, NSEvent *)`, and every window
    // has one - this crate installs the default on `NSWindow`.
    unsafe {
        let f: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            std::mem::transmute(objc::msg::entry());
        f(window, sel, event);
    }
}

/// `-[NSWindow sendEvent:]`: route one event to the responder it belongs to.
///
/// The default AppKit routing, and the two halves of it are not the same rule:
/// an event with a position goes to the view under it, and a key event goes to
/// the first responder. Either way the search walks up the responder chain from
/// there, and an event nothing implements is dropped.
/// `ROSETTA3_EVENT_LOG=FILE`: append a line per input event and scripted
/// action to a file.
///
/// Not stderr: a guest which redirects its own - Studio replaces it with its
/// log file once it is running - takes every later trace line with it, and
/// input arrives long after that.
pub const LOG_ENV: &str = "ROSETTA3_EVENT_LOG";

fn log_path() -> Option<&'static std::path::Path> {
    static PATH: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    PATH.get_or_init(|| std::env::var_os(LOG_ENV).map(std::path::PathBuf::from))
        .as_deref()
}

pub fn debug_log(line: &str) {
    use std::io::Write;
    let Some(path) = log_path() else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// What [`route`] is about to do with an event, for [`LOG_ENV`]: where it is,
/// which tracking area it came from, and the view under it.
fn log_route(window: Id, event: Id, kind: u64) {
    if log_path().is_none() {
        return;
    }
    let at = nsevent::location_of(event);
    // SAFETY: null or the content view, which this crate made.
    let hit = unsafe { crate::view::hit_test(window::content_view_of(window), at) };
    // SAFETY: null or a live object this runtime allocated.
    let class = unsafe { objc::object::class_of(hit) }.map_or("-", objc::class::Class::name_str);
    let rect = crate::view::rect_in_window(hit);
    let local = crate::view::point_from_window(hit, at);
    debug_log(&format!(
        "route kind {kind} window {window:p} at ({:.1},{:.1}) area {:p} hit {hit:p} {class} \
         in window ({:.1},{:.1} {:.0}x{:.0}) local ({:.1},{:.1})",
        at.x,
        at.y,
        nsevent::tracking_area_of(event),
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
        local.x,
        local.y,
    ));
}

pub fn route(window: Id, event: Id) {
    let kind = nsevent::kind_of(event);
    log_route(window, event, kind);
    let Some(selector) = responder_selector(kind) else {
        return;
    };
    let sel = objc::sel::get(selector);
    // A tracking area's events are its owner's, which need not be a view and
    // is not reached by walking up from one.
    let area = nsevent::tracking_area_of(event);
    if !area.is_null() {
        let owner = crate::cursor::tracking_owner_of(area);
        // SAFETY: null or a live object: an owner removes its areas before
        // it goes, as AppKit requires.
        let method = unsafe { objc::object::class_of(owner) }.and_then(|c| c.method(sel));
        if tracing() {
            eprintln!(
                "rosetta3: appkit: -{selector} to tracking area owner {owner:p}{}",
                match method.is_some() {
                    true => "",
                    false => " - NOT implemented",
                }
            );
        }
        if let Some(method) = method {
            // SAFETY: every tracking method is `void (id, SEL, NSEvent *)`.
            unsafe {
                let f: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
                    std::mem::transmute(method.implementation());
                f(owner, sel, event);
            }
        }
        return;
    }
    let start = match positional(kind) {
        // SAFETY: null or the content view, which this crate made.
        true => unsafe {
            crate::view::hit_test(window::content_view_of(window), nsevent::location_of(event))
        },
        false => responder_of(window),
    };
    let Some((responder, method)) = chain(start, sel) else {
        if tracing() {
            eprintln!("rosetta3: appkit: -{selector}: nothing from {start:p} up implements it");
        }
        return;
    };
    if tracing() {
        // SAFETY: a live object this runtime allocated.
        let class = unsafe { objc::object::class_of(responder) };
        eprintln!(
            "rosetta3: appkit: -{selector} to {responder:p} ({})",
            class.map_or("no class", |c| c.name_str()),
        );
    }
    // SAFETY: every responder method here is `void (id, SEL, NSEvent *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            std::mem::transmute(method.implementation());
        f(responder, sel, event);
    }
}

/// The responder method an event type is delivered through.
fn responder_selector(kind: u64) -> Option<&'static str> {
    Some(match kind {
        kind::LEFT_MOUSE_DOWN => "mouseDown:",
        kind::LEFT_MOUSE_UP => "mouseUp:",
        kind::RIGHT_MOUSE_DOWN => "rightMouseDown:",
        kind::RIGHT_MOUSE_UP => "rightMouseUp:",
        kind::OTHER_MOUSE_DOWN => "otherMouseDown:",
        kind::OTHER_MOUSE_UP => "otherMouseUp:",
        kind::MOUSE_MOVED => "mouseMoved:",
        kind::LEFT_MOUSE_DRAGGED => "mouseDragged:",
        kind::RIGHT_MOUSE_DRAGGED => "rightMouseDragged:",
        kind::OTHER_MOUSE_DRAGGED => "otherMouseDragged:",
        kind::MOUSE_ENTERED => "mouseEntered:",
        kind::MOUSE_EXITED => "mouseExited:",
        kind::CURSOR_UPDATE => "cursorUpdate:",
        kind::SCROLL_WHEEL => "scrollWheel:",
        kind::KEY_DOWN => "keyDown:",
        kind::KEY_UP => "keyUp:",
        kind::FLAGS_CHANGED => "flagsChanged:",
        _ => return None,
    })
}

/// Whether an event is about a place rather than about the keyboard.
fn positional(kind: u64) -> bool {
    !matches!(kind, kind::KEY_DOWN | kind::KEY_UP | kind::FLAGS_CHANGED)
}

/// Walk the responder chain from `start` for something that implements `sel`.
///
/// The chain is the superview chain and then the window, which is AppKit's for
/// a view in a window. Bounded, because a subview list a guest built with a
/// cycle in it would otherwise hang inside event delivery.
fn chain(start: Id, sel: objc::sel::Sel) -> Option<(Id, &'static objc::class::Method)> {
    let mut responder = start;
    let mut fallback = None;
    for _ in 0..64 {
        if responder.is_null() {
            return fallback;
        }
        // SAFETY: null or a live object this runtime allocated.
        if let Some(method) =
            unsafe { objc::object::class_of(responder) }.and_then(|c| c.method(sel))
        {
            let ours = crate::text::is_default_key_handling(method.implementation() as *const ());
            match ours {
                false => return Some((responder, method)),
                true => fallback = fallback.or(Some((responder, method))),
            }
        }
        let next = unsafe { crate::view::superview_of(responder) };
        responder = match next.is_null() {
            true => unsafe { crate::view::window_of(responder) },
            false => next,
        };
    }
    fallback
}

/// Where the pointer was last seen, in SDL's window coordinates.
///
/// A wheel event carries no position and a click carries one only where SDL
/// filled it in, so the last motion is what says which view the event is about.
static POINTER: std::sync::Mutex<(f32, f32)> = std::sync::Mutex::new((0.0, 0.0));

/// Record a programmatic cursor warp in window coordinates.
///
/// SDL does not deliver a motion event for an in-window warp while relative
/// mode is active. Without this explicit update, the cursor moves to an app's
/// lock point but every `NSEvent` and `+[NSEvent mouseLocation]` continues to
/// report the position from before the lock.
pub(crate) fn warp_pointer(x: f32, y: f32) {
    *POINTER.lock().unwrap_or_else(|e| e.into_inner()) = (x, y);
}

/// Where a physical warp is expected to be echoed back as a motion event.
///
/// CoreGraphics posts no event for a warp, but SDL does (Cocoa sends one
/// synchronously, X11 reports the server's MotionNotify), with a delta of
/// `target - previous`. A game that recentres the cursor after every
/// `-mouseMoved:` would read that delta as the player undoing their own motion.
static WARP_ECHO: std::sync::Mutex<Option<(f32, f32)>> = std::sync::Mutex::new(None);

/// Swallow the next motion event that lands on `to`, in window coordinates.
/// Nothing is armed when `from` is already there: SDL drops a warp that moves
/// nothing, and a stale echo would eat the next real motion to reach `to`.
pub(crate) fn expect_warp_echo(from: (f32, f32), to: (f32, f32)) {
    let echo = (!lands_on(from, to)).then_some(to);
    *WARP_ECHO.lock().unwrap_or_else(|e| e.into_inner()) = echo;
}

fn take_warp_echo(x: f32, y: f32) -> bool {
    let mut echo = WARP_ECHO.lock().unwrap_or_else(|e| e.into_inner());
    let hit = echo.is_some_and(|target| lands_on(target, (x, y)));
    if hit {
        *echo = None;
    }
    hit
}

/// Within half a pixel: SDL converts between desktop and window coordinates.
fn lands_on(target: (f32, f32), point: (f32, f32)) -> bool {
    (target.0 - point.0).abs() < 0.5 && (target.1 - point.1).abs() < 0.5
}

/// The last cursor position an event or programmatic warp established.
pub(crate) fn pointer_position() -> (f32, f32) {
    *POINTER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Record where an event says the pointer is, and answer where a guest is to be
/// told it is.
///
/// The two differ only while the pointer is disassociated, and then they differ
/// for every event and not only for motion: CoreGraphics leaves the cursor
/// exactly where it was, so a click and a wheel turn arrive at that point too.
/// SDL keeps tracking a position inside the window through relative mode, and a
/// click that took SDL's answer would move the frozen point to somewhere the
/// cursor never was - which is a hit test, and so a first responder, against the
/// wrong view.
fn track_pointer(window: u32, x: f32, y: f32) -> (f32, f32) {
    POINTER_WINDOW.store(window, std::sync::atomic::Ordering::Relaxed);
    let mut pointer = POINTER.lock().unwrap_or_else(|e| e.into_inner());
    if crate::screen::mouse_is_associated() {
        *pointer = (x, y);
    }
    *pointer
}

/// Where the guest is to be told the pointer is on the desktop, for the calls
/// that ask the window system rather than reading an event.
pub(crate) fn frozen_pointer() -> Option<(f32, f32)> {
    if crate::screen::mouse_is_associated() {
        return None;
    }
    let (x, y) = *POINTER.lock().unwrap_or_else(|e| e.into_inner());
    let origin = pointer_window()
        .and_then(sdl3::video::window_position)
        .unwrap_or((0, 0));
    Some((x + origin.0 as f32, y + origin.1 as f32))
}

/// The SDL id of the window the last pointer event came from; `0` before any.
static POINTER_WINDOW: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The window [`POINTER`] is relative to: the one the pointer was last seen
/// in, not the first one made. A game with a splash window still open would
/// otherwise translate every desktop point against the splash.
pub(crate) fn pointer_window() -> Option<sdl3::Window> {
    let windows = window::sdl_windows();
    let id = POINTER_WINDOW.load(std::sync::atomic::Ordering::Relaxed);
    windows
        .iter()
        .copied()
        .find(|w| id != 0 && sdl3::video::window_id(*w) == id)
        .or_else(|| windows.first().copied())
}

/// Whether `-mouseMoved:` should be delivered at all.
fn wants_mouse_moved(window: Id) -> bool {
    // SAFETY: null or the content view, which this crate made.
    window::accepts_mouse_moved_events(window)
        || unsafe { crate::view::any_tracking_area(window::content_view_of(window)) }
}

/// The object an input event goes to: the window's first responder, or its
/// content view when nothing has been made one.
///
/// `-firstResponder` answers with the window itself at the end of its own
/// chain, and a window is not what draws - so that answer means "the view".
fn responder_of(window: Id) -> Id {
    let sel = objc::sel::get("firstResponder");
    // SAFETY: `-firstResponder` is `id (id, SEL)` on a window this crate made.
    let responder = unsafe {
        let f: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(window, sel)
    };
    match responder == window || responder.is_null() {
        true => window::content_view_of(window),
        false => responder,
    }
}

/// The `NSWindow` an SDL window id names.
///
/// SDL identifies a window by a small integer in an event and by a pointer
/// everywhere else, and `crates/sdl3` does not expose the mapping - so this
/// walks the windows this crate made and asks each one. There are never more
/// than a handful.
pub(crate) fn object_for(id: u32) -> Id {
    for object in window::all() {
        // SAFETY: a window this crate made.
        let sdl = unsafe { window::sdl_window(object) };
        if !sdl.is_null() && sdl3::video::window_id(sdl) == id {
            return object;
        }
    }
    std::ptr::null_mut()
}

/// Post a window notification, to the centre and to the window's delegate.
///
/// Both, for the reason `-finishLaunching` sends the application's delegate
/// method directly as well: AppKit registers a window's delegate for the
/// notifications it declares the moment it is set, so a program that only
/// implements `-windowDidResize:` never registers as an observer of anything.
/// Set to print every window notification, and whether anything took it.
///
/// A window event that reaches nobody is invisible from both ends - the program
/// simply does not react - and a release build inlines every function on this
/// path, so a breakpoint cannot answer it either. This is the answer: the name,
/// the window, its delegate, and whether that delegate implements the method.
pub const TRACE_ENV: &str = "ROSETTA3_TRACE_EVENT";

pub(crate) fn tracing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os(TRACE_ENV).is_some())
}

fn post(id: u32, name: &str) {
    let object = object_for(id);
    if object.is_null() {
        if tracing() {
            eprintln!("rosetta3: appkit: {name}: SDL window {id} is no window this crate made");
        }
        return;
    }
    post_for(object, name);
}

/// Send the window notification `name` about `object` to its delegate and to
/// observers.
pub(crate) fn post_for(object: Id, name: &str) {
    let delegate = window::delegate_of(object);
    if tracing() {
        eprintln!("rosetta3: appkit: {name}: window {object:p}, delegate {delegate:p}");
    }
    if !delegate.is_null() {
        let selector = delegate_selector(name);
        let notification = foundation::notification::make(name, object, std::ptr::null_mut());
        // SAFETY: a window delegate method is
        // `void (id, SEL, NSNotification *)`.
        let sel = objc::sel::get(selector);
        // SAFETY: null or a live object this runtime allocated.
        let method = unsafe { objc::object::class_of(delegate) }.and_then(|c| c.method(sel));
        if tracing() {
            eprintln!(
                "rosetta3: appkit:   -{selector} is {}",
                match method.is_some() {
                    true => "implemented by the delegate",
                    false => "NOT implemented by the delegate",
                }
            );
        }
        if let Some(method) = method {
            // SAFETY: the prototype above.
            unsafe {
                let f: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
                    std::mem::transmute(method.implementation());
                f(delegate, sel, notification);
            }
        }
    }
    foundation::notification::post(name, object, std::ptr::null_mut());
}

/// The delegate method a notification name goes with.
///
/// The mapping is Apple's and is not derivable: `NSWindowDidResizeNotification`
/// is `-windowDidResize:`, which is the name without `NS` and without
/// `Notification` - but only because every one of them happens to be, and a
/// table is what makes that a decision rather than a coincidence.
fn delegate_selector(name: &str) -> &'static str {
    match name {
        window::DID_RESIZE => "windowDidResize:",
        window::DID_MOVE => "windowDidMove:",
        window::WILL_CLOSE => "windowWillClose:",
        window::DID_BECOME_KEY => "windowDidBecomeKey:",
        window::DID_RESIGN_KEY => "windowDidResignKey:",
        window::DID_BECOME_MAIN => "windowDidBecomeMain:",
        window::DID_RESIGN_MAIN => "windowDidResignMain:",
        window::DID_MINIATURIZE => "windowDidMiniaturize:",
        window::DID_DEMINIATURIZE => "windowDidDeminiaturize:",
        window::WILL_ENTER_FULL_SCREEN => "windowWillEnterFullScreen:",
        window::WILL_EXIT_FULL_SCREEN => "windowWillExitFullScreen:",
        window::DID_ENTER_FULL_SCREEN => "windowDidEnterFullScreen:",
        window::DID_EXIT_FULL_SCREEN => "windowDidExitFullScreen:",
        window::DID_CHANGE_BACKING_PROPERTIES => "windowDidChangeBackingProperties:",
        window::WILL_START_LIVE_RESIZE => "windowWillStartLiveResize:",
        window::DID_END_LIVE_RESIZE => "windowDidEndLiveResize:",
        other => objc::fail!("appkit::event: {other} has no delegate method in this table"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_programmatic_warp_updates_the_reported_pointer() {
        let previous = pointer_position();
        warp_pointer(320.0, 240.0);
        let warped = pointer_position();
        *POINTER.lock().unwrap_or_else(|e| e.into_inner()) = previous;

        assert_eq!(warped, (320.0, 240.0));
    }

    #[test]
    fn a_warp_echo_is_taken_once() {
        expect_warp_echo((400.0, 300.0), (400.0, 300.0));
        assert!(!take_warp_echo(400.0, 300.0));
        expect_warp_echo((0.0, 0.0), (400.0, 300.0));
        assert!(!take_warp_echo(405.0, 300.0));
        assert!(take_warp_echo(400.2, 299.9));
        assert!(!take_warp_echo(400.0, 300.0));
    }

    /// Every name this crate publishes has a delegate method, so a notification
    /// cannot be posted with nowhere to send it.
    #[test]
    fn every_window_notification_has_a_delegate_method() {
        for name in crate::NOTIFICATION_NAMES {
            if name.starts_with("NSWindow") {
                assert!(!delegate_selector(name).is_empty(), "{name}");
            }
        }
    }
}
