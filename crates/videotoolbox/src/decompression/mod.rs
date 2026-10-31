//! `VTDecompressionSession`: H.264 samples in, pixel buffers out through the
//! output callback.
//!
//! A session decodes through the first `mediabackend` decoder that works on
//! this host (see `backend`) and hands the callback pixel buffers in the
//! format the attributes ask for. Where none works, and for every other
//! codec, create answers as VideoToolbox does on a machine without the
//! codec, `kVTCouldNotFindVideoDecoderErr`, which is the answer a program
//! already handles by decoding some other way.
//!
//! Frames are decoded and delivered on the thread that submits them, even
//! when asynchronous decompression is asked for: that flag permits
//! asynchrony, it does not require it. A frame the decoder holds back to
//! reorder comes out of a later `DecodeFrame` or of `FinishDelayedFrames`.
//! No lock is held while the callback runs, and no thread is made, so guest
//! code only ever runs on threads the guest gave us.

#![allow(non_snake_case)]

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::ThreadId;

use corefoundation::api::CFAllocatorRef;
use corefoundation::base::{CFTypeID, CFTypeRef};
use coremedia::CMTime;
use mediabackend::{AccessUnit, Error};
use objc::class::Class;

use crate::constants::{
    kVTAllocationFailedErr, kVTCouldNotFindVideoDecoderErr,
    kVTFormatDescriptionChangeNotSupportedErr, kVTInvalidSessionErr, kVTParameterErr,
    kVTPropertyNotSupportedErr, kVTVideoDecoderBadDataErr, kVTVideoDecoderMalfunctionErr,
};
use crate::types::{OSStatus, VTDecodeFrameFlags, VTDecodeInfoFlags};

mod backend;
#[cfg(test)]
mod mock;
mod output;
#[cfg(test)]
mod tests;

use backend::{Backend, Decoder, HwPolicy, Output, Registry, Stream, report_once};
use output::{MakeError, OutputSpec};

/// `kVTDecodeFrame_DoNotOutputFrame`. The other `VTDecodeFrameFlags`
/// (asynchronous, 1x real-time playback, temporal processing) change nothing
/// here: frames always come out in display order, on the caller's thread.
pub const DECODE_DO_NOT_OUTPUT_FRAME: VTDecodeFrameFlags = 2;

/// `VTDecodeInfoFlags`; `kVTDecodeInfo_Asynchronous` is never reported.
pub const INFO_FRAME_DROPPED: VTDecodeInfoFlags = 2;
pub const INFO_IMAGE_BUFFER_MODIFIABLE: VTDecodeInfoFlags = 4;

const CODEC_H264: u32 = u32::from_be_bytes(*b"avc1");
const MEDIA_VIDEO: u32 = u32::from_be_bytes(*b"vide");
/// `nal_unit_type`s of the parameter sets a decoder is opened with.
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const NAL_TYPE_MASK: u8 = 0x1f;
const NANOSECONDS: i128 = 1_000_000_000;

const REAL_TIME: &str = "RealTime";
const USING_HARDWARE: &str = "UsingHardwareAcceleratedVideoDecoder";
const ENABLE_HARDWARE: &str = "EnableHardwareAcceleratedVideoDecoder";
const REQUIRE_HARDWARE: &str = "RequireHardwareAcceleratedVideoDecoder";
/// A cap on how many frames the decoder may hold back for reordering.
/// Accepted and stored: every backend here already delivers in display order
/// as soon as it can, so there is no wait for a cap to shorten. Refusing it
/// with `kVTPropertyNotSupportedErr` is what a guest may read as a decoder
/// that cannot be configured, and give up on before submitting a frame.
const MAX_FRAMES_BEFORE_OUTPUT: &str = "MaxNumberOfFramesBeforeOutput";

/// The session and decoder specification keys, as `(Mach-O name, text)`.
pub(crate) const KEYS: &[(&str, &str)] = &[
    ("_kVTDecompressionPropertyKey_RealTime", REAL_TIME),
    (
        "_kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder",
        USING_HARDWARE,
    ),
    (
        "_kVTDecompressionPropertyKey_MaxNumberOfFramesBeforeOutput",
        MAX_FRAMES_BEFORE_OUTPUT,
    ),
    (
        "_kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder",
        ENABLE_HARDWARE,
    ),
    (
        "_kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder",
        REQUIRE_HARDWARE,
    ),
];

