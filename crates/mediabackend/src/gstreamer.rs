//! Optional GStreamer backend: incremental H.264 Annex-B AU input, owned
//! decoded output, and exact identity independent of reordered PTS/DTS.
//!
//! This implements the backend contract only; it does not claim to implement
//! synchronous VideoToolbox decode, FinishDelayedFrames, or GPU CVPixelBuffers.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use gst::glib::translate::IntoGlib;
use gst::prelude::*;
use gst_app::{AppSink, AppSrc};
use gst_video::prelude::*;
use gst_video::video_frame::VideoBufferExt;

const INPUT_QUEUE_LIMIT: u64 = 8 * 1024 * 1024;
// appsink depth; submit reports Backpressure before the sink would block.
const OUTPUT_QUEUE_LIMIT: u64 = 32;
// Each initial SPS/PPS contributes a four-byte start code. Leave at least
// one start code and one NAL header for the first submitted access unit.
const MAX_INITIAL_PARAMETER_BYTES: usize = INPUT_QUEUE_LIMIT as usize - 5;

// GStreamer uses unsigned nanoseconds with u64::MAX reserved for NONE.
// Bias signed media times into a positive range (146 years of negative
// preroll, all positive i64 nanoseconds). Reject values outside this range.
const MEDIA_TIME_EPOCH_NS: i128 = 1_i128 << 62;

fn gst_media_time(ns: i64) -> Result<gst::ClockTime, Error> {
    let unsigned = i128::from(ns) + MEDIA_TIME_EPOCH_NS;
    if unsigned < 0 || unsigned >= i128::from(u64::MAX) {
        return Err(Error::InvalidInput(
            "timestamp outside GStreamer timeline range".into(),
        ));
    }
    Ok(gst::ClockTime::from_nseconds(unsigned as u64))
}

fn original_media_time(time: gst::ClockTime) -> Result<i64, Error> {
    i64::try_from(i128::from(time.nseconds()) - MEDIA_TIME_EPOCH_NS)
        .map_err(|_| Error::Decode("decoded timestamp outside signed timeline".into()))
}

// Distinguish genuinely unspecified metadata from both known and new
// host-supported colorimetry values. In particular, unknown GStreamer enum
// numbers must not silently look like unspecified color data.
fn color_matrix(matrix: gst_video::VideoColorMatrix) -> ColorMatrix {
    use gst_video::VideoColorMatrix as M;
    match matrix {
        M::Unknown => ColorMatrix::Unknown,
        M::Rgb => ColorMatrix::Rgb,
        M::Fcc => ColorMatrix::Fcc,
        M::Bt601 => ColorMatrix::Bt601,
        M::Bt709 => ColorMatrix::Bt709,
        M::Smpte240m => ColorMatrix::Smpte240m,
        M::Bt2020 => ColorMatrix::Bt2020,
        other => ColorMatrix::Other(other.into_glib()),
    }
}

fn color_primaries(primaries: gst_video::VideoColorPrimaries) -> ColorPrimaries {
    use gst_video::VideoColorPrimaries as P;
    match primaries {
        P::Unknown => ColorPrimaries::Unknown,
        P::Bt709 => ColorPrimaries::Bt709,
        P::Bt470m => ColorPrimaries::Bt470m,
        P::Bt470bg => ColorPrimaries::Bt470bg,
        P::Smpte170m => ColorPrimaries::Smpte170m,
        P::Smpte240m => ColorPrimaries::Smpte240m,
        P::Film => ColorPrimaries::Film,
        P::Bt2020 => ColorPrimaries::Bt2020,
        P::Adobergb => ColorPrimaries::AdobeRgb,
        P::Smptest428 => ColorPrimaries::Smptest428,
        P::Smpterp431 => ColorPrimaries::Smpterp431,
        P::Smpteeg432 => ColorPrimaries::Smpteeg432,
        P::Ebu3213 => ColorPrimaries::Ebu3213,
        other => ColorPrimaries::Other(other.into_glib()),
    }
}

fn color_transfer(transfer: gst_video::VideoTransferFunction) -> ColorTransfer {
    use gst_video::VideoTransferFunction as T;
    match transfer {
        T::Unknown => ColorTransfer::Unknown,
        T::Gamma10 => ColorTransfer::Gamma10,
        T::Gamma18 => ColorTransfer::Gamma18,
        T::Gamma20 => ColorTransfer::Gamma20,
        T::Gamma22 => ColorTransfer::Gamma22,
        T::Gamma28 => ColorTransfer::Gamma28,
        T::Bt709 => ColorTransfer::Bt709,
        T::Smpte240m => ColorTransfer::Smpte240m,
        T::Srgb => ColorTransfer::Srgb,
        T::Log100 => ColorTransfer::Log100,
        T::Log316 => ColorTransfer::Log316,
        T::Bt202010 => ColorTransfer::Bt202010,
        T::Bt202012 => ColorTransfer::Bt202012,
        T::Adobergb => ColorTransfer::AdobeRgb,
        T::Smpte2084 => ColorTransfer::Smpte2084,
        T::AribStdB67 => ColorTransfer::AribStdB67,
        T::Bt601 => ColorTransfer::Bt601,
        other => ColorTransfer::Other(other.into_glib()),
    }
}

fn chroma_siting(site: gst_video::VideoChromaSite) -> ChromaSiting {
    use gst_video::VideoChromaSite as S;
    if site.is_empty() {
        ChromaSiting::Unknown
    } else if site == S::JPEG {
        ChromaSiting::Jpeg
    } else if site == S::MPEG2 {
        ChromaSiting::Mpeg2
    } else if site == S::DV {
        ChromaSiting::Dv
    } else if site == S::COSITED {
        ChromaSiting::Cosited
    } else if site == S::ALT_LINE {
        ChromaSiting::AlternateLine
    } else {
        ChromaSiting::Other(site.bits())
    }
}

use crate::{
    AccessUnit, ChromaSiting, ColorMatrix, ColorPrimaries, ColorTransfer, DroppedSample, Error,
    FrameCrop, FrameFieldOrder, FrameInterlaceMode, FrameLayout, PixelFormat, VideoCodec,
    VideoDecoder, VideoFrame,
};

fn sample_id(buffer: &gst::BufferRef, reference: &gst::Caps) -> Option<u64> {
    buffer
        .iter_meta::<gst::ReferenceTimestampMeta>()
        .find(|meta| meta.reference() == reference.as_ref())
        .map(|meta| meta.timestamp().nseconds())
}

// Neither h264parse nor avdec_h264 reports which input it discarded or
// folded into another picture, so infer it at the decoder's pads. The whole
// pipeline runs on appsrc's one streaming thread with no queues, so a sample
// reaching the decoder means every earlier one has entered it or been
// dropped upstream, and one leaving it means every sample that is no longer
// among the decoder's pending frames was discarded.
#[derive(Default)]
struct SampleTracker {
    // Submitted but not yet seen by the decoder, in submission order.
    upstream: VecDeque<u64>,
    // Seen by the decoder but neither output nor known to be dropped.
    decoding: Vec<u64>,
    dropped: VecDeque<DroppedSample>,
}

impl SampleTracker {
    fn entered_decoder(&mut self, id: u64) {
        if let Some(position) = self.upstream.iter().position(|&queued| queued == id) {
            for id in self.upstream.drain(..position) {
                self.dropped.push_back(DroppedSample { id, error: None });
            }
            self.upstream.pop_front();
        }
        self.decoding.push(id);
    }

    fn left_decoder(&mut self, id: u64, pending: &[u64]) {
        for decoding in std::mem::take(&mut self.decoding) {
            if pending.contains(&decoding) {
                self.decoding.push(decoding);
            } else if decoding != id {
                self.dropped.push_back(DroppedSample {
                    id: decoding,
                    error: None,
                });
            }
        }
    }

    fn end_of_stream(&mut self) {
        for id in self.upstream.drain(..).chain(self.decoding.drain(..)) {
            self.dropped.push_back(DroppedSample { id, error: None });
        }
    }
}

fn lock_tracker(tracker: &Mutex<SampleTracker>) -> MutexGuard<'_, SampleTracker> {
    tracker.lock().unwrap_or_else(PoisonError::into_inner)
}

fn track_decoder(
    decoder: &gst_video::VideoDecoder,
    reference: &gst::Caps,
    tracker: &Arc<Mutex<SampleTracker>>,
) -> Result<(), Error> {
    let pad = |name| {
        decoder
            .static_pad(name)
            .ok_or_else(|| Error::BackendUnavailable(format!("decoder has no {name} pad")))
    };
    let (sink, src) = (pad("sink")?, pad("src")?);
    let (entering, reference_in) = (tracker.clone(), reference.clone());
    sink.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(id) = info.buffer().and_then(|b| sample_id(b, &reference_in)) {
            lock_tracker(&entering).entered_decoder(id);
        }
        gst::PadProbeReturn::Ok
    });
    // A strong reference here would keep the element alive through its pad.
    let weak_decoder = decoder.downgrade();
    let (leaving, reference_out) = (tracker.clone(), reference.clone());
    src.add_probe(
        gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM,
        move |_, info| {
            if let Some(gst::EventView::Eos(_)) = info.event().map(|e| e.view()) {
                lock_tracker(&leaving).end_of_stream();
            } else if let Some(id) = info.buffer().and_then(|b| sample_id(b, &reference_out))
                && let Some(decoder) = weak_decoder.upgrade()
            {
                let pending = decoder
                    .frames()
                    .iter()
                    .filter_map(|frame| {
                        frame
                            .input_buffer()
                            .and_then(|b| sample_id(b, &reference_out))
                    })
                    .collect::<Vec<_>>();
                lock_tracker(&leaving).left_decoder(id, &pending);
            }
            gst::PadProbeReturn::Ok
        },
    );
    Ok(())
}

pub struct Decoder {
    pipeline: gst::Pipeline,
    input: AppSrc,
    output: AppSink,
    reference: gst::Caps,
    tracker: Arc<Mutex<SampleTracker>>,
    completed: bool,
    fatal_error: Option<String>,
    output_format: PixelFormat,
    avcc: Option<AvccConfiguration>,
    pending_pps: Option<Vec<u8>>,
    avcc_annexb_fallback: bool,
    initial_annexb_sets_pending: bool,
    source_pts_reference: gst::Caps,
    source_dts_reference: gst::Caps,
}

struct AvccConfiguration {
    sps: Vec<Vec<u8>>,
    pps: Vec<Vec<u8>>,
    nal_length: u8,
    high_profile: Option<AvccHighProfile>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AvccHighProfile {
    chroma_format: u8,
    bit_depth_luma_minus8: u8,
    bit_depth_chroma_minus8: u8,
}

// Read only the fields needed by ISO/IEC 14496-15's high-profile avcC
// extension. Do not guess chroma format or bit depth from profile_idc.
fn avcc_sps_profile(sps: &[u8]) -> Result<Option<AvccHighProfile>, Error> {
    if sps.len() < 5 || sps[0] & 31 != 7 {
        return Err(Error::InvalidInput(
            "invalid AVC sequence parameter set".into(),
        ));
    }
    let mut rbsp = Vec::with_capacity(sps.len());
    let mut zero_count = 0;
    for &byte in &sps[1..] {
        if zero_count == 2 && byte == 3 {
            zero_count = 0;
            continue;
        }
        rbsp.push(byte);
        zero_count = if byte == 0 { zero_count + 1 } else { 0 };
    }
    fn bits(data: &[u8], pos: &mut usize, width: usize) -> Option<u32> {
        if width > 32 || pos.checked_add(width)? > data.len() * 8 {
            return None;
        }
        let mut value = 0;
        for _ in 0..width {
            value = (value << 1) | u32::from(data[*pos / 8] >> (7 - *pos % 8) & 1);
            *pos += 1;
        }
        Some(value)
    }
    fn ue(data: &[u8], pos: &mut usize) -> Option<u32> {
        let mut zeroes = 0;
        while bits(data, pos, 1)? == 0 {
            zeroes += 1;
            if zeroes > 31 {
                return None;
            }
        }
        Some(((1_u64 << zeroes) - 1 + u64::from(bits(data, pos, zeroes)?)) as u32)
    }
    let mut pos = 24; // profile_idc, constraint flags, level_idc
    let sps_id =
        ue(&rbsp, &mut pos).ok_or_else(|| Error::InvalidInput("truncated AVC SPS ID".into()))?;
    if sps_id > 31 {
        return Err(Error::InvalidInput("invalid AVC SPS ID".into()));
    }
    // H.264 7.3.2.1.1 lists the profiles carrying chroma_format_idc and bit
    // depths; 144 is the withdrawn High 4:4:4, which encoders still emit.
    match sps[1] {
        66 | 77 | 88 => return Ok(None),
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135 | 144 => {}
        _ => return Err(Error::InvalidInput("unsupported AVC profile".into())),
    }
    let read = || Error::InvalidInput("truncated AVC high-profile SPS".into());
    let chroma_format = ue(&rbsp, &mut pos).ok_or_else(read)?;
    if chroma_format > 3 {
        return Err(Error::InvalidInput("invalid AVC chroma format".into()));
    }
    if chroma_format == 3 && bits(&rbsp, &mut pos, 1).ok_or_else(read)? != 0 {
        return Err(Error::InvalidInput(
            "separate-colour-plane AVC SPS not supported".into(),
        ));
    }
    let luma_depth = ue(&rbsp, &mut pos).ok_or_else(read)?;
    let chroma_depth = ue(&rbsp, &mut pos).ok_or_else(read)?;
    if luma_depth > 6 || chroma_depth > 6 {
        return Err(Error::InvalidInput("invalid AVC bit depth".into()));
    }
    Ok(Some(AvccHighProfile {
        chroma_format: chroma_format as u8,
        bit_depth_luma_minus8: luma_depth as u8,
        bit_depth_chroma_minus8: chroma_depth as u8,
    }))
}

impl AvccConfiguration {
    fn new(sets: &[&[u8]], nal_length: u8) -> Result<Self, Error> {
        if !matches!(nal_length, 1 | 2 | 4) {
            return Err(Error::InvalidInput("invalid AVCC NAL length width".into()));
        }
        let mut sps = Vec::new();
        let mut pps = Vec::new();
        let mut initial_injection_bytes = 0usize;
        for &set in sets {
            // avcC parameter-set lengths are always u16; sample NAL prefixes
            // use the independently negotiated nal_length width.
            if set.first().is_none_or(|b| b & 0x80 != 0) || set.len() > u16::MAX as usize {
                return Err(Error::InvalidInput("malformed AVCC parameter set".into()));
            }
            initial_injection_bytes = initial_injection_bytes
                .checked_add(4)
                .and_then(|count| count.checked_add(set.len()))
                .filter(|count| *count <= MAX_INITIAL_PARAMETER_BYTES)
                .ok_or_else(|| {
                    Error::InvalidInput(
                        "aggregate AVC parameter sets exceed input queue limit".into(),
                    )
                })?;
            match set[0] & 31 {
                7 if set.len() >= 4 => sps.push(set.to_vec()),
                8 if set.len() >= 2 => pps.push(set.to_vec()),
                _ => return Err(Error::InvalidInput("unexpected parameter set type".into())),
            }
        }
        if sps.is_empty() || sps.len() > 31 || pps.is_empty() || pps.len() > 255 {
            return Err(Error::InvalidInput("AVCC requires SPS/PPS".into()));
        }
        let high_profile = avcc_sps_profile(&sps[0])?;
        for other in sps.iter().skip(1) {
            if other[1] != sps[0][1] || avcc_sps_profile(other)? != high_profile {
                return Err(Error::InvalidInput(
                    "conflicting AVC SPS profiles or format parameters".into(),
                ));
            }
        }
        Ok(Self {
            sps,
            pps,
            nal_length,
            high_profile,
        })
    }

