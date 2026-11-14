//! FFmpeg's H.264 decoder over a hardware API: one backend each for Vulkan
//! Video, VA-API and CUDA (NVDEC).
//!
//! FFmpeg 8 is loaded with `dlopen` (see the `ffmpeg` crate), never linked,
//! so a host without it only loses these backends. FFmpeg keeps the reference
//! pictures and reorders; this side feeds it Annex B access units, refuses its
//! software fallback (a hardware backend that quietly decodes on the CPU is
//! not what was asked for), and copies each picture back to main memory as
//! NV12.
//!
//! Decoding is synchronous: whatever a submitted access unit lets FFmpeg
//! output is ready to `pull` as soon as `submit` returns, so a caller that has
//! to deliver frames on its own thread never waits on a timeout.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::{CString, c_int, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ffmpeg::abi;
use ffmpeg::api::{self, Api};

use crate::{
    AccessUnit, ChromaSiting, ColorMatrix, ColorPrimaries, ColorTransfer, DroppedSample, Error,
    FrameFieldOrder, FrameInterlaceMode, FrameLayout, PixelFormat, VideoCodec, VideoDecoder,
    VideoFrame,
};

mod bitstream;
#[cfg(test)]
mod tests;

pub use bitstream::split_annex_b;

type Ptr = *mut c_void;

/// The probe clip: High profile, 64x64, six frames coded `IDR P B B P B` and
/// shown `I B B P B P`. Shown frame `n` has luma `40 + 30n` everywhere, Cb 90
/// on the left half and 170 on the right, Cr 100 on the top half and 160 on
/// the bottom. Made with:
///
/// ```text
/// ffmpeg -f lavfi -i "nullsrc=s=64x64:r=30:d=0.2,format=yuv420p,geq=lum='40+N*30':cb='if(lt(X,32*SW),90,170)':cr='if(lt(Y,32*SH),100,160)'" -c:v libx264 -profile:v high -preset veryslow -qp 4 -x264-params "keyint=6:min-keyint=6:scenecut=0:b-pyramid=none:bframes=2:ref=2" -pix_fmt yuv420p -bsf:v h264_mp4toannexb,filter_units=remove_types=6 probe.h264
/// ```
pub const PROBE_CLIP: &[u8] = include_bytes!("probe.h264");

/// The display index of each of the probe clip's pictures, in coded order.
pub const PROBE_DISPLAY_ORDER: [u64; 6] = [0, 3, 1, 2, 5, 4];

/// More samples than H.264 can hold back (16 reference frames) waiting for a
/// picture means the oldest were discarded by the decoder.
const MAX_PENDING: usize = 32;

/// `profile_idc` of the profiles a hardware decoder is asked for: Baseline,
/// Main and High. High 10, 4:2:2 and 4:4:4 are not decoded by any of these
/// APIs on common hardware, and a stream the hardware refuses fails anyway.
const PROFILE_BASELINE: u8 = 66;
const PROFILE_MAIN: u8 = 77;
const PROFILE_HIGH: u8 = 100;
/// `chroma_format_idc` of 4:2:0.
const CHROMA_420: u32 = 1;

/// A hardware API under FFmpeg's H.264 decoder.
pub struct Hardware {
    /// FFmpeg's name for the device type, which is also the backend's.
    name: &'static str,
    api: fn() -> Result<&'static Api, &'static str>,
    device: OnceLock<Result<Device, String>>,
}

pub static VULKAN: Hardware = Hardware::new("vulkan");
pub static VAAPI: Hardware = Hardware::new("vaapi");
pub static CUDA: Hardware = Hardware::new("cuda");

/// A hardware device that decoded the probe clip, shared by every decoder on
/// this backend for the rest of the process.
struct Device {
    /// `AVBufferRef *` of the `AVHWDeviceContext`, never released.
    buffer: usize,
    /// `const AVCodec *` of FFmpeg's `h264` decoder.
    codec: usize,
    /// The hardware pixel format decoded pictures are in.
    hw_format: c_int,
    /// `AV_PIX_FMT_NV12`, which pictures are copied back as.
    nv12: c_int,
    /// Which device it is, for messages.
    label: String,
}

impl Hardware {
    const fn new(name: &'static str) -> Hardware {
        Hardware {
            name,
            api: api::global,
            device: OnceLock::new(),
        }
    }

    /// A backend whose library is never there, for testing what a host
    /// without FFmpeg gets.
    #[doc(hidden)]
    pub fn bogus() -> &'static Hardware {
        Box::leak(Box::new(Hardware {
            api: api::bogus,
            ..Hardware::new("vulkan")
        }))
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Whether this backend decodes here: FFmpeg is loaded, the device opens
    /// and it decoded the probe clip correctly. Tried once per process.
    pub fn probe(&self) -> Result<(), String> {
        self.device().map(|_| ())
    }

