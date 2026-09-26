use std::ffi::c_void;

use static_init::dynamic;

use crate::status::SUCCESS;
use crate::stream::Format;

/// `AURenderCallback`.
pub type Proc = unsafe extern "C" fn(
    *mut c_void,
    *mut u32,
    *const TimeStamp,
    u32,
    u32,
    *mut AudioBufferList,
) -> i32;

/// What `kAudioUnitProperty_SetRenderCallback` installs: the function and the
/// cookie the guest wants handed back to it.
#[derive(Clone, Copy)]
pub struct Callback {
    pub proc: Proc,
    pub ref_con: *mut c_void,
}

/// `AudioTimeStamp`. Sixty-four bytes, and a guest reads `mSampleTime` out of
/// it to know how far into the stream it is.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct TimeStamp {
    pub sample_time: f64,
    pub host_time: u64,
    pub rate_scalar: f64,
    pub word_clock_time: u64,
    /// `SMPTETime`, which nothing here fills in.
    pub smpte: [u8; 24],
    pub flags: u32,
    pub reserved: u32,
}

const _: () = {
    assert!(size_of::<TimeStamp>() == 64);
    assert!(std::mem::offset_of!(TimeStamp, flags) == 56);
};

/// `kAudioTimeStampSampleTimeValid | kAudioTimeStampHostTimeValid`.
const TIME_VALID: u32 = 1 | 2;

/// `AudioBuffer`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioBuffer {
    pub channels: u32,
    pub byte_size: u32,
    pub data: *mut c_void,
}

/// `AudioBufferList`, whose `mBuffers` is a trailing array the header declares
/// as one element.
#[repr(C)]
pub struct AudioBufferList {
    pub count: u32,
    pub buffers: [AudioBuffer; MAX_BUFFERS],
}

const _: () = assert!(std::mem::offset_of!(AudioBufferList, buffers) == 8);

/// How many channels a non-interleaved format may have before this refuses to
/// build a list for it.
///
/// Eight, because that is a 7.1 stream and a game is one or two. A format
/// asking for more is silence rather than a heap allocation on the audio
/// thread.
pub const MAX_BUFFERS: usize = 8;

/// Frames pulled per call, which is also the size of the scratch below.
///
/// SDL asks in bytes and an `AURenderCallback` answers in frames, so one of the
/// two has to be chopped into fixed pieces. 512 is a hundredth of a second at
/// 48 kHz: small enough that a guest mixing per-buffer stays responsive, large
/// enough that the per-call cost of re-entering guest code is not the work.
const FRAMES_PER_PULL: usize = 512;

/// [`FRAMES_PER_PULL`] as the number `kAudioUnitProperty_MaximumFramesPerSlice`
/// reports, which a guest allocates its own mixing buffer against.
pub const FRAMES_PER_PULL_MAX: u32 = FRAMES_PER_PULL as u32;

/// Silence for a unit that cannot render, in the same units as the scratch.
///
/// A stream fed nothing underruns, and SDL reports an underrun as a gap the
/// device fills with whatever was there. Zeroes are the honest answer for "the
/// guest has no callback installed yet".
fn feed_silence(stream: sdl3::AudioStream, bytes: usize) {
    tally(0, bytes);
    let zeros = vec![0u8; bytes.min(FRAMES_PER_PULL * MAX_BUFFERS * 4)];
    // SAFETY: a stream this crate opened, and a live slice.
    unsafe { sdl3::audio::put(stream, &zeros) };
}