/// `VTDecompressionOutputCallback`. The two `CMTime`s are passed in memory,
/// as any twenty-four byte struct is.
pub type VTDecompressionOutputCallback = unsafe extern "C" fn(
    refcon: *mut c_void,
    source_frame_refcon: *mut c_void,
    status: OSStatus,
    info_flags: VTDecodeInfoFlags,
    image_buffer: CFTypeRef,
    presentation_time_stamp: CMTime,
    presentation_duration: CMTime,
);

/// `VTDecompressionOutputCallbackRecord`.
#[repr(C)]
pub struct VTDecompressionOutputCallbackRecord {
    pub decompression_output_callback: Option<VTDecompressionOutputCallback>,
    pub decompression_output_ref_con: *mut c_void,
}

/// A frame submitted and not yet called back for.
struct Pending {
    source_refcon: usize,
    pts: CMTime,
    duration: CMTime,
    do_not_output: bool,
}

/// A callback to make once the session lock is let go.
struct Ready {
    source_refcon: usize,
    status: OSStatus,
    info_flags: VTDecodeInfoFlags,
    /// A pixel buffer this list holds a reference to, or null.
    image: usize,
    pts: CMTime,
    duration: CMTime,
    /// The picture's size and format, for the first-frame diagnostic.
    frame: Option<(usize, usize, u32)>,
}

impl Ready {
    fn failed(pending: Pending, status: OSStatus) -> Ready {
        Ready {
            source_refcon: pending.source_refcon,
            status,
            info_flags: INFO_FRAME_DROPPED,
            image: 0,
            pts: pending.pts,
            duration: pending.duration,
            frame: None,
        }
    }
}

struct Inner {
    /// `None` after a malfunction, or after `FinishDelayedFrames` until the
    /// next sample opens another.
    decoder: Option<Box<dyn Decoder>>,
    output: OutputSpec,
    pending: BTreeMap<u64, Pending>,
    next_id: u64,
    invalid: bool,
    malfunctioned: bool,
    /// The retained `RealTime` value, or null.
    real_time: usize,
    /// The retained `MaxNumberOfFramesBeforeOutput` value, or null.
    max_frames_before_output: usize,
}

struct State {
    inner: Mutex<Inner>,
    backend: &'static dyn Backend,
    /// The retained format description.
    format: usize,
    /// Its NAL unit length size and parameter sets.
    nal_length: u8,
    parameter_sets: Vec<Vec<u8>>,
    callback: VTDecompressionOutputCallback,
    refcon: usize,
    /// The threads delivering callbacks now, once per delivery.
    delivering: Mutex<Vec<ThreadId>>,
    idle: Condvar,
    /// Whether a decoded frame has reached the callback yet, for the line
    /// that says video is flowing.
    delivered_first_frame: AtomicBool,
}

impl Drop for State {
    fn drop(&mut self) {
        let inner = self.inner.get_mut().unwrap_or_else(|e| e.into_inner());
        for object in [self.format, inner.real_time, inner.max_frames_before_output] {
            release(object as CFTypeRef);
        }
    }
}

fn class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let address = *CLASS.get_or_init(|| {
        corefoundation::bootstrap();
        let class = corefoundation::boxed::register::<State>(c"VTDecompressionSession", None, true);
        objc::class::ptr(class) as usize
    });
    // SAFETY: the registered class lives for the process.
    unsafe { &*(address as *const Class) }
}

fn state<'a>(session: CFTypeRef) -> Option<&'a State> {
    if session.is_null() {
        return None;
    }
    // SAFETY: an object a guest received from this runtime, checked against
    // the class before its contents are read.
    unsafe { corefoundation::boxed::contents::<State>(session, class()) }.map(|s| &*s)
}

fn release(object: CFTypeRef) {
    if !object.is_null() {
        // SAFETY: a reference the caller owns.
        unsafe { corefoundation::api::CFRelease(object) };
    }
}

fn retain(object: CFTypeRef) -> CFTypeRef {
    match object.is_null() {
        true => object,
        // SAFETY: a live CF object.
        false => unsafe { corefoundation::api::CFRetain(object) },
    }
}

fn is_key(candidate: CFTypeRef, text: &str) -> bool {
    // SAFETY: the caller's key, and a string made here.
    !candidate.is_null()
        && unsafe { corefoundation::api::CFEqual(candidate, output::key(text)) } != 0
}

