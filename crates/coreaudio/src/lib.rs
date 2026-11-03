#![allow(clippy::missing_safety_doc)]
#![allow(non_snake_case)]

use std::collections::BTreeSet;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};

mod capture;
mod converter;
mod foreign;
mod format;
mod hal;
pub mod mixer;
mod render;
mod status;
mod stream;
pub(crate) mod trace;

pub use status::*;

/// A four-character code as its characters, for a caller naming a component.
pub fn trace_code(value: u32) -> String {
    trace::code(value)
}

/// Say what one call did, under `ROSEMARY_TRACE_AUDIO`. For [`audiotoolbox`].
pub fn trace(what: std::fmt::Arguments<'_>, status: i32) {
    trace::say(what, status);
}

use stream::{Format, sdl_spec};

/// `AudioComponentDescription`, which is five four-byte fields.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct ComponentDescription {
    pub kind: u32,
    pub subtype: u32,
    pub manufacturer: u32,
    pub flags: u32,
    pub flags_mask: u32,
}

/// A four-character code, as the headers write them.
const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

/// `kAudioUnitType_Output`.
const TYPE_OUTPUT: u32 = fourcc(b"auou");
/// The three output subtypes that mean "the default device": `def `, `ahal`
/// and, on iOS-derived code paths, `rioc`.
const SUBTYPE_DEFAULT_OUTPUT: u32 = fourcc(b"def ");
const SUBTYPE_HAL_OUTPUT: u32 = fourcc(b"ahal");
const SUBTYPE_SYSTEM_OUTPUT: u32 = fourcc(b"sys ");
const SUBTYPE_REMOTE_IO: u32 = fourcc(b"rioc");
/// `kAudioUnitManufacturer_Apple`.
const MANUFACTURER_APPLE: u32 = fourcc(b"appl");

/// The components this crate has, as opaque handles.
///
/// The address of a static rather than an index: a guest compares the handle
/// against null and passes it back, and nothing else.
static OUTPUT_COMPONENT: u8 = 0;
static MIXER_COMPONENT: u8 = 0;

fn output_component() -> *mut c_void {
    &OUTPUT_COMPONENT as *const u8 as *mut c_void
}

fn mixer_component() -> *mut c_void {
    &MIXER_COMPONENT as *const u8 as *mut c_void
}

/// `kAudioUnitSubType_MatrixMixer`, a component of its own because it has
/// properties the others do not and a different default shape.
const SUBTYPE_MATRIX_MIXER: u32 = fourcc(b"mxmx");

static MATRIX_MIXER_COMPONENT: u8 = 0;

fn matrix_mixer_component() -> *mut c_void {
    &MATRIX_MIXER_COMPONENT as *const u8 as *mut c_void
}

/// `kAudioUnitType_FormatConverter` and the plain converter subtype.
const TYPE_CONVERTER: u32 = fourcc(b"aufc");
const SUBTYPE_CONVERTER: u32 = fourcc(b"conv");

static CONVERTER_COMPONENT: u8 = 0;

fn converter_component() -> *mut c_void {
    &CONVERTER_COMPONENT as *const u8 as *mut c_void
}

/// `kAudioUnitSubType_Varispeed`: a converter whose rate the program sets.
const SUBTYPE_VARISPEED: u32 = fourcc(b"vari");

static VARISPEED_COMPONENT: u8 = 0;

fn varispeed_component() -> *mut c_void {
    &VARISPEED_COMPONENT as *const u8 as *mut c_void
}

fn describes_varispeed(d: &ComponentDescription) -> bool {
    d.kind == TYPE_CONVERTER
        && d.subtype == SUBTYPE_VARISPEED
        && (d.manufacturer == 0 || d.manufacturer == MANUFACTURER_APPLE)
}

fn describes_converter(d: &ComponentDescription) -> bool {
    let matches = |field: u32, wanted: &[u32]| field == 0 || wanted.contains(&field);
    d.kind == TYPE_CONVERTER
        && matches(d.subtype, &[SUBTYPE_CONVERTER])
        && matches(d.manufacturer, &[MANUFACTURER_APPLE])
}

/// Whether `description` asks for a mixer this crate can be.
fn describes_mixer(d: &ComponentDescription) -> bool {
    let matches = |field: u32, wanted: &[u32]| field == 0 || wanted.contains(&field);
    d.kind == mixer::TYPE_MIXER
        && matches(d.subtype, mixer::SUBTYPES)
        && matches(d.manufacturer, &[MANUFACTURER_APPLE])
}

/// Whether `description` asks for the default output unit.
///
/// A zero field is a wildcard, which is what `AudioComponentFindNext`'s
/// contract says and what a guest that only fills in the type relies on.
fn describes_default_output(d: &ComponentDescription) -> bool {
    let matches = |field: u32, wanted: &[u32]| field == 0 || wanted.contains(&field);
    matches(d.kind, &[TYPE_OUTPUT])
        && matches(
            d.subtype,
            &[
                SUBTYPE_DEFAULT_OUTPUT,
                SUBTYPE_HAL_OUTPUT,
                SUBTYPE_SYSTEM_OUTPUT,
                SUBTYPE_REMOTE_IO,
            ],
        )
        && matches(d.manufacturer, &[MANUFACTURER_APPLE])
}

