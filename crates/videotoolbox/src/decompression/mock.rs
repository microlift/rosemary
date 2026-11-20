//! A `mediabackend` decoder for tests: it reads which picture a sample is
//! from the sample itself, holds pictures back to reorder them, and fails on
//! request. It goes behind the same adapter the real decoders do.
//!
//! A sample's last NAL unit is `[header, display index]`. The picture for
//! display index `n` has luma `luma(n)` everywhere and the configured chroma.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use mediabackend::{
    AccessUnit, ChromaSiting, ColorMatrix, ColorPrimaries, ColorTransfer, DroppedSample, Error,
    FrameFieldOrder, FrameInterlaceMode, FrameLayout, PixelFormat, VideoCodec, VideoDecoder,
    VideoFrame,
};

use super::backend::{Adapter, Backend, Decoder, Stream};

#[derive(Clone, Copy)]
pub(crate) struct Colour {
    pub full_range: Option<bool>,
    pub matrix: ColorMatrix,
    pub primaries: ColorPrimaries,
    pub transfer: ColorTransfer,
}

impl Colour {
    pub const UNSPECIFIED: Colour = Colour {
        full_range: None,
        matrix: ColorMatrix::Unknown,
        primaries: ColorPrimaries::Unknown,
        transfer: ColorTransfer::Unknown,
    };
}

#[derive(Clone)]
pub(crate) struct MockBackend {
    pub name: &'static str,
    pub hardware: bool,
    /// How many pictures are held back before the next comes out.
    pub delay: usize,
    /// The sample whose submission is invalid input.
    pub bad_data_at: Option<u64>,
    /// The sample whose submission breaks the decoder, and whether the
    /// backend is to blame.
    pub malfunction_at: Option<(u64, bool)>,
    /// The sample the decoder takes and never gives a picture for.
    pub lose_at: Option<u64>,
    pub colour: Colour,
    /// Cb and Cr of every picture.
    pub chroma: (u8, u8),
    /// A luma for every picture instead of one from its display index.
    pub luma: Option<u8>,
    /// Emit I420 rather than NV12.
    pub i420: bool,
    pub unhealthy: &'static AtomicBool,
    /// The size of the pictures.
    pub size: (usize, usize),
}

/// The luma of the picture shown `n`th.
pub(crate) fn luma(n: u64) -> u8 {
    (16 + 20 * n).min(235) as u8
}

impl MockBackend {
    pub fn new(name: &'static str) -> MockBackend {
        MockBackend {
            name,
            hardware: true,
            delay: 0,
            bad_data_at: None,
            malfunction_at: None,
            lose_at: None,
            colour: Colour::UNSPECIFIED,
            chroma: (90, 170),
            luma: None,
            i420: false,
            unhealthy: Box::leak(Box::new(AtomicBool::new(false))),
            size: (64, 64),
        }
    }

    pub fn software(self) -> MockBackend {
        MockBackend {
            hardware: false,
            ..self
        }
    }

    pub fn leak(self) -> &'static MockBackend {
        Box::leak(Box::new(self))
    }
}

impl Backend for MockBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    fn is_hardware(&self) -> bool {
        self.hardware
    }

    fn available(&self) -> Result<(), String> {
        match self.unhealthy.load(Ordering::Relaxed) {
            true => Err("unhealthy".to_string()),
            false => Ok(()),
        }
    }

    fn open(&self, _stream: &Stream) -> Result<Box<dyn Decoder>, Error> {
        Ok(Box::new(Adapter(MockDecoder {
            backend: self.clone(),
            held: Vec::new(),
            ready: VecDeque::new(),
            lost: Vec::new(),
            dropped: VecDeque::new(),
            ended: false,
        })))
    }

    fn set_unhealthy(&self) {
        self.unhealthy.store(true, Ordering::Relaxed);
    }
}

pub(crate) struct MockFrame {
    id: u64,
    layout: FrameLayout,
    bytes: Vec<u8>,
}

impl VideoFrame for MockFrame {
    fn id(&self) -> u64 {
        self.id
    }

    fn layout(&self) -> &FrameLayout {
        &self.layout
    }

    fn pts_ns(&self) -> Option<i64> {
        None
    }

    fn dts_ns(&self) -> Option<i64> {
        None
    }

    fn duration_ns(&self) -> Option<u64> {
        None
    }

    fn source_pts_ns(&self) -> Option<i64> {
        None
    }

    fn source_dts_ns(&self) -> Option<i64> {
        None
    }

