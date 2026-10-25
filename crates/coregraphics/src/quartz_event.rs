//! The Quartz event services: `CGEvent`, its posting, and the input counters.

#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::OnceLock;

use corefoundation::base::CFTypeRef;
use objc::class::Class;

use crate::CGPoint;

/// `CGEventType`, the ones this module acts on.
mod kind {
    pub const NULL: u32 = 0;
    pub const LEFT_MOUSE_DOWN: u32 = 1;
    pub const LEFT_MOUSE_UP: u32 = 2;
    pub const RIGHT_MOUSE_DOWN: u32 = 3;
    pub const RIGHT_MOUSE_UP: u32 = 4;
    pub const MOUSE_MOVED: u32 = 5;
    pub const LEFT_MOUSE_DRAGGED: u32 = 6;
    pub const RIGHT_MOUSE_DRAGGED: u32 = 7;
    pub const OTHER_MOUSE_DOWN: u32 = 25;
    pub const OTHER_MOUSE_UP: u32 = 26;
    pub const OTHER_MOUSE_DRAGGED: u32 = 27;
    pub const KEY_DOWN: u32 = 10;
    pub const KEY_UP: u32 = 11;

    /// Whether posting this event means "put the pointer here".
    pub fn moves_the_pointer(kind: u32) -> bool {
        matches!(
            kind,
            MOUSE_MOVED | LEFT_MOUSE_DRAGGED | RIGHT_MOUSE_DRAGGED | OTHER_MOUSE_DRAGGED
        )
    }

    /// Whether this event is a button the runtime would have to synthesise.
    pub fn presses_a_button(kind: u32) -> bool {
        matches!(
            kind,
            LEFT_MOUSE_DOWN
                | LEFT_MOUSE_UP
                | RIGHT_MOUSE_DOWN
                | RIGHT_MOUSE_UP
                | OTHER_MOUSE_DOWN
                | OTHER_MOUSE_UP
        )
    }
}

/// What AppKit provides to the Quartz event services: where the pointer is,
/// how to move it, and the keyboard's modifiers.
pub trait Provider: Send + Sync {
    /// The pointer in Quartz's global coordinates, or `None` when the host has
    /// no answer.
    fn pointer(&self) -> Option<(f32, f32)>;

    /// Put the pointer at `point`, in Quartz's global coordinates.
    fn warp_pointer(&self, point: CGPoint);

    /// The keyboard's modifiers, as `NSEvent`'s modifier bits.
    fn modifiers(&self) -> u64;
}

static PROVIDER: OnceLock<Box<dyn Provider>> = OnceLock::new();

/// Install the provider AppKit answers pointer and modifier queries with.
pub fn set_provider(provider: Box<dyn Provider>) {
    let _ = PROVIDER.set(provider);
}

#[repr(C)]
struct Body {
    isa: *const Class,
    /// Where the event says the pointer is, in Quartz's global coordinates.
    location: CGPoint,
    /// `CGEventType`.
    kind: u32,
    /// `CGEventFlags`, which share their bits with `NSEventModifierFlags`.
    flags: u64,
    /// `kCGKeyboardEventKeycode`: the virtual key, for a keyboard event.
    key_code: u16,
}

fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        let class =
            corefoundation::base::register(c"__NSCFType_CGEvent", None, size_of::<Body>(), true);
        objc::class::ptr(class) as usize
    });
    // SAFETY: the cell holds a pointer to the class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

/// An event object the caller owns and releases with `CFRelease`.
fn new(location: CGPoint, kind: u32) -> CFTypeRef {
    new_with(location, kind, 0, 0)
}

fn new_with(location: CGPoint, kind: u32, flags: u64, key_code: u16) -> CFTypeRef {
    // SAFETY: `Body` begins with `isa` and is the layout `class` was registered
    // with.
    let body: *mut Body = unsafe { corefoundation::base::allocate(class()) };
    if body.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a live instance whose remaining bytes are ours to fill in.
    unsafe {
        (*body).location = location;
        (*body).kind = kind;
        (*body).flags = flags;
        (*body).key_code = key_code;
    }
    body as CFTypeRef
}

