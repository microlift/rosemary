//! Hide stale, already-presented OpenGL pixels through Wayland composition.
//!
//! The guest owns its EGL context, possibly on another thread. An
//! input-transparent `wl_subsurface` carries a full-size ARGB buffer: pixels
//! inside AppKit's logical window are transparent, and excluded pixels opaque
//! black. The child is desynchronized from the parent and stays at (0,0):
//! only its own buffer is committed when the logical frame moves or shrinks.
//! In particular, `wl_subsurface.set_position` would wait for the parent's
//! next commit, so never use that operation to update idle-frame boundaries.
//! No requests are made to the guest's EGL context or its SDL wl_surface.

use crate::{Rect, Window, video};
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::{Mutex, OnceLock};

type Proxy = *mut c_void;

#[derive(Default)]
struct RegistryIds {
    compositor: u32,
    shm: u32,
    subcompositor: u32,
}

#[repr(C)]
struct RegistryListener {
    global: unsafe extern "C" fn(*mut c_void, Proxy, u32, *const c_char, u32),
    global_remove: unsafe extern "C" fn(*mut c_void, Proxy, u32),
}

unsafe extern "C" fn global(
    data: *mut c_void,
    _: Proxy,
    name: u32,
    interface: *const c_char,
    _: u32,
) {
    if data.is_null() || interface.is_null() {
        return;
    }
    // SAFETY: Wayland's registry callback owns a valid NUL-terminated name,
    // and the stack-local registry IDs outlive this synchronous roundtrip.
    let found = unsafe { &mut *data.cast::<RegistryIds>() };
    let interface = unsafe { CStr::from_ptr(interface) };
    if interface == c"wl_compositor" {
        found.compositor = name;
    } else if interface == c"wl_shm" {
        found.shm = name;
    } else if interface == c"wl_subcompositor" {
        found.subcompositor = name;
    }
}
unsafe extern "C" fn global_remove(_: *mut c_void, _: Proxy, _: u32) {}
static REGISTRY_LISTENER: RegistryListener = RegistryListener {
    global,
    global_remove,
};

struct Client {
    marshal: unsafe extern "C" fn(Proxy, u32, *const c_void, u32, u32, ...) -> Proxy,
    add_listener: unsafe extern "C" fn(Proxy, *const c_void, *mut c_void) -> c_int,
    #[cfg(test)]
    roundtrip: unsafe extern "C" fn(Proxy) -> c_int,
    flush: unsafe extern "C" fn(Proxy) -> c_int,
    proxy_destroy: unsafe extern "C" fn(Proxy),
    create_queue: unsafe extern "C" fn(Proxy) -> *mut c_void,
    queue_destroy: unsafe extern "C" fn(*mut c_void),
    create_wrapper: unsafe extern "C" fn(Proxy) -> Proxy,
    wrapper_destroy: unsafe extern "C" fn(Proxy),
    set_queue: unsafe extern "C" fn(Proxy, *mut c_void),
    roundtrip_queue: unsafe extern "C" fn(Proxy, *mut c_void) -> c_int,
    dispatch_queue_pending: unsafe extern "C" fn(Proxy, *mut c_void) -> c_int,
    registry: usize,
    compositor: usize,
    surface: usize,
    region: usize,
    shm: usize,
    pool: usize,
    buffer: usize,
    subcompositor: usize,
    subsurface: usize,
}
// SAFETY: immutable dynamically loaded function pointers and protocol
// interface descriptors; the native Wayland display is owned by SDL.
unsafe impl Send for Client {}
unsafe impl Sync for Client {}

