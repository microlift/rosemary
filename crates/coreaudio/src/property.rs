use std::ffi::c_void;

use crate::render::{Callback, Proc};
use crate::status::*;
use crate::stream::Format;

/// `kAudioUnitProperty_StreamFormat`.
const STREAM_FORMAT: u32 = 8;
/// `kAudioUnitProperty_SetRenderCallback`, and the older spelling that carries
/// the identical `AURenderCallbackStruct`.
const SET_RENDER_CALLBACK: u32 = 23;
const SET_INPUT_CALLBACK: u32 = 22;
/// `kAudioOutputUnitProperty_SetInputCallback`, which despite the similar name
/// is the *record* callback: the unit calls it when it has captured audio, and
/// it answers by calling `AudioUnitRender` on the input bus.
const OUTPUT_SET_INPUT_CALLBACK: u32 = 2005;
/// `kAudioOutputUnitProperty_HasIO`.
const HAS_IO: u32 = 2006;
/// `kAudioUnitProperty_ElementCount`: how many input buses a mixer has.
const ELEMENT_COUNT: u32 = 11;
/// The 3D mixer's own properties, which pick a spatialisation algorithm and
/// describe how distance attenuates. Accepted and not acted on - see the note
/// where they are matched.
const SPATIALIZATION: &[u32] = &[
    // `kAudioUnitProperty_SpatializationAlgorithm`, `DopplerRate`,
    // `3DMixerRenderingFlags`, `3DMixerDistanceAtten`, `3DMixerDistanceParams`,
    // and `ReverbPreset`.
    3000, 3002, 3003, 3004, 3010, 3012,
];
/// `kAudioUnitProperty_MatrixLevels`: the matrix mixer's gain table, a
/// `Float32` for each of `(inputs + 1) * (outputs + 1)` cells.
const MATRIX_LEVELS: u32 = 3006;
/// `kAudioUnitProperty_MatrixDimensions`: the matrix mixer's total input and
/// output channels, which size that table.
const MATRIX_DIMENSIONS: u32 = 3009;

/// A matrix mixer's `(input channels, output channels)`: every input bus's
/// channels together, and the output format's.
fn matrix_dimensions(unit: &crate::Unit) -> (u32, u32) {
    let inputs = unit
        .inputs
        .iter()
        .map(|input| input.format.channels_per_frame)
        .sum();
    (inputs, unit.format.channels_per_frame)
}

/// `kAudioUnitProperty_MaximumFramesPerSlice`, which a guest reads to size its
/// own mixing buffer.
const MAXIMUM_FRAMES_PER_SLICE: u32 = 14;
/// `kAudioUnitProperty_AudioChannelLayout`: which speaker each channel of the
/// stream format is meant for.
const AUDIO_CHANNEL_LAYOUT: u32 = 19;
/// `kAudioOutputUnitProperty_CurrentDevice`, the HAL device an `ahal` unit
/// plays through.
const CURRENT_DEVICE: u32 = 2000;
/// `kAudioOutputUnitProperty_EnableIO`, one flag per scope.
const ENABLE_IO: u32 = 2003;

/// `AudioUnitScope`.
mod scope {
    pub const INPUT: u32 = 1;
    pub const OUTPUT: u32 = 2;
}

/// The most input elements a mixer may have.
///
/// A game asks for one per simultaneous voice; this is well past what any of
/// them use, and the point of the cap is that the count arrives from the guest
/// and sizes an allocation.
const MAXIMUM_ELEMENTS: usize = 256;

/// The largest slice this crate will ever ask for. It is a constant rather than
/// a tuning knob, because a guest allocates against the number it is told.
const MAXIMUM_FRAMES: u32 = crate::render::FRAMES_PER_PULL_MAX;