/// The event's body, for anything this module made.
fn body_of<'a>(event: CFTypeRef) -> Option<&'a mut Body> {
    // SAFETY: null or an object this runtime allocated.
    if !unsafe { corefoundation::base::is_kind_of(event, class()) } {
        return None;
    }
    // SAFETY: an instance of `class()` is a `Body`.
    Some(unsafe { &mut *(event as *mut Body) })
}

/// A CoreGraphics event describing an `NSEvent`, autoreleased: what
/// `-[NSEvent CGEvent]` hands back, which lives as long as the event does.
pub fn describing(kind: u64, flags: u64, key_code: u16, location: CGPoint) -> CFTypeRef {
    let event = new_with(location, kind as u32, flags, key_code);
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(event) }
}

/// What `+[NSEvent eventWithCGEvent:]` reads: the type, flags, key and place.
pub fn parts(event: CFTypeRef) -> Option<(u64, u64, u16, CGPoint)> {
    body_of(event).map(|b| (u64::from(b.kind), b.flags, b.key_code, b.location))
}

/// `CGEventRef CGEventCreateKeyboardEvent(CGEventSourceRef, CGKeyCode, bool)`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGEventCreateKeyboardEvent(
    _source: *const c_void,
    key: u16,
    down: bool,
) -> CFTypeRef {
    let kind = match down {
        true => kind::KEY_DOWN,
        false => kind::KEY_UP,
    };
    new_with(global_pointer(), kind, 0, key)
}

/// `CGEventFlags CGEventGetFlags(CGEventRef)`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGEventGetFlags(event: CFTypeRef) -> u64 {
    body_of(event).map_or(0, |b| b.flags)
}

/// `void CGEventSetFlags(CGEventRef, CGEventFlags)`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGEventSetFlags(event: CFTypeRef, flags: u64) {
    if let Some(body) = body_of(event) {
        body.flags = flags;
    }
}

/// `CGEventGetType`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGEventGetType(event: CFTypeRef) -> u32 {
    body_of(event).map_or(kind::NULL, |b| b.kind)
}

/// `int64_t CGEventGetIntegerValueField(CGEventRef, CGEventField)`: the key
/// code; every other field this runtime does not track reads as zero, which
/// is what a fresh event of that type holds.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGEventGetIntegerValueField(event: CFTypeRef, field: u32) -> i64 {
    // `kCGKeyboardEventKeycode`.
    const KEYCODE: u32 = 9;
    match (body_of(event), field) {
        (Some(body), KEYCODE) => i64::from(body.key_code),
        _ => 0,
    }
}

/// `void CGEventSetIntegerValueField(CGEventRef, CGEventField, int64_t)`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGEventSetIntegerValueField(event: CFTypeRef, field: u32, value: i64) {
    if let (Some(body), 9) = (body_of(event), field) {
        body.key_code = value as u16;
    }
}

/// Put the pointer at `point`, in Quartz's global coordinates.
pub fn warp_pointer(point: CGPoint) {
    if let Some(provider) = PROVIDER.get() {
        provider.warp_pointer(point);
    }
}

/// Where the pointer is, in Quartz's global coordinates.
///
/// The same two exceptions `+[NSEvent mouseLocation]` makes, and for the same
/// reason: a scripted pointer and a disassociated one are both places the
/// window system's own cursor is not, and the two answers must not disagree.
pub fn global_pointer() -> CGPoint {
    let (x, y) = PROVIDER
        .get()
        .and_then(|provider| provider.pointer())
        .unwrap_or((0.0, 0.0));
    CGPoint {
        x: x as f64,
        y: y as f64,
    }
}

/// `CGEventRef CGEventCreate(CGEventSourceRef)`.
///
/// A null source means "describe the current input state", which is the only
/// way this is used: the event it hands back carries where the pointer is now.
/// A non-null source names a tap to take the state from, and this runtime has
/// one input state - so the answer is the same either way.
pub unsafe extern "C" fn CGEventCreate(_source: *const c_void) -> CFTypeRef {
    new(global_pointer(), kind::NULL)
}

/// `CGPoint CGEventGetLocation(CGEventRef)`.
///
/// Zero for anything that is not an event this module made, which is what
/// CoreGraphics answers for a null event.
pub unsafe extern "C" fn CGEventGetLocation(event: CFTypeRef) -> CGPoint {
    // SAFETY: null or an object this runtime allocated.
    if !unsafe { corefoundation::base::is_kind_of(event, class()) } {
        return CGPoint::default();
    }
    // SAFETY: an instance of `class()` is a `Body`.
    unsafe { (*(event as *const Body)).location }
}

