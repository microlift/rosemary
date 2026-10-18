//! Recording through an output unit's element 1.
//!
//! The unit tells the guest's `kAudioOutputUnitProperty_SetInputCallback` that a
//! slice was recorded, without buffers, and the guest pulls it with
//! `AudioUnitRender` on element 1. SDL's recording stream is the device.

use std::ffi::c_void;

use crate::Unit;
use crate::render::{AudioBuffer, AudioBufferList, MAX_BUFFERS, TimeStamp};
use crate::status::*;
use crate::stream::{Format, sdl_spec};

/// The element an output unit records on.
pub(crate) const ELEMENT: u32 = 1;

/// The format set on element 1's output scope, or the device's.
pub(crate) fn format(unit: &Unit) -> Format {
    unit.capture_format
        .or_else(crate::hal::input_format)
        .unwrap_or_default()
}

fn slice_frames() -> u32 {
    crate::hal::input_buffer_frames().clamp(1, crate::render::FRAMES_PER_PULL_MAX)
}

/// Open the recording device for `unit`, paused.
pub(crate) fn open(unit: &mut Unit, handle: *mut c_void) -> i32 {
    let Some(spec) = sdl_spec(&format(unit)) else {
        return FORMAT_NOT_SUPPORTED;
    };
    // Half a slice per device period, so a slice is passed on as soon as it is
    // whole. At SDL's default the PulseAudio backend delivers several slices in
    // one burst, which a guest expecting steady callbacks hears as dropouts.
    let device_frames = (slice_frames() / 2).max(64);
    // SAFETY: `record` has SDL's callback prototype, and `handle` is a key into
    // a table that outlives the process.
    let stream =
        unsafe { sdl3::audio::open_recording_sized(spec, device_frames, Some(record), handle) };
    let status = match stream {
        Some(stream) => {
            unit.capture = stream;
            unit.recorded = 0;
            SUCCESS
        }
        None => CANNOT_DO_IN_CURRENT_CONTEXT,
    };
    crate::trace::say(
        format_args!(
            "open recording {} Hz, {} channels",
            spec.freq, spec.channels
        ),
        status,
    );
    status
}

/// # Safety
///
/// The unit's lock must not be held: SDL waits for a callback in flight, and
/// that callback's guest calls `AudioUnitRender`, which takes it.
pub(crate) unsafe fn destroy(stream: sdl3::AudioStream) {
    if !stream.is_null() {
        // SAFETY: the caller's obligation.
        unsafe { sdl3::audio::destroy(stream) };
    }
}

/// SDL has recorded more: hand the guest every whole slice.
///
/// # Safety
///
/// `userdata` is the unit handle [`open`] passed.
unsafe extern "C" fn record(
    userdata: *mut c_void,
    stream: sdl3::AudioStream,
    _additional: i32,
    _total: i32,
) {
    let Some(unit) = crate::unit_for(userdata) else {
        return;
    };
    let _darwin = darwin::tls::DarwinThread::enter();
    loop {
        let (callback, frames, now) = {
            // `try_lock`, because stopping or disposing the unit holds the lock
            // while SDL waits for this callback. Whatever is left stays in the
            // stream until the next call.
            let mut unit = match unit.try_lock() {
                Ok(unit) => unit,
                Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return,
            };
            if !unit.running || unit.capture != stream {
                return;
            }
            let format = format(&unit);
            let frame_bytes =
                format.channels_per_frame as usize * format.bits_per_channel as usize / 8;
            let frames = slice_frames() as usize;
            let bytes = frames * frame_bytes;
            // SAFETY: a stream this crate opened.
            if bytes == 0 || unsafe { sdl3::audio::available(stream) } < bytes {
                return;
            }
            let mut captured = std::mem::take(&mut unit.captured);
            captured.resize(bytes, 0);
            // SAFETY: as above, and a live buffer.
            let got = unsafe { sdl3::audio::get(stream, &mut captured) };
            unit.captured = captured;
            if got < bytes {
                return;
            }
            let Some(callback) = unit.input_callback else {
                continue;
            };
            let now = TimeStamp {
                sample_time: unit.recorded as f64,
                host_time: crate::host_time(),
                rate_scalar: 1.0,
                flags: 1 | 2,
                ..TimeStamp::default()
            };
            unit.recorded += frames as u64;
            (callback, frames, now)
        };
        let mut action = 0u32;
        // SAFETY: a host-callable pointer this runtime was handed, called
        // without the unit's lock so that it can render this unit.
        unsafe {
            (callback.proc)(
                callback.ref_con,
                &mut action,
                &now,
                ELEMENT,
                frames as u32,
                std::ptr::null_mut(),
            )
        };
    }
}