    /// What decodes, for a diagnostic.
    pub fn description(&self) -> String {
        match self.device() {
            Ok((api, device)) => format!(
                "FFmpeg (libavcodec {}) through {} on the {}",
                api.version(),
                self.name,
                device.label
            ),
            Err(_) => self.name.to_string(),
        }
    }

    /// A decoder for an `avcC` stream: `sets` are its SPSs and PPSs as NAL
    /// units, `nal_length` the size of each sample's NAL unit lengths. Samples
    /// are submitted as they are stored, length-prefixed.
    pub fn h264_avcc(&self, sets: &[&[u8]], nal_length: u8) -> Result<Decoder, Error> {
        if !matches!(nal_length, 1 | 2 | 4) {
            return Err(Error::InvalidInput(format!(
                "a NAL unit length of {nal_length} bytes"
            )));
        }
        let sps = sets
            .iter()
            .find(|set| set.first().is_some_and(|h| h & 0x1f == bitstream::NAL_SPS))
            .ok_or_else(|| Error::InvalidInput("no SPS".into()))?;
        check_stream(sps)?;
        let (api, device) = self.device().map_err(Error::BackendUnavailable)?;
        Decoder::new(api, device, sets, nal_length)
    }

    fn device(&self) -> Result<(&'static Api, &Device), String> {
        let api = (self.api)().map_err(str::to_string)?;
        let device = self.device.get_or_init(|| self.open_device(api));
        device.as_ref().map(|d| (api, d)).map_err(Clone::clone)
    }

    /// The device to decode on, tried on the probe clip.
    fn open_device(&self, api: &'static Api) -> Result<Device, String> {
        let name = CString::new(self.name).unwrap_or_default();
        // SAFETY: resolved entry points and NUL-terminated names.
        let (device_type, codec, nv12) = unsafe {
            (
                (api.av_hwdevice_find_type_by_name)(name.as_ptr()),
                (api.avcodec_find_decoder_by_name)(c"h264".as_ptr()),
                (api.av_get_pix_fmt)(c"nv12".as_ptr()),
            )
        };
        if device_type == abi::HWDEVICE_TYPE_NONE {
            return Err(format!("this FFmpeg has no {} support", self.name));
        }
        if codec.is_null() {
            return Err("this FFmpeg has no h264 decoder".to_string());
        }
        if nv12 == abi::PIX_FMT_NONE {
            return Err("this FFmpeg has no NV12".to_string());
        }
        let Some(hw_format) = hw_format(api, codec, device_type) else {
            return Err(format!(
                "FFmpeg's h264 decoder cannot use a {} device",
                self.name
            ));
        };
        let mut reasons = Vec::new();
        for node in candidates(self.name) {
            let label = match (&node, self.name) {
                (None, _) => "default device".to_string(),
                (Some(index), "vulkan") => format!("Vulkan device {index}"),
                (Some(path), _) => path.clone(),
            };
            let node = node.map(|n| CString::new(n).unwrap_or_default());
            let mut buffer: Ptr = std::ptr::null_mut();
            // SAFETY: an out-parameter, a device type FFmpeg named, and a
            // NUL-terminated device string or null for FFmpeg's choice.
            let status = unsafe {
                (api.av_hwdevice_ctx_create)(
                    &mut buffer,
                    device_type,
                    node.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
                    std::ptr::null_mut(),
                    0,
                )
            };
            if status < 0 || buffer.is_null() {
                if self.name == "vulkan" && node.is_some() {
                    // Indices past the last device.
                    break;
                }
                reasons.push(format!("{label}: no device (error {status})"));
                continue;
            }
            let device = Device {
                buffer: buffer as usize,
                codec: codec as usize,
                hw_format,
                nv12,
                label: label.clone(),
            };
            match probe_decode(api, &device) {
                Ok(()) => return Ok(device),
                Err(why) => {
                    // SAFETY: the reference this function made.
                    unsafe { (api.av_buffer_unref)(&mut buffer) };
                    reasons.push(format!("{label}: {why}"));
                }
            }
        }
        if reasons.is_empty() {
            reasons.push("no device to try".to_string());
        }
        Err(reasons.join(", "))
    }
}

