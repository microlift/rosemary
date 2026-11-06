//! `ExtAudioFile`: linear PCM read and write on top of WAV and AIFF
//! containers.
//!
//! FIXME (ACCURACY): compressed formats (AAC, ALAC, ...) are not decoded or
//! encoded - only `kAudioFormatLinearPCM` file and client formats are
//! accepted. FIXME (ACCURACY): the client format must share the file's
//! sample rate and channel count; there is no resampling or channel
//! remapping here, only a bit-depth/float/endianness/interleaving
//! conversion through a scalar codec.

use std::ffi::c_void;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Mutex;

use corefoundation::base::CFTypeRef;

const SUCCESS: i32 = 0;
/// `paramErr`.
const PARAM_ERROR: i32 = -50;
/// `fnfErr`: the file `ExtAudioFileOpenURL` was asked to open is not there.
const FILE_NOT_FOUND: i32 = -43;

/// `kExtAudioFileError_InvalidProperty`.
const INVALID_PROPERTY: i32 = -66561;
/// `kExtAudioFileError_InvalidPropertySize`.
const INVALID_PROPERTY_SIZE: i32 = -66562;
/// `kExtAudioFileError_NonPCMClientFormat`.
const NON_PCM_CLIENT_FORMAT: i32 = -66563;
/// `kExtAudioFileError_InvalidChannelMap`: here, a client channel count that
/// does not match the file's, since nothing remaps channels.
const INVALID_CHANNEL_MAP: i32 = -66564;
/// `kExtAudioFileError_InvalidOperationOrder`: a read on a file opened for
/// writing, or vice versa.
const INVALID_OPERATION_ORDER: i32 = -66565;
/// `kExtAudioFileError_InvalidDataFormat`.
const INVALID_DATA_FORMAT: i32 = -66566;
/// `kExtAudioFileError_InvalidSeek`.
const INVALID_SEEK: i32 = -66568;

const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

/// `kAudioFormatLinearPCM`.
const FORMAT_LINEAR_PCM: u32 = fourcc(b"lpcm");
const IS_FLOAT: u32 = 1 << 0;
const IS_BIG_ENDIAN: u32 = 1 << 1;
const IS_SIGNED_INTEGER: u32 = 1 << 2;
const IS_NON_INTERLEAVED: u32 = 1 << 5;

/// `kAudioFileWAVEType`, `kAudioFileAIFFType`.
const TYPE_WAVE: u32 = fourcc(b"WAVE");
const TYPE_AIFF: u32 = fourcc(b"AIFF");

/// `ExtAudioFilePropertyID`s this answers.
mod property {
    use super::fourcc;
    pub const FILE_DATA_FORMAT: u32 = fourcc(b"ffmt");
    pub const CLIENT_DATA_FORMAT: u32 = fourcc(b"cfmt");
    pub const FILE_LENGTH_FRAMES: u32 = fourcc(b"#frm");
}

/// `AudioStreamBasicDescription`. A second copy of [`crate::queue::Format`]:
/// each module that needs the layout keeps its own, rather than sharing one
/// across a module boundary that otherwise has nothing to do with queues.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Format {
    pub sample_rate: f64,
    pub format_id: u32,
    pub format_flags: u32,
    pub bytes_per_packet: u32,
    pub frames_per_packet: u32,
    pub bytes_per_frame: u32,
    pub channels_per_frame: u32,
    pub bits_per_channel: u32,
    pub reserved: u32,
}

const _: () = assert!(size_of::<Format>() == 40);

/// `AudioBuffer`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioBuffer {
    pub channels: u32,
    pub byte_size: u32,
    pub data: *mut c_void,
}

/// `AudioBufferList`, whose `mBuffers` is a trailing array the header
/// declares as one element.
#[repr(C)]
pub struct AudioBufferList {
    pub count: u32,
    pub buffers: [AudioBuffer; MAX_BUFFERS],
}

/// As many channels as a non-interleaved list here will spread across
/// buffers. Eight is a 7.1 stream, which is already more than this format
/// family needs.
const MAX_BUFFERS: usize = 8;

/// Which container a file is: the two linear-PCM containers Core Audio
/// routes through `ExtAudioFile` without a codec in between.
#[derive(Clone, Copy)]
enum Kind {
    Wav,
    Aiff,
}

struct ExtAudioFile {
    file: File,
    writing: bool,
    kind: Kind,
    /// The format audio is actually stored in, on disk.
    file_format: Format,
    /// The format [`ExtAudioFileRead`]/[`ExtAudioFileWrite`] convert to and
    /// from. Starts equal to `file_format`.
    client_format: Format,
    /// Where the first byte of PCM data sits in the host file.
    data_offset: u64,
    /// How many bytes of PCM data the file holds, in `file_format`. Grows as
    /// a write appends; fixed once a read opens the file.
    data_bytes: u64,
    /// The next frame [`ExtAudioFileRead`]/[`ExtAudioFileSeek`] reads from,
    /// or the next frame a write lands at - in both cases, frames of
    /// `file_format`.
    position: u64,
}

/// # Safety
///
/// `handle` must be null or a pointer this module handed out and not yet
/// disposed.
unsafe fn handle_ref<'a>(handle: *mut c_void) -> Option<&'a Mutex<ExtAudioFile>> {
    (!handle.is_null()).then(|| unsafe { &*(handle as *const Mutex<ExtAudioFile>) })
}

/// Bytes one sample of `format` occupies.
fn sample_bytes(format: &Format) -> usize {
    (format.bits_per_channel as usize).div_ceil(8).max(1)
}