    fn caps(&self) -> String {
        let mut avcc = vec![
            1,
            self.sps[0][1],
            self.sps[0][2],
            self.sps[0][3],
            0xfc | (self.nal_length - 1),
            0xe0 | self.sps.len() as u8,
        ];
        for set in &self.sps {
            avcc.extend_from_slice(&(set.len() as u16).to_be_bytes());
            avcc.extend_from_slice(set);
        }
        avcc.push(self.pps.len() as u8);
        for set in &self.pps {
            avcc.extend_from_slice(&(set.len() as u16).to_be_bytes());
            avcc.extend_from_slice(set);
        }
        if let Some(high) = self.high_profile {
            avcc.extend_from_slice(&[
                0xfc | high.chroma_format,
                0xf8 | high.bit_depth_luma_minus8,
                0xf8 | high.bit_depth_chroma_minus8,
                0, // no sequence parameter set extensions were supplied
            ]);
        }
        let hex = avcc
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!(
            "video/x-h264,stream-format=(string)avc,alignment=(string)au,codec_data=(buffer){hex}"
        )
    }
}

pub struct DecodedFrame {
    id: u64,
    sample: gst::Sample,
    layout: FrameLayout,
    pts_ns: Option<i64>,
    dts_ns: Option<i64>,
    duration_ns: Option<u64>,
    source_pts_ns: Option<i64>,
    source_dts_ns: Option<i64>,
}

impl VideoFrame for DecodedFrame {
    fn id(&self) -> u64 {
        self.id
    }

    fn layout(&self) -> &FrameLayout {
        &self.layout
    }

    fn pts_ns(&self) -> Option<i64> {
        self.pts_ns
    }

    fn dts_ns(&self) -> Option<i64> {
        self.dts_ns
    }

    fn duration_ns(&self) -> Option<u64> {
        self.duration_ns
    }

    fn source_pts_ns(&self) -> Option<i64> {
        self.source_pts_ns
    }

    fn source_dts_ns(&self) -> Option<i64> {
        self.source_dts_ns
    }

    fn with_bytes<R>(&self, read: impl FnOnce(&[u8], &FrameLayout) -> R) -> Result<R, Error> {
        let buffer = self
            .sample
            .buffer()
            .ok_or_else(|| Error::Decode("missing decoded buffer".into()))?;
        let mapped = buffer
            .map_readable()
            .map_err(|_| Error::Decode("cannot map decoder buffer".into()))?;
        Ok(read(mapped.as_slice(), &self.layout))
    }
}

/// Reject invalid offsets/strides before exposing a mapped buffer to a
/// CoreVideo or renderer consumer. Use checked arithmetic for untrusted caps.
fn validate_planes(layout: &FrameLayout, size: usize) -> Result<(), Error> {
    if layout.width == 0 || layout.height == 0 || layout.width > 32768 || layout.height > 32768 {
        return Err(Error::Decode("invalid decoded plane dimensions".into()));
    }
    if let Some(crop) = layout.crop
        && (crop.width == 0
            || crop.height == 0
            || crop
                .x
                .checked_add(crop.width)
                .is_none_or(|x| x > layout.width)
            || crop
                .y
                .checked_add(crop.height)
                .is_none_or(|y| y > layout.height))
    {
        return Err(Error::Decode(
            "decoded crop exceeds frame dimensions".into(),
        ));
    }
    let plane_count = match layout.format {
        PixelFormat::Nv12 => 2,
        PixelFormat::I420 => 3,
        PixelFormat::Bgra => 1,
    };
    if plane_count != layout.strides.len() || plane_count != layout.offsets.len() {
        return Err(Error::Decode("invalid decoded plane count".into()));
    }
    for plane in 0..plane_count {
        let end = layout
            .plane_byte_span(plane)
            .and_then(|span| layout.offsets[plane].checked_add(span));
        if end.is_none_or(|n| n > size) {
            return Err(Error::Decode(format!(
                "decoded plane {plane} exceeds mapped buffer"
            )));
        }
    }
    Ok(())
}

// GStreamer only allows caps field-order for INTERLEAVED; when it is unknown,
// and always for MIXED, the buffer's TFF flag decides and its absence means
// bottom field first. A MIXED buffer is interlaced only if flagged so.
// FIELDS and ALTERNATE need field-specific planes this API cannot express.
fn frame_fields(
    mode: gst_video::VideoInterlaceMode,
    caps_order: gst_video::VideoFieldOrder,
    flags: gst_video::VideoBufferFlags,
) -> Result<(FrameInterlaceMode, FrameFieldOrder, bool, bool), Error> {
    use gst_video::{VideoBufferFlags as F, VideoFieldOrder as O, VideoInterlaceMode as M};
    let flagged_order = if flags.contains(F::TFF) {
        FrameFieldOrder::TopFirst
    } else {
        FrameFieldOrder::BottomFirst
    };
    let interlaced = match mode {
        M::Progressive => false,
        M::Interleaved => true,
        M::Mixed => flags.contains(F::INTERLACED),
        other => {
            return Err(Error::Decode(format!(
                "unsupported decoded interlace mode {other:?}"
            )));
        }
    };
    let rff = flags.contains(F::RFF);
    let field_order = match (mode, caps_order) {
        (M::Progressive, _) => FrameFieldOrder::Unknown,
        (M::Interleaved, O::TopFieldFirst) => FrameFieldOrder::TopFirst,
        (M::Interleaved, O::BottomFieldFirst) => FrameFieldOrder::BottomFirst,
        // A repeated field of a progressive MIXED frame still needs its parity.
        _ if interlaced || rff => flagged_order,
        _ => FrameFieldOrder::Unknown,
    };
    if !interlaced {
        return Ok((
            FrameInterlaceMode::Progressive,
            field_order,
            rff && mode == M::Mixed,
            false,
        ));
    }
    Ok((
        FrameInterlaceMode::Interleaved,
        field_order,
        rff,
        flags.contains(F::ONEFIELD),
    ))
}

// AUD (NAL type 9) must remain first in an H.264 access unit. Return the
// first NAL's payload length when it is an AUD; malformed first length
// prefixes fail before either PPS injection or queued state is consumed.
fn leading_avcc_aud(packet: &[u8], width: u8) -> Result<Option<usize>, Error> {
    let width = usize::from(width);
    if packet.len() < width {
        return Err(Error::InvalidInput("truncated AVC length prefix".into()));
    }
    let mut n = 0usize;
    for &b in &packet[..width] {
        n = (n << 8) | usize::from(b);
    }
    if n == 0 || n > packet.len() - width {
        return Err(Error::InvalidInput("invalid leading AVC NAL length".into()));
    }
    Ok((packet[width] & 31 == 9).then_some(n))
}

// Validate every NAL prefix and payload before anything reaches the
// asynchronous parser. The callback keeps ordinary AVCC zero-conversion and
// allocation-free; the Annex-B fallback reuses these exact checks.
fn each_avcc_nal(
    packet: &[u8],
    nal_length: u8,
    mut visit: impl FnMut(&[u8]) -> Result<(), Error>,
) -> Result<(), Error> {
    if !matches!(nal_length, 1 | 2 | 4) || packet.is_empty() {
        return Err(Error::InvalidInput(
            "invalid AVCC NAL prefix or empty sample".into(),
        ));
    }
    let width = usize::from(nal_length);
    let mut cursor = 0usize;
    while cursor < packet.len() {
        if packet.len() - cursor < width {
            return Err(Error::InvalidInput(
                "truncated AVC NAL-length prefix".into(),
            ));
        }
        let mut size = 0usize;
        for &byte in &packet[cursor..cursor + width] {
            size = (size << 8) | usize::from(byte);
        }
        cursor += width;
        if size == 0 || packet.len() - cursor < size || packet[cursor] & 0x80 != 0 {
            return Err(Error::InvalidInput(
                "invalid AVC NAL unit length or header".into(),
            ));
        }
        // H.264 reserves these types and assigns no defined payload format.
        // Other defined NALs (including extension types) remain the decoder's
        // responsibility; structural validation is not full bitstream parsing.
        if matches!(packet[cursor] & 31, 0 | 16..=18 | 22..=31) {
            return Err(Error::InvalidInput("undefined AVC NAL unit type".into()));
        }
        visit(&packet[cursor..cursor + size])?;
        cursor += size;
    }
    Ok(())
}

// Some GStreamer avcC parsers reject parameter sets exceeding 255 bytes even
// though the record encodes them with u16 lengths. Feed only those sessions
// through byte-stream caps; ordinary AVCC input remains zero-conversion.
// `inject` writes in-band parameter sets before the first NAL that is not
// an AUD, which must stay first in the access unit.
fn avcc_to_annexb(
    packet: &[u8],
    nal_length: u8,
    out: &mut Vec<u8>,
    inject: impl FnOnce(&mut Vec<u8>),
) -> Result<(), Error> {
    let mut inject = Some(inject);
    each_avcc_nal(packet, nal_length, |nal| {
        if nal[0] & 31 != 9
            && let Some(inject) = inject.take()
        {
            inject(out);
        }
        if out
            .len()
            .checked_add(4)
            .and_then(|len| len.checked_add(nal.len()))
            .is_none_or(|len| len as u64 > INPUT_QUEUE_LIMIT)
        {
            return Err(Error::InvalidInput(
                "expanded AVC packet exceeds queue limit".into(),
            ));
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        Ok(())
    })?;
    if let Some(inject) = inject {
        inject(out);
    }
    Ok(())
}

impl Decoder {
    /// Requires GStreamer's appsrc, h264parse, avdec_h264, videoconvert and
    /// appsink plugins at runtime. Never silently select another decoder or
    /// memory class; the returned planes are CPU-mappable NV12.
    pub fn h264_nv12(threads: u32) -> Result<Self, Error> {
        Self::h264_with_output(threads, PixelFormat::Nv12)
    }

    pub fn h264_with_output(threads: u32, format: PixelFormat) -> Result<Self, Error> {
        Self::with_h264_caps(
            threads,
            "video/x-h264,stream-format=(string)byte-stream,alignment=(string)au",
            format,
        )
    }

    /// Consume the guest's native AVCC access units without an intermediate
    /// Annex-B conversion. The SPS/PPS are packaged once into AVCDecoderConfigurationRecord.
    pub fn h264_avcc(sets: &[&[u8]], nal_length: u8, threads: u32) -> Result<Self, Error> {
        Self::h264_avcc_with_output(sets, nal_length, threads, PixelFormat::Nv12)
    }

    pub fn h264_avcc_with_output(
        sets: &[&[u8]],
        nal_length: u8,
        threads: u32,
        output_format: PixelFormat,
    ) -> Result<Self, Error> {
        let config = AvccConfiguration::new(sets, nal_length)?;
        let annexb_fallback = config
            .sps
            .iter()
            .chain(&config.pps)
            .any(|nal| nal.len() > 255);
        let caps = if annexb_fallback {
            "video/x-h264,stream-format=(string)byte-stream,alignment=(string)au".to_owned()
        } else {
            config.caps()
        };
        let mut decoder = Self::with_h264_caps(threads, &caps, output_format)?;
        decoder.avcc = Some(config);
        decoder.avcc_annexb_fallback = annexb_fallback;
        decoder.initial_annexb_sets_pending = annexb_fallback;
        Ok(decoder)
    }

    /// Update a compatible PPS in place without resetting the pipeline or
    /// losing decoder reference pictures. Changed SPS and NAL length are
    /// intentionally rejected until stream-compatibility is proven.
    pub fn reconfigure_avcc(&mut self, sets: &[&[u8]], nal_length: u8) -> Result<(), Error> {
        self.check_error()?;
        if self.completed {
            return Err(Error::EndOfStream);
        }
        let incoming = AvccConfiguration::new(sets, nal_length)?;
        let original = self
            .avcc
            .as_ref()
            .ok_or_else(|| Error::InvalidInput("not an AVCC session".into()))?;
        if original.nal_length != incoming.nal_length || original.sps != incoming.sps {
            return Err(Error::InvalidInput(
                "incompatible H.264 SPS or NAL-length update".into(),
            ));
        }
        // Changing appsrc codec_data caps inside a reference-dependent GOP
        // flushes gst-libav's H.264 reference pictures. Carry changed PPS NALs
        // in-band with the next submitted AU, preserving pipeline continuity.
        if incoming.pps != original.pps {
            let max_nal_len = (1_u64 << (u32::from(nal_length) * 8)) - 1;
            if !self.avcc_annexb_fallback
                && incoming
                    .pps
                    .iter()
                    .any(|pps| pps.len() as u64 > max_nal_len)
            {
                return Err(Error::InvalidInput(
                    "updated PPS exceeds sample NAL-length prefix capacity".into(),
                ));
            }
            let prefix_width = if self.avcc_annexb_fallback {
                4
            } else {
                nal_length as usize
            };
            let minimum_packet_bytes = prefix_width + 1;
            let pending_size = incoming
                .pps
                .iter()
                .try_fold(0usize, |sum, pps| {
                    sum.checked_add(prefix_width)?.checked_add(pps.len())
                })
                .filter(|count| *count <= INPUT_QUEUE_LIMIT as usize - minimum_packet_bytes)
                .ok_or_else(|| {
                    Error::InvalidInput("updated PPS aggregate exceeds input queue limit".into())
                })?;
            let mut pending = Vec::with_capacity(pending_size);
            for pps in &incoming.pps {
                if self.avcc_annexb_fallback {
                    pending.extend_from_slice(&[0, 0, 0, 1]);
                } else {
                    let len = (pps.len() as u32).to_be_bytes();
                    pending.extend_from_slice(&len[4 - nal_length as usize..]);
                }
                pending.extend_from_slice(pps);
            }
            self.pending_pps = Some(pending);
        }
        self.avcc = Some(incoming);
        self.check_error()
    }

    fn with_h264_caps(
        threads: u32,
        input_caps: &str,
        output_format: PixelFormat,
    ) -> Result<Self, Error> {
        gst::init().map_err(|e| Error::BackendUnavailable(e.to_string()))?;
        for name in [
            "appsrc",
            "h264parse",
            "avdec_h264",
            "videoconvert",
            "appsink",
        ] {
            if gst::ElementFactory::find(name).is_none() {
                return Err(Error::BackendUnavailable(format!(
                    "missing GStreamer plugin {name}"
                )));
            }
        }
        let output_caps = match output_format {
            PixelFormat::Nv12 => "NV12",
            PixelFormat::I420 => "I420",
            PixelFormat::Bgra => "BGRA",
        };
        // Explicitly parsed access units avoid shipping an entire demuxer.
        // GstReferenceTimestampMeta survives this tested software pipeline;
        // replacing any element requires re-running the identity regressions.
        let pipeline = gst::parse::launch(&format!(
            "appsrc name=rosemary_input format=time is-live=false block=false max-bytes={INPUT_QUEUE_LIMIT} \
             caps=\"{input_caps}\" \
             ! h264parse ! avdec_h264 name=rosemary_decoder max-threads={threads} output-corrupt=false discard-corrupted-frames=true \
             ! videoconvert ! video/x-raw,format=(string){output_caps} \
             ! appsink name=rosemary_output sync=false max-buffers={OUTPUT_QUEUE_LIMIT} drop=false"
        ))
        .map_err(|e| Error::BackendUnavailable(e.to_string()))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| Error::BackendUnavailable("invalid GStreamer pipeline".into()))?;
        let input = pipeline
            .by_name("rosemary_input")
            .and_then(|e| e.downcast::<AppSrc>().ok())
            .ok_or_else(|| Error::BackendUnavailable("GStreamer appsrc missing".into()))?;
        let output = pipeline
            .by_name("rosemary_output")
            .and_then(|e| e.downcast::<AppSink>().ok())
            .ok_or_else(|| Error::BackendUnavailable("GStreamer appsink missing".into()))?;
        let decoder = pipeline
            .by_name("rosemary_decoder")
            .and_then(|e| e.downcast::<gst_video::VideoDecoder>().ok())
            .ok_or_else(|| Error::BackendUnavailable("GStreamer video decoder missing".into()))?;
        let reference = gst::Caps::builder("application/x-rosemary-frame-id").build();
        let tracker = Arc::default();
        track_decoder(&decoder, &reference, &tracker)?;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| Error::BackendUnavailable(format!("GStreamer failed to start: {e:?}")))?;
        Ok(Self {
            pipeline,
            input,
            output,
            reference,
            tracker,
            completed: false,
            fatal_error: None,
            output_format,
            avcc: None,
            pending_pps: None,
            avcc_annexb_fallback: false,
            initial_annexb_sets_pending: false,
            source_pts_reference: gst::Caps::builder("application/x-rosemary-source-pts").build(),
            source_dts_reference: gst::Caps::builder("application/x-rosemary-source-dts").build(),
        })
    }

