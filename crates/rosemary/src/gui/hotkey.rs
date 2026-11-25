use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use super::sys;

/// `kVK_F6` and `kVK_F7` from `<Carbon/HIToolbox/Events.h>`.
const VK_F6: u32 = 0x61;
const VK_F7: u32 = 0x62;
const VK_F8: u32 = 0x64;
const VK_F9: u32 = 0x65;
const VK_F10: u32 = 0x6d;

/// `shiftKey`, from the same header. Carbon's modifier constants are the old
/// Event Manager's and are not `NSEventModifierFlags`.
const SHIFT: u32 = 0x0200;

/// `kCGEventFlagMaskShift`, which is the *other* vocabulary, used by the poller.
const CG_SHIFT: u64 = 0x0002_0000;

/// `NSEventModifierFlagShift` and the other modifiers a hot key must not have,
/// from `<AppKit/NSEvent.h>`. Carbon matches a hot key's modifiers exactly, and
/// the event monitor does the same so both routes take the same keystrokes.
const NS_SHIFT: u64 = 1 << 17;
const NS_CONTROL: u64 = 1 << 18;
const NS_OPTION: u64 = 1 << 19;
const NS_COMMAND: u64 = 1 << 20;

/// `kCGEventSourceStateHIDSystemState`: the hardware's own state, rather than
/// the state as it was when some event was posted.
const HID_STATE: u32 = 1;

/// `kEventClassKeyboard` and `kEventHotKeyPressed`.
const CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const HOT_KEY_PRESSED: u32 = 5;

/// `kEventParamDirectObject` and `typeEventHotKeyID`.
const PARAM_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
const TYPE_HOT_KEY_ID: u32 = u32::from_be_bytes(*b"hkid");

/// The signature this runtime's hotkeys are registered under.
const SIGNATURE: u32 = u32::from_be_bytes(*b"ro3g");

/// Which of the two was pressed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// shift+F6: start or stop an Instruments recording.
    Trace,
    /// shift+F7: show or hide the statistics panel.
    Statistics,
    /// shift+F8: take the mouse for the panel, or give it back.
    Focus,
    /// shift+F9: start or stop counting region entries.
    Stats,
    /// shift+F10: start or stop sampling the guest.
    Profile,
}

impl Key {
    /// The identifier the Carbon event carries back.
    fn id(self) -> u32 {
        match self {
            Key::Trace => 6,
            Key::Statistics => 7,
            Key::Focus => 8,
            Key::Stats => 9,
            Key::Profile => 10,
        }
    }

    fn code(self) -> u32 {
        match self {
            Key::Trace => VK_F6,
            Key::Statistics => VK_F7,
            Key::Focus => VK_F8,
            Key::Stats => VK_F9,
            Key::Profile => VK_F10,
        }
    }

    /// The hot key a key-down event with `code` and `NSEventModifierFlags`
    /// `modifiers` is, if it is one.
    pub fn from_key_event(code: u16, modifiers: u64) -> Option<Key> {
        let held = modifiers & (NS_SHIFT | NS_CONTROL | NS_OPTION | NS_COMMAND);
        if held != NS_SHIFT {
            return None;
        }
        Key::ALL
            .into_iter()
            .find(|key| key.code() == u32::from(code))
    }

    fn from_id(id: u32) -> Option<Key> {
        match id {
            6 => Some(Key::Trace),
            7 => Some(Key::Statistics),
            8 => Some(Key::Focus),
            9 => Some(Key::Stats),
            10 => Some(Key::Profile),
            _ => None,
        }
    }

    /// Every key the overlay binds, which is the list both mechanisms walk.
    pub const ALL: [Key; 5] = [
        Key::Trace,
        Key::Statistics,
        Key::Focus,
        Key::Stats,
        Key::Profile,
    ];
}

/// `EventHotKeyID`.
#[repr(C)]
#[derive(Clone, Copy)]
struct HotKeyId {
    signature: u32,
    id: u32,
}

/// `EventTypeSpec`.
#[repr(C)]
struct EventType {
    class: u32,
    kind: u32,
}

type EventRef = *mut c_void;
type TargetRef = *mut c_void;

/// The Carbon handler.
extern "C" fn handler(_call: *mut c_void, event: EventRef, _user: *mut c_void) -> i32 {
    type GetParameter =
        unsafe extern "C" fn(EventRef, u32, u32, *mut u32, usize, *mut usize, *mut c_void) -> i32;
    // SAFETY: the prototype is `<Carbon/HIToolbox/CarbonEvents.h>`'s.
    let Some(get) = (unsafe { sys::function::<GetParameter>("GetEventParameter") }) else {
        return 0;
    };
    let mut id = HotKeyId {
        signature: 0,
        id: 0,
    };
    // SAFETY: `event` is the event Carbon dispatched, the parameter and type
    // names are the ones a hot-key event carries, and the destination is a live
    // local of exactly the size passed.
    let status = unsafe {
        get(
            event,
            PARAM_DIRECT_OBJECT,
            TYPE_HOT_KEY_ID,
            std::ptr::null_mut(),
            size_of::<HotKeyId>(),
            std::ptr::null_mut(),
            (&raw mut id).cast(),
        )
    };
    if status == 0
        && id.signature == SIGNATURE
        && let Some(key) = Key::from_id(id.id)
    {
        delivered("Carbon");
        super::pressed(key);
    }
    0
}