/// What a unit is. There are two, and they render differently: an output unit
/// is pulled by the device and a mixer is pulled by whatever it feeds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Output,
    Mixer,
    /// `kAudioUnitSubType_AUConverter`: one input, one output, and a change of
    /// format in between. A graph puts one in front of each voice so that a
    /// sound recorded at its own rate reaches a mixer at the mixer's.
    Converter,
}

/// One unit, of either kind.
pub struct Unit {
    pub(crate) kind: Kind,
    /// A mixer's input elements. Empty for an output unit, whose single input
    /// is [`Unit::callback`] or [`Unit::source`].
    pub(crate) inputs: Vec<mixer::Element>,
    /// The unit a graph connected to this one's input, as its handle, or zero.
    pub(crate) source: usize,
    /// A converter's *input* scope format; [`Unit::format`] is its output.
    /// Unused by the other kinds, whose one format is the input they are given.
    pub(crate) input_format: Format,
    /// Input frames a converter has pulled and not yet consumed, interleaved as
    /// floats - what makes resampling continuous across calls instead of
    /// restarting, and clicking, at every buffer boundary.
    pub(crate) pending: Vec<f32>,
    /// How far into the first pending frame the next output sample falls.
    pub(crate) phase: f64,
    /// The format the guest set on the input scope, which is the format its
    /// render callback will be asked to fill.
    format: Format,
    /// The render callback and its cookie, as
    /// `kAudioUnitProperty_SetRenderCallback` gave them.
    callback: Option<render::Callback>,
    /// SDL's stream, once `AudioUnitInitialize` has opened the device.
    stream: sdl3::AudioStream,
    running: bool,
    /// Frames pulled so far, which is the `mSampleTime` a guest integrates.
    rendered: u64,
    /// A matrix mixer, with [`Unit::levels`].
    pub(crate) matrix: bool,
    /// `kAudioUnitProperty_MatrixLevels` as the guest last set it.
    pub(crate) levels: Vec<f32>,
    /// A varispeed's playback rate, which scales how fast it consumes its
    /// input; `None` for every other unit.
    pub(crate) varispeed: Option<f64>,
    /// `kAudioOutputUnitProperty_EnableIO` on element 0 (play) and element 1
    /// (record).
    pub(crate) playback: bool,
    pub(crate) recording: bool,
    /// `kAudioOutputUnitProperty_CurrentDevice`.
    pub(crate) device: u32,
    /// Element 1's output scope format, `None` for the device's.
    pub(crate) capture_format: Option<Format>,
    /// `kAudioOutputUnitProperty_SetInputCallback`.
    pub(crate) input_callback: Option<render::Callback>,
    /// The recording stream, and the last slice it recorded.
    pub(crate) capture: sdl3::AudioStream,
    pub(crate) captured: Vec<u8>,
    pub(crate) recorded: u64,
    /// Buffers for an element 1 render whose list brought none.
    pub(crate) lent: Vec<u8>,
}

// SAFETY: `stream` is SDL's own handle, used only through `sdl3::audio`, and the
// callback is a host-callable pointer this runtime was handed. Both are behind
// the mutex below.
unsafe impl Send for Unit {}

/// Every instance handed out, by the handle the guest holds - the address of
/// the `Mutex`, which is stable and unique for as long as the `Arc` lives.
#[derive(Default)]
struct Units {
    active: Vec<(usize, Arc<Mutex<Unit>>)>,
    disposed: BTreeSet<usize>,
}

static UNITS: Mutex<Units> = Mutex::new(Units {
    active: Vec::new(),
    disposed: BTreeSet::new(),
});

fn unit_for(handle: *mut c_void) -> Option<Arc<Mutex<Unit>>> {
    UNITS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .active
        .iter()
        .find(|(h, _)| *h == handle as usize)
        .map(|(_, u)| Arc::clone(u))
}

/// `AudioComponent AudioComponentFindNext(AudioComponent, const AudioComponentDescription *)`
///
/// # Safety
///
/// `description` must be null or a readable `AudioComponentDescription`.
pub unsafe extern "C" fn AudioComponentFindNext(
    after: *mut c_void,
    description: *const ComponentDescription,
) -> *mut c_void {
    // There is exactly one component, so "the next one after it" is none.
    if !after.is_null() || description.is_null() {
        return std::ptr::null_mut();
    }
    let wanted = unsafe { *description };
    trace::say(
        format_args!(
            "find component {} {} {}",
            trace::code(wanted.kind),
            trace::code(wanted.subtype),
            trace::code(wanted.manufacturer)
        ),
        SUCCESS,
    );
    if describes_default_output(&wanted) {
        return output_component();
    }
    if describes_mixer(&wanted) {
        return match wanted.subtype == SUBTYPE_MATRIX_MIXER {
            true => matrix_mixer_component(),
            false => mixer_component(),
        };
    }
    if describes_varispeed(&wanted) {
        return varispeed_component();
    }
    match describes_converter(&wanted) {
        true => converter_component(),
        false => std::ptr::null_mut(),
    }
}