    fn latch_error(&mut self, message: String) -> Error {
        let first = self.fatal_error.get_or_insert(message);
        Error::Decode(first.clone())
    }

    fn check_error(&mut self) -> Result<(), Error> {
        if let Some(message) = &self.fatal_error {
            return Err(Error::Decode(message.clone()));
        }
        let message = self
            .pipeline
            .bus()
            .ok_or_else(|| Error::Decode("missing GStreamer bus".into()))?
            .pop_filtered(&[gst::MessageType::Error]);
        if let Some(msg) = message
            && let gst::MessageView::Error(e) = msg.view()
        {
            return Err(self.latch_error(format!("{}: {:?}", e.error(), e.debug())));
        }
        Ok(())
    }

    fn decode_sample(&self, id: u64, sample: gst::Sample) -> Result<DecodedFrame, Error> {
        let buffer = sample
            .buffer()
            .ok_or_else(|| Error::Decode("no output buffer".into()))?;
        let info = gst_video::VideoInfo::from_caps(
            sample
                .caps()
                .ok_or_else(|| Error::Decode("missing video caps".into()))?,
        )
        .map_err(|e| Error::Decode(format!("invalid video caps: {e}")))?;
        let mapped_format = match info.format() {
            gst_video::VideoFormat::Nv12 => PixelFormat::Nv12,
            gst_video::VideoFormat::I420 => PixelFormat::I420,
            gst_video::VideoFormat::Bgra => PixelFormat::Bgra,
            _ => {
                return Err(Error::Decode(format!(
                    "unexpected output format {:?}",
                    info.format()
                )));
            }
        };
        if mapped_format != self.output_format {
            return Err(Error::Decode(
                "decoder negotiated an unrequested pixel format".into(),
            ));
        }
        let par = info.par();
        let pixel_aspect_ratio = match (par.numer(), par.denom()) {
            (n, d) if n > 0 && d > 0 => (n as u32, d as u32),
            _ => return Err(Error::Decode("invalid pixel aspect ratio".into())),
        };
        let (interlace_mode, field_order, repeat_first_field, one_field) = frame_fields(
            info.interlace_mode(),
            info.field_order(),
            buffer.video_flags(),
        )?;
        let color = info.colorimetry();
        let matrix = color_matrix(color.matrix());
        let layout = FrameLayout {
            width: info.width() as usize,
            height: info.height() as usize,
            format: mapped_format,
            strides: info
                .stride()
                .iter()
                .map(|n| usize::try_from(*n).unwrap_or(0))
                .collect(),
            offsets: info.offset().to_vec(),
            crop: buffer.meta::<gst_video::VideoCropMeta>().map(|meta| {
                let (x, y, width, height) = meta.rect();
                FrameCrop {
                    x: x as usize,
                    y: y as usize,
                    width: width as usize,
                    height: height as usize,
                }
            }),
            pixel_aspect_ratio,
            chroma_siting: chroma_siting(info.chroma_site()),
            interlace_mode,
            field_order,
            repeat_first_field,
            one_field,
            matrix,
            primaries: color_primaries(color.primaries()),
            transfer: color_transfer(color.transfer()),
            full_range: match color.range() {
                gst_video::VideoColorRange::Range0_255 => Some(true),
                gst_video::VideoColorRange::Range16_235 => Some(false),
                _ => None,
            },
        };
        // GstVideoMeta, when supplied by a decoder pool, describes physical
        // buffer plane offsets/strides rather than the negotiated default.
        let mut layout = layout;
        if let Some(meta) = buffer.meta::<gst_video::VideoMeta>() {
            if meta.width() != info.width()
                || meta.height() != info.height()
                || meta.format() != info.format()
                || meta.n_planes() as usize != layout.strides.len()
            {
                return Err(Error::Decode(
                    "incompatible GstVideoMeta plane layout".into(),
                ));
            }
            layout.offsets = meta.offset().to_vec();
            layout.strides = meta
                .stride()
                .iter()
                .map(|n| usize::try_from(*n).unwrap_or(0))
                .collect();
        }
        validate_planes(&layout, buffer.size())?;
        let duration_ns = buffer.duration().map(|time| time.nseconds());
        let source_pts_ns = buffer
            .iter_meta::<gst::ReferenceTimestampMeta>()
            .find(|meta| meta.reference() == self.source_pts_reference.as_ref())
            .map(|meta| original_media_time(meta.timestamp()))
            .transpose()?;
        let source_dts_ns = buffer
            .iter_meta::<gst::ReferenceTimestampMeta>()
            .find(|meta| meta.reference() == self.source_dts_reference.as_ref())
            .map(|meta| original_media_time(meta.timestamp()))
            .transpose()?;
        // A decoder may synthesize PTS/DTS starting at GstClockTime zero
        // for entirely untimestamped input. Such values were never shifted
        // onto our signed media timeline; undoing the epoch would fabricate
        // timestamps far in the past. Preserve absence instead. A genuine
        // input timestamp of -2^62 maps to zero, so numeric threshold checks
        // on the GStreamer clock would be incorrect.
        let (pts_ns, dts_ns) = if source_pts_ns.is_some() || source_dts_ns.is_some() {
            (
                buffer.pts().map(original_media_time).transpose()?,
                buffer.dts().map(original_media_time).transpose()?,
            )
        } else {
            (None, None)
        };
        Ok(DecodedFrame {
            id,
            sample,
            layout,
            pts_ns,
            dts_ns,
            duration_ns,
            source_pts_ns,
            source_dts_ns,
        })
    }

    // A frame without an id cannot be matched to its submission, so the
    // one-result-per-sample contract is broken for good. Anything else wrong
    // with an identified frame fails only that frame.
    fn decode_checked_sample(
        &mut self,
        sample: gst::Sample,
    ) -> Result<Option<DecodedFrame>, Error> {
        // Do NOT key callbacks on output PTS: h264parse/avdec_h264 can
        // legitimately output multiple frames with the same timestamp.
        let Some(id) = sample.buffer().and_then(|b| sample_id(b, &self.reference)) else {
            return Err(self.latch_error("H.264 output lost its sample identity metadata".into()));
        };
        match self.decode_sample(id, sample) {
            Ok(frame) => Ok(Some(frame)),
            Err(error) => {
                lock_tracker(&self.tracker)
                    .dropped
                    .push_back(DroppedSample {
                        id,
                        error: Some(error),
                    });
                Ok(None)
            }
        }
    }
}

