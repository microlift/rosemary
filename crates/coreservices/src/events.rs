use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

type OSStatus = i32;

const NO_ERR: OSStatus = 0;
/// `eventLoopTimedOutErr`: nothing arrived before the timeout.
const EVENT_LOOP_TIMED_OUT_ERR: OSStatus = -9875;
/// `eventNotHandledErr`: no handler claimed the event.
const EVENT_NOT_HANDLED_ERR: OSStatus = -9874;
/// `eventParameterNotFoundErr`, which is also what Carbon answers for a
/// reference that names nothing.
const EVENT_INVALID_PARAMETER: OSStatus = -50;

/// The singletons a program asks for by name. Distinct non-null addresses, so
/// that a caller comparing two targets gets the right answer; the bytes behind
/// them are never read by anything here.
static APPLICATION_TARGET: u8 = 0;
static DISPATCHER_TARGET: u8 = 0;
static MAIN_EVENT_LOOP: u8 = 0;
/// [`GetCurrentEventQueue`]'s answer: one queue, since nothing here runs more
/// than the one cooperative main thread a Carbon event queue would otherwise
/// distinguish.
static EVENT_QUEUE: u8 = 0;

/// Handler and timer references handed out and not yet removed.
///
/// Kept so that removing one can say whether it was ever installed, which is
/// the only thing a caller can observe about a handler that is never called.
static INSTALLED: Mutex<Option<HashSet<u64>>> = Mutex::new(None);

/// References are odd-numbered and start high, because a program that stores
/// one in a field it also uses for a Cocoa pointer is easier to spot that way
/// than if they were 1, 2, 3.
static NEXT_REFERENCE: AtomicU64 = AtomicU64::new(0x4361_7262_0001);

fn install() -> u64 {
    let reference = NEXT_REFERENCE.fetch_add(2, Ordering::Relaxed);
    let mut held = INSTALLED.lock().unwrap_or_else(|e| e.into_inner());
    held.get_or_insert_with(HashSet::new).insert(reference);
    reference
}

fn uninstall(reference: u64) -> bool {
    let mut held = INSTALLED.lock().unwrap_or_else(|e| e.into_inner());
    held.as_mut().is_some_and(|table| table.remove(&reference))
}

/// `EventTargetRef GetApplicationEventTarget(void)`
pub unsafe extern "C" fn GetApplicationEventTarget() -> *mut c_void {
    (&raw const APPLICATION_TARGET).cast_mut().cast()
}

/// `EventTargetRef GetEventDispatcherTarget(void)`
pub unsafe extern "C" fn GetEventDispatcherTarget() -> *mut c_void {
    (&raw const DISPATCHER_TARGET).cast_mut().cast()
}

/// `EventLoopRef GetMainEventLoop(void)`
pub unsafe extern "C" fn GetMainEventLoop() -> *mut c_void {
    (&raw const MAIN_EVENT_LOOP).cast_mut().cast()
}

/// `EventTime GetCurrentEventTime(void)`
///
/// Seconds since boot as a `double`, which is the same clock
/// `mach_absolute_time` reports and the one a program subtracts two readings
/// of to get a frame time.
pub unsafe extern "C" fn GetCurrentEventTime() -> f64 {
    darwin::mach::media_time()
}

/// `OSStatus InstallEventHandler(EventTargetRef inTarget, EventHandlerUPP
/// inHandler, ItemCount inNumTypes, const EventTypeSpec *inList, void
/// *inUserData, EventHandlerRef *outRef)`
///
/// # Safety
///
/// `out` is null or a writable `EventHandlerRef`.
pub unsafe extern "C" fn InstallEventHandler(
    _target: *mut c_void,
    _handler: *mut c_void,
    _types: u64,
    _list: *const c_void,
    _context: *mut c_void,
    out: *mut u64,
) -> OSStatus {
    let reference = install();
    if !out.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *out = reference };
    }
    NO_ERR
}

