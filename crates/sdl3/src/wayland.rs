//! Left-button presses on SDL's Wayland surfaces, seen through a `wl_pointer`
//! of this crate's own.
//!
//! A press in a region SDL's hit test calls draggable goes straight to the
//! compositor as an interactive move, and SDL reports nothing of it - so a
//! double click on a client-side titlebar would never be seen. Every
//! `wl_pointer` of a seat gets the seat's events, so a second one on SDL's
//! display sees the presses SDL keeps to itself. Its events arrive on the
//! display's default queue, which SDL dispatches while pumping events.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::{Mutex, OnceLock};

/// What a pointer did that SDL may not report.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerEvent {
    /// The left button went down.
    Press {
        /// The `wl_surface` it landed on.
        surface: usize,
        /// Surface-local, which is SDL's window coordinates.
        x: f32,
        y: f32,
        /// The compositor's timestamp, in milliseconds.
        time: u32,
    },
    /// The left button came up over one of the client's surfaces. A
    /// compositor moving a window keeps this to itself.
    Release,
    /// The pointer came onto `surface`, which is also how a compositor hands
    /// the pointer back after moving a window.
    Enter { surface: usize },
}

#[allow(non_snake_case)]
struct Client {
    wl_proxy_marshal_flags:
        unsafe extern "C" fn(*mut c_void, u32, *const c_void, u32, u32, ...) -> *mut c_void,
    wl_proxy_add_listener: unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> c_int,
    wl_proxy_get_version: unsafe extern "C" fn(*mut c_void) -> u32,
    wl_registry_interface: *const c_void,
    wl_seat_interface: *const c_void,
    wl_pointer_interface: *const c_void,
}

// SAFETY: function pointers and pointers to libwayland's static interface
// descriptions.
unsafe impl Send for Client {}
unsafe impl Sync for Client {}

fn client() -> Option<&'static Client> {
    static CLIENT: OnceLock<Option<Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            // SAFETY: a NUL-terminated soname. SDL has it loaded already when
            // it runs on Wayland; never closed, as with every other `dlopen`
            // in this crate.
            let handle = unsafe {
                libc::dlopen(
                    c"libwayland-client.so.0".as_ptr(),
                    libc::RTLD_LAZY | libc::RTLD_GLOBAL,
                )
            };
            if handle.is_null() {
                return None;
            }
            let symbol = |name: &CStr| {
                // SAFETY: a handle from `dlopen` and a NUL-terminated name.
                let found = unsafe { libc::dlsym(handle, name.as_ptr()) };
                (!found.is_null()).then_some(found)
            };
            // SAFETY: each is the prototype wayland-client.h declares.
            unsafe {
                Some(Client {
                    wl_proxy_marshal_flags: std::mem::transmute::<
                        *mut c_void,
                        unsafe extern "C" fn(
                            *mut c_void,
                            u32,
                            *const c_void,
                            u32,
                            u32,
                            ...
                        ) -> *mut c_void,
                    >(symbol(
                        c"wl_proxy_marshal_flags",
                    )?),
                    wl_proxy_add_listener: std::mem::transmute::<
                        *mut c_void,
                        unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> c_int,
                    >(symbol(c"wl_proxy_add_listener")?),
                    wl_proxy_get_version: std::mem::transmute::<
                        *mut c_void,
                        unsafe extern "C" fn(*mut c_void) -> u32,
                    >(symbol(c"wl_proxy_get_version")?),
                    wl_registry_interface: symbol(c"wl_registry_interface")?,
                    wl_seat_interface: symbol(c"wl_seat_interface")?,
                    wl_pointer_interface: symbol(c"wl_pointer_interface")?,
                })
            }
        })
        .as_ref()
}

const WL_DISPLAY_GET_REGISTRY: u32 = 1;
const WL_REGISTRY_BIND: u32 = 0;
const WL_SEAT_GET_POINTER: u32 = 0;
const WL_SEAT_CAPABILITY_POINTER: u32 = 1;
const BTN_LEFT: u32 = 0x110;
const PRESSED: u32 = 1;