/// The hardware pixel format FFmpeg's decoder produces on `device_type`.
fn hw_format(api: &Api, codec: *const c_void, device_type: c_int) -> Option<c_int> {
    for index in 0.. {
        // SAFETY: FFmpeg's decoder; a null entry ends the list.
        let config = unsafe { (api.avcodec_get_hw_config)(codec, index) };
        if config.is_null() {
            return None;
        }
        // SAFETY: an `AVCodecHWConfig` FFmpeg returned.
        let (pix_fmt, methods, kind) = unsafe {
            (
                abi::read::<c_int>(config, abi::hw_config::PIX_FMT),
                abi::read::<c_int>(config, abi::hw_config::METHODS),
                abi::read::<c_int>(config, abi::hw_config::DEVICE_TYPE),
            )
        };
        if methods & abi::METHOD_HW_DEVICE_CTX != 0 && kind == device_type {
            return Some(pix_fmt);
        }
    }
    None
}

/// How many Vulkan devices past FFmpeg's default are tried by index.
const MAX_VULKAN_DEVICES: usize = 8;

/// The device strings to try. For VA-API, each render node by its PCI path,
/// since the numbered nodes are not stable. For Vulkan, FFmpeg's choice and
/// then each device by index: the default is often an integrated GPU whose
/// driver has no video decode queue while another GPU's does.
fn candidates(name: &str) -> Vec<Option<String>> {
    match name {
        "vaapi" => {
            let mut nodes: Vec<String> = std::fs::read_dir("/dev/dri/by-path")
                .map(|dir| {
                    dir.filter_map(Result::ok)
                        .map(|entry| entry.path())
                        .filter(|path| {
                            path.file_name()
                                .and_then(|n| n.to_str())
                                .is_some_and(|n| n.ends_with("-render"))
                        })
                        .filter_map(|path| path.to_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            nodes.sort();
            nodes.into_iter().map(Some).collect()
        }
        "vulkan" => std::iter::once(None)
            .chain((0..MAX_VULKAN_DEVICES).map(|index| Some(index.to_string())))
            .collect(),
        _ => vec![None],
    }
}

/// The probe clip's parameter sets, and its slices as 4-byte `avcC` samples.
pub fn probe_samples() -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let units = split_annex_b(PROBE_CLIP);
    let (sets, slices) = units.split_at(2);
    let samples = slices
        .iter()
        .map(|slice| {
            let mut sample = (slice.len() as u32).to_be_bytes().to_vec();
            sample.extend_from_slice(slice);
            sample
        })
        .collect();
    (sets.iter().map(|s| s.to_vec()).collect(), samples)
}

/// Decode the probe clip on `device`: all six pictures, the first with the
/// luma it was made with.
fn probe_decode(api: &'static Api, device: &Device) -> Result<(), String> {
    let (sets, samples) = probe_samples();
    let sets: Vec<&[u8]> = sets.iter().map(Vec::as_slice).collect();
    let mut decoder = Decoder::new(api, device, &sets, 4).map_err(|e| e.to_string())?;
    let mut seen: Vec<(u64, u8)> = Vec::new();
    let mut collect = |decoder: &mut Decoder| -> Result<(), Error> {
        while let Some(frame) = decoder.pull(Duration::ZERO)? {
            let luma = frame.with_bytes(|bytes, _| bytes.first().copied().unwrap_or(0))?;
            seen.push((frame.id(), luma));
        }
        Ok(())
    };
    let refused = |decoder: &Decoder, why: Error| match decoder.refused() {
        true => "the driver cannot decode the probe's High profile stream".to_string(),
        false => format!("decoding failed ({why})"),
    };
    for (id, sample) in samples.iter().enumerate() {
        let unit = AccessUnit {
            id: id as u64,
            bytes: sample,
            pts_ns: None,
            dts_ns: None,
            duration_ns: None,
        };
        decoder
            .submit(unit)
            .and_then(|()| collect(&mut decoder))
            .map_err(|why| refused(&decoder, why))?;
    }
    decoder
        .end_stream()
        .and_then(|()| collect(&mut decoder))
        .map_err(|why| refused(&decoder, why))?;
    match seen.first() {
        _ if seen.len() != samples.len() => Err(format!(
            "{} of {} pictures came out",
            seen.len(),
            samples.len()
        )),
        Some(&(0, luma)) if luma.abs_diff(40) <= 2 => Ok(()),
        Some(&(id, luma)) => Err(format!(
            "the first picture came out wrong (access unit {id}, luma {luma}, not 40)"
        )),
        None => Err("no pictures came out".to_string()),
    }
}

/// Refuse what no hardware backend here decodes: anything but 8-bit 4:2:0
/// Baseline, Main or High.
fn check_stream(sps: &[u8]) -> Result<(), Error> {
    let fields = bitstream::sps_format(sps)
        .ok_or_else(|| Error::InvalidInput("the SPS does not parse".into()))?;
    let unsupported = |why: String| Err(Error::BackendUnavailable(why));
    if !matches!(
        fields.profile,
        PROFILE_BASELINE | PROFILE_MAIN | PROFILE_HIGH
    ) {
        return unsupported(format!("profile_idc {} is not supported", fields.profile));
    }
    if fields.chroma_format != CHROMA_420 {
        return unsupported(format!(
            "chroma_format_idc {} is not 4:2:0",
            fields.chroma_format
        ));
    }
    if fields.bit_depth > 8 {
        return unsupported(format!(
            "{}-bit pictures are not supported",
            fields.bit_depth
        ));
    }
    Ok(())
}

/// What `get_format` should pick, and whether it had to refuse.
struct Wanted {
    format: c_int,
    refused: AtomicBool,
}

/// `AVCodecContext.get_format`: the hardware format, or nothing - never
/// FFmpeg's software fallback.
unsafe extern "C" fn get_format(context: Ptr, formats: *const c_int) -> c_int {
    // SAFETY: the context a decoder opened, whose `opaque` it set to a
    // `Wanted` that outlives the context.
    let wanted = unsafe { abi::read::<*const Wanted>(context, abi::context::OPAQUE) };
    if wanted.is_null() || formats.is_null() {
        return abi::PIX_FMT_NONE;
    }
    // SAFETY: as above.
    let wanted = unsafe { &*wanted };
    for index in 0.. {
        // SAFETY: FFmpeg's list, ended by `AV_PIX_FMT_NONE`, read no further.
        let format = unsafe { *formats.add(index) };
        if format == abi::PIX_FMT_NONE {
            break;
        }
        if format == wanted.format {
            return format;
        }
    }
    wanted.refused.store(true, Ordering::Relaxed);
    abi::PIX_FMT_NONE
}

/// What the caller said about a sample, kept until its frame comes out.
#[derive(Clone, Copy)]
struct Source {
    pts_ns: Option<i64>,
    dts_ns: Option<i64>,
    duration_ns: Option<u64>,
}

/// One open `AVCodecContext`, the packet and frames it is driven with, and
/// the decoded pictures not yet pulled.
pub struct Decoder {
    api: &'static Api,
    context: Ptr,
    packet: Ptr,
    hw: Ptr,
    sw: Ptr,
    hw_format: c_int,
    nv12: c_int,
    /// Read through the context's `opaque` by [`get_format`].
    wanted: Box<Wanted>,
    converter: bitstream::Converter,
    /// Submitted and neither output nor known dropped, by id.
    pending: BTreeMap<u64, Source>,
    ready: VecDeque<Frame>,
    dropped: VecDeque<DroppedSample>,
    ended: bool,
    drained: bool,
    /// The first error that ended decoding.
    fatal: Option<Error>,
}

// SAFETY: the FFmpeg objects are used by one thread at a time, which is all
// FFmpeg asks of a codec context; `&mut self` sees to it.
unsafe impl Send for Decoder {}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: objects this decoder allocated, each freed once; FFmpeg's
        // free functions take null.
        unsafe {
            (self.api.avcodec_free_context)(&mut self.context);
            (self.api.av_packet_free)(&mut self.packet);
            (self.api.av_frame_free)(&mut self.hw);
            (self.api.av_frame_free)(&mut self.sw);
        }
    }
}

