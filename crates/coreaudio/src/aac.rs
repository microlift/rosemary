//! AAC: the `AudioSpecificConfig` inside a magic cookie, and an
//! `AudioConverter` that decodes AAC to LPCM through FFmpeg's software `aac`
//! decoder (shared loader: [`ffmpeg`]).
//!
//! What this does *not* parse: a `ProgramConfigElement` (channel
//! configuration 0), the extension rate of explicit SBR/PS signalling (only
//! skipped, to reach the core), or anything past `frameLengthFlag` in
//! `GASpecificConfig`. None of that changes a raw AAC LC or HE-AAC stream's
//! sample rate, channel count or frame length, which is all this needs to
//! know; a cookie that needs it is refused rather than guessed at.

use std::ffi::c_void;
use std::sync::Once;

use ffmpeg::abi;
use ffmpeg::api::Api;

/// `kAudioFormatMPEG4AAC`, `'aac '`.
pub(crate) const AAC_LC: u32 = u32::from_be_bytes(*b"aac ");
/// `kAudioFormatMPEG4AAC_HE`, `'aach'`: adds spectral band replication, which
/// doubles the output sample rate and the samples per packet over the core
/// stream the `AudioSpecificConfig` describes.
pub(crate) const AAC_HE: u32 = u32::from_be_bytes(*b"aach");
/// `kAudioFormatMPEG4AAC_HE_V2`, `'aacp'`: HE-AAC plus parametric stereo,
/// same doubling.
pub(crate) const AAC_HE_V2: u32 = u32::from_be_bytes(*b"aacp");

pub(crate) fn is_aac(format_id: u32) -> bool {
    matches!(format_id, AAC_LC | AAC_HE | AAC_HE_V2)
}

/// Whether `format_id` doubles the core stream's rate and frame length
/// (spectral band replication).
pub(crate) fn is_sbr(format_id: u32) -> bool {
    matches!(format_id, AAC_HE | AAC_HE_V2)
}

/// A bit-at-a-time reader, most significant bit first - the order every
/// MPEG-4 bitstream element (`ES_Descriptor`, `AudioSpecificConfig`) uses.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Bits<'a> {
        Bits { data, pos: 0 }
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        if self.pos + n as usize > self.data.len() * 8 {
            return None;
        }
        let mut value = 0u32;
        for _ in 0..n {
            let byte = self.data[self.pos / 8];
            let bit = (byte >> (7 - self.pos % 8)) & 1;
            value = (value << 1) | u32::from(bit);
            self.pos += 1;
        }
        Some(value)
    }
}

/// The handful of `AudioSpecificConfig` fields a decoder needs before it has
/// decoded anything: ISO/IEC 14496-3, section 1.6.2.1.
pub(crate) struct AudioSpecificConfig {
    pub sample_rate: u32,
    pub channels: u32,
    /// `frameLengthFlag`: 1024 samples per frame when clear, 960 when set.
    pub frame_length_1024: bool,
}

/// Table 1.18: index 15 is an escape to an explicit 24-bit rate; 13 and 14
/// are reserved.
fn sampling_frequency(bits: &mut Bits) -> Option<u32> {
    match bits.bits(4)? {
        0 => Some(96000),
        1 => Some(88200),
        2 => Some(64000),
        3 => Some(48000),
        4 => Some(44100),
        5 => Some(32000),
        6 => Some(24000),
        7 => Some(22050),
        8 => Some(16000),
        9 => Some(12000),
        10 => Some(11025),
        11 => Some(8000),
        12 => Some(7350),
        15 => bits.bits(24),
        _ => None,
    }
}

/// Table 1.19's `channelConfiguration`: 0 (defined by a `ProgramConfigElement`
/// later in the stream, not here) and 8-15 (reserved) are not a plain count.
fn channel_configuration(bits: &mut Bits) -> Option<u32> {
    match bits.bits(4)? {
        n @ 1..=6 => Some(n),
        7 => Some(8),
        _ => None,
    }
}