/// `AudioUnitRender` on element 1: copy out the slice last recorded.
///
/// A buffer with a null `mData` is pointed at one of the unit's own, as Apple's
/// unit does.
///
/// # Safety
///
/// `list` must be a writable `AudioBufferList` with `mNumberBuffers` buffers,
/// each writable for its `mDataByteSize` unless its `mData` is null.
pub(crate) unsafe fn render(unit: &mut Unit, frames: u32, list: *mut AudioBufferList) -> i32 {
    let format = format(unit);
    let channels = format.channels_per_frame as usize;
    let sample = format.bits_per_channel as usize / 8;
    let frame_bytes = channels * sample;
    if frame_bytes == 0 {
        return FORMAT_NOT_SUPPORTED;
    }
    let frames = frames as usize;
    let have = unit.captured.len() / frame_bytes;
    let planes = match format.non_interleaved() {
        true => channels,
        false => 1,
    };
    // SAFETY: the caller's obligation. The list holds `mNumberBuffers` buffers
    // rather than `MAX_BUFFERS`, so they are reached through a pointer.
    let count = unsafe { (*list).count } as usize;
    let buffers = unsafe { (&raw mut (*list).buffers).cast::<AudioBuffer>() };
    if count < planes || planes > MAX_BUFFERS {
        return PARAM_ERROR;
    }
    let plane_bytes = frames * frame_bytes / planes;
    unit.lent.resize(plane_bytes * planes, 0);
    for plane in 0..planes {
        // SAFETY: `plane < count`.
        let buffer = unsafe { &mut *buffers.add(plane) };
        let room = match buffer.data.is_null() {
            true => {
                buffer.data = unit.lent[plane * plane_bytes..].as_mut_ptr().cast();
                plane_bytes
            }
            false => (buffer.byte_size as usize).min(plane_bytes),
        };
        // SAFETY: `room` bytes at `data` are the caller's or the unit's.
        let out = unsafe { std::slice::from_raw_parts_mut(buffer.data.cast::<u8>(), room) };
        if planes == 1 {
            let copied = room.min(have * frame_bytes);
            out[..copied].copy_from_slice(&unit.captured[..copied]);
            out[copied..].fill(0);
        } else {
            for (frame, slot) in out.chunks_exact_mut(sample).enumerate() {
                let at = frame * frame_bytes + plane * sample;
                match frame < have {
                    true => slot.copy_from_slice(&unit.captured[at..at + sample]),
                    false => slot.fill(0),
                }
            }
        }
        buffer.byte_size = room as u32;
        buffer.channels = (channels / planes) as u32;
    }
    SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    const STREAM_FORMAT: u32 = 8;
    const CURRENT_DEVICE: u32 = 2000;
    const ENABLE_IO: u32 = 2003;
    const SET_INPUT_CALLBACK: u32 = 2005;

    static RENDERED: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" fn input(
        unit: *mut c_void,
        flags: *mut u32,
        time: *const TimeStamp,
        bus: u32,
        frames: u32,
        list: *mut AudioBufferList,
    ) -> i32 {
        assert!(list.is_null());
        let mut samples = vec![0f32; frames as usize];
        let mut list = AudioBufferList {
            count: 1,
            buffers: [AudioBuffer {
                channels: 1,
                byte_size: frames * 4,
                data: samples.as_mut_ptr().cast(),
            }; MAX_BUFFERS],
        };
        // SAFETY: the unit under test and a live list.
        let status = unsafe { crate::AudioUnitRender(unit, flags, time, bus, frames, &mut list) };
        assert_eq!(status, SUCCESS);
        RENDERED.fetch_add(list.buffers[0].byte_size / 4, Ordering::Relaxed);
        SUCCESS
    }

    /// Records from the host's default recording device, if it has one.
    #[test]
    fn a_record_only_unit_calls_its_input_callback() {
        if !crate::hal::has_input() {
            return;
        }
        let (on, off, device) = (1u32, 0u32, crate::hal::INPUT_DEVICE);
        let mono = Format {
            format_flags: crate::stream::flags::IS_FLOAT | crate::stream::flags::IS_PACKED,
            channels_per_frame: 1,
            ..crate::hal::input_format().unwrap()
        };
        let mut handle = std::ptr::null_mut();
        // SAFETY: every structure handed over is live for its call.
        unsafe {
            assert_eq!(
                crate::AudioComponentInstanceNew(crate::test_component(), &mut handle),
                SUCCESS
            );
            let callback: [*mut c_void; 2] = [input as *mut c_void, handle];
            let set = |id, scope, element, data: *const c_void, size| {
                crate::property::AudioUnitSetProperty(handle, id, scope, element, data, size)
            };
            assert_eq!(set(ENABLE_IO, 1, 1, (&raw const on).cast(), 4), SUCCESS);
            assert_eq!(set(ENABLE_IO, 2, 0, (&raw const off).cast(), 4), SUCCESS);
            assert_eq!(
                set(CURRENT_DEVICE, 0, 0, (&raw const device).cast(), 4),
                SUCCESS
            );
            let size = size_of::<Format>() as u32;
            assert_eq!(
                set(STREAM_FORMAT, 2, 1, (&raw const mono).cast(), size),
                SUCCESS
            );
            assert_eq!(
                set(SET_INPUT_CALLBACK, 0, 0, callback.as_ptr().cast(), 16),
                SUCCESS
            );
            assert_eq!(crate::AudioUnitInitialize(handle), SUCCESS);
            assert_eq!(crate::AudioOutputUnitStart(handle), SUCCESS);
            std::thread::sleep(std::time::Duration::from_millis(500));
            assert_eq!(crate::AudioOutputUnitStop(handle), SUCCESS);
            assert_eq!(crate::AudioComponentInstanceDispose(handle), SUCCESS);
        }
        assert!(RENDERED.load(Ordering::Relaxed) > 0);
    }
}