fn lock(state: &State) -> MutexGuard<'_, Inner> {
    state.inner.lock().unwrap_or_else(|e| e.into_inner())
}

/// A boolean in a dictionary, if it is there.
fn flag(dictionary: CFTypeRef, name: &str) -> Option<bool> {
    if dictionary.is_null() {
        return None;
    }
    // SAFETY: the caller's dictionary and a key made here.
    let value = unsafe { corefoundation::api::CFDictionaryGetValue(dictionary, output::key(name)) };
    if value.is_null() {
        return None;
    }
    // SAFETY: a CF object from the dictionary.
    unsafe { corefoundation::collections::number(value) }.map(|n| n.as_i64() != 0)
}

/// Which backends a decoder specification allows: Apple's meaning of the two
/// keys, with every backend when neither is there.
fn policy(specification: CFTypeRef) -> HwPolicy {
    let mut policy = HwPolicy::ANY;
    if flag(specification, REQUIRE_HARDWARE) == Some(true) {
        policy.software = false;
    }
    if flag(specification, ENABLE_HARDWARE) == Some(false) {
        policy.hardware = false;
    }
    policy
}

/// The `avcC` record's NAL unit length size and its SPSs and PPSs, in that
/// order, or `None` for a description without one.
fn parameter_sets(format: CFTypeRef) -> Option<(u8, Vec<Vec<u8>>)> {
    let (mut count, mut length_size) = (0usize, 0i32);
    // SAFETY: the description, and out-parameters for the count and length.
    let status = unsafe {
        coremedia::format_description::CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            format,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut count,
            &mut length_size,
        )
    };
    if status != 0 || count == 0 {
        return None;
    }
    let mut sets = Vec::with_capacity(count);
    for index in 0..count {
        let (mut pointer, mut size) = (std::ptr::null(), 0usize);
        // SAFETY: as above, for one set; the pointer is into the description.
        let status = unsafe {
            coremedia::format_description::CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                format,
                index,
                &mut pointer,
                &mut size,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != 0 || pointer.is_null() {
            return None;
        }
        // SAFETY: `size` bytes the description holds.
        sets.push(unsafe { std::slice::from_raw_parts(pointer, size) }.to_vec());
    }
    // The decoder needs the SPSs and PPSs; SPS extensions are for layers it
    // does not decode.
    let kind = |set: &Vec<u8>| set.first().map(|h| h & NAL_TYPE_MASK);
    sets.retain(|set| matches!(kind(set), Some(NAL_SPS | NAL_PPS)));
    sets.sort_by_key(|set| kind(set) != Some(NAL_SPS));
    Some((u8::try_from(length_size).ok()?, sets))
}

/// The picture size a description gives.
fn dimensions(format: CFTypeRef) -> (usize, usize) {
    // SAFETY: a description, or anything a guest passed.
    let d = unsafe { coremedia::format_description::CMVideoFormatDescriptionGetDimensions(format) };
    (d.width.max(0) as usize, d.height.max(0) as usize)
}

/// `OSStatus VTDecompressionSessionCreate(CFAllocatorRef allocator,
/// CMVideoFormatDescriptionRef videoFormatDescription, CFDictionaryRef
/// videoDecoderSpecification, CFDictionaryRef destinationImageBufferAttributes,
/// const VTDecompressionOutputCallbackRecord *outputCallback,
/// VTDecompressionSessionRef *decompressionSessionOut)`
pub unsafe extern "C" fn VTDecompressionSessionCreate(
    _allocator: CFAllocatorRef,
    format: CFTypeRef,
    specification: CFTypeRef,
    attributes: CFTypeRef,
    callback: *const VTDecompressionOutputCallbackRecord,
    out: *mut CFTypeRef,
) -> OSStatus {
    // SAFETY: as the caller promises.
    unsafe {
        create(
            Registry::global(),
            format,
            specification,
            attributes,
            callback,
            out,
        )
    }
}

