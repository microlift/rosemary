use std::ffi::c_void;

use foundation::geometry::{CGFloat, NSPoint};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

/// `NSEventType`, the values a guest compares `-type` against.
pub mod kind {
    pub const LEFT_MOUSE_DOWN: u64 = 1;
    pub const LEFT_MOUSE_UP: u64 = 2;
    pub const RIGHT_MOUSE_DOWN: u64 = 3;
    pub const RIGHT_MOUSE_UP: u64 = 4;
    pub const MOUSE_MOVED: u64 = 5;
    pub const LEFT_MOUSE_DRAGGED: u64 = 6;
    pub const RIGHT_MOUSE_DRAGGED: u64 = 7;
    pub const MOUSE_ENTERED: u64 = 8;
    pub const MOUSE_EXITED: u64 = 9;
    pub const KEY_DOWN: u64 = 10;
    pub const KEY_UP: u64 = 11;
    pub const FLAGS_CHANGED: u64 = 12;
    pub const CURSOR_UPDATE: u64 = 17;
    pub const SCROLL_WHEEL: u64 = 22;
    pub const OTHER_MOUSE_DOWN: u64 = 25;
    pub const OTHER_MOUSE_UP: u64 = 26;
    pub const OTHER_MOUSE_DRAGGED: u64 = 27;
}

#[repr(C)]
struct Event {
    isa: *const c_void,
    kind: u64,
    modifiers: u64,
    location: NSPoint,
    delta: NSPoint,
    /// The window the event is in, unretained: an event lives for one dispatch
    /// and the window that made it outlives that.
    window: Id,
    /// `-characters`, kept as an object so `-length` and `-characterAtIndex:`
    /// answer without this module knowing what a string is. Owned: a key event
    /// can sit in the application's queue past the pool it was made in.
    characters: Id,
    key_code: u16,
    button: i64,
    clicks: i64,
    repeat: bool,
    subtype: i16,
    data1: isize,
    data2: isize,
    /// `-trackingArea`, unretained like `window`: the view that holds the area
    /// outlives the dispatch the event is made for.
    tracking_area: Id,
    /// For a host key down, the text the host resolved for it, owned and
    /// taken once by whichever responder interprets the key. See
    /// [`take_text`].
    text: Id,
}

pub const INSTANCE_SIZE: usize = size_of::<Event>();

/// The event a receiver is, or `None` for anything else.
///
/// # Safety
///
/// `event` must be null or an object this runtime allocated.
unsafe fn event_of<'a>(event: Id) -> Option<&'a Event> {
    let is_event = unsafe { objc::object::class_of(event) }.is_some_and(|c| {
        objc::class::lookup(c"NSEvent").is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    });
    // SAFETY: the check above says the storage is this module's.
    is_event.then(|| unsafe { &*(event as *const Event) })
}

/// Read one field, or its zero value for a receiver that is not an event.
///
/// A macro because the alternative is twelve near-identical five-line functions,
/// which is the case where a near-copy hides a wrong field.
macro_rules! reader {
    ($($name:ident -> $ty:ty { $read:expr } ),* $(,)?) => {
        $(
            unsafe extern "C" fn $name(receiver: Id, _sel: Sel) -> $ty {
                // SAFETY: the caller sends to a live object.
                let read: fn(&Event) -> $ty = $read;
                unsafe { event_of(receiver) }.map_or(<$ty>::default(), read)
            }
        )*
    };
}

reader! {
    kind -> u64 { |e| e.kind },
    modifier_flags -> u64 { |e| e.modifiers },
    location_in_window -> NSPoint { |e| e.location },
    key_code -> u16 { |e| e.key_code },
    button_number -> i64 { |e| e.button },
    click_count -> i64 { |e| e.clicks },
    delta_x -> CGFloat { |e| e.delta.x },
    delta_y -> CGFloat { |e| e.delta.y },
    characters -> Id { |e| e.characters },
    window -> Id { |e| e.window },
    is_a_repeat -> u8 { |e| e.repeat as u8 },
    subtype -> i16 { |e| e.subtype },
    data1 -> isize { |e| e.data1 },
    data2 -> isize { |e| e.data2 },
    tracking_area -> Id { |e| e.tracking_area },
}

