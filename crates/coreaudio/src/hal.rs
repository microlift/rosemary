use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::render::{AudioBuffer, AudioBufferList, MAX_BUFFERS, TimeStamp};
use crate::status::SUCCESS;

/// `AudioObjectID`. Apple's are opaque; these are constants, because a program
/// compares them and passes them back and does nothing else with them.
pub type ObjectId = u32;

/// `kAudioObjectSystemObject`.
const SYSTEM_OBJECT: ObjectId = 1;
/// The playback device. Not 0: `kAudioObjectUnknown` is 0, and a caller checks
/// for it.
pub(crate) const OUTPUT_DEVICE: ObjectId = 2;
/// The recording device, present only when the host has something to record
/// from.
pub(crate) const INPUT_DEVICE: ObjectId = 3;

/// `kAudioHardwareUnknownPropertyError`, `'who?'`.
const UNKNOWN_PROPERTY: i32 = i32::from_be_bytes(*b"who?");
/// `kAudioHardwareBadObjectError`, `'!obj'`.
const BAD_OBJECT: i32 = i32::from_be_bytes(*b"!obj");
/// `kAudioHardwareUnsupportedOperationError`, `'unop'`.
const UNSUPPORTED: i32 = i32::from_be_bytes(*b"unop");
/// `kAudioHardwareIllegalOperationError`, `'nope'`.
const ILLEGAL: i32 = i32::from_be_bytes(*b"nope");

const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

/// The property selectors this HAL answers. Every one is a four-character code
/// and the codes are the interface - a guest passes the number, never a name.
mod selector {
    use super::fourcc;

    // On the system object.
    pub const DEVICES: u32 = fourcc(b"dev#");
    pub const DEFAULT_OUTPUT_DEVICE: u32 = fourcc(b"dOut");
    pub const DEFAULT_SYSTEM_OUTPUT_DEVICE: u32 = fourcc(b"sOut");
    pub const DEFAULT_INPUT_DEVICE: u32 = fourcc(b"dIn ");

    // On a device.
    pub const NAME: u32 = fourcc(b"lnam");
    /// `kAudioDevicePropertyDeviceName`: the deprecated C-string name, which
    /// WebRTC still lists devices by.
    pub const C_NAME: u32 = fourcc(b"name");
    pub const MANUFACTURER: u32 = fourcc(b"lmak");
    pub const DEVICE_UID: u32 = fourcc(b"uid ");
    pub const MODEL_UID: u32 = fourcc(b"muid");
    pub const TRANSPORT_TYPE: u32 = fourcc(b"tran");
    pub const NOMINAL_SAMPLE_RATE: u32 = fourcc(b"nsrt");
    pub const AVAILABLE_NOMINAL_SAMPLE_RATES: u32 = fourcc(b"nsr#");
    pub const STREAM_CONFIGURATION: u32 = fourcc(b"slay");
    pub const STREAMS: u32 = fourcc(b"stm#");
    pub const BUFFER_FRAME_SIZE: u32 = fourcc(b"fsiz");
    pub const BUFFER_FRAME_SIZE_RANGE: u32 = fourcc(b"fsz#");
    /// `kAudioDevicePropertyBufferSize` and its range: the same size in bytes.
    pub const BUFFER_SIZE: u32 = fourcc(b"bsiz");
    pub const BUFFER_SIZE_RANGE: u32 = fourcc(b"bsz#");
    pub const HOG_MODE: u32 = fourcc(b"oink");
    pub const DEVICE_IS_ALIVE: u32 = fourcc(b"livn");
    pub const DEVICE_IS_RUNNING: u32 = fourcc(b"goin");
    pub const LATENCY: u32 = fourcc(b"ltnc");
    pub const SAFETY_OFFSET: u32 = fourcc(b"saft");
    pub const STREAM_FORMAT: u32 = fourcc(b"sfmt");
    pub const PREFERRED_CHANNEL_LAYOUT: u32 = fourcc(b"srnd");
    pub const PREFERRED_CHANNELS_FOR_STEREO: u32 = fourcc(b"dch2");
}

/// `AudioObjectPropertyScope`. All three are declared even though only the
/// input side is branched on: a scope a caller passes is read here, and the
/// reader has to be able to see which value that is.
#[allow(dead_code)]
mod scope {
    use super::fourcc;
    pub const GLOBAL: u32 = fourcc(b"glob");
    pub const INPUT: u32 = fourcc(b"inpt");
    pub const OUTPUT: u32 = fourcc(b"outp");
}

/// `AudioObjectPropertyAddress`: what is being asked, of which side of the
/// device, and of which channel. Twelve bytes, and the layout is ABI.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Address {
    pub selector: u32,
    pub scope: u32,
    pub element: u32,
}

const _: () = assert!(size_of::<Address>() == 12);

/// `AudioValueRange`, which is how the HAL reports every "from here to there".
#[repr(C)]
#[derive(Clone, Copy)]
struct ValueRange {
    minimum: f64,
    maximum: f64,
}

/// What a device is, as SDL reports it.
///
/// Read once: SDL's default devices do not change under this process, and a
/// program that asked twice and got two answers would have no way to tell which
/// one its buffers were sized for.
struct Device {
    id: ObjectId,
    /// The scope this device has its one stream on.
    side: u32,
    sample_rate: f64,
    channels: u32,
    name: String,
    uid: &'static str,
    /// The buffer size an `IOProc` is called with, in frames.
    ///
    /// Settable, because `kAudioDevicePropertyBufferFrameSize` is settable on a
    /// real device and a program that sets it sizes its own buffers to match.
    /// Nothing else reads it until the next `AudioDeviceStart`.
    buffer_frames: AtomicU32,
}

impl Device {
    fn new(
        id: ObjectId,
        side: u32,
        spec: Option<sdl3::AudioSpec>,
        name: Option<String>,
        fallback: &str,
        uid: &'static str,
    ) -> Device {
        Device {
            id,
            side,
            // 48 kHz stereo when SDL declines to say, which is what it opens a
            // device at by default - so the fallback is the same number the
            // device would have run at rather than a guess.
            sample_rate: spec.map_or(48_000.0, |s| s.freq as f64),
            channels: spec.map_or(2, |s| s.channels.max(1) as u32),
            name: name.unwrap_or_else(|| fallback.to_string()),
            uid,
            buffer_frames: AtomicU32::new(512),
        }
    }

    fn frame_bytes(&self) -> usize {
        4 * self.channels as usize
    }

    fn buffer_frames(&self) -> u32 {
        self.buffer_frames.load(Ordering::Relaxed)
    }

    fn spec(&self) -> sdl3::AudioSpec {
        sdl3::AudioSpec {
            format: sdl3::audio::format::F32,
            channels: self.channels as i32,
            freq: self.sample_rate as i32,
        }
    }
}

