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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorMatrix {
    Bt601,
    Bt709,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorPrimaries {
    Bt709,
    Ebu3213,
    SmpteC,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorTransfer {
    Bt709,
    Srgb,
    Unknown,
}

/// A decode-order sample identifier is not a timestamp. Never recover it from
/// PTS or DTS: B-frame reordering, repeated timestamps and clock conversion
/// can all make timestamps non-unique.
#[derive(Clone, Copy)]
pub struct AccessUnit<'a> {
    pub id: u64,
    pub bytes: &'a [u8],
    pub pts_ns: Option<u64>,
    pub dts_ns: Option<u64>,
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

#[derive(Clone, Debug)]
pub struct FrameLayout {
    pub width: usize,
    pub height: usize,
    pub format: PixelFormat,
    pub strides: Vec<usize>,
    pub offsets: Vec<usize>,
    /// None means the full decoded dimensions are visible.
    pub crop: Option<FrameCrop>,
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
    /// Presentation, decoding and duration times as reported on decoded output.
    /// These are not sample identifiers and may be absent or repeated.
    fn pts_ns(&self) -> Option<u64>;
    fn dts_ns(&self) -> Option<u64>;
    fn duration_ns(&self) -> Option<u64>;
    fn with_bytes<R>(&self, read: impl FnOnce(&[u8], &FrameLayout) -> R) -> Result<R, Error>;
}

/// An incremental decode producer/consumer. Submission need not produce an
/// immediately available picture when the compressed stream uses reordering.
/// The caller must not interpret an empty poll as a dropped input sample.
pub trait VideoDecoder {
    type Frame: VideoFrame;

    fn codec(&self) -> VideoCodec;
    fn submit(&mut self, packet: AccessUnit<'_>) -> Result<(), Error>;
    fn pull(&mut self, timeout: Duration) -> Result<Option<Self::Frame>, Error>;
    /// Terminal stream completion, not a VideoToolbox-compatible mid-GOP wait.
    fn end_stream(&mut self) -> Result<(), Error>;
    /// True only after a terminal end-of-stream has passed through all
    /// buffered output. A temporary empty poll is not equivalent to EOS.
    fn is_drained(&self) -> bool;
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
