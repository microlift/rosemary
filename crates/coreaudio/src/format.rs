use std::ffi::c_void;

use crate::aac;
use crate::status::{PARAM_ERROR, SUCCESS};
use crate::stream::Format;

/// `kAudioFormatUnknownFormatError`, `'!fmt'`: the format is not one this
/// understands at all.
const UNKNOWN_FORMAT: i32 = i32::from_be_bytes(*b"!fmt");
/// `kAudioFormatUnsupportedDataFormatError`, `'fmt?'`: it is recognized as
/// AAC, but the magic cookie needed to complete it is missing or does not
/// parse.
const UNSUPPORTED_FORMAT: i32 = i32::from_be_bytes(*b"fmt?");

const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

/// `kAudioFormatProperty_FormatInfo`: complete a partly filled description.
const FORMAT_INFO: u32 = fourcc(b"fmti");

/// `kAudioFormatLinearPCM`.
const LINEAR_PCM: u32 = fourcc(b"lpcm");

/// `kAudioFormatFlagIsNonInterleaved`.
const NON_INTERLEAVED: u32 = 1 << 5;

/// Fill in what a caller left at zero.
///
/// The rules are the header's: one frame per packet for PCM, and a frame is
/// every channel's sample side by side - except for a non-interleaved format,
/// where `mBytesPerFrame` counts one channel only.
fn completed(mut format: Format) -> Option<Format> {
    if format.format_id != LINEAR_PCM || format.bits_per_channel == 0 {
        return None;
    }
    if format.channels_per_frame == 0 {
        return None;
    }
    if format.frames_per_packet == 0 {
        format.frames_per_packet = 1;
    }
    let per_channel = format.bits_per_channel.div_ceil(8);
    if format.bytes_per_frame == 0 {
        format.bytes_per_frame = match format.format_flags & NON_INTERLEAVED != 0 {
            true => per_channel,
            false => per_channel * format.channels_per_frame,
        };
    }
    if format.bytes_per_packet == 0 {
        format.bytes_per_packet = format.bytes_per_frame * format.frames_per_packet;
    }
    Some(format)
}

/// Completes an MPEG-4 AAC description from its magic cookie: the
/// `AudioSpecificConfig`'s sample rate (with the explicit-frequency escape)
/// and channel count, and `mFramesPerPacket` (1024, or 960 when the config's
/// `frameLengthFlag` says so; doubled for `'aach'`/`'aacp'`, whose sample
/// rate is also the SBR-doubled one). Every byte field is left at zero, as
/// Apple's own answer does for a variable-bitrate compressed format - there
/// is no one `mBytesPerPacket` to report.
fn completed_aac(mut format: Format, cookie: Option<&[u8]>) -> Option<Format> {
    let cookie = cookie.filter(|c| !c.is_empty())?;
    let asc = aac::parse_cookie(cookie)?;
    let doubled = aac::is_sbr(format.format_id);
    format.sample_rate = f64::from(asc.sample_rate) * if doubled { 2.0 } else { 1.0 };
    format.channels_per_frame = asc.channels;
    let frames = if asc.frame_length_1024 { 1024 } else { 960 };
    format.frames_per_packet = if doubled { frames * 2 } else { frames };
    format.bytes_per_packet = 0;
    format.bytes_per_frame = 0;
    Some(format)
}