/// The scroll accessors, which a program reads on every wheel event.
///
/// All four describe a *trackpad*, and this runtime has none: SDL reports a
/// wheel, which is a discrete notch from a device with no fingers on it. So the
/// answers are the ones AppKit gives for a mouse wheel, and each of them
/// matters to a caller - `-hasPreciseScrollingDeltas` says the deltas are lines
/// rather than points, and the two phases say the gesture is not one AppKit is
/// tracking, so nothing waits for an end that never comes.
unsafe extern "C" fn no_phase(_receiver: Id, _sel: Sel) -> u64 {
    // `NSEventPhaseNone`.
    0
}

unsafe extern "C" fn no(_receiver: Id, _sel: Sel) -> u8 {
    0
}

unsafe extern "C" fn zero(_receiver: Id, _sel: Sel) -> isize {
    0
}

/// `-_eventRef`: the Carbon `EventRef` behind the event. AppKit answers NULL
/// for an event no Carbon event backs, and none does here (`ReceiveNextEvent`
/// never yields one), so callers get the NULL they already have to handle.
unsafe extern "C" fn event_ref(_receiver: Id, _sel: Sel) -> *mut c_void {
    std::ptr::null_mut()
}

unsafe extern "C" fn zero_double(_receiver: Id, _sel: Sel) -> CGFloat {
    0.0
}

/// `-pressure`: full for a button that is down, none for one that is not.
///
/// A wheel or a moved-mouse event has no button, and a program that scaled
/// something by the pressure of a click would get zero for every click if this
/// answered zero throughout.
unsafe extern "C" fn pressure(receiver: Id, _sel: Sel) -> f32 {
    // SAFETY: the caller sends to a live object.
    let down = unsafe { event_of(receiver) }.is_some_and(|e| {
        matches!(
            e.kind,
            kind::LEFT_MOUSE_DOWN
                | kind::RIGHT_MOUSE_DOWN
                | kind::OTHER_MOUSE_DOWN
                | kind::LEFT_MOUSE_DRAGGED
                | kind::RIGHT_MOUSE_DRAGGED
                | kind::OTHER_MOUSE_DRAGGED
        )
    });
    f32::from(u8::from(down))
}

/// `-windowNumber`, from the window the event was built for, so an event and
/// `+windowNumberAtPoint:` agree about which window is meant.
unsafe extern "C" fn window_number(receiver: Id, _sel: Sel) -> isize {
    // SAFETY: the caller sends to a live object.
    let window = unsafe { event_of(receiver) }.map_or(std::ptr::null_mut(), |e| e.window);
    if window.is_null() {
        return 0;
    }
    let sel = objc::sel::get("windowNumber");
    // SAFETY: `-windowNumber` is `NSInteger (id, SEL)` on a window this crate
    // made.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> isize = std::mem::transmute(objc::msg::entry());
        f(window, sel)
    }
}

/// `-timestamp`: seconds since the machine started, on the clock
/// `mach_absolute_time` reads - which is what Apple's is. Programs compare it
/// with their own reading of that clock: Unity's input system times its events
/// against it, and treats an event from a different epoch as days old.
unsafe extern "C" fn timestamp(_receiver: Id, _sel: Sel) -> CGFloat {
    darwin::mach::media_time()
}

/// `+[NSEvent doubleClickInterval]`, in seconds.
///
/// SDL counts the clicks this runtime hands on as `-clickCount`, so the answer
/// has to be SDL's window rather than the host desktop's preference: a guest
/// that does its own timing - a view that treats two presses inside the interval
/// as one gesture - must agree with the count it is given, and SDL's is half a
/// second unless `SDL_MOUSE_DOUBLE_CLICK_TIME` says otherwise. Nothing here sets
/// that hint.
unsafe extern "C" fn double_click_interval(_class: Id, _sel: Sel) -> CGFloat {
    0.5
}

/// `+[NSEvent modifierFlags]`, which asks about the keyboard *now* rather than
/// about an event. SDL is the only thing here that knows.
unsafe extern "C" fn current_modifier_flags(_class: Id, _sel: Sel) -> u64 {
    crate::keymap::modifiers(sdl3::video::keyboard_modifiers())
}

/// What an event is made of, named rather than positional: eleven fields of
/// which most are zero for any one event, and a builder is how a caller says
/// which.
#[derive(Clone, Default)]
pub struct Fields {
    pub kind: u64,
    pub modifiers: u64,
    pub location: NSPoint,
    pub delta: NSPoint,
    pub window: Id,
    pub characters: Id,
    pub key_code: u16,
    pub button: i64,
    pub clicks: i64,
    pub repeat: bool,
    pub subtype: i16,
    pub data1: isize,
    pub data2: isize,
    pub tracking_area: Id,
}