/// [`VTDecompressionSessionCreate`] from the backends of `registry`.
///
/// # Safety
///
/// `out` writable, the rest null or what their types say.
pub(crate) unsafe fn create(
    registry: &Registry,
    format: CFTypeRef,
    specification: CFTypeRef,
    attributes: CFTypeRef,
    callback: *const VTDecompressionOutputCallbackRecord,
    out: *mut CFTypeRef,
) -> OSStatus {
    if out.is_null() {
        return kVTParameterErr;
    }
    // SAFETY: the caller's out-parameter.
    unsafe { out.write(std::ptr::null()) };
    // SAFETY: null or an object a guest received from this runtime.
    let video = !format.is_null()
        && unsafe { coremedia::format_description::CMFormatDescriptionGetMediaType(format) }
            == MEDIA_VIDEO;
    if !video {
        return kVTParameterErr;
    }
    // SAFETY: null or the caller's record.
    let record = unsafe { callback.as_ref() };
    let Some((Some(function), refcon)) = record.map(|r| {
        (
            r.decompression_output_callback,
            r.decompression_output_ref_con,
        )
    }) else {
        return kVTParameterErr;
    };
    // SAFETY: a video description.
    let codec =
        unsafe { coremedia::format_description::CMFormatDescriptionGetMediaSubType(format) };
    if codec != CODEC_H264 {
        report_once(format!(
            "rosemary: VideoToolbox: no decoder for '{}'",
            output::fourcc(codec)
        ));
        return kVTCouldNotFindVideoDecoderErr;
    }
    let Some((nal_length, sets)) = parameter_sets(format) else {
        report_once(
            "rosemary: VideoToolbox: an H.264 description without an avcC record cannot be decoded"
                .to_string(),
        );
        return kVTCouldNotFindVideoDecoderErr;
    };
    let (width, height) = dimensions(format);
    let output = match OutputSpec::parse(attributes, width, height) {
        Ok(output) => output,
        Err(why) => {
            report_once(format!("rosemary: VideoToolbox: {why}"));
            return kVTCouldNotFindVideoDecoderErr;
        }
    };
    let stream = Stream {
        parameter_sets: &sets,
        nal_length,
    };
    let (decoder, chosen) = match registry.open(&stream, policy(specification)) {
        Ok(opened) => opened,
        Err(why) => {
            report_once(format!("rosemary: VideoToolbox: no H.264 decoder: {why}"));
            return kVTCouldNotFindVideoDecoderErr;
        }
    };
    report_once(format!(
        "rosemary: VideoToolbox: H.264 through {}",
        chosen.description()
    ));
    let state = State {
        inner: Mutex::new(Inner {
            decoder: Some(decoder),
            output,
            pending: BTreeMap::new(),
            next_id: 0,
            invalid: false,
            malfunctioned: false,
            real_time: 0,
            max_frames_before_output: 0,
        }),
        backend: chosen,
        format: retain(format) as usize,
        nal_length,
        parameter_sets: sets,
        callback: function,
        refcon: refcon as usize,
        delivering: Mutex::new(Vec::new()),
        idle: Condvar::new(),
        delivered_first_frame: AtomicBool::new(false),
    };
    let session = corefoundation::boxed::make(class(), state);
    if session.is_null() {
        return kVTAllocationFailedErr;
    }
    // SAFETY: the caller's out-parameter.
    unsafe { out.write(session) };
    0
}

/// Whether `format` describes the session's stream: an equal description, or
/// one with the same picture size and the same `avcC` parameter sets. The
/// second does not depend on CF value equality, which needs Foundation's
/// `-isEqual:` to compare the extensions' contents rather than identity.
fn same_stream(state: &State, format: CFTypeRef) -> bool {
    let own = state.format as CFTypeRef;
    if format.is_null() {
        return false;
    }
    // SAFETY: two descriptions, or anything a guest passed.
    unsafe {
        if format == own
            || coremedia::format_description::CMFormatDescriptionEqual(format, own) != 0
        {
            return true;
        }
        coremedia::format_description::CMFormatDescriptionGetMediaSubType(format) == CODEC_H264
            && dimensions(format) == dimensions(own)
            && parameter_sets(format)
                .is_some_and(|(n, sets)| n == state.nal_length && sets == state.parameter_sets)
    }
}

/// Keeps a session alive while a call uses it, whatever its callbacks do.
struct Hold(CFTypeRef);

impl Hold {
    fn new(session: CFTypeRef) -> Hold {
        Hold(retain(session))
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        release(self.0);
    }
}

/// Turns what the decoder gives into callbacks to make.
struct Collector<'a> {
    pending: &'a mut BTreeMap<u64, Pending>,
    output: &'a mut OutputSpec,
    ready: &'a mut Vec<Ready>,
}