/// `OSStatus RemoveEventHandler(EventHandlerRef inHandlerRef)`
pub unsafe extern "C" fn RemoveEventHandler(reference: u64) -> OSStatus {
    match uninstall(reference) {
        true => NO_ERR,
        false => EVENT_INVALID_PARAMETER,
    }
}

/// `OSStatus InstallEventLoopTimer(EventLoopRef inEventLoop, EventTimerInterval
/// inFireDelay, EventTimerInterval inInterval, EventLoopTimerUPP inTimerProc,
/// void *inTimerData, EventLoopTimerRef *outTimer)`
///
/// The timer is recorded and never fires: nothing here drives a Carbon event
/// loop, and a program that depends on one would be waiting on this tree
/// rather than on its own clock.
///
/// # Safety
///
/// `out` is null or a writable `EventLoopTimerRef`.
pub unsafe extern "C" fn InstallEventLoopTimer(
    _loop_ref: *mut c_void,
    _delay: f64,
    _interval: f64,
    _proc: *mut c_void,
    _context: *mut c_void,
    out: *mut u64,
) -> OSStatus {
    let reference = install();
    if !out.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *out = reference };
    }
    NO_ERR
}

/// `OSStatus RemoveEventLoopTimer(EventLoopTimerRef inTimer)`
pub unsafe extern "C" fn RemoveEventLoopTimer(reference: u64) -> OSStatus {
    match uninstall(reference) {
        true => NO_ERR,
        false => EVENT_INVALID_PARAMETER,
    }
}

/// `OSStatus ReceiveNextEvent(ItemCount inNumTypes, const EventTypeSpec
/// *inList, EventTimeout inTimeout, Boolean inPullEvent, EventRef *outEvent)`
///
/// The queue is empty, so this is the timeout answer. The timeout itself is
/// *not* waited out: a caller passing `kEventDurationForever` means "until
/// something happens", and nothing ever will, so sleeping on it would hang the
/// program rather than idle it.
///
/// # Safety
///
/// `out` is null or a writable `EventRef`.
pub unsafe extern "C" fn ReceiveNextEvent(
    _types: u64,
    _list: *const c_void,
    _timeout: f64,
    _pull: u8,
    out: *mut *mut c_void,
) -> OSStatus {
    if !out.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *out = std::ptr::null_mut() };
    }
    EVENT_LOOP_TIMED_OUT_ERR
}

/// `void ReleaseEvent(EventRef inEvent)`
///
/// Nothing here makes an event, so nothing here owns one.
pub unsafe extern "C" fn ReleaseEvent(_event: *mut c_void) {}

/// `OSStatus SendEventToEventTarget(EventRef inEvent, EventTargetRef inTarget)`
pub unsafe extern "C" fn SendEventToEventTarget(
    _event: *mut c_void,
    _target: *mut c_void,
) -> OSStatus {
    EVENT_NOT_HANDLED_ERR
}

/// `OSStatus SetSystemUIMode(SystemUIMode inMode, SystemUIOptions inOptions)`
///
/// Hiding the menu bar and the Dock for a full-screen game. There is neither
/// here, and the window's own full-screen state is AppKit's - so this is
/// accepted and has nothing to do.
pub unsafe extern "C" fn SetSystemUIMode(_mode: u32, _options: u32) -> OSStatus {
    NO_ERR
}

/// `UInt32 GetEventKind(EventRef)`
///
/// Zero: nothing here makes an `EventRef` (see [`ReceiveNextEvent`]), so there
/// is no event whose kind could be asked for.
pub unsafe extern "C" fn GetEventKind(_event: *const c_void) -> u32 {
    0
}

/// `typeWildCard`, `'****'`: [`GetEventParameter`]'s desired type when the
/// caller will accept whatever type the value was set with.
const TYPE_WILD_CARD: u32 = u32::from_be_bytes(*b"****");

