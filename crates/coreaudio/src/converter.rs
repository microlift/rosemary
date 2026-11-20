use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use crate::aac::{self, AacDecoder};
use crate::render::{AudioBufferList, MAX_BUFFERS};
use crate::status::{PARAM_ERROR, SUCCESS};
use crate::stream::{Format, sdl_spec};

/// `kAudioConverterErr_FormatNotSupported`, `'fmt?'`.
const FORMAT_NOT_SUPPORTED: i32 = i32::from_be_bytes(*b"fmt?");
/// `kAudioConverterErr_PropertyNotSupported`, `'prop'`.
const PROPERTY_NOT_SUPPORTED: i32 = i32::from_be_bytes(*b"prop");
/// `kAudioConverterErr_UnspecifiedError`, `'what'`.
const UNSPECIFIED: i32 = i32::from_be_bytes(*b"what");

const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

/// The `AudioConverterPropertyID`s this answers.
mod property {
    use super::fourcc;
    pub const CURRENT_INPUT_FORMAT: u32 = fourcc(b"ciff");
    pub const CURRENT_OUTPUT_FORMAT: u32 = fourcc(b"cofm");
    /// `kAudioConverterPropertyMaximumOutputPacketSize`.
    pub const MAXIMUM_OUTPUT_PACKET_SIZE: u32 = fourcc(b"xops");
    /// `kAudioConverterPropertyCalculateInputBufferSize`.
    pub const CALCULATE_INPUT_BUFFER_SIZE: u32 = fourcc(b"cibs");
    /// `kAudioConverterPropertyCalculateOutputBufferSize`.
    pub const CALCULATE_OUTPUT_BUFFER_SIZE: u32 = fourcc(b"cobs");
    /// `kAudioConverterSampleRateConverterQuality`, which SDL has no dial for.
    pub const SAMPLE_RATE_CONVERTER_QUALITY: u32 = fourcc(b"srcq");
    /// `kAudioConverterPropertyMaximumInputPacketSize`.
    pub const MAXIMUM_INPUT_PACKET_SIZE: u32 = fourcc(b"xips");
    /// `kAudioConverterDecompressionMagicCookie`.
    pub const DECOMPRESSION_MAGIC_COOKIE: u32 = fourcc(b"dmgc");
}

/// `AudioStreamPacketDescription`: where one compressed packet falls inside
/// the buffer an `AudioConverterComplexInputDataProc` returned.
#[repr(C)]
#[derive(Clone, Copy)]
struct PacketDescription {
    start_offset: i64,
    #[allow(dead_code)]
    variable_frames_in_packet: u32,
    data_byte_size: u32,
}

const _: () = assert!(size_of::<PacketDescription>() == 16);

struct Converter {
    source: Format,
    destination: Format,
    /// SDL's converting stream, attached to no device. For a compressed
    /// source this starts null: the spec it converts *from* is the AAC
    /// decoder's own output, not known until something has decoded, so the
    /// stream is made lazily on the first frame rather than guessed at here.
    stream: sdl3::AudioStream,
    /// Set for a compressed source; `None` for PCM-to-PCM, which SDL alone
    /// handles.
    aac: Option<Box<AacDecoder>>,
    /// The `(rate, channels)` `stream` now converts from, for a compressed
    /// source: the decoder's, which can change at a frame boundary.
    decoded: Option<(u32, u32)>,
    /// The magic cookie last given through `kAudioConverterDecompressionMagicCookie`,
    /// kept only so `AudioConverterGetProperty` can hand the same bytes back.
    cookie: Vec<u8>,
    /// The bytes the input callback last handed over, kept alive until the next
    /// call: Apple's contract lets a caller point `mData` at storage it reuses
    /// only after the *next* invocation, and SDL has already copied by then.
    held: Vec<u8>,
    /// Converted frames on their way out, interleaved as SDL produces them.
    /// Kept on the converter rather than allocated per call, because every one
    /// of those calls is on an audio thread.
    scratch: Vec<u8>,
}

// SAFETY: `stream` is SDL's own handle, used only through `sdl3::audio`, and it
// is behind the mutex below.
unsafe impl Send for Converter {}

static CONVERTERS: Mutex<Vec<(usize, Arc<Mutex<Converter>>)>> = Mutex::new(Vec::new());

fn converter_for(handle: *mut c_void) -> Option<Arc<Mutex<Converter>>> {
    CONVERTERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(h, _)| *h == handle as usize)
        .map(|(_, c)| Arc::clone(c))
}

