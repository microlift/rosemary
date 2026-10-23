//! Optional GStreamer backend: incremental H.264 Annex-B AU input, owned
//! decoded output, and exact identity independent of reordered PTS/DTS.
//!
//! This implements the backend contract only; it does not claim to implement
//! synchronous VideoToolbox decode, FinishDelayedFrames, or GPU CVPixelBuffers.

use std::time::Duration;

use gst::prelude::*;
use gst_app::{AppSink, AppSrc};

use crate::{
    AccessUnit, ColorMatrix, ColorPrimaries, ColorTransfer, Error, FrameLayout, PixelFormat,
    VideoCodec, VideoDecoder, VideoFrame,
};

pub struct Decoder {
    pipeline: gst::Pipeline,
    input: AppSrc,
    output: AppSink,
    reference: gst::Caps,
    completed: bool,
    output_format: PixelFormat,
    avcc: Option<AvccConfiguration>,
}

struct AvccConfiguration {
    sps: Vec<Vec<u8>>,
    pps: Vec<Vec<u8>>,
    nal_length: u8,
}

impl AvccConfiguration {
    fn new(sets: &[&[u8]], nal_length: u8) -> Result<Self, Error> {
        if !matches!(nal_length, 1 | 2 | 4) {
            return Err(Error::InvalidInput("invalid AVCC NAL length width".into()));
        }
        let mut sps = Vec::new();
        let mut pps = Vec::new();
        for &set in sets {
            if set.first().is_none_or(|b| b & 0x80 != 0) || set.len() > u16::MAX as usize {
                return Err(Error::InvalidInput("malformed AVCC parameter set".into()));
            }
            match set[0] & 31 {
                7 if set.len() >= 4 => sps.push(set.to_vec()),
                8 if set.len() >= 2 => pps.push(set.to_vec()),
                _ => return Err(Error::InvalidInput("unexpected parameter set type".into())),
            }
        }
        if sps.is_empty() || sps.len() > 31 || pps.is_empty() || pps.len() > 255 {
            return Err(Error::InvalidInput("AVCC requires SPS/PPS".into()));
        }
        Ok(Self {
            sps,
            pps,
            nal_length,
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
}

impl VideoFrame for DecodedFrame {
    fn id(&self) -> u64 {
        self.id
    }

    fn layout(&self) -> &FrameLayout {
        &self.layout
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
    let chroma_width = layout.width.div_ceil(2);
    let chroma_height = layout.height.div_ceil(2);
    let required: Vec<(usize, usize)> = match layout.format {
        PixelFormat::Nv12 => vec![
            (layout.width, layout.height),
            (chroma_width * 2, chroma_height),
        ],
        PixelFormat::I420 => vec![
            (layout.width, layout.height),
            (chroma_width, chroma_height),
            (chroma_width, chroma_height),
        ],
        PixelFormat::Bgra => vec![(layout.width * 4, layout.height)],
    };
    if required.len() != layout.strides.len() || required.len() != layout.offsets.len() {
        return Err(Error::Decode("invalid decoded plane count".into()));
    }
    for (plane, (row_bytes, rows)) in required.into_iter().enumerate() {
        let stride = layout.strides[plane];
        let offset = layout.offsets[plane];
        let end = (rows - 1)
            .checked_mul(stride)
            .and_then(|v| v.checked_add(row_bytes))
            .and_then(|v| v.checked_add(offset));
        if stride < row_bytes || end.is_none_or(|n| n > size) {
            return Err(Error::Decode(format!(
                "decoded plane {plane} exceeds mapped buffer"
            )));
        }
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
        let mut decoder = Self::with_h264_caps(threads, &config.caps(), output_format)?;
        decoder.avcc = Some(config);
        Ok(decoder)
    }

    /// Update a compatible PPS in place without resetting the pipeline or
    /// losing decoder reference pictures. Changed SPS and NAL length are
    /// intentionally rejected until stream-compatibility is proven.
    pub fn reconfigure_avcc(&mut self, sets: &[&[u8]], nal_length: u8) -> Result<(), Error> {
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
        let caps = incoming
            .caps()
            .parse::<gst::Caps>()
            .map_err(|e| Error::InvalidInput(format!("invalid AVCC caps: {e}")))?;
        self.input.set_caps(Some(&caps));
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
            "appsrc name=rosemary_input format=time is-live=false block=false max-bytes=8388608 \
             caps=\"{input_caps}\" \
             ! h264parse ! avdec_h264 max-threads={threads} output-corrupt=false discard-corrupted-frames=true \
             ! videoconvert ! video/x-raw,format=(string){output_caps} \
             ! appsink name=rosemary_output sync=false max-buffers=32 drop=false"
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
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| Error::BackendUnavailable(format!("GStreamer failed to start: {e:?}")))?;
        Ok(Self {
            pipeline,
            input,
            output,
            reference: gst::Caps::builder("application/x-rosemary-frame-id").build(),
            completed: false,
            output_format,
            avcc: None,
        })
    }

    fn check_error(&self) -> Result<(), Error> {
        let bus = self
            .pipeline
            .bus()
            .ok_or_else(|| Error::Decode("missing GStreamer bus".into()))?;
        if let Some(msg) = bus.pop_filtered(&[gst::MessageType::Error])
            && let gst::MessageView::Error(e) = msg.view()
        {
            return Err(Error::Decode(format!("{}: {:?}", e.error(), e.debug())));
        }
        Ok(())
    }

    fn decode_sample(&self, sample: gst::Sample) -> Result<DecodedFrame, Error> {
        let buffer = sample
            .buffer()
            .ok_or_else(|| Error::Decode("no output buffer".into()))?;
        // Do NOT key callbacks on output PTS: h264parse/avdec_h264 can
        // legitimately output multiple frames with the same timestamp.
        let identity = buffer
            .iter_meta::<gst::ReferenceTimestampMeta>()
            .find(|meta| meta.reference() == self.reference.as_ref())
            .ok_or_else(|| {
                Error::Decode("H.264 output lost its sample identity metadata".into())
            })?;
        let id = identity.timestamp().nseconds();
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
        let color = info.colorimetry();
        let matrix = match color.matrix() {
            gst_video::VideoColorMatrix::Bt601 => ColorMatrix::Bt601,
            gst_video::VideoColorMatrix::Bt709 => ColorMatrix::Bt709,
            _ => ColorMatrix::Unknown,
        };
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
            matrix,
            primaries: match color.primaries() {
                gst_video::VideoColorPrimaries::Bt709 => ColorPrimaries::Bt709,
                gst_video::VideoColorPrimaries::Bt470bg => ColorPrimaries::Ebu3213,
                gst_video::VideoColorPrimaries::Smpte170m => ColorPrimaries::SmpteC,
                _ => ColorPrimaries::Unknown,
            },
            transfer: match color.transfer() {
                gst_video::VideoTransferFunction::Bt709 => ColorTransfer::Bt709,
                gst_video::VideoTransferFunction::Srgb => ColorTransfer::Srgb,
                _ => ColorTransfer::Unknown,
            },
            full_range: color.range() == gst_video::VideoColorRange::Range0_255,
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
        Ok(DecodedFrame { id, sample, layout })
    }
}

impl VideoDecoder for Decoder {
    type Frame = DecodedFrame;

    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }

    fn submit(&mut self, packet: AccessUnit<'_>) -> Result<(), Error> {
        if self.completed {
            return Err(Error::EndOfStream);
        }
        if packet.id == u64::MAX
            || [packet.pts_ns, packet.dts_ns, packet.duration_ns]
                .into_iter()
                .flatten()
                .any(|v| v == u64::MAX)
            || packet.bytes.is_empty()
            || packet.bytes.len() > 64 * 1024 * 1024
        {
            return Err(Error::InvalidInput(
                "invalid packet identifier or length".into(),
            ));
        }
        // GStreamer is asynchronous. Never block the guest's decode thread
        // while an appsink queue could itself be awaiting that same thread.
        // Return a recoverable signal without consuming the input instead.
        const INPUT_QUEUE_LIMIT: u64 = 8 * 1024 * 1024;
        if self.output.current_level_buffers() >= 32
            || (self.input.current_level_bytes() != 0
                && self
                    .input
                    .current_level_bytes()
                    .saturating_add(packet.bytes.len() as u64)
                    > INPUT_QUEUE_LIMIT)
        {
            return Err(Error::Backpressure);
        }
        // The caller owns input access units. GStreamer gets its own buffer;
        // decoded GstSample allocations remain owned until callback completion.
        let mut buffer = gst::Buffer::from_mut_slice(packet.bytes.to_vec());
        let writable = buffer
            .get_mut()
            .ok_or_else(|| Error::Decode("GstBuffer is not writable".into()))?;
        writable.set_pts(packet.pts_ns.map(gst::ClockTime::from_nseconds));
        writable.set_dts(packet.dts_ns.map(gst::ClockTime::from_nseconds));
        writable.set_duration(packet.duration_ns.map(gst::ClockTime::from_nseconds));
        gst::ReferenceTimestampMeta::add(
            writable,
            &self.reference,
            gst::ClockTime::from_nseconds(packet.id),
            None,
        );
        self.input
            .push_buffer(buffer)
            .map_err(|e| Error::Decode(format!("cannot submit H.264 access unit: {e:?}")))?;
        self.check_error()
    }

    fn pull(&mut self, timeout: Duration) -> Result<Option<Self::Frame>, Error> {
        // Zero-duration polling is valid: a decoder may still be holding
        // reference pictures and the caller must supply future access units.
        let nanos = u64::try_from(timeout.as_nanos().min((u64::MAX - 1) as u128)).unwrap();
        if let Some(sample) = self
            .output
            .try_pull_sample(gst::ClockTime::from_nseconds(nanos))
        {
            return self.decode_sample(sample).map(Some);
        }
        self.check_error()?;
        Ok(None)
    }

    fn end_stream(&mut self) -> Result<(), Error> {
        if !self.completed {
            self.input
                .end_of_stream()
                .map_err(|e| Error::Decode(format!("GStreamer EOS: {e:?}")))?;
            self.completed = true;
        }
        Ok(())
    }

    fn is_drained(&self) -> bool {
        self.completed && self.output.is_eos()
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

    fn fixture_access_units() -> Vec<Vec<u8>> {
        let stream = include_bytes!("../tests/fixtures/bframes_8_64x64.h264");
        let mut starts = Vec::new();
        for (i, window) in stream.windows(4).enumerate() {
            if window == [0, 0, 0, 1] {
                starts.push(i);
            }
        }
        let mut packets = Vec::new();
        for (index, &pos) in starts.iter().enumerate() {
            let end = starts.get(index + 1).copied().unwrap_or(stream.len());
            let nal = &stream[pos + 4..end];
            if nal.is_empty() {
                continue;
            }
            if nal[0] & 31 == 9 {
                packets.push(Vec::new());
            }
            if packets.is_empty() {
                packets.push(Vec::new());
            }
            packets
                .last_mut()
                .unwrap()
                .extend_from_slice(&stream[pos..end]);
        }
        packets
    }

    #[test]
    fn multi_gop_reordering_matches_ffmpeg_presentation_pixels() {
        let stream = include_bytes!("../tests/fixtures/multi_gop_bframes_64x64.h264");
        let expected = include_str!("../tests/fixtures/multi_gop_bframes_64x64.luma_fnv64");
        let mut starts = Vec::new();
        let mut cursor = 0;
        while cursor + 3 <= stream.len() {
            let size = if stream[cursor..].starts_with(&[0, 0, 0, 1]) {
                4
            } else if stream[cursor..].starts_with(&[0, 0, 1]) {
                3
            } else {
                cursor += 1;
                continue;
            };
            starts.push((cursor, size));
            cursor += size;
        }
        let mut units = Vec::<Vec<u8>>::new();
        for (i, &(pos, size)) in starts.iter().enumerate() {
            let end = starts.get(i + 1).map(|(p, _)| *p).unwrap_or(stream.len());
            let nal = &stream[pos + size..end];
            if nal.is_empty() {
                continue;
            }
            if nal[0] & 31 == 9 {
                units.push(Vec::new());
            }
            if units.is_empty() {
                units.push(Vec::new());
            }
            units
                .last_mut()
                .unwrap()
                .extend_from_slice(&stream[pos..end]);
        }
        assert_eq!(units.len(), 60);
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        for (i, bytes) in units.iter().enumerate() {
            decoder
                .submit(AccessUnit {
                    id: (i + 100) as u64,
                    bytes,
                    pts_ns: Some(i as u64 * 33_333_333),
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
        let mut sorted = identities;
        sorted.sort_unstable();
        assert_eq!(sorted, (100..160).collect::<Vec<_>>());
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
        let mut offsets = Vec::new();
        for (i, bytes) in stream.windows(5).enumerate() {
            if bytes[..4] == [0, 0, 0, 1] && bytes[4] & 31 == 9 {
                offsets.push(i);
            }
        }
        assert_eq!(offsets.len(), 60);
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        for (index, &start) in offsets.iter().enumerate() {
            let end = offsets.get(index + 1).copied().unwrap_or(stream.len());
            let unit = &stream[start..end];
            decoder
                .submit(AccessUnit {
                    id: index as u64,
                    bytes: unit,
                    pts_ns: Some(index as u64 * 33_333_333),
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
                pts_ns: Some(produced * 33_333_333),
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
    fn terminal_eos_and_clock_sentinels_are_not_silent() {
        let mut decoder = Decoder::h264_nv12(1).unwrap();
        assert!(!decoder.is_drained());
        let data = include_bytes!("../tests/fixtures/red_32x32.h264");
        for (id, pts, dts, duration) in [
            (u64::MAX, None, None, None),
            (1, Some(u64::MAX), None, None),
            (1, None, Some(u64::MAX), None),
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
        assert!(!decoder.is_drained());
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
        assert!(decoder.is_drained());
    }

    #[test]
    fn malformed_plane_layouts_are_rejected_without_panics() {
        let baseline = FrameLayout {
            width: 17,
            height: 17,
            format: PixelFormat::Nv12,
            strides: vec![17, 18],
            offsets: vec![0, 17 * 17],
            matrix: ColorMatrix::Bt601,
            primaries: ColorPrimaries::Bt709,
            transfer: ColorTransfer::Bt709,
            full_range: false,
        };
        assert!(validate_planes(&baseline, 17 * 17 + 18 * 9).is_ok());
        let mut invalid = baseline.clone();
        invalid.strides[1] = 17;
        assert!(validate_planes(&invalid, usize::MAX).is_err());
        invalid = baseline.clone();
        invalid.offsets[1] = usize::MAX;
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
        assert!(frame.layout().full_range);
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
            assert_eq!(frame.layout().full_range, full, "range");
            assert_eq!(frame.layout().matrix, matrix, "matrix");
            // No VUI primaries/transfer in these older fixtures; unlike matrix
            // and range they must remain unknown rather than guessed.
            assert_eq!(frame.layout().primaries, ColorPrimaries::Unknown);
            assert_eq!(frame.layout().transfer, ColorTransfer::Unknown);
        }
    }

    fn avcc_idr_fixture(stream: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
        let mut positions = Vec::new();
        let mut pos = 0;
        while pos + 3 <= stream.len() {
            let prefix = if stream[pos..].starts_with(&[0, 0, 0, 1]) {
                4
            } else if stream[pos..].starts_with(&[0, 0, 1]) {
                3
            } else {
                pos += 1;
                continue;
            };
            positions.push((pos, prefix));
            pos += prefix;
        }
        let mut headers = Vec::new();
        let mut idr = Vec::new();
        for (i, &(start, prefix)) in positions.iter().enumerate() {
            let end = positions.get(i + 1).map_or(stream.len(), |(n, _)| *n);
            let bytes = &stream[start + prefix..end];
            if bytes.is_empty() {
                continue;
            }
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
            let mut starts = Vec::new();
            let mut i = 0;
            while i + 3 <= bytes.len() {
                let separator = if bytes[i..].starts_with(&[0, 0, 0, 1]) {
                    4
                } else if bytes[i..].starts_with(&[0, 0, 1]) {
                    3
                } else {
                    i += 1;
                    continue;
                };
                starts.push((i, separator));
                i += separator;
            }
            let mut packet = Vec::new();
            for (n, &(start, separator)) in starts.iter().enumerate() {
                let end = starts
                    .get(n + 1)
                    .map(|(pos, _)| *pos)
                    .unwrap_or(bytes.len());
                let nal = &bytes[start + separator..end];
                if nal.is_empty() {
                    continue;
                }
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
                    pts_ns: Some(i as u64 * 33_333_333),
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
                    pts_ns: Some((id as u64 / 2) * 33_333_333),
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
        ids.sort_unstable();
        assert_eq!(ids, (1..=8).collect::<Vec<_>>());
        assert!(decoder.pull(Duration::ZERO).unwrap().is_none());
        // GStreamer marks its appsink EOS asynchronously even after the last
        // sample has been returned. Poll for terminal EOS; an empty output
        // queue alone never establishes decoder completion.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !decoder.is_drained() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(decoder.is_drained(), "terminal EOS never arrived");
        assert!(matches!(
            decoder.submit(AccessUnit {
                id: 9,
                bytes: &frames[0],
                pts_ns: None,
                dts_ns: None,
                duration_ns: None,
            }),
            Err(Error::EndOfStream)
        ));
    }
}