/// Every parameter [`SetEventParameter`] has stored on an event, by the
/// event's own address and the parameter's name: `(type, bytes)`.
///
/// Nothing in this tree ever dispatches a real event to an installed handler
/// (see [`ReceiveNextEvent`]), so the only `EventRef`s a guest ever has in
/// hand are ones it builds and fills in itself, entirely through this pair of
/// calls. Keying by address rather than by a reference this module minted is
/// what lets that work: whatever the guest's own `EventRef` is, setting a
/// parameter on it and reading it back has to agree.
type ParameterTable = HashMap<usize, HashMap<u32, (u32, Vec<u8>)>>;
static PARAMETERS: Mutex<Option<ParameterTable>> = Mutex::new(None);

/// `OSStatus GetEventParameter(EventRef inEvent, EventParamName inName,
/// EventParamType inDesiredType, EventParamType *outActualType, UInt32
/// inBufferSize, UInt32 *outActualSize, void *outData)`
///
/// # Safety
///
/// `actual_type` and `actual_size` are null or writable for a `UInt32`;
/// `data` is null or writable for `buffer_size` bytes.
pub unsafe extern "C" fn GetEventParameter(
    event: *mut c_void,
    name: u32,
    desired_type: u32,
    actual_type: *mut u32,
    buffer_size: u32,
    actual_size: *mut u32,
    data: *mut c_void,
) -> OSStatus {
    if event.is_null() {
        return EVENT_INVALID_PARAMETER;
    }
    let held = PARAMETERS.lock().unwrap_or_else(|e| e.into_inner());
    let Some((kind, bytes)) = held
        .as_ref()
        .and_then(|events| events.get(&(event as usize)))
        .and_then(|params| params.get(&name))
    else {
        return EVENT_INVALID_PARAMETER;
    };
    // A real coercion between related types (`typeChar` read back as
    // `typeUnicodeText`, say) is not attempted - only an exact match, or the
    // wildcard that takes whatever was stored.
    if desired_type != TYPE_WILD_CARD && desired_type != *kind {
        return EVENT_INVALID_PARAMETER;
    }
    if !actual_type.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *actual_type = *kind };
    }
    if !actual_size.is_null() {
        unsafe { *actual_size = bytes.len() as u32 };
    }
    if !data.is_null() {
        let n = bytes.len().min(buffer_size as usize);
        // SAFETY: the caller states `data` is writable for `buffer_size` bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), data.cast(), n) };
    }
    NO_ERR
}

/// `OSStatus SetEventParameter(EventRef inEvent, EventParamName inName,
/// EventParamType inType, UInt32 inSize, const void *inDataPtr)`
///
/// # Safety
///
/// `data` is null, when `size` is zero, or readable for `size` bytes.
pub unsafe extern "C" fn SetEventParameter(
    event: *mut c_void,
    name: u32,
    kind: u32,
    size: u32,
    data: *const c_void,
) -> OSStatus {
    if event.is_null() || (data.is_null() && size != 0) {
        return EVENT_INVALID_PARAMETER;
    }
    let bytes = if size == 0 {
        Vec::new()
    } else {
        // SAFETY: the caller states this is readable for `size` bytes.
        unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size as usize) }.to_vec()
    };
    let mut held = PARAMETERS.lock().unwrap_or_else(|e| e.into_inner());
    held.get_or_insert_with(HashMap::default)
        .entry(event as usize)
        .or_default()
        .insert(name, (kind, bytes));
    NO_ERR
}

/// `OSStatus CallNextEventHandler(EventHandlerCallRef inHandlerCallRef,
/// EventRef inEvent)`
///
/// Every handler a guest installs through [`InstallEventHandler`] is recorded
/// and never called - nothing here drives an event loop that would dispatch to
/// it - so there is never a next handler in the chain to reach, and the honest
/// answer is the same one Apple's own gives at the end of a real chain:
/// `eventNotHandledErr`.
pub unsafe extern "C" fn CallNextEventHandler(next: *mut c_void, _event: *mut c_void) -> OSStatus {
    match next.is_null() {
        true => EVENT_INVALID_PARAMETER,
        false => EVENT_NOT_HANDLED_ERR,
    }
}