fn output() -> &'static Device {
    static DEVICE: std::sync::OnceLock<Device> = std::sync::OnceLock::new();
    DEVICE.get_or_init(|| {
        Device::new(
            OUTPUT_DEVICE,
            scope::OUTPUT,
            sdl3::audio::device_format(),
            sdl3::audio::device_name(),
            "Default Output",
            "rosemary:default-output",
        )
    })
}

/// The recording device, or `None` on a host with nothing to record from - the
/// answer a Mac without a microphone gives, which callers already handle.
fn input() -> Option<&'static Device> {
    static DEVICE: std::sync::OnceLock<Option<Device>> = std::sync::OnceLock::new();
    DEVICE
        .get_or_init(|| {
            sdl3::audio::has_recording_device().then(|| {
                Device::new(
                    INPUT_DEVICE,
                    scope::INPUT,
                    sdl3::audio::recording_format(),
                    sdl3::audio::recording_name(),
                    "Default Input",
                    "rosemary:default-input",
                )
            })
        })
        .as_ref()
}

pub(crate) fn has_input() -> bool {
    input().is_some()
}

/// The recording device's format, in the canonical layout.
pub(crate) fn input_format() -> Option<crate::stream::Format> {
    input().map(|device| crate::stream::Format {
        sample_rate: device.sample_rate,
        channels_per_frame: device.channels,
        ..crate::stream::Format::default()
    })
}

pub(crate) fn input_buffer_frames() -> u32 {
    input().map_or(512, Device::buffer_frames)
}

fn device(object: ObjectId) -> Option<&'static Device> {
    match object {
        OUTPUT_DEVICE => Some(output()),
        INPUT_DEVICE => input(),
        _ => None,
    }
}

/// The buffer sizes a device accepts, in frames.
const MINIMUM_FRAMES: u32 = 64;
const MAXIMUM_FRAMES: u32 = 4096;

/// What one registered `AudioDeviceIOProc` is.
struct Proc {
    /// The device it was registered on, which decides whether it is handed
    /// captured frames or asked for frames to play.
    device: &'static Device,
    /// The guest's function, as a host-callable pointer.
    call: IoProc,
    client_data: *mut c_void,
    /// SDL's stream while this proc is started, null while it is stopped.
    stream: sdl3::AudioStream,
    /// Frames handed to it so far, which is the `mSampleTime` it integrates.
    rendered: u64,
}

// SAFETY: as `crate::Unit` - the stream is SDL's own handle, used only through
// `sdl3::audio`, and the callback is a host-callable pointer this runtime was
// handed. Both are behind the mutex below.
unsafe impl Send for Proc {}

/// `AudioDeviceIOProc`.
type IoProc = unsafe extern "C" fn(
    ObjectId,
    *const TimeStamp,
    *const AudioBufferList,
    *const TimeStamp,
    *mut AudioBufferList,
    *const TimeStamp,
    *mut c_void,
) -> i32;

static PROCS: Mutex<Vec<(usize, Arc<Mutex<Proc>>)>> = Mutex::new(Vec::new());

fn proc_for(id: *mut c_void) -> Option<Arc<Mutex<Proc>>> {
    PROCS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(handle, _)| *handle == id as usize)
        .map(|(_, p)| Arc::clone(p))
}

/// A property listener the guest registered.
///
/// Kept so that removing one succeeds and so that a second registration of the
/// same pair is not counted twice. None is ever called: nothing about this
/// device changes while the process runs - SDL opens one device and keeps it -
/// so a listener that fired would be reporting a change that did not happen.
struct Listener {
    object: ObjectId,
    selector: u32,
    call: usize,
    client_data: usize,
}

static LISTENERS: Mutex<Vec<Listener>> = Mutex::new(Vec::new());

/// Write `value` into a caller's buffer, honouring the size protocol every
/// `AudioObjectGetPropertyData` follows: `size` is in-out, the buffer may be
/// null when the caller is only asking how big the answer is, and a buffer
/// smaller than the answer takes as much as fits.
///
/// Getting this wrong is the whole failure mode of this API: a caller that is
/// told 8 bytes and given 16 has a corrupted stack frame, and one told 0
/// concludes the device has no channels.
unsafe fn answer(bytes: &[u8], data: *mut c_void, size: *mut u32) -> i32 {
    if size.is_null() {
        return crate::status::PARAM_ERROR;
    }
    let capacity = unsafe { *size } as usize;
    if !data.is_null() {
        let taken = capacity.min(bytes.len());
        // SAFETY: the caller says `data` is writable for `capacity` bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), data.cast::<u8>(), taken) };
        unsafe { *size = taken as u32 };
        return SUCCESS;
    }
    unsafe { *size = bytes.len() as u32 };
    SUCCESS
}

fn as_bytes<T: Copy>(value: &T) -> &[u8] {
    // SAFETY: `T` is a `Copy` structure with no padding this crate reads back,
    // and the slice borrows it for no longer than the call.
    unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
}

/// A `CFStringRef` answer, retained as the "copy" in the property's name says.
///
/// The caller releases it. A property whose name says `CFString` hands over
/// ownership, and a runtime that autoreleased it instead would be freeing a
/// string the guest still holds.
fn string_property(text: &str) -> Vec<u8> {
    let string = corefoundation::string::from_bytes(text.as_bytes(), false);
    as_bytes(&(string as usize)).to_vec()
}

/// The stream description a device runs at, for the deprecated
/// `kAudioDevicePropertyStreamFormat`: one interleaved buffer of floats, which
/// is what an `IOProc` is handed.
fn device_format(device: &Device) -> crate::stream::Format {
    let bytes_per_frame = 4 * device.channels;
    let canonical = crate::stream::Format::default();
    crate::stream::Format {
        sample_rate: device.sample_rate,
        format_flags: canonical.format_flags & !crate::stream::flags::IS_NON_INTERLEAVED,
        channels_per_frame: device.channels,
        bytes_per_frame,
        bytes_per_packet: bytes_per_frame,
        ..canonical
    }
}