/// Build an autoreleased event.
///
/// Autoreleased because it is handed to one responder method and never stored:
/// a guest that keeps it retains it, and the pool the pump runs under is what
/// frees the rest.
pub fn make(fields: Fields) -> Id {
    let class = objc::class::lookup(c"NSEvent").expect("declared before its methods");
    // SAFETY: `Event` is the layout the class was registered with.
    let event = unsafe { corefoundation::base::allocate::<Event>(class) };
    // SAFETY: freshly allocated; every field is written before it is read.
    unsafe {
        (*event).kind = fields.kind;
        (*event).modifiers = fields.modifiers;
        (*event).location = fields.location;
        (*event).delta = fields.delta;
        (*event).window = fields.window;
        (*event).characters = objc::arc::retain(fields.characters);
        (*event).key_code = fields.key_code;
        (*event).button = fields.button;
        (*event).clicks = fields.clicks;
        (*event).repeat = fields.repeat;
        (*event).subtype = fields.subtype;
        (*event).data1 = fields.data1;
        (*event).data2 = fields.data2;
        (*event).tracking_area = fields.tracking_area;
        (*event).text = std::ptr::null_mut();
    }
    objc::arc::autorelease(event as Id)
}

/// `-dealloc`: the strings are the event's own.
unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let event = receiver as *mut Event;
    // SAFETY: `-dealloc` is sent once, to an event of this module's layout,
    // with the last reference gone.
    unsafe {
        objc::arc::release((*event).characters);
        objc::arc::release((*event).text);
    }
    objc::arc::finalize(receiver);
}

/// Attach the text the host resolved for a key down to its event.
pub(crate) fn attach_text(event: Id, text: &str) {
    // SAFETY: null or a live object this runtime allocated.
    if unsafe { event_of(event) }.is_none() || text.is_empty() {
        return;
    }
    let event = event as *mut Event;
    // SAFETY: an event of this module's layout, checked above.
    unsafe {
        objc::arc::release((*event).text);
        (*event).text = foundation::string::from_str(text);
    }
}

/// Take the text attached to a key down, once.
///
/// Once because AppKit commits a keystroke's text to one input client: a
/// second `-interpretKeyEvents:` with the same event must not type it again.
pub(crate) fn take_text(event: Id) -> Option<String> {
    // SAFETY: null or a live object this runtime allocated.
    unsafe { event_of(event) }?;
    let event = event as *mut Event;
    // SAFETY: an event of this module's layout, checked above.
    let text = unsafe { std::mem::replace(&mut (*event).text, std::ptr::null_mut()) };
    if text.is_null() {
        return None;
    }
    // SAFETY: an `NSString` `attach_text` made.
    let value = String::from_utf8_lossy(&unsafe { foundation::string::bytes(text) }).into_owned();
    objc::arc::release(text);
    Some(value)
}

/// An event's type, for the window's own routing.
///
/// Zero for anything that is not an event, which no selector matches - so a
/// stray object routes nowhere rather than to whatever `mouseDown:` would do.
pub fn kind_of(event: Id) -> u64 {
    // SAFETY: null or a live object this runtime allocated.
    unsafe { event_of(event) }.map_or(0, |e| e.kind)
}

/// The fields an event was built from, or `None` for anything else.
pub fn fields_of(event: Id) -> Option<Fields> {
    // SAFETY: null or a live object this runtime allocated.
    unsafe { event_of(event) }.map(|e| Fields {
        kind: e.kind,
        modifiers: e.modifiers,
        location: e.location,
        delta: e.delta,
        window: e.window,
        characters: e.characters,
        key_code: e.key_code,
        button: e.button,
        clicks: e.clicks,
        repeat: e.repeat,
        subtype: e.subtype,
        data1: e.data1,
        data2: e.data2,
        tracking_area: e.tracking_area,
    })
}

/// The tracking area an enter, exit or cursor-update event is about, or null.
pub fn tracking_area_of(event: Id) -> Id {
    // SAFETY: null or a live object this runtime allocated.
    unsafe { event_of(event) }.map_or(std::ptr::null_mut(), |e| e.tracking_area)
}