/// Decode interleaved PCM bytes of `format` into `[-1.0, 1.0]`-normalized
/// samples, interleaved the same way. Any trailing partial sample is
/// dropped.
///
/// A scalar codec rather than a lookup table: this format family runs from
/// 8-bit integers to 32-bit floats, in either endianness, and every one of
/// those combinations reduces to the same two cases below.
fn decode_to_f32(bytes: &[u8], format: &Format) -> Vec<f32> {
    let width = sample_bytes(format);
    let big_endian = format.format_flags & IS_BIG_ENDIAN != 0;
    let is_float = format.format_flags & IS_FLOAT != 0;
    bytes
        .chunks_exact(width)
        .map(|chunk| {
            if is_float {
                match width {
                    4 => {
                        let b: [u8; 4] = chunk.try_into().unwrap();
                        if big_endian {
                            f32::from_be_bytes(b)
                        } else {
                            f32::from_le_bytes(b)
                        }
                    }
                    8 => {
                        let b: [u8; 8] = chunk.try_into().unwrap();
                        (if big_endian {
                            f64::from_be_bytes(b)
                        } else {
                            f64::from_le_bytes(b)
                        }) as f32
                    }
                    // FIXME (ACCURACY): no other float width is defined for
                    // linear PCM; treated as silence rather than guessed at.
                    _ => 0.0,
                }
            } else {
                let mut raw: i64 = 0;
                for (i, &byte) in chunk.iter().enumerate() {
                    let shift = if big_endian {
                        (width - 1 - i) * 8
                    } else {
                        i * 8
                    };
                    raw |= (byte as i64) << shift;
                }
                let bits = (width * 8) as u32;
                if bits < 64 && raw & (1 << (bits - 1)) != 0 {
                    raw -= 1i64 << bits;
                }
                raw as f32 / (1i64 << (bits - 1)) as f32
            }
        })
        .collect()
}

/// The inverse of [`decode_to_f32`]: normalized samples back into `format`'s
/// interleaved PCM bytes, clamped to its range.
fn encode_from_f32(samples: &[f32], format: &Format) -> Vec<u8> {
    let width = sample_bytes(format);
    let big_endian = format.format_flags & IS_BIG_ENDIAN != 0;
    let is_float = format.format_flags & IS_FLOAT != 0;
    let mut out = Vec::with_capacity(samples.len() * width);
    for &sample in samples {
        if is_float {
            match width {
                4 => out.extend_from_slice(&if big_endian {
                    sample.to_be_bytes()
                } else {
                    sample.to_le_bytes()
                }),
                8 => {
                    let value = sample as f64;
                    out.extend_from_slice(&if big_endian {
                        value.to_be_bytes()
                    } else {
                        value.to_le_bytes()
                    });
                }
                _ => out.extend(std::iter::repeat_n(0u8, width)),
            }
            continue;
        }
        let bits = (width * 8) as u32;
        let max = ((1i64 << (bits - 1)) - 1) as f32;
        let raw = (sample.clamp(-1.0, 1.0) * max).round() as i64;
        for i in 0..width {
            let shift = if big_endian {
                (width - 1 - i) * 8
            } else {
                i * 8
            };
            out.push(((raw >> shift) & 0xff) as u8);
        }
    }
    out
}

/// The guest path a `file:` `CFURLRef` names, or `None` for anything else.
///
/// # Safety
///
/// `url` must be null or a `CFURLRef`.
unsafe fn url_guest_path(url: CFTypeRef) -> Option<String> {
    unsafe { corefoundation::url::as_url(url) }.and_then(|u| u.file_path())
}

// --- WAV ---

/// The `WAVEFORMAT` tag for linear PCM and for IEEE float - the only two this
/// reads. `WAVE_FORMAT_EXTENSIBLE` (0xfffe) carries its real tag inside a
/// SubFormat GUID this does not decode.
///
/// FIXME (ACCURACY): extensible `fmt ` chunks are refused rather than
/// partially understood.
const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

fn decode_wav_fmt(body: &[u8]) -> Option<Format> {
    if body.len() < 16 {
        return None;
    }
    let tag = u16::from_le_bytes(body[0..2].try_into().unwrap());
    let channels = u16::from_le_bytes(body[2..4].try_into().unwrap()) as u32;
    let sample_rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
    let bits = u16::from_le_bytes(body[14..16].try_into().unwrap()) as u32;
    let is_float = match tag {
        WAVE_FORMAT_PCM => false,
        WAVE_FORMAT_IEEE_FLOAT => true,
        _ => return None,
    };
    if channels == 0 || bits == 0 {
        return None;
    }
    let bytes_per_frame = sample_bytes_for(bits) * channels;
    Some(Format {
        sample_rate: sample_rate as f64,
        format_id: FORMAT_LINEAR_PCM,
        format_flags: if is_float {
            IS_FLOAT
        } else {
            IS_SIGNED_INTEGER
        },
        bytes_per_packet: bytes_per_frame,
        frames_per_packet: 1,
        bytes_per_frame,
        channels_per_frame: channels,
        bits_per_channel: bits,
        reserved: 0,
    })
}

fn sample_bytes_for(bits: u32) -> u32 {
    bits.div_ceil(8).max(1)
}

/// Scan a WAV file's chunks for `fmt ` and `data`, and answer the format and
/// where the audio bytes are. Chunks are padded to an even length; the pad
/// byte is not part of a chunk's declared size.
fn parse_wav(file: &mut File) -> Option<(Format, u64, u64)> {
    let mut riff = [0u8; 12];
    file.read_exact(&mut riff).ok()?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return None;
    }
    let mut format = None;
    loop {
        let mut head = [0u8; 8];
        file.read_exact(&mut head).ok()?;
        let id = &head[0..4];
        let size = u32::from_le_bytes(head[4..8].try_into().unwrap()) as u64;
        if id == b"fmt " {
            let mut body = vec![0u8; size as usize];
            file.read_exact(&mut body).ok()?;
            format = decode_wav_fmt(&body);
            if size % 2 == 1 {
                file.seek(SeekFrom::Current(1)).ok()?;
            }
        } else if id == b"data" {
            let offset = file.stream_position().ok()?;
            return Some((format?, offset, size));
        } else {
            file.seek(SeekFrom::Current((size + (size & 1)) as i64))
                .ok()?;
        }
    }
}