/// `Boolean GetColor(Point where, ConstStr255Param prompt, const RGBColor
/// *inColor, RGBColor *outColor)`
///
/// The classic Color Picker dialog, which this host has no window to show.
/// `where` and `prompt` place and label a dialog that never opens, so they are
/// taken and ignored; the result is the one Apple documents for a user who
/// cancels - `false`, with `outColor` left exactly as the caller passed it.
pub unsafe extern "C" fn GetColor(
    _where: u32,
    _prompt: *const u8,
    _in_color: *const c_void,
    _out_color: *mut c_void,
) -> u8 {
    0
}

/// `UInt32 GetCurrentButtonState(void)`
///
/// Bit `n` is the button whose `-[NSEvent buttonNumber]` is `n`, the same
/// convention `CGEventSourceButtonState` and `+[NSEvent pressedMouseButtons]`
/// use - see `appkit::button::pressed_mask`, which this mirrors without this
/// crate taking a dependency on all of AppKit for four lines.
pub unsafe extern "C" fn GetCurrentButtonState() -> u32 {
    let Some((_, _, mask)) = sdl3::video::global_mouse_state() else {
        return 0;
    };
    let number = |button: u8| match button {
        sdl3::mouse::LEFT => 0,
        sdl3::mouse::RIGHT => 1,
        sdl3::mouse::MIDDLE => 2,
        other => u32::from(other) - 1,
    };
    (1..=32u8)
        .filter(|b| mask & sdl3::mouse::mask(*b) != 0)
        .fold(0, |bits, b| bits | 1 << number(b))
}

/// `OSStatus EnableSecureEventInput(void)` and its pair.
///
/// Secure input keeps other processes from reading keystrokes; no process
/// here can, so there is nothing to turn on.
pub unsafe extern "C" fn EnableSecureEventInput() -> OSStatus {
    NO_ERR
}

pub unsafe extern "C" fn DisableSecureEventInput() -> OSStatus {
    NO_ERR
}

/// `TSMDocumentID TSMGetActiveDocument(void)`
///
/// NULL: Cocoa text input goes through `NSTextInputContext`, and no Carbon
/// text services document is ever activated.
pub unsafe extern "C" fn TSMGetActiveDocument() -> *mut c_void {
    std::ptr::null_mut()
}

/// `OSStatus TSMSetDocumentProperty(TSMDocumentID, OSType, UInt32, void *)`
/// and `TSMRemoveDocumentProperty(TSMDocumentID, OSType)`.
///
/// `tsmInvalidDocIDErr` for the NULL document, which is the only one
/// [`TSMGetActiveDocument`] answers.
pub unsafe extern "C" fn TSMSetDocumentProperty(
    _document: *mut c_void,
    _tag: u32,
    _size: u32,
    _data: *const c_void,
) -> OSStatus {
    TSM_INVALID_DOC_ID_ERR
}

pub unsafe extern "C" fn TSMRemoveDocumentProperty(_document: *mut c_void, _tag: u32) -> OSStatus {
    TSM_INVALID_DOC_ID_ERR
}

/// `tsmInvalidDocIDErr`.
const TSM_INVALID_DOC_ID_ERR: OSStatus = -2507;

/// `EventQueueRef GetCurrentEventQueue(void)`
///
/// "If the current thread is a cooperative thread, the main event queue is
/// returned" - and every guest thread here is one, since nothing in this tree
/// gives a thread its own Carbon event queue.
pub unsafe extern "C" fn GetCurrentEventQueue() -> *mut c_void {
    (&raw const EVENT_QUEUE).cast_mut().cast()
}

/// `OSStatus FlushEventsMatchingListFromQueue(EventQueueRef inQueue,
/// ItemCount inNumTypes, const EventTypeSpec *inList)`
///
/// Nothing here ever posts an event onto a queue - see [`ReceiveNextEvent`] -
/// so there is never one matching `inList` to flush; a non-null queue is the
/// only thing worth checking.
pub unsafe extern "C" fn FlushEventsMatchingListFromQueue(
    queue: *mut c_void,
    _num_types: u64,
    _list: *const c_void,
) -> OSStatus {
    match queue.is_null() {
        true => EVENT_INVALID_PARAMETER,
        false => NO_ERR,
    }
}

