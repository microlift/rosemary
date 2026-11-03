//! Host-neutral compressed-media decoding contracts.
//!
//! This crate does not implement any Darwin framework APIs. CoreMedia owns
//! compressed samples; VideoToolbox decides when guest callbacks run; and
//! CoreVideo owns guest-visible images. The decoder backend owns codec buffers,
//! their synchronization and any host-native plugin resources.

use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoCodec {
    H264,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    Nv12,
    I420,
    Bgra,
}

/// Chroma sample alignment relative to luma samples. Unknown means no
/// location was negotiated; Other preserves unusual GStreamer flag sets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChromaSiting {
    Unknown,
    Jpeg,
    Mpeg2,
    Dv,
    Cosited,
    AlternateLine,
    Other(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorMatrix {
    Bt601,
    Bt709,
    Bt2020,
    Rgb,
    Fcc,
    Smpte240m,
    /// No matrix was specified by the decoded stream.
    Unknown,
    /// Unrecognized GStreamer numeric value, distinct from unspecified.
    Other(i32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorPrimaries {
    Bt709,
    Ebu3213,
    SmpteC,
    Bt470m,
    Bt470bg,
    Smpte170m,
    Smpte240m,
    Film,
    Bt2020,
    AdobeRgb,
    Smptest428,
    Smpterp431,
    Smpteeg432,
    Unknown,
    Other(i32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorTransfer {
    Bt709,
    Srgb,
    Bt601,
    Smpte240m,
    Gamma10,
    Gamma18,
    Gamma20,
    Gamma22,
    Gamma28,
    Log100,
    Log316,
    Bt202010,
    Bt202012,
    AdobeRgb,
    /// SMPTE ST 2084 perceptual quantizer (PQ).
    Smpte2084,
    /// ARIB STD-B67 hybrid log-gamma (HLG).
    AribStdB67,
    Unknown,
    Other(i32),
}

/// A decode-order sample identifier is not a timestamp. Never recover it from
/// PTS or DTS: B-frame reordering, repeated timestamps and clock conversion
/// can all make timestamps non-unique.
#[derive(Clone, Copy)]
pub struct AccessUnit<'a> {
    pub id: u64,
    pub bytes: &'a [u8],
    /// Signed nanoseconds on the caller's media timeline. No implicit clamp
    /// or wrap to GStreamer's unsigned clock is allowed.
    pub pts_ns: Option<i64>,
    pub dts_ns: Option<i64>,
    /// A duration cannot be negative.
    pub duration_ns: Option<u64>,
}

/// A decoder-provided visible subrectangle of a physically larger frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameCrop {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// Physical field organization of the decoded video buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameInterlaceMode {
    Progressive,
    Interleaved,
}

/// Display order for interlaced fields. Unknown is distinct from progressive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameFieldOrder {
    Unknown,
    TopFirst,
    BottomFirst,
}

#[derive(Clone, Debug)]
pub struct FrameLayout {
    pub width: usize,
    pub height: usize,
    pub format: PixelFormat,
    pub strides: Vec<usize>,
    pub offsets: Vec<usize>,
    /// None means the full decoded dimensions are visible.
    pub crop: Option<FrameCrop>,
    /// Pixel (sample) aspect ratio, reduced to a positive numerator/denominator.
    pub pixel_aspect_ratio: (u32, u32),
    /// Chroma cositing from decoded output caps, distinct from colorimetry.
    pub chroma_siting: ChromaSiting,
    /// Per-frame: a stream mixing interlaced and progressive pictures reports
    /// each one as it is. Modes requiring field-specific planes fail at
    /// decode instead of being represented as incomplete frame layouts.
    pub interlace_mode: FrameInterlaceMode,
    /// First field in display order. Unknown for progressive frames unless
    /// repeat_first_field is set, since the repeated field has a parity.
    pub field_order: FrameFieldOrder,
    /// Repeat the first field in presentation, as specified by the buffer.
    pub repeat_first_field: bool,
    /// Only the first field is valid; the other field must be ignored.
    pub one_field: bool,
    pub matrix: ColorMatrix,
    pub primaries: ColorPrimaries,
    pub transfer: ColorTransfer,
    /// None means GStreamer did not establish a usable color range.
    pub full_range: Option<bool>,
}

impl FrameLayout {
    /// Minimum contiguous mapped span starting at a plane's offset, including
    /// padding between rows but not padding after the last active row.
    pub fn plane_byte_span(&self, plane: usize) -> Option<usize> {
        let chroma_width = self.width.div_ceil(2);
        let chroma_height = self.height.div_ceil(2);
        let (row_bytes, rows) = match (self.format, plane) {
            (PixelFormat::Nv12, 0) | (PixelFormat::I420, 0) => (self.width, self.height),
            (PixelFormat::Nv12, 1) => (chroma_width.checked_mul(2)?, chroma_height),
            (PixelFormat::I420, 1 | 2) => (chroma_width, chroma_height),
            (PixelFormat::Bgra, 0) => (self.width.checked_mul(4)?, self.height),
            _ => return None,
        };
        let stride = *self.strides.get(plane)?;
        if rows == 0 || row_bytes == 0 || stride < row_bytes {
            return None;
        }
        (rows - 1).checked_mul(stride)?.checked_add(row_bytes)
    }
}

#[derive(Debug)]
pub enum Error {
    UnsupportedCodec,
    BackendUnavailable(String),
    InvalidInput(String),
    Decode(String),
    /// Caller must consume available output and retry this unconsumed input.
    Backpressure,
    EndOfStream,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

/// The returned frame owns its host allocation until the consumer finishes
/// copying/presenting it. `with_bytes` gives access to the actual decoder
/// buffer without allocating an intermediate Rust Vec for every plane.
pub trait VideoFrame {
    fn id(&self) -> u64;
    fn layout(&self) -> &FrameLayout;
    /// Decoded presentation and decode times on the source media timeline,
    /// not sample identifiers. These may be absent, repeated or reordered.
    /// When a compressed sample has neither source PTS nor DTS, the GStreamer
    /// backend suppresses generated output clock values rather than treating
    /// them as if they were provided on the caller's signed timeline.
    fn pts_ns(&self) -> Option<i64>;
    fn dts_ns(&self) -> Option<i64>;
    fn duration_ns(&self) -> Option<u64>;
    /// Exact original compressed sample timeline, keyed by sample identity.
    /// Decoders may legitimately omit their output DTS; source DTS remains
    /// independently recoverable and is not fabricated as output DTS.
    fn source_pts_ns(&self) -> Option<i64>;
    fn source_dts_ns(&self) -> Option<i64>;
    fn with_bytes<R>(&self, read: impl FnOnce(&[u8], &FrameLayout) -> R) -> Result<R, Error>;
}

/// A submitted sample that will never be returned as a frame.
#[derive(Debug)]
pub struct DroppedSample {
    pub id: u64,
    /// None when the decoder discarded the sample (for example a corrupt or
    /// pre-recovery picture) or folded it into another picture.
    pub error: Option<Error>,
}

/// An incremental decode producer/consumer. Submission need not produce an
/// immediately available picture when the compressed stream uses reordering.
/// The caller must not interpret an empty poll as a dropped input sample;
/// every accepted sample id is eventually returned exactly once, either as a
/// frame by `pull` or by `take_dropped`.
pub trait VideoDecoder {
    type Frame: VideoFrame;

    fn codec(&self) -> VideoCodec;
    fn submit(&mut self, packet: AccessUnit<'_>) -> Result<(), Error>;
    /// An error is fatal to the session. A decoded frame that cannot be
    /// returned is reported through `take_dropped` with its error instead,
    /// and this returns None.
    fn pull(&mut self, timeout: Duration) -> Result<Option<Self::Frame>, Error>;
    /// The next sample known never to produce a frame. A drop becomes known
    /// once the decoder outputs a later frame or reaches end of stream, so
    /// poll this alongside `pull`.
    fn take_dropped(&mut self) -> Option<DroppedSample>;
    /// Terminal stream completion, not a VideoToolbox-compatible mid-GOP wait.
    fn end_stream(&mut self) -> Result<(), Error>;
    /// True only after a terminal end-of-stream has passed through all
    /// buffered output. A temporary empty poll is not equivalent to EOS.
    /// Returns an error if decoding failed, including a previously unobserved
    /// fatal bus error. True means successful terminal EOS.
    fn is_drained(&mut self) -> Result<bool, Error>;
}

#[cfg(feature = "gstreamer")]
pub mod gstreamer;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_identity_is_separate_from_presentation_time() {
        let sample = AccessUnit {
            id: 729,
            bytes: &[0x65, 0x01],
            pts_ns: Some(0),
            dts_ns: Some(1_000_000),
            duration_ns: None,
        };
        assert_eq!(sample.id, 729);
        assert_eq!(sample.pts_ns, Some(0));
    }
}