    fn with_bytes<R>(&self, read: impl FnOnce(&[u8], &FrameLayout) -> R) -> Result<R, Error> {
        Ok(read(&self.bytes, &self.layout))
    }
}

struct MockDecoder {
    backend: MockBackend,
    /// `(display index, id)` of each picture not yet out.
    held: Vec<(u64, u64)>,
    ready: VecDeque<MockFrame>,
    lost: Vec<u64>,
    dropped: VecDeque<DroppedSample>,
    ended: bool,
}

impl MockDecoder {
    fn picture(&self, display: u64, id: u64) -> MockFrame {
        let (w, h) = self.backend.size;
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (cb, cr) = self.backend.chroma;
        let mut bytes = vec![self.backend.luma.unwrap_or(luma(display)); w * h];
        let (format, strides, offsets) = match self.backend.i420 {
            true => {
                bytes.extend(std::iter::repeat_n(cb, cw * ch));
                bytes.extend(std::iter::repeat_n(cr, cw * ch));
                (
                    PixelFormat::I420,
                    vec![w, cw, cw],
                    vec![0, w * h, w * h + cw * ch],
                )
            }
            false => {
                bytes.extend((0..cw * ch).flat_map(|_| [cb, cr]));
                (PixelFormat::Nv12, vec![w, 2 * cw], vec![0, w * h])
            }
        };
        let colour = self.backend.colour;
        MockFrame {
            id,
            bytes,
            layout: FrameLayout {
                width: w,
                height: h,
                format,
                strides,
                offsets,
                crop: None,
                pixel_aspect_ratio: (1, 1),
                chroma_siting: ChromaSiting::Unknown,
                interlace_mode: FrameInterlaceMode::Progressive,
                field_order: FrameFieldOrder::Unknown,
                repeat_first_field: false,
                one_field: false,
                matrix: colour.matrix,
                primaries: colour.primaries,
                transfer: colour.transfer,
                full_range: colour.full_range,
            },
        }
    }

    /// Output the earliest-shown pictures until at most `keep` are held.
    fn release(&mut self, keep: usize) {
        self.held.sort();
        while self.held.len() > keep {
            let (display, id) = self.held.remove(0);
            let frame = self.picture(display, id);
            self.ready.push_back(frame);
        }
    }
}

/// The display index a mock sample carries: the second byte of its last
/// 4-byte-length NAL unit.
fn display_index(sample: &[u8]) -> Option<u64> {
    let mut rest = sample;
    let mut last = None;
    while !rest.is_empty() {
        let (length, after) = rest.split_at_checked(4)?;
        let length = u32::from_be_bytes(length.try_into().ok()?) as usize;
        let (unit, after) = after.split_at_checked(length)?;
        last = Some(unit);
        rest = after;
    }
    last?.get(1).map(|&n| u64::from(n))
}

impl VideoDecoder for MockDecoder {
    type Frame = MockFrame;

    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }

    fn submit(&mut self, packet: AccessUnit<'_>) -> Result<(), Error> {
        if self.ended {
            return Err(Error::EndOfStream);
        }
        if let Some((at, global)) = self.backend.malfunction_at
            && at == packet.id
        {
            return Err(match global {
                true => Error::BackendUnavailable("the mock broke".into()),
                false => Error::Decode("the mock broke on this stream".into()),
            });
        }
        if self.backend.bad_data_at == Some(packet.id) {
            return Err(Error::InvalidInput("the mock refuses this sample".into()));
        }
        if self.backend.lose_at == Some(packet.id) {
            self.lost.push(packet.id);
            return Ok(());
        }
        let display = display_index(packet.bytes)
            .ok_or_else(|| Error::InvalidInput("not a mock sample".into()))?;
        self.held.push((display, packet.id));
        self.release(self.backend.delay);
        Ok(())
    }

    fn pull(&mut self, _timeout: Duration) -> Result<Option<MockFrame>, Error> {
        Ok(self.ready.pop_front())
    }

    fn take_dropped(&mut self) -> Option<DroppedSample> {
        self.dropped.pop_front()
    }

    fn end_stream(&mut self) -> Result<(), Error> {
        self.ended = true;
        self.release(0);
        for id in self.lost.drain(..) {
            self.dropped.push_back(DroppedSample { id, error: None });
        }
        Ok(())
    }

    fn is_drained(&mut self) -> Result<bool, Error> {
        Ok(self.ended && self.ready.is_empty())
    }
}
