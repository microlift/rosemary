use std::ffi::{c_ulong, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use super::objc::{Id, send0, send1, send2};
use super::sys;

/// `NSEventType`, from `<AppKit/NSEvent.h>`. A mask bit is `1 << type`.
const TYPE_LEFT_MOUSE_DOWN: u64 = 1;
const TYPE_LEFT_MOUSE_UP: u64 = 2;
const TYPE_MOUSE_MOVED: u64 = 5;
const TYPE_LEFT_MOUSE_DRAGGED: u64 = 6;
const TYPE_KEY_DOWN: u64 = 10;
const TYPE_SCROLL_WHEEL: u64 = 22;

/// The events the monitor asks for: the pointer, the press-drag-release that
/// moves windows about, a wheel for their contents, and keys for the hotkeys and
/// the one text box the overlay has.
const MASK: u64 = (1 << TYPE_LEFT_MOUSE_DOWN)
    | (1 << TYPE_LEFT_MOUSE_UP)
    | (1 << TYPE_MOUSE_MOVED)
    | (1 << TYPE_LEFT_MOUSE_DRAGGED)
    | (1 << TYPE_KEY_DOWN)
    | (1 << TYPE_SCROLL_WHEEL);

/// `BLOCK_IS_GLOBAL` and `BLOCK_HAS_SIGNATURE`, from `Block_private.h`.
const BLOCK_IS_GLOBAL: i32 = 1 << 28;
const BLOCK_HAS_SIGNATURE: i32 = 1 << 30;

/// The handler's type encoding: returns `id`, takes the block and an `id`, in
/// sixteen bytes of arguments.
const SIGNATURE: &[u8] = b"@16@?0@8\0";

/// A block literal with no captures.
#[repr(C)]
struct Block {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: extern "C" fn(*mut c_void, Id) -> Id,
    descriptor: *const Descriptor,
}

/// Its descriptor. No copy or dispose helpers: nothing is captured, so there is
/// nothing to copy.
#[repr(C)]
struct Descriptor {
    reserved: c_ulong,
    size: c_ulong,
    signature: *const u8,
}

// SAFETY: both structures are plain data whose pointers are to process-lifetime
// objects - a class object from libSystem, a leaked descriptor, and a `'static`
// byte string. They are written once, before publication, and only read after.
unsafe impl Sync for Block {}
unsafe impl Sync for Descriptor {}

/// Whether the overlay is taking the mouse.
static FOCUSED: AtomicBool = AtomicBool::new(false);

/// What has happened since the last frame drew.
///
/// The monitor runs on the main thread and the overlay draws on a render
/// thread, so events are accumulated here and acted on once, where the layout
/// they landed on is known.
#[derive(Default)]
struct Pending {
    at: Option<(f32, f32)>,
    press: Option<(f32, f32)>,
    released: bool,
    delta: (f32, f32),
    wheel: Option<(f32, f32, f32)>,
    keys: Vec<u8>,
    /// Whether the press that is down was one the overlay took. A drag whose
    /// press went to the guest belongs to the guest for its whole length.
    holding: bool,
}

static PENDING: Mutex<Pending> = Mutex::new(Pending {
    at: None,
    press: None,
    released: false,
    delta: (0.0, 0.0),
    wheel: None,
    keys: Vec::new(),
    holding: false,
});

/// Whether the mouse belongs to the overlay right now.
pub fn focused() -> bool {
    FOCUSED.load(Relaxed)
}

/// Take the mouse, or give it back. Bound to shift+F8.
pub fn toggle_focus() -> bool {
    let focused = !FOCUSED.fetch_xor(true, Relaxed);
    match focused {
        true => install(),
        // Anything half-done goes with the mouse, so the overlay does not come
        // back mid-drag.
        false => {
            *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Pending::default();
            super::dock::abandon();
        }
    }
    focused
}

/// Everything that has happened since the last frame, cleared as it is taken.
pub fn drain() -> super::dock::Events {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let events = super::dock::Events {
        at: pending.at,
        press: pending.press.take(),
        released: std::mem::take(&mut pending.released),
        delta: std::mem::replace(&mut pending.delta, (0.0, 0.0)),
        wheel: pending.wheel.take(),
        keys: std::mem::take(&mut pending.keys),
    };
    if !focused() {
        return super::dock::Events::default();
    }
    events
}

/// The panel's row height in pixels, so the wheel can scroll in rows and a drag
/// in points can be turned into one in pixels.
static ROW_HEIGHT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(16);

/// Tell the input layer how tall a row is.
pub fn set_row_height(pixels: f32) {
    ROW_HEIGHT.store(pixels.max(1.0) as u32, Relaxed);
}

/// The handler, called by AppKit for every event in [`MASK`].
///
/// Returns the event to let it through, or nil to swallow it. Never unwinds.
extern "C" fn on_event(_block: *mut c_void, event: Id) -> Id {
    if event.is_null() {
        return event;
    }
    let mut consumed = false;
    super::guarded(|| consumed = hot_key(event) || (focused() && handle(event)));
    match consumed {
        true => std::ptr::null_mut(),
        false => event,
    }
}

/// Where the pointer is, in the pixels the draw list uses.
fn cursor(event: Id) -> Option<(f32, f32)> {
    let sel = super::objc::sel;
    // SAFETY: each is a documented `NSEvent`, `NSWindow` or `NSView` method
    // with the prototype its header gives. `NSPoint` and `NSRect` are
    // homogeneous aggregates of `CGFloat`, which AAPCS64 returns in the vector
    // registers - Rust's `extern "C"` does that correctly for a `repr(C)`
    // struct of `f64`, which is why there is no `_stret` question here.
    unsafe {
        let window = send0::<Id>(event, sel("window"));
        if window.is_null() {
            return None;
        }
        let view = send0::<Id>(window, sel("contentView"));
        if view.is_null() {
            return None;
        }
        let location = send0::<sys::Point>(event, sel("locationInWindow"));
        let in_view = send2::<sys::Point, sys::Point, Id>(
            view,
            sel("convertPoint:fromView:"),
            location,
            std::ptr::null_mut(),
        );
        let backing = send1::<sys::Point, sys::Point>(view, sel("convertPointToBacking:"), in_view);
        let bounds = send0::<sys::Rect>(view, sel("bounds"));
        let bounds_backing =
            send1::<sys::Rect, sys::Rect>(view, sel("convertRectToBacking:"), bounds);
        Some((
            backing.x as f32,
            (bounds_backing.size.height - backing.y) as f32,
        ))
    }
}

/// The characters a key event carries, as the ASCII the atlas can draw.
fn typed(event: Id) -> Vec<u8> {
    let sel = super::objc::sel;
    // SAFETY: `-characters` is `NSEvent`'s and returns an autoreleased
    // `NSString`; `-UTF8String` is `NSString`'s and returns a C string owned by
    // that string, read before this scope ends.
    unsafe {
        let string = send0::<Id>(event, sel("characters"));
        if string.is_null() {
            return Vec::new();
        }
        let bytes = send0::<*const libc::c_char>(string, sel("UTF8String"));
        if bytes.is_null() {
            return Vec::new();
        }
        std::ffi::CStr::from_ptr(bytes).to_bytes().to_vec()
    }
}

/// Act on `event` if it is a hot key. Returns whether it was one.
///
/// The only route under the builtin frameworks, which have neither Carbon hot
/// keys nor a window server whose key state could be polled.
fn hot_key(event: Id) -> bool {
    let sel = super::objc::sel;
    // SAFETY: `-type`, `-isARepeat`, `-keyCode` and `-modifierFlags` are
    // `NSEvent`'s, take nothing, and return an `NSEventType`, a `BOOL`, an
    // `unsigned short` and an `NSEventModifierFlags`. The last three are asked
    // only of a key-down event, which is the one kind they are defined on.
    let key = unsafe {
        if send0::<u64>(event, sel("type")) != TYPE_KEY_DOWN
            || send0::<u8>(event, sel("isARepeat")) != 0
        {
            return false;
        }
        let code = send0::<u16>(event, sel("keyCode"));
        let modifiers = send0::<u64>(event, sel("modifierFlags"));
        super::hotkey::Key::from_key_event(code, modifiers)
    };
    let Some(key) = key else {
        return false;
    };
    super::hotkey::delivered("the NSEvent monitor");
    super::pressed(key);
    true
}

/// Act on one event. Returns whether the guest should be denied it.
fn handle(event: Id) -> bool {
    // SAFETY: `-type` is `NSEvent`'s, takes nothing and returns an
    // `NSEventType`, which is an `NSUInteger`.
    let kind = unsafe { send0::<u64>(event, super::objc::sel("type")) };

    if kind == TYPE_KEY_DOWN {
        // Only while a text box is open, so the guest keeps its keyboard.
        if !super::dock::typing() {
            return false;
        }
        let keys = typed(event);
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        pending.keys.extend(keys);
        return true;
    }

    let at = cursor(event);
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(at) = at {
        pending.at = Some(at);
    }
    // Where the pointer is decides whose event this is, and the answer is the
    // rectangles the overlay drew last frame.
    let over = at.is_some_and(|(x, y)| super::dock::occupied(x, y));

    match kind {
        TYPE_MOUSE_MOVED => false,
        TYPE_LEFT_MOUSE_DOWN => {
            pending.holding = over;
            if over {
                pending.press = at;
            }
            over
        }
        TYPE_LEFT_MOUSE_UP => {
            let held = std::mem::take(&mut pending.holding);
            if held {
                pending.released = true;
            }
            held
        }
        TYPE_LEFT_MOUSE_DRAGGED => {
            if !pending.holding {
                return false;
            }
            // SAFETY: `-deltaX` and `-deltaY` are `NSEvent`'s, taking nothing
            // and returning `CGFloat`. Deltas rather than the absolute cursor:
            // a drag driven by deltas cannot drift away from where the grab
            // started, and a resize driven by absolute positions would jump the
            // first time a conversion was a pixel out.
            let (dx, dy) = unsafe {
                (
                    send0::<f64>(event, super::objc::sel("deltaX")) as f32,
                    send0::<f64>(event, super::objc::sel("deltaY")) as f32,
                )
            };
            // The deltas are in points and the windows are in pixels. The ratio
            // is whatever the drawable's is, which the row height carries.
            let scale = (ROW_HEIGHT.load(Relaxed) as f32 / 16.0).clamp(1.0, 3.0);
            pending.delta.0 += dx * scale;
            pending.delta.1 += dy * scale;
            true
        }
        TYPE_SCROLL_WHEEL => {
            if !over {
                return false;
            }
            // SAFETY: `-scrollingDeltaY` and `-hasPreciseScrollingDeltas` are
            // `NSEvent`'s, taking nothing and returning a `CGFloat` and a
            // `BOOL`.
            let (delta, precise) = unsafe {
                (
                    send0::<f64>(event, super::objc::sel("scrollingDeltaY")) as f32,
                    send0::<u8>(event, super::objc::sel("hasPreciseScrollingDeltas")) != 0,
                )
            };
            // A precise delta is in points and a coarse one is in *lines*, and
            // treating them alike makes a trackpad a hundred times faster than
            // a wheel.
            let row = ROW_HEIGHT.load(Relaxed).max(1) as f32;
            let pixels = match precise {
                true => delta * row / 14.0,
                false => delta * row * 3.0,
            };
            let (x, y) = at.unwrap_or_default();
            let carried = pending.wheel.map(|(.., p)| p).unwrap_or(0.0);
            pending.wheel = Some((x, y, carried + pixels));
            true
        }
        _ => false,
    }
}

/// Whether the monitor is in, or on its way in from the main queue.
static ASKED: AtomicBool = AtomicBool::new(false);

/// Put the monitor in, once, from the main thread.
pub fn install() {
    if ASKED.swap(true, Relaxed) {
        return;
    }
    type AsyncF = unsafe extern "C" fn(*mut c_void, *mut c_void, extern "C" fn(*mut c_void));
    // SAFETY: `dispatch_async_f`'s prototype from `<dispatch/queue.h>`, and
    // `_dispatch_main_q` is the object `dispatch_get_main_queue()` is a macro
    // for - its *address* is the queue.
    let (Some(async_f), Some(main_queue)) = (unsafe {
        (
            sys::function::<AsyncF>("dispatch_async_f"),
            sys::symbol("_dispatch_main_q"),
        )
    }) else {
        super::say_once("no libdispatch: the overlay cannot take the mouse");
        ASKED.store(false, Relaxed);
        return;
    };
    // SAFETY: a real queue and an `extern "C"` function of the right shape. The
    // context is null because `install_on_main` captures nothing.
    unsafe { async_f(main_queue, std::ptr::null_mut(), install_on_main) };
}

/// The half that has to be on the main thread.
extern "C" fn install_on_main(_context: *mut c_void) {
    super::guarded(|| {
        let class = super::objc::class("NSEvent");
        if class.is_null() {
            super::say_once(
                "no NSEvent: this guest has not loaded AppKit yet, so there is no mouse; \
                 shift+F8 asks again",
            );
            ASKED.store(false, Relaxed);
            return;
        }
        let descriptor = Box::leak(Box::new(Descriptor {
            reserved: 0,
            size: size_of::<Block>() as c_ulong,
            signature: SIGNATURE.as_ptr(),
        }));
        let Some(isa) = sys::symbol("_NSConcreteGlobalBlock") else {
            super::say_once("no _NSConcreteGlobalBlock: the mouse handler cannot be built");
            return;
        };
        let block = Box::leak(Box::new(Block {
            isa,
            flags: BLOCK_IS_GLOBAL | BLOCK_HAS_SIGNATURE,
            reserved: 0,
            invoke: on_event,
            descriptor,
        }));
        // SAFETY: `+addLocalMonitorForEventsMatchingMask:handler:` is
        // `NSEvent`'s, takes an `NSEventMask` and a block, and returns the
        // monitor object. The block is a correctly formed global block, leaked,
        // so it outlives the monitor - which is never removed, because the
        // overlay lives as long as the process.
        let monitor = unsafe {
            send2::<Id, u64, *const Block>(
                class,
                super::objc::sel("addLocalMonitorForEventsMatchingMask:handler:"),
                MASK,
                block,
            )
        };
        if monitor.is_null() {
            super::say_once("AppKit refused the overlay's event monitor");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both of these drive [`FOCUSED`], which is one flag for the process.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Draining twice does not deliver the same press twice.
    ///
    /// A press acted on in two consecutive frames is a window that jumps to the
    /// front twice, or a button pressed twice - and the second one is invisible.
    #[test]
    fn a_press_is_delivered_once() {
        let _serial = serial();
        FOCUSED.store(true, Relaxed);
        {
            let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
            *pending = Pending::default();
            pending.press = Some((10.0, 20.0));
            pending.at = Some((10.0, 20.0));
        }
        assert_eq!(drain().press, Some((10.0, 20.0)));
        assert_eq!(drain().press, None);
        FOCUSED.store(false, Relaxed);
    }

    /// Nothing is delivered while the guest has the mouse.
    #[test]
    fn an_unfocused_overlay_gets_nothing() {
        let _serial = serial();
        FOCUSED.store(false, Relaxed);
        {
            let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
            pending.press = Some((10.0, 20.0));
        }
        let events = drain();
        assert!(events.press.is_none() && events.at.is_none());
    }
}
