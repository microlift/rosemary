//! Host-side black bands over an SDL X11 OpenGL window.
//!
//! A guest may stop swapping after resizing its logical fullscreen frame.
//! OpenGL commands cannot be issued from AppKit's event thread while its
//! context belongs to another thread, so the latest *front* buffer needs an
//! independent host presentation boundary. An X11 child window with black
//! background, a bounding shape containing only excluded bands, and an empty
//! input shape obscures stale pixels immediately without taking the GL
//! context, resizing SDL or consuming the guest's mouse/keyboard events.
//! No link-time X11 requirement: SDL itself loads dynamically.

use crate::{Rect, Window, video};
use std::collections::HashMap;
use std::ffi::{c_int, c_ulong, c_void};
use std::sync::{Mutex, OnceLock};

type Display = *mut c_void;
type NativeWindow = c_ulong;

#[repr(C)]
#[derive(Clone, Copy)]
struct XRectangle {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

struct Xlib {
    _x11: usize,
    _xext: usize,
    create: unsafe extern "C" fn(
        Display,
        NativeWindow,
        c_int,
        c_int,
        u32,
        u32,
        u32,
        c_ulong,
        c_ulong,
    ) -> NativeWindow,
    resize: unsafe extern "C" fn(Display, NativeWindow, c_int, c_int, u32, u32) -> c_int,
    map: unsafe extern "C" fn(Display, NativeWindow) -> c_int,
    unmap: unsafe extern "C" fn(Display, NativeWindow) -> c_int,
    destroy: unsafe extern "C" fn(Display, NativeWindow) -> c_int,
    lock: unsafe extern "C" fn(Display),
    unlock: unsafe extern "C" fn(Display),
    sync: unsafe extern "C" fn(Display, c_int) -> c_int,
    shape_query: unsafe extern "C" fn(Display, *mut c_int, *mut c_int) -> c_int,
    shape_version: unsafe extern "C" fn(Display, *mut c_int, *mut c_int) -> c_int,
    shape: unsafe extern "C" fn(
        Display,
        NativeWindow,
        c_int,
        c_int,
        c_int,
        *const XRectangle,
        c_int,
        c_int,
        c_int,
    ),
}
// Function pointers and handles loaded once; display access serialized by Xlib.
unsafe impl Send for Xlib {}
unsafe impl Sync for Xlib {}

fn xlib() -> Option<&'static Xlib> {
    static LIB: OnceLock<Option<Xlib>> = OnceLock::new();
    LIB.get_or_init(|| unsafe {
        let x11 = libc::dlopen(c"libX11.so.6".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if x11.is_null() {
            return None;
        }
        let xext = libc::dlopen(c"libXext.so.6".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if xext.is_null() {
            libc::dlclose(x11);
            return None;
        }
        macro_rules! sym {
            ($handle:expr, $symbol:literal) => {{
                let symbol = libc::dlsym($handle, concat!($symbol, "\0").as_ptr().cast());
                if symbol.is_null() {
                    return None;
                }
                {
                    // Function pointers have pointer-sized ABI on supported
                    // X11 platforms; the inferred T is the exact Xlib field type.
                    std::mem::transmute_copy::<*mut c_void, _>(&symbol)
                }
            }};
        }
        Some(Xlib {
            _x11: x11 as usize,
            _xext: xext as usize,
            create: sym!(x11, "XCreateSimpleWindow"),
            resize: sym!(x11, "XMoveResizeWindow"),
            map: sym!(x11, "XMapRaised"),
            unmap: sym!(x11, "XUnmapWindow"),
            destroy: sym!(x11, "XDestroyWindow"),
            lock: sym!(x11, "XLockDisplay"),
            unlock: sym!(x11, "XUnlockDisplay"),
            sync: sym!(x11, "XSync"),
            shape_query: sym!(xext, "XShapeQueryExtension"),
            shape_version: sym!(xext, "XShapeQueryVersion"),
            shape: sym!(xext, "XShapeCombineRectangles"),
        })
    })
    .as_ref()
}

#[derive(Clone, Copy)]
struct Mask {
    display: usize,
    overlay: NativeWindow,
    size: (i32, i32),
    clip: Rect,
    shown: bool,
}
static MASKS: Mutex<Option<HashMap<usize, Mask>>> = Mutex::new(None);

fn bands(size: (i32, i32), clip: Rect) -> Vec<XRectangle> {
    clip.complements(size)
        .map(|band| XRectangle {
            x: band.x as i16,
            y: band.y as i16,
            width: band.w as u16,
            height: band.h as u16,
        })
        .collect()
}

/// Set or clear the visual mask without touching the guest's OpenGL context.
/// Returns false when X11/Shape is not available. `clip` is in host window
/// (not pixel-density-scaled) coordinates.
pub fn update(window: Window, clip: Option<Rect>) -> bool {
    let Some(x) = xlib() else {
        return false;
    };
    let Some((display, host)) = video::x11_native_window(window) else {
        return false;
    };
    let Some(size) = video::window_size(window) else {
        return false;
    };
    let mut masks = MASKS.lock().unwrap_or_else(|e| e.into_inner());
    let map = masks.get_or_insert_with(HashMap::new);
    let clip = clip.unwrap_or(Rect {
        x: 0,
        y: 0,
        w: size.0,
        h: size.1,
    });
    // Most GL windows never need a mask. Do not create a host child for a
    // fullscreen window until the guest actually narrows its logical frame.
    if !map.contains_key(&(window as usize)) && bands(size, clip).is_empty() {
        return true;
    }
    if let std::collections::hash_map::Entry::Vacant(vacant) = map.entry(window as usize) {
        let mut event = 0;
        let mut error = 0;
        // SAFETY: SDL supplies a live X11 Display and native window; access is
        // guarded by Xlib on the same display. Child is input-transparent.
        let overlay = unsafe {
            (x.lock)(display);
            let supported = (x.shape_query)(display, &mut event, &mut error) != 0;
            let mut major = 0;
            let mut minor = 0;
            let supported = supported
                && (x.shape_version)(display, &mut major, &mut minor) != 0
                && (major > 1 || (major == 1 && minor >= 1));
            let overlay = if supported {
                (x.create)(
                    display,
                    host as NativeWindow,
                    0,
                    0,
                    size.0.max(1) as u32,
                    size.1.max(1) as u32,
                    0,
                    0,
                    0,
                )
            } else {
                0
            };
            if overlay != 0 {
                (x.shape)(display, overlay, 2, 0, 0, std::ptr::null(), 0, 0, 0);
            }
            (x.unlock)(display);
            overlay
        };
        if overlay == 0 {
            return false;
        }
        vacant.insert(Mask {
            display: display as usize,
            overlay,
            size,
            clip: Rect::default(),
            shown: false,
        });
    }
    let entry = map.get_mut(&(window as usize)).expect("just inserted");
    if entry.clip == clip && entry.size == size {
        return true;
    }
    let mask_bands = bands(size, clip);
    // SAFETY: SDL retains the X11 display and parent until forget() below.
    unsafe {
        (x.lock)(display);
        if entry.size != size {
            (x.resize)(
                display,
                entry.overlay,
                0,
                0,
                size.0.max(1) as u32,
                size.1.max(1) as u32,
            );
        }
        (x.shape)(
            display,
            entry.overlay,
            0,
            0,
            0,
            mask_bands.as_ptr(),
            mask_bands.len() as c_int,
            0,
            0,
        );
        if mask_bands.is_empty() {
            if entry.shown {
                (x.unmap)(display, entry.overlay);
            }
        } else {
            (x.map)(display, entry.overlay);
        }
        (x.sync)(display, 0);
        (x.unlock)(display);
    }
    entry.clip = clip;
    entry.size = size;
    entry.shown = !mask_bands.is_empty();
    true
}

/// Destroy a child mask while its SDL parent and X11 display are still alive.
pub fn forget(window: Window) {
    let entry = MASKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|map| map.remove(&(window as usize)));
    if let (Some(x), Some(entry)) = (xlib(), entry) {
        let display = entry.display as Display;
        unsafe {
            (x.lock)(display);
            (x.destroy)(display, entry.overlay);
            (x.sync)(display, 0);
            (x.unlock)(display);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blackout_bands_are_exact_complements_of_logical_rectangle() {
        let clips = [
            Rect {
                x: 1,
                y: 2,
                w: 7,
                h: 5,
            },
            Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 8,
            },
            Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 7,
            },
            Rect {
                x: 1,
                y: 0,
                w: 9,
                h: 8,
            },
        ];
        for clip in clips {
            let shapes = bands((10, 8), clip);
            for y in 0..8 {
                for x in 0..10 {
                    let covered = shapes
                        .iter()
                        .filter(|band| {
                            x >= i32::from(band.x)
                                && y >= i32::from(band.y)
                                && x < i32::from(band.x) + i32::from(band.width)
                                && y < i32::from(band.y) + i32::from(band.height)
                        })
                        .count();
                    let excluded =
                        x < clip.x || y < clip.y || x >= clip.x + clip.w || y >= clip.y + clip.h;
                    assert_eq!(covered > 0, excluded, "pixel ({x},{y}) of {clip:?}");
                    assert!(covered <= 1);
                }
            }
        }
    }
}