/// `CGEventRef CGEventCreateMouseEvent(CGEventSourceRef, CGEventType, CGPoint,
/// CGMouseButton)`.
///
/// The button is recorded nowhere: the only kind of event this runtime can act
/// on when it is posted is one that moves the pointer, and which button is held
/// while it moves changes nothing about where it lands.
pub unsafe extern "C" fn CGEventCreateMouseEvent(
    _source: *const c_void,
    kind: u32,
    location: CGPoint,
    _button: u32,
) -> CFTypeRef {
    new(location, kind)
}

/// `void CGEventSetType(CGEventRef, CGEventType)`.
pub unsafe extern "C" fn CGEventSetType(event: CFTypeRef, kind: u32) {
    // SAFETY: null or an object this runtime allocated.
    if !unsafe { corefoundation::base::is_kind_of(event, class()) } {
        return;
    }
    // SAFETY: an instance of `class()` is a `Body`.
    unsafe { (*(event as *mut Body)).kind = kind };
}

/// `void CGEventPost(CGEventTapLocation, CGEventRef)`.
///
/// A posted mouse-moved event is how a program that has hidden the cursor puts
/// it back where it wants it - the same request `CGWarpMouseCursorPosition`
/// makes, so it is answered by the same code and inherits its handling of
/// relative mouse mode.
///
/// A posted *button* is not delivered. Quartz would put it into the window
/// server's stream, where it reaches every application and not only this one;
/// what this runtime has instead is SDL's queue, which is fed by the window
/// system and is not writable. Said once rather than ignored, because a program
/// that synthesises a click and never sees one has no other way to find out.
pub unsafe extern "C" fn CGEventPost(_tap: u32, event: CFTypeRef) {
    // SAFETY: null or an object this runtime allocated.
    if !unsafe { corefoundation::base::is_kind_of(event, class()) } {
        return;
    }
    // SAFETY: an instance of `class()` is a `Body`.
    let body = unsafe { &*(event as *const Body) };
    if kind::moves_the_pointer(body.kind) {
        if let Some(provider) = PROVIDER.get() {
            provider.warp_pointer(body.location);
        }
        return;
    }
    if kind::presses_a_button(body.kind) {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            eprintln!(
                "rosemary: coregraphics: CGEventPost: this program synthesises mouse button \
                 events, which this runtime cannot deliver - its event queue is the window \
                 system's and is not writable"
            );
        });
    }
}

/// Pointer motion since `CGGetLastMouseDelta` last read it, in whole points.
static DELTA: std::sync::Mutex<(f32, f32)> = std::sync::Mutex::new((0.0, 0.0));

/// Record a host motion event's relative movement.
pub fn note_mouse_delta(dx: f32, dy: f32) {
    let mut delta = DELTA.lock().unwrap_or_else(|e| e.into_inner());
    delta.0 += dx;
    delta.1 += dy;
}

/// `void CGGetLastMouseDelta(int32_t *deltaX, int32_t *deltaY)`
///
/// CoreGraphics answers with the last event's movement. A game reads it once
/// a frame to turn a locked cursor into camera motion, and the host delivers
/// several events a frame, so the movement since the last read is what it
/// gets here - the same total, without losing all but one event's share. The
/// fraction that does not make a whole point is kept for the next read.
///
/// # Safety
///
/// Both pointers are null or writable.
pub unsafe extern "C" fn CGGetLastMouseDelta(dx: *mut i32, dy: *mut i32) {
    let mut delta = DELTA.lock().unwrap_or_else(|e| e.into_inner());
    let (x, y) = (delta.0.trunc(), delta.1.trunc());
    delta.0 -= x;
    delta.1 -= y;
    // SAFETY: the caller's out-parameters.
    unsafe {
        if !dx.is_null() {
            *dx = x as i32;
        }
        if !dy.is_null() {
            *dy = y as i32;
        }
    }
}

/// Input events delivered so far, by `CGEventType` - which for every input
/// event is the same number as its `NSEventType`.
static COUNTS: [std::sync::atomic::AtomicU32; 32] =
    [const { std::sync::atomic::AtomicU32::new(0) }; 32];