/// `OSStatus AudioComponentInstanceNew(AudioComponent, AudioComponentInstance *)`
///
/// # Safety
///
/// `out` must be null or a writable pointer-sized slot.
pub unsafe extern "C" fn AudioComponentInstanceNew(
    component: *mut c_void,
    out: *mut *mut c_void,
) -> i32 {
    if out.is_null() {
        return PARAM_ERROR;
    }
    let matrix = component == matrix_mixer_component();
    let kind = match component {
        c if c == output_component() => Kind::Output,
        c if c == mixer_component() || matrix => Kind::Mixer,
        c if c == converter_component() || c == varispeed_component() => Kind::Converter,
        _ => return INVALID_COMPONENT,
    };
    let unit = Arc::new(Mutex::new(Unit {
        kind,
        // A mixer starts with no elements: a guest sets how many it wants with
        // `kAudioUnitProperty_ElementCount` before it configures any of them.
        // Apple's matrix mixer has one input bus from the start, and a program
        // configures it without asking for any.
        inputs: match matrix {
            true => vec![mixer::Element::default()],
            false => Vec::new(),
        },
        matrix,
        levels: Vec::new(),
        varispeed: (component == varispeed_component()).then_some(1.0),
        source: 0,
        input_format: Format::default(),
        pending: Vec::new(),
        phase: 0.0,
        format: Format::default(),
        callback: None,
        stream: std::ptr::null_mut(),
        running: false,
        rendered: 0,
        playback: true,
        recording: false,
        device: hal::OUTPUT_DEVICE,
        capture_format: None,
        input_callback: None,
        capture: std::ptr::null_mut(),
        captured: Vec::new(),
        recorded: 0,
        lent: Vec::new(),
    }));
    let handle = Arc::as_ptr(&unit) as *mut c_void;
    let mut units = UNITS.lock().unwrap_or_else(|e| e.into_inner());
    units.disposed.remove(&(handle as usize));
    units.active.push((handle as usize, unit));
    unsafe { *out = handle };
    SUCCESS
}

/// `OSStatus AudioComponentInstanceDispose(AudioComponentInstance)`
pub unsafe extern "C" fn AudioComponentInstanceDispose(handle: *mut c_void) -> i32 {
    let unit = {
        let mut units = UNITS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = units.active.iter().position(|(h, _)| *h == handle as usize) {
            let (_, unit) = units.active.remove(index);
            units.disposed.insert(handle as usize);
            Some(unit)
        } else if units.disposed.contains(&(handle as usize)) {
            return INVALID_COMPONENT;
        } else {
            None
        }
    };
    let Some(unit) = unit else {
        return foreign::status(c"AudioComponentInstanceDispose", handle)
            .unwrap_or(INVALID_COMPONENT);
    };
    // SAFETY: `close` owns the streams and leaves none behind.
    unsafe { close(&unit) };
    SUCCESS
}

/// Close both of a unit's streams. The recording stream is destroyed after the
/// lock is released, as [`capture::destroy`] requires.
///
/// # Safety
///
/// As [`stream::close`].
unsafe fn close(unit: &Mutex<Unit>) {
    let capture = {
        let mut unit = unit.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the caller's obligation.
        unsafe { stream::close(&mut unit) };
        std::mem::replace(&mut unit.capture, std::ptr::null_mut())
    };
    // SAFETY: taken out of the unit, with its lock released.
    unsafe { capture::destroy(capture) };
}

/// `OSStatus AudioUnitInitialize(AudioUnit)`: open the device.
///
/// Opened here rather than at `AudioOutputUnitStart`, because that is where
/// Apple's fails for a format the hardware cannot take - and a guest that has
/// already started is past the point where it checks.
pub unsafe extern "C" fn AudioUnitInitialize(handle: *mut c_void) -> i32 {
    let Some(unit) = unit_for(handle) else {
        return foreign::status(c"AudioUnitInitialize", handle).unwrap_or(INVALID_COMPONENT);
    };
    let mut unit = unit.lock().unwrap_or_else(|e| e.into_inner());
    // A mixer plays through nothing: the unit it feeds is what opens a device,
    // and this one is ready as soon as it exists.
    if unit.kind != Kind::Output {
        return SUCCESS;
    }
    if !unit.stream.is_null() || !unit.capture.is_null() {
        return SUCCESS;
    }
    if !unit.playback {
        return match unit.recording {
            true => capture::open(&mut unit, handle),
            false => SUCCESS,
        };
    }
    let Some(spec) = sdl_spec(&unit.format) else {
        trace::say(
            format_args!("initialize {:?}", unit.format),
            FORMAT_NOT_SUPPORTED,
        );
        return FORMAT_NOT_SUPPORTED;
    };
    let mut status = match stream::open(&mut unit, spec, handle) {
        true => SUCCESS,
        false => CANNOT_DO_IN_CURRENT_CONTEXT,
    };
    trace::say(
        format_args!("initialize {} Hz, {} channels", spec.freq, spec.channels),
        status,
    );
    if status == SUCCESS && unit.recording {
        status = capture::open(&mut unit, handle);
    }
    status
}

