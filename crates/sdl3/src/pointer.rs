//! Putting the host pointer where a guest asked, on Wayland, where SDL's own
//! warp is not enough.
//!
//! SDL prefers `wp_pointer_warp_v1`, which mutter refuses to a client not
//! clicked since the pointer entered it, and no compositor moves a pointer
//! that is outside the client's surfaces. [`warp`] goes through a momentary
//! pointer lock instead, and [`pin`] holds a point for a pointer that is
//! outside until it comes back.
//!
//! A lock is the one constraint a seat may have on a surface, so a warp first
//! lifts the ones SDL holds - relative mode, a grab, a confinement rectangle -
//! and puts them back after. SDL's own warp emulation would take relative
//! mode without being asked, so [`crate::video::init_video`] turns it off.

use std::sync::Mutex;

use crate::video::{self, flags};
use crate::wayland;
use crate::{Rect, Window};

/// Points a pointer coming onto a window is to be put at, by `wl_surface`.
static PINS: Mutex<Vec<Pin>> = Mutex::new(Vec::new());

#[derive(Clone, Copy)]
struct Pin {
    surface: usize,
    window: usize,
    at: (f32, f32),
}

/// Put the host pointer at `(x, y)` in `window`'s coordinates.
///
/// `Some` with where the pointer was, surface-local, when a warp was made;
/// the compositor reports the move as motion. `None` when the pointer is
/// already there, or either it or the point is off the window, where no
/// compositor will put a pointer.
pub fn warp(window: Window, x: f32, y: f32) -> Option<(f32, f32)> {
    let (_, surface) = video::wayland_surface(window)?;
    if !inside(window, (x, y)) {
        return None;
    }
    // SDL warps in window coordinates and scales them to the surface; this
    // path does not, so it is taken only where the two are the same.
    if scales_to_display() || !wayland::has_pointer_constraints() {
        let from = video::mouse_state().map(|(x, y, _)| (x, y));
        video::warp_mouse_in_window(window, x, y);
        return from;
    }
    let from = wayland::pointer_at(surface)?;
    if same_point(from, (x, y)) {
        return None;
    }
    let _commits = crate::compositor::guard();
    let lifted = lift_constraints(window, (x, y));
    let warped = wayland::warp_through_lock(surface, x, y);
    lifted.restore(window);
    warped.then_some(from)
}

/// Keep `window`'s pointer at `at`, in window coordinates, each time it
/// comes onto the window, or stop for `None`. Applied by [`service`].
pub fn pin(window: Window, at: Option<(f32, f32)>) {
    let Some(at) = at.filter(|at| inside(window, *at)) else {
        forget(window);
        return;
    };
    let Some((_, surface)) = video::wayland_surface(window) else {
        return;
    };
    let mut pins = PINS.lock().unwrap_or_else(|e| e.into_inner());
    pins.retain(|pin| pin.window != window as usize);
    pins.push(Pin {
        surface: surface as usize,
        window: window as usize,
        at,
    });
}

/// Drop `window`'s pin without asking SDL anything, for a window SDL may
/// already have destroyed.
pub fn forget(window: Window) {
    PINS.lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|pin| pin.window != window as usize);
}

/// Bring pinned pointers that came onto their windows to their points.
///
/// After SDL's events are taken: a warp may leave and re-enter relative mode,
/// which SDL must not do from inside its own dispatch.
pub fn service() {
    let entered = wayland::take_entered();
    if entered.is_empty() {
        return;
    }
    let pins: Vec<Pin> = PINS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for pin in pins {
        if entered.contains(&pin.surface) {
            warp(pin.window as Window, pin.at.0, pin.at.1);
        }
    }
}

/// The constraints SDL held on a window before [`lift_constraints`].
struct Lifted {
    relative: bool,
    grabbed: bool,
    rect: Option<Rect>,
}

/// Lift SDL's constraints on `window` for a warp to `to`.
fn lift_constraints(window: Window, to: (f32, f32)) -> Lifted {
    let lifted = Lifted {
        relative: video::relative_mouse_mode(window),
        grabbed: video::window_flags(window) & flags::MOUSE_GRABBED != 0,
        rect: video::get_mouse_rect(window),
    };
    if lifted.relative {
        // Leaving relative mode warps to SDL's own position: make that the
        // target. In relative mode this moves nothing else.
        video::warp_mouse_in_window(window, to.0, to.1);
        video::set_relative_mouse_mode(window, false);
    }
    if lifted.rect.is_some() {
        video::set_mouse_rect(window, None);
    }
    if lifted.grabbed {
        video::set_mouse_grab(window, false);
    }
    lifted
}

impl Lifted {
    fn restore(self, window: Window) {
        if self.grabbed {
            video::set_mouse_grab(window, true);
        }
        if self.rect.is_some() {
            video::set_mouse_rect(window, self.rect);
        }
        if self.relative {
            video::set_relative_mouse_mode(window, true);
        }
    }
}

/// `SDL_HINT_VIDEO_WAYLAND_SCALE_TO_DISPLAY`: window coordinates are then
/// buffer pixels, which SDL divides by a scale it does not publish.
fn scales_to_display() -> bool {
    video::get_hint(c"SDL_VIDEO_WAYLAND_SCALE_TO_DISPLAY")
        .is_some_and(|value| !matches!(value.to_bytes(), b"0" | b"false" | b""))
}

fn inside(window: Window, (x, y): (f32, f32)) -> bool {
    video::window_size(window)
        .is_some_and(|(w, h)| x >= 0.0 && y >= 0.0 && x < w as f32 && y < h as f32)
}

/// Within half a pixel: window and surface coordinates meet through floats.
fn same_point(a: (f32, f32), b: (f32, f32)) -> bool {
    (a.0 - b.0).abs() < 0.5 && (a.1 - b.1).abs() < 0.5
}