/// A fatal error, kept so that every later call reports it again.
fn replay(error: &Error) -> Error {
    match error {
        Error::BackendUnavailable(why) => Error::BackendUnavailable(why.clone()),
        Error::Decode(why) => Error::Decode(why.clone()),
        other => Error::Decode(other.to_string()),
    }
}

impl Decoder {
    fn new(
        api: &'static Api,
        device: &Device,
        sets: &[&[u8]],
        nal_length: u8,
    ) -> Result<Self, Error> {
        let codec = device.codec as *const c_void;
        let out_of_memory = || Error::BackendUnavailable("FFmpeg is out of memory".into());
        // SAFETY: resolved entry points; each result is checked below.
        let decoder = unsafe {
            Decoder {
                api,
                context: (api.avcodec_alloc_context3)(codec),
                packet: (api.av_packet_alloc)(),
                hw: (api.av_frame_alloc)(),
                sw: (api.av_frame_alloc)(),
                hw_format: device.hw_format,
                nv12: device.nv12,
                wanted: Box::new(Wanted {
                    format: device.hw_format,
                    refused: AtomicBool::new(false),
                }),
                converter: bitstream::Converter::new(nal_length, sets),
                pending: BTreeMap::new(),
                ready: VecDeque::new(),
                dropped: VecDeque::new(),
                ended: false,
                drained: false,
                fatal: None,
            }
        };
        if [decoder.context, decoder.packet, decoder.hw, decoder.sw]
            .iter()
            .any(|p| p.is_null())
        {
            return Err(out_of_memory());
        }
        let context = decoder.context;
        // SAFETY: a fresh context, its fields at the offsets `abi` checks,
        // and a device reference the context takes over.
        let status = unsafe {
            let device_ref = (api.av_buffer_ref)(device.buffer as *const c_void);
            if device_ref.is_null() {
                return Err(out_of_memory());
            }
            abi::write::<Ptr>(context, abi::context::HW_DEVICE_CTX, device_ref);
            abi::write::<unsafe extern "C" fn(Ptr, *const c_int) -> c_int>(
                context,
                abi::context::GET_FORMAT,
                get_format,
            );
            abi::write::<*const Wanted>(context, abi::context::OPAQUE, &*decoder.wanted);
            (api.av_opt_set_int)(context, c"threads".as_ptr(), 1, 0);
            (api.avcodec_open2)(context, codec, std::ptr::null_mut())
        };
        if status < 0 {
            return Err(Error::BackendUnavailable(format!(
                "FFmpeg did not open the decoder (error {status})"
            )));
        }
        Ok(decoder)
    }