/// `OSStatus AudioUnitUninitialize(AudioUnit)`
pub unsafe extern "C" fn AudioUnitUninitialize(handle: *mut c_void) -> i32 {
    let Some(unit) = unit_for(handle) else {
        return foreign::status(c"AudioUnitUninitialize", handle).unwrap_or(INVALID_COMPONENT);
    };
    // SAFETY: as `AudioComponentInstanceDispose`.
    unsafe { close(&unit) };
    SUCCESS
}

/// `OSStatus AudioOutputUnitStart(AudioOutputUnit)`
pub unsafe extern "C" fn AudioOutputUnitStart(handle: *mut c_void) -> i32 {
    let Some(unit) = unit_for(handle) else {
        return foreign::status(c"AudioOutputUnitStart", handle).unwrap_or(INVALID_COMPONENT);
    };
    let mut unit = unit.lock().unwrap_or_else(|e| e.into_inner());
    // A mixer has no device to start. A graph starts every unit it holds, so
    // answering `kAudioUnitErr_Uninitialized` here would fail the whole graph
    // over the one unit that was never going to play anything by itself.
    if unit.kind != Kind::Output {
        unit.running = true;
        return SUCCESS;
    }
    if unit.stream.is_null() && unit.capture.is_null() {
        return UNINITIALIZED;
    }
    unit.running = true;
    for stream in [unit.stream, unit.capture] {
        if !stream.is_null() {
            // SAFETY: a stream this crate opened.
            unsafe { sdl3::audio::resume(stream) };
        }
    }
    trace::say(format_args!("start"), SUCCESS);
    SUCCESS
}

/// `OSStatus AudioOutputUnitStop(AudioOutputUnit)`
pub unsafe extern "C" fn AudioOutputUnitStop(handle: *mut c_void) -> i32 {
    let Some(unit) = unit_for(handle) else {
        return foreign::status(c"AudioOutputUnitStop", handle).unwrap_or(INVALID_COMPONENT);
    };
    let capture = {
        let mut unit = unit.lock().unwrap_or_else(|e| e.into_inner());
        if !unit.stream.is_null() {
            // SAFETY: a stream this crate opened.
            unsafe { sdl3::audio::pause(unit.stream) };
        }
        unit.running = false;
        unit.capture
    };
    // Outside the lock, for the reason `close` gives.
    if !capture.is_null() {
        // SAFETY: a stream this crate opened.
        unsafe { sdl3::audio::pause(capture) };
    }
    SUCCESS
}

/// The clock `AudioGetCurrentHostTime` reports, in nanoseconds.
///
/// It must be the *same* clock `mach_absolute_time` is, because a program
/// compares an `AudioTimeStamp`'s `mHostTime` against one it read from Mach -
/// scheduling a sound "half a second from now" is exactly that subtraction.
/// `darwin::mach` answers `CLOCK_MONOTONIC` nanoseconds off Darwin and reports
/// a 1/1 timebase, so this reads the same clock and
/// [`AudioConvertHostTimeToNanos`] is the identity.
pub(crate) fn host_time() -> u64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `clock_gettime` writes the `timespec` it is given and nothing
    // else.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } != 0 {
        return 0;
    }
    (now.tv_sec as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(now.tv_nsec as u64)
}

/// `UInt64 AudioGetCurrentHostTime(void)`
pub unsafe extern "C" fn AudioGetCurrentHostTime() -> u64 {
    host_time()
}

/// `UInt64 AudioConvertHostTimeToNanos(UInt64)`, and its inverse: both the
/// identity here, for the reason [`host_time`] gives.
pub unsafe extern "C" fn AudioConvertHostTimeToNanos(host_time: u64) -> u64 {
    host_time
}

pub unsafe extern "C" fn AudioConvertNanosToHostTime(nanos: u64) -> u64 {
    nanos
}