/// `OSStatus AudioConverterNew(const AudioStreamBasicDescription *, const
/// AudioStreamBasicDescription *, AudioConverterRef *)`
///
/// # Safety
///
/// Both descriptions must be readable, and `out` null or a writable
/// pointer-sized slot.
pub unsafe extern "C" fn AudioConverterNew(
    source: *const Format,
    destination: *const Format,
    out: *mut *mut c_void,
) -> i32 {
    if out.is_null() {
        return PARAM_ERROR;
    }
    unsafe { *out = std::ptr::null_mut() };
    if source.is_null() || destination.is_null() {
        return PARAM_ERROR;
    }
    let (source, destination) = unsafe { (*source, *destination) };
    let converter = if aac::is_aac(source.format_id) {
        // The destination must still be something SDL can produce; only the
        // *source* side is decoded through FFmpeg rather than SDL.
        if sdl_spec(&destination).is_none() {
            return FORMAT_NOT_SUPPORTED;
        }
        let Ok(mut decoder) = AacDecoder::new() else {
            return FORMAT_NOT_SUPPORTED;
        };
        // Apple's decoder starts from the description alone, and Roblox never
        // sets a magic cookie on its converter: open from the description
        // now, and let a cookie set later reopen it. A description that does
        // not say enough is left for a cookie to complete.
        if let Some(asc) = aac::asc_for_description(
            source.format_id,
            source.sample_rate,
            source.channels_per_frame,
            source.frames_per_packet,
        ) && decoder.set_extradata(&asc).is_err()
        {
            return FORMAT_NOT_SUPPORTED;
        }
        Converter {
            source,
            destination,
            stream: std::ptr::null_mut(),
            aac: Some(Box::new(decoder)),
            decoded: None,
            cookie: Vec::new(),
            held: Vec::new(),
            scratch: Vec::new(),
        }
    } else {
        let (Some(from), Some(to)) = (sdl_spec(&source), sdl_spec(&destination)) else {
            return FORMAT_NOT_SUPPORTED;
        };
        let Some(stream) = sdl3::audio::create_converter(from, to) else {
            return UNSPECIFIED;
        };
        Converter {
            source,
            destination,
            stream,
            aac: None,
            decoded: None,
            cookie: Vec::new(),
            held: Vec::new(),
            scratch: Vec::new(),
        }
    };
    let converter = Arc::new(Mutex::new(converter));
    let handle = Arc::as_ptr(&converter) as *mut c_void;
    CONVERTERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((handle as usize, converter));
    unsafe { *out = handle };
    SUCCESS
}

/// `OSStatus AudioConverterNewSpecific(..., UInt32, const AudioClassDescription
/// *, AudioConverterRef *)`
///
/// The class list names *which codec implementation* to use - hardware or
/// software - and there is one implementation here either way (FFmpeg's
/// software `aac` decoder, for a compressed source), so it is ignored
/// rather than refused: a format this cannot convert at all is refused by
/// [`AudioConverterNew`] on its own account, one line down.
///
/// # Safety
///
/// As [`AudioConverterNew`].
pub unsafe extern "C" fn AudioConverterNewSpecific(
    source: *const Format,
    destination: *const Format,
    _class_count: u32,
    _classes: *const c_void,
    out: *mut *mut c_void,
) -> i32 {
    unsafe { AudioConverterNew(source, destination, out) }
}

/// `OSStatus AudioConverterDispose(AudioConverterRef)`
pub unsafe extern "C" fn AudioConverterDispose(handle: *mut c_void) -> i32 {
    let Some(converter) = converter_for(handle) else {
        return PARAM_ERROR;
    };
    {
        let mut converter = converter.lock().unwrap_or_else(|e| e.into_inner());
        if !converter.stream.is_null() {
            // SAFETY: a stream this crate made, with no device and so no other
            // thread inside it.
            unsafe { sdl3::audio::destroy(converter.stream) };
            converter.stream = std::ptr::null_mut();
        }
    }
    CONVERTERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(h, _)| *h != handle as usize);
    SUCCESS
}

/// `OSStatus AudioConverterReset(AudioConverterRef)`: drop whatever is in
/// flight, which is what a caller does after a seek.
pub unsafe extern "C" fn AudioConverterReset(handle: *mut c_void) -> i32 {
    let Some(converter) = converter_for(handle) else {
        return PARAM_ERROR;
    };
    let mut converter = converter.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(aac) = converter.aac.as_deref_mut() {
        aac.flush();
    }
    if !converter.stream.is_null() {
        // SAFETY: a stream this crate made.
        unsafe { sdl3::audio::clear(converter.stream) };
    }
    converter.held.clear();
    SUCCESS
}