    /// Whether `get_format` had to refuse: the hardware cannot decode this
    /// stream (FFmpeg then reports each picture as bad data).
    fn refused(&self) -> bool {
        self.wanted.refused.load(Ordering::Relaxed)
    }

    /// End decoding for good. A failure is the stream's when the hardware
    /// refused it, the backend's otherwise.
    fn fail(&mut self, what: &str, status: c_int) -> Error {
        let error = match self.refused() {
            true => Error::Decode("the hardware cannot decode this stream".into()),
            false => Error::BackendUnavailable(format!("{what} failed (error {status})")),
        };
        replay(self.fatal.get_or_insert(error))
    }

    fn check(&self) -> Result<(), Error> {
        self.fatal.as_ref().map_or(Ok(()), |e| Err(replay(e)))
    }

    /// Turn the picture in `hw` into a [`Frame`].
    fn take_picture(&mut self) -> Result<Frame, Error> {
        let api = self.api;
        let (hw, sw) = (self.hw, self.sw);
        let unsupported = |why: &str| Error::Decode(format!("FFmpeg output {why}"));
        // SAFETY: a frame `avcodec_receive_frame` just filled.
        let format = unsafe { abi::read::<c_int>(hw, abi::frame::FORMAT) };
        if format != self.hw_format {
            return Err(unsupported("a picture that is not on the hardware"));
        }
        // SAFETY: the decoded frame, and an empty frame for FFmpeg to
        // allocate in the first format it can copy to.
        let status = unsafe {
            (api.av_frame_unref)(sw);
            (api.av_hwframe_transfer_data)(sw, hw, 0)
        };
        if status < 0 {
            return Err(self.fail("copying a picture from the device", status));
        }
        let frame = self.copy_nv12();
        // SAFETY: the copy, no longer borrowed.
        unsafe { (api.av_frame_unref)(sw) };
        frame.ok_or_else(|| unsupported("a picture that is not well-formed NV12"))
    }