/// SDL wants `additional` more bytes: pull them from the guest.
///
/// # Safety
///
/// `userdata` is the unit handle `stream::open` passed, and SDL calls this on
/// its own audio thread with a stream this crate opened.
pub unsafe extern "C" fn feed(
    userdata: *mut c_void,
    stream: sdl3::AudioStream,
    additional: i32,
    _total: i32,
) {
    if additional <= 0 {
        return;
    }
    let Some(unit) = crate::unit_for(userdata) else {
        return;
    };
    // The lock is held across the guest's callback deliberately: `AudioUnitStop`
    // and `AudioComponentInstanceDispose` take it too, so a guest cannot free
    // what it is being asked to fill.
    let mut unit = unit.lock().unwrap_or_else(|e| e.into_inner());
    if !unit.running {
        return;
    }
    let (format, callback, source) = (unit.format, unit.callback, unit.source);
    if callback.is_none() && source == 0 {
        feed_silence(stream, additional as usize);
        return;
    }

    let frame_bytes = match format.non_interleaved() {
        true => format.bytes_per_frame() * format.channels_per_frame as usize,
        false => format.bytes_per_frame(),
    };
    if frame_bytes == 0 {
        return;
    }

    let mut wanted = additional as usize;
    let mut scratch = vec![0u8; FRAMES_PER_PULL * frame_bytes];
    while wanted > 0 {
        let frames = (wanted / frame_bytes).clamp(1, FRAMES_PER_PULL);
        let filled = &mut scratch[..frames * frame_bytes];
        filled.fill(0);
        // A graph connects a unit to this one's input; a program without one
        // installs a callback instead. The connection wins, as CoreAudio's does.
        let rendered = match source {
            0 => match callback {
                Some(callback) => pull(&mut unit.rendered, callback, format, frames, filled),
                None => false,
            },
            source => render_from_unit(source, format, frames, filled, 0),
        };
        if !rendered {
            // Silence for this slice, and carry on: returning here would leave
            // the rest of what the device asked for unwritten, which is an
            // underrun - a gap far longer than the slice that failed.
            filled.fill(0);
            tally(0, filled.len());
        }
        dump(filled);
        tally(filled.len(), 0);
        // SAFETY: a stream this crate opened, and a live slice.
        unsafe { sdl3::audio::put(stream, filled) };
        wanted = wanted.saturating_sub(filled.len());
    }
}

/// One call into the guest, filling `into`.
///
/// `false` when the guest's callback reports an error, which is the one thing
/// this cannot paper over: an `AURenderCallback` that fails has left the buffer
/// in whatever state it likes.
fn pull(
    rendered: &mut u64,
    callback: Callback,
    format: Format,
    frames: usize,
    into: &mut [u8],
) -> bool {
    let channels = format.channels_per_frame as usize;
    let mut list = AudioBufferList {
        count: 1,
        buffers: [AudioBuffer {
            channels: 0,
            byte_size: 0,
            data: std::ptr::null_mut(),
        }; MAX_BUFFERS],
    };
    match format.non_interleaved() {
        // One buffer per channel, each a contiguous run of the scratch. The
        // guest writes them separately and `interleave` puts them back together,
        // because SDL takes one interleaved stream.
        true => {
            if channels == 0 || channels > MAX_BUFFERS {
                return false;
            }
            let per = frames * format.bytes_per_frame();
            // One run per channel, side by side in `into`: a format whose frame
            // is wider than the caller sized for would have the last channel's
            // run past the end of it.
            if per.saturating_mul(channels) > into.len() {
                return false;
            }
            list.count = channels as u32;
            for (i, buffer) in list.buffers[..channels].iter_mut().enumerate() {
                buffer.channels = 1;
                buffer.byte_size = per as u32;
                buffer.data = into[i * per..].as_mut_ptr().cast();
            }
        }
        false => {
            list.buffers[0].channels = channels as u32;
            list.buffers[0].byte_size = into.len() as u32;
            list.buffers[0].data = into.as_mut_ptr().cast();
        }
    }

    let now = TimeStamp {
        sample_time: *rendered as f64,
        host_time: 0,
        rate_scalar: 1.0,
        flags: TIME_VALID,
        ..TimeStamp::default()
    };
    let mut action_flags = 0u32;
    // SAFETY: `callback.proc` is a host-callable pointer this runtime was handed
    // for the guest's function, and every structure it is given outlives the
    // call.
    let status = unsafe {
        (callback.proc)(
            callback.ref_con,
            &mut action_flags,
            &now,
            0,
            frames as u32,
            &mut list,
        )
    };
    if status != SUCCESS {
        note_pull_failure(status, frames);
        return false;
    }
    *rendered += frames as u64;
    dump_source(into);
    if format.non_interleaved() {
        interleave(into, channels, frames, format.bytes_per_frame());
    }
    true
}