/// The bytes a property answers with, or `None` for one this converter has not
/// got.
fn property(converter: &Converter, id: u32) -> Option<Vec<u8>> {
    let bytes = |value: &[u8]| value.to_vec();
    Some(match id {
        property::CURRENT_INPUT_FORMAT => bytes(as_bytes(&converter.source)),
        property::CURRENT_OUTPUT_FORMAT => bytes(as_bytes(&converter.destination)),
        // One packet is one frame for linear PCM, which is the only thing a
        // PCM-to-PCM converter's destination ever is.
        property::MAXIMUM_OUTPUT_PACKET_SIZE => {
            bytes(as_bytes(&converter.destination.bytes_per_packet))
        }
        // For a compressed source, ISO/IEC 14496-3's bound on one AAC raw
        // data block: 6144 bits (768 bytes) per channel. PCM has no larger
        // packet than one frame either way, so the same answer serves both.
        property::MAXIMUM_INPUT_PACKET_SIZE => {
            let channels = converter.source.channels_per_frame.max(1);
            let size = match converter.aac.is_some() {
                true => 768 * channels,
                false => converter.source.bytes_per_packet,
            };
            bytes(as_bytes(&size))
        }
        // Readable (as empty) as soon as the converter exists, not only
        // after a cookie has been set: a caller may probe the property
        // before deciding whether it has one to give.
        property::DECOMPRESSION_MAGIC_COOKIE if converter.aac.is_some() => converter.cookie.clone(),
        _ => return None,
    })
}

fn as_bytes<T: Copy>(value: &T) -> &[u8] {
    // SAFETY: `T` is a `Copy` structure and the slice borrows it for no longer
    // than the call.
    unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
}

/// `OSStatus AudioConverterGetProperty(AudioConverterRef,
/// AudioConverterPropertyID, UInt32 *, void *)`
///
/// # Safety
///
/// `size` must be a writable `UInt32` and `data` null or writable for `*size`.
pub unsafe extern "C" fn AudioConverterGetProperty(
    handle: *mut c_void,
    id: u32,
    size: *mut u32,
    data: *mut c_void,
) -> i32 {
    let Some(converter) = converter_for(handle) else {
        return PARAM_ERROR;
    };
    if size.is_null() {
        return PARAM_ERROR;
    }
    let converter = converter.lock().unwrap_or_else(|e| e.into_inner());
    let Some(bytes) = property(&converter, id) else {
        unsafe { *size = 0 };
        return PROPERTY_NOT_SUPPORTED;
    };
    let capacity = unsafe { *size } as usize;
    if !data.is_null() {
        let taken = capacity.min(bytes.len());
        // SAFETY: the caller says `data` is writable for `capacity` bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), data.cast::<u8>(), taken) };
        unsafe { *size = taken as u32 };
    } else {
        unsafe { *size = bytes.len() as u32 };
    }
    SUCCESS
}

/// `OSStatus AudioConverterGetPropertyInfo(AudioConverterRef,
/// AudioConverterPropertyID, UInt32 *, Boolean *)`
///
/// # Safety
///
/// Both out-parameters must be null or writable.
pub unsafe extern "C" fn AudioConverterGetPropertyInfo(
    handle: *mut c_void,
    id: u32,
    size: *mut u32,
    writable: *mut u8,
) -> i32 {
    let Some(converter) = converter_for(handle) else {
        return PARAM_ERROR;
    };
    let converter = converter.lock().unwrap_or_else(|e| e.into_inner());
    let found = property(&converter, id);
    if !size.is_null() {
        unsafe { *size = found.as_ref().map_or(0, |b| b.len() as u32) };
    }
    if !writable.is_null() {
        // Nothing here is settable after the converter is made except the
        // decompression magic cookie of a compressed source, which Apple's
        // own converter lets a caller replace mid-stream too.
        unsafe {
            *writable =
                u8::from(id == property::DECOMPRESSION_MAGIC_COOKIE && converter.aac.is_some())
        };
    }
    match found.is_some() {
        true => SUCCESS,
        false => PROPERTY_NOT_SUPPORTED,
    }
}