    /// The picture in `sw`, cropped and packed into one buffer.
    fn copy_nv12(&self) -> Option<Frame> {
        let (hw, sw) = (self.hw, self.sw);
        // SAFETY: the decoded frame and its copy, at `abi`'s offsets.
        let (sw_format, width, height, crop_top, crop_left, id) = unsafe {
            (
                abi::read::<c_int>(sw, abi::frame::FORMAT),
                abi::read::<c_int>(hw, abi::frame::WIDTH),
                abi::read::<c_int>(hw, abi::frame::HEIGHT),
                abi::read::<usize>(hw, abi::frame::CROP_TOP),
                abi::read::<usize>(hw, abi::frame::CROP_LEFT),
                abi::read::<i64>(hw, abi::frame::PTS),
            )
        };
        if sw_format != self.nv12 || crop_left % 2 != 0 || crop_top % 2 != 0 {
            return None;
        }
        // FFmpeg crops a hardware frame on the right and bottom by shrinking
        // it; the left and top are left to whoever reads it.
        let width = usize::try_from(width).ok()?.checked_sub(crop_left)?;
        let height = usize::try_from(height).ok()?.checked_sub(crop_top)?;
        if width == 0 || height == 0 {
            return None;
        }
        // SAFETY: the copy's plane pointers and row sizes.
        let (y, uv, y_stride, uv_stride) = unsafe {
            (
                abi::read::<*const u8>(sw, abi::frame::DATA),
                abi::read::<*const u8>(sw, abi::frame::DATA + size_of::<Ptr>()),
                abi::read::<c_int>(sw, abi::frame::LINESIZE),
                abi::read::<c_int>(sw, abi::frame::LINESIZE + size_of::<c_int>()),
            )
        };
        let (y_stride, uv_stride) = (
            usize::try_from(y_stride).ok()?,
            usize::try_from(uv_stride).ok()?,
        );
        let chroma_row = width.div_ceil(2) * 2;
        let chroma_height = height.div_ceil(2);
        if y.is_null() || uv.is_null() || y_stride < width || uv_stride < chroma_row {
            return None;
        }
        let mut bytes = Vec::with_capacity(width * height + chroma_row * chroma_height);
        // SAFETY: FFmpeg allocated the copy at least the frame's coded size,
        // of which the cropped picture is a part.
        unsafe {
            for row in 0..height {
                let from = y.add((crop_top + row) * y_stride + crop_left);
                bytes.extend_from_slice(std::slice::from_raw_parts(from, width));
            }
            for row in 0..chroma_height {
                let from = uv.add((crop_top / 2 + row) * uv_stride + crop_left);
                bytes.extend_from_slice(std::slice::from_raw_parts(from, chroma_row));
            }
        }
        let id = u64::try_from(id).ok()?;
        // SAFETY: the decoded frame.
        let layout = unsafe { frame_layout(hw, width, height, chroma_row) };
        Some(Frame {
            id,
            layout,
            bytes,
            source: None,
        })
    }

    /// Receive pictures until FFmpeg has none, then account for samples it
    /// will never output.
    fn pump(&mut self) -> Result<(), Error> {
        let api = self.api;
        loop {
            // SAFETY: the open context and this decoder's frame.
            let status = unsafe { (api.avcodec_receive_frame)(self.context, self.hw) };
            match status {
                0 => {
                    let picture = self.take_picture();
                    // SAFETY: as above.
                    unsafe { (api.av_frame_unref)(self.hw) };
                    let mut frame = match picture {
                        Ok(frame) => frame,
                        Err(e) if self.fatal.is_some() => return Err(e),
                        Err(e) => return Err(replay(self.fatal.insert(e))),
                    };
                    match self.pending.remove(&frame.id) {
                        Some(source) => {
                            frame.source = Some(source);
                            self.ready.push_back(frame);
                        }
                        // A second picture for one sample: not the caller's
                        // to see, since its identity is already spent.
                        None => continue,
                    }
                }
                abi::EAGAIN => break,
                abi::EOF => {
                    self.drained = true;
                    break;
                }
                // A broken picture: what it held up is accounted for below.
                abi::INVALIDDATA if !self.refused() => break,
                _ => return Err(self.fail("decoding", status)),
            }
        }
        let cutoff = match self.drained {
            true => 0,
            false => MAX_PENDING,
        };
        while self.pending.len() > cutoff {
            let Some((id, _)) = self.pending.pop_first() else {
                break;
            };
            self.dropped.push_back(DroppedSample { id, error: None });
        }
        Ok(())
    }
}

/// The layout of a packed NV12 copy of `frame`, with its colour and field
/// description.
///
/// # Safety
///
/// `frame` must be a live `AVFrame` holding a decoded picture.
unsafe fn frame_layout(
    frame: *const c_void,
    width: usize,
    height: usize,
    chroma_row: usize,
) -> FrameLayout {
    // SAFETY: as the caller promises, at `abi`'s offsets.
    let field = |offset| unsafe { abi::read::<c_int>(frame, offset) };
    let (sar_num, sar_den) = (
        field(abi::frame::SAMPLE_ASPECT_RATIO),
        field(abi::frame::SAMPLE_ASPECT_RATIO + size_of::<c_int>()),
    );
    let flags = field(abi::frame::FLAGS);
    let interlaced = flags & abi::FRAME_FLAG_INTERLACED != 0;
    let repeat_first_field = field(abi::frame::REPEAT_PICT) == 1;
    let field_order = match (
        interlaced || repeat_first_field,
        flags & abi::FRAME_FLAG_TOP_FIELD_FIRST != 0,
    ) {
        (false, _) => FrameFieldOrder::Unknown,
        (true, true) => FrameFieldOrder::TopFirst,
        (true, false) => FrameFieldOrder::BottomFirst,
    };
    FrameLayout {
        width,
        height,
        format: PixelFormat::Nv12,
        strides: vec![width, chroma_row],
        offsets: vec![0, width * height],
        crop: None,
        pixel_aspect_ratio: aspect_ratio(sar_num, sar_den),
        chroma_siting: match field(abi::frame::CHROMA_LOCATION) {
            0 => ChromaSiting::Unknown,
            abi::CHROMA_LOC_LEFT => ChromaSiting::Mpeg2,
            abi::CHROMA_LOC_CENTER => ChromaSiting::Jpeg,
            abi::CHROMA_LOC_TOPLEFT => ChromaSiting::Cosited,
            other => ChromaSiting::Other(other as u32),
        },
        interlace_mode: match interlaced {
            true => FrameInterlaceMode::Interleaved,
            false => FrameInterlaceMode::Progressive,
        },
        field_order,
        repeat_first_field,
        one_field: false,
        matrix: matrix(field(abi::frame::COLORSPACE)),
        primaries: primaries(field(abi::frame::COLOR_PRIMARIES)),
        transfer: transfer(field(abi::frame::COLOR_TRC)),
        full_range: match field(abi::frame::COLOR_RANGE) {
            abi::RANGE_JPEG => Some(true),
            abi::RANGE_MPEG => Some(false),
            _ => None,
        },
    }
}