/// Turn `channels` contiguous per-channel runs into one interleaved stream, in
/// place.
///
/// Through a copy of the input rather than a swap chain: the permutation is not
/// a rotation, and doing it in place without one is the kind of cleverness that
/// is wrong for three channels and right for two.
fn interleave(buffer: &mut [u8], channels: usize, frames: usize, sample: usize) {
    let planar = buffer.to_vec();
    for frame in 0..frames {
        for channel in 0..channels {
            let from = channel * frames * sample + frame * sample;
            let to = (frame * channels + channel) * sample;
            buffer[to..to + sample].copy_from_slice(&planar[from..from + sample]);
        }
    }
}

/// Say once what a guest's render callback answered when it refused.
fn note_pull_failure(status: i32, frames: usize) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    if crate::trace::on() && n < 12 {
        eprintln!("audio: a render callback refused {frames} frames with status {status}");
    }
}

/// How many elements the last mix summed, for the tally below.
static VOICES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn note_voices(count: usize) {
    if crate::trace::on() {
        VOICES.store(count, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Count what reached the device, and say so every few seconds under
/// `ROSETTA3_TRACE_AUDIO`.
///
/// The one number that matters is how much of the stream was real: a device fed
/// silence because a pull failed sounds like a stutter and looks like nothing
/// at all from the guest's side, which reports only that it rendered.
fn tally(played: usize, silenced: usize) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static PLAYED: AtomicU64 = AtomicU64::new(0);
    static SILENCED: AtomicU64 = AtomicU64::new(0);
    static SINCE: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    if !crate::trace::on() {
        return;
    }
    PLAYED.fetch_add(played as u64, Ordering::Relaxed);
    SILENCED.fetch_add(silenced as u64, Ordering::Relaxed);
    let mut since = SINCE.lock().unwrap_or_else(|e| e.into_inner());
    let start = since.get_or_insert_with(std::time::Instant::now);
    if start.elapsed().as_secs() < 2 {
        return;
    }
    *since = Some(std::time::Instant::now());
    let (played, silenced) = (
        PLAYED.swap(0, Ordering::Relaxed),
        SILENCED.swap(0, Ordering::Relaxed),
    );
    let voices = VOICES.load(Ordering::Relaxed);
    eprintln!("audio: fed {played} bytes, silenced {silenced}, {voices} voices mixing");
}

/// `ROSETTA3_DUMP_AUDIO_SOURCE=FILE`: append every buffer a guest callback
/// filled, before anything in this crate touched it.
///
/// The companion to [`dump`]: the two files together say whether what came out
/// of the device is what the program put in, which is the question every report
/// of "the audio sounds wrong" turns into.
fn dump_source(bytes: &[u8]) {
    use std::io::Write;

    #[dynamic]
    static WANTED: Option<std::path::PathBuf> =
        std::env::var_os("ROSETTA3_DUMP_AUDIO_SOURCE").map(Into::into);

    static FILE: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);

    let Some(path) = WANTED.as_ref() else {
        return;
    };

    let mut file = FILE.lock().unwrap_or_else(|e| e.into_inner());
    if file.is_none() {
        *file = std::fs::File::create(path).ok();
    }
    if let Some(file) = file.as_mut() {
        let _ = file.write_all(bytes);
    }
}

/// `ROSETTA3_DUMP_AUDIO=FILE`: append every buffer handed to the device.
///
/// Raw frames in the output unit's own format, which is what a program's own
/// silence, clipping or stutter can be measured out of - none of which is
/// visible from anywhere else once the samples are inside SDL.
fn dump(bytes: &[u8]) {
    use std::io::Write;
    static FILE: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);
    #[dynamic]
    static WANTED: Option<std::path::PathBuf> =
        std::env::var_os("ROSETTA3_DUMP_AUDIO").map(Into::into);
    let Some(path) = WANTED.as_ref() else {
        return;
    };
    let mut file = FILE.lock().unwrap_or_else(|e| e.into_inner());
    if file.is_none() {
        *file = std::fs::File::create(path).ok();
    }
    if let Some(file) = file.as_mut() {
        let _ = file.write_all(bytes);
    }
}