/// `OSStatus AudioConverterSetProperty(AudioConverterRef,
/// AudioConverterPropertyID, UInt32, const void *)`
///
/// The quality dials are accepted and dropped, because SDL's resampler has one
/// quality and a caller that asked for a better one is not owed an error - it
/// gets conversion either way. The decompression magic cookie is the one
/// property that actually changes what this converter can do (it is what
/// lets a compressed source decode at all). Everything else is refused
/// rather than silently ignored: a channel map or a priming setting that
/// did nothing would change what comes out.
///
/// # Safety
///
/// `data` must be null or readable for `size` bytes.
pub unsafe extern "C" fn AudioConverterSetProperty(
    handle: *mut c_void,
    id: u32,
    size: u32,
    data: *const c_void,
) -> i32 {
    let Some(converter) = converter_for(handle) else {
        return PARAM_ERROR;
    };
    match id {
        property::SAMPLE_RATE_CONVERTER_QUALITY
        | property::CALCULATE_INPUT_BUFFER_SIZE
        | property::CALCULATE_OUTPUT_BUFFER_SIZE => SUCCESS,
        property::DECOMPRESSION_MAGIC_COOKIE => {
            let mut converter = converter.lock().unwrap_or_else(|e| e.into_inner());
            let Some(aac) = converter.aac.as_deref_mut() else {
                return PROPERTY_NOT_SUPPORTED;
            };
            if data.is_null() || size == 0 {
                return PARAM_ERROR;
            }
            // SAFETY: the caller says `data` is readable for `size` bytes.
            let cookie = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size as usize) };
            match aac.set_extradata(cookie) {
                Ok(()) => {
                    converter.cookie = cookie.to_vec();
                    SUCCESS
                }
                Err(_) => FORMAT_NOT_SUPPORTED,
            }
        }
        _ => PROPERTY_NOT_SUPPORTED,
    }
}

/// `AudioConverterComplexInputDataProc`: the callback that supplies input.
type InputProc = unsafe extern "C" fn(
    *mut c_void,
    *mut u32,
    *mut AudioBufferList,
    *mut *mut c_void,
    *mut c_void,
) -> i32;

/// `OSStatus AudioConverterFillComplexBuffer(AudioConverterRef,
/// AudioConverterComplexInputDataProc, void *, UInt32 *, AudioBufferList *,
/// AudioStreamPacketDescription *)`
///
/// The pull loop, and the shape of it is the interface: the caller says how
/// many output packets it wants, this asks the callback for input until SDL has
/// produced that much, and reports back how many packets were actually
/// produced. A callback that supplies nothing ends the loop - that is how a
/// caller signals end of stream - and the count written back is what it reads
/// to know the stream is over.
///
/// # Safety
///
/// `packets` must be a writable `UInt32`, `output` a writable `AudioBufferList`
/// whose buffers are writable for their own `mDataByteSize`, and `input` a
/// host-callable input procedure.
pub unsafe extern "C" fn AudioConverterFillComplexBuffer(
    handle: *mut c_void,
    input: Option<InputProc>,
    input_data: *mut c_void,
    packets: *mut u32,
    output: *mut AudioBufferList,
    _descriptions: *mut c_void,
) -> i32 {
    if packets.is_null() || output.is_null() {
        return PARAM_ERROR;
    }
    let Some(converter) = converter_for(handle) else {
        return PARAM_ERROR;
    };
    let Some(input) = input else {
        unsafe { *packets = 0 };
        return PARAM_ERROR;
    };
    let mut converter = converter.lock().unwrap_or_else(|e| e.into_inner());

    let out = unsafe { &mut *output };
    let destination = converter.destination;
    let channels = destination.channels_per_frame.max(1) as usize;
    // How many buffers the output is spread over, and how much of a frame each
    // one holds: a non-interleaved format puts one channel in each, and an
    // interleaved one puts the whole frame in the first.
    let planes = match destination.non_interleaved() {
        true => channels.min(out.count as usize),
        false => 1,
    };
    let plane_frame = destination.bytes_per_frame.max(1) as usize;
    if out.count == 0 || out.count as usize > MAX_BUFFERS || planes == 0 {
        unsafe { *packets = 0 };
        return PARAM_ERROR;
    }
    if out.buffers[..planes].iter().any(|b| b.data.is_null()) {
        unsafe { *packets = 0 };
        return PARAM_ERROR;
    }

    // In frames, which for linear PCM is also in packets: every buffer has to
    // hold the whole of what is produced, so the smallest of them is the room.
    let mut room = unsafe { *packets } as usize;
    for buffer in &out.buffers[..planes] {
        room = room.min(buffer.byte_size as usize / plane_frame);
    }

    // What SDL hands back is interleaved whatever the destination's layout is,
    // so it is taken into this scratch and dealt out to the buffers below.
    let stream_frame = match destination.non_interleaved() {
        true => plane_frame * channels,
        false => plane_frame,
    };
    converter.scratch.clear();
    converter.scratch.resize(room * stream_frame, 0);

    let mut produced = 0usize;
    while produced < converter.scratch.len() {
        // A compressed source has no stream yet until the first frame comes
        // out of FFmpeg and says what spec it converts from - `feed` makes
        // it then. Until that happens `sdl3::audio::available` has nothing
        // live to ask.
        let available = match converter.stream.is_null() {
            true => 0,
            // SAFETY: a stream this crate made.
            false => unsafe { sdl3::audio::available(converter.stream) },
        };
        if available == 0 && !feed(&mut converter, input, input_data) {
            break;
        }
        if converter.stream.is_null() {
            // Nothing has decoded yet and the input is exhausted: there is
            // no stream to pull from and nothing more `feed` can do.
            break;
        }
        let taken =
            unsafe { sdl3::audio::get(converter.stream, &mut converter.scratch[produced..]) };
        if taken == 0 {
            break;
        }
        produced += taken;
    }
    // A partial frame is no frame: half of one written into a caller's buffer
    // is a click, and the count reported back would not account for it.
    let frames = produced / stream_frame;

    for (plane, buffer) in out.buffers[..planes].iter_mut().enumerate() {
        // SAFETY: the caller says the buffer is writable for `mDataByteSize`,
        // and `room` was measured against that.
        let into = unsafe {
            std::slice::from_raw_parts_mut(buffer.data.cast::<u8>(), frames * plane_frame)
        };
        match destination.non_interleaved() {
            true => unweave(&converter.scratch, into, plane, plane_frame, stream_frame),
            false => into.copy_from_slice(&converter.scratch[..frames * plane_frame]),
        }
        buffer.byte_size = (frames * plane_frame) as u32;
    }

    unsafe { *packets = frames as u32 };
    SUCCESS
}