/// Write a `fmt `/`data` WAV header with placeholder sizes, and answer where
/// the audio bytes start. [`finalize_wav`] patches the sizes once the real
/// length is known.
fn write_wav_header(file: &mut File, format: &Format) -> std::io::Result<u64> {
    file.write_all(b"RIFF")?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(b"WAVE")?;
    file.write_all(b"fmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    let tag: u16 = if format.format_flags & IS_FLOAT != 0 {
        WAVE_FORMAT_IEEE_FLOAT
    } else {
        WAVE_FORMAT_PCM
    };
    file.write_all(&tag.to_le_bytes())?;
    file.write_all(&(format.channels_per_frame as u16).to_le_bytes())?;
    file.write_all(&(format.sample_rate as u32).to_le_bytes())?;
    let byte_rate = (format.sample_rate as u32).saturating_mul(format.bytes_per_frame);
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&(format.bytes_per_frame as u16).to_le_bytes())?;
    file.write_all(&(format.bits_per_channel as u16).to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&0u32.to_le_bytes())?;
    file.stream_position()
}

/// Patch the `RIFF` and `data` sizes a real write fills in only once it knows
/// how much audio there is, and pad to an even file length.
fn finalize_wav(file: &mut File, data_offset: u64, data_bytes: u64) -> std::io::Result<()> {
    if data_bytes % 2 == 1 {
        file.seek(SeekFrom::Start(data_offset + data_bytes))?;
        file.write_all(&[0])?;
    }
    let riff_size = (data_offset - 8 + data_bytes) as u32;
    file.seek(SeekFrom::Start(4))?;
    file.write_all(&riff_size.to_le_bytes())?;
    file.seek(SeekFrom::Start(data_offset - 4))?;
    file.write_all(&(data_bytes as u32).to_le_bytes())?;
    Ok(())
}

// --- AIFF ---

/// Fixed offsets into the header [`write_aiff_header`] writes: there are
/// never any chunks besides `COMM` and `SSND`, so these never move.
const AIFF_FORM_SIZE_OFFSET: u64 = 4;
const AIFF_FRAME_COUNT_OFFSET: u64 = 22;
const AIFF_SSND_SIZE_OFFSET: u64 = 42;
const AIFF_DATA_OFFSET: u64 = 54;

/// Decode a SANE 80-bit extended float (big-endian): sign and 15-bit biased
/// exponent in the first two bytes, a 64-bit unnormalized mantissa in the
/// rest. AIFF's `COMM` chunk has stored a sample rate this way since the
/// original Mac - there is no shorter encoding in the format.
fn decode_extended(bytes: &[u8; 10]) -> f64 {
    let sign = if bytes[0] & 0x80 != 0 { -1.0 } else { 1.0 };
    let exponent = (((bytes[0] & 0x7f) as i32) << 8 | bytes[1] as i32) - 16383;
    let mantissa = u64::from_be_bytes(bytes[2..10].try_into().unwrap());
    if exponent == -16383 && mantissa == 0 {
        return 0.0;
    }
    sign * (mantissa as f64) * 2f64.powi(exponent - 63)
}

/// The inverse of [`decode_extended`].
fn encode_extended(value: f64) -> [u8; 10] {
    let mut out = [0u8; 10];
    if value == 0.0 {
        return out;
    }
    let sign = value.is_sign_negative();
    let magnitude = value.abs();
    let exponent = magnitude.log2().floor() as i32;
    let mantissa = (magnitude / 2f64.powi(exponent - 63)).round() as u64;
    let biased = (exponent + 16383) as u16;
    out[0] = ((biased >> 8) as u8) | if sign { 0x80 } else { 0 };
    out[1] = (biased & 0xff) as u8;
    out[2..10].copy_from_slice(&mantissa.to_be_bytes());
    out
}

fn parse_aiff(file: &mut File) -> Option<(Format, u64, u64)> {
    let mut form = [0u8; 12];
    file.read_exact(&mut form).ok()?;
    if &form[0..4] != b"FORM" || &form[8..12] != b"AIFF" {
        return None;
    }
    let mut format = None;
    loop {
        let mut head = [0u8; 8];
        file.read_exact(&mut head).ok()?;
        let id = &head[0..4];
        let size = u32::from_be_bytes(head[4..8].try_into().unwrap()) as u64;
        if id == b"COMM" {
            let mut body = vec![0u8; size as usize];
            file.read_exact(&mut body).ok()?;
            if body.len() < 18 {
                return None;
            }
            let channels = u16::from_be_bytes(body[0..2].try_into().unwrap()) as u32;
            let bits = u16::from_be_bytes(body[6..8].try_into().unwrap()) as u32;
            let rate: [u8; 10] = body[8..18].try_into().ok()?;
            if channels == 0 || bits == 0 {
                return None;
            }
            let bytes_per_frame = sample_bytes_for(bits) * channels;
            format = Some(Format {
                sample_rate: decode_extended(&rate),
                format_id: FORMAT_LINEAR_PCM,
                format_flags: IS_BIG_ENDIAN | IS_SIGNED_INTEGER,
                bytes_per_packet: bytes_per_frame,
                frames_per_packet: 1,
                bytes_per_frame,
                channels_per_frame: channels,
                bits_per_channel: bits,
                reserved: 0,
            });
            if size % 2 == 1 {
                file.seek(SeekFrom::Current(1)).ok()?;
            }
        } else if id == b"SSND" {
            let mut prefix = [0u8; 8];
            file.read_exact(&mut prefix).ok()?;
            let skip = u32::from_be_bytes(prefix[0..4].try_into().unwrap()) as u64;
            let offset = file.stream_position().ok()? + skip;
            let data_bytes = size.checked_sub(8 + skip)?;
            return Some((format?, offset, data_bytes));
        } else {
            file.seek(SeekFrom::Current((size + (size & 1)) as i64))
                .ok()?;
        }
    }
}