/// The channel layout, as the `AudioBufferList` `kAudioDevicePropertyStream‑
/// Configuration` answers with: one interleaved buffer on the device's own
/// side and none at all on the other.
///
/// The *number of buffers* is what a caller counts channels from, so a side
/// that answered the other's layout would have a program open a capture path
/// against a speaker, or play to a microphone.
fn stream_configuration(device: &Device, scope: u32) -> Vec<u8> {
    let mut list = AudioBufferList {
        count: 0,
        buffers: [AudioBuffer {
            channels: 0,
            byte_size: 0,
            data: std::ptr::null_mut(),
        }; MAX_BUFFERS],
    };
    if on_side(device, scope) {
        list.count = 1;
        list.buffers[0].channels = device.channels;
        list.buffers[0].byte_size = device.buffer_frames() * device.frame_bytes() as u32;
    }
    // Only the buffers that are declared: the header's trailing array is one
    // element long and a caller reads `mNumberBuffers` of them.
    let mut bytes = as_bytes(&list.count).to_vec();
    bytes.extend_from_slice(&[0; 4]);
    for buffer in &list.buffers[..list.count as usize] {
        bytes.extend_from_slice(as_bytes(buffer));
    }
    bytes
}

/// Whether `scope` names the side of `device` that has a stream. The global
/// scope counts as either.
fn on_side(device: &Device, scope: u32) -> bool {
    scope == device.side || scope == scope::GLOBAL
}

/// `kAudioChannelLabel_*` for each channel SDL's default order puts in a frame
/// of `channels` channels.
fn channel_labels(channels: u32) -> &'static [u32] {
    const MONO: u32 = 42;
    const LEFT: u32 = 1;
    const RIGHT: u32 = 2;
    const CENTER: u32 = 3;
    const LFE: u32 = 4;
    const LEFT_SURROUND: u32 = 5;
    const RIGHT_SURROUND: u32 = 6;
    const REAR_LEFT: u32 = 33;
    const REAR_RIGHT: u32 = 34;
    const REAR_CENTER: u32 = 9;
    match channels {
        1 => &[MONO],
        2 => &[LEFT, RIGHT],
        3 => &[LEFT, RIGHT, LFE],
        4 => &[LEFT, RIGHT, REAR_LEFT, REAR_RIGHT],
        5 => &[LEFT, RIGHT, LFE, REAR_LEFT, REAR_RIGHT],
        6 => &[LEFT, RIGHT, CENTER, LFE, LEFT_SURROUND, RIGHT_SURROUND],
        7 => &[
            LEFT,
            RIGHT,
            CENTER,
            LFE,
            REAR_CENTER,
            LEFT_SURROUND,
            RIGHT_SURROUND,
        ],
        _ => &[
            LEFT,
            RIGHT,
            CENTER,
            LFE,
            REAR_LEFT,
            REAR_RIGHT,
            LEFT_SURROUND,
            RIGHT_SURROUND,
        ],
    }
}

/// `AudioChannelLayout` for the output side, as a real device reports it:
/// `kAudioChannelLayoutTag_UseChannelDescriptions` and one description per
/// channel.
///
/// Programs size their speaker setup from this rather than from the stream
/// configuration, and one refused falls back to whatever was on its stack.
fn preferred_channel_layout(device: &Device) -> Vec<u8> {
    let channels = device.channels;
    let labels = channel_labels(channels);
    let count = (channels as usize).min(labels.len());
    // mChannelLayoutTag, mChannelBitmap, mNumberChannelDescriptions.
    let mut bytes = Vec::with_capacity(12 + count * 20);
    bytes.extend_from_slice(as_bytes(&0u32));
    bytes.extend_from_slice(as_bytes(&0u32));
    bytes.extend_from_slice(as_bytes(&(count as u32)));
    for label in &labels[..count] {
        // mChannelLabel, mChannelFlags, mCoordinates[3].
        bytes.extend_from_slice(as_bytes(label));
        bytes.extend_from_slice(&[0; 16]);
    }
    bytes
}

/// The bytes one property answers with, or `None` for one this HAL does not
/// have.
fn property(object: ObjectId, address: &Address) -> Option<Vec<u8>> {
    let answer = described(object, address);
    if crate::trace::on() {
        eprintln!(
            "audio: object {object} {} property {} scope {} element {}",
            match answer {
                Some(_) => "has",
                None => "has no",
            },
            crate::trace::code(address.selector),
            crate::trace::code(address.scope),
            address.element,
        );
    }
    answer
}

fn described(object: ObjectId, address: &Address) -> Option<Vec<u8>> {
    match object {
        SYSTEM_OBJECT => system_property(address.selector),
        _ => device_property(device(object)?, address),
    }
}

fn system_property(selector: u32) -> Option<Vec<u8>> {
    Some(match selector {
        selector::DEVICES => {
            let mut ids = as_bytes(&OUTPUT_DEVICE).to_vec();
            if let Some(input) = input() {
                ids.extend_from_slice(as_bytes(&input.id));
            }
            ids
        }
        selector::DEFAULT_OUTPUT_DEVICE | selector::DEFAULT_SYSTEM_OUTPUT_DEVICE => {
            as_bytes(&OUTPUT_DEVICE).to_vec()
        }
        // `kAudioObjectUnknown` when there is nothing to record from: a caller
        // checks for it before opening an input.
        selector::DEFAULT_INPUT_DEVICE => as_bytes(&input().map_or(0, |d| d.id)).to_vec(),
        _ => return None,
    })
}

fn device_property(device: &'static Device, address: &Address) -> Option<Vec<u8>> {
    let mine = on_side(device, address.scope);
    Some(match address.selector {
        selector::NAME => string_property(&device.name),
        selector::C_NAME => {
            let mut name = device.name.clone().into_bytes();
            name.push(0);
            name
        }
        selector::MANUFACTURER => string_property("SDL"),
        selector::DEVICE_UID | selector::MODEL_UID => string_property(device.uid),
        // `kAudioDeviceTransportTypeBuiltIn`. A program checks this to decide
        // whether the device may vanish - USB and Bluetooth ones do.
        selector::TRANSPORT_TYPE => as_bytes(&fourcc(b"bltn")).to_vec(),
        selector::NOMINAL_SAMPLE_RATE => as_bytes(&device.sample_rate).to_vec(),
        selector::AVAILABLE_NOMINAL_SAMPLE_RATES => as_bytes(&ValueRange {
            minimum: device.sample_rate,
            maximum: device.sample_rate,
        })
        .to_vec(),
        selector::STREAM_CONFIGURATION => stream_configuration(device, address.scope),
        // One stream on the device's own side and none on the other, matching
        // the configuration above. The stream is the device's own id, so a
        // stream property asked of it is answered by the device.
        selector::STREAMS => match mine {
            true => as_bytes(&device.id).to_vec(),
            false => Vec::new(),
        },
        selector::STREAM_FORMAT => as_bytes(&device_format(device)).to_vec(),
        selector::PREFERRED_CHANNEL_LAYOUT if mine => preferred_channel_layout(device),
        // Channel numbers are 1-based; a mono device plays both on its one.
        selector::PREFERRED_CHANNELS_FOR_STEREO if mine => {
            let right = device.channels.min(2);
            [as_bytes(&1u32), as_bytes(&right)].concat()
        }
        selector::BUFFER_FRAME_SIZE => as_bytes(&device.buffer_frames()).to_vec(),
        selector::BUFFER_FRAME_SIZE_RANGE => as_bytes(&ValueRange {
            minimum: MINIMUM_FRAMES as f64,
            maximum: MAXIMUM_FRAMES as f64,
        })
        .to_vec(),
        selector::BUFFER_SIZE => {
            as_bytes(&(device.buffer_frames() * device.frame_bytes() as u32)).to_vec()
        }
        selector::BUFFER_SIZE_RANGE => {
            let frame = device.frame_bytes() as f64;
            as_bytes(&ValueRange {
                minimum: MINIMUM_FRAMES as f64 * frame,
                maximum: MAXIMUM_FRAMES as f64 * frame,
            })
            .to_vec()
        }
        // The `pid_t` holding the device exclusively, -1 for nobody. WebRTC
        // refuses a device it cannot read this from.
        selector::HOG_MODE => as_bytes(&-1i32).to_vec(),
        selector::DEVICE_IS_ALIVE => as_bytes(&1u32).to_vec(),
        selector::DEVICE_IS_RUNNING => {
            let running = PROCS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|(_, p)| {
                    let p = p.lock().unwrap_or_else(|e| e.into_inner());
                    p.device.id == device.id && !p.stream.is_null()
                });
            as_bytes(&u32::from(running)).to_vec()
        }
        // Latency and safety offset are what a program *adds* to its own
        // scheduling, so zero is a real answer rather than a missing one: SDL
        // does not report the device's, and claiming a number would move every
        // sound the program schedules.
        selector::LATENCY | selector::SAFETY_OFFSET => as_bytes(&0u32).to_vec(),
        _ => return None,
    })
}