/// `GetAudioObjectType()`: five bits, 31 escaping to 32 plus six more.
fn audio_object_type(bits: &mut Bits) -> Option<u32> {
    match bits.bits(5)? {
        31 => Some(32 + bits.bits(6)?),
        n => Some(n),
    }
}

/// Parses a bare `AudioSpecificConfig`, i.e. already stripped of any MP4
/// `esds` wrapper by [`strip_wrapper`].
pub(crate) fn parse_asc(bytes: &[u8]) -> Option<AudioSpecificConfig> {
    let mut bits = Bits::new(bytes);
    let mut object_type = audio_object_type(&mut bits)?;
    let sample_rate = sampling_frequency(&mut bits)?;
    let channels = channel_configuration(&mut bits)?;
    // Explicit SBR (5) or PS (29) signalling puts the extension rate and the
    // core's own object type between the channels and GASpecificConfig;
    // without skipping them the frame-length bit below is read from the
    // wrong place. The rate kept is the core's, the base-level format.
    if object_type == 5 || object_type == 29 {
        sampling_frequency(&mut bits)?;
        object_type = audio_object_type(&mut bits)?;
        if object_type == 22 {
            bits.bits(4)?;
        }
    }
    // Only these object types carry a GASpecificConfig; any other has no
    // frameLengthFlag here to read, so it is refused rather than misread.
    if !matches!(object_type, 1..=4 | 6 | 7 | 17 | 19..=23) {
        return None;
    }
    // `GASpecificConfig.frameLengthFlag`; `dependsOnCoreCoder` and whatever
    // follows it do not change the frame length and are not read.
    let frame_length_1024 = bits.bits(1)? == 0;
    Some(AudioSpecificConfig {
        sample_rate,
        channels,
        frame_length_1024,
    })
}

/// The MPEG-4 descriptor tag and payload at the start of `data`, and how many
/// bytes the tag plus its length took.
fn descriptor(data: &[u8]) -> Option<(u8, &[u8])> {
    let tag = *data.first()?;
    let mut pos = 1;
    let mut size = 0usize;
    loop {
        let b = *data.get(pos)?;
        pos += 1;
        size = (size << 7) | (b & 0x7f) as usize;
        if b & 0x80 == 0 {
            break;
        }
        // Four bytes is the format's own limit on a descriptor's length field.
        if pos > 4 {
            return None;
        }
    }
    let end = pos.checked_add(size)?;
    (end <= data.len()).then(|| (tag, &data[pos..end]))
}

/// Strips an MP4 `esds` box (`size`+`'esds'`+version/flags), an
/// `ES_Descriptor` (tag `0x03`) and a `DecoderConfigDescriptor` (tag `0x04`)
/// down to the `DecoderSpecificInfo` (tag `0x05`) payload, which for AAC is
/// the bare `AudioSpecificConfig`. A cookie that is already bare, or whose
/// wrapper does not look like one of these, is returned unchanged: trying to
/// parse it as a raw `AudioSpecificConfig` is then the caller's and
/// [`parse_asc`]'s to refuse, not this function's to guess at.
pub(crate) fn strip_wrapper(cookie: &[u8]) -> &[u8] {
    // A loop, not recursion: a guest's cookie of nested descriptors would
    // otherwise recurse once per five bytes, deep enough to overflow an
    // audio thread's stack.
    let mut cookie = cookie;
    loop {
        if cookie.len() > 12 && cookie[4..8] == *b"esds" {
            cookie = &cookie[12..];
            continue;
        }
        let rest = match descriptor(cookie) {
            // ES_Descriptor: ES_ID (2) + flags (1), then optionally
            // dependsOn_ES_ID (2), a URL (length-prefixed) and OCR_ES_Id (2),
            // then the DecoderConfigDescriptor this descends into.
            Some((0x03, payload)) if payload.len() >= 3 => {
                let flags = payload[2];
                let mut at = 3usize;
                if flags & 0x80 != 0 {
                    at += 2;
                }
                if flags & 0x40 != 0 {
                    match payload.get(at) {
                        Some(&len) => at += 1 + len as usize,
                        None => return cookie,
                    }
                }
                if flags & 0x20 != 0 {
                    at += 2;
                }
                payload.get(at..)
            }
            // DecoderConfigDescriptor: objectTypeIndication (1), streamType
            // and flags (1), bufferSizeDB (3), maxBitrate (4), avgBitrate
            // (4), then the DecoderSpecificInfo this descends into.
            Some((0x04, payload)) => payload.get(13..),
            // DecoderSpecificInfo: its payload already is the
            // AudioSpecificConfig.
            Some((0x05, payload)) => return payload,
            _ => return cookie,
        };
        match rest {
            Some(rest) => cookie = rest,
            None => return cookie,
        }
    }
}