fn write_aiff_header(file: &mut File, format: &Format) -> std::io::Result<u64> {
    file.write_all(b"FORM")?;
    file.write_all(&0u32.to_be_bytes())?;
    file.write_all(b"AIFF")?;
    file.write_all(b"COMM")?;
    file.write_all(&18u32.to_be_bytes())?;
    file.write_all(&(format.channels_per_frame as u16).to_be_bytes())?;
    file.write_all(&0u32.to_be_bytes())?; // numSampleFrames, patched by finalize_aiff
    file.write_all(&(format.bits_per_channel as u16).to_be_bytes())?;
    file.write_all(&encode_extended(format.sample_rate))?;
    file.write_all(b"SSND")?;
    file.write_all(&0u32.to_be_bytes())?; // chunk size, patched by finalize_aiff
    file.write_all(&0u32.to_be_bytes())?; // dataOffset
    file.write_all(&0u32.to_be_bytes())?; // blockSize
    file.stream_position()
}

fn finalize_aiff(file: &mut File, format: &Format, data_bytes: u64) -> std::io::Result<()> {
    if data_bytes % 2 == 1 {
        file.seek(SeekFrom::Start(AIFF_DATA_OFFSET + data_bytes))?;
        file.write_all(&[0])?;
    }
    let form_size = (AIFF_DATA_OFFSET - 8 + data_bytes) as u32;
    file.seek(SeekFrom::Start(AIFF_FORM_SIZE_OFFSET))?;
    file.write_all(&form_size.to_be_bytes())?;
    let frames = data_bytes / format.bytes_per_frame.max(1) as u64;
    file.seek(SeekFrom::Start(AIFF_FRAME_COUNT_OFFSET))?;
    file.write_all(&(frames as u32).to_be_bytes())?;
    let ssnd_size = (8 + data_bytes) as u32;
    file.seek(SeekFrom::Start(AIFF_SSND_SIZE_OFFSET))?;
    file.write_all(&ssnd_size.to_be_bytes())?;
    Ok(())
}

// --- ExtAudioFile ---

/// `OSStatus ExtAudioFileOpenURL(CFURLRef, ExtAudioFileRef *)`: opens an
/// existing WAV or AIFF file for reading, detecting its format from its own
/// header.
///
/// # Safety
///
/// `url` must be null or a `CFURLRef`; `out` must be null or writable.
pub unsafe extern "C" fn ExtAudioFileOpenURL(url: CFTypeRef, out: *mut *mut c_void) -> i32 {
    if out.is_null() {
        return PARAM_ERROR;
    }
    // SAFETY: the caller's out-parameter.
    unsafe { out.write(std::ptr::null_mut()) };
    let Some(guest_path) = (unsafe { url_guest_path(url) }) else {
        return PARAM_ERROR;
    };
    let host = prefix::ns::existing_host_path(&guest_path);
    let Ok(mut file) = File::open(&host) else {
        return FILE_NOT_FOUND;
    };
    let parsed = parse_wav(&mut file)
        .map(|(f, o, s)| (Kind::Wav, f, o, s))
        .or_else(|| {
            file.rewind().ok()?;
            parse_aiff(&mut file).map(|(f, o, s)| (Kind::Aiff, f, o, s))
        });
    let Some((kind, format, data_offset, data_bytes)) = parsed else {
        return INVALID_DATA_FORMAT;
    };
    let ext = ExtAudioFile {
        file,
        writing: false,
        kind,
        file_format: format,
        client_format: format,
        data_offset,
        data_bytes,
        position: 0,
    };
    let handle = Box::into_raw(Box::new(Mutex::new(ext))) as *mut c_void;
    // SAFETY: checked non-null above.
    unsafe { out.write(handle) };
    SUCCESS
}

/// `OSStatus ExtAudioFileCreateWithURL(CFURLRef, AudioFileTypeID, const
/// AudioStreamBasicDescription *, const AudioChannelLayout *, UInt32,
/// ExtAudioFileRef *)`: creates a new WAV or AIFF file for writing in
/// `inStreamDesc`'s format - which `inFlags`'
/// `kAudioFileFlags_EraseFile` or not, this always truncates, as a fresh
/// recording does on a Mac too.
///
/// # Safety
///
/// `url` must be null or a `CFURLRef`; `stream_desc` must be null or
/// readable; `out` must be null or writable.
pub unsafe extern "C" fn ExtAudioFileCreateWithURL(
    url: CFTypeRef,
    file_type: u32,
    stream_desc: *const Format,
    _channel_layout: *const c_void,
    _flags: u32,
    out: *mut *mut c_void,
) -> i32 {
    if out.is_null() {
        return PARAM_ERROR;
    }
    // SAFETY: the caller's out-parameter.
    unsafe { out.write(std::ptr::null_mut()) };
    if stream_desc.is_null() {
        return PARAM_ERROR;
    }
    // SAFETY: checked non-null above; the caller's obligation that it reads.
    let format = unsafe { *stream_desc };
    if format.format_id != FORMAT_LINEAR_PCM || format.channels_per_frame == 0 {
        // FIXME (ACCURACY): no compressed-format encoder exists here.
        return INVALID_DATA_FORMAT;
    }
    let kind = match file_type {
        TYPE_WAVE => Kind::Wav,
        TYPE_AIFF => Kind::Aiff,
        _ => return INVALID_DATA_FORMAT,
    };
    let Some(guest_path) = (unsafe { url_guest_path(url) }) else {
        return PARAM_ERROR;
    };
    let host = prefix::ns::host_path(&guest_path);
    let Ok(mut file) = File::create(&host) else {
        return PARAM_ERROR;
    };
    let header = match kind {
        Kind::Wav => write_wav_header(&mut file, &format),
        Kind::Aiff => write_aiff_header(&mut file, &format),
    };
    let Ok(data_offset) = header else {
        return PARAM_ERROR;
    };
    let ext = ExtAudioFile {
        file,
        writing: true,
        kind,
        file_format: format,
        client_format: format,
        data_offset,
        data_bytes: 0,
        position: 0,
    };
    let handle = Box::into_raw(Box::new(Mutex::new(ext))) as *mut c_void;
    // SAFETY: checked non-null above.
    unsafe { out.write(handle) };
    SUCCESS
}