impl VideoDecoder for Decoder {
    type Frame = DecodedFrame;

    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }

    fn submit(&mut self, packet: AccessUnit<'_>) -> Result<(), Error> {
        self.check_error()?;
        if self.completed {
            return Err(Error::EndOfStream);
        }
        if packet.bytes.len() as u64 > INPUT_QUEUE_LIMIT {
            return Err(Error::InvalidInput(
                "compressed access unit exceeds queue limit".into(),
            ));
        }
        // Preflight the complete input and its expanded byte count before
        // allocating a converted AU or copying (possibly large) parameter
        // sets. Even with a full output queue, malformed input must fail as
        // InvalidInput rather than Backpressure.
        let total_len = if let Some(config) = &self.avcc {
            if self.avcc_annexb_fallback {
                let mut expanded = 0usize;
                each_avcc_nal(packet.bytes, config.nal_length, |nal| {
                    expanded = expanded
                        .checked_add(4)
                        .and_then(|n| n.checked_add(nal.len()))
                        .filter(|&n| n as u64 <= INPUT_QUEUE_LIMIT)
                        .ok_or_else(|| {
                            Error::InvalidInput("expanded AVC packet exceeds queue limit".into())
                        })?;
                    Ok(())
                })?;
                let injected = if self.initial_annexb_sets_pending {
                    config
                        .sps
                        .iter()
                        .chain(&config.pps)
                        .try_fold(0usize, |sum, nal| {
                            sum.checked_add(4).and_then(|n| n.checked_add(nal.len()))
                        })
                } else {
                    Some(self.pending_pps.as_ref().map_or(0, Vec::len))
                };
                expanded.checked_add(injected.ok_or_else(|| {
                    Error::InvalidInput("AVC parameter sets exceed queue limit".into())
                })?)
            } else {
                each_avcc_nal(packet.bytes, config.nal_length, |_| Ok(()))?;
                packet
                    .bytes
                    .len()
                    .checked_add(self.pending_pps.as_ref().map_or(0, Vec::len))
            }
        } else {
            Some(packet.bytes.len())
        };
        if packet.id == u64::MAX
            || packet.duration_ns == Some(u64::MAX)
            || packet.bytes.is_empty()
            || total_len.is_none_or(|len| len as u64 > INPUT_QUEUE_LIMIT)
        {
            return Err(Error::InvalidInput(
                "invalid packet identifier or length".into(),
            ));
        }
        let pts = packet.pts_ns.map(gst_media_time).transpose()?;
        let dts = packet.dts_ns.map(gst_media_time).transpose()?;
        // GStreamer is asynchronous. Never block the guest's decode thread
        // while an appsink queue could itself be awaiting that same thread.
        // Return a recoverable signal without consuming the input instead.
        if self.output.current_level_buffers() >= OUTPUT_QUEUE_LIMIT
            || self
                .input
                .current_level_bytes()
                .saturating_add(total_len.unwrap() as u64)
                > INPUT_QUEUE_LIMIT
        {
            return Err(Error::Backpressure);
        }
        // The caller owns input access units. GStreamer receives its own
        // buffer only after structural, size, time and backpressure checks.
        let compressed = if self.avcc_annexb_fallback {
            let config = self
                .avcc
                .as_ref()
                .expect("AVCC fallback without configuration");
            let (initial_sets, pending_pps) = (self.initial_annexb_sets_pending, &self.pending_pps);
            let mut converted = Vec::with_capacity(total_len.unwrap());
            avcc_to_annexb(packet.bytes, config.nal_length, &mut converted, |out| {
                if initial_sets {
                    for nal in config.sps.iter().chain(&config.pps) {
                        out.extend_from_slice(&[0, 0, 0, 1]);
                        out.extend_from_slice(nal);
                    }
                } else if let Some(pps) = pending_pps {
                    out.extend_from_slice(pps);
                }
            })?;
            converted
        } else {
            let mut bytes = Vec::with_capacity(total_len.unwrap());
            if let Some(pps) = &self.pending_pps {
                let split = leading_avcc_aud(packet.bytes, self.avcc.as_ref().unwrap().nal_length)?
                    .map_or(0, |n| n + self.avcc.as_ref().unwrap().nal_length as usize);
                bytes.extend_from_slice(&packet.bytes[..split]);
                bytes.extend_from_slice(pps);
                bytes.extend_from_slice(&packet.bytes[split..]);
            } else {
                bytes.extend_from_slice(packet.bytes);
            }
            bytes
        };
        let mut buffer = gst::Buffer::from_mut_slice(compressed);
        let writable = buffer
            .get_mut()
            .ok_or_else(|| Error::Decode("GstBuffer is not writable".into()))?;
        writable.set_pts(pts);
        writable.set_dts(dts);
        writable.set_duration(packet.duration_ns.map(gst::ClockTime::from_nseconds));
        gst::ReferenceTimestampMeta::add(
            writable,
            &self.reference,
            gst::ClockTime::from_nseconds(packet.id),
            None,
        );
        if let Some(pts) = pts {
            gst::ReferenceTimestampMeta::add(writable, &self.source_pts_reference, pts, None);
        }
        if let Some(dts) = dts {
            gst::ReferenceTimestampMeta::add(writable, &self.source_dts_reference, dts, None);
        }
        // Record the id first: the streaming thread may reach the decoder
        // before push_buffer returns.
        lock_tracker(&self.tracker).upstream.push_back(packet.id);
        if let Err(e) = self.input.push_buffer(buffer) {
            lock_tracker(&self.tracker).upstream.pop_back();
            return Err(self.latch_error(format!("cannot submit H.264 access unit: {e:?}")));
        }
        self.pending_pps = None;
        self.initial_annexb_sets_pending = false;
        self.check_error()
    }

    fn pull(&mut self, timeout: Duration) -> Result<Option<Self::Frame>, Error> {
        self.check_error()?;
        // Zero-duration polling is valid: a decoder may still be holding
        // reference pictures and the caller must supply future access units.
        let nanos = u64::try_from(timeout.as_nanos().min((u64::MAX - 1) as u128)).unwrap();
        if let Some(sample) = self
            .output
            .try_pull_sample(gst::ClockTime::from_nseconds(nanos))
        {
            self.check_error()?;
            return self.decode_checked_sample(sample);
        }
        self.check_error()?;
        Ok(None)
    }

    fn take_dropped(&mut self) -> Option<DroppedSample> {
        lock_tracker(&self.tracker).dropped.pop_front()
    }

    fn end_stream(&mut self) -> Result<(), Error> {
        self.check_error()?;
        if !self.completed {
            if let Err(e) = self.input.end_of_stream() {
                return Err(self.latch_error(format!("GStreamer EOS: {e:?}")));
            }
            self.completed = true;
        }
        self.check_error()
    }

    fn is_drained(&mut self) -> Result<bool, Error> {
        self.check_error()?;
        Ok(self.completed && self.output.is_eos())
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each Annex-B NAL as its start-code offset and payload, recognizing
    /// both 3- and 4-byte start codes.
    fn annexb_nals(stream: &[u8]) -> Vec<(usize, &[u8])> {
        let mut starts = Vec::new();
        let mut cursor = 0;
        while cursor + 3 <= stream.len() {
            let prefix = if stream[cursor..].starts_with(&[0, 0, 0, 1]) {
                4
            } else if stream[cursor..].starts_with(&[0, 0, 1]) {
                3
            } else {
                cursor += 1;
                continue;
            };
            starts.push((cursor, prefix));
            cursor += prefix;
        }
        starts
            .iter()
            .enumerate()
            .map(|(i, &(start, prefix))| {
                let end = starts.get(i + 1).map_or(stream.len(), |&(next, _)| next);
                (start, &stream[start + prefix..end])
            })
            .filter(|(_, nal)| !nal.is_empty())
            .collect()
    }

    /// Access units of an Annex-B stream, each starting at an AUD (or at the
    /// first NAL when the stream does not open with one).
    fn access_units(stream: &[u8]) -> Vec<&[u8]> {
        let starts = annexb_nals(stream)
            .into_iter()
            .enumerate()
            .filter(|(i, (_, nal))| *i == 0 || nal[0] & 31 == 9)
            .map(|(_, (start, _))| start)
            .collect::<Vec<_>>();
        starts
            .iter()
            .enumerate()
            .map(|(i, &start)| &stream[start..starts.get(i + 1).copied().unwrap_or(stream.len())])
            .collect()
    }

    fn fixture_access_units() -> Vec<&'static [u8]> {
        access_units(include_bytes!("../tests/fixtures/bframes_8_64x64.h264"))
    }

    #[test]
    fn multi_gop_reordering_matches_ffmpeg_presentation_pixels() {
        let stream = include_bytes!("../tests/fixtures/multi_gop_bframes_64x64.h264");
        let expected = include_str!("../tests/fixtures/multi_gop_bframes_64x64.luma_fnv64");
        let units = access_units(stream);
        assert_eq!(units.len(), 60);
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        for (i, bytes) in units.iter().enumerate() {
            decoder
                .submit(AccessUnit {
                    id: (i + 100) as u64,
                    bytes,
                    pts_ns: Some(i as i64 * 33_333_333),
                    dts_ns: None,
                    duration_ns: Some(33_333_333),
                })
                .unwrap();
        }
        decoder.end_stream().unwrap();
        let mut identities = Vec::new();
        let mut checksums = Vec::new();
        for _ in 0..units.len() {
            let frame = decoder
                .pull(Duration::from_secs(3))
                .unwrap()
                .expect("missing multi-GOP frame");
            identities.push(frame.id());
            let hash = frame
                .with_bytes(|mapped, info| {
                    assert_eq!((info.width, info.height), (64, 64));
                    let mut hash = 0xcbf29ce484222325_u64;
                    for row in 0..64 {
                        for &value in &mapped[info.offsets[0] + row * info.strides[0]
                            ..info.offsets[0] + row * info.strides[0] + 64]
                        {
                            hash ^= u64::from(value);
                            hash = hash.wrapping_mul(0x100000001b3);
                        }
                    }
                    hash
                })
                .unwrap();
            checksums.push(format!("{hash:016x}"));
        }
        // Match source AU identities to independently pinned presentation-
        // order luma hashes, not merely to an unordered set of frame IDs.
        const EXPECTED_PRESENTATION_IDS: [u64; 60] = [
            100, 102, 103, 101, 105, 106, 104, 108, 109, 107, 111, 112, 110, 114, 115, 113, 117,
            118, 116, 119, 120, 122, 123, 121, 124, 126, 125, 128, 127, 129, 131, 130, 132, 134,
            133, 136, 135, 138, 139, 137, 140, 142, 143, 141, 145, 144, 147, 146, 149, 148, 150,
            152, 151, 153, 154, 155, 157, 156, 158, 159,
        ];
        assert_eq!(identities, EXPECTED_PRESENTATION_IDS);

        let golden = expected.lines().collect::<Vec<_>>();
        assert_eq!(checksums.len(), golden.len());
        for (i, hash) in checksums.iter().enumerate() {
            assert_eq!(
                hash, golden[i],
                "incorrect decoded picture at presentation frame {i}"
            );
        }
    }

    #[test]
    fn missing_initial_idr_does_not_emit_pre_recovery_pictures() {
        // The first IDR slice was removed from the same pinned multi-GOP
        // fixture. A decoder must recover at the later IDR, not present
        // pre-recovery pictures as valid output.
        let stream = include_bytes!("../tests/fixtures/multi_gop_missing_first_idr_64x64.h264");
        let golden = include_str!("../tests/fixtures/multi_gop_bframes_64x64.luma_fnv64");
        let units = access_units(stream);
        assert_eq!(units.len(), 60);
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        for (index, unit) in units.iter().enumerate() {
            decoder
                .submit(AccessUnit {
                    id: index as u64,
                    bytes: unit,
                    pts_ns: Some(index as i64 * 33_333_333),
                    dts_ns: None,
                    duration_ns: Some(33_333_333),
                })
                .unwrap();
        }
        decoder.end_stream().unwrap();
        let mut ids = Vec::new();
        let mut pixels = Vec::new();
        for _ in 0..60 {
            let Some(frame) = decoder.pull(Duration::from_secs(2)).unwrap() else {
                break;
            };
            ids.push(frame.id());
            let hash = frame
                .with_bytes(|mapped, info| {
                    let mut hash = 0xcbf29ce484222325_u64;
                    for row in 0..64 {
                        let begin = info.offsets[0] + row * info.strides[0];
                        for &pixel in &mapped[begin..begin + 64] {
                            hash = (hash ^ u64::from(pixel)).wrapping_mul(0x100000001b3);
                        }
                    }
                    format!("{hash:016x}")
                })
                .unwrap();
            pixels.push(hash);
        }
        // FFmpeg's normal public output emits only the 40 pictures after the
        // second IDR in this fixture. The public draw_horiz_band hook, by
        // contrast, fires for invalid pre-roll and cannot establish eligibility.
        assert_eq!(ids.len(), 40);
        assert!(
            ids.iter().all(|&id| id >= 20),
            "pre-recovery picture escaped"
        );
        assert_eq!(pixels, golden.lines().skip(20).collect::<Vec<_>>());
        // Every discarded pre-recovery sample is reported, so a caller
        // waiting for one result per submission is not left hanging.
        wait_drained(&mut decoder);
        let mut dropped = Vec::new();
        while let Some(sample) = decoder.take_dropped() {
            assert!(sample.error.is_none(), "{:?}", sample.error);
            dropped.push(sample.id);
        }
        dropped.sort_unstable();
        assert_eq!(dropped, (0..20).collect::<Vec<_>>());
    }

    fn wait_drained(decoder: &mut Decoder) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !decoder.is_drained().unwrap() {
            assert!(
                std::time::Instant::now() < deadline,
                "terminal EOS never arrived"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn decoded_samples_are_never_reported_dropped() {
        let frames = fixture_access_units();
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        for (id, bytes) in frames.iter().enumerate() {
            decoder
                .submit(AccessUnit {
                    id: id as u64,
                    bytes,
                    pts_ns: Some(id as i64 * 33_333_333),
                    dts_ns: None,
                    duration_ns: Some(33_333_333),
                })
                .unwrap();
        }
        decoder.end_stream().unwrap();
        for _ in 0..frames.len() {
            decoder.pull(Duration::from_secs(3)).unwrap().unwrap();
        }
        wait_drained(&mut decoder);
        assert!(decoder.take_dropped().is_none());
    }

    #[test]
    fn single_idr_can_complete_without_terminal_eos() {
        let bytes = include_bytes!("../tests/fixtures/red_32x32.h264");
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        decoder
            .submit(AccessUnit {
                id: 123,
                bytes,
                pts_ns: Some(0),
                dts_ns: None,
                duration_ns: Some(33_333_333),
            })
            .unwrap();
        let immediate = decoder.pull(Duration::from_millis(300)).unwrap();
        assert!(
            immediate.is_some(),
            "Even all-IDR decoder requires a later frame or terminal EOS before emitting"
        );
        assert_eq!(immediate.unwrap().id(), 123);
    }

    #[test]
    fn first_fatal_bus_error_is_latched_across_decoder_operations() {
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        let bus = decoder.pipeline.bus().unwrap();
        bus.post(
            gst::message::Error::builder(gst::CoreError::Failed, "injected first fatal")
                .src(&decoder.pipeline)
                .build(),
        )
        .unwrap();
        let first = match decoder.pull(Duration::ZERO) {
            Err(Error::Decode(message)) => message,
            _ => panic!("fatal bus error was not reported"),
        };
        assert!(first.contains("injected first fatal"), "{first}");

        bus.post(
            gst::message::Error::builder(gst::CoreError::Failed, "second fatal")
                .src(&decoder.pipeline)
                .build(),
        )
        .unwrap();
        let sample = include_bytes!("../tests/fixtures/red_32x32.h264");
        let packet = AccessUnit {
            id: 1,
            bytes: sample,
            pts_ns: None,
            dts_ns: None,
            duration_ns: None,
        };
        for result in [
            decoder.submit(packet),
            decoder.end_stream(),
            decoder.pull(Duration::ZERO).map(|_| ()),
        ] {
            match result {
                Err(Error::Decode(message)) => assert_eq!(message, first),
                _ => panic!("decoder operation did not retain first fatal error"),
            }
        }
        assert!(matches!(decoder.is_drained(), Err(Error::Decode(_))));
    }

    #[test]
    fn missing_decoded_sample_identity_is_a_sticky_fatal_error() {
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        let buffer = gst::Buffer::from_mut_slice(vec![0]);
        let sample = gst::Sample::builder().buffer(&buffer).build();
        let first = match decoder.decode_checked_sample(sample) {
            Err(Error::Decode(message)) => message,
            _ => panic!("missing identity was not rejected"),
        };
        assert!(first.contains("lost its sample identity"));
        match decoder.submit(AccessUnit {
            id: 9,
            bytes: include_bytes!("../tests/fixtures/red_32x32.h264"),
            pts_ns: None,
            dts_ns: None,
            duration_ns: None,
        }) {
            Err(Error::Decode(message)) => assert_eq!(message, first),
            _ => panic!("decoder accepted input after losing frame identity"),
        }
    }

    #[test]
    fn unusable_identified_frame_is_dropped_without_poisoning_the_stream() {
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        // An identified frame whose caps are missing cannot be described.
        let mut buffer = gst::Buffer::from_mut_slice(vec![0]);
        gst::ReferenceTimestampMeta::add(
            buffer.get_mut().unwrap(),
            &decoder.reference,
            gst::ClockTime::from_nseconds(41),
            None,
        );
        let sample = gst::Sample::builder().buffer(&buffer).build();
        assert!(decoder.decode_checked_sample(sample).unwrap().is_none());
        let dropped = decoder.take_dropped().unwrap();
        assert_eq!(dropped.id, 41);
        assert!(matches!(dropped.error, Some(Error::Decode(_))));
        assert!(decoder.fatal_error.is_none());
        decoder
            .submit(AccessUnit {
                id: 42,
                bytes: include_bytes!("../tests/fixtures/red_32x32.h264"),
                pts_ns: Some(0),
                dts_ns: None,
                duration_ns: None,
            })
            .unwrap();
        assert_eq!(
            decoder.pull(Duration::from_secs(2)).unwrap().unwrap().id(),
            42
        );
    }

    #[test]
    fn truncated_h264_access_unit_never_fabricates_a_decoded_frame() {
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        // One incomplete IDR NAL header without a decodable slice body.
        let invalid = [0, 0, 0, 1, 0x65, 0xff];
        let submission = decoder.submit(AccessUnit {
            id: 33,
            bytes: &invalid,
            pts_ns: Some(0),
            dts_ns: None,
            duration_ns: None,
        });
        assert!(submission.is_ok() || matches!(submission, Err(Error::Decode(_))));
        // A parser may reject the sample asynchronously, or discard it.
        // Neither path may present an invented frame as decoded output.
        let _ = decoder.end_stream();
        assert!(!matches!(
            decoder.pull(Duration::from_millis(500)),
            Ok(Some(_))
        ));
    }

    #[test]
    fn untimestamped_input_never_exposes_unanchored_generated_time() {
        let bytes = include_bytes!("../tests/fixtures/red_32x32.h264");
        for duration_ns in [Some(33_333_333), None] {
            let mut decoder = Decoder::h264_nv12(1).unwrap();
            for index in 0..3_u64 {
                decoder
                    .submit(AccessUnit {
                        id: 310 + index,
                        bytes,
                        pts_ns: None,
                        dts_ns: None,
                        duration_ns,
                    })
                    .unwrap();
            }
            decoder.end_stream().unwrap();
            for index in 0..3_u64 {
                let frame = decoder
                    .pull(Duration::from_secs(3))
                    .unwrap()
                    .expect("missing untimestamped decoded picture");
                assert_eq!(frame.id(), 310 + index);
                assert_eq!(frame.source_pts_ns(), None);
                assert_eq!(frame.source_dts_ns(), None);
                assert_eq!(
                    frame.pts_ns(),
                    None,
                    "unanchored output PTS was epoch-decoded"
                );
                assert_eq!(
                    frame.dts_ns(),
                    None,
                    "unanchored output DTS was epoch-decoded"
                );
            }
            assert!(decoder.pull(Duration::from_millis(100)).unwrap().is_none());
        }
    }

    #[test]
    fn timestamp_provenance_is_per_frame_in_mixed_input() {
        let bytes = include_bytes!("../tests/fixtures/red_32x32.h264");
        let input_pts = [Some(-33_333_333), None, Some(33_333_333)];
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        for (index, &pts_ns) in input_pts.iter().enumerate() {
            decoder
                .submit(AccessUnit {
                    id: 350 + index as u64,
                    bytes,
                    pts_ns,
                    dts_ns: None,
                    duration_ns: Some(33_333_333),
                })
                .unwrap();
        }
        decoder.end_stream().unwrap();
        for (index, expected_pts) in input_pts.into_iter().enumerate() {
            let frame = decoder.pull(Duration::from_secs(3)).unwrap().unwrap();
            assert_eq!(frame.id(), 350 + index as u64);
            assert_eq!(frame.source_pts_ns(), expected_pts);
            assert_eq!(frame.source_dts_ns(), None);
            if expected_pts.is_none() {
                assert_eq!(frame.pts_ns(), None);
                assert_eq!(frame.dts_ns(), None);
            } else {
                assert!(frame.pts_ns().is_some(), "known signed PTS was lost");
            }
        }
    }

    #[test]
    fn signed_timestamp_epoch_boundary_roundtrips_through_decode() {
        const EARLIEST: i64 = -(1_i64 << 62);
        assert_eq!(gst_media_time(EARLIEST).unwrap().nseconds(), 0);
        assert_eq!(
            original_media_time(gst_media_time(EARLIEST).unwrap()).unwrap(),
            EARLIEST
        );
        assert!(matches!(
            gst_media_time(EARLIEST - 1),
            Err(Error::InvalidInput(_))
        ));
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        decoder
            .submit(AccessUnit {
                id: 400,
                bytes: include_bytes!("../tests/fixtures/red_32x32.h264"),
                pts_ns: Some(EARLIEST),
                dts_ns: Some(EARLIEST),
                duration_ns: Some(33_333_333),
            })
            .unwrap();
        decoder.end_stream().unwrap();
        let frame = decoder.pull(Duration::from_secs(3)).unwrap().unwrap();
        assert_eq!(frame.id(), 400);
        assert_eq!(frame.source_pts_ns(), Some(EARLIEST));
        assert_eq!(frame.source_dts_ns(), Some(EARLIEST));
        assert_eq!(frame.pts_ns(), Some(EARLIEST));
    }

    #[test]
    fn decoded_frame_exposes_output_timing() {
        let bytes = include_bytes!("../tests/fixtures/red_32x32.h264");
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        decoder
            .submit(AccessUnit {
                id: 71,
                bytes,
                pts_ns: Some(500_000_000),
                dts_ns: Some(500_000_000),
                duration_ns: Some(33_333_333),
            })
            .unwrap();
        let frame = decoder.pull(Duration::from_secs(2)).unwrap().unwrap();
        assert_eq!(frame.id(), 71);
        assert_eq!(frame.pts_ns(), Some(500_000_000));
        assert_eq!(frame.duration_ns(), Some(33_333_333));
        // The H.264 parser/decoder may discard DTS on reordered output.
        // Expose the actual decoded-buffer value rather than copying input DTS.
        assert_eq!(frame.dts_ns(), None);
    }

    #[test]
    fn overlarge_access_unit_is_rejected_without_poisoning_the_stream() {
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        let too_large = vec![0x7f; INPUT_QUEUE_LIMIT as usize + 1];
        assert!(matches!(
            decoder.submit(AccessUnit {
                id: 1,
                bytes: &too_large,
                pts_ns: None,
                dts_ns: None,
                duration_ns: None,
            }),
            Err(Error::InvalidInput(_))
        ));
        decoder
            .submit(AccessUnit {
                id: 2,
                bytes: include_bytes!("../tests/fixtures/red_32x32.h264"),
                pts_ns: Some(0),
                dts_ns: None,
                duration_ns: None,
            })
            .unwrap();
        assert_eq!(
            decoder.pull(Duration::from_secs(2)).unwrap().unwrap().id(),
            2
        );
    }

    #[test]
    fn requested_nv12_i420_and_bgra_outputs_are_mappable() {
        let data = include_bytes!("../tests/fixtures/red_32x32.h264");
        for format in [PixelFormat::Nv12, PixelFormat::I420, PixelFormat::Bgra] {
            let mut decoder = Decoder::h264_with_output(1, format).unwrap();
            decoder
                .submit(AccessUnit {
                    id: 17,
                    bytes: data,
                    pts_ns: Some(0),
                    dts_ns: Some(0),
                    duration_ns: Some(33_333_333),
                })
                .unwrap();
            decoder.end_stream().unwrap();
            let frame = decoder.pull(Duration::from_secs(2)).unwrap().unwrap();
            assert_eq!((frame.layout().width, frame.layout().height), (32, 32));
            assert_eq!(frame.layout().format, format);
            assert_eq!(frame.id(), 17);
            frame
                .with_bytes(|bytes, info| {
                    validate_planes(info, bytes.len()).unwrap();
                    for plane in 0..info.offsets.len() {
                        let span = info.plane_byte_span(plane).unwrap();
                        assert!(span > 0);
                        assert!(info.offsets[plane] + span <= bytes.len());
                    }
                    assert_eq!(
                        info.offsets.len(),
                        match format {
                            PixelFormat::Nv12 => 2,
                            PixelFormat::I420 => 3,
                            PixelFormat::Bgra => 1,
                        }
                    );
                })
                .unwrap();
            // The sample remains valid independently of the pipeline lifetime.
            drop(decoder);
            frame
                .with_bytes(|bytes, info| assert!(bytes.len() > info.offsets[0]))
                .unwrap();
        }
    }

    #[test]
    fn queue_backpressure_does_not_consume_input_or_lose_frames() {
        let bytes = include_bytes!("../tests/fixtures/red_32x32.h264");
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        let total = 256_u64;
        let mut received = Vec::new();
        let mut blocked_count = 0;
        let mut produced = 0;
        let mut exercised_pause = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while produced < total {
            assert!(
                std::time::Instant::now() < deadline,
                "GStreamer producer stalled"
            );
            match decoder.submit(AccessUnit {
                id: produced,
                bytes,
                pts_ns: Some(produced as i64 * 33_333_333),
                dts_ns: None,
                duration_ns: Some(33_333_333),
            }) {
                Ok(()) => produced += 1,
                Err(Error::Backpressure) => {
                    blocked_count += 1;
                    if let Some(frame) = decoder.pull(Duration::from_millis(100)).unwrap() {
                        received.push(frame.id());
                    }
                }
                Err(e) => panic!("unexpected producer error at {produced}: {e}"),
            }
            if produced == 64 && !exercised_pause {
                exercised_pause = true;
                // The source has a finite appsink queue. Let the decoder
                // reach that limit while no consumer is pulling frames.
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        decoder.end_stream().unwrap();
        while received.len() < total as usize {
            assert!(
                std::time::Instant::now() < deadline,
                "GStreamer consumer stalled"
            );
            if let Some(frame) = decoder.pull(Duration::from_millis(100)).unwrap() {
                received.push(frame.id());
            }
        }
        assert!(
            blocked_count > 0,
            "output queue did not exercise backpressure"
        );
        received.sort_unstable();
        assert_eq!(received, (0..total).collect::<Vec<_>>());
    }

    #[test]
    fn anamorphic_h264_preserves_pixel_aspect_ratio() {
        let data = include_bytes!("../tests/fixtures/anamorphic_4_64x64.h264");
        let units = access_units(data);
        assert_eq!(units.len(), 4);
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        for (index, bytes) in units.iter().enumerate() {
            decoder
                .submit(AccessUnit {
                    id: index as u64,
                    bytes,
                    pts_ns: Some(index as i64 * 100_000_000),
                    dts_ns: None,
                    duration_ns: Some(100_000_000),
                })
                .unwrap();
        }
        decoder.end_stream().unwrap();
        for index in 0..4 {
            let frame = decoder.pull(Duration::from_secs(2)).unwrap().unwrap();
            assert_eq!(frame.id(), index);
            assert_eq!(frame.layout().pixel_aspect_ratio, (4, 3));
            assert_eq!(
                frame.layout().interlace_mode,
                FrameInterlaceMode::Progressive
            );
        }
    }

    #[test]
    fn interlaced_h264_preserves_field_order_and_mode() {
        let data = include_bytes!("../tests/fixtures/interlaced_tff_4_64x64.h264");
        let units = access_units(data);
        assert_eq!(units.len(), 4);
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        for (index, bytes) in units.iter().enumerate() {
            decoder
                .submit(AccessUnit {
                    id: index as u64,
                    bytes,
                    pts_ns: Some(index as i64 * 100_000_000),
                    dts_ns: None,
                    duration_ns: Some(100_000_000),
                })
                .unwrap();
        }
        decoder.end_stream().unwrap();
        for index in 0..4 {
            let frame = decoder.pull(Duration::from_secs(2)).unwrap().unwrap();
            assert_eq!(frame.id(), index);
            assert!(
                frame
                    .sample
                    .buffer()
                    .unwrap()
                    .video_flags()
                    .contains(gst_video::VideoBufferFlags::INTERLACED)
            );
            assert!(
                !frame
                    .sample
                    .buffer()
                    .unwrap()
                    .video_flags()
                    .contains(gst_video::VideoBufferFlags::TFF)
            );
            assert_eq!(frame.layout().pixel_aspect_ratio, (1, 1));
            assert_eq!(
                frame.layout().interlace_mode,
                FrameInterlaceMode::Interleaved
            );
            // Despite the file name, the caps say bottom-field-first and
            // FFprobe's per-frame top_field_first=0 agrees; only FFprobe's
            // stream-level field_order=tt says otherwise.
            assert_eq!(frame.layout().field_order, FrameFieldOrder::BottomFirst);
            assert!(!frame.layout().one_field);
            assert!(!frame.layout().repeat_first_field);
        }
    }

    #[test]
    fn interlace_flags_are_per_buffer_and_unsupported_modes_are_rejected() {
        use gst_video::{VideoBufferFlags as F, VideoFieldOrder as O, VideoInterlaceMode as M};
        use {FrameFieldOrder as Order, FrameInterlaceMode as Mode};
        for (mode, caps_order, flags, expected) in [
            (
                M::Progressive,
                O::Unknown,
                F::empty(),
                (Mode::Progressive, Order::Unknown, false, false),
            ),
            (
                M::Progressive,
                O::Unknown,
                F::RFF,
                (Mode::Progressive, Order::Unknown, false, false),
            ),
            (
                M::Interleaved,
                O::Unknown,
                F::INTERLACED | F::TFF | F::RFF,
                (Mode::Interleaved, Order::TopFirst, true, false),
            ),
            (
                M::Interleaved,
                O::Unknown,
                F::INTERLACED | F::ONEFIELD,
                (Mode::Interleaved, Order::BottomFirst, false, true),
            ),
            (
                M::Interleaved,
                O::Unknown,
                F::INTERLACED | F::TFF | F::ONEFIELD,
                (Mode::Interleaved, Order::TopFirst, false, true),
            ),
            // A stream-wide caps field order applies even without buffer flags.
            (
                M::Interleaved,
                O::TopFieldFirst,
                F::empty(),
                (Mode::Interleaved, Order::TopFirst, false, false),
            ),
            (
                M::Interleaved,
                O::BottomFieldFirst,
                F::TFF,
                (Mode::Interleaved, Order::BottomFirst, false, false),
            ),
            // Mixed streams carry interlacing and field order per buffer.
            (
                M::Mixed,
                O::Unknown,
                F::INTERLACED | F::TFF,
                (Mode::Interleaved, Order::TopFirst, false, false),
            ),
            (
                M::Mixed,
                O::Unknown,
                F::INTERLACED,
                (Mode::Interleaved, Order::BottomFirst, false, false),
            ),
            (
                M::Mixed,
                O::Unknown,
                F::empty(),
                (Mode::Progressive, Order::Unknown, false, false),
            ),
            (
                M::Mixed,
                O::Unknown,
                F::TFF | F::RFF,
                (Mode::Progressive, Order::TopFirst, true, false),
            ),
        ] {
            assert_eq!(
                frame_fields(mode, caps_order, flags).unwrap(),
                expected,
                "{mode:?} {caps_order:?} {flags:?}"
            );
        }
        for mode in [M::Fields, M::Alternate] {
            assert!(matches!(
                frame_fields(mode, O::Unknown, F::INTERLACED),
                Err(Error::Decode(_))
            ));
        }
    }

    #[test]
    fn avcc_high_profile_extension_and_unsupported_sps_are_explicit() {
        let stream = include_bytes!("../tests/fixtures/bframes_8_64x64.h264");
        let sps = annexb_nals(stream)
            .into_iter()
            .map(|(_, nal)| nal)
            .find(|nal| nal[0] & 31 == 7)
            .unwrap();
        assert_eq!(sps[1], 100); // High profile, ordinary 8-bit 4:2:0
        let pps = [0x68, 0xce, 0x0f, 0xc8];
        let config = AvccConfiguration::new(&[sps, &pps], 4).unwrap();
        assert_eq!(
            config.high_profile,
            Some(AvccHighProfile {
                chroma_format: 1,
                bit_depth_luma_minus8: 0,
                bit_depth_chroma_minus8: 0,
            })
        );
        assert!(config.caps().ends_with("fdf8f800"));
        let unknown_profile = [0x67, 99, 0, 10, 0x80];
        assert!(matches!(
            AvccConfiguration::new(&[&unknown_profile, &pps], 4),
            Err(Error::InvalidInput(_))
        ));
        // CAVLC 4:4:4 Intra: sps_id 0, chroma_format_idc 3, separate planes
        // off, 8-bit luma and chroma.
        let cavlc444 = [0x67, 44, 0, 10, 0b1001_0001, 0b1100_0000];
        assert_eq!(
            AvccConfiguration::new(&[&cavlc444, &pps], 4)
                .unwrap()
                .high_profile,
            Some(AvccHighProfile {
                chroma_format: 3,
                bit_depth_luma_minus8: 0,
                bit_depth_chroma_minus8: 0,
            })
        );
        let truncated_high = [0x67, 100, 0, 10, 0x80];
        assert!(matches!(
            AvccConfiguration::new(&[&truncated_high, &pps], 4),
            Err(Error::InvalidInput(_))
        ));
        let oversized_pps = vec![0x68; u16::MAX as usize + 1];
        assert!(matches!(
            AvccConfiguration::new(&[sps, &oversized_pps], 1),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn high444_predictive_avcc_decodes() {
        let (sets, idr) =
            avcc_idr_fixture(include_bytes!("../tests/fixtures/high444_2_32x32.h264"));
        assert_eq!(sets[0][1], 244);
        let refs = sets.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let config = AvccConfiguration::new(&refs, 4).unwrap();
        assert_eq!(config.high_profile.unwrap().chroma_format, 3);
        let mut decoder = Decoder::h264_avcc(&refs, 4, 1).unwrap();
        decoder
            .submit(AccessUnit {
                id: 244,
                bytes: &idr,
                pts_ns: Some(0),
                dts_ns: None,
                duration_ns: None,
            })
            .unwrap();
        decoder.end_stream().unwrap();
        let frame = decoder.pull(Duration::from_secs(3)).unwrap().unwrap();
        assert_eq!(frame.id(), 244);
        assert_eq!((frame.layout().width, frame.layout().height), (32, 32));
    }

    #[test]
    fn high10_avcc_uses_16_bit_parameter_set_lengths_with_1_byte_nal_prefixes() {
        // Real High10 Annex-B, independently accepted by ffmpeg/ffprobe as
        // two valid frames. Its PPS contains 8 legal scaling lists (425 bytes).
        let source = include_bytes!("../tests/fixtures/high10_large_pps_2_32x32.h264");
        let mut sps = None;
        let mut pps = None;
        let mut vcl = Vec::new();
        for (_, nal) in annexb_nals(source) {
            match nal[0] & 31 {
                7 => sps = Some(nal),
                8 => pps = Some(nal),
                1 | 5 => {
                    assert!(nal.len() <= 255);
                    let mut packet = vec![nal.len() as u8];
                    packet.extend_from_slice(nal);
                    vcl.push(packet);
                }
                _ => {}
            }
        }
        let sps = sps.unwrap();
        let pps = pps.unwrap();
        assert_eq!(pps.len(), 425);
        assert_eq!(sps[1], 110); // High 10 profile
        let config = AvccConfiguration::new(&[sps, pps], 1).unwrap();
        assert_eq!(
            config.high_profile,
            Some(AvccHighProfile {
                chroma_format: 1,
                bit_depth_luma_minus8: 2,
                bit_depth_chroma_minus8: 2,
            })
        );
        let caps = config.caps();
        let hex = caps
            .strip_prefix(
                "video/x-h264,stream-format=(string)avc,alignment=(string)au,codec_data=(buffer)",
            )
            .unwrap();
        let record = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(record[4], 0xfc); // 1-byte NAL prefixes
        assert_eq!(record[5], 0xe1);
        let pps_offset = 8 + sps.len();
        assert_eq!(record[pps_offset], 1);
        assert_eq!(
            &record[pps_offset + 1..pps_offset + 3],
            &(pps.len() as u16).to_be_bytes()
        );
        assert_eq!(&record[pps_offset + 3..pps_offset + 3 + pps.len()], pps);
        assert_eq!(&record[record.len() - 4..], &[0xfd, 0xfa, 0xfa, 0]);
        assert_eq!(vcl.len(), 2);
        // The large-PPS source also decodes through the bounded Annex-B
        // fallback when GStreamer's AVC codec-data parser rejects long PPS.
        let smaller_pps = [0x68, 0xce, 0x0f, 0xc8];
        let mut decoder = Decoder::h264_avcc(&[sps, pps], 1, 1).unwrap();
        assert!(decoder.avcc_annexb_fallback);
        assert!(matches!(
            decoder.submit(AccessUnit {
                id: 299,
                bytes: &[3, 0x65],
                pts_ns: None,
                dts_ns: None,
                duration_ns: None,
            }),
            Err(Error::InvalidInput(_))
        ));
        assert!(decoder.initial_annexb_sets_pending);
        for (i, bytes) in vcl.iter().enumerate() {
            decoder
                .submit(AccessUnit {
                    id: i as u64 + 300,
                    bytes,
                    pts_ns: Some(i as i64 * 200_000_000),
                    dts_ns: None,
                    duration_ns: Some(200_000_000),
                })
                .unwrap();
        }
        decoder.end_stream().unwrap();
        for i in 0..2 {
            let frame = decoder
                .pull(Duration::from_secs(3))
                .unwrap()
                .expect("High10 picture dropped");
            assert_eq!(frame.id(), i + 300);
            assert_eq!((frame.layout().width, frame.layout().height), (32, 32));
        }
        // The ordinary AVCC path continues decoding High10 without copying
        // and rewriting NAL prefixes when no parameter set exceeds 255 bytes.
        let mut normal = Decoder::h264_avcc(&[sps, &smaller_pps], 1, 1).unwrap();
        assert!(!normal.avcc_annexb_fallback);
        for (i, bytes) in vcl.iter().enumerate() {
            normal
                .submit(AccessUnit {
                    id: i as u64 + 400,
                    bytes,
                    pts_ns: Some(i as i64 * 200_000_000),
                    dts_ns: None,
                    duration_ns: Some(200_000_000),
                })
                .unwrap();
        }
        normal.end_stream().unwrap();
        for i in 0..2 {
            let frame = normal.pull(Duration::from_secs(3)).unwrap().unwrap();
            assert_eq!(frame.id(), i + 400);
        }
        // A large initial avcC PPS is valid; sending such a PPS in-band
        // is impossible with a one-byte prefix and must be rejected without
        // modifying an existing session's configuration.
        let mut short = Decoder::h264_avcc(&[sps, &smaller_pps], 1, 1).unwrap();
        assert!(matches!(
            short.reconfigure_avcc(&[sps, pps], 1),
            Err(Error::InvalidInput(_))
        ));
        assert_eq!(short.avcc.as_ref().unwrap().pps[0], smaller_pps);
        assert!(short.pending_pps.is_none());
    }

    #[test]
    fn terminal_eos_and_clock_sentinels_are_not_silent() {
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        assert!(!decoder.is_drained().unwrap());
        let data = include_bytes!("../tests/fixtures/red_32x32.h264");
        for (id, pts, dts, duration) in [
            (u64::MAX, None, None, None),
            (1, Some(i64::MIN), None, None),
            (1, None, Some(i64::MIN), None),
            (1, None, None, Some(u64::MAX)),
        ] {
            assert!(matches!(
                decoder.submit(AccessUnit {
                    id,
                    bytes: data,
                    pts_ns: pts,
                    dts_ns: dts,
                    duration_ns: duration,
                }),
                Err(Error::InvalidInput(_))
            ));
        }
        assert!(!decoder.is_drained().unwrap());
        decoder
            .submit(AccessUnit {
                id: 7,
                bytes: data,
                pts_ns: Some(0),
                dts_ns: None,
                duration_ns: None,
            })
            .unwrap();
        decoder.end_stream().unwrap();
        let frame = decoder.pull(Duration::from_secs(3)).unwrap().unwrap();
        assert_eq!(frame.id(), 7);
        assert!(decoder.pull(Duration::from_secs(1)).unwrap().is_none());
        assert!(decoder.is_drained().unwrap());
    }

    #[test]
    fn fatal_bus_error_after_eos_prevents_successful_drain() {
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        let bytes = include_bytes!("../tests/fixtures/red_32x32.h264");
        decoder
            .submit(AccessUnit {
                id: 7,
                bytes,
                pts_ns: Some(0),
                dts_ns: None,
                duration_ns: None,
            })
            .unwrap();
        decoder.end_stream().unwrap();
        let frame = decoder.pull(Duration::from_secs(2)).unwrap().unwrap();
        assert_eq!(frame.id(), 7);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !decoder.is_drained().unwrap() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(decoder.is_drained().unwrap());
        decoder
            .pipeline
            .bus()
            .unwrap()
            .post(
                gst::message::Error::builder(gst::CoreError::Failed, "post-eos injected fatal")
                    .src(&decoder.pipeline)
                    .build(),
            )
            .unwrap();
        match decoder.is_drained() {
            Err(Error::Decode(message)) => assert!(message.contains("post-eos injected fatal")),
            _ => panic!("drain status ignored a pending fatal bus error"),
        }
        assert!(matches!(decoder.is_drained(), Err(Error::Decode(_))));
    }

    #[test]
    fn malformed_plane_layouts_are_rejected_without_panics() {
        let baseline = FrameLayout {
            width: 17,
            height: 17,
            format: PixelFormat::Nv12,
            strides: vec![17, 18],
            offsets: vec![0, 17 * 17],
            crop: None,
            pixel_aspect_ratio: (1, 1),
            chroma_siting: ChromaSiting::Unknown,
            interlace_mode: FrameInterlaceMode::Progressive,
            field_order: FrameFieldOrder::Unknown,
            repeat_first_field: false,
            one_field: false,
            matrix: ColorMatrix::Bt601,
            primaries: ColorPrimaries::Bt709,
            transfer: ColorTransfer::Bt709,
            full_range: Some(false),
        };
        assert!(validate_planes(&baseline, 17 * 17 + 18 * 9).is_ok());
        assert_eq!(baseline.plane_byte_span(0), Some(17 * 17));
        assert_eq!(baseline.plane_byte_span(1), Some(18 * 9));
        assert_eq!(baseline.plane_byte_span(2), None);
        let mut invalid = baseline.clone();
        invalid.strides[1] = 17;
        assert!(validate_planes(&invalid, usize::MAX).is_err());
        invalid = baseline.clone();
        invalid.offsets[1] = usize::MAX;
        assert!(validate_planes(&invalid, usize::MAX).is_err());
        invalid = baseline.clone();
        invalid.crop = Some(FrameCrop {
            x: 16,
            y: 0,
            width: 2,
            height: 1,
        });
        assert!(validate_planes(&invalid, usize::MAX).is_err());
        invalid.crop = Some(FrameCrop {
            x: 0,
            y: 16,
            width: 1,
            height: 2,
        });
        assert!(validate_planes(&invalid, usize::MAX).is_err());
        invalid = baseline;
        invalid.width = usize::MAX;
        assert!(validate_planes(&invalid, usize::MAX).is_err());
    }

    #[test]
    fn explicit_vui_color_primaries_and_transfer_are_preserved() {
        let stream = include_bytes!("../tests/fixtures/explicit_bt709_color_64x64.h264");
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        decoder
            .submit(AccessUnit {
                id: 998,
                bytes: stream,
                pts_ns: Some(0),
                dts_ns: Some(0),
                duration_ns: None,
            })
            .unwrap();
        decoder.end_stream().unwrap();
        let frame = decoder.pull(Duration::from_secs(2)).unwrap().unwrap();
        assert_eq!(frame.layout().matrix, ColorMatrix::Bt709);
        assert_eq!(frame.layout().primaries, ColorPrimaries::Bt709);
        assert_eq!(frame.layout().transfer, ColorTransfer::Bt709);
        assert_eq!(frame.layout().full_range, Some(true));
    }

    #[test]
    fn colorimetry_is_exposed_for_cpu_output() {
        for (stream, full, matrix) in [
            (
                include_bytes!("../tests/fixtures/color_full_64x64.h264").as_slice(),
                true,
                ColorMatrix::Bt709,
            ),
            (
                include_bytes!("../tests/fixtures/color_limited_64x64.h264").as_slice(),
                false,
                ColorMatrix::Bt601,
            ),
        ] {
            let mut decoder = Decoder::h264_nv12(1).unwrap();
            decoder
                .submit(AccessUnit {
                    id: 678,
                    bytes: stream,
                    pts_ns: Some(0),
                    dts_ns: Some(0),
                    duration_ns: Some(33_333_333),
                })
                .unwrap();
            decoder.end_stream().unwrap();
            let frame = decoder.pull(Duration::from_secs(2)).unwrap().unwrap();
            assert_eq!(frame.layout().full_range, Some(full), "range");
            assert_eq!(frame.layout().matrix, matrix, "matrix");
            // No VUI primaries/transfer in these older fixtures; unlike matrix
            // and range they must remain unknown rather than guessed.
            assert_eq!(frame.layout().primaries, ColorPrimaries::Unknown);
            assert_eq!(frame.layout().transfer, ColorTransfer::Unknown);
        }
    }

    #[test]
    fn unrecognized_numeric_color_values_are_not_treated_as_unspecified() {
        assert_eq!(
            color_matrix(gst_video::VideoColorMatrix::Unknown),
            ColorMatrix::Unknown
        );
        assert_eq!(
            color_matrix(gst_video::VideoColorMatrix::__Unknown(413)),
            ColorMatrix::Other(413)
        );
        assert_eq!(
            color_primaries(gst_video::VideoColorPrimaries::Unknown),
            ColorPrimaries::Unknown
        );
        assert_eq!(
            color_primaries(gst_video::VideoColorPrimaries::__Unknown(414)),
            ColorPrimaries::Other(414)
        );
        assert_eq!(
            color_transfer(gst_video::VideoTransferFunction::Unknown),
            ColorTransfer::Unknown
        );
        assert_eq!(
            color_transfer(gst_video::VideoTransferFunction::__Unknown(415)),
            ColorTransfer::Other(415)
        );
    }

    #[test]
    fn bt2020_pq_and_hlg_metadata_survive_decode() {
        for (stream, transfer) in [
            (
                include_bytes!("../tests/fixtures/bt2020_pq_2_64x64.h264").as_slice(),
                ColorTransfer::Smpte2084,
            ),
            (
                include_bytes!("../tests/fixtures/bt2020_hlg_2_64x64.h264").as_slice(),
                ColorTransfer::AribStdB67,
            ),
        ] {
            let units = access_units(stream);
            assert_eq!(units.len(), 2);
            let mut decoder = Decoder::h264_nv12(1).unwrap();
            for (i, bytes) in units.iter().enumerate() {
                decoder
                    .submit(AccessUnit {
                        id: i as u64 + 9000,
                        bytes,
                        pts_ns: Some(i as i64 * 200_000_000),
                        dts_ns: None,
                        duration_ns: Some(200_000_000),
                    })
                    .unwrap();
            }
            decoder.end_stream().unwrap();
            for i in 0..2 {
                let frame = decoder.pull(Duration::from_secs(3)).unwrap().unwrap();
                assert_eq!(frame.id(), i + 9000);
                assert_eq!(frame.layout().matrix, ColorMatrix::Bt2020);
                assert_eq!(frame.layout().primaries, ColorPrimaries::Bt2020);
                assert_eq!(frame.layout().transfer, transfer);
            }
        }
    }

    #[test]
    fn undefined_avcc_nal_types_are_rejected_before_submission() {
        for width in [1_u8, 2, 4] {
            for nal_type in [0_u8, 16, 17, 18, 22, 23, 24, 31] {
                let mut packet = vec![0; usize::from(width)];
                packet[usize::from(width) - 1] = 1;
                packet.push(nal_type);
                assert!(matches!(
                    each_avcc_nal(&packet, width, |_| Ok(())),
                    Err(Error::InvalidInput(_))
                ));
                let mut annexb = Vec::new();
                assert!(matches!(
                    avcc_to_annexb(&packet, width, &mut annexb, |_| {}),
                    Err(Error::InvalidInput(_))
                ));
                assert!(annexb.is_empty());
                // A structurally valid first NAL does not exempt a later
                // undefined NAL header from validation.
                let mut second = vec![0; usize::from(width)];
                second[usize::from(width) - 1] = 1;
                second.push(0x65);
                second.extend_from_slice(&packet);
                assert!(matches!(
                    each_avcc_nal(&second, width, |_| Ok(())),
                    Err(Error::InvalidInput(_))
                ));
            }
            // Defined extension NAL types must not be mistaken for reserved
            // values; the decoder is responsible for semantic support.
            for nal_type in [1_u8, 6, 9, 13, 15, 19, 20, 21] {
                let mut packet = vec![0; usize::from(width)];
                packet[usize::from(width) - 1] = 1;
                packet.push(nal_type);
                each_avcc_nal(&packet, width, |_| Ok(())).unwrap();
            }
        }
    }

    #[test]
    fn malformed_avcc_nal_chain_does_not_poison_decoder() {
        let (sets, valid) =
            avcc_idr_fixture(include_bytes!("../tests/fixtures/pps_qp18_64x64.h264"));
        let refs = sets.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let mut decoder = Decoder::h264_avcc(&refs, 4, 1).unwrap();
        let bad = [
            vec![0, 0, 0, 10, 0x65],
            vec![0, 0, 0, 0],
            vec![0, 0, 0, 1, 0x65, 0, 0, 0, 10, 0x61],
            vec![0, 0, 0, 1, 0x65, 0, 0],
            vec![0, 0, 0, 1, 0xe5], // forbidden_zero_bit
            vec![0, 0, 0, 1, 0x00], // unspecified type 0
            vec![0, 0, 0, 1, 0x1f], // unspecified type 31
        ];
        for (i, bytes) in bad.iter().enumerate() {
            assert!(
                matches!(
                    decoder.submit(AccessUnit {
                        id: i as u64 + 700,
                        bytes,
                        pts_ns: Some(0),
                        dts_ns: None,
                        duration_ns: Some(33_333_333),
                    }),
                    Err(Error::InvalidInput(_))
                ),
                "malformed AVCC sequence {i} accepted"
            );
            assert!(decoder.fatal_error.is_none());
            assert_eq!(decoder.input.current_level_bytes(), 0);
        }
        decoder
            .submit(AccessUnit {
                id: 999,
                bytes: &valid,
                pts_ns: Some(0),
                dts_ns: None,
                duration_ns: Some(33_333_333),
            })
            .unwrap();
        decoder.end_stream().unwrap();
        let frame = decoder.pull(Duration::from_secs(3)).unwrap().unwrap();
        assert_eq!(frame.id(), 999);
    }

    #[test]
    fn aggregate_parameter_set_boundary_and_failed_update_are_atomic() {
        let (sets, valid) =
            avcc_idr_fixture(include_bytes!("../tests/fixtures/pps_qp18_64x64.h264"));
        let sps = sets[0].as_slice();
        // For byte-stream fallback the decoder prepends four start-code bytes
        // to every SPS/PPS, and a valid first 1-byte slice needs five bytes.
        let allowance = INPUT_QUEUE_LIMIT as usize - 5;
        let oversized_pps = vec![0x68; u16::MAX as usize];
        let mut near = Vec::with_capacity(130);
        near.push(sps);
        for _ in 0..127 {
            near.push(oversized_pps.as_slice());
        }
        let used = (sps.len() + 4) + 127 * (oversized_pps.len() + 4);
        let remainder = allowance - used - 4;
        assert!((2..=u16::MAX as usize).contains(&remainder));
        let last_pps = vec![0x68; remainder];
        near.push(&last_pps);
        let near_config = AvccConfiguration::new(&near, 4).unwrap();
        assert_eq!(
            near_config
                .sps
                .iter()
                .chain(&near_config.pps)
                .map(|s| s.len() + 4)
                .sum::<usize>(),
            allowance
        );
        // One more byte leaves no space for the smallest valid converted AU.
        let mut over_pps = last_pps.clone();
        over_pps.push(0x68);
        near.pop();
        near.push(&over_pps);
        assert!(matches!(
            AvccConfiguration::new(&near, 4),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            Decoder::h264_avcc(&near, 4, 1),
            Err(Error::InvalidInput(_))
        ));
        let mut decoder = Decoder::h264_avcc(&[sps, &sets[1]], 4, 1).unwrap();
        assert!(matches!(
            decoder.reconfigure_avcc(&near, 4),
            Err(Error::InvalidInput(_))
        ));
        assert_eq!(decoder.avcc.as_ref().unwrap().pps.len(), 1);
        assert!(decoder.pending_pps.is_none());
        decoder
            .submit(AccessUnit {
                id: 800,
                bytes: &valid,
                pts_ns: Some(0),
                dts_ns: None,
                duration_ns: None,
            })
            .unwrap();
        decoder.end_stream().unwrap();
        assert_eq!(
            decoder.pull(Duration::from_secs(3)).unwrap().unwrap().id(),
            800
        );
    }

    fn avcc_idr_fixture(stream: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
        let mut headers = Vec::new();
        let mut idr = Vec::new();
        for (_, bytes) in annexb_nals(stream) {
            match bytes[0] & 31 {
                7 | 8 => headers.push(bytes.to_vec()),
                5 => {
                    idr.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
                    idr.extend_from_slice(bytes);
                }
                _ => {}
            }
        }
        assert_eq!(headers.len(), 2);
        assert!(!idr.is_empty());
        (headers, idr)
    }

    #[test]
    fn chroma_siting_preserves_distinct_negotiated_positions() {
        let sources: [(&[u8], ChromaSiting); 2] = [
            (
                include_bytes!("../tests/fixtures/chroma_mpeg2_2_64x64.h264"),
                ChromaSiting::Mpeg2,
            ),
            (
                include_bytes!("../tests/fixtures/chroma_jpeg_2_64x64.h264"),
                ChromaSiting::Jpeg,
            ),
        ];
        for (source, expected) in sources {
            let units = access_units(source);
            assert_eq!(units.len(), 2);
            let mut decoder = Decoder::h264_nv12(1).unwrap();
            for (id, bytes) in units.iter().enumerate() {
                decoder
                    .submit(AccessUnit {
                        id: id as u64 + 1000,
                        bytes,
                        pts_ns: Some(id as i64 * 200_000_000),
                        dts_ns: None,
                        duration_ns: Some(200_000_000),
                    })
                    .unwrap();
            }
            decoder.end_stream().unwrap();
            for index in 0..2 {
                let frame = decoder.pull(Duration::from_secs(3)).unwrap().unwrap();
                assert_eq!(frame.id(), index + 1000);
                assert_eq!(frame.layout().chroma_siting, expected);
                assert_eq!(frame.layout().format, PixelFormat::Nv12);
            }
        }
        assert_eq!(
            chroma_siting(gst_video::VideoChromaSite::empty()),
            ChromaSiting::Unknown
        );
        assert_eq!(
            chroma_siting(gst_video::VideoChromaSite::DV),
            ChromaSiting::Dv
        );
    }

    #[test]
    fn negative_media_times_roundtrip_through_bframe_reordering() {
        assert!(matches!(
            gst_media_time(i64::MIN),
            Err(Error::InvalidInput(_))
        ));
        for ts in [
            -1_000_000_000,
            -99_999_999,
            -1,
            0,
            1,
            1_000_000_000,
            i64::MAX,
        ] {
            assert_eq!(
                original_media_time(gst_media_time(ts).unwrap()).unwrap(),
                ts
            );
        }
        let frames = fixture_access_units();
        assert_eq!(frames.len(), 8);
        // Decode order differs from display order; the initial decode time is
        // negative. Per-ID source timing is retained even when avdec_h264
        // discards its output DTS.
        let presentation_order = [0_i64, 3, 1, 2, 6, 4, 5, 7];
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        for (index, packet) in frames.iter().enumerate() {
            decoder
                .submit(AccessUnit {
                    id: index as u64 + 1100,
                    bytes: packet,
                    pts_ns: Some((presentation_order[index] - 4) * 33_333_333),
                    dts_ns: Some((index as i64 - 4) * 33_333_333),
                    duration_ns: Some(33_333_333),
                })
                .unwrap();
        }
        decoder.end_stream().unwrap();
        let mut found = Vec::new();
        let mut negative_decoded = false;
        for _ in 0..8 {
            let frame = decoder.pull(Duration::from_secs(3)).unwrap().unwrap();
            let i = (frame.id() - 1100) as usize;
            assert_eq!(
                frame.source_pts_ns(),
                Some((presentation_order[i] - 4) * 33_333_333)
            );
            assert_eq!(frame.source_dts_ns(), Some((i as i64 - 4) * 33_333_333));
            // avdec_h264 may reassign decoded PTS while preserving source
            // timing metadata attached to the exact source frame identity.
            negative_decoded |= frame.pts_ns().is_some_and(|pts| pts < 0);
            found.push(i);
        }
        found.sort_unstable();
        assert_eq!(found, (0..8).collect::<Vec<_>>());
        assert!(negative_decoded, "negative decoded output PTS was lost");
    }

    #[test]
    fn annexb_fallback_injects_parameter_sets_after_a_leading_aud() {
        let inject = |out: &mut Vec<u8>| out.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xce]);
        let mut out = Vec::new();
        avcc_to_annexb(&[2, 9, 0xf0, 2, 0x65, 0x88], 1, &mut out, inject).unwrap();
        assert_eq!(
            out,
            [
                0, 0, 0, 1, 9, 0xf0, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1, 0x65, 0x88
            ]
        );
        out.clear();
        avcc_to_annexb(&[2, 0x65, 0x88], 1, &mut out, inject).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1, 0x65, 0x88]);
        out.clear();
        avcc_to_annexb(&[2, 9, 0xf0], 1, &mut out, inject).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 9, 0xf0, 0, 0, 0, 1, 0x68, 0xce]);
    }

    #[test]
    fn leading_avcc_aud_is_recognized_and_malformed_lengths_rejected() {
        for width in [1, 2, 4] {
            let mut packet = vec![0; width];
            packet[width - 1] = 2;
            packet.extend_from_slice(&[9, 0xf0]);
            packet[width - 1] = 2;
            assert_eq!(leading_avcc_aud(&packet, width as u8).unwrap(), Some(2));
            packet[width] = 0x65;
            assert_eq!(leading_avcc_aud(&packet, width as u8).unwrap(), None);
            packet[width - 1] = 3;
            assert!(matches!(
                leading_avcc_aud(&packet, width as u8),
                Err(Error::InvalidInput(_))
            ));
        }
    }

    #[test]
    fn changed_midgop_pps_preserves_reference_dependent_output() {
        // The source has one IDR and nineteen dependent P pictures. At picture
        // ten its PPS ID 0 is redefined with pic_init_qp_minus26 -3 -> -2.
        // FFmpeg's independent raw-video decode hashes are pinned below.
        let stream = include_bytes!("../tests/fixtures/pps_midgop_changed_20_64x64.h264");
        let golden = include_str!("../tests/fixtures/pps_midgop_changed_20_64x64.luma_fnv64");
        let units = access_units(stream);
        assert_eq!(units.len(), 20);
        let mut sps = None;
        let mut old_pps = None;
        let mut new_pps = None;
        let mut avcc = Vec::new();

        for (index, au) in units.iter().enumerate() {
            let mut packet = Vec::new();
            for (_, nal) in annexb_nals(au) {
                match nal[0] & 31 {
                    7 => {
                        assert_eq!(index, 0);
                        sps = Some(nal.to_vec());
                    }
                    8 => {
                        if index == 0 {
                            old_pps = Some(nal.to_vec());
                        } else {
                            assert_eq!(index, 10);
                            new_pps = Some(nal.to_vec());
                        }
                    }
                    1 | 5 | 9 => {
                        packet.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                        packet.extend_from_slice(nal);
                    }
                    _ => {}
                }
            }
            assert!(!packet.is_empty(), "missing slice in picture {index}");
            assert_eq!(packet[4] & 31, 9, "each PPS regression AU retains its AUD");
            avcc.push(packet);
        }
        let sps = sps.unwrap();
        let old_pps = old_pps.unwrap();
        let new_pps = new_pps.unwrap();
        assert_eq!(old_pps, [0x68, 0xce, 0x0f, 0xc8]);
        assert_eq!(new_pps, [0x68, 0xce, 0x0b, 0xc8]);
        let mut decoder = Decoder::h264_avcc(&[&sps, &old_pps], 4, 1).unwrap();

        for (index, packet) in avcc.iter().enumerate() {
            if index == 10 {
                decoder.reconfigure_avcc(&[&sps, &new_pps], 4).unwrap();
            }
            decoder
                .submit(AccessUnit {
                    id: index as u64 + 200,
                    bytes: packet,
                    pts_ns: Some(index as i64 * 100_000_000),
                    dts_ns: None,
                    duration_ns: Some(100_000_000),
                })
                .unwrap();
        }
        decoder.end_stream().unwrap();
        let mut hashes = Vec::new();
        for index in 0..20 {
            let frame = decoder
                .pull(Duration::from_secs(3))
                .unwrap()
                .unwrap_or_else(|| {
                    panic!("decoder dropped PPS-change picture after {index} outputs")
                });
            assert_eq!(frame.id(), index + 200);
            let hash = frame
                .with_bytes(|mapped, info| {
                    let mut hash = 0xcbf29ce484222325_u64;
                    for row in 0..64 {
                        let begin = info.offsets[0] + row * info.strides[0];
                        for &pixel in &mapped[begin..begin + 64] {
                            hash = (hash ^ u64::from(pixel)).wrapping_mul(0x100000001b3);
                        }
                    }
                    format!("{hash:016x}")
                })
                .unwrap();
            hashes.push(hash);
        }
        assert_eq!(hashes, golden.lines().collect::<Vec<_>>());
    }

    #[test]
    fn compatible_pps_change_keeps_gstreamer_session_alive() {
        let first = avcc_idr_fixture(include_bytes!("../tests/fixtures/pps_qp18_64x64.h264"));
        let second = avcc_idr_fixture(include_bytes!("../tests/fixtures/pps_qp32_64x64.h264"));
        assert_eq!(first.0[0], second.0[0], "must be compatible SPS");
        assert_ne!(first.0[1], second.0[1], "PPS must change");
        fn refs(s: &[Vec<u8>]) -> Vec<&[u8]> {
            s.iter().map(Vec::as_slice).collect()
        }
        let mut decoder = Decoder::h264_avcc(&refs(&first.0), 4, 1).unwrap();
        decoder
            .submit(AccessUnit {
                id: 50,
                bytes: &first.1,
                pts_ns: Some(0),
                dts_ns: Some(0),
                duration_ns: None,
            })
            .unwrap();
        // First frame may be delayed internally, but reconfiguration must not
        // discard it or reset session identity.
        let mut invalid = refs(&second.0);
        let mut bad_sps = second.0[0].clone();
        bad_sps[2] ^= 1;
        invalid[0] = &bad_sps;
        assert!(matches!(
            decoder.reconfigure_avcc(&invalid, 4),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            decoder.reconfigure_avcc(&refs(&second.0), 2),
            Err(Error::InvalidInput(_))
        ));
        decoder.reconfigure_avcc(&refs(&second.0), 4).unwrap();
        decoder
            .submit(AccessUnit {
                id: 51,
                bytes: &second.1,
                pts_ns: Some(33_333_333),
                dts_ns: Some(33_333_333),
                duration_ns: None,
            })
            .unwrap();
        decoder.end_stream().unwrap();
        let mut ids = Vec::new();
        for _ in 0..2 {
            ids.push(
                decoder
                    .pull(Duration::from_secs(3))
                    .unwrap()
                    .expect("PPS output missing")
                    .id(),
            );
        }
        ids.sort_unstable();
        assert_eq!(ids, [50, 51]);
    }

    #[test]
    fn avcc_samples_decode_using_format_description_parameter_sets() {
        let frames = fixture_access_units();
        let mut sets = Vec::new();
        let mut packets = Vec::new();
        for bytes in &frames {
            let mut packet = Vec::new();
            for (_, nal) in annexb_nals(bytes) {
                match nal[0] & 31 {
                    7 | 8 => sets.push(nal),
                    1 | 5 | 6 => {
                        packet.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                        packet.extend_from_slice(nal);
                    }
                    _ => {}
                }
            }
            packets.push(packet);
        }
        assert_eq!(sets.len(), 2);
        let mut decoder = Decoder::h264_avcc(&sets, 4, 1).unwrap();
        for (i, bytes) in packets.iter().enumerate() {
            decoder
                .submit(AccessUnit {
                    id: (i + 17) as u64,
                    bytes,
                    pts_ns: Some(i as i64 * 33_333_333),
                    dts_ns: None,
                    duration_ns: Some(33_333_333),
                })
                .unwrap();
        }
        decoder.end_stream().unwrap();
        let mut found = Vec::new();
        for _ in 0..8 {
            let output = decoder
                .pull(Duration::from_secs(3))
                .unwrap()
                .expect("missing AVCC output");
            found.push(output.id());
        }
        found.sort_unstable();
        assert_eq!(found, (17..25).collect::<Vec<_>>());
    }

    #[test]
    fn bframes_preserve_identity_through_parser_decoder_and_color_converter() {
        let frames = fixture_access_units();
        assert_eq!(frames.len(), 8);
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        for (id, bytes) in frames.iter().enumerate() {
            decoder
                .submit(AccessUnit {
                    id: id as u64 + 1,
                    bytes,
                    // Deliberately repeated PTS: mapping by timestamp is wrong.
                    pts_ns: Some((id as i64 / 2) * 33_333_333),
                    dts_ns: None,
                    duration_ns: Some(33_333_333),
                })
                .unwrap();
        }
        decoder.end_stream().unwrap();
        let mut ids = Vec::new();
        for _ in 0..frames.len() {
            let frame = decoder
                .pull(Duration::from_secs(3))
                .unwrap()
                .expect("missing frame");
            ids.push(frame.id());
            assert_eq!((frame.layout().width, frame.layout().height), (64, 64));
            frame
                .with_bytes(|bytes, info| {
                    assert!(info.offsets[1] > info.offsets[0]);
                    assert!(bytes.len() > info.offsets[1]);
                })
                .unwrap();
        }
        // Display-order B-frame identity must not be reassigned by repeated
        // PTS values or the GStreamer parser/converter chain.
        assert_eq!(ids, [1, 3, 4, 2, 6, 7, 5, 8]);
        assert!(decoder.pull(Duration::ZERO).unwrap().is_none());
        // GStreamer marks its appsink EOS asynchronously even after the last
        // sample has been returned. Poll for terminal EOS; an empty output
        // queue alone never establishes decoder completion.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !decoder.is_drained().unwrap() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(decoder.is_drained().unwrap(), "terminal EOS never arrived");
        assert!(matches!(
            decoder.submit(AccessUnit {
                id: 9,
                bytes: frames[0],
                pts_ns: None,
                dts_ns: None,
                duration_ns: None,
            }),
            Err(Error::EndOfStream)
        ));
    }
}