/// `OSStatus AudioUnitRender(AudioUnit, AudioUnitRenderActionFlags *, const
/// AudioTimeStamp *, UInt32, UInt32, AudioBufferList *)`
///
/// Pull this unit's render callback into the caller's buffer list. A program
/// calls it when it drives the graph itself rather than letting the output unit
/// pull - a mixer that wants a block of its input, or a tap recording what is
/// about to be played.
///
/// # Safety
///
/// `list` must be a writable `AudioBufferList` whose buffers are writable for
/// their own `mDataByteSize`, and `timestamp` null or readable.
pub unsafe extern "C" fn AudioUnitRender(
    handle: *mut c_void,
    flags: *mut u32,
    timestamp: *const render::TimeStamp,
    bus: u32,
    frames: u32,
    list: *mut render::AudioBufferList,
) -> i32 {
    if list.is_null() {
        return PARAM_ERROR;
    }
    let Some(unit) = unit_for(handle) else {
        return foreign::status(c"AudioUnitRender", handle).unwrap_or(INVALID_COMPONENT);
    };
    let mut unit = unit.lock().unwrap_or_else(|e| e.into_inner());
    // A mixer or converter renders what feeds it, converted - often another
    // unit a graph connected rather than a callback of its own.
    if unit.kind != Kind::Output {
        let format = unit.format;
        // `render_into_list` takes the lock again, on this unit and on the
        // units upstream of it.
        drop(unit);
        // SAFETY: the caller's obligation, as this function's.
        return unsafe { render::render_into_list(handle as usize, format, frames as usize, list) };
    }
    if bus == capture::ELEMENT && !unit.capture.is_null() {
        // SAFETY: the caller's obligation, as this function's.
        return unsafe { capture::render(&mut unit, frames, list) };
    }
    let Some(callback) = unit.callback else {
        // A unit with no callback has nothing to render, and the buffers are
        // left as they are: an `AudioUnitRender` that failed has not promised
        // anything about them.
        return UNINITIALIZED;
    };
    let now = match timestamp.is_null() {
        true => render::TimeStamp {
            sample_time: unit.rendered as f64,
            host_time: host_time(),
            rate_scalar: 1.0,
            flags: 1 | 2,
            ..render::TimeStamp::default()
        },
        false => unsafe { *timestamp },
    };
    let mut action = 0u32;
    // SAFETY: `callback.proc` is a host-callable pointer this runtime was
    // handed for the guest's function, and every structure it is given outlives
    // the call.
    let status = unsafe {
        (callback.proc)(
            callback.ref_con,
            match flags.is_null() {
                true => &mut action,
                false => flags,
            },
            &now,
            bus,
            frames,
            list,
        )
    };
    if status == SUCCESS {
        unit.rendered += u64::from(frames);
    }
    status
}

/// `OSStatus AudioUnitSetParameter(AudioUnit, AudioUnitParameterID,
/// AudioUnitScope, AudioUnitElement, AudioUnitParameterValue, UInt32)`
///
/// The parameters that mean something here are a mixer's per-element placement
/// and gain; everything else is accepted by a unit that has the element and
/// refused by one that does not, which is what tells a guest whether its voice
/// exists.
/// `OSStatus AudioUnitAddPropertyListener(AudioUnit, AudioUnitPropertyID,
/// AudioUnitPropertyListenerProc, void *)`, and the two ways of removing one.
///
/// Accepted for a live unit and never called back: a unit's properties here
/// change only when the program sets them, never behind its back (no device
/// is unplugged, no stream format renegotiated).
pub unsafe extern "C" fn AudioUnitAddPropertyListener(
    handle: *mut c_void,
    _id: u32,
    _listener: *const c_void,
    _data: *mut c_void,
) -> i32 {
    match unit_for(handle) {
        Some(_) => 0,
        None => INVALID_COMPONENT,
    }
}

pub unsafe extern "C" fn AudioUnitRemovePropertyListenerWithUserData(
    handle: *mut c_void,
    _id: u32,
    _listener: *const c_void,
    _data: *mut c_void,
) -> i32 {
    // SAFETY: the same check.
    unsafe { AudioUnitAddPropertyListener(handle, 0, std::ptr::null(), std::ptr::null_mut()) }
}

/// `OSStatus AudioUnitReset(AudioUnit, AudioUnitScope, AudioUnitElement)`:
/// a unit here keeps no rendering history between cycles, so there is none to
/// clear.
pub unsafe extern "C" fn AudioUnitReset(handle: *mut c_void, _scope: u32, _element: u32) -> i32 {
    match unit_for(handle) {
        Some(_) => 0,
        None => INVALID_COMPONENT,
    }
}

/// `kVarispeedParam_PlaybackRate`, a factor, and `kVarispeedParam_PlaybackCents`,
/// the same rate in cents.
const VARISPEED_RATE: u32 = 0;
const VARISPEED_CENTS: u32 = 1;

pub unsafe extern "C" fn AudioUnitSetParameter(
    handle: *mut c_void,
    id: u32,
    scope: u32,
    element: u32,
    value: f32,
    _buffer_offset: u32,
) -> i32 {
    let Some(unit) = unit_for(handle) else {
        return INVALID_COMPONENT;
    };
    let mut unit = unit.lock().unwrap_or_else(|e| e.into_inner());
    if unit.varispeed.is_some() {
        // Apple's ranges: a quarter to four times, and two octaves either way.
        let rate = match id {
            VARISPEED_RATE => f64::from(value).clamp(0.25, 4.0),
            VARISPEED_CENTS => 2f64.powf(f64::from(value).clamp(-2400.0, 2400.0) / 1200.0),
            _ => return INVALID_PARAMETER,
        };
        unit.varispeed = Some(rate);
        return SUCCESS;
    }
    if unit.kind != Kind::Mixer {
        return INVALID_PARAMETER;
    }
    // A mixer's parameters live on its input elements; the output scope has a
    // volume this crate does not separate from the device's own.
    const SCOPE_INPUT: u32 = 1;
    if scope != SCOPE_INPUT {
        return SUCCESS;
    }
    let Some(input) = mixer::element_mut(&mut unit.inputs, element) else {
        return INVALID_ELEMENT;
    };
    trace::say(
        format_args!("set parameter {id} element {element} = {value}"),
        SUCCESS,
    );
    use mixer::parameter as p;
    match id {
        p::AZIMUTH => input.azimuth = value,
        p::ELEVATION => input.elevation = value,
        p::DISTANCE => input.distance = value,
        p::GAIN => input.gain = value,
        p::ENABLE => input.enabled = value != 0.0,
        // Rate is a resampling this crate does not do. Accepted so that a game
        // setting it to 1.0 - which every one of them does - is not told its
        // voice failed.
        p::PLAYBACK_RATE => {
            if (value - 1.0).abs() > f32::EPSILON {
                report_unsupported_rate();
            }
        }
        _ => return INVALID_PARAMETER,
    }
    SUCCESS
}