/// Lay one channel's own buffer into its place in an interleaved stream.
///
/// `plane_frame` is what one frame takes in the channel's buffer and
/// `stream_frame` what it takes in the stream, so the channel's slot is the
/// `plane`th of them. The two are what a non-interleaved description means:
/// `mBytesPerFrame` counts one channel, and the stream SDL takes counts all of
/// them.
fn weave(from: &[u8], into: &mut [u8], plane: usize, plane_frame: usize, stream_frame: usize) {
    let at = plane * plane_frame;
    for (frame, sample) in from.chunks_exact(plane_frame).enumerate() {
        let to = frame * stream_frame + at;
        if to + plane_frame > into.len() {
            return;
        }
        into[to..to + plane_frame].copy_from_slice(sample);
    }
}

/// Take one channel back out of an interleaved stream: [`weave`] the other way,
/// for the buffers a caller hands a converter to fill.
fn unweave(from: &[u8], into: &mut [u8], plane: usize, plane_frame: usize, stream_frame: usize) {
    let at = plane * plane_frame;
    for (frame, sample) in into.chunks_exact_mut(plane_frame).enumerate() {
        let take = frame * stream_frame + at;
        if take + plane_frame > from.len() {
            return;
        }
        sample.copy_from_slice(&from[take..take + plane_frame]);
    }
}

/// Ask the caller's input procedure for more, and give it to SDL - directly,
/// for a PCM source, or through FFmpeg first for a compressed one.
///
/// `false` when the source has nothing more, which is end of stream.
fn feed(converter: &mut Converter, input: InputProc, input_data: *mut c_void) -> bool {
    match converter.aac.is_some() {
        true => feed_aac(converter, input, input_data),
        false => feed_pcm(converter, input, input_data),
    }
}

/// Points the SDL stream at the AAC decoder's output for one frame (always
/// interleaved `f32`, at the frame's rate and channel count): made on the
/// first frame, and re-pointed when a later frame differs, which SDL applies
/// to data put from then on only. `false` for a spec SDL cannot convert.
fn ensure_decoded_stream(converter: &mut Converter, rate: u32, channels: u32) -> bool {
    if converter.decoded == Some((rate, channels)) && !converter.stream.is_null() {
        return true;
    }
    let (Ok(freq), Ok(channels_i32)) = (i32::try_from(rate), i32::try_from(channels)) else {
        return false;
    };
    let from = sdl3::AudioSpec {
        format: sdl3::audio::format::F32,
        channels: channels_i32,
        freq,
    };
    let Some(to) = sdl_spec(&converter.destination) else {
        return false;
    };
    let made = match converter.stream.is_null() {
        true => match sdl3::audio::create_converter(from, to) {
            Some(stream) => {
                converter.stream = stream;
                true
            }
            None => false,
        },
        // SAFETY: a stream this crate made.
        false => unsafe { sdl3::audio::set_format(converter.stream, from, to) },
    };
    if made {
        converter.decoded = Some((rate, channels));
    }
    made
}

/// `f32` samples as the native-endian bytes SDL's stream takes.
fn f32_bytes(samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_ne_bytes());
    }
    bytes
}

/// Moves every frame FFmpeg has ready now into the SDL stream, one frame at
/// a time so each goes in under its own spec.
fn drain_decoded(converter: &mut Converter) {
    loop {
        let mut samples = Vec::new();
        let Some(aac) = converter.aac.as_deref_mut() else {
            return;
        };
        let Ok(Some((rate, channels))) = aac.receive_interleaved(&mut samples) else {
            return;
        };
        if !ensure_decoded_stream(converter, rate, channels) {
            // Dropped, not converted as if it were some other spec.
            continue;
        }
        let bytes = f32_bytes(&samples);
        // SAFETY: a stream this crate made (or just made, above).
        unsafe { sdl3::audio::put(converter.stream, &bytes) };
    }
}