/// Render `frames` frames of `format` from the unit `handle` names.
///
/// `into` arrives zeroed and is what the caller plays. `depth` is the guard
/// against a graph that names itself: a connection is made by a guest, and a
/// cycle in one would otherwise be an unbounded recursion on the audio thread.
fn render_from_unit(
    handle: usize,
    format: Format,
    frames: usize,
    into: &mut [u8],
    depth: u32,
) -> bool {
    if depth > MAX_GRAPH_DEPTH {
        return false;
    }
    let Some(unit) = crate::unit_for(handle as *mut c_void) else {
        return false;
    };
    let mut unit = unit.lock().unwrap_or_else(|e| e.into_inner());
    match unit.kind {
        // An output unit plays into a device. One connected to another unit's
        // input is a graph this runtime cannot make sense of.
        crate::Kind::Output => false,
        crate::Kind::Mixer => mix(&mut unit, format, frames, into, depth),
        crate::Kind::Converter => convert(&mut unit, format, frames, into, depth),
    }
}

/// Pull a converter's input and hand it back in `format`.
///
/// Two changes at once, which is what a converter is for: the sample type and
/// channel count of the input become the output's, and the rates are bridged by
/// linear interpolation between neighbouring input frames.
///
/// The leftover input frames and the fractional position between them are kept
/// on the unit, so a rate that is not a whole ratio carries across calls rather
/// than restarting at every buffer - which is a click per buffer, forty times a
/// second.
fn convert(
    unit: &mut crate::Unit,
    format: Format,
    frames: usize,
    into: &mut [u8],
    depth: u32,
) -> bool {
    let source_format = unit.input_format;
    let (in_channels, out_channels) = (
        source_format.channels_per_frame as usize,
        format.channels_per_frame as usize,
    );
    // The layout of `format` is not read here: `into` is this runtime's own
    // buffer, always interleaved, and `pull` has already put a guest's planar
    // fill back together by the time its frames reach `pending`. What the
    // format still has to say is the sample type and the channel count.
    if !format.is_float32() || !(1..=2).contains(&out_channels) {
        report_unmixable(format);
        return false;
    }
    if !(1..=2).contains(&in_channels) || source_format.sample_rate <= 0.0 {
        report_unmixable(source_format);
        return false;
    }

    // How many input frames this many output frames spans.
    //
    // The last output frame reads the input frame under it and, *only* when it
    // falls between two, the one after. Asking for that second frame when the
    // rates are equal - when it is always weighted zero - would add a one-frame
    // render call to every buffer, and a guest whose callback decodes a block
    // per call loses the rest of that block to it.
    // A varispeed consumes its input faster or slower by its rate.
    let ratio = source_format.sample_rate * unit.varispeed.unwrap_or(1.0) / format.sample_rate;
    let last = unit.phase + ratio * frames.saturating_sub(1) as f64;
    let between = last.fract() > 0.0;
    let needed = last.floor() as usize + 1 + between as usize;

    let have = unit.pending.len() / in_channels;
    if have < needed
        && !fill_pending(unit, source_format, in_channels, needed - have, depth)
        && unit.pending.is_empty()
    {
        return false;
    }

    let available = unit.pending.len() / in_channels;
    let mut phase = unit.phase;
    for frame in 0..frames {
        let at = phase.floor() as usize;
        let fraction = (phase - phase.floor()) as f32;
        for channel in 0..out_channels {
            // Mono into stereo repeats the one channel; stereo into mono takes
            // the left, as [`add`] does and for the same reason.
            let source = match in_channels {
                1 => 0,
                _ => channel.min(in_channels - 1),
            };
            let first = sample_at(&unit.pending, at, in_channels, source, available);
            let next = sample_at(&unit.pending, at + 1, in_channels, source, available);
            let value = first + (next - first) * fraction;
            let to = (frame * out_channels + channel) * size_of::<f32>();
            if to + 4 > into.len() {
                break;
            }
            into[to..to + 4].copy_from_slice(&value.to_ne_bytes());
        }
        phase += ratio;
    }

    // Drop the whole input frames that are now behind the read position.
    let consumed = phase.floor() as usize;
    let drop = consumed.min(available);
    unit.pending.drain(..drop * in_channels);
    unit.phase = phase - drop as f64;
    unit.rendered += frames as u64;
    true
}