/// A two-byte AAC LC `AudioSpecificConfig` for a description that came
/// without a magic cookie, which is how Roblox opens its converter: Apple's
/// decoder starts from the description alone. `None` for what two bytes
/// cannot say exactly - a rate outside the table (FFmpeg refuses the
/// explicit-frequency escape) or a channel count with no
/// `channelConfiguration` - so that is refused, not approximated.
///
/// HE-AAC's description gives the SBR-doubled rate and frame length; the
/// core this describes is half of each, and FFmpeg finds the SBR (and
/// HE-AAC v2's parametric stereo, over a mono core) in the stream itself.
pub(crate) fn asc_for_description(
    format_id: u32,
    sample_rate: f64,
    channels: u32,
    frames_per_packet: u32,
) -> Option<[u8; 2]> {
    const RATES: [u32; 13] = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    let (rate, frames) = match is_sbr(format_id) {
        true => (sample_rate / 2.0, frames_per_packet / 2),
        false => (sample_rate, frames_per_packet),
    };
    let index = RATES.iter().position(|&r| f64::from(r) == rate)? as u8;
    let channels = match format_id {
        AAC_HE_V2 => 1,
        _ => channels,
    };
    let configuration = match channels {
        1..=6 => channels as u8,
        8 => 7,
        _ => return None,
    };
    let frame_length_flag = u8::from(frames == 960);
    // audioObjectType 2 (5 bits), samplingFrequencyIndex (4),
    // channelConfiguration (4), frameLengthFlag, dependsOnCoreCoder and
    // extensionFlag (1 each, the last two clear).
    Some([
        (2 << 3) | (index >> 1),
        ((index & 1) << 7) | (configuration << 3) | (frame_length_flag << 2),
    ])
}

/// [`strip_wrapper`] then [`parse_asc`], for a cookie exactly as a guest
/// handed it over.
pub(crate) fn parse_cookie(cookie: &[u8]) -> Option<AudioSpecificConfig> {
    parse_asc(strip_wrapper(cookie))
}

/// `AVSampleFormat` `AV_SAMPLE_FMT_FLTP`: what FFmpeg's `aac` decoder always
/// produces, per `libavcodec/aacdec.c` (it never sets `request_sample_fmt`
/// away from the decoder's native planar float).
const SAMPLE_FMT_FLTP: i32 = 8;

type Ptr = *mut c_void;

/// One open AAC decoder: an `AVCodecContext` for FFmpeg's `aac` decoder, fed
/// compressed packets and drained of planar float frames.
pub(crate) struct AacDecoder {
    api: &'static Api,
    context: Ptr,
    packet: Ptr,
    frame: Ptr,
    /// Set once `avcodec_open2` has succeeded, which needs the
    /// `AudioSpecificConfig` as `extradata` first.
    opened: bool,
    pub(crate) reported_first_packet: Once,
}

// SAFETY: the FFmpeg objects are used by one thread at a time, which is all
// FFmpeg asks of a codec context; the converter's lock sees to it.
unsafe impl Send for AacDecoder {}