/// [`feed`] for a compressed source: pulls AAC packets from the caller's
/// input procedure and decodes each through FFmpeg into the SDL stream
/// that brings it to the destination format.
///
/// `AudioStreamPacketDescription`s are read when the procedure supplies
/// them, which is how a real CoreAudio caller marks where each packet
/// starts in a buffer it may have concatenated several into. A procedure
/// that supplies none is read as one packet, the one shape this can still
/// make sense of without them.
///
/// Each packet is drained before the next is sent: FFmpeg holds only one
/// unsent packet, and refuses (`EAGAIN`) a third before a receive.
fn feed_aac(converter: &mut Converter, input: InputProc, input_data: *mut c_void) -> bool {
    let mut wanted = 32u32;
    let mut list = AudioBufferList {
        count: 1,
        buffers: [crate::render::AudioBuffer {
            channels: converter.source.channels_per_frame,
            byte_size: 0,
            data: std::ptr::null_mut(),
        }; MAX_BUFFERS],
    };
    let mut descriptions: *mut c_void = std::ptr::null_mut();
    // SAFETY: `input` is a host-callable pointer this runtime was handed for
    // the guest's procedure, and every structure outlives the call.
    let status = unsafe {
        input(
            input_data,
            &mut wanted,
            &mut list,
            &mut descriptions,
            input_data,
        )
    };
    if status != SUCCESS {
        // Apple's contract: an error with no packets is "nothing more for
        // now", not end of stream - Roblox's procedure answers that way
        // after every packet. Nothing is flushed, so the resampler's tail
        // stays queued for the packets still to come.
        return false;
    }
    if list.count == 0 || wanted == 0 || list.buffers[0].data.is_null() {
        // End of stream: the resampler hands back its tail now.
        if !converter.stream.is_null() {
            // SAFETY: a stream this crate made.
            unsafe { sdl3::audio::flush(converter.stream) };
        }
        return false;
    }

    let buffer = list.buffers[0];
    // SAFETY: the guest's procedure says this buffer holds `mDataByteSize`
    // readable bytes.
    let data =
        unsafe { std::slice::from_raw_parts(buffer.data.cast::<u8>(), buffer.byte_size as usize) };
    let packets: &[PacketDescription] = if descriptions.is_null() {
        &[]
    } else {
        // SAFETY: the guest's procedure set this to its own array of
        // `wanted` descriptions, which it owns until the next call - read
        // before this one returns, as every other borrow from `list` is.
        unsafe {
            std::slice::from_raw_parts(descriptions.cast::<PacketDescription>(), wanted as usize)
        }
    };
    let whole = [PacketDescription {
        start_offset: 0,
        variable_frames_in_packet: 0,
        data_byte_size: buffer.byte_size,
    }];
    let packets = match packets {
        [] => &whole[..],
        some => some,
    };
    for description in packets {
        let start = usize::try_from(description.start_offset).unwrap_or(usize::MAX);
        let end = start.saturating_add(description.data_byte_size as usize);
        let Some(packet) = data.get(start..end) else {
            // A description outside the buffer it describes: the caller's
            // data is wrong, and nothing after it can be trusted either.
            return false;
        };
        let Some(aac) = converter.aac.as_deref_mut() else {
            return false;
        };
        // A packet FFmpeg cannot decode is skipped, as a decoder conceals a
        // damaged access unit; it is not the end of the stream. Drained
        // either way, so nothing is left in front of the next packet.
        let _ = aac.send(packet);
        drain_decoded(converter);
    }
    true
}