impl Collector<'_> {
    fn take(&mut self, out: Output<'_>) {
        let picture = match out {
            Output::Picture(picture) => picture,
            Output::Dropped(id) => {
                if let Some(pending) = self.pending.remove(&id) {
                    self.ready
                        .push(Ready::failed(pending, kVTVideoDecoderBadDataErr));
                }
                return;
            }
        };
        let Some(pending) = self.pending.remove(&picture.id) else {
            // A picture for a sample already failed; nobody waits for it.
            return;
        };
        if pending.do_not_output {
            self.ready.push(Ready::failed(pending, 0));
            return;
        }
        match output::make_image(&picture, self.output) {
            Ok((image, format)) => self.ready.push(Ready {
                source_refcon: pending.source_refcon,
                status: 0,
                info_flags: INFO_IMAGE_BUFFER_MODIFIABLE,
                image: image as usize,
                pts: pending.pts,
                duration: pending.duration,
                // SAFETY: the buffer just made.
                frame: Some(unsafe {
                    (
                        corevideo::pixel_buffer::CVPixelBufferGetWidth(image),
                        corevideo::pixel_buffer::CVPixelBufferGetHeight(image),
                        format,
                    )
                }),
            }),
            Err(why) => {
                report_once(format!(
                    "rosemary: VideoToolbox: a {}x{} {:?} picture could not be made ({why:?})",
                    picture.layout.width, picture.layout.height, picture.layout.format
                ));
                let status = match why {
                    MakeError::Layout => kVTVideoDecoderMalfunctionErr,
                    MakeError::CoreVideo(_) => kVTAllocationFailedErr,
                };
                self.ready.push(Ready::failed(pending, status));
            }
        }
    }
}

impl Inner {
    /// Stop decoding for good after the decoder broke, because of `error`.
    /// Every frame still waiting is failed, except `current`, which the
    /// caller reports by its status.
    fn malfunction(
        &mut self,
        backend: &'static dyn Backend,
        error: &Error,
        current: Option<u64>,
        ready: &mut Vec<Ready>,
    ) {
        self.malfunctioned = true;
        self.decoder = None;
        let global = matches!(error, Error::BackendUnavailable(_));
        report_once(format!(
            "rosemary: VideoToolbox: the {} decoder failed: {}{}",
            backend.name(),
            backend::describe(error),
            match global {
                true => "; it is not used again in this process",
                false => "",
            }
        ));
        if global {
            backend.set_unhealthy();
        }
        if let Some(id) = current {
            self.pending.remove(&id);
        }
        while let Some((_, pending)) = self.pending.pop_first() {
            ready.push(Ready::failed(pending, kVTVideoDecoderMalfunctionErr));
        }
    }

    /// Take what the decoder has ready, or everything at the end of the
    /// stream. `false` if it broke.
    fn receive(
        &mut self,
        backend: &'static dyn Backend,
        finish: bool,
        current: Option<u64>,
        ready: &mut Vec<Ready>,
    ) -> bool {
        let Some(decoder) = self.decoder.as_mut() else {
            return true;
        };
        let mut collector = Collector {
            pending: &mut self.pending,
            output: &mut self.output,
            ready: &mut *ready,
        };
        let mut sink = |out: Output<'_>| collector.take(out);
        let result = match finish {
            true => decoder.finish(&mut sink),
            false => decoder.collect(&mut sink),
        };
        match result {
            Ok(()) => true,
            Err(error) => {
                self.malfunction(backend, &error, current, ready);
                false
            }
        }
    }
}

/// Make the callbacks, then let go of their images.
fn deliver(state: &State, ready: Vec<Ready>) {
    if ready.is_empty() {
        return;
    }
    let me = std::thread::current().id();
    state
        .delivering
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(me);
    struct Done<'a>(&'a State, ThreadId);
    impl Drop for Done<'_> {
        fn drop(&mut self) {
            let mut delivering = self.0.delivering.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(at) = delivering.iter().position(|t| *t == self.1) {
                delivering.swap_remove(at);
            }
            if delivering.is_empty() {
                self.0.idle.notify_all();
            }
        }
    }
    let _done = Done(state, me);
    for frame in ready {
        if let Some((width, height, format)) = frame.frame
            && !state.delivered_first_frame.swap(true, Ordering::Relaxed)
        {
            eprintln!(
                "rosemary: VideoToolbox: first frame delivered ({width}x{height}, format '{}')",
                output::fourcc(format)
            );
        }
        // Checked before each callback: one may invalidate the session.
        if !lock(state).invalid {
            // SAFETY: the guest's callback, with the arguments it registered
            // for and a buffer that is live until it returns.
            unsafe {
                (state.callback)(
                    state.refcon as *mut c_void,
                    frame.source_refcon as *mut c_void,
                    frame.status,
                    frame.info_flags,
                    frame.image as CFTypeRef,
                    frame.pts,
                    frame.duration,
                )
            };
        }
        release(frame.image as CFTypeRef);
    }
}