/// How many channels an `AudioChannelLayout` describes, or `None` for one too
/// short to read or naming no channels.
///
/// The structure is a tag, a bitmap and a count of trailing descriptions, and
/// which of the three carries the answer is what the tag says. Getting this
/// wrong is a layout accepted against a stream format with a different number
/// of channels, which plays as the wrong speakers rather than as an error.
///
/// # Safety
///
/// `data` must be readable for `size` bytes.
unsafe fn layout_channels(data: *const c_void, size: u32) -> Option<u32> {
    /// `kAudioChannelLayoutTag_UseChannelDescriptions`.
    const USE_DESCRIPTIONS: u32 = 0;
    /// `kAudioChannelLayoutTag_UseChannelBitmap`.
    const USE_BITMAP: u32 = 1 << 16;

    if (size as usize) < 3 * size_of::<u32>() {
        return None;
    }
    let header = unsafe { std::slice::from_raw_parts(data.cast::<u32>(), 3) };
    let channels = match header[0] {
        USE_DESCRIPTIONS => header[2],
        USE_BITMAP => header[1].count_ones(),
        // Every other tag spells its channel count in its low half.
        tag => tag & 0xffff,
    };
    (channels != 0).then_some(channels)
}

fn carries_a_callback(id: u32) -> bool {
    matches!(id, SET_RENDER_CALLBACK | SET_INPUT_CALLBACK)
}

/// `OSStatus AudioUnitSetProperty(AudioUnit, AudioUnitPropertyID,
/// AudioUnitScope, AudioUnitElement, const void *inData, UInt32 inDataSize)`
///
/// # Safety
///
/// `data` must be null or `size` readable bytes, and for a callback property it
/// must hold an `AURenderCallbackStruct` whose function pointer is *host*
/// callable - the caller substitutes a guest's for a thunk before it gets here.
pub unsafe extern "C" fn AudioUnitSetProperty(
    handle: *mut c_void,
    id: u32,
    scope: u32,
    element: u32,
    data: *const c_void,
    size: u32,
) -> i32 {
    let Some(unit) = crate::unit_for(handle) else {
        return unsafe { crate::foreign::set_property(handle, id, scope, element, data, size) }
            .unwrap_or(INVALID_COMPONENT);
    };
    let status = unsafe { set(&unit, id, scope, element, data, size) };
    crate::trace::say(
        format_args!("set property {id} scope {scope} element {element}"),
        status,
    );
    status
}

/// A property on one of a mixer's input buses.
///
/// # Safety
///
/// As [`AudioUnitSetProperty`]'s.
unsafe fn set_on_element(
    unit: &mut crate::Unit,
    id: u32,
    element: u32,
    data: *const c_void,
    size: u32,
) -> i32 {
    let Some(input) = unit.inputs.get_mut(element as usize) else {
        // A bus past the count the guest asked for, rather than growing the
        // mixer to fit: the count is what it sized its own voices against.
        return INVALID_ELEMENT;
    };
    match id {
        STREAM_FORMAT => {
            if (size as usize) < size_of::<Format>() {
                return INVALID_PROPERTY_VALUE;
            }
            input.format = unsafe { *(data as *const Format) };
            SUCCESS
        }
        id if carries_a_callback(id) => {
            if (size as usize) < size_of::<[usize; 2]>() {
                return INVALID_PROPERTY_VALUE;
            }
            // SAFETY: as above; an `AURenderCallbackStruct` is a function
            // pointer and a cookie.
            let raw = unsafe { *(data as *const [*mut c_void; 2]) };
            input.callback = (!raw[0].is_null()).then(|| Callback {
                // SAFETY: this pointer is host
                // callable with the `AURenderCallback` prototype.
                proc: unsafe { std::mem::transmute::<*mut c_void, Proc>(raw[0]) },
                ref_con: raw[1],
            });
            SUCCESS
        }
        // As on the unit: this crate never asks for more than `MAXIMUM_FRAMES`.
        MAXIMUM_FRAMES_PER_SLICE => SUCCESS,
        _ => INVALID_PROPERTY,
    }
}