fn feed_pcm(converter: &mut Converter, input: InputProc, input_data: *mut c_void) -> bool {
    let mut wanted = 4096u32;
    let mut list = AudioBufferList {
        count: 1,
        buffers: [crate::render::AudioBuffer {
            channels: converter.source.channels_per_frame,
            byte_size: 0,
            data: std::ptr::null_mut(),
        }; MAX_BUFFERS],
    };
    // SAFETY: `input` is a host-callable pointer this runtime was handed for
    // the guest's procedure, and both structures outlive the call. The packet
    // description list is null, which is what a caller of a PCM converter
    // passes.
    let status = unsafe {
        input(
            input_data,
            &mut wanted,
            &mut list,
            std::ptr::null_mut(),
            input_data,
        )
    };
    if status != SUCCESS || list.count == 0 {
        // SAFETY: a stream this crate made.
        unsafe { sdl3::audio::flush(converter.stream) };
        return false;
    }
    let channels = converter.source.channels_per_frame.max(1) as usize;
    let planes = match converter.source.non_interleaved() {
        true => channels.min(list.count as usize),
        false => 1,
    };
    let plane_frame = converter.source.bytes_per_frame.max(1) as usize;
    // Frames the procedure supplied, which is the smallest of what it put in
    // each plane and of what it says it supplied in packets.
    let mut frames = wanted as usize * converter.source.frames_per_packet.max(1) as usize;
    for buffer in &list.buffers[..planes] {
        if buffer.data.is_null() {
            frames = 0;
            break;
        }
        frames = frames.min(buffer.byte_size as usize / plane_frame);
    }
    if frames == 0 {
        unsafe { sdl3::audio::flush(converter.stream) };
        return false;
    }

    // SDL takes one interleaved stream, so a planar supply is woven together
    // here rather than put a channel at a time - which would play as the left
    // channel followed by the right.
    let stream_frame = match converter.source.non_interleaved() {
        true => plane_frame * channels,
        false => plane_frame,
    };
    converter.held.clear();
    converter.held.resize(frames * stream_frame, 0);
    for (plane, buffer) in list.buffers[..planes].iter().enumerate() {
        // SAFETY: the guest's procedure says this buffer holds `mDataByteSize`
        // readable bytes, and `frames` was measured against that.
        let from =
            unsafe { std::slice::from_raw_parts(buffer.data.cast::<u8>(), frames * plane_frame) };
        match converter.source.non_interleaved() {
            true => weave(from, &mut converter.held, plane, plane_frame, stream_frame),
            false => converter.held.copy_from_slice(from),
        }
    }
    // SAFETY: a stream this crate made, and a live slice.
    unsafe { sdl3::audio::put(converter.stream, &converter.held) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(rate: f64, channels: u32) -> Format {
        Format {
            sample_rate: rate,
            channels_per_frame: channels,
            bytes_per_frame: 4 * channels,
            bytes_per_packet: 4 * channels,
            ..Format::default()
        }
    }

    /// A compressed format neither SDL nor the AAC path knows is refused at
    /// creation rather than accepted and then silently producing nothing.
    #[test]
    fn an_unrelated_compressed_format_is_refused() {
        let mut handle = std::ptr::null_mut();
        let alac = Format {
            format_id: fourcc(b"alac"),
            ..format(44100.0, 2)
        };
        let pcm = format(48000.0, 2);
        // SAFETY: two live descriptions and a writable slot.
        assert_eq!(
            unsafe { AudioConverterNew(&alac, &pcm, &mut handle) },
            FORMAT_NOT_SUPPORTED
        );
        assert!(handle.is_null());
    }

    /// An AAC source still needs a destination this crate can actually
    /// produce - the FFmpeg decoder only replaces the *source* side of the
    /// conversion, not SDL's part of it.
    #[test]
    fn an_aac_source_still_needs_a_destination_sdl_can_produce() {
        let mut handle = std::ptr::null_mut();
        let aac = Format {
            format_id: fourcc(b"aac "),
            ..format(44100.0, 2)
        };
        let unsupported_destination = Format {
            format_id: fourcc(b"alac"),
            ..format(44100.0, 2)
        };
        // SAFETY: two live descriptions and a writable slot.
        assert_eq!(
            unsafe { AudioConverterNew(&aac, &unsupported_destination, &mut handle) },
            FORMAT_NOT_SUPPORTED
        );
        assert!(handle.is_null());
    }

    /// With a destination SDL can produce, an AAC source either makes a
    /// converter (FFmpeg's software `aac` decoder is on this host) or is
    /// refused exactly as it would have been before this backend existed
    /// (it is not) - never anything else, and never a handle alongside a
    /// failing status.
    #[test]
    fn an_aac_source_is_accepted_or_refused_outright_never_half_made() {
        let mut handle = std::ptr::null_mut();
        let aac = Format {
            format_id: fourcc(b"aac "),
            ..format(44100.0, 2)
        };
        let pcm = format(44100.0, 2);
        // SAFETY: two live descriptions and a writable slot.
        let status = unsafe { AudioConverterNew(&aac, &pcm, &mut handle) };
        match status {
            SUCCESS => {
                assert!(!handle.is_null());
                // SAFETY: a converter this call just made.
                assert_eq!(unsafe { AudioConverterDispose(handle) }, SUCCESS);
            }
            FORMAT_NOT_SUPPORTED => assert!(handle.is_null(), "no FFmpeg on this host"),
            other => panic!("unexpected status {other}"),
        }
    }

    /// Two planar channels become one interleaved stream and come back out of
    /// it, which is the whole of what a non-interleaved format asks for.
    #[test]
    fn a_planar_channel_weaves_into_its_own_slot_and_back() {
        let (left, right) = (*b"L0L1L2L3", *b"R0R1R2R3");
        let mut stream = [0u8; 16];
        weave(&left, &mut stream, 0, 2, 4);
        weave(&right, &mut stream, 1, 2, 4);
        assert_eq!(&stream, b"L0R0L1R1L2R2L3R3");

        let (mut back_left, mut back_right) = ([0u8; 8], [0u8; 8]);
        unweave(&stream, &mut back_left, 0, 2, 4);
        unweave(&stream, &mut back_right, 1, 2, 4);
        assert_eq!(&back_left, &left);
        assert_eq!(&back_right, &right);
    }

    /// A stream shorter than the buffer it is dealt into stops rather than
    /// reading past it: what SDL had is what the caller is told it produced.
    #[test]
    fn weaving_stops_at_the_end_of_what_there_is() {
        let mut half = [b'.'; 8];
        unweave(b"L0R0L1R1", &mut half, 0, 2, 4);
        assert_eq!(&half, b"L0L1....");
    }

    /// The decompression magic cookie round-trips through Set/Get once an
    /// AAC converter exists, is reported as settable (unlike everything
    /// else here) and readable-but-empty before one is given, and is
    /// refused outright on a PCM-to-PCM converter, which never has one.
    #[test]
    fn the_decompression_magic_cookie_round_trips_on_an_aac_converter() {
        let mut handle = std::ptr::null_mut();
        let aac = Format {
            format_id: fourcc(b"aac "),
            ..format(44100.0, 2)
        };
        let pcm = format(44100.0, 2);
        // SAFETY: two live descriptions and a writable slot.
        let status = unsafe { AudioConverterNew(&aac, &pcm, &mut handle) };
        if status != SUCCESS {
            return; // No FFmpeg on this host; covered by the creation test.
        }
        let mut writable = 0u8;
        let mut size = 0u32;
        // SAFETY: a converter this call just made, and writable out-params.
        unsafe {
            assert_eq!(
                AudioConverterGetPropertyInfo(
                    handle,
                    property::DECOMPRESSION_MAGIC_COOKIE,
                    &mut size,
                    &mut writable
                ),
                SUCCESS
            );
            assert_eq!(size, 0, "nothing set yet");
            assert_eq!(writable, 1);
        }
        let cookie = [0x12u8, 0x10]; // a plain AAC LC AudioSpecificConfig.
        // SAFETY: as above, and a live cookie buffer.
        unsafe {
            assert_eq!(
                AudioConverterSetProperty(
                    handle,
                    property::DECOMPRESSION_MAGIC_COOKIE,
                    cookie.len() as u32,
                    cookie.as_ptr().cast(),
                ),
                SUCCESS
            );
        }
        let mut back = [0u8; 2];
        let mut back_size = back.len() as u32;
        // SAFETY: as above.
        unsafe {
            assert_eq!(
                AudioConverterGetProperty(
                    handle,
                    property::DECOMPRESSION_MAGIC_COOKIE,
                    &mut back_size,
                    back.as_mut_ptr().cast(),
                ),
                SUCCESS
            );
        }
        assert_eq!(back, cookie);
        // SAFETY: the same converter.
        unsafe { assert_eq!(AudioConverterDispose(handle), SUCCESS) };

        // A PCM-to-PCM converter never has one.
        let mut pcm_handle = std::ptr::null_mut();
        let left = format(44100.0, 2);
        let right = format(48000.0, 2);
        // SAFETY: two live descriptions and a writable slot.
        unsafe { AudioConverterNew(&left, &right, &mut pcm_handle) };
        assert_eq!(
            unsafe {
                AudioConverterSetProperty(
                    pcm_handle,
                    property::DECOMPRESSION_MAGIC_COOKIE,
                    cookie.len() as u32,
                    cookie.as_ptr().cast(),
                )
            },
            PROPERTY_NOT_SUPPORTED
        );
        // SAFETY: the converter just made.
        unsafe { AudioConverterDispose(pcm_handle) };
    }

    /// A handle this crate did not make is a parameter error rather than a
    /// dereference of whatever it is.
    #[test]
    fn a_foreign_handle_is_refused() {
        let mut bogus = 0u8;
        let handle = (&mut bogus as *mut u8).cast::<c_void>();
        // SAFETY: a pointer that is not a converter, which is the case being
        // tested.
        unsafe {
            assert_eq!(AudioConverterDispose(handle), PARAM_ERROR);
            assert_eq!(AudioConverterReset(handle), PARAM_ERROR);
        }
    }
}