/// Wait until no callback is running, unless this thread is in one.
fn wait_idle(state: &State) {
    let me = std::thread::current().id();
    let mut delivering = state.delivering.lock().unwrap_or_else(|e| e.into_inner());
    while !delivering.is_empty() && !delivering.contains(&me) {
        delivering = state
            .idle
            .wait(delivering)
            .unwrap_or_else(|e| e.into_inner());
    }
}

fn invalidate(state: &State) {
    let (decoder, pending) = {
        let mut inner = lock(state);
        inner.invalid = true;
        (inner.decoder.take(), std::mem::take(&mut inner.pending))
    };
    drop(decoder);
    drop(pending);
    wait_idle(state);
}

/// Signed nanoseconds of a numeric time, for a decoder that keeps them.
fn nanoseconds(time: CMTime) -> Option<i64> {
    if !time.is_numeric() || time.timescale <= 0 {
        return None;
    }
    i64::try_from(i128::from(time.value) * NANOSECONDS / i128::from(time.timescale)).ok()
}

/// The bytes of a ready sample buffer holding one sample.
fn sample_bytes(sample: CFTypeRef) -> Option<Vec<u8>> {
    use coremedia::block_buffer as block;
    use coremedia::sample_buffer as samples;
    if sample.is_null() {
        return None;
    }
    // SAFETY: a CF object, checked to be a sample buffer before it is read as
    // one, and its block buffer copied whole.
    unsafe {
        if corefoundation::api::CFGetTypeID(sample) != samples::CMSampleBufferGetTypeID()
            || samples::CMSampleBufferDataIsReady(sample) == 0
            || samples::CMSampleBufferGetNumSamples(sample) != 1
        {
            return None;
        }
        let data = samples::CMSampleBufferGetDataBuffer(sample);
        let length = block::CMBlockBufferGetDataLength(data);
        if data.is_null() || length == 0 {
            return None;
        }
        let mut bytes = vec![0u8; length];
        match block::CMBlockBufferCopyDataBytes(data, 0, length, bytes.as_mut_ptr().cast()) {
            0 => Some(bytes),
            _ => None,
        }
    }
}

/// `OSStatus VTDecompressionSessionDecodeFrame(VTDecompressionSessionRef,
/// CMSampleBufferRef, VTDecodeFrameFlags, void *sourceFrameRefCon,
/// VTDecodeInfoFlags *infoFlagsOut)`
///
/// Decoded synchronously: `*infoFlagsOut` never has `Asynchronous`, and the
/// callbacks for every frame that is ready have run by the time this returns.
pub unsafe extern "C" fn VTDecompressionSessionDecodeFrame(
    session: CFTypeRef,
    sample: CFTypeRef,
    flags: VTDecodeFrameFlags,
    source_frame_refcon: *mut c_void,
    info_flags_out: *mut VTDecodeInfoFlags,
) -> OSStatus {
    if !info_flags_out.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { info_flags_out.write(0) };
    }
    let Some(state) = state(session) else {
        return kVTParameterErr;
    };
    let _hold = Hold::new(session);
    let mut ready = Vec::new();
    let status = {
        let mut inner = lock(state);
        submit(
            state,
            &mut inner,
            sample,
            flags,
            source_frame_refcon,
            &mut ready,
        )
    };
    deliver(state, ready);
    status
}