/// The format a scope and element of `unit` carries.
///
/// Which one a guest is asking about is the whole of the question: a mixer's
/// input scope is its elements, each with a format of its own, and a converter
/// exists to have two. Answering the output format to every ask is what has a
/// program build its own descriptions against the wrong side of a unit - and it
/// does build them that way, by reading one and editing it.
fn format_of(unit: &crate::Unit, scope: u32, element: u32) -> Format {
    match (unit.kind, scope) {
        (crate::Kind::Mixer, scope::INPUT) => unit
            .inputs
            .get(element as usize)
            .map_or_else(Format::default, |input| input.format),
        (crate::Kind::Converter, scope::INPUT) => unit.input_format,
        // Element 1 records: its input scope is the device's side.
        (crate::Kind::Output, scope::INPUT) if element == crate::capture::ELEMENT => {
            crate::hal::input_format().unwrap_or_default()
        }
        (crate::Kind::Output, scope::OUTPUT) if element == crate::capture::ELEMENT => {
            crate::capture::format(unit)
        }
        _ => unit.format,
    }
}

/// # Safety
///
/// As [`AudioUnitSetProperty`]'s.
unsafe fn set(
    unit: &std::sync::Mutex<crate::Unit>,
    id: u32,
    scope: u32,
    element: u32,
    data: *const c_void,
    size: u32,
) -> i32 {
    if data.is_null() {
        return PARAM_ERROR;
    }
    let mut unit = unit.lock().unwrap_or_else(|e| e.into_inner());
    crate::trace::say(
        format_args!("set property {id} scope {scope} element {element} size {size}"),
        SUCCESS,
    );
    // A mixer's input scope is its elements, each with a format and a callback
    // of its own - so the same two property ids mean "this bus" there and "the
    // whole unit" everywhere else.
    // A 3D mixer's placement properties, which say *how* to spatialise rather
    // than where, and are set on the input scope like everything else about an
    // element. This one pans and attenuates however it is asked to, so the
    // choice changes nothing - and refusing it fails the voice outright, which
    // is why this comes before the element dispatch rather than inside it.
    if unit.kind == crate::Kind::Mixer && SPATIALIZATION.contains(&id) {
        return SUCCESS;
    }
    if unit.matrix && id == MATRIX_LEVELS {
        // SAFETY: the caller's `size` bytes of floats.
        let levels = unsafe { std::slice::from_raw_parts(data as *const f32, size as usize / 4) };
        unit.levels = levels.to_vec();
        // FIXME (ACCURACY): the levels are kept and read back but not applied;
        // this mixer sums every input into the output as the other mixers do.
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| {
            eprintln!(
                "rosemary: coreaudio: a matrix mixer's levels are kept but not applied; every \
                 input is mixed to the output at unity"
            )
        });
        return SUCCESS;
    }
    if unit.kind == crate::Kind::Mixer && scope == scope::INPUT && id != ELEMENT_COUNT {
        return unsafe { set_on_element(&mut unit, id, element, data, size) };
    }
    // A converter's two scopes are two different formats, and it is the only
    // unit here for which the distinction matters.
    if unit.kind == crate::Kind::Converter && id == STREAM_FORMAT {
        if (size as usize) < size_of::<Format>() {
            return INVALID_PROPERTY_VALUE;
        }
        let format = unsafe { *(data as *const Format) };
        match scope {
            scope::INPUT => unit.input_format = format,
            scope::OUTPUT => unit.format = format,
            _ => return INVALID_SCOPE,
        }
        crate::trace::say(
            format_args!(
                "converter {} format: {} Hz, {} channels, {} bits, flags {:#x}, {} bytes/frame",
                match scope {
                    scope::INPUT => "input",
                    _ => "output",
                },
                format.sample_rate,
                format.channels_per_frame,
                format.bits_per_channel,
                format.format_flags,
                format.bytes_per_frame,
            ),
            SUCCESS,
        );
        return SUCCESS;
    }
    if unit.kind == crate::Kind::Output && element == crate::capture::ELEMENT {
        match id {
            STREAM_FORMAT => {
                if (size as usize) < size_of::<Format>() {
                    return INVALID_PROPERTY_VALUE;
                }
                let format = unsafe { *(data as *const Format) };
                return match scope {
                    scope::OUTPUT => {
                        if crate::stream::sdl_spec(&format).is_none() {
                            return FORMAT_NOT_SUPPORTED;
                        }
                        if !unit.capture.is_null() {
                            return CANNOT_DO_IN_CURRENT_CONTEXT;
                        }
                        unit.capture_format = Some(format);
                        SUCCESS
                    }
                    // The device's own format can be set back, not changed.
                    scope::INPUT => match crate::hal::input_format() == Some(format) {
                        true => SUCCESS,
                        false => FORMAT_NOT_SUPPORTED,
                    },
                    _ => INVALID_SCOPE,
                };
            }
            MAXIMUM_FRAMES_PER_SLICE => return SUCCESS,
            _ => {}
        }
    }
    match id {
        ELEMENT_COUNT if unit.kind == crate::Kind::Mixer => {
            if size < 4 {
                return INVALID_PROPERTY_VALUE;
            }
            let count = unsafe { *(data as *const u32) } as usize;
            if count > MAXIMUM_ELEMENTS {
                return INVALID_PROPERTY_VALUE;
            }
            match scope {
                scope::INPUT => {
                    unit.inputs.resize(count, crate::mixer::Element::default());
                    SUCCESS
                }
                // The output is one bus. A guest asking for another is asking
                // for a mixer that splits rather than mixes.
                scope::OUTPUT => match count {
                    1 => SUCCESS,
                    _ => INVALID_PROPERTY_VALUE,
                },
                _ => INVALID_SCOPE,
            }
        }
        STREAM_FORMAT => {
            if (size as usize) < size_of::<Format>() {
                return INVALID_PROPERTY_VALUE;
            }
            let format = unsafe { *(data as *const Format) };
            if crate::stream::sdl_spec(&format).is_none() {
                return FORMAT_NOT_SUPPORTED;
            }
            // A format set after the device is open would have the guest's
            // callback filling one format while SDL converted another.
            if !unit.stream.is_null() {
                return CANNOT_DO_IN_CURRENT_CONTEXT;
            }
            unit.format = format;
            SUCCESS
        }
        id if carries_a_callback(id) => {
            if (size as usize) < size_of::<[usize; 2]>() {
                return INVALID_PROPERTY_VALUE;
            }
            // SAFETY: as above; an `AURenderCallbackStruct` is a function
            // pointer and a cookie.
            let raw = unsafe { *(data as *const [*mut c_void; 2]) };
            unit.callback = (!raw[0].is_null()).then(|| Callback {
                // SAFETY: this pointer is host
                // callable with the `AURenderCallback` prototype.
                proc: unsafe { std::mem::transmute::<*mut c_void, Proc>(raw[0]) },
                ref_con: raw[1],
            });
            SUCCESS
        }
        MAXIMUM_FRAMES_PER_SLICE => {
            // Accepted and ignored: this crate never asks for more than
            // `MAXIMUM_FRAMES`, so a guest lowering the ceiling changes nothing
            // and a guest raising it gets what it asked for anyway.
            match size >= 4 {
                true => SUCCESS,
                false => INVALID_PROPERTY_VALUE,
            }
        }
        AUDIO_CHANNEL_LAYOUT => {
            // SAFETY: the caller's obligation, and the size is checked against
            // the header before any of it is read.
            let Some(channels) = (unsafe { layout_channels(data, size) }) else {
                return INVALID_PROPERTY_VALUE;
            };
            // Only the count is honoured, in the standard order SDL plays.
            // A layout naming the same channels in a different order would be
            // played in this one, which is the pair of speakers swapped and
            // nothing to say so.
            match channels == unit.format.channels_per_frame {
                true => SUCCESS,
                false => FORMAT_NOT_SUPPORTED,
            }
        }
        ENABLE_IO => {
            if size < 4 {
                return INVALID_PROPERTY_VALUE;
            }
            let enabled = unsafe { *(data as *const u32) } != 0;
            if !unit.stream.is_null() || !unit.capture.is_null() {
                return CANNOT_DO_IN_CURRENT_CONTEXT;
            }
            match scope {
                scope::OUTPUT => {
                    unit.playback = enabled;
                    SUCCESS
                }
                // Refused here when there is nothing to record from, where the
                // program still checks.
                scope::INPUT => match enabled && !crate::hal::has_input() {
                    true => INVALID_PROPERTY_VALUE,
                    false => {
                        unit.recording = enabled;
                        SUCCESS
                    }
                },
                _ => INVALID_SCOPE,
            }
        }
        OUTPUT_SET_INPUT_CALLBACK => {
            if (size as usize) < size_of::<[usize; 2]>() {
                return INVALID_PROPERTY_VALUE;
            }
            // SAFETY: as for the render callback.
            let raw = unsafe { *(data as *const [*mut c_void; 2]) };
            unit.input_callback = (!raw[0].is_null()).then(|| Callback {
                // SAFETY: as for the render callback.
                proc: unsafe { std::mem::transmute::<*mut c_void, Proc>(raw[0]) },
                ref_con: raw[1],
            });
            SUCCESS
        }
        CURRENT_DEVICE => {
            if size < 4 {
                return INVALID_PROPERTY_VALUE;
            }
            let device = unsafe { *(data as *const u32) };
            let known = device == crate::hal::OUTPUT_DEVICE
                || (device == crate::hal::INPUT_DEVICE && crate::hal::has_input());
            match known {
                true => {
                    unit.device = device;
                    SUCCESS
                }
                false => INVALID_PROPERTY_VALUE,
            }
        }
        _ => INVALID_PROPERTY,
    }
}