impl Drop for AacDecoder {
    fn drop(&mut self) {
        // SAFETY: objects this decoder allocated, each freed once; FFmpeg's
        // free functions take null. `avcodec_free_context` also frees
        // `extradata`, which was `libc::malloc`ed to match.
        unsafe {
            (self.api.avcodec_free_context)(&mut self.context);
            (self.api.av_packet_free)(&mut self.packet);
            (self.api.av_frame_free)(&mut self.frame);
        }
    }
}

impl AacDecoder {
    /// An unopened decoder: the codec exists, but nothing can be decoded
    /// until [`AacDecoder::set_extradata`] gives it an `AudioSpecificConfig`.
    pub(crate) fn new() -> Result<AacDecoder, String> {
        let api = ffmpeg::api::global().map_err(str::to_string)?;
        // SAFETY: a NUL-terminated name.
        let codec = unsafe { (api.avcodec_find_decoder_by_name)(c"aac".as_ptr()) };
        if codec.is_null() {
            return Err("this FFmpeg has no software aac decoder".to_string());
        }
        // SAFETY: resolved entry points; each result is checked below.
        let decoder = unsafe {
            AacDecoder {
                api,
                context: (api.avcodec_alloc_context3)(codec),
                packet: (api.av_packet_alloc)(),
                frame: (api.av_frame_alloc)(),
                opened: false,
                reported_first_packet: Once::new(),
            }
        };
        if [decoder.context, decoder.packet, decoder.frame]
            .iter()
            .any(|p| p.is_null())
        {
            return Err("FFmpeg is out of memory".to_string());
        }
        Ok(decoder)
    }

    /// Opens (or reopens) the decoder with `cookie` as the magic cookie:
    /// [`strip_wrapper`] down to the bare `AudioSpecificConfig`, which
    /// becomes the context's `extradata`.
    ///
    /// A later call replaces the context outright, since `avcodec_open2`
    /// cannot be redone on one that is already open.
    pub(crate) fn set_extradata(&mut self, cookie: &[u8]) -> Result<(), String> {
        let codec = self.reopen_context()?;
        let asc = strip_wrapper(cookie);
        let Ok(size) = i32::try_from(asc.len()) else {
            return Err("the magic cookie is too large".to_string());
        };
        // SAFETY: a fresh context; `extradata` is `libc::malloc`ed with the
        // padding FFmpeg's own comment on the field asks for, so freeing it
        // with `free()` in `avcodec_free_context` is the same allocator on
        // both ends.
        let status = unsafe {
            let buffer = libc::malloc(asc.len() + abi::INPUT_BUFFER_PADDING_SIZE).cast::<u8>();
            if buffer.is_null() {
                return Err("out of memory".to_string());
            }
            std::ptr::copy_nonoverlapping(asc.as_ptr(), buffer, asc.len());
            buffer
                .add(asc.len())
                .write_bytes(0, abi::INPUT_BUFFER_PADDING_SIZE);
            abi::write::<*mut u8>(self.context, abi::context::EXTRADATA, buffer);
            abi::write::<i32>(self.context, abi::context::EXTRADATA_SIZE, size);
            (self.api.avcodec_open2)(self.context, codec, std::ptr::null_mut())
        };
        if status < 0 {
            return Err(format!(
                "FFmpeg did not open the aac decoder (error {status})"
            ));
        }
        // Not the context's `sample_rate`: FFmpeg's aac decoder leaves it at
        // zero until the first frame (it may yet find SBR and double it), so
        // each frame's own rate and channel count are what the output is
        // converted from.
        self.opened = true;
        Ok(())
    }