/// `OSStatus AudioUnitGetParameter(...)`
pub unsafe extern "C" fn AudioUnitGetParameter(
    handle: *mut c_void,
    id: u32,
    scope: u32,
    element: u32,
    out: *mut f32,
) -> i32 {
    if out.is_null() {
        return PARAM_ERROR;
    }
    let Some(unit) = unit_for(handle) else {
        return INVALID_COMPONENT;
    };
    let unit = unit.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(rate) = unit.varispeed {
        // SAFETY: the caller's out-parameter, checked non-null above.
        unsafe {
            *out = match id {
                VARISPEED_RATE => rate as f32,
                VARISPEED_CENTS => (1200.0 * rate.log2()) as f32,
                _ => return INVALID_PARAMETER,
            }
        };
        return SUCCESS;
    }
    const SCOPE_INPUT: u32 = 1;
    if unit.kind != Kind::Mixer || scope != SCOPE_INPUT {
        return INVALID_PARAMETER;
    }
    let Some(input) = unit.inputs.get(element as usize) else {
        return INVALID_ELEMENT;
    };
    use mixer::parameter as p;
    let value = match id {
        p::AZIMUTH => input.azimuth,
        p::ELEVATION => input.elevation,
        p::DISTANCE => input.distance,
        p::GAIN => input.gain,
        p::ENABLE => input.enabled as u32 as f32,
        p::PLAYBACK_RATE => 1.0,
        _ => return INVALID_PARAMETER,
    };
    // SAFETY: a writable float, as just checked.
    unsafe { *out = value };
    SUCCESS
}

/// Said once: a voice asking to be resampled is played at its own rate.
fn report_unsupported_rate() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        eprintln!(
            "rosemary: audio: this program sets a mixer playback rate, which this runtime does \
             not resample - those voices play at their recorded pitch"
        );
    });
}

/// Route `source`'s output into `destination`'s input bus, for a graph.
///
/// Answers whether both handles name units of this crate's. The connection is
/// what makes a graph play: an output unit pulled by the device pulls the unit
/// connected to it, and so on back through the mixer to each voice.
pub fn connect(source: *mut c_void, destination: *mut c_void, destination_bus: u32) -> bool {
    let (Some(from), Some(unit)) = (unit_for(source), unit_for(destination)) else {
        return false;
    };
    // The format the connection carries is the source's output format, which is
    // what CoreAudio propagates across a connection. Taking the destination's
    // own instead would have the mixer asking for a stream nobody set up.
    let carried = {
        let from = from.lock().unwrap_or_else(|e| e.into_inner());
        from.format
    };
    let mut unit = unit.lock().unwrap_or_else(|e| e.into_inner());
    match unit.kind {
        Kind::Mixer => match mixer::element_mut(&mut unit.inputs, destination_bus) {
            Some(input) => {
                input.source = source as usize;
                input.format = carried;
            }
            None => return false,
        },
        Kind::Output | Kind::Converter => unit.source = source as usize,
    }
    true
}

/// Undo what [`connect`] did to one input bus.
pub fn disconnect(destination: *mut c_void, destination_bus: u32) -> bool {
    let Some(unit) = unit_for(destination) else {
        return false;
    };
    let mut unit = unit.lock().unwrap_or_else(|e| e.into_inner());
    match unit.kind {
        Kind::Mixer => match mixer::element_mut(&mut unit.inputs, destination_bus) {
            Some(input) => input.source = 0,
            None => return false,
        },
        Kind::Output | Kind::Converter => unit.source = 0,
    }
    true
}