/// `OSStatus AudioObjectGetPropertyData(AudioObjectID, const
/// AudioObjectPropertyAddress *, UInt32, const void *, UInt32 *, void *)`
///
/// # Safety
///
/// `address` must be a readable `AudioObjectPropertyAddress`, `size` a writable
/// `UInt32`, and `data` null or writable for `*size` bytes.
pub unsafe extern "C" fn AudioObjectGetPropertyData(
    object: ObjectId,
    address: *const Address,
    _qualifier_size: u32,
    _qualifier: *const c_void,
    size: *mut u32,
    data: *mut c_void,
) -> i32 {
    if address.is_null() {
        return crate::status::PARAM_ERROR;
    }
    let address = unsafe { *address };
    if !known(object) {
        return BAD_OBJECT;
    }
    match property(object, &address) {
        Some(bytes) => unsafe { answer(&bytes, data, size) },
        None => {
            // The size is zeroed rather than left alone, so a caller that
            // ignores the status reads an empty answer rather than whatever was
            // in its variable before.
            if !size.is_null() {
                unsafe { *size = 0 };
            }
            UNKNOWN_PROPERTY
        }
    }
}

/// `OSStatus AudioObjectGetPropertyDataSize(...)`
///
/// # Safety
///
/// As [`AudioObjectGetPropertyData`], with no data buffer.
pub unsafe extern "C" fn AudioObjectGetPropertyDataSize(
    object: ObjectId,
    address: *const Address,
    qualifier_size: u32,
    qualifier: *const c_void,
    size: *mut u32,
) -> i32 {
    if !size.is_null() {
        // The buffer is null below, so the answer is the full size whatever the
        // caller left in here.
        unsafe { *size = 0 };
    }
    unsafe {
        AudioObjectGetPropertyData(
            object,
            address,
            qualifier_size,
            qualifier,
            size,
            std::ptr::null_mut(),
        )
    }
}

fn known(object: ObjectId) -> bool {
    let known = object == SYSTEM_OBJECT || device(object).is_some();
    if !known && crate::trace::on() {
        eprintln!("audio: no object {object}");
    }
    known
}

/// `Boolean AudioObjectHasProperty(AudioObjectID, const
/// AudioObjectPropertyAddress *)`
///
/// # Safety
///
/// `address` must be null or a readable `AudioObjectPropertyAddress`.
pub unsafe extern "C" fn AudioObjectHasProperty(object: ObjectId, address: *const Address) -> u8 {
    if address.is_null() || !known(object) {
        return 0;
    }
    let address = unsafe { *address };
    u8::from(property(object, &address).is_some())
}

/// `OSStatus AudioObjectIsPropertySettable(AudioObjectID, const
/// AudioObjectPropertyAddress *, Boolean *)`
///
/// # Safety
///
/// As [`AudioObjectHasProperty`], and `settable` must be null or writable.
pub unsafe extern "C" fn AudioObjectIsPropertySettable(
    object: ObjectId,
    address: *const Address,
    settable: *mut u8,
) -> i32 {
    if address.is_null() {
        return crate::status::PARAM_ERROR;
    }
    let address = unsafe { *address };
    if !known(object) {
        return BAD_OBJECT;
    }
    if property(object, &address).is_none() {
        return UNKNOWN_PROPERTY;
    }
    if !settable.is_null() {
        unsafe { *settable = u8::from(is_settable(object, address.selector)) };
    }
    SUCCESS
}

/// The one size a caller may change, in frames or in bytes. The sample rate is
/// *not* settable: SDL opened the device at its own rate and converts to it, so
/// accepting a new one would be agreeing to something the device is not doing.
fn is_settable(object: ObjectId, selector: u32) -> bool {
    device(object).is_some()
        && matches!(
            selector,
            selector::BUFFER_FRAME_SIZE | selector::BUFFER_SIZE
        )
}

/// `OSStatus AudioObjectSetPropertyData(...)`
///
/// # Safety
///
/// As [`AudioObjectGetPropertyData`], and `data` must be readable for `size`
/// bytes.
pub unsafe extern "C" fn AudioObjectSetPropertyData(
    object: ObjectId,
    address: *const Address,
    _qualifier_size: u32,
    _qualifier: *const c_void,
    size: u32,
    data: *const c_void,
) -> i32 {
    if address.is_null() || data.is_null() {
        return crate::status::PARAM_ERROR;
    }
    let address = unsafe { *address };
    if !known(object) {
        return BAD_OBJECT;
    }
    if !is_settable(object, address.selector) {
        return match property(object, &address).is_some() {
            true => ILLEGAL,
            false => UNKNOWN_PROPERTY,
        };
    }
    if (size as usize) < size_of::<u32>() {
        return crate::status::PARAM_ERROR;
    }
    let Some(device) = device(object) else {
        return BAD_OBJECT;
    };
    let value = unsafe { *(data as *const u32) };
    let frames = match address.selector {
        selector::BUFFER_SIZE => value / device.frame_bytes() as u32,
        _ => value,
    };
    device.buffer_frames.store(
        frames.clamp(MINIMUM_FRAMES, MAXIMUM_FRAMES),
        Ordering::Relaxed,
    );
    SUCCESS
}