/// One interleaved sample, or silence past the end of what was pulled.
fn sample_at(pending: &[f32], frame: usize, channels: usize, channel: usize, have: usize) -> f32 {
    if frame >= have {
        return 0.0;
    }
    pending
        .get(frame * channels + channel)
        .copied()
        .unwrap_or(0.0)
}

/// Pull `wanted` more input frames into the converter's pending buffer.
fn fill_pending(
    unit: &mut crate::Unit,
    source_format: Format,
    channels: usize,
    wanted: usize,
    depth: u32,
) -> bool {
    let frame_bytes = match source_format.non_interleaved() {
        true => source_format.bytes_per_frame() * channels,
        false => source_format.bytes_per_frame(),
    };
    if frame_bytes == 0 {
        return false;
    }
    let mut got_any = false;
    let mut left = wanted;
    while left > 0 {
        let frames = left.min(FRAMES_PER_PULL);
        let mut scratch = vec![0u8; frames * frame_bytes];
        let filled = match unit.source {
            0 => match unit.callback {
                Some(callback) => pull(
                    &mut unit.rendered,
                    callback,
                    source_format,
                    frames,
                    &mut scratch,
                ),
                None => false,
            },
            source => render_from_unit(source, source_format, frames, &mut scratch, depth + 1),
        };
        if !filled {
            break;
        }
        got_any = true;
        let Some(samples) = to_floats(&scratch, source_format, frames, channels) else {
            report_unmixable(source_format);
            return false;
        };
        unit.pending.extend_from_slice(&samples);
        left -= frames;
    }
    got_any
}

/// Interleaved floats from whatever sample type the format names.
///
/// The bytes are **already interleaved**, whatever the format's layout says: a
/// planar fill is woven back together by [`pull`] as it comes out of the guest,
/// and a connected unit renders interleaved. Only the sample type is read from
/// the format here; reading the layout too would take the stream apart a second
/// time and play each channel's samples as the other's.
///
/// `None` for a format this does not decode, which the caller reports rather
/// than playing as if the bytes meant something else.
fn to_floats(bytes: &[u8], format: Format, frames: usize, channels: usize) -> Option<Vec<f32>> {
    const IS_FLOAT: u32 = 1 << 0;
    const IS_BIG_ENDIAN: u32 = 1 << 1;
    const IS_SIGNED_INTEGER: u32 = 1 << 2;
    if format.format_flags & IS_BIG_ENDIAN != 0 {
        return None;
    }
    let float = format.format_flags & IS_FLOAT != 0;
    let signed = format.format_flags & IS_SIGNED_INTEGER != 0;
    let width = (format.bits_per_channel / 8) as usize;
    let total = frames * channels;
    let mut out = Vec::with_capacity(total);
    for i in 0..total {
        let at = i * width;
        if at + width > bytes.len() {
            return None;
        }
        let value = match (float, width, signed) {
            (true, 4, _) => f32::from_ne_bytes(bytes[at..at + 4].try_into().ok()?),
            (false, 2, _) => {
                i16::from_ne_bytes(bytes[at..at + 2].try_into().ok()?) as f32 / 32768.0
            }
            (false, 4, true) => {
                i32::from_ne_bytes(bytes[at..at + 4].try_into().ok()?) as f32 / 2147483648.0
            }
            (false, 1, false) => (bytes[at] as f32 - 128.0) / 128.0,
            _ => return None,
        };
        out.push(value);
    }
    Some(out)
}

/// How far a chain of connected units may run before this gives up.
const MAX_GRAPH_DEPTH: u32 = 8;