/// A reduced, positive pixel aspect ratio; FFmpeg's `0/1` means unknown,
/// which is square as far as a picture can tell.
fn aspect_ratio(num: c_int, den: c_int) -> (u32, u32) {
    let (Ok(num), Ok(den)) = (u32::try_from(num), u32::try_from(den)) else {
        return (1, 1);
    };
    if num == 0 || den == 0 {
        return (1, 1);
    }
    let (mut a, mut b) = (num, den);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    (num / a, den / a)
}

/// ITU-T H.273 `MatrixCoefficients`, which FFmpeg's `AVColorSpace` is.
fn matrix(code: c_int) -> ColorMatrix {
    match code {
        0 => ColorMatrix::Rgb,
        1 => ColorMatrix::Bt709,
        2 => ColorMatrix::Unknown,
        4 => ColorMatrix::Fcc,
        5 | 6 => ColorMatrix::Bt601,
        7 => ColorMatrix::Smpte240m,
        9 | 10 => ColorMatrix::Bt2020,
        other => ColorMatrix::Other(other),
    }
}

/// ITU-T H.273 `ColourPrimaries`, which FFmpeg's `AVColorPrimaries` is.
fn primaries(code: c_int) -> ColorPrimaries {
    match code {
        1 => ColorPrimaries::Bt709,
        2 => ColorPrimaries::Unknown,
        4 => ColorPrimaries::Bt470m,
        5 => ColorPrimaries::Bt470bg,
        6 => ColorPrimaries::Smpte170m,
        7 => ColorPrimaries::Smpte240m,
        8 => ColorPrimaries::Film,
        9 => ColorPrimaries::Bt2020,
        10 => ColorPrimaries::Smptest428,
        11 => ColorPrimaries::Smpterp431,
        12 => ColorPrimaries::Smpteeg432,
        22 => ColorPrimaries::Ebu3213,
        other => ColorPrimaries::Other(other),
    }
}

/// ITU-T H.273 `TransferCharacteristics`, which FFmpeg's
/// `AVColorTransferCharacteristic` is.
fn transfer(code: c_int) -> ColorTransfer {
    match code {
        1 => ColorTransfer::Bt709,
        2 => ColorTransfer::Unknown,
        4 => ColorTransfer::Gamma22,
        5 => ColorTransfer::Gamma28,
        6 => ColorTransfer::Bt601,
        7 => ColorTransfer::Smpte240m,
        8 => ColorTransfer::Gamma10,
        9 => ColorTransfer::Log100,
        10 => ColorTransfer::Log316,
        13 => ColorTransfer::Srgb,
        14 => ColorTransfer::Bt202010,
        15 => ColorTransfer::Bt202012,
        16 => ColorTransfer::Smpte2084,
        18 => ColorTransfer::AribStdB67,
        other => ColorTransfer::Other(other),
    }
}