fn submit(
    state: &State,
    inner: &mut Inner,
    sample: CFTypeRef,
    flags: VTDecodeFrameFlags,
    source_frame_refcon: *mut c_void,
    ready: &mut Vec<Ready>,
) -> OSStatus {
    if inner.invalid {
        return kVTInvalidSessionErr;
    }
    if inner.malfunctioned {
        return kVTVideoDecoderMalfunctionErr;
    }
    let Some(bytes) = sample_bytes(sample) else {
        return kVTVideoDecoderBadDataErr;
    };
    // SAFETY: a sample buffer, as `sample_bytes` checked.
    let format = unsafe { coremedia::sample_buffer::CMSampleBufferGetFormatDescription(sample) };
    if !same_stream(state, format) {
        return kVTFormatDescriptionChangeNotSupportedErr;
    }
    if inner.decoder.is_none() {
        let stream = Stream {
            parameter_sets: &state.parameter_sets,
            nal_length: state.nal_length,
        };
        match state.backend.open(&stream) {
            Ok(decoder) => inner.decoder = Some(decoder),
            Err(error) => {
                inner.malfunction(state.backend, &error, None, ready);
                return kVTVideoDecoderMalfunctionErr;
            }
        }
    }
    let id = inner.next_id;
    inner.next_id += 1;
    // SAFETY: as above.
    let (pts, dts, duration) = unsafe {
        (
            coremedia::sample_buffer::CMSampleBufferGetPresentationTimeStamp(sample),
            coremedia::sample_buffer::CMSampleBufferGetDecodeTimeStamp(sample),
            coremedia::sample_buffer::CMSampleBufferGetDuration(sample),
        )
    };
    inner.pending.insert(
        id,
        Pending {
            source_refcon: source_frame_refcon as usize,
            pts,
            duration,
            do_not_output: flags & DECODE_DO_NOT_OUTPUT_FRAME != 0,
        },
    );
    let unit = AccessUnit {
        id,
        bytes: &bytes,
        pts_ns: nanoseconds(pts),
        dts_ns: nanoseconds(dts),
        duration_ns: nanoseconds(duration).and_then(|d| u64::try_from(d).ok()),
    };
    let Inner {
        decoder,
        pending,
        output,
        ..
    } = &mut *inner;
    let Some(decoder) = decoder.as_mut() else {
        pending.remove(&id);
        return kVTVideoDecoderMalfunctionErr;
    };
    let mut collector = Collector {
        pending,
        output,
        ready: &mut *ready,
    };
    let submitted = decoder.submit(unit, &mut |out| collector.take(out));
    match submitted {
        Ok(()) => {}
        // The sample is not decodable; the decoder carries on.
        Err(Error::InvalidInput(_)) => {
            if let Some(pending) = inner.pending.remove(&id) {
                ready.push(Ready::failed(pending, kVTVideoDecoderBadDataErr));
            }
        }
        Err(error) => {
            inner.malfunction(state.backend, &error, Some(id), ready);
            return kVTVideoDecoderMalfunctionErr;
        }
    }
    if !inner.receive(state.backend, false, Some(id), ready) {
        // The decoder broke while this frame was in it: it is failed by this
        // call's status rather than a callback, unless it came out already.
        return kVTVideoDecoderMalfunctionErr;
    }
    0
}

/// `OSStatus VTDecompressionSessionFinishDelayedFrames(VTDecompressionSessionRef)`
///
/// FIXME (ACCURACY): a decoder hands over what it holds only by ending its
/// stream, which forgets its reference pictures too, so decoding resumes
/// cleanly only at the next IDR. Apple's decoder carries on from where it was.
pub unsafe extern "C" fn VTDecompressionSessionFinishDelayedFrames(session: CFTypeRef) -> OSStatus {
    let Some(state) = state(session) else {
        return kVTParameterErr;
    };
    let _hold = Hold::new(session);
    let mut ready = Vec::new();
    {
        let mut inner = lock(state);
        if inner.invalid {
            return kVTInvalidSessionErr;
        }
        if inner.receive(state.backend, true, None, &mut ready) {
            // The next sample opens a decoder of its own.
            inner.decoder = None;
        }
        while let Some((_, pending)) = inner.pending.pop_first() {
            ready.push(Ready::failed(pending, kVTVideoDecoderBadDataErr));
        }
    }
    deliver(state, ready);
    0
}

/// `OSStatus VTDecompressionSessionWaitForAsynchronousFrames(VTDecompressionSessionRef)`:
/// frames are delivered as they are submitted, so only callbacks running on
/// other threads are waited for.
pub unsafe extern "C" fn VTDecompressionSessionWaitForAsynchronousFrames(
    session: CFTypeRef,
) -> OSStatus {
    let Some(state) = state(session) else {
        return kVTParameterErr;
    };
    if lock(state).invalid {
        return kVTInvalidSessionErr;
    }
    wait_idle(state);
    0
}

/// `void VTDecompressionSessionInvalidate(VTDecompressionSessionRef)`: no
/// callback runs once this returns, and frames still held are dropped
/// without one.
pub unsafe extern "C" fn VTDecompressionSessionInvalidate(session: CFTypeRef) {
    if let Some(state) = state(session) {
        invalidate(state);
    }
}