/// `-userData`: for a tracking-area event, the `userInfo` the area was made
/// with, which is what AppKit hands back.
unsafe extern "C" fn user_data(receiver: Id, _sel: Sel) -> *mut c_void {
    crate::cursor::tracking_user_info_of(tracking_area_of(receiver))
}

/// Where the event happened, in its window's coordinates.
pub fn location_of(event: Id) -> NSPoint {
    unsafe { event_of(event) }.map_or(NSPoint::default(), |e| e.location)
}

/// `-copyWithZone:`. An event is immutable, so its copy is the same object
/// with one more owner - and the unretained window and tracking area it names
/// stay as valid as they were in the original.
unsafe extern "C" fn copy_with_zone(receiver: Id, _sel: Sel, _zone: *mut c_void) -> Id {
    objc::arc::retain(receiver)
}

/// `+[NSEvent mouseLocation]`: the pointer in screen coordinates, asked of the
/// window system rather than of the last event - which is the difference
/// between this and `-locationInWindow`, and the reason a program uses it.
unsafe extern "C" fn mouse_location(_class: Id, _sel: Sel) -> NSPoint {
    // A scripted pointer is the exception: nothing moved the compositor's
    // cursor, so the window system would answer where a person left it and
    // disagree with the events the guest has just been given.
    if let Some((x, y)) = crate::script::pointer() {
        return crate::geometry::point_from_sdl(x as f64, y as f64);
    }
    // A disassociated pointer is the other exception, and for the same reason
    // in reverse: the window system's cursor is still moving and the guest's is
    // not - see `event::frozen_pointer`.
    if let Some((x, y)) = crate::event::frozen_pointer() {
        return crate::geometry::point_from_sdl(x as f64, y as f64);
    }
    match crate::window::desktop_mouse_state() {
        Some((x, y, _)) => crate::geometry::point_from_sdl(x as f64, y as f64),
        None => NSPoint::default(),
    }
}

/// `+[NSEvent pressedMouseButtons]`: the buttons held right now, asked of the
/// window system for the same reason `+mouseLocation` is.
unsafe extern "C" fn pressed_mouse_buttons(_class: Id, _sel: Sel) -> u64 {
    sdl3::video::global_mouse_state().map_or(0, |(_, _, mask)| crate::button::pressed_mask(mask))
}

/// `+[NSEvent enterExitEventWithType:location:modifierFlags:timestamp:
/// windowNumber:context:eventNumber:trackingNumber:userData:]`.
///
/// The tracking metadata has no accessor used by this runtime. The event's
/// window, type, location and modifiers are the parts Qt reads and routes.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn enter_exit_event(
    _class: Id,
    _sel: Sel,
    kind: u64,
    location: NSPoint,
    modifiers: u64,
    _timestamp: CGFloat,
    window_number: isize,
    _context: Id,
    _event_number: isize,
    _tracking_number: isize,
    _user_data: *mut c_void,
) -> Id {
    let window = crate::window::all()
        .into_iter()
        .find(|window| {
            let sel = objc::sel::get("windowNumber");
            // SAFETY: every entry is a live `NSWindow`, and this is
            // `NSInteger (id, SEL)`.
            let number = unsafe {
                let f: unsafe extern "C" fn(Id, Sel) -> isize =
                    std::mem::transmute(objc::msg::entry());
                f(*window, sel)
            };
            number == window_number
        })
        .unwrap_or(std::ptr::null_mut());
    make(Fields {
        kind,
        modifiers,
        location,
        window,
        ..Fields::default()
    })
}

/// `+[NSEvent otherEventWithType:location:modifierFlags:timestamp:
/// windowNumber:context:subtype:data1:data2:]`.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn other_event(
    class: Id,
    sel: Sel,
    kind: u64,
    location: NSPoint,
    modifiers: u64,
    timestamp: CGFloat,
    window_number: isize,
    context: Id,
    subtype: i16,
    data1: isize,
    data2: isize,
) -> Id {
    // Use the same window lookup as enter/exit events, then add the fields
    // specific to application-defined events.
    let event = unsafe {
        enter_exit_event(
            class,
            sel,
            kind,
            location,
            modifiers,
            timestamp,
            window_number,
            context,
            0,
            0,
            std::ptr::null_mut(),
        )
    };
    if !event.is_null() {
        // SAFETY: `make` returned this module's allocation; the autorelease
        // pool owns it but has not drained during this call.
        let event = event as *mut Event;
        unsafe {
            (*event).subtype = subtype;
            (*event).data1 = data1;
            (*event).data2 = data2;
        }
    }
    event
}