/// `OSStatus AudioObjectAddPropertyListener(AudioObjectID, const
/// AudioObjectPropertyAddress *, AudioObjectPropertyListenerProc, void *)`
///
/// Accepted and recorded, never called - see [`Listener`]. Success rather than
/// a refusal because the registration *did* succeed: there is nothing wrong
/// with the caller's request, there is simply no change to report.
///
/// # Safety
///
/// `address` must be null or readable.
pub unsafe extern "C" fn AudioObjectAddPropertyListener(
    object: ObjectId,
    address: *const Address,
    listener: *const c_void,
    client_data: *mut c_void,
) -> i32 {
    if address.is_null() || listener.is_null() {
        return crate::status::PARAM_ERROR;
    }
    if !known(object) {
        return BAD_OBJECT;
    }
    let address = unsafe { *address };
    LISTENERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Listener {
            object,
            selector: address.selector,
            call: listener as usize,
            client_data: client_data as usize,
        });
    SUCCESS
}

/// `OSStatus AudioObjectRemovePropertyListener(...)`
///
/// # Safety
///
/// As [`AudioObjectAddPropertyListener`].
pub unsafe extern "C" fn AudioObjectRemovePropertyListener(
    object: ObjectId,
    address: *const Address,
    listener: *const c_void,
    client_data: *mut c_void,
) -> i32 {
    if address.is_null() {
        return crate::status::PARAM_ERROR;
    }
    let address = unsafe { *address };
    let mut listeners = LISTENERS.lock().unwrap_or_else(|e| e.into_inner());
    let before = listeners.len();
    listeners.retain(|l| {
        !(l.object == object
            && l.selector == address.selector
            && l.call == listener as usize
            && l.client_data == client_data as usize)
    });
    match listeners.len() < before {
        true => SUCCESS,
        // Apple's answers this for a listener that was never added, and a
        // caller that removes one twice is asking a real question.
        false => crate::status::INVALID_PARAMETER,
    }
}

fn hardware_address(selector: u32) -> Address {
    Address {
        selector,
        scope: scope::GLOBAL,
        element: 0,
    }
}

fn device_address(channel: u32, is_input: u8, selector: u32) -> Address {
    Address {
        selector,
        scope: match is_input {
            0 => scope::OUTPUT,
            _ => scope::INPUT,
        },
        element: channel,
    }
}

/// The legacy `GetPropertyInfo` shape: a size and a writable flag, either null.
unsafe fn property_info(
    object: ObjectId,
    address: &Address,
    size: *mut u32,
    writable: *mut u8,
) -> i32 {
    let mut answer = 0u32;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(object, address, 0, std::ptr::null(), &mut answer)
    };
    if status != SUCCESS {
        return status;
    }
    if !size.is_null() {
        unsafe { *size = answer };
    }
    if !writable.is_null() {
        unsafe { *writable = u8::from(is_settable(object, address.selector)) };
    }
    SUCCESS
}

/// `OSStatus AudioHardwareGetPropertyInfo(AudioHardwarePropertyID, UInt32 *,
/// Boolean *)`
///
/// # Safety
///
/// `size` and `writable` must each be null or writable.
pub unsafe extern "C" fn AudioHardwareGetPropertyInfo(
    selector: u32,
    size: *mut u32,
    writable: *mut u8,
) -> i32 {
    unsafe { property_info(SYSTEM_OBJECT, &hardware_address(selector), size, writable) }
}

/// `OSStatus AudioHardwareGetProperty(AudioHardwarePropertyID, UInt32 *, void *)`
///
/// # Safety
///
/// As [`AudioObjectGetPropertyData`].
pub unsafe extern "C" fn AudioHardwareGetProperty(
    selector: u32,
    size: *mut u32,
    data: *mut c_void,
) -> i32 {
    let address = hardware_address(selector);
    unsafe { AudioObjectGetPropertyData(SYSTEM_OBJECT, &address, 0, std::ptr::null(), size, data) }
}

/// `OSStatus AudioHardwareAddPropertyListener(AudioHardwarePropertyID,
/// AudioHardwarePropertyListenerProc, void *)`
///
/// Accepted and not recorded: no listener is ever called (see [`Listener`]),
/// and this proc's signature is not the one [`LISTENERS`] holds.
pub unsafe extern "C" fn AudioHardwareAddPropertyListener(
    _selector: u32,
    listener: *const c_void,
    _client_data: *mut c_void,
) -> i32 {
    match listener.is_null() {
        true => crate::status::PARAM_ERROR,
        false => SUCCESS,
    }
}

/// `OSStatus AudioDeviceGetPropertyInfo(AudioDeviceID, UInt32 inChannel,
/// Boolean isInput, AudioDevicePropertyID, UInt32 *, Boolean *)`
///
/// # Safety
///
/// As [`AudioHardwareGetPropertyInfo`].
pub unsafe extern "C" fn AudioDeviceGetPropertyInfo(
    object: ObjectId,
    channel: u32,
    is_input: u8,
    selector: u32,
    size: *mut u32,
    writable: *mut u8,
) -> i32 {
    let address = device_address(channel, is_input, selector);
    unsafe { property_info(object, &address, size, writable) }
}

/// `OSStatus AudioDeviceGetProperty(AudioDeviceID, UInt32, Boolean,
/// AudioDevicePropertyID, UInt32 *, void *)`
///
/// # Safety
///
/// As [`AudioObjectGetPropertyData`].
pub unsafe extern "C" fn AudioDeviceGetProperty(
    object: ObjectId,
    channel: u32,
    is_input: u8,
    selector: u32,
    size: *mut u32,
    data: *mut c_void,
) -> i32 {
    let address = device_address(channel, is_input, selector);
    unsafe { AudioObjectGetPropertyData(object, &address, 0, std::ptr::null(), size, data) }
}

/// `OSStatus AudioDeviceSetProperty(AudioDeviceID, const AudioTimeStamp *,
/// UInt32, Boolean, AudioDevicePropertyID, UInt32, const void *)`
///
/// The time stamp asks for the change to land at a given sample time; every
/// change here is immediate, which is what a null one means.
///
/// # Safety
///
/// As [`AudioObjectSetPropertyData`].
pub unsafe extern "C" fn AudioDeviceSetProperty(
    object: ObjectId,
    _when: *const c_void,
    channel: u32,
    is_input: u8,
    selector: u32,
    size: u32,
    data: *const c_void,
) -> i32 {
    let address = device_address(channel, is_input, selector);
    unsafe { AudioObjectSetPropertyData(object, &address, 0, std::ptr::null(), size, data) }
}

