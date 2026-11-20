//! Which decoder is behind a decompression session.
//!
//! The decoders are `mediabackend`'s: FFmpeg over Vulkan Video, VA-API or
//! CUDA, and GStreamer's software decoder when built with the `gstreamer`
//! feature. A session opens one [`Decoder`] from the first backend in the
//! [`Registry`] that its decoder specification allows and that takes the
//! stream. `ROSEMARY_VIDEO_DECODERS` orders them; see `docs/usage.md`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use mediabackend::{AccessUnit, Error, FrameLayout, VideoDecoder, VideoFrame};

/// The backends tried when `ROSEMARY_VIDEO_DECODERS` is not set.
#[cfg(not(feature = "gstreamer"))]
const DEFAULT_ORDER: &str = "vulkan,vaapi,cuda";
#[cfg(feature = "gstreamer")]
const DEFAULT_ORDER: &str = "vulkan,vaapi,cuda,gstreamer";

/// How long a decoder that decodes on threads of its own is waited for:
/// for room to take a sample, and for its last pictures at end of stream.
const BACKPRESSURE_WAIT: Duration = Duration::from_millis(500);
const DRAIN_WAIT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(5);

/// What a decoder is opened for: the `avcC` stream of a format description.
pub(crate) struct Stream<'a> {
    /// SPSs, then PPSs, as NAL units.
    pub parameter_sets: &'a [Vec<u8>],
    pub nal_length: u8,
}

/// A decoded picture, borrowed until the sink returns.
pub(crate) struct Picture<'a> {
    pub id: u64,
    pub bytes: &'a [u8],
    pub layout: &'a FrameLayout,
}

/// What a decoder hands the session.
pub(crate) enum Output<'a> {
    Picture(Picture<'a>),
    /// A sample that will never be a picture.
    Dropped(u64),
}