/// `TextEncoding GetApplicationTextEncoding(void)`
///
/// `kTextEncodingMacRoman`: the encoding this tree's own CFBundle/`'vers'`
/// localization machinery would resolve to for an English US application with
/// no other preference recorded - see `MacApplication.h`'s own list of
/// fallbacks, the last of which is "the current localization of the operating
/// system", which is what this host answers everywhere else.
pub unsafe extern "C" fn GetApplicationTextEncoding() -> u32 {
    corefoundation::string::encoding::MAC_ROMAN
}

/// A virtual key code's bit position within a [`GetKeys`] `KeyMap`: byte
/// `code / 8`, bit `7 - code % 8` of that byte - the same numbering
/// [`BitTst`](crate::osutils::BitTst) documents, which this is specialized to
/// avoid a cross-module call for four keys.
fn set_key_bit(map: &mut [u8; 16], code: u8) {
    let byte = usize::from(code / 8);
    let bit = 7 - (code % 8);
    map[byte] |= 1 << bit;
}

/// `void GetKeys(KeyMap theKeys)`.
///
/// FIXME (ACCURACY): only the modifier keys' bits are answered, from the same
/// live keyboard state `appkit`'s own event handling reads
/// (`sdl3::video::keyboard_modifiers`) - mirrored here rather than taking a
/// dependency on all of `appkit`, as `coreservices::events::GetCurrentButtonState`
/// already does for the mouse buttons. Every other key's bit is always 0: SDL
/// publishes no per-scancode *global* keyboard state (only a
/// window-focus-relative one), so whether an ordinary key is down cannot be
/// answered honestly without one, and guessing would be worse than admitting
/// it. Caps Lock's bit reflects the lock being *engaged*, not the physical key
/// being held, since that is the only signal SDL's modifier mask carries.
///
/// # Safety
///
/// `the_keys` is null or writable for 16 bytes (`sizeof(KeyMap)`).
pub unsafe extern "C" fn GetKeys(the_keys: *mut u8) {
    if the_keys.is_null() {
        return;
    }
    use appkit_key_bits::*;
    let mods = sdl3::video::keyboard_modifiers();
    let mut map = [0_u8; 16];
    if mods & sdl3::keycodes::keymod::LGUI != 0 {
        set_key_bit(&mut map, COMMAND);
    }
    if mods & sdl3::keycodes::keymod::RGUI != 0 {
        set_key_bit(&mut map, RIGHT_COMMAND);
    }
    if mods & sdl3::keycodes::keymod::LSHIFT != 0 {
        set_key_bit(&mut map, SHIFT);
    }
    if mods & sdl3::keycodes::keymod::RSHIFT != 0 {
        set_key_bit(&mut map, RIGHT_SHIFT);
    }
    if mods & sdl3::keycodes::keymod::CAPS != 0 {
        set_key_bit(&mut map, CAPS_LOCK);
    }
    if mods & sdl3::keycodes::keymod::LALT != 0 {
        set_key_bit(&mut map, OPTION);
    }
    if mods & sdl3::keycodes::keymod::RALT != 0 {
        set_key_bit(&mut map, RIGHT_OPTION);
    }
    if mods & sdl3::keycodes::keymod::LCTRL != 0 {
        set_key_bit(&mut map, CONTROL);
    }
    if mods & sdl3::keycodes::keymod::RCTRL != 0 {
        set_key_bit(&mut map, RIGHT_CONTROL);
    }
    // SAFETY: the caller states this is writable for 16 bytes.
    unsafe { std::ptr::copy_nonoverlapping(map.as_ptr(), the_keys, 16) };
}

/// `kVK_*` for the modifier keys [`GetKeys`] answers - transcribed from
/// `appkit::keymap::vk`, which this module does not depend on for eight
/// constants.
mod appkit_key_bits {
    pub const RIGHT_COMMAND: u8 = 0x36;
    pub const COMMAND: u8 = 0x37;
    pub const SHIFT: u8 = 0x38;
    pub const CAPS_LOCK: u8 = 0x39;
    pub const OPTION: u8 = 0x3a;
    pub const CONTROL: u8 = 0x3b;
    pub const RIGHT_SHIFT: u8 = 0x3c;
    pub const RIGHT_OPTION: u8 = 0x3d;
    pub const RIGHT_CONTROL: u8 = 0x3e;
}