/// `OSStatus AudioHardwareUnload(void)`
///
/// Nothing is loaded that a program can unload: the device belongs to SDL and
/// outlives this call. Success, because the caller's request - "I am done with
/// the HAL" - is satisfied.
pub unsafe extern "C" fn AudioHardwareUnload() -> i32 {
    SUCCESS
}

/// `OSStatus AudioDeviceCreateIOProcID(AudioObjectID, AudioDeviceIOProc, void *,
/// AudioDeviceIOProcID *)`
///
/// The `IOProcID` handed back is the address of the registration, which is what
/// every other handle in this crate is and for the same reason: the guest
/// compares it against null and passes it back.
///
/// # Safety
///
/// `call` must be a host-callable `AudioDeviceIOProc`, and `out` null or a
/// writable pointer-sized slot.
pub unsafe extern "C" fn AudioDeviceCreateIOProcID(
    object: ObjectId,
    call: Option<IoProc>,
    client_data: *mut c_void,
    out: *mut *mut c_void,
) -> i32 {
    if out.is_null() {
        return crate::status::PARAM_ERROR;
    }
    unsafe { *out = std::ptr::null_mut() };
    let Some(call) = call else {
        return crate::status::PARAM_ERROR;
    };
    let Some(device) = device(object) else {
        return BAD_OBJECT;
    };
    let registration = Arc::new(Mutex::new(Proc {
        device,
        call,
        client_data,
        stream: std::ptr::null_mut(),
        rendered: 0,
    }));
    let handle = Arc::as_ptr(&registration) as *mut c_void;
    PROCS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((handle as usize, registration));
    unsafe { *out = handle };
    crate::trace::say(format_args!("create IOProc on device {object}"), SUCCESS);
    SUCCESS
}

/// The registration `id` names on `object`. A proc passed back with a device it
/// was not created on is refused as Apple's does, rather than started on the
/// device it belongs to.
fn registered(object: ObjectId, id: *mut c_void) -> Result<Arc<Mutex<Proc>>, i32> {
    if device(object).is_none() {
        return Err(BAD_OBJECT);
    }
    let registration = proc_for(id).ok_or(crate::status::INVALID_PARAMETER)?;
    let device = registration
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .device;
    match device.id == object {
        true => Ok(registration),
        false => Err(crate::status::INVALID_PARAMETER),
    }
}

/// `OSStatus AudioDeviceDestroyIOProcID(AudioObjectID, AudioDeviceIOProcID)`
pub unsafe extern "C" fn AudioDeviceDestroyIOProcID(object: ObjectId, id: *mut c_void) -> i32 {
    let registration = match registered(object, id) {
        Ok(registration) => registration,
        Err(status) => return status,
    };
    // SAFETY: `stop` owns the stream and leaves none behind.
    unsafe { stop(&mut registration.lock().unwrap_or_else(|e| e.into_inner())) };
    PROCS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(handle, _)| *handle != id as usize);
    SUCCESS
}

/// `OSStatus AudioDeviceStart(AudioObjectID, AudioDeviceIOProcID)`
pub unsafe extern "C" fn AudioDeviceStart(object: ObjectId, id: *mut c_void) -> i32 {
    let registration = match registered(object, id) {
        Ok(registration) => registration,
        Err(status) => return status,
    };
    let mut started = registration.lock().unwrap_or_else(|e| e.into_inner());
    if !started.stream.is_null() {
        return SUCCESS;
    }
    let device = started.device;
    // SAFETY: `drive` and `capture` have SDL's callback prototype, and `id` is
    // a key into a table that outlives the process rather than a borrow.
    let stream = unsafe {
        match device.id {
            INPUT_DEVICE => sdl3::audio::open_recording(device.spec(), Some(capture), id),
            _ => sdl3::audio::open_playback(device.spec(), Some(drive), id),
        }
    };
    let Some(stream) = stream else {
        crate::trace::say(format_args!("start device {object}"), UNSUPPORTED);
        return UNSUPPORTED;
    };
    started.stream = stream;
    // SAFETY: a stream this crate just opened.
    unsafe { sdl3::audio::resume(stream) };
    crate::trace::say(format_args!("start device {object}"), SUCCESS);
    SUCCESS
}

/// `OSStatus AudioDeviceStop(AudioObjectID, AudioDeviceIOProcID)`
pub unsafe extern "C" fn AudioDeviceStop(object: ObjectId, id: *mut c_void) -> i32 {
    let registration = match registered(object, id) {
        Ok(registration) => registration,
        Err(status) => return status,
    };
    // SAFETY: SDL's destroy waits for its own callback, which is the only other
    // caller.
    unsafe { stop(&mut registration.lock().unwrap_or_else(|e| e.into_inner())) };
    SUCCESS
}

/// # Safety
///
/// No other thread may be inside this proc's stream.
unsafe fn stop(registration: &mut Proc) {
    if registration.stream.is_null() {
        return;
    }
    // SAFETY: the caller's obligation; the handle came from `AudioDeviceStart`.
    unsafe { sdl3::audio::destroy(registration.stream) };
    registration.stream = std::ptr::null_mut();
}

fn empty_list() -> AudioBufferList {
    AudioBufferList {
        count: 0,
        buffers: [AudioBuffer {
            channels: 0,
            byte_size: 0,
            data: std::ptr::null_mut(),
        }; MAX_BUFFERS],
    }
}

/// One interleaved buffer over `bytes`.
fn list_over(channels: u32, bytes: &mut [u8]) -> AudioBufferList {
    let mut list = empty_list();
    list.count = 1;
    list.buffers[0].channels = channels;
    list.buffers[0].byte_size = bytes.len() as u32;
    list.buffers[0].data = bytes.as_mut_ptr().cast();
    list
}

/// The time an `IOProc` is told it is, `frames` into its stream.
fn timestamp(frames: u64) -> TimeStamp {
    TimeStamp {
        sample_time: frames as f64,
        host_time: crate::host_time(),
        rate_scalar: 1.0,
        // `kAudioTimeStampSampleTimeValid | kAudioTimeStampHostTimeValid`.
        flags: 1 | 2,
        ..TimeStamp::default()
    }
}