/// `Boolean VTDecompressionSessionCanAcceptFormatDescription(
/// VTDecompressionSessionRef, CMFormatDescriptionRef)`: only the session's
/// own stream, which is the conservative answer Apple allows.
pub unsafe extern "C" fn VTDecompressionSessionCanAcceptFormatDescription(
    session: CFTypeRef,
    format: CFTypeRef,
) -> u8 {
    let Some(state) = state(session) else {
        return 0;
    };
    if lock(state).invalid {
        return 0;
    }
    u8::from(same_stream(state, format))
}

/// `CFTypeID VTDecompressionSessionGetTypeID(void)`
pub extern "C" fn VTDecompressionSessionGetTypeID() -> CFTypeID {
    corefoundation::base::type_id(class())
}

/// `Boolean VTIsHardwareDecodeSupported(CMVideoCodecType)`: H.264 where a
/// hardware backend works.
pub extern "C" fn VTIsHardwareDecodeSupported(codec: u32) -> u8 {
    u8::from(codec == CODEC_H264 && Registry::global().hardware_available())
}

/// `void VTRegisterSupplementalVideoDecoderIfAvailable(CMVideoCodecType)`:
/// there is no supplemental decoder to register.
pub extern "C" fn VTRegisterSupplementalVideoDecoderIfAvailable(_codec: u32) {}

/// `VTSessionCopyProperty` for a decompression session.
pub(crate) fn copy_property(session: CFTypeRef, key: CFTypeRef, out: *mut CFTypeRef) -> OSStatus {
    let Some(state) = state(session) else {
        return kVTParameterErr;
    };
    if out.is_null() || key.is_null() {
        return kVTParameterErr;
    }
    let inner = lock(state);
    if inner.invalid {
        return kVTInvalidSessionErr;
    }
    let value = if is_key(key, USING_HARDWARE) {
        corefoundation::collections::boolean(state.backend.is_hardware())
    } else if is_key(key, REAL_TIME) {
        match inner.real_time {
            0 => corefoundation::collections::boolean(false),
            value => value as CFTypeRef,
        }
    } else if is_key(key, MAX_FRAMES_BEFORE_OUTPUT) {
        match inner.max_frames_before_output {
            0 => return kVTPropertyNotSupportedErr,
            value => value as CFTypeRef,
        }
    } else {
        return kVTPropertyNotSupportedErr;
    };
    // SAFETY: the caller's out-parameter, given a reference of its own.
    unsafe { out.write(retain(value)) };
    0
}

/// `VTSessionSetProperty` for a decompression session.
pub(crate) fn set_property(session: CFTypeRef, key: CFTypeRef, value: CFTypeRef) -> OSStatus {
    let Some(state) = state(session) else {
        return kVTParameterErr;
    };
    let mut inner = lock(state);
    if inner.invalid {
        return kVTInvalidSessionErr;
    }
    let slot = if is_key(key, REAL_TIME) {
        &mut inner.real_time
    } else if is_key(key, MAX_FRAMES_BEFORE_OUTPUT) {
        &mut inner.max_frames_before_output
    } else {
        return kVTPropertyNotSupportedErr;
    };
    let old = std::mem::replace(slot, retain(value) as usize);
    drop(inner);
    release(old as CFTypeRef);
    0
}

exports::exports! {
    functions {
        "VTDecompressionSessionCreate" => VTDecompressionSessionCreate,
        "VTDecompressionSessionDecodeFrame" => VTDecompressionSessionDecodeFrame,
        "VTDecompressionSessionFinishDelayedFrames" => VTDecompressionSessionFinishDelayedFrames,
        "VTDecompressionSessionWaitForAsynchronousFrames" => VTDecompressionSessionWaitForAsynchronousFrames,
        "VTDecompressionSessionInvalidate" => VTDecompressionSessionInvalidate,
        "VTDecompressionSessionCanAcceptFormatDescription" => VTDecompressionSessionCanAcceptFormatDescription,
        "VTDecompressionSessionGetTypeID" => VTDecompressionSessionGetTypeID,
        "VTIsHardwareDecodeSupported" => VTIsHardwareDecodeSupported,
        "VTRegisterSupplementalVideoDecoderIfAvailable" => VTRegisterSupplementalVideoDecoderIfAvailable,
    }
}