/// `OSStatus ExtAudioFileDispose(ExtAudioFileRef)`: for a file opened to
/// write, patches the header's sizes now that the real length is known.
///
/// # Safety
///
/// `handle` must be null or a pointer [`ExtAudioFileOpenURL`] or
/// [`ExtAudioFileCreateWithURL`] produced and not yet disposed.
pub unsafe extern "C" fn ExtAudioFileDispose(handle: *mut c_void) -> i32 {
    if handle.is_null() {
        return PARAM_ERROR;
    }
    // SAFETY: the caller's obligation; this is the one place that retires the
    // handle, so nothing else may still hold it afterward.
    let ext = unsafe { Box::from_raw(handle as *mut Mutex<ExtAudioFile>) };
    let mut ext = ext.into_inner().unwrap_or_else(|e| e.into_inner());
    if ext.writing {
        let result = match ext.kind {
            Kind::Wav => finalize_wav(&mut ext.file, ext.data_offset, ext.data_bytes),
            Kind::Aiff => finalize_aiff(&mut ext.file, &ext.file_format, ext.data_bytes),
        };
        if result.is_err() {
            return PARAM_ERROR;
        }
    }
    SUCCESS
}

/// Copy `value` out through a guest `UInt32 *ioPropertyDataSize, void
/// *outPropertyData` pair, refusing a buffer too small to hold it.
///
/// # Safety
///
/// `size` must be writable; `data` must be writable for `size_of::<T>()`
/// bytes once the size check passes.
unsafe fn write_property<T: Copy>(size: *mut u32, data: *mut c_void, value: &T) -> i32 {
    if size.is_null() || data.is_null() {
        return PARAM_ERROR;
    }
    let needed = size_of::<T>() as u32;
    // SAFETY: the caller's obligation.
    if unsafe { *size } < needed {
        return INVALID_PROPERTY_SIZE;
    }
    // SAFETY: as documented above.
    unsafe {
        std::ptr::copy_nonoverlapping(value, data.cast::<T>(), 1);
        size.write(needed);
    }
    SUCCESS
}

/// `OSStatus ExtAudioFileGetProperty(ExtAudioFileRef, ExtAudioFilePropertyID,
/// UInt32 *, void *)`.
///
/// # Safety
///
/// As [`write_property`]; `handle` as [`ExtAudioFileDispose`].
pub unsafe extern "C" fn ExtAudioFileGetProperty(
    handle: *mut c_void,
    property_id: u32,
    size: *mut u32,
    data: *mut c_void,
) -> i32 {
    let Some(ext) = (unsafe { handle_ref(handle) }) else {
        return PARAM_ERROR;
    };
    let ext = ext.lock().unwrap_or_else(|e| e.into_inner());
    match property_id {
        property::FILE_DATA_FORMAT => unsafe { write_property(size, data, &ext.file_format) },
        property::CLIENT_DATA_FORMAT => unsafe { write_property(size, data, &ext.client_format) },
        property::FILE_LENGTH_FRAMES => {
            let frames = ext.data_bytes / ext.file_format.bytes_per_frame.max(1) as u64;
            unsafe { write_property(size, data, &(frames as i64)) }
        }
        _ => INVALID_PROPERTY,
    }
}

/// `OSStatus ExtAudioFileSetProperty(ExtAudioFileRef, ExtAudioFilePropertyID,
/// UInt32, const void *)`.
///
/// Only `kExtAudioFileProperty_ClientDataFormat` can be set here, and only to
/// another linear-PCM format with the file's own channel count and sample
/// rate - see the module-level FIXME.
///
/// # Safety
///
/// `data` must be readable for `size` bytes when `property_id` is the client
/// format; `handle` as [`ExtAudioFileDispose`].
pub unsafe extern "C" fn ExtAudioFileSetProperty(
    handle: *mut c_void,
    property_id: u32,
    size: u32,
    data: *const c_void,
) -> i32 {
    let Some(ext) = (unsafe { handle_ref(handle) }) else {
        return PARAM_ERROR;
    };
    let mut ext = ext.lock().unwrap_or_else(|e| e.into_inner());
    match property_id {
        property::CLIENT_DATA_FORMAT => {
            if data.is_null() || size as usize != size_of::<Format>() {
                return INVALID_PROPERTY_SIZE;
            }
            // SAFETY: checked above.
            let format = unsafe { *data.cast::<Format>() };
            if format.format_id != FORMAT_LINEAR_PCM {
                return NON_PCM_CLIENT_FORMAT;
            }
            if format.channels_per_frame != ext.file_format.channels_per_frame {
                return INVALID_CHANNEL_MAP;
            }
            if format.sample_rate != ext.file_format.sample_rate {
                // FIXME (ACCURACY): no sample-rate converter here; refused
                // rather than played back at the wrong pitch.
                return INVALID_DATA_FORMAT;
            }
            ext.client_format = format;
            SUCCESS
        }
        property::FILE_DATA_FORMAT | property::FILE_LENGTH_FRAMES => INVALID_PROPERTY,
        _ => INVALID_PROPERTY,
    }
}