/// SDL wants more bytes: call the guest's `IOProc` for them.
///
/// The same shape as [`crate::render::feed`], and the difference is the
/// interface: an `IOProc` is handed the output buffer list directly rather than
/// through a render callback, and gets three timestamps because a device that
/// also records has an input time.
///
/// # Safety
///
/// `userdata` is the `IOProcID` [`AudioDeviceStart`] passed, and SDL calls this
/// on its own audio thread.
unsafe extern "C" fn drive(
    userdata: *mut c_void,
    stream: sdl3::AudioStream,
    additional: i32,
    _total: i32,
) {
    if additional <= 0 {
        return;
    }
    let Some(registration) = proc_for(userdata) else {
        return;
    };
    // Held across the guest's call for the reason `render::feed` holds its own:
    // `AudioDeviceStop` takes it too, so a guest cannot free what it is being
    // asked to fill.
    let mut registration = registration.lock().unwrap_or_else(|e| e.into_inner());
    if registration.stream.is_null() {
        return;
    }
    // On SDL's thread, as in `render::feed`.
    let _darwin = darwin::tls::DarwinThread::enter();
    let device = registration.device;
    let frame_bytes = device.frame_bytes();
    let per_call = device.buffer_frames() as usize;
    let mut wanted = additional as usize;
    let mut scratch = vec![0u8; per_call * frame_bytes];
    let empty = empty_list();
    while wanted > 0 {
        let frames = (wanted / frame_bytes).clamp(1, per_call);
        let filled = &mut scratch[..frames * frame_bytes];
        filled.fill(0);
        let mut list = list_over(device.channels, filled);
        let now = timestamp(registration.rendered);
        // SAFETY: `call` is a host-callable pointer this runtime was handed for
        // the guest's function, and every structure it is given outlives the
        // call. The input list is empty, as it is for a device with no input
        // stream.
        let status = unsafe {
            (registration.call)(
                device.id,
                &now,
                &empty,
                &now,
                &mut list,
                &now,
                registration.client_data,
            )
        };
        if status != SUCCESS {
            filled.fill(0);
        }
        registration.rendered += frames as u64;
        // SAFETY: a stream this crate opened, and a live slice.
        unsafe { sdl3::audio::put(stream, filled) };
        wanted = wanted.saturating_sub(filled.len());
    }
}