/// `UInt32 KeyTranslate(const void *transData, UInt16 keycode, UInt32 *state)`.
///
/// `keycode` packs, as `KeyTrans` always has: bits 0-6 the virtual key code,
/// bit 7 set for a key-up event, and the high byte the modifier state (the
/// same byte `UCKeyTranslate`'s own `modifiers` argument takes in its low
/// eight bits - Apple's docs for that call describe it as "the modifiers
/// field of an event record, right shifted by 8 bits", which is exactly this
/// keycode's high byte).
///
/// FIXME (ACCURACY): `transData` - the caller's own `'KCHR'` resource - is
/// ignored. Parsing the classic `'KCHR'` binary format is not implemented;
/// this always answers from the one built-in US layout `appkit::uchr` already
/// provides for `UCKeyTranslate`, through the same dead-key state machine, so
/// a caller using the default US layout (overwhelmingly the common case for
/// `transData == GetScriptManagerVariable(smKCHRCache)`) gets the real
/// character, while one that installed a custom `'KCHR'` does not.
///
/// # Safety
///
/// `state` is null or a writable/readable `UInt32`.
pub unsafe extern "C" fn KeyTranslate(
    _trans_data: *const c_void,
    keycode: u16,
    state: *mut u32,
) -> u32 {
    let vk = keycode & 0x7f;
    let modifiers = u32::from(keycode >> 8);
    let mut dead_state = match state.is_null() {
        true => 0,
        // SAFETY: the caller's contract.
        false => unsafe { *state },
    };
    // SAFETY: `appkit::uchr::layout()` is this module's own static resource.
    let chars = unsafe {
        appkit::uchr::translate(
            appkit::uchr::layout().as_ptr(),
            vk,
            0,
            modifiers,
            u32::from(appkit::textinput::LMGetKbdType()),
            0,
            &mut dead_state,
        )
    }
    .unwrap_or_default();
    if !state.is_null() {
        // SAFETY: the caller's contract.
        unsafe { *state = dead_state };
    }
    // A classic `'KCHR'` table can itself produce up to two characters for
    // one key (an accent that did not combine, say); `appkit::uchr::translate`
    // can too, and both land the same way in the result's low two bytes.
    let mac_roman = |unit: u16| -> u8 {
        corefoundation::eightbit::encode(unit, corefoundation::string::encoding::MAC_ROMAN)
            .unwrap_or(b'?')
    };
    let mut result = 0_u32;
    if let Some(&first) = chars.first() {
        result |= u32::from(mac_roman(first));
    }
    if let Some(&second) = chars.get(1) {
        result |= u32::from(mac_roman(second)) << 8;
    }
    result
}

