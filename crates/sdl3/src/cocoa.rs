//! Rounded corners on a Cocoa window, cut by the window server.
//!
//! A window with the runtime's own titlebar is borderless to AppKit, which
//! draws it square. Masking its content view's layer rounds every path that
//! presents into it - the window surface, OpenGL and Metal sublayers - alike,
//! so nothing has to give its pixels alpha or cut them itself.

use std::ffi::{CStr, c_char, c_void};

use crate::{Window, video};

type Id = *mut c_void;
type Sel = *const c_void;

// The host's libobjc, whichever Objective-C runtime the guest has.
#[link(name = "objc")]
unsafe extern "C" {
    #[link_name = "objc_getClass"]
    fn get_class(name: *const c_char) -> Id;
    #[link_name = "sel_registerName"]
    fn register_selector(name: *const c_char) -> Sel;
    #[link_name = "objc_msgSend"]
    fn msg_send();
    #[link_name = "objc_autoreleasePoolPush"]
    fn pool_push() -> *mut c_void;
    #[link_name = "objc_autoreleasePoolPop"]
    fn pool_pop(token: *mut c_void);
}

// `objc_msgSend` cast to each method's own signature.
macro_rules! send {
    ($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty) => {
        unsafe fn $name(receiver: Id, selector: &CStr, $($arg: $ty),*) -> $ret {
            let send: unsafe extern "C" fn(Id, Sel, $($ty),*) -> $ret =
                unsafe { std::mem::transmute(msg_send as unsafe extern "C" fn()) };
            unsafe { send(receiver, register_selector(selector.as_ptr()), $($arg),*) }
        }
    };
}

send!(send_id() -> Id);
send!(send_f64() -> f64);
send!(send_void() -> ());
send!(send_void_id(value: Id) -> ());
send!(send_void_bool(value: bool) -> ());
send!(send_void_f64(value: f64) -> ());

/// Round `window`'s corners to `radius` points, or square them for 0.
/// False when it has no `NSWindow` to round.
pub fn round_corners(window: Window, radius: f64) -> bool {
    if window.is_null() || !video::is_video_thread() {
        return false;
    }
    let Some(ns_window) = video::cocoa_window(window) else {
        return false;
    };
    // SAFETY: on the main thread, which SDL's Cocoa video thread is, with a
    // live `NSWindow` SDL owns; each selector is sent with its own signature.
    unsafe {
        let pool = pool_push();
        let rounded = round(ns_window, radius);
        pool_pop(pool);
        rounded
    }
}

unsafe fn round(window: Id, radius: f64) -> bool {
    unsafe {
        let view = send_id(window, c"contentView");
        if view.is_null() {
            return false;
        }
        send_void_bool(view, c"setWantsLayer:", true);
        let layer = send_id(view, c"layer");
        if layer.is_null() {
            return false;
        }
        if send_f64(layer, c"cornerRadius") == radius {
            return true;
        }
        let rounded = radius > 0.0;
        send_void_f64(layer, c"setCornerRadius:", radius);
        send_void_bool(layer, c"setMasksToBounds:", rounded);
        // What the mask cuts away shows the desktop only through a window
        // that is not opaque; a square one stays opaque for the compositor.
        if rounded {
            let clear = send_id(get_class(c"NSColor".as_ptr()), c"clearColor");
            send_void_id(window, c"setBackgroundColor:", clear);
        }
        send_void_bool(window, c"setOpaque:", !rounded);
        // The shadow follows the window's shape as it was when last cast.
        send_void(window, c"invalidateShadow");
        true
    }
}