    /// Frees the current context (which also frees its `extradata`, if any)
    /// and allocates a fresh, unopened one for the same codec, for
    /// [`AacDecoder::set_extradata`] to configure and open.
    fn reopen_context(&mut self) -> Result<*const c_void, String> {
        // SAFETY: a NUL-terminated name.
        let codec = unsafe { (self.api.avcodec_find_decoder_by_name)(c"aac".as_ptr()) };
        if codec.is_null() {
            return Err("this FFmpeg has no software aac decoder".to_string());
        }
        // SAFETY: `avcodec_free_context` frees `self.context` (and its
        // `extradata`, which `set_extradata` allocated to match) and nulls
        // the pointer; it takes a null pointer safely too, which is how the
        // first open gets here.
        unsafe { (self.api.avcodec_free_context)(&mut self.context) };
        self.opened = false;
        // SAFETY: `codec` just resolved.
        self.context = unsafe { (self.api.avcodec_alloc_context3)(codec) };
        if self.context.is_null() {
            return Err("FFmpeg is out of memory".to_string());
        }
        Ok(codec)
    }

    /// Sends one compressed AAC access unit.
    pub(crate) fn send(&mut self, bytes: &[u8]) -> Result<(), i32> {
        let api = self.api;
        let Ok(size) = i32::try_from(bytes.len()) else {
            return Err(abi::INVALIDDATA);
        };
        // SAFETY: this decoder's packet, filled with a fresh, padded buffer
        // of `size` bytes, which the copy below fits.
        let status = unsafe {
            (api.av_packet_unref)(self.packet);
            if (api.av_new_packet)(self.packet, size) < 0 {
                return Err(-1);
            }
            let data = abi::read::<*mut u8>(self.packet, abi::packet::DATA);
            if data.is_null() {
                return Err(-1);
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            (api.avcodec_send_packet)(self.context, self.packet)
        };
        // Not `EAGAIN` as success: that means FFmpeg did not take the
        // packet (one is already waiting), and the caller drains between
        // packets so that it never happens; were it to, the packet would be
        // lost silently.
        match status {
            0 => Ok(()),
            other => Err(other),
        }
    }

    /// Pulls one decoded frame's samples, interleaved as `f32`, appended to
    /// `out`, with the frame's own sample rate and channel count - which
    /// FFmpeg may change from one frame to the next (implicit SBR or PS is
    /// only found by decoding). `Ok(None)` once FFmpeg has none ready now.
    pub(crate) fn receive_interleaved(
        &mut self,
        out: &mut Vec<f32>,
    ) -> Result<Option<(u32, u32)>, i32> {
        let api = self.api;
        // SAFETY: the open context and this decoder's frame.
        let status = unsafe { (api.avcodec_receive_frame)(self.context, self.frame) };
        match status {
            abi::EAGAIN | abi::EOF => return Ok(None),
            s if s < 0 => return Err(s),
            _ => {}
        }
        self.reported_first_packet.call_once(|| {
            eprintln!("rosemary: coreaudio: first AAC packet decoded through FFmpeg");
        });
        let result = self.interleave(out);
        // SAFETY: the frame, no longer needed once copied out.
        unsafe { (api.av_frame_unref)(self.frame) };
        result.map(Some)
    }

    /// The frame `avcodec_receive_frame` just filled, appended to `out`
    /// interleaved, and its sample rate and channel count.
    fn interleave(&self, out: &mut Vec<f32>) -> Result<(u32, u32), i32> {
        // SAFETY: a frame `avcodec_receive_frame` just filled.
        let (format, nb_samples, rate, channels) = unsafe {
            (
                abi::read::<i32>(self.frame, abi::frame::FORMAT),
                abi::read::<i32>(self.frame, abi::frame::NB_SAMPLES),
                abi::read::<i32>(self.frame, abi::frame::SAMPLE_RATE),
                abi::read::<i32>(
                    self.frame,
                    abi::frame::CH_LAYOUT + abi::ch_layout::NB_CHANNELS,
                ),
            )
        };
        if format != SAMPLE_FMT_FLTP {
            return Err(-1);
        }
        let (Ok(nb_samples), Ok(rate), Ok(channels)) = (
            usize::try_from(nb_samples),
            u32::try_from(rate),
            usize::try_from(channels),
        ) else {
            return Err(-1);
        };
        if rate == 0 || channels == 0 {
            return Err(-1);
        }
        // SAFETY: as above; for audio, `extended_data` always points at
        // one plane per channel (at `data` itself up to eight).
        let planes =
            unsafe { abi::read::<*const *const f32>(self.frame, abi::frame::EXTENDED_DATA) };
        if planes.is_null() {
            return Err(-1);
        }
        let base = out.len();
        out.resize(base + nb_samples * channels, 0.0);
        for channel in 0..channels {
            // SAFETY: one of `channels` plane pointers.
            let plane = unsafe { planes.add(channel).read() };
            if plane.is_null() {
                out.truncate(base);
                return Err(-1);
            }
            // SAFETY: FFmpeg allocated this plane at least `nb_samples`
            // floats long.
            let plane = unsafe { std::slice::from_raw_parts(plane, nb_samples) };
            for (frame, &sample) in plane.iter().enumerate() {
                out[base + frame * channels + channel] = sample;
            }
        }
        Ok((rate, channels as u32))
    }

    /// `AudioConverterReset`: drop whatever is in flight and let the decoder
    /// start again at the next access unit.
    pub(crate) fn flush(&mut self) {
        if self.opened {
            // SAFETY: the open context.
            unsafe { (self.api.avcodec_flush_buffers)(self.context) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packs `(value, width)` pairs into bytes, most significant bit first,
    /// zero-padding the last byte - the inverse of [`Bits`], and much less
    /// error-prone than writing the hex by hand for every test fixture.
    fn pack_bits(fields: &[(u32, u32)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let (mut cur, mut filled) = (0u8, 0u32);
        for &(value, width) in fields {
            for i in (0..width).rev() {
                cur = (cur << 1) | ((value >> i) & 1) as u8;
                filled += 1;
                if filled == 8 {
                    bytes.push(cur);
                    (cur, filled) = (0, 0);
                }
            }
        }
        if filled > 0 {
            bytes.push(cur << (8 - filled));
        }
        bytes
    }

    /// A plain AAC LC `AudioSpecificConfig`: object type 2, 44.1kHz, stereo,
    /// 1024-sample frames.
    fn stereo_44k_lc() -> Vec<u8> {
        pack_bits(&[(2, 5), (4, 4), (2, 4), (0, 1)])
    }

    #[test]
    fn a_bare_asc_parses_rate_channels_and_frame_length() {
        let bytes = stereo_44k_lc();
        assert_eq!(bytes, [0x12, 0x10], "the textbook two-byte AAC LC encoding");
        let asc = parse_asc(&bytes).expect("a plain AAC LC config parses");
        assert_eq!(asc.sample_rate, 44100);
        assert_eq!(asc.channels, 2);
        assert!(asc.frame_length_1024);
    }

    #[test]
    fn channel_configuration_zero_and_reserved_values_are_refused() {
        // Object type 2, rate index 4 (44100), channel config 0: defined by
        // a ProgramConfigElement later in the stream, which this does not
        // read, so it is refused rather than assumed to be mono.
        let pce_channels = pack_bits(&[(2, 5), (4, 4), (0, 4)]);
        assert!(parse_asc(&pce_channels).is_none());
    }

    #[test]
    fn a_bare_cookie_is_not_mistaken_for_a_wrapped_one() {
        let bytes = stereo_44k_lc();
        assert_eq!(strip_wrapper(&bytes), bytes.as_slice());
    }

    /// `DecoderSpecificInfo` (tag 5) holding the same bytes, as a
    /// `DecoderConfigDescriptor` (tag 4) holding *that*, as an
    /// `ES_Descriptor` (tag 3, minimal flags) holding *that* - the whole
    /// chain an `esds` box's payload carries.
    #[test]
    fn an_es_descriptor_chain_is_stripped_to_the_bare_config() {
        let asc = stereo_44k_lc();
        let dsi = {
            let mut v = vec![0x05, asc.len() as u8];
            v.extend_from_slice(&asc);
            v
        };
        let dcd = {
            // objectTypeIndication, streamType/flags, bufferSizeDB(3),
            // maxBitrate(4), avgBitrate(4).
            let mut fixed = vec![0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            fixed.extend_from_slice(&dsi);
            let mut v = vec![0x04, fixed.len() as u8];
            v.extend_from_slice(&fixed);
            v
        };
        let es = {
            // ES_ID(2) + flags(1, all clear: no depends/URL/OCR).
            let mut fixed = vec![0, 1, 0x00];
            fixed.extend_from_slice(&dcd);
            let mut v = vec![0x03, fixed.len() as u8];
            v.extend_from_slice(&fixed);
            v
        };
        assert_eq!(strip_wrapper(&es), asc.as_slice());

        let mut boxed = Vec::new();
        boxed.extend_from_slice(&(es.len() as u32 + 12).to_be_bytes());
        boxed.extend_from_slice(b"esds");
        boxed.extend_from_slice(&[0, 0, 0, 0]); // version/flags
        boxed.extend_from_slice(&es);
        assert_eq!(strip_wrapper(&boxed), asc.as_slice());
    }

    #[test]
    fn the_explicit_frequency_escape_reads_a_24_bit_rate() {
        // Object type 2, rate index 15 (escape) + explicit 24-bit 12345,
        // channel config 1, frameLengthFlag 1 (960-sample frames).
        let bytes = pack_bits(&[(2, 5), (15, 4), (12345, 24), (1, 4), (1, 1)]);
        let asc = parse_asc(&bytes).expect("an explicit-rate config parses");
        assert_eq!(asc.sample_rate, 12345);
        assert_eq!(asc.channels, 1);
        assert!(!asc.frame_length_1024);
    }

    /// Explicit SBR puts an extension rate and the core's object type before
    /// GASpecificConfig; an extension index of 8 starts with a set bit, which
    /// read as the frame-length flag would claim 960-sample frames.
    #[test]
    fn explicit_sbr_signalling_reads_the_core_config_after_it() {
        let bytes = pack_bits(&[(5, 5), (11, 4), (2, 4), (8, 4), (2, 5), (0, 1)]);
        let asc = parse_asc(&bytes).expect("explicit SBR parses");
        assert_eq!(asc.sample_rate, 8000);
        assert_eq!(asc.channels, 2);
        assert!(asc.frame_length_1024);
    }

    /// Roblox's own cookie: each descriptor length written as four bytes.
    #[test]
    fn four_byte_descriptor_lengths_are_followed() {
        let wrap = |tag: u8, body: &[u8]| {
            let mut v = vec![tag, 0x80, 0x80, 0x80, body.len() as u8];
            v.extend_from_slice(body);
            v
        };
        let asc = stereo_44k_lc();
        let mut dcd = vec![0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        dcd.extend_from_slice(&wrap(0x05, &asc));
        let mut es = vec![0, 0, 0];
        es.extend_from_slice(&wrap(0x04, &dcd));
        assert_eq!(strip_wrapper(&wrap(0x03, &es)), asc.as_slice());
    }

    #[test]
    fn a_description_without_a_cookie_becomes_the_matching_lc_config() {
        let asc = asc_for_description(AAC_LC, 44100.0, 2, 1024).expect("a table rate, stereo");
        assert_eq!(asc, [0x12, 0x10]);
        // HE-AAC's description has the doubled rate; the core is 22050.
        assert_eq!(
            asc_for_description(AAC_HE, 44100.0, 2, 2048),
            Some([0x13, 0x90])
        );
        assert!(asc_for_description(AAC_LC, 44101.0, 2, 1024).is_none());
        assert!(asc_for_description(AAC_LC, 44100.0, 7, 1024).is_none());
    }

    #[test]
    fn a_truncated_config_is_refused_not_guessed_at() {
        // Only the object type and part of the rate index: not enough bits
        // for even the fields this reads, let alone the ones it does not.
        assert!(parse_asc(&[0x00]).is_none());
        assert!(parse_asc(&[]).is_none());
    }
}