/// Sum a mixer's input elements into `into`, placed and attenuated.
///
/// Mixing happens in the output's own format, and only where that format is
/// interleaved 32-bit float mono or stereo - which is the canonical format
/// every unit here starts at. Anything else is refused rather than summed as if
/// the bytes meant what this assumed, because a mixer that guesses wrong at the
/// sample type plays full-scale noise.
fn mix(unit: &mut crate::Unit, format: Format, frames: usize, into: &mut [u8], depth: u32) -> bool {
    let out_channels = format.channels_per_frame as usize;
    // As in [`convert`]: `into` is interleaved whatever the format says, since
    // it is this runtime's buffer and not one a guest laid out.
    if !format.is_float32() || !(1..=2).contains(&out_channels) {
        report_unmixable(format);
        return false;
    }

    // `Element` is `Copy`, so the elements are taken by value and the mixer's
    // own state is not borrowed across the call into the guest - which may set
    // a parameter on this very unit from its callback.
    let mut elements = unit.inputs.clone();
    let mut scratch = vec![0u8; frames * size_of::<f32>() * MAX_BUFFERS];
    let mut voices = 0usize;

    for element in &mut elements {
        let amplitude = element.amplitude();
        if amplitude == 0.0 || (element.callback.is_none() && element.source == 0) {
            continue;
        }
        let source_format = element.format;
        let channels = source_format.channels_per_frame as usize;
        if !source_format.is_float32() || !(1..=2).contains(&channels) {
            report_unmixable(source_format);
            continue;
        }
        let bytes = frames * channels * size_of::<f32>();
        let piece = &mut scratch[..bytes];
        piece.fill(0);
        let filled = match element.source {
            0 => match element.callback {
                Some(callback) => pull(
                    &mut element.rendered,
                    callback,
                    source_format,
                    frames,
                    piece,
                ),
                None => false,
            },
            source => render_from_unit(source, source_format, frames, piece, depth + 1),
        };
        if !filled {
            continue;
        }
        let (left, right) = element.pan();
        add(
            into,
            piece,
            out_channels,
            channels,
            frames,
            amplitude,
            left,
            right,
        );
        voices += 1;
    }
    note_voices(voices);
    // The counters the pulls advanced are on the copies, so they go back.
    for (element, updated) in unit.inputs.iter_mut().zip(&elements) {
        element.rendered = updated.rendered;
    }
    unit.rendered += frames as u64;
    true
}

/// Add one element's frames into the mix, panned and attenuated.
///
/// A mono source is placed by the pan; a stereo one keeps its own image and is
/// only attenuated, because panning a stereo source would collapse it.
#[allow(clippy::too_many_arguments)]
fn add(
    into: &mut [u8],
    from: &[u8],
    out_channels: usize,
    in_channels: usize,
    frames: usize,
    amplitude: f32,
    left: f32,
    right: f32,
) {
    for frame in 0..frames {
        for channel in 0..out_channels {
            let source = match (in_channels, out_channels) {
                // Mono into stereo: one sample, weighted by the pan.
                (1, _) => 0,
                // Stereo into mono: the left channel rather than a sum, which
                // would be twice as loud wherever the two agree.
                (_, 1) => 0,
                _ => channel.min(in_channels - 1),
            };
            let gain = match (in_channels, out_channels) {
                (1, 2) => amplitude * if channel == 0 { left } else { right },
                _ => amplitude,
            };
            let at = (frame * in_channels + source) * size_of::<f32>();
            let to = (frame * out_channels + channel) * size_of::<f32>();
            if at + 4 > from.len() || to + 4 > into.len() {
                return;
            }
            let sample = f32::from_ne_bytes(from[at..at + 4].try_into().expect("four bytes"));
            let existing = f32::from_ne_bytes(into[to..to + 4].try_into().expect("four bytes"));
            let summed = existing + sample * gain;
            into[to..to + 4].copy_from_slice(&summed.to_ne_bytes());
        }
    }
}