exports::exports! {
    functions {
        // The three Carbon Component Manager spellings are aliases rather than
        // entries of their own: they are these calls under 1994 names, with the
        // same twenty-byte descriptor and the same argument order, and a guest
        // that opens its output unit the old way has to reach this registry and
        // not a second one beside it. `OSErr` is `OSStatus` narrowed to its low
        // half, which a zero and the framework's own negative codes survive.
        "AudioComponentFindNext" | "FindNextComponent" => AudioComponentFindNext,
        "AudioComponentInstanceNew" | "OpenAComponent" => AudioComponentInstanceNew,
        "AudioComponentInstanceDispose" | "CloseComponent" => AudioComponentInstanceDispose,
        "AudioUnitInitialize" => AudioUnitInitialize,
        "AudioUnitUninitialize" => AudioUnitUninitialize,
        "AudioOutputUnitStart" => AudioOutputUnitStart,
        "AudioOutputUnitStop" => AudioOutputUnitStop,
        "AudioUnitGetProperty" => property::AudioUnitGetProperty,
        "AudioUnitGetPropertyInfo" => property::AudioUnitGetPropertyInfo,
        "AudioUnitSetProperty" => property::AudioUnitSetProperty,
        "AudioUnitRender" => AudioUnitRender,
        "AudioUnitSetParameter" => AudioUnitSetParameter,
        "AudioUnitAddPropertyListener" => AudioUnitAddPropertyListener,
        "AudioUnitRemovePropertyListenerWithUserData" => AudioUnitRemovePropertyListenerWithUserData,
        "AudioUnitReset" => AudioUnitReset,
        "AudioUnitGetParameter" => AudioUnitGetParameter,
        "AudioGetCurrentHostTime" => AudioGetCurrentHostTime,
        "AudioConvertHostTimeToNanos" => AudioConvertHostTimeToNanos,
        "AudioConvertNanosToHostTime" => AudioConvertNanosToHostTime,
        "AudioObjectGetPropertyData" => hal::AudioObjectGetPropertyData,
        "AudioObjectGetPropertyDataSize" => hal::AudioObjectGetPropertyDataSize,
        "AudioObjectHasProperty" => hal::AudioObjectHasProperty,
        "AudioObjectIsPropertySettable" => hal::AudioObjectIsPropertySettable,
        "AudioObjectSetPropertyData" => hal::AudioObjectSetPropertyData,
        "AudioObjectAddPropertyListener" => hal::AudioObjectAddPropertyListener,
        "AudioObjectRemovePropertyListener" => hal::AudioObjectRemovePropertyListener,
        "AudioHardwareUnload" => hal::AudioHardwareUnload,
        "AudioHardwareGetPropertyInfo" => hal::AudioHardwareGetPropertyInfo,
        "AudioHardwareGetProperty" => hal::AudioHardwareGetProperty,
        "AudioHardwareAddPropertyListener" => hal::AudioHardwareAddPropertyListener,
        "AudioDeviceGetPropertyInfo" => hal::AudioDeviceGetPropertyInfo,
        "AudioDeviceGetProperty" => hal::AudioDeviceGetProperty,
        "AudioDeviceSetProperty" => hal::AudioDeviceSetProperty,
        "AudioDeviceCreateIOProcID" => hal::AudioDeviceCreateIOProcID,
        "AudioDeviceDestroyIOProcID" => hal::AudioDeviceDestroyIOProcID,
        "AudioDeviceStart" => hal::AudioDeviceStart,
        "AudioDeviceStop" => hal::AudioDeviceStop,
        "AudioConverterNew" => converter::AudioConverterNew,
        "AudioConverterNewSpecific" => converter::AudioConverterNewSpecific,
        "AudioConverterDispose" => converter::AudioConverterDispose,
        "AudioConverterReset" => converter::AudioConverterReset,
        "AudioConverterGetProperty" => converter::AudioConverterGetProperty,
        "AudioConverterGetPropertyInfo" => converter::AudioConverterGetPropertyInfo,
        "AudioConverterSetProperty" => converter::AudioConverterSetProperty,
        "AudioConverterFillComplexBuffer" => converter::AudioConverterFillComplexBuffer,
        "AudioFormatGetProperty" => format::AudioFormatGetProperty,
    }
}

pub mod property;