fn client() -> Option<&'static Client> {
    static CLIENT: OnceLock<Option<Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| unsafe {
            let handle = libc::dlopen(
                c"libwayland-client.so.0".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_LOCAL,
            );
            if handle.is_null() {
                return None;
            }
            unsafe fn symbol<T>(handle: *mut c_void, name: &CStr) -> Option<T> {
                let symbol = unsafe { libc::dlsym(handle, name.as_ptr()) };
                if symbol.is_null() {
                    return None;
                }
                assert_eq!(size_of::<T>(), size_of::<*mut c_void>());
                Some(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&symbol) })
            }
            Some(Client {
                marshal: symbol(handle, c"wl_proxy_marshal_flags")?,
                add_listener: symbol(handle, c"wl_proxy_add_listener")?,
                #[cfg(test)]
                roundtrip: symbol(handle, c"wl_display_roundtrip")?,
                flush: symbol(handle, c"wl_display_flush")?,
                proxy_destroy: symbol(handle, c"wl_proxy_destroy")?,
                create_queue: symbol(handle, c"wl_display_create_queue")?,
                queue_destroy: symbol(handle, c"wl_event_queue_destroy")?,
                create_wrapper: symbol(handle, c"wl_proxy_create_wrapper")?,
                wrapper_destroy: symbol(handle, c"wl_proxy_wrapper_destroy")?,
                set_queue: symbol(handle, c"wl_proxy_set_queue")?,
                roundtrip_queue: symbol(handle, c"wl_display_roundtrip_queue")?,
                dispatch_queue_pending: symbol(handle, c"wl_display_dispatch_queue_pending")?,
                registry: symbol::<*mut c_void>(handle, c"wl_registry_interface")? as usize,
                compositor: symbol::<*mut c_void>(handle, c"wl_compositor_interface")? as usize,
                surface: symbol::<*mut c_void>(handle, c"wl_surface_interface")? as usize,
                region: symbol::<*mut c_void>(handle, c"wl_region_interface")? as usize,
                shm: symbol::<*mut c_void>(handle, c"wl_shm_interface")? as usize,
                pool: symbol::<*mut c_void>(handle, c"wl_shm_pool_interface")? as usize,
                buffer: symbol::<*mut c_void>(handle, c"wl_buffer_interface")? as usize,
                subcompositor: symbol::<*mut c_void>(handle, c"wl_subcompositor_interface")?
                    as usize,
                subsurface: symbol::<*mut c_void>(handle, c"wl_subsurface_interface")? as usize,
            })
        })
        .as_ref()
}

const DESTROY: u32 = 1;

// This protocol binding is deliberately restricted to version 1 for the
// globals and surface version 1: attach, damage and commit suffice. No
// extension or version-specific interface layout is required.
struct Runtime {
    display: usize,
    // Every proxy here is on this queue, never SDL's default one: dispatching
    // that would run SDL's listeners outside SDL's pump, on any thread.
    queue: usize,
    compositor: usize,
    shm: usize,
    subcompositor: usize,
}
impl Runtime {
    unsafe fn initialize(c: &Client, display: Proxy) -> Option<Self> {
        let queue = unsafe { (c.create_queue)(display) };
        if queue.is_null() {
            return None;
        }
        // The registry inherits the wrapper's queue, and the globals bound
        // from it inherit the registry's.
        let wrapper = unsafe { (c.create_wrapper)(display) };
        if wrapper.is_null() {
            unsafe { (c.queue_destroy)(queue) };
            return None;
        }
        unsafe { (c.set_queue)(wrapper, queue) };
        let registry = unsafe {
            (c.marshal)(
                wrapper,
                1,
                c.registry as *const c_void,
                1,
                0,
                std::ptr::null_mut::<c_void>(),
            )
        };
        unsafe { (c.wrapper_destroy)(wrapper) };
        if registry.is_null() {
            unsafe { (c.queue_destroy)(queue) };
            return None;
        }
        let mut ids = RegistryIds::default();
        let listened = unsafe {
            (c.add_listener)(
                registry,
                (&raw const REGISTRY_LISTENER).cast(),
                (&raw mut ids).cast(),
            )
        } == 0;
        let roundtrip = listened && unsafe { (c.roundtrip_queue)(display, queue) } >= 0;
        if !roundtrip || ids.compositor == 0 || ids.shm == 0 || ids.subcompositor == 0 {
            unsafe { (c.proxy_destroy)(registry) };
            unsafe { (c.queue_destroy)(queue) };
            return None;
        }
        unsafe fn bind(c: &Client, registry: Proxy, name: u32, interface: usize) -> Proxy {
            // wl_registry.bind(name, interface_name, version, new_id).
            let interface_name = unsafe { *(interface as *const *const c_char) };
            unsafe {
                (c.marshal)(
                    registry,
                    0,
                    interface as *const c_void,
                    1,
                    0,
                    name,
                    interface_name,
                    1u32,
                    std::ptr::null_mut::<c_void>(),
                )
            }
        }
        let compositor = unsafe { bind(c, registry, ids.compositor, c.compositor) };
        let shm = unsafe { bind(c, registry, ids.shm, c.shm) };
        let subcompositor = unsafe { bind(c, registry, ids.subcompositor, c.subcompositor) };
        unsafe { (c.proxy_destroy)(registry) };
        if compositor.is_null() || shm.is_null() || subcompositor.is_null() {
            return None;
        }
        Some(Self {
            display: display as usize,
            queue: queue as usize,
            compositor: compositor as usize,
            shm: shm as usize,
            subcompositor: subcompositor as usize,
        })
    }
}