/// Say once which format a mixer was asked to handle and could not.
fn report_unmixable(format: Format) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        eprintln!(
            "rosetta3: audio: this program's mixer was given a stream this runtime cannot mix \
             ({} Hz, {} channels, {} bits, flags {:#x}) - those voices are silent",
            format.sample_rate,
            format.channels_per_frame,
            format.bits_per_channel,
            format.format_flags,
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mono source panned centre lands in both output channels, quieter in
    /// each than it was, which is what constant-power panning means.
    #[test]
    fn a_mono_source_pans_into_both_channels() {
        let from = 1.0f32.to_ne_bytes().to_vec();
        let mut into = vec![0u8; 8];
        let centre = std::f32::consts::FRAC_1_SQRT_2;
        add(&mut into, &from, 2, 1, 1, 1.0, centre, centre);
        let left = f32::from_ne_bytes(into[0..4].try_into().unwrap());
        let right = f32::from_ne_bytes(into[4..8].try_into().unwrap());
        assert!((left - centre).abs() < 1e-6);
        assert!((right - centre).abs() < 1e-6);
    }

    /// And it sums with whatever is already there rather than replacing it.
    #[test]
    fn adding_sums_with_the_mix_so_far() {
        let from = 0.25f32.to_ne_bytes().to_vec();
        let mut into = 0.5f32.to_ne_bytes().to_vec();
        add(&mut into, &from, 1, 1, 1, 1.0, 1.0, 1.0);
        let got = f32::from_ne_bytes(into[0..4].try_into().unwrap());
        assert!((got - 0.75).abs() < 1e-6, "{got}");
    }

    /// A stereo source keeps its own image: the pan is not applied to it.
    #[test]
    fn a_stereo_source_keeps_its_image() {
        let mut from = Vec::new();
        from.extend_from_slice(&1.0f32.to_ne_bytes());
        from.extend_from_slice(&0.0f32.to_ne_bytes());
        let mut into = vec![0u8; 8];
        add(&mut into, &from, 2, 2, 1, 0.5, 0.0, 1.0);
        let left = f32::from_ne_bytes(into[0..4].try_into().unwrap());
        let right = f32::from_ne_bytes(into[4..8].try_into().unwrap());
        assert!((left - 0.5).abs() < 1e-6, "{left}");
        assert!(right.abs() < 1e-6, "{right}");
    }

    /// Signed 16-bit samples become floats at full scale.
    #[test]
    fn sixteen_bit_samples_become_floats() {
        let format = Format {
            format_flags: 1 << 2,
            bits_per_channel: 16,
            channels_per_frame: 1,
            ..Format::default()
        };
        let bytes = [i16::MIN.to_ne_bytes(), 0i16.to_ne_bytes()].concat();
        let got = to_floats(&bytes, format, 2, 1).expect("decodable");
        assert_eq!(got, vec![-1.0, 0.0]);
    }

    /// A planar format's bytes have already been woven together by the time
    /// they reach here, so they are read straight through rather than taken
    /// apart again - which is what the canonical format made the common case.
    #[test]
    fn a_planar_format_does_not_deinterleave_twice() {
        let format = Format::default();
        assert!(format.non_interleaved());
        let mut bytes = Vec::new();
        for value in [1.0f32, 2.0, 3.0, 4.0] {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        let got = to_floats(&bytes, format, 2, 2).expect("decodable");
        assert_eq!(got, vec![1.0, 2.0, 3.0, 4.0]);
    }

    /// A format nothing here decodes is refused rather than played as noise.
    #[test]
    fn an_undecodable_sample_type_is_refused() {
        let format = Format {
            format_flags: 1 << 2,
            bits_per_channel: 24,
            channels_per_frame: 1,
            ..Format::default()
        };
        assert!(to_floats(&[0; 6], format, 2, 1).is_none());
    }

    /// Two planar channels become one interleaved stream, sample by sample.
    #[test]
    fn planar_channels_interleave() {
        // Four frames of two channels, one byte each: `LLLLRRRR`.
        let mut buffer = *b"1234abcd";
        interleave(&mut buffer, 2, 4, 1);
        assert_eq!(&buffer, b"1a2b3c4d");
    }

    /// And with a wider sample, because the stride is the thing that is easy to
    /// get wrong.
    #[test]
    fn planar_channels_interleave_at_two_bytes_a_sample() {
        let mut buffer = *b"L1L2R1R2";
        interleave(&mut buffer, 2, 2, 2);
        assert_eq!(&buffer, b"L1R1L2R2");
    }
}