/// `OSStatus ExtAudioFileRead(ExtAudioFileRef, UInt32 *, AudioBufferList *)`.
///
/// # Safety
///
/// `io_frames` must be writable; `buffer_list` must be readable and its
/// buffers writable for their stated capacities; `handle` as
/// [`ExtAudioFileDispose`].
pub unsafe extern "C" fn ExtAudioFileRead(
    handle: *mut c_void,
    io_frames: *mut u32,
    buffer_list: *mut AudioBufferList,
) -> i32 {
    if io_frames.is_null() || buffer_list.is_null() {
        return PARAM_ERROR;
    }
    let Some(ext) = (unsafe { handle_ref(handle) }) else {
        return PARAM_ERROR;
    };
    let mut ext = ext.lock().unwrap_or_else(|e| e.into_inner());
    if ext.writing {
        // SAFETY: the caller's out-parameter.
        unsafe { io_frames.write(0) };
        return INVALID_OPERATION_ORDER;
    }
    // SAFETY: the caller's structure.
    let out = unsafe { &mut *buffer_list };
    if out.count == 0 || out.count as usize > MAX_BUFFERS {
        unsafe { io_frames.write(0) };
        return PARAM_ERROR;
    }
    let client = ext.client_format;
    let channels = client.channels_per_frame.max(1) as usize;
    let non_interleaved = client.format_flags & IS_NON_INTERLEAVED != 0;
    let planes = if non_interleaved {
        channels.min(out.count as usize)
    } else {
        1
    };
    if out.buffers[..planes].iter().any(|b| b.data.is_null()) {
        unsafe { io_frames.write(0) };
        return PARAM_ERROR;
    }
    let client_frame_bytes = client.bytes_per_frame.max(1) as u64;
    let capacity_frames = out.buffers[0].byte_size as u64 / client_frame_bytes;
    let file_frame_bytes = ext.file_format.bytes_per_frame.max(1) as u64;
    let total_frames = ext.data_bytes / file_frame_bytes;
    let remaining_frames = total_frames.saturating_sub(ext.position);
    // SAFETY: the caller's in/out parameter.
    let wanted = unsafe { *io_frames } as u64;
    let frames = wanted.min(capacity_frames).min(remaining_frames);

    if frames == 0 {
        for buffer in &mut out.buffers[..planes] {
            buffer.byte_size = 0;
        }
        unsafe { io_frames.write(0) };
        return SUCCESS;
    }

    let mut file_bytes = vec![0u8; (frames * file_frame_bytes) as usize];
    let seek_to = ext.data_offset + ext.position * file_frame_bytes;
    if ext.file.seek(SeekFrom::Start(seek_to)).is_err()
        || ext.file.read_exact(&mut file_bytes).is_err()
    {
        unsafe { io_frames.write(0) };
        return PARAM_ERROR;
    }
    ext.position += frames;

    let samples = decode_to_f32(&file_bytes, &ext.file_format);
    if non_interleaved {
        for (channel, buffer) in out.buffers[..planes].iter_mut().enumerate() {
            let channel_samples: Vec<f32> = samples
                .iter()
                .skip(channel)
                .step_by(channels)
                .copied()
                .collect();
            let encoded = encode_from_f32(&channel_samples, &client);
            // SAFETY: capacity was checked against `frames` above, and
            // `encoded.len()` is exactly `frames * sample_bytes(&client)`.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    encoded.as_ptr(),
                    buffer.data.cast::<u8>(),
                    encoded.len(),
                );
            }
            buffer.byte_size = encoded.len() as u32;
        }
    } else {
        let encoded = encode_from_f32(&samples, &client);
        let buffer = &mut out.buffers[0];
        // SAFETY: as above.
        unsafe {
            std::ptr::copy_nonoverlapping(
                encoded.as_ptr(),
                buffer.data.cast::<u8>(),
                encoded.len(),
            );
        }
        buffer.byte_size = encoded.len() as u32;
    }
    // SAFETY: the caller's out-parameter.
    unsafe { io_frames.write(frames as u32) };
    SUCCESS
}

/// `OSStatus ExtAudioFileWrite(ExtAudioFileRef, UInt32, const
/// AudioBufferList *)`.
///
/// # Safety
///
/// `buffer_list` must be readable and its buffers readable for `frames`
/// worth of the client format; `handle` as [`ExtAudioFileDispose`].
pub unsafe extern "C" fn ExtAudioFileWrite(
    handle: *mut c_void,
    frames: u32,
    buffer_list: *const AudioBufferList,
) -> i32 {
    if buffer_list.is_null() {
        return PARAM_ERROR;
    }
    let Some(ext) = (unsafe { handle_ref(handle) }) else {
        return PARAM_ERROR;
    };
    let mut ext = ext.lock().unwrap_or_else(|e| e.into_inner());
    if !ext.writing {
        return INVALID_OPERATION_ORDER;
    }
    let frames = frames as usize;
    if frames == 0 {
        return SUCCESS;
    }
    let client = ext.client_format;
    let channels = client.channels_per_frame.max(1) as usize;
    // SAFETY: the caller's structure.
    let input = unsafe { &*buffer_list };
    if input.count == 0 || input.count as usize > MAX_BUFFERS {
        return PARAM_ERROR;
    }
    let non_interleaved = client.format_flags & IS_NON_INTERLEAVED != 0;
    let planes = if non_interleaved {
        channels.min(input.count as usize)
    } else {
        1
    };
    if input.buffers[..planes].iter().any(|b| b.data.is_null()) {
        return PARAM_ERROR;
    }
    let width = sample_bytes(&client);
    let samples: Vec<f32> = if non_interleaved {
        let mut interleaved = vec![0f32; frames * channels];
        for (channel, buffer) in input.buffers[..planes].iter().enumerate() {
            // SAFETY: the caller's obligation that each plane holds `frames`
            // samples of the client format.
            let bytes =
                unsafe { std::slice::from_raw_parts(buffer.data.cast::<u8>(), frames * width) };
            for (frame, value) in decode_to_f32(bytes, &client).into_iter().enumerate() {
                interleaved[frame * channels + channel] = value;
            }
        }
        interleaved
    } else {
        // SAFETY: as above, for one interleaved buffer.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                input.buffers[0].data.cast::<u8>(),
                frames * channels * width,
            )
        };
        decode_to_f32(bytes, &client)
    };

    let encoded = encode_from_f32(&samples, &ext.file_format);
    if ext.file.write_all(&encoded).is_err() {
        return PARAM_ERROR;
    }
    ext.data_bytes += encoded.len() as u64;
    ext.position += frames as u64;
    SUCCESS
}