struct Mask {
    parent: usize,
    surface: usize,
    subsurface: usize,
    buffer: usize,
    size: (i32, i32),
    clip: Rect,
}
#[derive(Default)]
struct State {
    runtime: Option<Runtime>,
    masks: HashMap<usize, Mask>,
}
static STATE: Mutex<Option<State>> = Mutex::new(None);

/// An immutable ARGB buffer, transparent inside the logical window and
/// opaque black outside it. Once attached it is never modified; a fresh
/// wl_buffer replaces it on every logical clip change.
unsafe fn black_buffer(
    c: &Client,
    rt: &Runtime,
    width: i32,
    height: i32,
    clip: Rect,
) -> Option<Proxy> {
    let bytes = usize::try_from(width)
        .ok()?
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if bytes == 0 || bytes > 256 * 1024 * 1024 {
        return None;
    }
    let fd = unsafe { libc::memfd_create(c"rosemary-wl-black".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    if unsafe { libc::ftruncate(fd, bytes as libc::off_t) } != 0 {
        unsafe { libc::close(fd) };
        return None;
    }
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            bytes,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        unsafe { libc::close(fd) };
        return None;
    }
    // Premultiplied ARGB8888: zero is fully transparent, and 0xff000000
    // is opaque black. Paint the complement, not a moved/scaled subsurface.
    let pixels = unsafe { std::slice::from_raw_parts_mut(ptr.cast::<u32>(), bytes / 4) };
    pixels.fill(0);
    for band in clip.complements((width, height)) {
        let (x, w) = (band.x as usize, band.w as usize);
        for row in band.y as usize..(band.y + band.h) as usize {
            let start = row * width as usize + x;
            pixels[start..start + w].fill(0xff00_0000);
        }
    }
    unsafe { libc::munmap(ptr, bytes) };
    let pool = unsafe {
        (c.marshal)(
            rt.shm as Proxy,
            0,
            c.pool as *const c_void,
            1,
            0,
            std::ptr::null_mut::<c_void>(),
            fd,
            bytes as c_int,
        )
    };
    unsafe { libc::close(fd) };
    if pool.is_null() {
        return None;
    }
    let buffer = unsafe {
        (c.marshal)(
            pool,
            0,
            c.buffer as *const c_void,
            1,
            0,
            std::ptr::null_mut::<c_void>(),
            0i32,
            width,
            height,
            width * 4,
            0u32,
        )
    };
    // wl_shm_pool.destroy is opcode 1, while the new buffer retains the pool.
    unsafe { (c.marshal)(pool, 1, std::ptr::null(), 1, DESTROY) };
    (!buffer.is_null()).then_some(buffer)
}

unsafe fn create_mask(c: &Client, rt: &Runtime, parent: Proxy, size: (i32, i32)) -> Option<Mask> {
    let surface = unsafe {
        (c.marshal)(
            rt.compositor as Proxy,
            0,
            c.surface as *const c_void,
            1,
            0,
            std::ptr::null_mut::<c_void>(),
        )
    };
    if surface.is_null() {
        return None;
    }
    let subsurface = unsafe {
        (c.marshal)(
            rt.subcompositor as Proxy,
            1,
            c.subsurface as *const c_void,
            1,
            0,
            std::ptr::null_mut::<c_void>(),
            surface,
            parent,
        )
    };
    if subsurface.is_null() {
        return None;
    }
    // Do not call set_position: that would become effective only with a
    // parent commit and a guest which stopped swapping would never show it.
    unsafe { (c.marshal)(subsurface, 5, std::ptr::null(), 1, 0) };
    let region = unsafe {
        (c.marshal)(
            rt.compositor as Proxy,
            1,
            c.region as *const c_void,
            1,
            0,
            std::ptr::null_mut::<c_void>(),
        )
    };
    if region.is_null() {
        return None;
    }
    // An empty region makes the entire overlay input-transparent.
    unsafe { (c.marshal)(surface, 5, std::ptr::null(), 1, 0, region) };
    unsafe { (c.marshal)(region, 0, std::ptr::null(), 1, DESTROY) };
    Some(Mask {
        parent: parent as usize,
        surface: surface as usize,
        subsurface: subsurface as usize,
        buffer: 0,
        size,
        clip: Rect::default(),
    })
}

/// Apply the *current front-buffer* boundary without issuing guest GL calls.
/// The rectangle is in Wayland surface-local logical coordinates (not backing
/// pixels). Called from AppKit's SDL/Wayland event thread.
pub fn update(window: Window, clip: Option<Rect>) -> bool {
    let Some((display, parent)) = video::wayland_surface(window) else {
        return false;
    };
    let Some(size) = video::window_size(window) else {
        return false;
    };
    let Some(c) = client() else {
        return false;
    };
    let mut locked = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let state = locked.get_or_insert_with(State::default);
    let clip = clip.unwrap_or(Rect {
        x: 0,
        y: 0,
        w: size.0,
        h: size.1,
    });
    let covers_whole = clip.complements(size).next().is_none();
    let exists = state.masks.contains_key(&(window as usize));
    if !exists && covers_whole {
        return true;
    }
    if state.runtime.is_none() {
        state.runtime = unsafe { Runtime::initialize(c, display) };
    }
    let Some(rt) = &state.runtime else {
        return false;
    };
    if rt.display != display as usize {
        return false;
    }
    if !exists {
        let Some(mask) = (unsafe { create_mask(c, rt, parent, size) }) else {
            return false;
        };
        state.masks.insert(window as usize, mask);
    }
    let rt = state.runtime.as_ref().expect("just bound Wayland globals");
    let mask = state
        .masks
        .get_mut(&(window as usize))
        .expect("mask inserted");
    if mask.clip == clip && mask.size == size {
        return true;
    }
    if mask.parent != parent as usize {
        return false;
    }
    // A constant (0,0) child surface never needs a parent-synchronized
    // set_position. Updating this child buffer becomes visible on its own.
    let buffer = if covers_whole {
        None
    } else {
        unsafe { black_buffer(c, rt, size.0, size.1, clip) }
    };
    if !covers_whole && buffer.is_none() {
        return false;
    }
    let next = buffer.unwrap_or(std::ptr::null_mut());
    unsafe {
        (c.marshal)(
            mask.surface as Proxy,
            1,
            std::ptr::null(),
            1,
            0,
            next,
            0i32,
            0i32,
        );
        (c.marshal)(
            mask.surface as Proxy,
            2,
            std::ptr::null(),
            1,
            0,
            0i32,
            0i32,
            size.0,
            size.1,
        );
        (c.marshal)(mask.surface as Proxy, 6, std::ptr::null(), 1, 0);
        if mask.buffer != 0 {
            (c.marshal)(mask.buffer as Proxy, 0, std::ptr::null(), 1, DESTROY);
        }
    }
    mask.buffer = next as usize;
    mask.clip = clip;
    mask.size = size;
    // Drop the shm formats and buffer releases nothing here listens for.
    unsafe { (c.dispatch_queue_pending)(display, rt.queue as *mut c_void) };
    unsafe { (c.flush)(display) >= 0 }
}

/// Remove child surfaces while the SDL-owned parent is still alive.
pub fn forget(window: Window) {
    let mut locked = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(state) = locked.as_mut() else {
        return;
    };
    let Some(mask) = state.masks.remove(&(window as usize)) else {
        return;
    };
    let Some(c) = client() else {
        return;
    };
    unsafe {
        if mask.subsurface != 0 {
            (c.marshal)(mask.subsurface as Proxy, 0, std::ptr::null(), 1, DESTROY);
        }
        if mask.surface != 0 {
            (c.marshal)(mask.surface as Proxy, 0, std::ptr::null(), 1, DESTROY);
        }
        if mask.buffer != 0 {
            (c.marshal)(mask.buffer as Proxy, 0, std::ptr::null(), 1, DESTROY);
        }
    }
    if let Some(rt) = &state.runtime {
        unsafe { (c.flush)(rt.display as Proxy) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// An SDL-owned wl_surface receives an input-transparent black/clear
    /// subsurface and stays alive across mask expansion/removal. The Weston
    /// compositor must accept the actual wl_shm and subsurface requests.
    #[test]
    #[ignore = "requires nested Weston with SDL_VIDEODRIVER=wayland"]
    fn idle_opengl_wayland_mask_survives_real_compositor_roundtrips() {
        assert!(video::init_video());
        assert!(video::is_wayland(), "required Wayland video driver");
        let window = video::create_window(c"Wayland SDL masking", 400, 300, video::flags::OPENGL)
            .expect("SDL window");
        video::show_window(window);
        for _ in 0..30 {
            while video::poll().is_some() {}
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let pixel_test = std::env::var_os("ROSEMARY_WAYLAND_MASK_PIXELS").is_some();
        let image_size = (1440usize, 900usize); // nested Weston X11 test fixture
        let screenshot = || {
            let filename = std::env::temp_dir()
                .join(format!("rosemary-wayland-mask-{}.png", std::process::id()));
            let capture = std::process::Command::new("import")
                .args(["-window", "root"])
                .arg(&filename)
                .status()
                .expect("ImageMagick import");
            assert!(capture.success());
            let rgb = std::process::Command::new("magick")
                .arg(&filename)
                .args(["-depth", "8", "RGB:-"])
                .output()
                .expect("ImageMagick magick");
            let _ = std::fs::remove_file(filename);
            assert!(rgb.status.success());
            assert_eq!(rgb.stdout.len(), image_size.0 * image_size.1 * 3);
            rgb.stdout
        };
        let sample = |pixels: &[u8], x: usize, y: usize| {
            let at = (y * image_size.0 + x) * 3;
            [pixels[at], pixels[at + 1], pixels[at + 2]]
        };
        let mut red_bounds = None;
        let mut gl = std::ptr::null_mut();
        if pixel_test {
            gl = video::gl_create_context(window).expect("Wayland OpenGL context");
            assert!(video::gl_make_current(window, gl));
            let color = video::gl_proc_address(c"glClearColor").expect("glClearColor");
            let color: unsafe extern "C" fn(f32, f32, f32, f32) =
                unsafe { std::mem::transmute(color) };
            let clear = video::gl_proc_address(c"glClear").expect("glClear");
            let clear: unsafe extern "C" fn(u32) = unsafe { std::mem::transmute(clear) };
            let mut found = None;
            for _ in 0..8 {
                unsafe {
                    color(1.0, 0.0, 0.0, 1.0);
                    clear(0x4000)
                };
                video::swap_window(window);
                while video::poll().is_some() {}
                std::thread::sleep(std::time::Duration::from_millis(80));
                let original = screenshot();
                let mut bounds = (usize::MAX, usize::MAX, 0usize, 0usize, 0usize);
                for (i, pixel) in original.as_chunks::<3>().0.iter().enumerate() {
                    if *pixel == [255, 0, 0] {
                        let x = i % image_size.0;
                        let y = i / image_size.0;
                        bounds.0 = bounds.0.min(x);
                        bounds.1 = bounds.1.min(y);
                        bounds.2 = bounds.2.max(x);
                        bounds.3 = bounds.3.max(y);
                        bounds.4 += 1;
                    }
                }
                if bounds.4 > 10000 && bounds.3.saturating_sub(bounds.1) > 80 {
                    found = Some(bounds);
                    break;
                }
            }
            let bounds = found.expect("Wayland GLX/EGL front buffer never became visibly red");
            eprintln!(
                "WAYLAND_MASK_PIXELS red bounds={bounds:?} SDL={:?}",
                video::window_size(window)
            );
            red_bounds = Some(bounds);
        }
        let (display, _) = video::wayland_surface(window).expect("native wl_surface");
        let (sw, sh) = video::window_size(window).unwrap();
        assert!(
            update(
                window,
                Some(Rect {
                    x: 1,
                    y: 0,
                    w: sw - 32,
                    h: sh - 31,
                })
            ),
            "create real wl_shm children"
        );
        let c = client().expect("Wayland client");
        assert!(
            unsafe { (c.roundtrip)(display) } >= 0,
            "Wayland rejected masked subsurfaces"
        );
        if let Some((minx, miny, maxx, maxy, _)) = red_bounds {
            // Only the host-side mask updates here: never call glClear or
            // SDL_GL_SwapWindow between the screenshots.
            std::thread::sleep(std::time::Duration::from_millis(90));
            let masked = screenshot();
            // Sample away from the X11 cursor, which Weston draws over the
            // centre of its nested output in automated Xvfb sessions.
            let x = minx + (maxx - minx) / 3;
            assert_eq!(
                sample(&masked, x, (miny + maxy) / 2),
                [255, 0, 0],
                "interior GL pixels vanished"
            );
            assert_eq!(
                sample(&masked, x, maxy - 5),
                [0, 0, 0],
                "idle Wayland bottom edge escaped"
            );
            assert_eq!(
                sample(&masked, maxx - 5, (miny + maxy) / 3),
                [0, 0, 0],
                "idle Wayland right edge escaped"
            );
            eprintln!("WAYLAND_MASK_PIXELS idle masked bottom/right, retained interior");
        }
        assert!(
            update(
                window,
                Some(Rect {
                    x: 2,
                    y: 3,
                    w: sw - 32,
                    h: sh - 35,
                })
            ),
            "adjust masks"
        );
        assert!(unsafe { (c.roundtrip)(display) } >= 0);
        if let Some((minx, miny, maxx, maxy, _)) = red_bounds {
            // Same *physical* window and same guest front buffer; only the
            // logical inset changed, including both the top and left edges.
            std::thread::sleep(std::time::Duration::from_millis(80));
            let moved = screenshot();
            let x = minx + (maxx - minx) / 3;
            assert_eq!(
                sample(&moved, x, miny + 1),
                [0, 0, 0],
                "moved top boundary waited for guest swap"
            );
            assert_eq!(
                sample(&moved, minx + 1, miny + 100),
                [0, 0, 0],
                "moved left boundary waited for guest swap"
            );
            assert_eq!(sample(&moved, minx + 8, miny + 100), [255, 0, 0]);
            assert_eq!(sample(&moved, x, maxy - 5), [0, 0, 0]);
        }
        assert!(update(window, None), "remove black bands on restoration");
        assert!(unsafe { (c.roundtrip)(display) } >= 0);
        if pixel_test {
            assert!(video::gl_make_current(window, gl));
            let color: unsafe extern "C" fn(f32, f32, f32, f32) =
                unsafe { std::mem::transmute(video::gl_proc_address(c"glClearColor").unwrap()) };
            let clear: unsafe extern "C" fn(u32) =
                unsafe { std::mem::transmute(video::gl_proc_address(c"glClear").unwrap()) };
            unsafe {
                color(1.0, 0.0, 0.0, 1.0);
                clear(0x4000)
            };
            video::swap_window(window);
            std::thread::sleep(std::time::Duration::from_millis(100));
            let restored = screenshot();
            let (minx, miny, maxx, maxy, _) = red_bounds.unwrap();
            assert_eq!(
                sample(&restored, (minx + maxx) / 2, maxy - 5),
                [255, 0, 0],
                "restoring Wayland logical frame retained blackout"
            );
            assert_eq!(sample(&restored, maxx - 1, (miny + maxy) / 2), [255, 0, 0]);
        }
        forget(window);
        assert!(unsafe { (c.roundtrip)(display) } >= 0, "remove children");
        if pixel_test {
            video::gl_make_current(window, std::ptr::null_mut());
            video::gl_destroy_context(gl);
        }
        video::destroy_window(window);
    }

    static SDL_QUEUE_DISPATCHED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    unsafe extern "C" fn sync_done(_: *mut c_void, _: Proxy, _: u32) {
        SDL_QUEUE_DISPATCHED.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// The mask binds its globals without dispatching SDL's default queue:
    /// SDL's listeners run only from SDL's own pump, on SDL's thread.
    #[test]
    #[ignore = "requires Weston with SDL_VIDEODRIVER=wayland"]
    fn binding_the_mask_globals_leaves_sdl_events_queued() {
        assert!(video::init_video());
        assert!(video::is_wayland(), "required Wayland video driver");
        let window = video::create_window(c"Wayland mask queue", 400, 300, video::flags::OPENGL)
            .expect("SDL window");
        video::show_window(window);
        for _ in 0..30 {
            while video::poll().is_some() {}
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let (display, _) = video::wayland_surface(window).expect("native wl_surface");
        let c = client().expect("Wayland client");
        // A wl_display.sync on the default queue stands in for SDL's events.
        static LISTENER: [unsafe extern "C" fn(*mut c_void, Proxy, u32); 1] = [sync_done];
        let callback_interface = unsafe {
            let handle = libc::dlopen(
                c"libwayland-client.so.0".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_NOLOAD,
            );
            libc::dlsym(handle, c"wl_callback_interface".as_ptr())
        };
        assert!(!callback_interface.is_null());
        let callback = unsafe {
            (c.marshal)(
                display,
                0,
                callback_interface,
                1,
                0,
                std::ptr::null_mut::<c_void>(),
            )
        };
        assert!(!callback.is_null());
        assert_eq!(
            unsafe { (c.add_listener)(callback, LISTENER.as_ptr().cast(), std::ptr::null_mut()) },
            0
        );
        let (sw, sh) = video::window_size(window).unwrap();
        assert!(update(
            window,
            Some(Rect {
                x: 0,
                y: 31,
                w: sw,
                h: sh - 31,
            })
        ));
        assert!(
            !SDL_QUEUE_DISPATCHED.load(std::sync::atomic::Ordering::SeqCst),
            "the mask dispatched SDL's default queue"
        );
        forget(window);
        unsafe { (c.roundtrip)(display) };
        assert!(SDL_QUEUE_DISPATCHED.load(std::sync::atomic::Ordering::SeqCst));
        unsafe { (c.proxy_destroy)(callback) };
        video::destroy_window(window);
    }

    #[test]
    fn four_bands_cover_exactly_the_excluded_wayland_pixels() {
        for clip in [
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
        ] {
            let shapes: Vec<Rect> = clip.complements((10, 8)).collect();
            for y in 0..8 {
                for x in 0..10 {
                    let hits = shapes
                        .iter()
                        .filter(|b| x >= b.x && y >= b.y && x < b.x + b.w && y < b.y + b.h)
                        .count();
                    let outside =
                        x < clip.x || y < clip.y || x >= clip.x + clip.w || y >= clip.y + clip.h;
                    assert_eq!(hits > 0, outside, "{x},{y} in {clip:?}");
                    assert!(hits <= 1);
                }
            }
        }
    }
}