/// `-CGEvent`: a CoreGraphics event with this one's type, flags and key.
unsafe extern "C" fn cg_event(receiver: Id, _sel: Sel) -> *const c_void {
    // SAFETY: null or a live object this runtime allocated.
    let Some(event) = (unsafe { event_of(receiver) }) else {
        return std::ptr::null();
    };
    crate::cgevent::describing(event.kind, event.modifiers, event.key_code, event.location)
}

/// The text a key types with `flags` held, from the keyboard layout: Shift,
/// Option and Caps Lock change it, Command and Control do not.
fn typed_characters(key_code: u16, flags: u64) -> String {
    use crate::keymap::flags as f;
    use crate::uchr::modifier as m;
    let mut modifiers = 0u32;
    for (flag, bit) in [
        (f::SHIFT, m::SHIFT),
        (f::OPTION, m::OPTION),
        (f::CAPS_LOCK, m::ALPHA_LOCK),
    ] {
        if flags & flag != 0 {
            modifiers |= u32::from(bit);
        }
    }
    let mut state = 0;
    // SAFETY: this crate's own layout resource; kind 40 is the ANSI keyboard.
    let typed = unsafe {
        crate::uchr::translate(
            crate::uchr::layout().as_ptr(),
            key_code,
            0,
            modifiers,
            40,
            crate::uchr::NO_DEAD_KEYS,
            &mut state,
        )
    }
    .map(|chars| String::from_utf16_lossy(&chars))
    .unwrap_or_default();
    // A function key has no layout entry; it types AppKit's private-use scalar.
    if typed.is_empty()
        && let Some(scancode) = crate::keymap::scancode_of(key_code)
    {
        return crate::keymap::characters(scancode, 0);
    }
    typed
}