/// `OSStatus AudioUnitGetProperty(AudioUnit, AudioUnitPropertyID,
/// AudioUnitScope, AudioUnitElement, void *outData, UInt32 *ioDataSize)`
///
/// # Safety
///
/// `out` must be null or writable for `*size` bytes, and `size` must be null or
/// a writable `UInt32`.
pub unsafe extern "C" fn AudioUnitGetProperty(
    handle: *mut c_void,
    id: u32,
    scope: u32,
    element: u32,
    out: *mut c_void,
    size: *mut u32,
) -> i32 {
    let Some(unit) = crate::unit_for(handle) else {
        return unsafe { crate::foreign::get_property(handle, id, scope, element, out, size) }
            .unwrap_or(INVALID_COMPONENT);
    };
    if size.is_null() {
        return PARAM_ERROR;
    }
    let unit = unit.lock().unwrap_or_else(|e| e.into_inner());
    let capacity = unsafe { *size };
    crate::trace::say(
        format_args!("get property {id} scope {scope} element {element} capacity {capacity}"),
        SUCCESS,
    );
    if unit.matrix && (id == MATRIX_DIMENSIONS || id == MATRIX_LEVELS) {
        let (inputs, outputs) = matrix_dimensions(&unit);
        let words: Vec<u32> = match id {
            MATRIX_DIMENSIONS => vec![inputs, outputs],
            _ => {
                let cells = ((inputs + 1) * (outputs + 1)) as usize;
                let mut levels = unit.levels.clone();
                levels.resize(cells, 0.0);
                levels.into_iter().map(f32::to_bits).collect()
            }
        };
        let wanted = (words.len() * 4) as u32;
        // SAFETY: the caller's out-parameters, with the capacity checked
        // before the table is written.
        unsafe {
            *size = wanted;
            if out.is_null() {
                return SUCCESS;
            }
            if capacity < wanted {
                return INVALID_PROPERTY_VALUE;
            }
            std::ptr::copy_nonoverlapping(words.as_ptr(), out as *mut u32, words.len());
        }
        return SUCCESS;
    }
    match id {
        STREAM_FORMAT => {
            let wanted = size_of::<Format>() as u32;
            unsafe { *size = wanted };
            if out.is_null() {
                // A null `outData` is how a guest asks only for the size.
                return SUCCESS;
            }
            if capacity < wanted {
                return INVALID_PROPERTY_VALUE;
            }
            // SAFETY: the caller's obligation, and the capacity was checked.
            unsafe { *(out as *mut Format) = format_of(&unit, scope, element) };
            SUCCESS
        }
        MAXIMUM_FRAMES_PER_SLICE => {
            unsafe { *size = 4 };
            if out.is_null() {
                return SUCCESS;
            }
            if capacity < 4 {
                return INVALID_PROPERTY_VALUE;
            }
            unsafe { *(out as *mut u32) = MAXIMUM_FRAMES };
            SUCCESS
        }
        // Read back so that a program can confirm what it set.
        ENABLE_IO | CURRENT_DEVICE | HAS_IO => {
            let value = match (id, scope) {
                (CURRENT_DEVICE, _) => unit.device,
                (ENABLE_IO, scope::INPUT) => u32::from(unit.recording),
                (ENABLE_IO, _) => u32::from(unit.playback),
                (_, scope::INPUT) => u32::from(crate::hal::has_input()),
                _ => 1,
            };
            unsafe { *size = 4 };
            if out.is_null() {
                return SUCCESS;
            }
            if capacity < 4 {
                return INVALID_PROPERTY_VALUE;
            }
            // SAFETY: as above, and the capacity was checked.
            unsafe { *(out as *mut u32) = value };
            SUCCESS
        }
        _ => INVALID_PROPERTY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit() -> *mut c_void {
        let mut handle = std::ptr::null_mut();
        // SAFETY: a writable slot and this crate's own component.
        unsafe {
            crate::AudioComponentInstanceNew(crate::test_component(), &mut handle);
        }
        handle
    }

    #[test]
    fn the_stream_format_round_trips() {
        let handle = unit();
        let wanted = Format {
            sample_rate: 48000.0,
            ..Format::default()
        };
        // SAFETY: live structures of the sizes declared.
        unsafe {
            assert_eq!(
                AudioUnitSetProperty(
                    handle,
                    STREAM_FORMAT,
                    1,
                    0,
                    (&wanted as *const Format).cast(),
                    size_of::<Format>() as u32,
                ),
                SUCCESS
            );
            let mut read = Format::default();
            let mut size = size_of::<Format>() as u32;
            assert_eq!(
                AudioUnitGetProperty(
                    handle,
                    STREAM_FORMAT,
                    1,
                    0,
                    (&mut read as *mut Format).cast(),
                    &mut size,
                ),
                SUCCESS
            );
            assert_eq!(read, wanted);
            crate::AudioComponentInstanceDispose(handle);
        }
    }

    /// A format the device cannot be opened for is refused *here*, where the
    /// guest still checks, rather than at start where it no longer does.
    #[test]
    fn an_unsupported_format_is_refused_when_it_is_set() {
        let handle = unit();
        let bad = Format {
            bits_per_channel: 24,
            format_flags: 4,
            ..Format::default()
        };
        // SAFETY: a live structure of the size declared.
        unsafe {
            assert_eq!(
                AudioUnitSetProperty(
                    handle,
                    STREAM_FORMAT,
                    1,
                    0,
                    (&bad as *const Format).cast(),
                    size_of::<Format>() as u32,
                ),
                FORMAT_NOT_SUPPORTED
            );
            crate::AudioComponentInstanceDispose(handle);
        }
    }

    /// A property nothing here has is named rather than silently accepted: a
    /// guest that sets one and hears nothing would otherwise have no way to tell
    /// which of the two it was.
    #[test]
    fn an_unknown_property_is_an_invalid_property() {
        let handle = unit();
        let value = 0u32;
        // SAFETY: a live word.
        unsafe {
            assert_eq!(
                AudioUnitSetProperty(handle, 9999, 0, 0, (&value as *const u32).cast(), 4),
                INVALID_PROPERTY
            );
            crate::AudioComponentInstanceDispose(handle);
        }
    }

    /// The three properties a HAL output unit is configured through, which is
    /// what a program asking for the `ahal` subtype does before it initializes.
    /// Each of them has exactly one answer here, and the other answer has to be
    /// an error rather than a silent agreement: a program told it may record,
    /// or may play through some other device, has configured itself for
    /// something that will never happen.
    #[test]
    fn the_output_unit_is_configured_and_disagreement_is_refused() {
        let handle = unit();
        let word = |value: u32, id: u32, scope: u32| {
            // SAFETY: a live word of the size declared.
            unsafe { AudioUnitSetProperty(handle, id, scope, 0, (&value as *const u32).cast(), 4) }
        };
        assert_eq!(word(1, ENABLE_IO, scope::OUTPUT), SUCCESS);
        assert_eq!(word(0, ENABLE_IO, scope::INPUT), SUCCESS);
        let input = match crate::hal::has_input() {
            true => SUCCESS,
            false => INVALID_PROPERTY_VALUE,
        };
        assert_eq!(word(1, ENABLE_IO, scope::INPUT), input);
        assert_eq!(word(0, ENABLE_IO, scope::INPUT), SUCCESS);
        assert_eq!(word(0, ENABLE_IO, scope::OUTPUT), SUCCESS);
        assert_eq!(word(1, ENABLE_IO, scope::OUTPUT), SUCCESS);
        assert_eq!(word(0, ENABLE_IO, 0), INVALID_SCOPE);

        assert_eq!(word(crate::hal::OUTPUT_DEVICE, CURRENT_DEVICE, 0), SUCCESS);
        assert_eq!(word(crate::hal::INPUT_DEVICE, CURRENT_DEVICE, 0), input);
        assert_eq!(word(99, CURRENT_DEVICE, 0), INVALID_PROPERTY_VALUE);

        // `kAudioChannelLayoutTag_Stereo`, against the default format's two
        // channels, and a five-channel tag against the same.
        let layout = |tag: u32| {
            let value: [u32; 8] = [tag, 0, 0, 0, 0, 0, 0, 0];
            // SAFETY: a live structure of the size declared.
            unsafe {
                AudioUnitSetProperty(
                    handle,
                    AUDIO_CHANNEL_LAYOUT,
                    scope::INPUT,
                    0,
                    value.as_ptr().cast(),
                    size_of_val(&value) as u32,
                )
            }
        };
        assert_eq!(layout((101 << 16) | 2), SUCCESS);
        assert_eq!(layout((120 << 16) | 5), FORMAT_NOT_SUPPORTED);

        // SAFETY: a unit this test made.
        unsafe { crate::AudioComponentInstanceDispose(handle) };
    }

    #[test]
    fn the_render_callback_and_its_cookie_are_both_kept() {
        unsafe extern "C" fn never(
            _: *mut c_void,
            _: *mut u32,
            _: *const crate::render::TimeStamp,
            _: u32,
            _: u32,
            _: *mut crate::render::AudioBufferList,
        ) -> i32 {
            SUCCESS
        }
        let handle = unit();
        let installed: [*mut c_void; 2] = [never as *mut c_void, 0x1234 as *mut c_void];
        // SAFETY: a live pair of the size declared.
        unsafe {
            assert_eq!(
                AudioUnitSetProperty(
                    handle,
                    SET_RENDER_CALLBACK,
                    1,
                    0,
                    installed.as_ptr().cast(),
                    16,
                ),
                SUCCESS
            );
        }
        let unit = crate::unit_for(handle).expect("a unit");
        let callback = unit.lock().unwrap().callback.expect("installed");
        assert_eq!(callback.ref_con, 0x1234 as *mut c_void);
        // SAFETY: a unit this test made.
        unsafe { crate::AudioComponentInstanceDispose(handle) };
    }
}