/// `OSStatus ExtAudioFileSeek(ExtAudioFileRef, SInt64)`: the offset is in
/// frames of the *file's* format, which is unambiguous here since the client
/// format is never at a different sample rate.
///
/// # Safety
///
/// `handle` as [`ExtAudioFileDispose`].
pub unsafe extern "C" fn ExtAudioFileSeek(handle: *mut c_void, frame_offset: i64) -> i32 {
    let Some(ext) = (unsafe { handle_ref(handle) }) else {
        return PARAM_ERROR;
    };
    let mut ext = ext.lock().unwrap_or_else(|e| e.into_inner());
    if ext.writing {
        return INVALID_OPERATION_ORDER;
    }
    if frame_offset < 0 {
        return INVALID_SEEK;
    }
    let total_frames = ext.data_bytes / ext.file_format.bytes_per_frame.max(1) as u64;
    if frame_offset as u64 > total_frames {
        return INVALID_SEEK;
    }
    ext.position = frame_offset as u64;
    SUCCESS
}

exports::exports! {
    functions {
        "ExtAudioFileOpenURL" => ExtAudioFileOpenURL,
        "ExtAudioFileCreateWithURL" => ExtAudioFileCreateWithURL,
        "ExtAudioFileDispose" => ExtAudioFileDispose,
        "ExtAudioFileGetProperty" => ExtAudioFileGetProperty,
        "ExtAudioFileSetProperty" => ExtAudioFileSetProperty,
        "ExtAudioFileRead" => ExtAudioFileRead,
        "ExtAudioFileWrite" => ExtAudioFileWrite,
        "ExtAudioFileSeek" => ExtAudioFileSeek,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm16(channels: u32, sample_rate: f64) -> Format {
        Format {
            sample_rate,
            format_id: FORMAT_LINEAR_PCM,
            format_flags: IS_SIGNED_INTEGER,
            bytes_per_packet: 2 * channels,
            frames_per_packet: 1,
            bytes_per_frame: 2 * channels,
            channels_per_frame: channels,
            bits_per_channel: 16,
            reserved: 0,
        }
    }

    fn float32(channels: u32, sample_rate: f64) -> Format {
        Format {
            sample_rate,
            format_id: FORMAT_LINEAR_PCM,
            format_flags: IS_FLOAT,
            bytes_per_packet: 4 * channels,
            frames_per_packet: 1,
            bytes_per_frame: 4 * channels,
            channels_per_frame: channels,
            bits_per_channel: 32,
            reserved: 0,
        }
    }

    #[test]
    fn int16_round_trips_through_the_scalar_codec() {
        let format = pcm16(1, 44100.0);
        for sample in [i16::MIN, -1, 0, 1, i16::MAX] {
            let bytes = sample.to_le_bytes();
            let decoded = decode_to_f32(&bytes, &format);
            assert_eq!(decoded.len(), 1);
            let encoded = encode_from_f32(&decoded, &format);
            let back = i16::from_le_bytes(encoded.try_into().unwrap());
            // The most negative sample is one past what `max` (built from
            // the positive range) can re-represent; everything else is
            // exact.
            assert!(
                (back as i32 - sample as i32).abs() <= 1,
                "{sample} -> {back}"
            );
        }
    }

    #[test]
    fn float32_round_trips_exactly() {
        let format = float32(2, 48000.0);
        let samples = [-1.0f32, -0.25, 0.0, 0.5, 1.0];
        let mut bytes = Vec::new();
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let decoded = decode_to_f32(&bytes, &format);
        assert_eq!(decoded, samples);
        assert_eq!(encode_from_f32(&decoded, &format), bytes);
    }

    #[test]
    fn big_endian_aiff_pcm_decodes_the_same_value_as_little_endian_wav() {
        let be = Format {
            format_flags: IS_BIG_ENDIAN | IS_SIGNED_INTEGER,
            ..pcm16(1, 44100.0)
        };
        let le = pcm16(1, 44100.0);
        let sample: i16 = 12345;
        assert_eq!(
            decode_to_f32(&sample.to_be_bytes(), &be),
            decode_to_f32(&sample.to_le_bytes(), &le)
        );
    }

    #[test]
    fn extended_float_round_trips_a_typical_sample_rate() {
        for rate in [8000.0, 44100.0, 48000.0, 96000.0] {
            let encoded = encode_extended(rate);
            let decoded = decode_extended(&encoded);
            assert!((decoded - rate).abs() < 0.001, "{rate} -> {decoded}");
        }
        assert_eq!(decode_extended(&encode_extended(0.0)), 0.0);
    }

    #[test]
    fn a_written_wav_file_reads_back_with_the_same_samples() {
        let dir = std::env::temp_dir().join(format!(
            "rosemary-extaudiofile-test-{:?}",
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip.wav");
        let mut file = File::create(&path).unwrap();
        let format = pcm16(1, 44100.0);
        let data_offset = write_wav_header(&mut file, &format).unwrap();
        let samples: [i16; 4] = [100, -100, 32767, -32768];
        let mut bytes = Vec::new();
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        file.write_all(&bytes).unwrap();
        finalize_wav(&mut file, data_offset, bytes.len() as u64).unwrap();
        drop(file);

        let mut reopened = File::open(&path).unwrap();
        let (parsed_format, parsed_offset, parsed_size) = parse_wav(&mut reopened).unwrap();
        assert_eq!(parsed_offset, data_offset);
        assert_eq!(parsed_size, bytes.len() as u64);
        assert_eq!(parsed_format.channels_per_frame, 1);
        assert_eq!(parsed_format.bits_per_channel, 16);
        assert_eq!(parsed_format.sample_rate, 44100.0);
        reopened.seek(SeekFrom::Start(parsed_offset)).unwrap();
        let mut read_back = vec![0u8; parsed_size as usize];
        reopened.read_exact(&mut read_back).unwrap();
        assert_eq!(read_back, bytes);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_written_aiff_file_reads_back_with_the_same_samples() {
        let dir = std::env::temp_dir().join(format!(
            "rosemary-extaudiofile-test-aiff-{:?}",
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip.aiff");
        let mut file = File::create(&path).unwrap();
        let format = Format {
            format_flags: IS_BIG_ENDIAN | IS_SIGNED_INTEGER,
            ..pcm16(2, 22050.0)
        };
        let data_offset = write_aiff_header(&mut file, &format).unwrap();
        assert_eq!(data_offset, AIFF_DATA_OFFSET);
        let samples: [i16; 4] = [1, -1, 1000, -1000];
        let mut bytes = Vec::new();
        for s in samples {
            bytes.extend_from_slice(&s.to_be_bytes());
        }
        file.write_all(&bytes).unwrap();
        finalize_aiff(&mut file, &format, bytes.len() as u64).unwrap();
        drop(file);

        let mut reopened = File::open(&path).unwrap();
        let (parsed_format, parsed_offset, parsed_size) = parse_aiff(&mut reopened).unwrap();
        assert_eq!(parsed_offset, data_offset);
        assert_eq!(parsed_size, bytes.len() as u64);
        assert_eq!(parsed_format.channels_per_frame, 2);
        assert!((parsed_format.sample_rate - 22050.0).abs() < 0.001);
        reopened.seek(SeekFrom::Start(parsed_offset)).unwrap();
        let mut read_back = vec![0u8; parsed_size as usize];
        reopened.read_exact(&mut read_back).unwrap();
        assert_eq!(read_back, bytes);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn get_property_reports_the_file_and_client_format_and_frame_count() {
        let format = pcm16(1, 44100.0);
        let ext = ExtAudioFile {
            file: File::open("/dev/null").unwrap(),
            writing: false,
            kind: Kind::Wav,
            file_format: format,
            client_format: format,
            data_offset: 44,
            data_bytes: 8,
            position: 0,
        };
        let handle = Box::into_raw(Box::new(Mutex::new(ext))) as *mut c_void;

        let mut frames: i64 = -1;
        let mut size = size_of::<i64>() as u32;
        let status = unsafe {
            ExtAudioFileGetProperty(
                handle,
                property::FILE_LENGTH_FRAMES,
                &mut size,
                (&mut frames as *mut i64).cast(),
            )
        };
        assert_eq!(status, SUCCESS);
        assert_eq!(frames, 4);

        let mut too_small = 1u32;
        let status = unsafe {
            ExtAudioFileGetProperty(
                handle,
                property::FILE_LENGTH_FRAMES,
                &mut too_small,
                (&mut frames as *mut i64).cast(),
            )
        };
        assert_eq!(status, INVALID_PROPERTY_SIZE);

        assert_eq!(unsafe { ExtAudioFileDispose(handle) }, SUCCESS);
    }

    #[test]
    fn set_client_format_refuses_a_different_rate_or_channel_count() {
        let format = pcm16(2, 44100.0);
        let ext = ExtAudioFile {
            file: File::open("/dev/null").unwrap(),
            writing: false,
            kind: Kind::Wav,
            file_format: format,
            client_format: format,
            data_offset: 44,
            data_bytes: 0,
            position: 0,
        };
        let handle = Box::into_raw(Box::new(Mutex::new(ext))) as *mut c_void;

        let wrong_rate = pcm16(2, 48000.0);
        let status = unsafe {
            ExtAudioFileSetProperty(
                handle,
                property::CLIENT_DATA_FORMAT,
                size_of::<Format>() as u32,
                (&wrong_rate as *const Format).cast(),
            )
        };
        assert_eq!(status, INVALID_DATA_FORMAT);

        let wrong_channels = pcm16(1, 44100.0);
        let status = unsafe {
            ExtAudioFileSetProperty(
                handle,
                property::CLIENT_DATA_FORMAT,
                size_of::<Format>() as u32,
                (&wrong_channels as *const Format).cast(),
            )
        };
        assert_eq!(status, INVALID_CHANNEL_MAP);

        let float_client = float32(2, 44100.0);
        let status = unsafe {
            ExtAudioFileSetProperty(
                handle,
                property::CLIENT_DATA_FORMAT,
                size_of::<Format>() as u32,
                (&float_client as *const Format).cast(),
            )
        };
        assert_eq!(status, SUCCESS);

        assert_eq!(unsafe { ExtAudioFileDispose(handle) }, SUCCESS);
    }

    #[test]
    fn reading_past_the_end_of_data_returns_fewer_frames_and_then_zero() {
        let format = pcm16(1, 44100.0);
        let dir = std::env::temp_dir().join(format!(
            "rosemary-extaudiofile-test-read-{:?}",
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("short.wav");
        let mut file = File::create(&path).unwrap();
        let data_offset = write_wav_header(&mut file, &format).unwrap();
        let samples: [i16; 3] = [10, 20, 30];
        let mut bytes = Vec::new();
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        file.write_all(&bytes).unwrap();
        finalize_wav(&mut file, data_offset, bytes.len() as u64).unwrap();
        drop(file);

        let ext = ExtAudioFile {
            file: File::open(&path).unwrap(),
            writing: false,
            kind: Kind::Wav,
            file_format: format,
            client_format: format,
            data_offset,
            data_bytes: bytes.len() as u64,
            position: 0,
        };
        let handle = Box::into_raw(Box::new(Mutex::new(ext))) as *mut c_void;

        let mut output = [0i16; 8];
        let mut list = AudioBufferList {
            count: 1,
            buffers: [AudioBuffer {
                channels: 1,
                byte_size: (output.len() * 2) as u32,
                data: output.as_mut_ptr().cast(),
            }; MAX_BUFFERS],
        };
        let mut frames = 8u32;
        let status = unsafe { ExtAudioFileRead(handle, &mut frames, &mut list) };
        assert_eq!(status, SUCCESS);
        assert_eq!(frames, 3);
        assert_eq!(&output[..3], &samples);

        let mut frames = 8u32;
        let status = unsafe { ExtAudioFileRead(handle, &mut frames, &mut list) };
        assert_eq!(status, SUCCESS);
        assert_eq!(frames, 0);

        assert_eq!(unsafe { ExtAudioFileDispose(handle) }, SUCCESS);
        std::fs::remove_dir_all(&dir).ok();
    }
}