/// Register both hotkeys. Returns whether the event target took them.
fn register() -> bool {
    type GetTarget = unsafe extern "C" fn() -> TargetRef;
    type Install = unsafe extern "C" fn(
        TargetRef,
        extern "C" fn(*mut c_void, EventRef, *mut c_void) -> i32,
        usize,
        *const EventType,
        *mut c_void,
        *mut *mut c_void,
    ) -> i32;
    type Register =
        unsafe extern "C" fn(u32, u32, HotKeyId, TargetRef, u32, *mut *mut c_void) -> i32;

    // SAFETY: each prototype is transcribed from
    // `<Carbon/HIToolbox/CarbonEvents.h>`.
    let (Some(target), Some(install), Some(register)) = (unsafe {
        (
            sys::function::<GetTarget>("GetApplicationEventTarget"),
            sys::function::<Install>("InstallEventHandler"),
            sys::function::<Register>("RegisterEventHotKey"),
        )
    }) else {
        return false;
    };

    // SAFETY: the application's event target is a process-lifetime object and
    // the handler is an `extern "C"` function with the signature
    // `EventHandlerUPP` names.
    let target = unsafe { target() };
    if target.is_null() {
        return false;
    }
    let types = [EventType {
        class: CLASS_KEYBOARD,
        kind: HOT_KEY_PRESSED,
    }];
    // SAFETY: `types` is one live `EventTypeSpec` and the count says so. The
    // handler reference is discarded deliberately - nothing here ever removes
    // the handler, because the overlay lives as long as the process.
    let installed = unsafe {
        install(
            target,
            handler,
            types.len(),
            types.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if installed != 0 {
        return false;
    }

    let mut all = true;
    for key in Key::ALL {
        let id = HotKeyId {
            signature: SIGNATURE,
            id: key.id(),
        };
        // SAFETY: a virtual key code, Carbon's own modifier mask, a live
        // identifier passed by value, and the target above. The hot-key
        // reference is discarded for the reason the handler's is.
        let status = unsafe { register(key.code(), SHIFT, id, target, 0, std::ptr::null_mut()) };
        all &= status == 0;
    }
    all
}

/// The fallback: read the keys directly, thirty times a second.
fn poll() {
    type KeyState = unsafe extern "C" fn(u32, u16) -> u8;
    type FlagsState = unsafe extern "C" fn(u32) -> u64;
    // SAFETY: both prototypes are `<CoreGraphics/CGEventSource.h>`'s.
    let (Some(key_state), Some(flags_state)) = (unsafe {
        (
            sys::function::<KeyState>("CGEventSourceKeyState"),
            sys::function::<FlagsState>("CGEventSourceFlagsState"),
        )
    }) else {
        return;
    };

    let spawned = std::thread::Builder::new()
        .name("rosemary-gui-keys".into())
        .spawn(move || {
            let mut was = [false; Key::ALL.len()];
            loop {
                std::thread::sleep(std::time::Duration::from_millis(33));
                // SAFETY: both take a source-state identifier and, for the
                // first, a virtual key code.
                let shift = unsafe { flags_state(HID_STATE) } & CG_SHIFT != 0;
                for (slot, key) in Key::ALL.into_iter().enumerate() {
                    let down = shift && unsafe { key_state(HID_STATE, key.code() as u16) } != 0;
                    // The edge is tracked either way, so that going quiet
                    // because an event route is working does not leave a stale
                    // edge to fire the moment it stops.
                    let edge = down && !was[slot];
                    was[slot] = down;
                    if edge && !EVENTS_ARRIVE.load(Relaxed) {
                        super::pressed(key);
                    }
                }
            }
        });
    if spawned.is_err() {
        eprintln!("rosemary: gui  no hotkeys: the fallback poller would not start");
    }
}

/// Whether a hot key has ever arrived as an event, through Carbon or the
/// overlay's `NSEvent` monitor.
static EVENTS_ARRIVE: AtomicBool = AtomicBool::new(false);

/// Take the hotkeys, by every route this process has.
///
/// The third route, an `NSEvent` monitor, is [`super::input`]'s.
pub fn install() {
    let registered = register();
    // The builtin frameworks answer "up" for every key here: there is no window
    // server to ask.
    let polling = !crate::frameworks::using_builtin();
    // Always, not just on failure. See [`EVENTS_ARRIVE`].
    if polling {
        poll();
    }
    if super::debugging() {
        eprintln!(
            "rosemary: gui  RegisterEventHotKey {}; the key-state poller is {}",
            match registered {
                true => "accepted shift+F6 through shift+F10",
                false => "refused",
            },
            match polling {
                true => "also running",
                false => "not running",
            },
        );
    }
}

/// A hot key arrived as an event, so the poller can stop.
pub(crate) fn delivered(route: &str) {
    if !EVENTS_ARRIVE.swap(true, Relaxed) && super::debugging() {
        eprintln!("rosemary: gui  hotkeys are arriving through {route}; the poller is now quiet");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KVK_F7: u16 = VK_F7 as u16;

    #[test]
    fn a_key_event_is_a_hot_key_only_with_shift_alone() {
        assert!(matches!(
            Key::from_key_event(KVK_F7, NS_SHIFT),
            Some(Key::Statistics)
        ));
        assert!(Key::from_key_event(KVK_F7, 0).is_none());
        assert!(Key::from_key_event(KVK_F7, NS_SHIFT | NS_COMMAND).is_none());
        assert!(Key::from_key_event(0x00, NS_SHIFT).is_none());
    }
}