exports::exports! {
    functions {
        "GetEventKind" => GetEventKind,
        "EnableSecureEventInput" => EnableSecureEventInput,
        "DisableSecureEventInput" => DisableSecureEventInput,
        "TSMGetActiveDocument" => TSMGetActiveDocument,
        "TSMSetDocumentProperty" => TSMSetDocumentProperty,
        "TSMRemoveDocumentProperty" => TSMRemoveDocumentProperty,
        "GetApplicationEventTarget" => GetApplicationEventTarget,
        "GetEventDispatcherTarget" => GetEventDispatcherTarget,
        "GetMainEventLoop" => GetMainEventLoop,
        "GetCurrentEventTime" => GetCurrentEventTime,
        "InstallEventHandler" => InstallEventHandler,
        "RemoveEventHandler" => RemoveEventHandler,
        "InstallEventLoopTimer" => InstallEventLoopTimer,
        "RemoveEventLoopTimer" => RemoveEventLoopTimer,
        "ReceiveNextEvent" => ReceiveNextEvent,
        "ReleaseEvent" => ReleaseEvent,
        "SendEventToEventTarget" => SendEventToEventTarget,
        "SetSystemUIMode" => SetSystemUIMode,
        "GetEventParameter" => GetEventParameter,
        "SetEventParameter" => SetEventParameter,
        "CallNextEventHandler" => CallNextEventHandler,
        "GetColor" => GetColor,
        "GetCurrentButtonState" => GetCurrentButtonState,
        "GetCurrentEventQueue" => GetCurrentEventQueue,
        "FlushEventsMatchingListFromQueue" => FlushEventsMatchingListFromQueue,
        "GetApplicationTextEncoding" => GetApplicationTextEncoding,
        "GetKeys" => GetKeys,
        "KeyTranslate" => KeyTranslate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three singletons are non-null and are three different things, which
    /// is what a program comparing two targets depends on.
    #[test]
    fn the_targets_are_distinct_and_not_null() {
        // SAFETY: none of the three reads anything.
        let (application, dispatcher, main) = unsafe {
            (
                GetApplicationEventTarget(),
                GetEventDispatcherTarget(),
                GetMainEventLoop(),
            )
        };
        assert!(!application.is_null() && !dispatcher.is_null() && !main.is_null());
        assert_ne!(application, dispatcher);
        assert_ne!(application, main);
    }

    /// A receive on an empty queue times out and leaves no event behind - the
    /// caller would release whatever was written there.
    #[test]
    fn a_receive_times_out_and_writes_no_event() {
        // Not null to begin with, so that the call having written null is
        // something this can tell from it having written nothing.
        let mut event = std::ptr::without_provenance_mut::<c_void>(1);
        // SAFETY: a writable out-parameter.
        let status = unsafe { ReceiveNextEvent(0, std::ptr::null(), f64::MAX, 1, &mut event) };
        assert_eq!(status, EVENT_LOOP_TIMED_OUT_ERR);
        assert!(event.is_null());
    }

    /// A handler can be removed once, and a reference that names nothing is
    /// refused rather than reported as removed.
    #[test]
    fn a_handler_is_removed_once() {
        let mut reference = 0_u64;
        // SAFETY: a writable out-parameter.
        let status = unsafe {
            InstallEventHandler(
                GetApplicationEventTarget(),
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut reference,
            )
        };
        assert_eq!(status, NO_ERR);
        assert_ne!(reference, 0);
        unsafe {
            assert_eq!(RemoveEventHandler(reference), NO_ERR);
            assert_eq!(RemoveEventHandler(reference), EVENT_INVALID_PARAMETER);
        }
    }

    /// The event time moves forward, because a program divides by the
    /// difference between two readings.
    #[test]
    fn the_event_time_advances() {
        let first = unsafe { GetCurrentEventTime() };
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = unsafe { GetCurrentEventTime() };
        assert!(second > first, "{second} is not after {first}");
    }

    /// A parameter round-trips on the event it was set on, and asking a
    /// different event - or the right one for a type it was not set as -
    /// finds nothing.
    #[test]
    fn an_event_parameter_round_trips() {
        let event = std::ptr::without_provenance_mut::<c_void>(0x1234);
        let other = std::ptr::without_provenance_mut::<c_void>(0x5678);
        let name = u32::from_be_bytes(*b"ABCD");
        let kind = u32::from_be_bytes(*b"TEXT");
        let value = 0x2a_u32;
        // SAFETY: a value of the size this call is told it is.
        unsafe {
            assert_eq!(
                SetEventParameter(
                    event,
                    name,
                    kind,
                    size_of::<u32>() as u32,
                    (&raw const value).cast()
                ),
                NO_ERR
            );
        }
        let mut got = 0_u32;
        let mut actual_type = 0_u32;
        let mut actual_size = 0_u32;
        // SAFETY: storage for a `UInt32` and the type/size out-parameters.
        unsafe {
            assert_eq!(
                GetEventParameter(
                    event,
                    name,
                    kind,
                    &mut actual_type,
                    size_of::<u32>() as u32,
                    &mut actual_size,
                    (&raw mut got).cast()
                ),
                NO_ERR
            );
        }
        assert_eq!(got, value);
        assert_eq!(actual_type, kind);
        assert_eq!(actual_size, size_of::<u32>() as u32);
        let wrong_type = u32::from_be_bytes(*b"long");
        // SAFETY: as above.
        unsafe {
            assert_eq!(
                GetEventParameter(
                    event,
                    name,
                    wrong_type,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                EVENT_INVALID_PARAMETER
            );
            assert_eq!(
                GetEventParameter(
                    other,
                    name,
                    kind,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                EVENT_INVALID_PARAMETER
            );
        }
    }

    /// Nothing here ever reaches a next handler, so the honest answer is the
    /// one Apple's own gives at the end of a real chain.
    #[test]
    fn call_next_event_handler_always_falls_through() {
        let handler = std::ptr::without_provenance_mut::<c_void>(1);
        // SAFETY: neither argument is read.
        unsafe {
            assert_eq!(
                CallNextEventHandler(handler, std::ptr::null_mut()),
                EVENT_NOT_HANDLED_ERR
            );
            assert_eq!(
                CallNextEventHandler(std::ptr::null_mut(), std::ptr::null_mut()),
                EVENT_INVALID_PARAMETER
            );
        }
    }

    /// There is no window to show a color picker in, so the answer is the
    /// documented "the user canceled" one, and the caller's color is untouched.
    #[test]
    fn get_color_reports_no_picker() {
        let mut color = [0x11_u8; 6];
        let before = color;
        // SAFETY: a prompt and colors this test owns, none of which are read.
        let picked = unsafe {
            GetColor(
                0,
                std::ptr::null(),
                color.as_ptr().cast(),
                color.as_mut_ptr().cast(),
            )
        };
        assert_eq!(picked, 0);
        assert_eq!(color, before);
    }

    #[test]
    fn flushing_the_current_event_queue_succeeds_and_null_is_refused() {
        // SAFETY: nothing here reads through the queue reference.
        unsafe {
            let queue = GetCurrentEventQueue();
            assert!(!queue.is_null());
            assert_eq!(
                FlushEventsMatchingListFromQueue(queue, 0, std::ptr::null()),
                NO_ERR
            );
            assert_eq!(
                FlushEventsMatchingListFromQueue(std::ptr::null_mut(), 0, std::ptr::null()),
                EVENT_INVALID_PARAMETER
            );
        }
    }

    #[test]
    fn application_text_encoding_is_mac_roman() {
        assert_eq!(
            unsafe { GetApplicationTextEncoding() },
            corefoundation::string::encoding::MAC_ROMAN
        );
    }

    /// Nothing in a test run is actually holding down a key, so every bit is
    /// 0 - including the modifier bits this module does track, which is
    /// still useful coverage of the write path and the bit numbering.
    #[test]
    fn get_keys_with_nothing_held_is_all_zero() {
        let mut map = [0xaa_u8; 16];
        // SAFETY: `map` is writable for 16 bytes.
        unsafe { GetKeys(map.as_mut_ptr()) };
        assert_eq!(map, [0_u8; 16]);
        // SAFETY: null is accepted and does nothing.
        unsafe { GetKeys(std::ptr::null_mut()) };
    }

    /// `vk::ANSI_A` with no modifiers types "a"; with the shift bit in the
    /// keycode's high byte, "A" - the same layout `appkit::uchr`'s own tests
    /// check, reached through the classic keycode packing instead of
    /// `UCKeyTranslate`'s separate arguments.
    #[test]
    fn key_translate_answers_the_built_in_us_layout() {
        let mut state = 0_u32;
        // SAFETY: `state` is this test's own storage; `transData` is unread.
        let plain = unsafe { KeyTranslate(std::ptr::null(), 0x0000, &mut state) };
        assert_eq!(plain & 0xff, u32::from(b'a'));
        let shifted = unsafe {
            KeyTranslate(
                std::ptr::null(),
                u16::from(appkit::uchr::modifier::SHIFT) << 8,
                &mut state,
            )
        };
        assert_eq!(shifted & 0xff, u32::from(b'A'));
    }
}