#[derive(Default)]
struct State {
    /// The surface each of this crate's pointers is over, by pointer.
    focus: Vec<(usize, usize, f32, f32)>,
    /// Seats already given a pointer.
    seats: Vec<usize>,
    events: Vec<PointerEvent>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> T {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(state.get_or_insert_with(State::default))
}

/// Start watching `display`'s pointers. Only the first display is watched:
/// SDL has one.
pub fn watch(display: *mut c_void) {
    static WATCHED: OnceLock<()> = OnceLock::new();
    if display.is_null() || WATCHED.get().is_some() {
        return;
    }
    WATCHED.get_or_init(|| ());
    let Some(client) = client() else {
        return;
    };
    // SAFETY: SDL's live display; `wl_display_get_registry` spelled as the
    // header's inline wrapper spells it.
    unsafe {
        let registry = (client.wl_proxy_marshal_flags)(
            display,
            WL_DISPLAY_GET_REGISTRY,
            client.wl_registry_interface,
            (client.wl_proxy_get_version)(display),
            0,
            std::ptr::null_mut::<c_void>(),
        );
        if registry.is_null() {
            return;
        }
        (client.wl_proxy_add_listener)(
            registry,
            (&raw const REGISTRY_LISTENER).cast(),
            std::ptr::null_mut(),
        );
    }
}

/// The events seen since the last call, oldest first.
pub fn take_events() -> Vec<PointerEvent> {
    with_state(|state| std::mem::take(&mut state.events))
}

fn record(state: &mut State, event: PointerEvent) {
    // Only the latest few matter: a double click and what follows it.
    if state.events.len() >= 16 {
        state.events.remove(0);
    }
    state.events.push(event);
}

type Proxy = *mut c_void;

/// `wl_registry_listener`.
#[repr(C)]
struct RegistryListener {
    global: unsafe extern "C" fn(*mut c_void, Proxy, u32, *const c_char, u32),
    global_remove: unsafe extern "C" fn(*mut c_void, Proxy, u32),
}

/// `wl_seat_listener`, as far as version 1.
#[repr(C)]
struct SeatListener {
    capabilities: unsafe extern "C" fn(*mut c_void, Proxy, u32),
}

/// `wl_pointer_listener`, as far as version 1.
#[repr(C)]
struct PointerListener {
    enter: unsafe extern "C" fn(*mut c_void, Proxy, u32, Proxy, i32, i32),
    leave: unsafe extern "C" fn(*mut c_void, Proxy, u32, Proxy),
    motion: unsafe extern "C" fn(*mut c_void, Proxy, u32, i32, i32),
    button: unsafe extern "C" fn(*mut c_void, Proxy, u32, u32, u32, u32),
    axis: unsafe extern "C" fn(*mut c_void, Proxy, u32, u32, i32),
}

static REGISTRY_LISTENER: RegistryListener = RegistryListener {
    global,
    global_remove,
};
static SEAT_LISTENER: SeatListener = SeatListener { capabilities };
static POINTER_LISTENER: PointerListener = PointerListener {
    enter,
    leave,
    motion,
    button,
    axis,
};

unsafe extern "C" fn global(
    _data: *mut c_void,
    registry: *mut c_void,
    name: u32,
    interface: *const c_char,
    _version: u32,
) {
    let Some(client) = client() else {
        return;
    };
    // SAFETY: libwayland passes the interface's NUL-terminated name.
    if interface.is_null() || unsafe { CStr::from_ptr(interface) } != c"wl_seat" {
        return;
    }
    // Version 1 has every event this needs and the fewest to listen for.
    let version = 1u32;
    // SAFETY: `wl_registry_bind` as the header's inline wrapper spells it;
    // the interface's first field is its name.
    unsafe {
        let interface_name = *(client.wl_seat_interface as *const *const c_char);
        let seat = (client.wl_proxy_marshal_flags)(
            registry,
            WL_REGISTRY_BIND,
            client.wl_seat_interface,
            version,
            0,
            name,
            interface_name,
            version,
            std::ptr::null_mut::<c_void>(),
        );
        if !seat.is_null() {
            (client.wl_proxy_add_listener)(
                seat,
                (&raw const SEAT_LISTENER).cast(),
                std::ptr::null_mut(),
            );
        }
    }
}

unsafe extern "C" fn global_remove(_data: *mut c_void, _registry: *mut c_void, _name: u32) {}

unsafe extern "C" fn capabilities(_data: *mut c_void, seat: *mut c_void, capabilities: u32) {
    let Some(client) = client() else {
        return;
    };
    // A seat that loses its pointer keeps the proxy, inert; one that gains it
    // back is not given a second.
    if capabilities & WL_SEAT_CAPABILITY_POINTER == 0
        || with_state(|state| {
            let known = state.seats.contains(&(seat as usize));
            state.seats.push(seat as usize);
            known
        })
    {
        return;
    }
    // SAFETY: `wl_seat_get_pointer` as the header's inline wrapper spells it.
    unsafe {
        let pointer = (client.wl_proxy_marshal_flags)(
            seat,
            WL_SEAT_GET_POINTER,
            client.wl_pointer_interface,
            (client.wl_proxy_get_version)(seat),
            0,
            std::ptr::null_mut::<c_void>(),
        );
        if !pointer.is_null() {
            (client.wl_proxy_add_listener)(
                pointer,
                (&raw const POINTER_LISTENER).cast(),
                std::ptr::null_mut(),
            );
        }
    }
}

fn fixed(value: i32) -> f32 {
    value as f32 / 256.0
}

fn set_focus(pointer: *mut c_void, surface: usize, x: f32, y: f32) {
    with_state(|state| {
        state.focus.retain(|(p, ..)| *p != pointer as usize);
        if surface != 0 {
            state.focus.push((pointer as usize, surface, x, y));
        }
    });
}

unsafe extern "C" fn enter(
    _data: *mut c_void,
    pointer: *mut c_void,
    _serial: u32,
    surface: *mut c_void,
    x: i32,
    y: i32,
) {
    set_focus(pointer, surface as usize, fixed(x), fixed(y));
    with_state(|state| {
        record(
            state,
            PointerEvent::Enter {
                surface: surface as usize,
            },
        )
    });
}

unsafe extern "C" fn leave(
    _data: *mut c_void,
    pointer: *mut c_void,
    _serial: u32,
    _surface: *mut c_void,
) {
    set_focus(pointer, 0, 0.0, 0.0);
}

unsafe extern "C" fn motion(_data: *mut c_void, pointer: *mut c_void, _time: u32, x: i32, y: i32) {
    with_state(|state| {
        if let Some(focus) = state.focus.iter_mut().find(|f| f.0 == pointer as usize) {
            (focus.2, focus.3) = (fixed(x), fixed(y));
        }
    });
}

unsafe extern "C" fn button(
    _data: *mut c_void,
    pointer: *mut c_void,
    _serial: u32,
    time: u32,
    button: u32,
    state: u32,
) {
    if button != BTN_LEFT {
        return;
    }
    let pressed = state == PRESSED;
    with_state(|state| {
        if !pressed {
            record(state, PointerEvent::Release);
            return;
        }
        if let Some(&(_, surface, x, y)) = state.focus.iter().find(|f| f.0 == pointer as usize) {
            record(
                state,
                PointerEvent::Press {
                    surface,
                    x,
                    y,
                    time,
                },
            );
        }
    });
}

unsafe extern "C" fn axis(
    _data: *mut c_void,
    _pointer: *mut c_void,
    _time: u32,
    _axis: u32,
    _value: i32,
) {
}