/// `+eventWithCGEvent:`.
unsafe extern "C" fn event_with_cg_event(_class: Id, _sel: Sel, cg: *const c_void) -> Id {
    let Some((kind, flags, key_code, location)) = crate::cgevent::parts(cg as _) else {
        return std::ptr::null_mut();
    };
    let characters = match kind {
        10 | 11 => objc::arc::autorelease(foundation::string::from_str(&typed_characters(
            key_code, flags,
        ))),
        _ => std::ptr::null_mut(),
    };
    make(Fields {
        kind,
        modifiers: flags,
        location,
        characters,
        key_code,
        ..Fields::default()
    })
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSEvent").expect("declared before its methods");
    crate::define::class_methods!(class, {
        "modifierFlags" => current_modifier_flags, c"Q16@0:8";
        "mouseLocation" => mouse_location, c"{CGPoint=dd}16@0:8";
        "pressedMouseButtons" => pressed_mouse_buttons, c"Q16@0:8";
        "eventWithCGEvent:" => event_with_cg_event, c"@24@0:8^{__CGEvent=}16";
        "doubleClickInterval" => double_click_interval, c"d16@0:8";
        "enterExitEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:trackingNumber:userData:"
            => enter_exit_event, c"@96@0:8Q16{CGPoint=dd}24Q40d48q56@64q72q80^v88";
        "otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:"
            => other_event, c"@96@0:8Q16{CGPoint=dd}24Q40d48q56@64s72q80q88";
    });
    methods!(class, {
        "type" => kind, c"Q16@0:8";
        "CGEvent" => cg_event, c"^{__CGEvent=}16@0:8";
        "modifierFlags" => modifier_flags, c"Q16@0:8";
        "locationInWindow" => location_in_window, c"{CGPoint=dd}16@0:8";
        "keyCode" => key_code, c"S16@0:8";
        "buttonNumber" => button_number, c"q16@0:8";
        "clickCount" => click_count, c"q16@0:8";
        "deltaX" => delta_x, c"d16@0:8";
        "deltaY" => delta_y, c"d16@0:8";
        "scrollingDeltaX" => delta_x, c"d16@0:8";
        "scrollingDeltaY" => delta_y, c"d16@0:8";
        "characters" => characters, c"@16@0:8";
        "charactersIgnoringModifiers" => characters, c"@16@0:8";
        "window" => window, c"@16@0:8";
        "isARepeat" => is_a_repeat, c"c16@0:8";
        "phase" => no_phase, c"Q16@0:8";
        "momentumPhase" => no_phase, c"Q16@0:8";
        "hasPreciseScrollingDeltas" => no, c"c16@0:8";
        "isDirectionInvertedFromDevice" => no, c"c16@0:8";
        "pressure" => pressure, c"f16@0:8";
        "windowNumber" => window_number, c"q16@0:8";
        // An event number counts *tracking* sequences on a Mac, and nothing
        // here tracks one; zero is what an event outside any sequence carries.
        "eventNumber" => zero, c"q16@0:8";
        // `NSEventSubtype`, which for a mouse or key event is zero.
        "subtype" => subtype, c"s16@0:8";
        "data1" => data1, c"q16@0:8";
        "data2" => data2, c"q16@0:8";
        "trackingArea" => tracking_area, c"@16@0:8";
        "userData" => user_data, c"^v16@0:8";
        "_eventRef" => event_ref, c"^v16@0:8";
        // `-magnification` is the pinch gesture's, and SDL delivers no pinch.
        "magnification" => zero_double, c"d16@0:8";
        "deltaZ" => zero_double, c"d16@0:8";
        "timestamp" => timestamp, c"d16@0:8";
        "copyWithZone:" => copy_with_zone, c"@24@0:8^v16";
        "dealloc" => dealloc, c"v16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_reports_what_it_was_built_with() {
        crate::bootstrap();
        let event = make(Fields {
            kind: kind::KEY_DOWN,
            key_code: 0x7E,
            modifiers: crate::keymap::flags::SHIFT,
            location: NSPoint { x: 3.0, y: 4.0 },
            ..Fields::default()
        });
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            assert_eq!(super::kind(event, objc::sel::get("type")), kind::KEY_DOWN);
            assert_eq!(super::key_code(event, objc::sel::get("keyCode")), 0x7E);
            assert_eq!(
                super::location_in_window(event, objc::sel::get("locationInWindow")),
                NSPoint { x: 3.0, y: 4.0 }
            );
        }
    }

    /// A key down's host text is typed once, by whichever responder takes it
    /// first, however many times the event is interpreted.
    #[test]
    fn a_key_downs_text_is_taken_once() {
        crate::bootstrap();
        let event = make(Fields {
            kind: kind::KEY_DOWN,
            ..Fields::default()
        });
        attach_text(event, "a");
        assert_eq!(take_text(event).as_deref(), Some("a"));
        assert_eq!(take_text(event), None);
    }

    /// An event is immutable, so its copy is itself with one more owner.
    #[test]
    fn an_event_copies_to_itself() {
        crate::bootstrap();
        let event = make(Fields::default());
        let before = objc::arc::retain_count(event);
        // SAFETY: `-copy` is `id (id, SEL)`, inherited from `NSObject`.
        let copy = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(event, objc::sel::get("copy"))
        };
        assert_eq!(copy, event);
        assert_eq!(objc::arc::retain_count(event), before + 1);
        objc::arc::release(copy);
    }

    /// No Carbon event backs an event here, so `-_eventRef` is NULL for every
    /// kind, and it is dispatched through the runtime rather than called.
    #[test]
    fn an_event_has_no_carbon_event_ref() {
        crate::bootstrap();
        for kind in [kind::KEY_DOWN, kind::LEFT_MOUSE_DOWN, kind::SCROLL_WHEEL] {
            let event = make(Fields {
                kind,
                ..Fields::default()
            });
            // SAFETY: `-_eventRef` is `void *(id, SEL)`.
            let reference = unsafe {
                let f: unsafe extern "C" fn(Id, Sel) -> *mut c_void =
                    std::mem::transmute(objc::msg::entry());
                f(event, objc::sel::get("_eventRef"))
            };
            assert!(reference.is_null());
        }
    }

    /// A receiver that is not an event answers zero rather than reading a field
    /// out of whatever it is.
    #[test]
    fn a_non_event_reads_as_zero() {
        crate::bootstrap();
        let string = foundation::string::from_str("not an event");
        // SAFETY: `-type` is `NSUInteger (id, SEL)`.
        assert_eq!(unsafe { super::kind(string, objc::sel::get("type")) }, 0);
    }
}