/// SDL has captured more bytes: hand the guest's `IOProc` every whole buffer of
/// them.
///
/// Whole buffers only, of the size the program set: it sized its own ring
/// buffer and converter to that, and a short one would be read as a dropout.
/// What is left over stays in SDL's stream until the next call.
///
/// # Safety
///
/// As [`drive`].
unsafe extern "C" fn capture(
    userdata: *mut c_void,
    stream: sdl3::AudioStream,
    _additional: i32,
    _total: i32,
) {
    let Some(registration) = proc_for(userdata) else {
        return;
    };
    // Held across the guest's call, as in `drive`.
    let mut registration = registration.lock().unwrap_or_else(|e| e.into_inner());
    // On SDL's thread, as in `drive`.
    let _darwin = darwin::tls::DarwinThread::enter();
    if registration.stream.is_null() {
        return;
    }
    let device = registration.device;
    let frames = device.buffer_frames() as usize;
    let mut captured = vec![0u8; frames * device.frame_bytes()];
    // SAFETY: a stream this crate opened.
    while unsafe { sdl3::audio::available(stream) } >= captured.len() {
        // SAFETY: as above, and a live buffer.
        if unsafe { sdl3::audio::get(stream, &mut captured) } < captured.len() {
            break;
        }
        let list = list_over(device.channels, &mut captured);
        let mut output = empty_list();
        let now = timestamp(registration.rendered);
        // SAFETY: as in `drive`. The output list is empty, as it is for a
        // device with no output stream.
        unsafe {
            (registration.call)(
                device.id,
                &now,
                &list,
                &now,
                &mut output,
                &now,
                registration.client_data,
            )
        };
        registration.rendered += frames as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(object: ObjectId, selector: u32, scope: u32) -> Option<Vec<u8>> {
        let address = Address {
            selector,
            scope,
            element: 0,
        };
        let mut size = 0u32;
        // SAFETY: a live address and a writable size, with no data buffer.
        let status = unsafe {
            AudioObjectGetPropertyDataSize(object, &address, 0, std::ptr::null(), &mut size)
        };
        if status != SUCCESS {
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        // SAFETY: as above, with a buffer of the size just asked for.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                buffer.as_mut_ptr().cast(),
            )
        };
        (status == SUCCESS).then_some(buffer)
    }

    /// The system object names a device, and that device is the one this crate
    /// answers about - a caller follows exactly this chain.
    #[test]
    fn the_default_output_device_is_the_one_that_answers() {
        let id = ask(
            SYSTEM_OBJECT,
            selector::DEFAULT_OUTPUT_DEVICE,
            scope::GLOBAL,
        )
        .expect("a default output device");
        assert_eq!(id, as_bytes(&OUTPUT_DEVICE));
        assert!(ask(OUTPUT_DEVICE, selector::NOMINAL_SAMPLE_RATE, scope::GLOBAL).is_some());
    }

    /// FMOD in Unity 5 asks through the pre-10.5 spellings, and an unbound one
    /// is its first call jumping to address zero.
    #[test]
    fn the_legacy_spellings_reach_the_same_properties() {
        let mut device: ObjectId = 0;
        let mut size = size_of::<ObjectId>() as u32;
        // SAFETY: a writable size and a buffer of that size.
        let status = unsafe {
            AudioHardwareGetProperty(
                selector::DEFAULT_OUTPUT_DEVICE,
                &mut size,
                (&raw mut device).cast(),
            )
        };
        assert_eq!(status, SUCCESS);
        assert_eq!(device, OUTPUT_DEVICE);

        let (mut size, mut writable) = (0u32, 0u8);
        // SAFETY: two writable locals.
        let status = unsafe {
            AudioDeviceGetPropertyInfo(
                OUTPUT_DEVICE,
                0,
                0,
                selector::BUFFER_FRAME_SIZE,
                &mut size,
                &mut writable,
            )
        };
        assert_eq!(status, SUCCESS);
        assert_eq!(size as usize, size_of::<u32>());
        assert_eq!(writable, 1);
        for name in [
            "AudioHardwareGetPropertyInfo",
            "AudioHardwareGetProperty",
            "AudioHardwareAddPropertyListener",
            "AudioDeviceGetPropertyInfo",
            "AudioDeviceGetProperty",
            "AudioDeviceSetProperty",
        ] {
            assert!(crate::names().contains(&name), "{name} must be published");
        }
    }

    /// An unknown property is refused *and* reports a size of zero, so a caller
    /// that ignores the status does not read its own uninitialised buffer.
    #[test]
    fn an_unknown_property_is_refused_and_leaves_no_size_behind() {
        let address = Address {
            selector: fourcc(b"zzzz"),
            scope: scope::GLOBAL,
            element: 0,
        };
        let mut size = 1234u32;
        // SAFETY: a live address and a writable size.
        let status = unsafe {
            AudioObjectGetPropertyData(
                OUTPUT_DEVICE,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, UNKNOWN_PROPERTY);
        assert_eq!(size, 0);
    }

    /// An object nothing knows about is `kAudioHardwareBadObjectError` rather
    /// than an unknown property: the two send a caller looking in different
    /// places.
    #[test]
    fn an_unknown_object_is_a_bad_object() {
        let address = Address {
            selector: selector::DEVICE_IS_ALIVE,
            scope: scope::GLOBAL,
            element: 0,
        };
        let mut size = 4u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                99,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, BAD_OBJECT);
    }

    /// Feral's Wwise sink sizes its speaker map from the preferred layout and
    /// reads its own stack when the property is refused.
    #[test]
    fn the_preferred_layout_describes_every_output_channel() {
        let layout = ask(
            OUTPUT_DEVICE,
            selector::PREFERRED_CHANNEL_LAYOUT,
            scope::OUTPUT,
        )
        .expect("answered");
        let word = |at: usize| u32::from_ne_bytes(layout[at..at + 4].try_into().unwrap());
        let channels = output().channels;
        assert_eq!(word(0), 0, "kAudioChannelLayoutTag_UseChannelDescriptions");
        assert_eq!(word(8), channels);
        assert_eq!(layout.len(), 12 + 20 * channels as usize);
        if channels == 2 {
            assert_eq!((word(12), word(32)), (1, 2), "left, then right");
        }
        assert!(
            ask(
                OUTPUT_DEVICE,
                selector::PREFERRED_CHANNEL_LAYOUT,
                scope::INPUT
            )
            .is_none()
        );
    }

    /// Each device has buffers on its own side only, and that is how a caller
    /// counts its channels - and so tells a speaker from a microphone.
    #[test]
    fn each_device_reports_channels_on_its_own_side_only() {
        let speaker = output();
        assert_eq!(
            stream_configuration(speaker, scope::INPUT)[..4],
            *as_bytes(&0u32)
        );
        assert_eq!(
            stream_configuration(speaker, scope::OUTPUT)[..4],
            *as_bytes(&1u32)
        );
        let microphone = Device::new(
            INPUT_DEVICE,
            scope::INPUT,
            None,
            None,
            "Default Input",
            "test",
        );
        assert_eq!(
            stream_configuration(&microphone, scope::OUTPUT)[..4],
            *as_bytes(&0u32)
        );
        assert_eq!(
            stream_configuration(&microphone, scope::INPUT)[..4],
            *as_bytes(&1u32)
        );
    }

    /// The default input is the recording device when the host has one, and
    /// `kAudioObjectUnknown` when it does not; either way the device list
    /// agrees with it.
    #[test]
    fn the_default_input_device_is_listed_when_present() {
        let default =
            ask(SYSTEM_OBJECT, selector::DEFAULT_INPUT_DEVICE, scope::GLOBAL).expect("answered");
        let devices = ask(SYSTEM_OBJECT, selector::DEVICES, scope::GLOBAL).expect("answered");
        match input() {
            Some(_) => {
                assert_eq!(default, as_bytes(&INPUT_DEVICE));
                assert_eq!(devices[4..8], *as_bytes(&INPUT_DEVICE));
                assert_eq!(
                    ask(INPUT_DEVICE, selector::STREAMS, scope::INPUT),
                    Some(as_bytes(&INPUT_DEVICE).to_vec())
                );
            }
            None => {
                assert_eq!(default, as_bytes(&0u32));
                assert_eq!(devices.len(), 4);
            }
        }
    }

    /// WebRTC refuses a device whose hog mode it cannot read.
    #[test]
    fn nobody_hogs_the_output_device() {
        assert_eq!(
            ask(OUTPUT_DEVICE, selector::HOG_MODE, scope::OUTPUT),
            Some(as_bytes(&-1i32).to_vec())
        );
    }

    /// An `IOProc` passed back with a device it was not created on is refused
    /// rather than started on its own.
    #[test]
    fn an_ioproc_belongs_to_its_device() {
        unsafe extern "C" fn silent(
            _: ObjectId,
            _: *const TimeStamp,
            _: *const AudioBufferList,
            _: *const TimeStamp,
            _: *mut AudioBufferList,
            _: *const TimeStamp,
            _: *mut c_void,
        ) -> i32 {
            SUCCESS
        }
        let mut id = std::ptr::null_mut();
        // SAFETY: a host function and a writable slot.
        let status = unsafe {
            AudioDeviceCreateIOProcID(OUTPUT_DEVICE, Some(silent), std::ptr::null_mut(), &mut id)
        };
        assert_eq!(status, SUCCESS);
        assert_ne!(unsafe { AudioDeviceStop(INPUT_DEVICE, id) }, SUCCESS);
        assert_eq!(
            unsafe { AudioDeviceDestroyIOProcID(OUTPUT_DEVICE, id) },
            SUCCESS
        );
    }

    /// The buffer size is the one property a program may set, in frames or in
    /// bytes, and it reads back as what was set.
    #[test]
    fn the_buffer_size_is_settable_and_reads_back() {
        let address = Address {
            selector: selector::BUFFER_FRAME_SIZE,
            scope: scope::OUTPUT,
            element: 0,
        };
        let wanted = 1024u32;
        // SAFETY: a live address and a live `UInt32`.
        let status = unsafe {
            AudioObjectSetPropertyData(
                OUTPUT_DEVICE,
                &address,
                0,
                std::ptr::null(),
                4,
                (&wanted as *const u32).cast(),
            )
        };
        assert_eq!(status, SUCCESS);
        assert_eq!(
            ask(OUTPUT_DEVICE, selector::BUFFER_FRAME_SIZE, scope::OUTPUT),
            Some(as_bytes(&wanted).to_vec())
        );
        // The same size in bytes is the same setting.
        let bytes = 256 * output().frame_bytes() as u32;
        let in_bytes = Address {
            selector: selector::BUFFER_SIZE,
            ..address
        };
        let status = unsafe {
            AudioObjectSetPropertyData(
                OUTPUT_DEVICE,
                &in_bytes,
                0,
                std::ptr::null(),
                4,
                (&bytes as *const u32).cast(),
            )
        };
        assert_eq!(status, SUCCESS);
        assert_eq!(
            ask(OUTPUT_DEVICE, selector::BUFFER_FRAME_SIZE, scope::OUTPUT),
            Some(as_bytes(&256u32).to_vec())
        );
        // And back, so the order tests run in cannot matter.
        let default = 512u32;
        unsafe {
            AudioObjectSetPropertyData(
                OUTPUT_DEVICE,
                &address,
                0,
                std::ptr::null(),
                4,
                (&default as *const u32).cast(),
            )
        };
    }
}