/// Note one delivered event of `kind`, for [`CGEventSourceCounterForEventType`].
pub fn count(kind: u64) {
    if let Some(counter) = COUNTS.get(kind as usize) {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// `uint32_t CGEventSourceCounterForEventType(CGEventSourceStateID, CGEventType)`
///
/// How many events of a type have arrived, which a program samples twice to
/// learn whether the user did anything in between. `kCGAnyInputEventType` is
/// the sum of every type.
pub extern "C" fn CGEventSourceCounterForEventType(_state: u32, kind: u32) -> u32 {
    /// `kCGAnyInputEventType`.
    const ANY_INPUT: u32 = u32::MAX;
    let load = |c: &std::sync::atomic::AtomicU32| c.load(std::sync::atomic::Ordering::Relaxed);
    match kind {
        ANY_INPUT => COUNTS.iter().map(load).fold(0, u32::wrapping_add),
        _ => COUNTS.get(kind as usize).map_or(0, load),
    }
}

/// `CGError CGSetLocalEventsSuppressionInterval(CFTimeInterval)`.
///
/// How long real mouse input is ignored after a warp. `CGWarpMouseCursorPosition`
/// never suppresses anything, which is the interval games set - zero, so mouse
/// look keeps its deltas. Any other interval is reported once rather than kept.
pub unsafe extern "C" fn CGSetLocalEventsSuppressionInterval(seconds: f64) -> i32 {
    static TOLD: std::sync::Once = std::sync::Once::new();
    if seconds != 0.0 {
        TOLD.call_once(|| {
            eprintln!(
                "rosemary: coregraphics: CGSetLocalEventsSuppressionInterval({seconds}) is not \
                 honoured; input is never suppressed after a warp"
            )
        });
    }
    0
}

/// `bool CGEventSourceKeyState(CGEventSourceStateID, CGKeyCode)`.
///
/// False: the key state a caller asks about here is the window server's, and
/// there is no window server to ask. SDL's keyboard state is this process's own
/// view and answers a different question.
pub unsafe extern "C" fn CGEventSourceKeyState(_source: i32, _key: u16) -> u8 {
    0
}

/// `CFMachPortRef CGEventTapCreate(CGEventTapLocation, CGEventTapPlacement,
/// CGEventTapOptions, CGEventMask, CGEventTapCallBack, void *)`
///
/// NULL, which is what Apple's answers to a process not trusted for
/// accessibility: no other process's input passes through this one.
pub extern "C" fn CGEventTapCreate(
    _tap: u32,
    _place: u32,
    _options: u32,
    _mask: u64,
    _callback: *const c_void,
    _info: *mut c_void,
) -> *mut c_void {
    std::ptr::null_mut()
}

/// The keyboard's modifiers now. The flags are NSEvent's modifier bits, which
/// is how AppKit defines them, so the same translation answers both.
pub extern "C" fn CGEventSourceFlagsState(_state: u32) -> u64 {
    PROVIDER.get().map_or(0, |provider| provider.modifiers())
}

exports::exports! {
    functions {
        "CGEventCreate" => CGEventCreate,
        "CGEventGetLocation" => CGEventGetLocation,
        "CGEventCreateMouseEvent" => CGEventCreateMouseEvent,
        "CGEventSetType" => CGEventSetType,
        "CGEventGetType" => CGEventGetType,
        "CGEventCreateKeyboardEvent" => CGEventCreateKeyboardEvent,
        "CGEventGetFlags" => CGEventGetFlags,
        "CGEventSetFlags" => CGEventSetFlags,
        "CGEventGetIntegerValueField" => CGEventGetIntegerValueField,
        "CGEventSetIntegerValueField" => CGEventSetIntegerValueField,
        "CGEventPost" => CGEventPost,
        "CGGetLastMouseDelta" => CGGetLastMouseDelta,
        "CGEventSourceFlagsState" => CGEventSourceFlagsState,
        "CGEventSourceCounterForEventType" => CGEventSourceCounterForEventType,
        "CGSetLocalEventsSuppressionInterval" => CGSetLocalEventsSuppressionInterval,
        "CGEventSourceKeyState" => CGEventSourceKeyState,
        "CGEventTapCreate" => CGEventTapCreate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_constructors_record_the_key_and_direction_without_modifiers() {
        // SAFETY: a null source is supported; each owned event is released once.
        unsafe {
            for (down, kind) in [(true, kind::KEY_DOWN), (false, kind::KEY_UP)] {
                let event = CGEventCreateKeyboardEvent(std::ptr::null(), 42, down);
                assert!(!event.is_null());
                assert_eq!(CGEventGetType(event), kind);
                assert_eq!(CGEventGetFlags(event), 0);
                assert_eq!(CGEventGetIntegerValueField(event, 9), 42);
                corefoundation::api::CFRelease(event);
            }
        }
    }

    #[test]
    fn keyboard_fields_can_be_changed_without_affecting_the_location() {
        let location = CGPoint { x: 12.5, y: -7.0 };
        let event = new_with(location, kind::KEY_DOWN, 0, 42);
        let flags = (1u64 << 63) | (1 << 20) | (1 << 17);
        // SAFETY: a live event owned by this test.
        unsafe {
            CGEventSetFlags(event, flags);
            CGEventSetIntegerValueField(event, 9, 123);
            CGEventSetType(event, kind::KEY_UP);
            assert_eq!(CGEventGetFlags(event), flags);
            assert_eq!(CGEventGetIntegerValueField(event, 9), 123);
            assert_eq!(CGEventGetType(event), kind::KEY_UP);
            assert_eq!(CGEventGetLocation(event), location);
            assert_eq!(parts(event), Some((11, flags, 123, location)));
            CGEventSetIntegerValueField(event, 8, 99);
            assert_eq!(CGEventGetIntegerValueField(event, 8), 0);
            assert_eq!(CGEventGetIntegerValueField(event, 9), 123);
            corefoundation::api::CFRelease(event);
        }
    }

    #[test]
    fn integer_key_fields_use_the_virtual_keys_sixteen_bits() {
        let event = new_with(CGPoint::default(), kind::KEY_DOWN, 0, 0);
        // SAFETY: a live event owned by this test.
        unsafe {
            for (value, expected) in [(-1, 65535), (65536, 0), (65537, 1)] {
                CGEventSetIntegerValueField(event, 9, value);
                assert_eq!(CGEventGetIntegerValueField(event, 9), expected);
            }
            corefoundation::api::CFRelease(event);
        }
    }

    #[test]
    fn describing_round_trips_appkit_fields_and_autoreleases_the_event() {
        let pool = objc::arc::pool_push();
        let location = CGPoint { x: -10.0, y: 25.5 };
        let event = describing(10, 1 << 20, 12, location);
        assert!(!event.is_null());
        assert_eq!(parts(event), Some((10, 1 << 20, 12, location)));
        // SAFETY: retain the event across draining the pool that owns its release.
        unsafe { corefoundation::api::CFRetain(event) };
        objc::arc::pool_pop(pool);
        // SAFETY: the retained reference is still live and is released once.
        unsafe {
            assert_eq!(corefoundation::api::CFGetRetainCount(event), 1);
            assert_eq!(CGEventGetIntegerValueField(event, 9), 12);
            corefoundation::api::CFRelease(event);
        }
    }

    #[test]
    fn keyboard_accessors_reject_null_and_non_events() {
        crate::bootstrap();
        // SAFETY: null or a live color space, neither of which is an event.
        unsafe {
            let space = crate::cgcolor::CGColorSpaceCreateDeviceRGB();
            for event in [std::ptr::null(), space] {
                assert_eq!(parts(event), None);
                assert_eq!(CGEventGetType(event), kind::NULL);
                assert_eq!(CGEventGetFlags(event), 0);
                assert_eq!(CGEventGetIntegerValueField(event, 9), 0);
                CGEventSetFlags(event, u64::MAX);
                CGEventSetIntegerValueField(event, 9, 42);
                CGEventSetType(event, kind::KEY_DOWN);
                assert_eq!(parts(event), None);
            }
            crate::cgcolor::CGColorSpaceRelease(space);
        }
    }

    #[test]
    fn the_keyboard_event_calls_are_exported() {
        let names = crate::names();
        for name in [
            "CGEventCreateKeyboardEvent",
            "CGEventGetType",
            "CGEventGetFlags",
            "CGEventSetFlags",
            "CGEventGetIntegerValueField",
            "CGEventSetIntegerValueField",
        ] {
            assert!(names.contains(&name), "{name}");
        }
    }
}