/// A `mediabackend` decoder, whichever it is. The trait there has an
/// associated frame type, so a session holds one through this.
pub(crate) trait Decoder: Send {
    /// Decode one `avcC` sample. A decoder with no room is drained into
    /// `sink` until it takes the sample.
    fn submit(
        &mut self,
        unit: AccessUnit<'_>,
        sink: &mut dyn FnMut(Output<'_>),
    ) -> Result<(), Error>;
    /// Everything the decoder has ready now.
    fn collect(&mut self, sink: &mut dyn FnMut(Output<'_>)) -> Result<(), Error>;
    /// End the stream and hand on every picture still held. Terminal: the
    /// session opens another decoder to go on.
    fn finish(&mut self, sink: &mut dyn FnMut(Output<'_>)) -> Result<(), Error>;
}

/// Any [`VideoDecoder`] as a [`Decoder`].
pub(crate) struct Adapter<D>(pub D);

impl<D: VideoDecoder + Send> Adapter<D> {
    fn drain(
        &mut self,
        timeout: Duration,
        sink: &mut dyn FnMut(Output<'_>),
    ) -> Result<bool, Error> {
        // Pictures before drops: a picture ready now was decoded before any
        // sample the decoder has given up on since.
        let mut any = false;
        loop {
            let wait = if any { Duration::ZERO } else { timeout };
            let Some(frame) = self.0.pull(wait)? else {
                break;
            };
            frame.with_bytes(|bytes, layout| {
                sink(Output::Picture(Picture {
                    id: frame.id(),
                    bytes,
                    layout,
                }))
            })?;
            any = true;
        }
        while let Some(dropped) = self.0.take_dropped() {
            sink(Output::Dropped(dropped.id));
            any = true;
        }
        Ok(any)
    }
}

impl<D: VideoDecoder + Send> Decoder for Adapter<D> {
    fn submit(
        &mut self,
        unit: AccessUnit<'_>,
        sink: &mut dyn FnMut(Output<'_>),
    ) -> Result<(), Error> {
        let deadline = Instant::now() + BACKPRESSURE_WAIT;
        loop {
            match self.0.submit(unit) {
                Err(Error::Backpressure) if Instant::now() < deadline => {
                    self.drain(POLL, sink)?;
                }
                result => return result,
            }
        }
    }

    fn collect(&mut self, sink: &mut dyn FnMut(Output<'_>)) -> Result<(), Error> {
        self.drain(Duration::ZERO, sink).map(|_| ())
    }

    fn finish(&mut self, sink: &mut dyn FnMut(Output<'_>)) -> Result<(), Error> {
        self.0.end_stream()?;
        let deadline = Instant::now() + DRAIN_WAIT;
        loop {
            self.drain(POLL, sink)?;
            if self.0.is_drained()? || Instant::now() >= deadline {
                return Ok(());
            }
        }
    }
}

pub(crate) trait Backend: Sync {
    fn name(&self) -> &'static str;
    fn is_hardware(&self) -> bool;
    /// Whether the backend works here.
    fn available(&self) -> Result<(), String>;
    fn open(&self, stream: &Stream) -> Result<Box<dyn Decoder>, Error>;
    /// Stop offering this backend for the rest of the process, after it
    /// failed in a way no stream causes.
    fn set_unhealthy(&self);
    /// How a session announces it.
    fn description(&self) -> String {
        self.name().to_string()
    }
}

/// An FFmpeg hardware API.
pub(crate) struct Ffmpeg {
    hardware: &'static mediabackend::ffmpeg::Hardware,
    unhealthy: AtomicBool,
}

impl Ffmpeg {
    pub(crate) const fn new(hardware: &'static mediabackend::ffmpeg::Hardware) -> Ffmpeg {
        Ffmpeg {
            hardware,
            unhealthy: AtomicBool::new(false),
        }
    }
}

pub(crate) static VULKAN: Ffmpeg = Ffmpeg::new(&mediabackend::ffmpeg::VULKAN);
pub(crate) static VAAPI: Ffmpeg = Ffmpeg::new(&mediabackend::ffmpeg::VAAPI);
pub(crate) static CUDA: Ffmpeg = Ffmpeg::new(&mediabackend::ffmpeg::CUDA);

const UNHEALTHY: &str = "it failed earlier in this process";

fn sets<'a>(stream: &Stream<'a>) -> Vec<&'a [u8]> {
    stream.parameter_sets.iter().map(Vec::as_slice).collect()
}

impl Backend for Ffmpeg {
    fn name(&self) -> &'static str {
        self.hardware.name()
    }

    fn is_hardware(&self) -> bool {
        true
    }

    fn available(&self) -> Result<(), String> {
        if self.unhealthy.load(Ordering::Relaxed) {
            return Err(UNHEALTHY.to_string());
        }
        self.hardware.probe()
    }

    fn open(&self, stream: &Stream) -> Result<Box<dyn Decoder>, Error> {
        let decoder = self.hardware.h264_avcc(&sets(stream), stream.nal_length)?;
        Ok(Box::new(Adapter(decoder)))
    }

    fn set_unhealthy(&self) {
        self.unhealthy.store(true, Ordering::Relaxed);
    }

    fn description(&self) -> String {
        self.hardware.description()
    }
}

/// GStreamer's `avdec_h264`, a software decoder.
#[cfg(feature = "gstreamer")]
pub(crate) struct Gstreamer {
    unhealthy: AtomicBool,
}

#[cfg(feature = "gstreamer")]
pub(crate) static GSTREAMER: Gstreamer = Gstreamer {
    unhealthy: AtomicBool::new(false),
};

/// libav picks its own thread count.
#[cfg(feature = "gstreamer")]
const GSTREAMER_THREADS: u32 = 0;

#[cfg(feature = "gstreamer")]
impl Backend for Gstreamer {
    fn name(&self) -> &'static str {
        "gstreamer"
    }

    fn is_hardware(&self) -> bool {
        false
    }

    fn available(&self) -> Result<(), String> {
        match self.unhealthy.load(Ordering::Relaxed) {
            true => Err(UNHEALTHY.to_string()),
            false => Ok(()),
        }
    }

    fn open(&self, stream: &Stream) -> Result<Box<dyn Decoder>, Error> {
        let decoder = mediabackend::gstreamer::Decoder::h264_avcc_with_output(
            &sets(stream),
            stream.nal_length,
            GSTREAMER_THREADS,
            mediabackend::PixelFormat::Nv12,
        )?;
        Ok(Box::new(Adapter(decoder)))
    }

    fn set_unhealthy(&self) {
        self.unhealthy.store(true, Ordering::Relaxed);
    }

    fn description(&self) -> String {
        "GStreamer's avdec_h264 (software)".to_string()
    }
}

/// Which kinds of backend a decoder specification allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HwPolicy {
    pub hardware: bool,
    pub software: bool,
}

impl HwPolicy {
    pub const ANY: HwPolicy = HwPolicy {
        hardware: true,
        software: true,
    };

    fn allows(&self, backend: &dyn Backend) -> bool {
        match backend.is_hardware() {
            true => self.hardware,
            false => self.software,
        }
    }
}

/// The backends a session may use, in the order they are tried.
pub(crate) struct Registry {
    backends: Vec<&'static dyn Backend>,
}

/// The names `ROSEMARY_VIDEO_DECODERS` takes, for the message about one it
/// does not.
#[cfg(not(feature = "gstreamer"))]
const NAMES: &str = "vulkan, vaapi, cuda and none";
#[cfg(feature = "gstreamer")]
const NAMES: &str = "vulkan, vaapi, cuda, gstreamer and none";

/// The backend a name stands for. `Some(None)` for a name that stands for
/// nothing on purpose, or for one this build leaves out.
fn named(name: &str) -> Option<Option<&'static dyn Backend>> {
    Some(Some(match name {
        "vulkan" => &VULKAN,
        "vaapi" => &VAAPI,
        "cuda" => &CUDA,
        #[cfg(feature = "gstreamer")]
        "gstreamer" => &GSTREAMER,
        #[cfg(not(feature = "gstreamer"))]
        "gstreamer" => {
            report_once(
                "rosemary: VideoToolbox: ROSEMARY_VIDEO_DECODERS names gstreamer, which this \
                 build leaves out (the videotoolbox crate's gstreamer feature)"
                    .to_string(),
            );
            return Some(None);
        }
        "none" => return Some(None),
        _ => return None,
    }))
}

/// The backends a comma-separated list names, without repeats, and the
/// names in it that stand for none.
pub(crate) fn parse_names(list: &str) -> (Vec<&'static dyn Backend>, Vec<String>) {
    let mut backends: Vec<&'static dyn Backend> = Vec::new();
    let mut unknown = Vec::new();
    for name in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        match named(&name.to_ascii_lowercase()) {
            Some(Some(backend)) => {
                if !backends.iter().any(|b| b.name() == backend.name()) {
                    backends.push(backend);
                }
            }
            Some(None) => {}
            None => unknown.push(name.to_string()),
        }
    }
    (backends, unknown)
}

impl Registry {
    pub fn new(backends: Vec<&'static dyn Backend>) -> Registry {
        Registry { backends }
    }

    /// The process's registry, from `ROSEMARY_VIDEO_DECODERS`.
    pub fn global() -> &'static Registry {
        static GLOBAL: OnceLock<Registry> = OnceLock::new();
        GLOBAL.get_or_init(|| {
            let list = std::env::var("ROSEMARY_VIDEO_DECODERS")
                .ok()
                .filter(|v| !v.trim().is_empty());
            Registry::from_names(list.as_deref().unwrap_or(DEFAULT_ORDER))
        })
    }

    /// The backends `list` names, in its order. Unknown names are reported
    /// and skipped.
    pub fn from_names(list: &str) -> Registry {
        let (backends, unknown) = parse_names(list);
        for name in unknown {
            report_once(format!(
                "rosemary: VideoToolbox: ROSEMARY_VIDEO_DECODERS names no decoder `{name}`; \
                 the names are {NAMES}"
            ));
        }
        Registry::new(backends)
    }

    #[cfg(test)]
    pub fn backends(&self) -> &[&'static dyn Backend] {
        &self.backends
    }

    /// A decoder for `stream` from the first backend `policy` allows that
    /// works and takes it, or every backend's reason it did not.
    pub fn open(
        &self,
        stream: &Stream,
        policy: HwPolicy,
    ) -> Result<(Box<dyn Decoder>, &'static dyn Backend), String> {
        let mut reasons = Vec::new();
        for &backend in &self.backends {
            if !policy.allows(backend) {
                continue;
            }
            let opened = backend
                .available()
                .and_then(|()| backend.open(stream).map_err(|e| describe(&e)));
            match opened {
                Ok(decoder) => return Ok((decoder, backend)),
                Err(why) => reasons.push(format!("{}: {why}", backend.name())),
            }
        }
        if reasons.is_empty() {
            reasons.push(match self.backends.is_empty() {
                true => "none is enabled (ROSEMARY_VIDEO_DECODERS)".to_string(),
                false => "the decoder specification allows none of the enabled ones".to_string(),
            });
        }
        Err(reasons.join("; "))
    }

    /// `VTIsHardwareDecodeSupported`: whether a hardware backend works.
    pub fn hardware_available(&self) -> bool {
        self.backends
            .iter()
            .any(|b| b.is_hardware() && b.available().is_ok())
    }
}

/// A backend error, without its variant's name.
pub(crate) fn describe(error: &Error) -> String {
    match error {
        Error::BackendUnavailable(why) | Error::InvalidInput(why) | Error::Decode(why) => {
            why.clone()
        }
        Error::UnsupportedCodec => "the codec is not supported".to_string(),
        Error::Backpressure => "the decoder takes no more input".to_string(),
        Error::EndOfStream => "the decoder's stream has ended".to_string(),
    }
}

/// Print `message` on stderr, unless it has been already.
pub(crate) fn report_once(message: String) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    let fresh = seen
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(message.clone());
    if fresh {
        eprintln!("{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::super::mock::MockBackend;
    use super::*;

    fn names(registry: &Registry) -> Vec<&'static str> {
        registry.backends().iter().map(|b| b.name()).collect()
    }

    fn probe_stream() -> Vec<Vec<u8>> {
        mediabackend::ffmpeg::probe_samples().0
    }

    #[test]
    fn registry_order_comes_from_the_list() {
        assert_eq!(
            names(&Registry::from_names("cuda, vulkan,cuda,VAAPI")),
            ["cuda", "vulkan", "vaapi"]
        );
        assert_eq!(
            names(&Registry::from_names(DEFAULT_ORDER)),
            [
                "vulkan",
                "vaapi",
                "cuda",
                #[cfg(feature = "gstreamer")]
                "gstreamer"
            ]
        );
    }

    #[test]
    fn none_gives_an_empty_registry() {
        let registry = Registry::from_names("none");
        assert!(registry.backends().is_empty());
        let sets = probe_stream();
        let stream = Stream {
            parameter_sets: &sets,
            nal_length: 4,
        };
        let why = registry.open(&stream, HwPolicy::ANY).err().unwrap();
        assert!(why.contains("none is enabled"), "{why}");
        assert!(!registry.hardware_available());
    }

    #[test]
    fn unknown_names_are_reported_and_skipped() {
        let (backends, unknown) = parse_names("vulkan,nvdec,,none,cuda");
        let names: Vec<_> = backends.iter().map(|b| b.name()).collect();
        assert_eq!(names, ["vulkan", "cuda"]);
        assert_eq!(unknown, ["nvdec"]);
        #[cfg(not(feature = "gstreamer"))]
        assert!(parse_names("gstreamer").0.is_empty() && parse_names("gstreamer").1.is_empty());
    }

    #[test]
    fn require_hardware_excludes_software() {
        let software = MockBackend::new("soft").software().leak();
        let hardware = MockBackend::new("hard").leak();
        let registry = Registry::new(vec![software, hardware]);
        let sets = probe_stream();
        let stream = Stream {
            parameter_sets: &sets,
            nal_length: 4,
        };
        let only_hardware = HwPolicy {
            hardware: true,
            software: false,
        };
        let (_, chosen) = registry.open(&stream, only_hardware).ok().unwrap();
        assert_eq!(chosen.name(), "hard");
        let (_, chosen) = registry.open(&stream, HwPolicy::ANY).ok().unwrap();
        assert_eq!(chosen.name(), "soft", "in list order otherwise");
        let software_only = Registry::new(vec![software]);
        assert!(software_only.open(&stream, only_hardware).is_err());
        assert!(registry.hardware_available());
        assert!(!software_only.hardware_available());
    }

    #[test]
    fn an_unhealthy_backend_is_passed_over() {
        let first = MockBackend::new("first").leak();
        let second = MockBackend::new("second").leak();
        let registry = Registry::new(vec![first, second]);
        let sets = probe_stream();
        let stream = Stream {
            parameter_sets: &sets,
            nal_length: 4,
        };
        first.set_unhealthy();
        let (_, chosen) = registry.open(&stream, HwPolicy::ANY).ok().unwrap();
        assert_eq!(chosen.name(), "second");
    }

    #[test]
    fn a_missing_ffmpeg_is_unavailable_not_fatal() {
        let backend: &'static Ffmpeg = Box::leak(Box::new(Ffmpeg::new(
            mediabackend::ffmpeg::Hardware::bogus(),
        )));
        let why = backend.available().unwrap_err();
        assert!(why.contains("not installed"), "{why}");
        let registry = Registry::new(vec![backend]);
        assert!(!registry.hardware_available());
        let sets = probe_stream();
        let stream = Stream {
            parameter_sets: &sets,
            nal_length: 4,
        };
        assert!(registry.open(&stream, HwPolicy::ANY).is_err());
    }
}