/// The component handle, for this crate's own tests.
#[cfg(test)]
fn test_component() -> *mut c_void {
    output_component()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UNDERTALE opens its output unit through the Carbon Component Manager,
    /// and a legacy spelling nothing publishes binds to zero: the game runs to
    /// its first sound and faults at a null call. Asserted at the registry
    /// rather than described, so the aliases cannot be dropped by accident.
    #[test]
    fn the_carbon_spellings_reach_the_same_registry() {
        for (legacy, modern) in [
            ("_FindNextComponent", "_AudioComponentFindNext"),
            ("_OpenAComponent", "_AudioComponentInstanceNew"),
            ("_CloseComponent", "_AudioComponentInstanceDispose"),
        ] {
            let bare = legacy.trim_start_matches('_');
            assert_eq!(
                symbol(bare),
                symbol(modern.trim_start_matches('_')),
                "{legacy} must be {modern}"
            );
            assert!(names().contains(&bare), "{legacy} must be published");
        }
    }

    #[test]
    fn only_the_default_output_component_is_found() {
        let output = ComponentDescription {
            kind: TYPE_OUTPUT,
            subtype: SUBTYPE_DEFAULT_OUTPUT,
            manufacturer: MANUFACTURER_APPLE,
            ..ComponentDescription::default()
        };
        // SAFETY: a live description.
        assert_eq!(
            unsafe { AudioComponentFindNext(std::ptr::null_mut(), &output) },
            output_component()
        );

        // A mixer is a real AudioUnit and is not this one. Answering with the
        // output unit would be a guest rendering into something that never
        // plays.
        let mixer = ComponentDescription {
            kind: fourcc(b"aumx"),
            ..output
        };
        assert!(unsafe { AudioComponentFindNext(std::ptr::null_mut(), &mixer) }.is_null());

        // And "the next one after this" is nothing, which is how a guest's
        // enumeration loop terminates.
        assert!(unsafe { AudioComponentFindNext(output_component(), &output) }.is_null());
    }

    /// A wildcard field matches, which is what a guest filling in only the type
    /// relies on.
    #[test]
    fn a_zero_field_is_a_wildcard() {
        let sparse = ComponentDescription {
            kind: TYPE_OUTPUT,
            ..ComponentDescription::default()
        };
        assert!(describes_default_output(&sparse));
    }

    #[test]
    fn an_instance_is_its_own_handle_and_is_disposed_once() {
        let mut handle = std::ptr::null_mut();
        // SAFETY: a writable slot and this crate's own component.
        unsafe {
            assert_eq!(
                AudioComponentInstanceNew(output_component(), &mut handle),
                SUCCESS
            );
            assert!(!handle.is_null());
            assert_eq!(AudioComponentInstanceDispose(handle), SUCCESS);
            // Twice is a guest bug, and it is named rather than crashing.
            assert_eq!(AudioComponentInstanceDispose(handle), INVALID_COMPONENT);
        }
    }

    /// Fills channel 0 with 0.5 and channel 1 with -0.25, planar.
    unsafe extern "C" fn constant_source(
        _ref_con: *mut c_void,
        _flags: *mut u32,
        _timestamp: *const render::TimeStamp,
        _bus: u32,
        frames: u32,
        list: *mut render::AudioBufferList,
    ) -> i32 {
        for (channel, value) in [0.5f32, -0.25].into_iter().enumerate() {
            // SAFETY: the list this crate built, with one buffer per channel.
            let data = unsafe { (*list).buffers[channel].data.cast::<f32>() };
            for frame in 0..frames as usize {
                unsafe { *data.add(frame) = value };
            }
        }
        SUCCESS
    }

    /// Life Is Strange mixes its own voices, each a varispeed feeding a
    /// converter, and pulls the converter with `AudioUnitRender`. The
    /// converter's input is the connection, not a callback, and refusing it
    /// left every voice silent.
    #[test]
    fn rendering_a_converter_pulls_the_unit_connected_to_it() {
        let (mut varispeed, mut converter) = (std::ptr::null_mut(), std::ptr::null_mut());
        let callback: [*const c_void; 2] = [constant_source as *const c_void, std::ptr::null()];
        const FRAMES: usize = 4;
        let mut left = [0f32; FRAMES];
        let mut right = [0f32; FRAMES];
        let mut list = render::AudioBufferList {
            count: 2,
            buffers: [render::AudioBuffer {
                channels: 1,
                byte_size: (FRAMES * 4) as u32,
                data: std::ptr::null_mut(),
            }; render::MAX_BUFFERS],
        };
        list.buffers[0].data = left.as_mut_ptr().cast();
        list.buffers[1].data = right.as_mut_ptr().cast();
        // SAFETY: this crate's own components, live structures of the sizes
        // declared, and a list whose buffers hold `FRAMES` samples each.
        unsafe {
            AudioComponentInstanceNew(varispeed_component(), &mut varispeed);
            AudioComponentInstanceNew(converter_component(), &mut converter);
            assert_eq!(
                property::AudioUnitSetProperty(
                    varispeed,
                    23,
                    1,
                    0,
                    callback.as_ptr().cast(),
                    size_of_val(&callback) as u32,
                ),
                SUCCESS
            );
            assert!(connect(varispeed, converter, 0));
            let status = AudioUnitRender(
                converter,
                std::ptr::null_mut(),
                std::ptr::null(),
                0,
                FRAMES as u32,
                &mut list,
            );
            AudioComponentInstanceDispose(converter);
            AudioComponentInstanceDispose(varispeed);
            assert_eq!(status, SUCCESS);
        }
        assert_eq!(left, [0.5; FRAMES]);
        assert_eq!(right, [-0.25; FRAMES]);
    }

    /// A converter with nothing feeding it says so rather than handing back
    /// buffers it never wrote.
    #[test]
    fn rendering_an_unconnected_converter_is_no_connection() {
        let mut converter = std::ptr::null_mut();
        let mut sample = 0f32;
        let mut list = render::AudioBufferList {
            count: 2,
            buffers: [render::AudioBuffer {
                channels: 1,
                byte_size: 4,
                data: (&mut sample as *mut f32).cast(),
            }; render::MAX_BUFFERS],
        };
        // SAFETY: as above, one frame.
        unsafe {
            AudioComponentInstanceNew(converter_component(), &mut converter);
            let status = AudioUnitRender(
                converter,
                std::ptr::null_mut(),
                std::ptr::null(),
                0,
                1,
                &mut list,
            );
            AudioComponentInstanceDispose(converter);
            assert_eq!(status, status::NO_CONNECTION);
        }
    }

    /// Starting a unit nothing initialized is the framework's own error, not a
    /// silent no-op: a guest that ignores it and then hears nothing has no way
    /// to tell that from a device with the volume down.
    #[test]
    fn starting_an_uninitialised_unit_says_so() {
        let mut handle = std::ptr::null_mut();
        unsafe {
            AudioComponentInstanceNew(output_component(), &mut handle);
            assert_eq!(AudioOutputUnitStart(handle), UNINITIALIZED);
            AudioComponentInstanceDispose(handle);
        }
    }
}