impl VideoDecoder for Decoder {
    type Frame = Frame;

    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }

    fn submit(&mut self, packet: AccessUnit<'_>) -> Result<(), Error> {
        self.check()?;
        if self.ended {
            return Err(Error::EndOfStream);
        }
        if self.pending.contains_key(&packet.id) {
            return Err(Error::InvalidInput(format!(
                "sample {} is pending",
                packet.id
            )));
        }
        let Ok(id) = i64::try_from(packet.id) else {
            return Err(Error::InvalidInput("a sample id above i64::MAX".into()));
        };
        let annex_b = self.converter.convert(packet.bytes)?;
        let Ok(size) = c_int::try_from(annex_b.len()) else {
            return Err(Error::InvalidInput("an access unit too large".into()));
        };
        let api = self.api;
        // SAFETY: this decoder's packet, filled with a fresh, padded buffer
        // of `size` bytes, which the copy fits.
        unsafe {
            (api.av_packet_unref)(self.packet);
            let status = (api.av_new_packet)(self.packet, size);
            if status < 0 {
                return Err(self.fail("allocating a packet", status));
            }
            let data = abi::read::<*mut u8>(self.packet, abi::packet::DATA);
            if data.is_null() || abi::read::<c_int>(self.packet, abi::packet::SIZE) != size {
                return Err(self.fail("allocating a packet", 0));
            }
            std::ptr::copy_nonoverlapping(annex_b.as_ptr(), data, annex_b.len());
            // The sample id rides in the timestamp FFmpeg carries through
            // reordering to the picture.
            abi::write::<i64>(self.packet, abi::packet::PTS, id);
        }
        // A full decoder takes the packet once its output has been taken.
        for _ in 0..2 {
            // SAFETY: the open context and the packet filled above.
            let status = unsafe { (api.avcodec_send_packet)(self.context, self.packet) };
            match status {
                0 => {
                    self.converter.sent();
                    self.pending.insert(
                        packet.id,
                        Source {
                            pts_ns: packet.pts_ns,
                            dts_ns: packet.dts_ns,
                            duration_ns: packet.duration_ns,
                        },
                    );
                    return self.pump();
                }
                abi::EAGAIN => self.pump()?,
                abi::INVALIDDATA if !self.refused() => {
                    return Err(Error::InvalidInput("FFmpeg refused the access unit".into()));
                }
                _ => return Err(self.fail("sending a packet", status)),
            }
        }
        Err(Error::Backpressure)
    }

    /// Never waits: decoding happened in `submit` and `end_stream`.
    fn pull(&mut self, _timeout: Duration) -> Result<Option<Self::Frame>, Error> {
        if let Some(frame) = self.ready.pop_front() {
            return Ok(Some(frame));
        }
        self.check()?;
        Ok(None)
    }

    fn take_dropped(&mut self) -> Option<DroppedSample> {
        self.dropped.pop_front()
    }

    fn end_stream(&mut self) -> Result<(), Error> {
        self.check()?;
        if self.ended {
            return Ok(());
        }
        self.ended = true;
        // SAFETY: the open context; a null packet starts draining.
        let status = unsafe { (self.api.avcodec_send_packet)(self.context, std::ptr::null()) };
        if status < 0 && status != abi::EOF {
            return Err(self.fail("draining", status));
        }
        // A broken picture is skipped and the rest still come out; the
        // bound is for a decoder that reports the same one forever.
        for _ in 0..64 {
            self.pump()?;
            if self.drained {
                return Ok(());
            }
        }
        // Whatever FFmpeg still holds will never come out.
        self.drained = true;
        while let Some((id, _)) = self.pending.pop_first() {
            self.dropped.push_back(DroppedSample { id, error: None });
        }
        Ok(())
    }

    fn is_drained(&mut self) -> Result<bool, Error> {
        self.check()?;
        Ok(self.drained && self.ready.is_empty())
    }
}

/// A decoded picture, packed NV12 in main memory.
pub struct Frame {
    id: u64,
    layout: FrameLayout,
    bytes: Vec<u8>,
    source: Option<Source>,
}

impl VideoFrame for Frame {
    fn id(&self) -> u64 {
        self.id
    }

    fn layout(&self) -> &FrameLayout {
        &self.layout
    }

    /// The sample's own presentation time: FFmpeg reorders by it, it does
    /// not compute another.
    fn pts_ns(&self) -> Option<i64> {
        self.source.and_then(|s| s.pts_ns)
    }

    /// FFmpeg does not report the decode order it used.
    fn dts_ns(&self) -> Option<i64> {
        None
    }

    fn duration_ns(&self) -> Option<u64> {
        self.source.and_then(|s| s.duration_ns)
    }

    fn source_pts_ns(&self) -> Option<i64> {
        self.source.and_then(|s| s.pts_ns)
    }

    fn source_dts_ns(&self) -> Option<i64> {
        self.source.and_then(|s| s.dts_ns)
    }

    fn with_bytes<R>(&self, read: impl FnOnce(&[u8], &FrameLayout) -> R) -> Result<R, Error> {
        Ok(read(&self.bytes, &self.layout))
    }
}