/// `OSStatus AudioFormatGetProperty(AudioFormatPropertyID, UInt32, const void *,
/// UInt32 *, void *)`
///
/// # Safety
///
/// `specifier` must be readable for `specifier_size` bytes, `size` a writable
/// `UInt32`, and `data` null or writable for `*size` bytes.
pub unsafe extern "C" fn AudioFormatGetProperty(
    property: u32,
    specifier_size: u32,
    specifier: *const c_void,
    size: *mut u32,
    data: *mut c_void,
) -> i32 {
    if size.is_null() {
        return PARAM_ERROR;
    }
    if property != FORMAT_INFO {
        unsafe { *size = 0 };
        return UNKNOWN_FORMAT;
    }
    // The description to complete is always `data`: in by contract (it must
    // have at least `mFormatID` filled in) and out once this fills in what
    // it can. `specifier`/`specifierSize` is separate, optional, per-format
    // auxiliary data - for a compressed format, its magic cookie. The one
    // exception is a caller asking only for the size, with `data` null:
    // then `specifier` must hold the description, since there is nothing
    // else to read one from, and so there is no separate cookie either.
    let data_holds_format = !data.is_null() && unsafe { *size } as usize >= size_of::<Format>();
    let (source, cookie): (*const Format, Option<&[u8]>) = if data_holds_format {
        let cookie = (!specifier.is_null() && specifier_size > 0).then(|| {
            // SAFETY: the caller says `specifier` is readable for
            // `specifier_size` bytes.
            unsafe { std::slice::from_raw_parts(specifier.cast::<u8>(), specifier_size as usize) }
        });
        (data as *const Format, cookie)
    } else if !specifier.is_null() && specifier_size as usize >= size_of::<Format>() {
        (specifier as *const Format, None)
    } else {
        unsafe { *size = 0 };
        return PARAM_ERROR;
    };
    // SAFETY: `source` was just checked, on every path, to be readable for a
    // whole description.
    let format = unsafe { *source };
    let result = if format.format_id == LINEAR_PCM {
        completed(format).ok_or(UNKNOWN_FORMAT)
    } else if aac::is_aac(format.format_id) {
        completed_aac(format, cookie).ok_or(UNSUPPORTED_FORMAT)
    } else {
        Err(UNKNOWN_FORMAT)
    };
    let format = match result {
        Ok(format) => format,
        Err(status) => {
            unsafe { *size = 0 };
            return status;
        }
    };
    if data.is_null() {
        unsafe { *size = size_of::<Format>() as u32 };
        return SUCCESS;
    }
    // SAFETY: the caller says `data` is writable for `*size` bytes, which was
    // checked to be at least a whole description.
    unsafe {
        if (*size as usize) < size_of::<Format>() {
            *size = 0;
            return PARAM_ERROR;
        }
        *(data as *mut Format) = format;
        *size = size_of::<Format>() as u32;
    }
    SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case every caller uses: a rate, a channel count and a sample width,
    /// with the three derived fields left at zero.
    #[test]
    fn the_derived_fields_are_filled_in() {
        let sparse = Format {
            sample_rate: 44100.0,
            format_id: LINEAR_PCM,
            format_flags: 1,
            bits_per_channel: 32,
            channels_per_frame: 2,
            bytes_per_frame: 0,
            bytes_per_packet: 0,
            frames_per_packet: 0,
            reserved: 0,
        };
        let full = completed(sparse).expect("linear PCM completes");
        assert_eq!(full.frames_per_packet, 1);
        assert_eq!(full.bytes_per_frame, 8, "two channels of four bytes");
        assert_eq!(full.bytes_per_packet, 8);
    }

    /// A non-interleaved frame is *one* channel wide, which is the rule in the
    /// header that is easiest to get backwards.
    #[test]
    fn a_non_interleaved_frame_is_one_channel() {
        let planar = Format {
            format_flags: 1 | NON_INTERLEAVED,
            bytes_per_frame: 0,
            bytes_per_packet: 0,
            ..Format::default()
        };
        assert_eq!(completed(planar).unwrap().bytes_per_frame, 4);
    }

    /// A format with no codec here is refused rather than completed with
    /// arithmetic that only holds for PCM.
    #[test]
    fn a_compressed_format_has_no_derived_fields() {
        let aac = Format {
            format_id: fourcc(b"aac "),
            ..Format::default()
        };
        assert!(completed(aac).is_none());
    }

    /// Object type 2 (AAC LC), 44.1kHz, stereo, 1024-sample frames - the
    /// textbook two-byte `AudioSpecificConfig`; see `aac::tests` for the
    /// bit-by-bit derivation.
    const STEREO_44K_COOKIE: [u8; 2] = [0x12, 0x10];

    fn aac_asbd(format_id: u32) -> Format {
        Format {
            format_id,
            sample_rate: 0.0,
            channels_per_frame: 0,
            frames_per_packet: 0,
            bytes_per_frame: 0,
            bytes_per_packet: 0,
            bits_per_channel: 0,
            format_flags: 0,
            reserved: 0,
        }
    }

    #[test]
    fn completed_aac_fills_rate_channels_and_frame_length() {
        let full = completed_aac(aac_asbd(fourcc(b"aac ")), Some(&STEREO_44K_COOKIE))
            .expect("a plain AAC LC cookie completes");
        assert_eq!(full.sample_rate, 44100.0);
        assert_eq!(full.channels_per_frame, 2);
        assert_eq!(full.frames_per_packet, 1024);
        assert_eq!(
            full.bytes_per_frame, 0,
            "a VBR format has no fixed frame size"
        );
        assert_eq!(full.bytes_per_packet, 0);
    }

    /// HE-AAC's own sample rate and frame length are the core stream's
    /// doubled - the extension band SBR adds, not a second cookie.
    #[test]
    fn completed_aac_doubles_for_he_aac() {
        let full = completed_aac(aac_asbd(fourcc(b"aach")), Some(&STEREO_44K_COOKIE))
            .expect("an HE-AAC cookie completes");
        assert_eq!(full.sample_rate, 88200.0);
        assert_eq!(full.frames_per_packet, 2048);
    }

    #[test]
    fn completed_aac_without_a_cookie_is_refused() {
        assert!(completed_aac(aac_asbd(fourcc(b"aac ")), None).is_none());
        assert!(completed_aac(aac_asbd(fourcc(b"aac ")), Some(&[])).is_none());
        assert!(completed_aac(aac_asbd(fourcc(b"aac ")), Some(&[0xFF])).is_none());
    }

    /// The entry point as Roblox's `MacCoreAudioCodecs` calls it: the magic
    /// cookie as `specifier`, a partly filled description as `data` - two
    /// different buffers, not the same pointer read twice.
    #[test]
    fn audio_format_get_property_completes_aac_from_a_separate_cookie() {
        let mut format = aac_asbd(fourcc(b"aac "));
        let mut size = size_of::<Format>() as u32;
        // SAFETY: live, correctly sized buffers.
        let status = unsafe {
            AudioFormatGetProperty(
                FORMAT_INFO,
                STEREO_44K_COOKIE.len() as u32,
                STEREO_44K_COOKIE.as_ptr().cast(),
                &mut size,
                &mut format as *mut Format as *mut c_void,
            )
        };
        assert_eq!(status, SUCCESS);
        assert_eq!(format.sample_rate, 44100.0);
        assert_eq!(format.channels_per_frame, 2);
    }

    /// A caller that never supplies a cookie gets Apple's "unsupported", not
    /// a silent guess.
    #[test]
    fn audio_format_get_property_without_a_cookie_is_unsupported() {
        let mut format = aac_asbd(fourcc(b"aac "));
        let mut size = size_of::<Format>() as u32;
        // SAFETY: a live, correctly sized buffer, and a null specifier.
        let status = unsafe {
            AudioFormatGetProperty(
                FORMAT_INFO,
                0,
                std::ptr::null(),
                &mut size,
                &mut format as *mut Format as *mut c_void,
            )
        };
        assert_eq!(status, UNSUPPORTED_FORMAT);
        assert_eq!(size, 0);
    }

    /// A format this crate has never heard of is Apple's "unknown", the
    /// same answer a non-PCM format always got before AAC existed here.
    #[test]
    fn audio_format_get_property_rejects_an_unrelated_format() {
        let mut format = aac_asbd(fourcc(b"alac"));
        let mut size = size_of::<Format>() as u32;
        // SAFETY: a live, correctly sized buffer.
        let status = unsafe {
            AudioFormatGetProperty(
                FORMAT_INFO,
                0,
                std::ptr::null(),
                &mut size,
                &mut format as *mut Format as *mut c_void,
            )
        };
        assert_eq!(status, UNKNOWN_FORMAT);
    }
}
