//! Compressed-media sample buffers with retained CoreFoundation dependencies.

#![allow(non_snake_case)]

use core::ffi::c_void;

/// Opaque CoreMedia sample handle, shared with VideoToolbox callers.
pub type CMSampleBufferRef = *mut c_void;
use objc::class::Class;
use objc::object::Id;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::time::{self, CMTime};

const REQUIRED_PARAMETER: i32 = -12731;
const INVALID_ENTRY_COUNT: i32 = -12738;
const ALLOCATION_FAILED: i32 = -12730;
const BUFFER_NOT_READY: i32 = -12733;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CMSampleTimingInfo {
    pub duration: CMTime,
    pub presentation_time_stamp: CMTime,
    pub decode_time_stamp: CMTime,
}

type MakeDataReady = unsafe extern "C" fn(*const c_void, *mut c_void) -> i32;

struct Sample {
    data: *const c_void,
    format: *const c_void,
    count: isize,
    timing: Vec<CMSampleTimingInfo>,
    sizes: Vec<usize>,
    ready: AtomicBool,
    making_ready: AtomicBool,
    make_ready: Option<MakeDataReady>,
    refcon: *mut c_void,
    attachments: Mutex<usize>,
}

impl Drop for Sample {
    fn drop(&mut self) {
        // SAFETY: both CF objects have one owned retain from creation.
        unsafe {
            if !self.data.is_null() {
                corefoundation::api::CFRelease(self.data);
            }
            if !self.format.is_null() {
                corefoundation::api::CFRelease(self.format);
            }
            let attachments = *self
                .attachments
                .get_mut()
                .unwrap_or_else(|e| e.into_inner());
            if attachments != 0 {
                corefoundation::api::CFRelease(attachments as *const c_void);
            }
        }
    }
}

#[repr(C)]
struct Body {
    isa: *const Class,
    contents: *mut Sample,
}

fn class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        let class =
            corefoundation::base::register(c"__NSCMSampleBuffer", None, size_of::<Body>(), true);
        class.replace_method(
            objc::sel::get("dealloc"),
            dealloc as *const c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: registered class persists for the lifetime of the process.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("CMSampleBuffer class") }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: objc::sel::Sel) {
    // SAFETY: contents is owned by the instance and freed on final release.
    unsafe {
        let body = receiver as *mut Body;
        if !(*body).contents.is_null() {
            drop(Box::from_raw((*body).contents));
            (*body).contents = core::ptr::null_mut();
        }
        objc::arc::finalize(receiver);
    }
}

fn new_sample(sample: Sample) -> *const c_void {
    // SAFETY: Body layout matches the registered class instance size.
    let body: *mut Body = unsafe { corefoundation::base::allocate(class()) };
    if body.is_null() {
        return core::ptr::null();
    }
    // SAFETY: new instance owns its boxed sample.
    unsafe { (*body).contents = Box::into_raw(Box::new(sample)) };
    body.cast()
}

/// # Safety
/// The handle must be null or a live builtin CF object.
unsafe fn sample<'a>(handle: *const c_void) -> Option<&'a Sample> {
    if handle.is_null() || !unsafe { corefoundation::base::is_kind_of(handle, class()) } {
        return None;
    }
    // SAFETY: class's Body owns a Sample that outlives the handle's retain.
    unsafe { (*(handle as *const Body)).contents.as_ref() }
}

pub extern "C" fn CMSampleBufferGetTypeID() -> usize {
    corefoundation::base::type_id(class())
}

/// Creates a retained sample buffer. Timing and size arrays are copied.
///
/// # Safety
/// Nonnull arrays must hold their declared number of entries. Output must be
/// writable. Callback and refcon must remain callable until readiness, and
/// any nonnull data and format refs must be live builtin CF objects.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CMSampleBufferCreate(
    allocator: *const c_void,
    data: *const c_void,
    data_ready: u8,
    make_ready: Option<MakeDataReady>,
    refcon: *mut c_void,
    format: *const c_void,
    num_samples: isize,
    num_timing_entries: isize,
    timing_array: *const CMSampleTimingInfo,
    num_size_entries: isize,
    size_array: *const usize,
    out: *mut *const c_void,
) -> i32 {
    if out.is_null() {
        return REQUIRED_PARAMETER;
    }
    unsafe { *out = core::ptr::null() };
    if allocator
        == corefoundation::api::standard_allocator(corefoundation::api::StandardAllocator::Null)
    {
        return ALLOCATION_FAILED;
    }
    if !corefoundation::api::supports_malloc_allocator(allocator) {
        return REQUIRED_PARAMETER;
    }
    if num_samples < 0 || num_timing_entries < 0 || num_size_entries < 0 {
        return INVALID_ENTRY_COUNT;
    }
    if ![0, 1, num_samples].contains(&num_timing_entries)
        || ![0, 1, num_samples].contains(&num_size_entries)
    {
        return INVALID_ENTRY_COUNT;
    }
    if (num_timing_entries > 0 && timing_array.is_null())
        || (num_size_entries > 0 && size_array.is_null())
        || (!data.is_null() && !unsafe { crate::block_buffer::is_buffer(data) })
        || (!format.is_null()
            && unsafe { corefoundation::api::CFGetTypeID(format) }
                != unsafe { crate::format_description::CMFormatDescriptionGetTypeID() })
    {
        return REQUIRED_PARAMETER;
    }
    // Empty samples cannot carry per-sample size entries, even without data.
    if num_samples == 0 && num_size_entries != 0 {
        return INVALID_ENTRY_COUNT;
    }

    let timing = if num_timing_entries == 0 {
        Vec::new()
    } else {
        // SAFETY: validated caller-provided readable timing array.
        unsafe { core::slice::from_raw_parts(timing_array, num_timing_entries as usize) }.to_vec()
    };
    let sizes = if num_size_entries == 0 {
        Vec::new()
    } else {
        // SAFETY: validated caller-provided readable sizes array.
        unsafe { core::slice::from_raw_parts(size_array, num_size_entries as usize) }.to_vec()
    };
    // Sample sizes are caller-owned metadata, not a contiguous extent. Apple
    // accepts SIZE_MAX entries even when their sum or product overflows size_t.
    // Actual byte reads remain bounded by CMBlockBuffer's checked operations.

    // Retain dependencies only after validation has succeeded.
    // SAFETY: both references were type-checked above.
    unsafe {
        if !data.is_null() {
            corefoundation::api::CFRetain(data);
        }
        if !format.is_null() {
            corefoundation::api::CFRetain(format);
        }
    }
    let result = new_sample(Sample {
        data,
        format,
        count: num_samples,
        timing,
        sizes,
        ready: AtomicBool::new(data_ready != 0),
        making_ready: AtomicBool::new(false),
        make_ready,
        refcon,
        attachments: Mutex::new(0),
    });
    if result.is_null() {
        return ALLOCATION_FAILED;
    }
    unsafe { *out = result };
    0
}

/// # Safety
/// All pointer inputs must satisfy CMSampleBufferCreate's contract.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CMSampleBufferCreateReady(
    allocator: *const c_void,
    data: *const c_void,
    format: *const c_void,
    num_samples: isize,
    num_timing_entries: isize,
    timing_array: *const CMSampleTimingInfo,
    num_size_entries: isize,
    size_array: *const usize,
    out: *mut *const c_void,
) -> i32 {
    unsafe {
        CMSampleBufferCreate(
            allocator,
            data,
            1,
            None,
            core::ptr::null_mut(),
            format,
            num_samples,
            num_timing_entries,
            timing_array,
            num_size_entries,
            size_array,
            out,
        )
    }
}

/// # Safety
/// Handle must refer to a live builtin CoreFoundation object.
pub unsafe extern "C" fn CMSampleBufferGetDataBuffer(handle: *const c_void) -> *const c_void {
    unsafe { sample(handle) }.map_or(core::ptr::null(), |sample| sample.data)
}

/// # Safety
/// Handle must refer to a live builtin CoreFoundation object.
pub unsafe extern "C" fn CMSampleBufferGetFormatDescription(
    handle: *const c_void,
) -> *const c_void {
    unsafe { sample(handle) }.map_or(core::ptr::null(), |sample| sample.format)
}

/// No image-backed samples are created by these compressed-buffer APIs.
///
/// # Safety
/// Handle must refer to a live builtin CoreFoundation object.
pub unsafe extern "C" fn CMSampleBufferGetImageBuffer(_handle: *const c_void) -> *const c_void {
    core::ptr::null()
}

/// # Safety
/// Handle must refer to a live builtin CoreFoundation object.
pub unsafe extern "C" fn CMSampleBufferGetNumSamples(handle: *const c_void) -> isize {
    unsafe { sample(handle) }.map_or(0, |sample| sample.count)
}

/// # Safety
/// Handle must refer to a live builtin CoreFoundation object.
pub unsafe extern "C" fn CMSampleBufferDataIsReady(handle: *const c_void) -> u8 {
    unsafe { sample(handle) }.is_some_and(|sample| sample.ready.load(Ordering::Acquire)) as u8
}

/// # Safety
/// Handle must refer to a live builtin CoreFoundation object.
pub unsafe extern "C" fn CMSampleBufferIsValid(handle: *const c_void) -> u8 {
    unsafe { sample(handle) }.is_some() as u8
}

/// # Safety
/// Handle must refer to a live builtin CoreFoundation object.
pub unsafe extern "C" fn CMSampleBufferSetDataReady(handle: *const c_void) -> i32 {
    let Some(sample) = (unsafe { sample(handle) }) else {
        return REQUIRED_PARAMETER;
    };
    sample.ready.store(true, Ordering::Release);
    0
}

/// # Safety
/// Caller must own a retain of the buffer while invoking this function.
/// Callback must be substituted by the emulator's existing callback ABI.
pub unsafe extern "C" fn CMSampleBufferMakeDataReady(handle: *const c_void) -> i32 {
    let Some(contents) = (unsafe { sample(handle) }) else {
        return REQUIRED_PARAMETER;
    };
    if contents.ready.load(Ordering::Acquire) {
        return 0;
    }
    let Some(callback) = contents.make_ready else {
        return BUFFER_NOT_READY;
    };
    // Only one caller may invoke readiness at a time; a callback can itself
    // query readiness, and reentry must not recurse indefinitely.
    if contents.making_ready.swap(true, Ordering::AcqRel) {
        return BUFFER_NOT_READY;
    }
    if contents.ready.load(Ordering::Acquire) {
        contents.making_ready.store(false, Ordering::Release);
        return 0;
    }
    // The buffer remains valid if the callback drops another owner's retain.
    unsafe { corefoundation::api::CFRetain(handle) };
    let status = unsafe { callback(handle, contents.refcon) };
    if status == 0 {
        contents.ready.store(true, Ordering::Release);
    }
    contents.making_ready.store(false, Ordering::Release);
    unsafe { corefoundation::api::CFRelease(handle) };
    status
}

/// The earliest timestamp uses CoreMedia's time ordering. Invalid entries
/// are excluded unless all entries are invalid. Among the remaining values,
/// negative infinity precedes finite numeric times, then indefinite, then
/// positive infinity. Numeric times compare exactly by epoch and timescale.
fn earliest_time(
    timing: &[CMSampleTimingInfo],
    get: impl Fn(&CMSampleTimingInfo) -> CMTime,
) -> CMTime {
    fn rank(time: CMTime) -> Option<u8> {
        if time.flags & time::VALID == 0 {
            return None;
        }
        if time.flags & time::NEGATIVE_INFINITY != 0 {
            return Some(0);
        }
        if time.is_numeric() {
            return Some(1);
        }
        if time.flags & time::INDEFINITE != 0 {
            return Some(2);
        }
        if time.flags & time::POSITIVE_INFINITY != 0 {
            return Some(3);
        }
        None
    }

    let mut earliest: Option<CMTime> = None;
    for entry in timing {
        let candidate = get(entry);
        let Some(candidate_rank) = rank(candidate) else {
            continue;
        };
        earliest = match earliest {
            None => Some(candidate),
            Some(current) => {
                let current_rank = rank(current).expect("selected timestamp is valid");
                if candidate_rank < current_rank
                    || (candidate_rank == current_rank
                        && candidate_rank == 1
                        && time::compare_numeric(candidate, current)
                            == Some(core::cmp::Ordering::Less))
                {
                    Some(candidate)
                } else {
                    Some(current)
                }
            }
        };
    }
    earliest.unwrap_or(CMTime::INVALID)
}

/// # Safety
/// Handle must refer to a live builtin CoreFoundation object.
pub unsafe extern "C" fn CMSampleBufferGetPresentationTimeStamp(handle: *const c_void) -> CMTime {
    let Some(contents) = (unsafe { sample(handle) }) else {
        return CMTime::INVALID;
    };
    earliest_time(&contents.timing, |info| info.presentation_time_stamp)
}

/// # Safety
/// Handle must refer to a live builtin CoreFoundation object.
pub unsafe extern "C" fn CMSampleBufferGetDecodeTimeStamp(handle: *const c_void) -> CMTime {
    let Some(contents) = (unsafe { sample(handle) }) else {
        return CMTime::INVALID;
    };
    earliest_time(&contents.timing, |info| info.decode_time_stamp)
}

/// # Safety
/// Handle must refer to a live builtin CoreFoundation object.
pub unsafe extern "C" fn CMSampleBufferGetDuration(handle: *const c_void) -> CMTime {
    let Some(contents) = (unsafe { sample(handle) }) else {
        return CMTime::INVALID;
    };
    let Some(first) = contents.timing.first() else {
        return time::CMTimeMake(0, 1);
    };
    if contents.timing.len() == 1 {
        return time::multiply_time(first.duration, contents.count.max(1));
    }
    let mut total = first.duration;
    for entry in contents.timing.iter().skip(1) {
        total = time::add_time(total, entry.duration);
        if total.flags & time::VALID == 0 {
            return CMTime::INVALID;
        }
    }
    total
}

/// # Safety
/// Handle must refer to a live builtin CoreFoundation object; the returned CF
/// array is borrowed and must not be released without retaining it first.
pub unsafe extern "C" fn CMSampleBufferGetSampleAttachmentsArray(
    handle: *const c_void,
    create_if_necessary: u8,
) -> *const c_void {
    let Some(contents) = (unsafe { sample(handle) }) else {
        return core::ptr::null();
    };
    let mut guard = contents
        .attachments
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if *guard != 0 {
        return *guard as *const c_void;
    }
    if create_if_necessary == 0 || contents.count == 0 {
        return core::ptr::null();
    }
    let mut dictionaries = Vec::new();
    for _ in 0..contents.count {
        // SAFETY: dictionary stores CF objects under full key/value callbacks.
        let dict = unsafe {
            corefoundation::api::CFDictionaryCreateMutable(
                core::ptr::null(),
                0,
                &corefoundation::callbacks::TYPE_KEY_CALLBACKS,
                &corefoundation::callbacks::TYPE_VALUE_CALLBACKS,
            )
        };
        if dict.is_null() {
            for handle in dictionaries {
                unsafe { corefoundation::api::CFRelease(handle) };
            }
            return core::ptr::null();
        }
        dictionaries.push(dict);
    }
    // SAFETY: pointers refer to live dictionary CF objects, retained by array.
    let array = unsafe {
        corefoundation::api::CFArrayCreate(
            core::ptr::null(),
            dictionaries.as_ptr(),
            dictionaries.len() as isize,
            &corefoundation::callbacks::TYPE_VALUE_CALLBACKS,
        )
    };
    for handle in dictionaries {
        unsafe { corefoundation::api::CFRelease(handle) };
    }
    if array.is_null() {
        return core::ptr::null();
    }
    *guard = array as usize;
    array
}

/// # Safety
/// Handle must refer to a live builtin CoreFoundation object.
pub unsafe extern "C" fn CMSampleBufferGetSampleSize(handle: *const c_void, index: isize) -> usize {
    let Some(contents) = (unsafe { sample(handle) }) else {
        return 0;
    };
    if index < 0 || index >= contents.count {
        return 0;
    }
    if contents.sizes.len() == 1 {
        contents.sizes[0]
    } else {
        contents.sizes.get(index as usize).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_buffer::{CMBlockBufferCreateWithMemoryBlock, CMBlockBufferReplaceDataBytes};
    use crate::time::CMTimeMake;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CALLED: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn ready_callback(sbuf: *const c_void, _refcon: *mut c_void) -> i32 {
        CALLED.fetch_add(1, Ordering::SeqCst);
        // SAFETY: the buffer is kept retained through callback invocation.
        unsafe { CMSampleBufferSetDataReady(sbuf) }
    }

    unsafe fn create_block() -> *const c_void {
        let mut block = core::ptr::null();
        assert_eq!(
            unsafe {
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    core::ptr::null_mut(),
                    4,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    4,
                    0,
                    &mut block,
                )
            },
            0
        );
        assert_eq!(
            unsafe { CMBlockBufferReplaceDataBytes(b"abcd".as_ptr().cast(), block, 0, 4) },
            0
        );
        block
    }

    #[test]
    fn retains_block_timing_and_mutable_attachments() {
        let block = unsafe { create_block() };
        let timing = CMSampleTimingInfo {
            duration: CMTimeMake(1, 30),
            presentation_time_stamp: CMTimeMake(5, 30),
            decode_time_stamp: CMTime::INVALID,
        };
        let size = [4usize];
        let mut result = core::ptr::null();
        let before = CALLED.load(Ordering::SeqCst);
        // SAFETY: inputs and outputs are valid, callback has the documented ABI.
        unsafe {
            assert_eq!(
                CMSampleBufferCreate(
                    core::ptr::null(),
                    block,
                    0,
                    Some(ready_callback),
                    core::ptr::null_mut(),
                    core::ptr::null(),
                    1,
                    1,
                    &timing,
                    1,
                    size.as_ptr(),
                    &mut result,
                ),
                0
            );
            assert_eq!(
                corefoundation::api::CFGetTypeID(result),
                CMSampleBufferGetTypeID()
            );
            assert_eq!(CMSampleBufferGetNumSamples(result), 1);
            assert_eq!(CMSampleBufferGetSampleSize(result, 0), 4);
            assert_eq!(CMSampleBufferGetSampleSize(result, 1), 0);
            assert_eq!(CMSampleBufferGetDataBuffer(result), block);
            assert_eq!(
                CMSampleBufferGetPresentationTimeStamp(result),
                timing.presentation_time_stamp
            );
            assert_eq!(CMSampleBufferGetDecodeTimeStamp(result), CMTime::INVALID);
            assert_eq!(CMSampleBufferGetDuration(result), timing.duration);
            assert_eq!(CMSampleBufferDataIsReady(result), 0);
            corefoundation::api::CFRelease(block);
            assert_eq!(crate::block_buffer::CMBlockBufferGetDataLength(block), 4);
            assert_eq!(CMSampleBufferMakeDataReady(result), 0);
            assert_eq!(CMSampleBufferDataIsReady(result), 1);
            assert_eq!(CALLED.load(Ordering::SeqCst), before + 1);
            assert_eq!(CMSampleBufferMakeDataReady(result), 0);
            assert_eq!(CALLED.load(Ordering::SeqCst), before + 1);
            assert!(CMSampleBufferGetSampleAttachmentsArray(result, 0).is_null());
            let attachments = CMSampleBufferGetSampleAttachmentsArray(result, 1);
            assert!(!attachments.is_null());
            assert_eq!(
                attachments,
                CMSampleBufferGetSampleAttachmentsArray(result, 0)
            );
            assert_eq!(corefoundation::api::CFArrayGetCount(attachments), 1);
            let dict = corefoundation::api::CFArrayGetValueAtIndex(attachments, 0);
            assert!(!dict.is_null());
            let key = corefoundation::api::CFStringCreateWithCString(
                core::ptr::null(),
                c"test".as_ptr(),
                0x0800_0100,
            );
            assert!(!key.is_null());
            corefoundation::api::CFDictionarySetValue(dict, key, key);
            assert_eq!(corefoundation::api::CFDictionaryGetValue(dict, key), key);
            corefoundation::api::CFRelease(key);
            corefoundation::api::CFRelease(result);
        }
    }

    #[test]
    fn rejects_non_format_corefoundation_objects() {
        let block = unsafe { create_block() };
        let mut sample = core::ptr::dangling::<c_void>();
        // A valid CoreFoundation object is not necessarily a format description.
        unsafe {
            assert_ne!(
                corefoundation::api::CFGetTypeID(block),
                crate::format_description::CMFormatDescriptionGetTypeID()
            );
            assert_eq!(
                CMSampleBufferCreateReady(
                    core::ptr::null(),
                    block,
                    block,
                    1,
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null(),
                    &mut sample,
                ),
                REQUIRED_PARAMETER
            );
            assert!(sample.is_null());
            corefoundation::api::CFRelease(block);
        }
    }

    #[test]
    fn retains_video_format_description_after_caller_release() {
        const SPS: [u8; 24] = [
            0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16, 0xec, 0x04, 0x40, 0, 0, 0x03, 0, 0x40,
            0, 0, 0x0c, 0x83, 0xc6, 0x0c, 0x65, 0x80,
        ];
        const PPS: [u8; 4] = [0x68, 0xce, 0x06, 0xe2];
        let pointers = [SPS.as_ptr(), PPS.as_ptr()];
        let lengths = [SPS.len(), PPS.len()];
        let mut format = core::ptr::null();
        // SAFETY: input bytes and outputs are valid.
        unsafe {
            assert_eq!(
                crate::format_description::CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    core::ptr::null(),
                    2,
                    pointers.as_ptr(),
                    lengths.as_ptr(),
                    4,
                    &mut format
                ),
                0
            );
            let block = create_block();
            let mut sample = core::ptr::null();
            assert_eq!(
                CMSampleBufferCreate(
                    core::ptr::null(),
                    block,
                    1,
                    None,
                    core::ptr::null_mut(),
                    format,
                    1,
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null(),
                    &mut sample
                ),
                0
            );
            corefoundation::api::CFRelease(format);
            corefoundation::api::CFRelease(block);
            let borrowed = CMSampleBufferGetFormatDescription(sample);
            assert_eq!(borrowed, format);
            assert_eq!(
                crate::format_description::CMFormatDescriptionGetMediaSubType(borrowed),
                u32::from_be_bytes(*b"avc1")
            );
            assert_eq!(
                crate::format_description::CMVideoFormatDescriptionGetDimensions(borrowed),
                crate::format_description::Dimensions {
                    width: 1280,
                    height: 720
                }
            );
            corefoundation::api::CFRelease(sample);
        }
    }

    unsafe extern "C" fn failing_callback(_buffer: *const c_void, _refcon: *mut c_void) -> i32 {
        -4321
    }

    #[test]
    fn readiness_callback_error_does_not_mark_data_ready() {
        let block = unsafe { create_block() };
        let mut sample = core::ptr::null();
        // SAFETY: owned CF block and callback stay valid for the test.
        unsafe {
            assert_eq!(
                CMSampleBufferCreate(
                    core::ptr::null(),
                    block,
                    0,
                    Some(failing_callback),
                    core::ptr::null_mut(),
                    core::ptr::null(),
                    1,
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null(),
                    &mut sample
                ),
                0
            );
            assert_eq!(CMSampleBufferMakeDataReady(sample), -4321);
            assert_eq!(CMSampleBufferDataIsReady(sample), 0);
            corefoundation::api::CFRelease(sample);
            corefoundation::api::CFRelease(block);
        }
    }

    unsafe extern "C" fn reentrant_callback(buffer: *const c_void, _refcon: *mut c_void) -> i32 {
        assert_eq!(
            unsafe { CMSampleBufferMakeDataReady(buffer) },
            BUFFER_NOT_READY
        );
        unsafe { CMSampleBufferSetDataReady(buffer) }
    }

    #[test]
    fn callback_reentry_is_bounded_and_sample_remains_owned() {
        let block = unsafe { create_block() };
        let mut sample = core::ptr::null();
        unsafe {
            assert_eq!(
                CMSampleBufferCreate(
                    core::ptr::null(),
                    block,
                    0,
                    Some(reentrant_callback),
                    core::ptr::null_mut(),
                    core::ptr::null(),
                    1,
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null(),
                    &mut sample,
                ),
                0
            );
            assert_eq!(CMSampleBufferMakeDataReady(sample), 0);
            assert_eq!(CMSampleBufferDataIsReady(sample), 1);
            corefoundation::api::CFRelease(sample);
            corefoundation::api::CFRelease(block);
        }
    }

    #[test]
    fn multi_sample_timing_selects_earliest_and_sums_mixed_scales() {
        let block = unsafe { create_block() };
        let timing = [
            CMSampleTimingInfo {
                duration: CMTimeMake(1, 3),
                presentation_time_stamp: CMTimeMake(3, 2),
                decode_time_stamp: CMTimeMake(7, 10),
            },
            CMSampleTimingInfo {
                duration: CMTimeMake(1, 6),
                presentation_time_stamp: CMTimeMake(1, 3),
                decode_time_stamp: CMTimeMake(1, 2),
            },
            CMSampleTimingInfo {
                duration: CMTimeMake(1, 2),
                presentation_time_stamp: CMTimeMake(7, 6),
                decode_time_stamp: CMTimeMake(3, 4),
            },
        ];
        let mut sample = core::ptr::null();
        unsafe {
            assert_eq!(
                CMSampleBufferCreate(
                    core::ptr::null(),
                    block,
                    1,
                    None,
                    core::ptr::null_mut(),
                    core::ptr::null(),
                    3,
                    3,
                    timing.as_ptr(),
                    0,
                    core::ptr::null(),
                    &mut sample,
                ),
                0
            );
            assert_eq!(
                CMSampleBufferGetPresentationTimeStamp(sample),
                CMTimeMake(1, 3)
            );
            assert_eq!(CMSampleBufferGetDecodeTimeStamp(sample), CMTimeMake(1, 2));
            assert_eq!(
                time::CMTimeGetSeconds(CMSampleBufferGetDuration(sample)),
                1.0
            );
            corefoundation::api::CFRelease(sample);
            corefoundation::api::CFRelease(block);
        }
    }

    #[test]
    fn null_data_ready_samples_and_default_duration_match_native() {
        let mut output = core::ptr::null();
        unsafe {
            assert_eq!(
                CMSampleBufferCreateReady(
                    core::ptr::null(),
                    core::ptr::null(),
                    core::ptr::null(),
                    1,
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null(),
                    &mut output
                ),
                0
            );
            assert!(!output.is_null());
            assert!(CMSampleBufferGetDataBuffer(output).is_null());
            assert_eq!(CMSampleBufferGetDuration(output), CMTimeMake(0, 1));
            corefoundation::api::CFRelease(output);
        }
    }

    #[test]
    fn zero_sample_timing_size_and_attachment_contracts() {
        let duration = CMTimeMake(12, 30);
        let timing = CMSampleTimingInfo {
            duration,
            presentation_time_stamp: CMTimeMake(0, 30),
            decode_time_stamp: CMTime::INVALID,
        };
        let mut output = core::ptr::null();
        unsafe {
            assert_eq!(
                CMSampleBufferCreateReady(
                    core::ptr::null(),
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    1,
                    &timing,
                    0,
                    core::ptr::null(),
                    &mut output
                ),
                0
            );
            assert_eq!(CMSampleBufferGetNumSamples(output), 0);
            assert_eq!(CMSampleBufferGetDuration(output), duration);
            assert!(CMSampleBufferGetSampleAttachmentsArray(output, 0).is_null());
            assert!(CMSampleBufferGetSampleAttachmentsArray(output, 1).is_null());
            corefoundation::api::CFRelease(output);
            let size = [4usize];
            assert_eq!(
                CMSampleBufferCreateReady(
                    core::ptr::null(),
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    0,
                    core::ptr::null(),
                    1,
                    size.as_ptr(),
                    &mut output
                ),
                INVALID_ENTRY_COUNT
            );
            assert!(output.is_null());
        }
    }

    #[test]
    fn sample_duration_uses_native_rounded_addition_and_multiplication() {
        let cases = [
            (
                "coprime timescales",
                [CMTimeMake(1, i32::MAX), CMTimeMake(1, i32::MAX - 1)],
                2,
                2,
                CMTime {
                    value: 0,
                    timescale: 1_000_000_000,
                    flags: time::VALID | time::HAS_BEEN_ROUNDED,
                    epoch: 0,
                },
            ),
            (
                "large timescale numerator",
                [
                    CMTimeMake(i64::MAX, i32::MAX),
                    CMTimeMake(i64::MAX, i32::MAX),
                ],
                2,
                2,
                CMTime {
                    value: 9223372032559808508,
                    timescale: 1073741823,
                    flags: 3,
                    epoch: 0,
                },
            ),
            (
                "one timing repeated overflows",
                [CMTimeMake(i64::MAX, 1), CMTimeMake(0, 1)],
                1,
                2,
                CMTime {
                    value: 0,
                    timescale: 0,
                    flags: 5,
                    epoch: 0,
                },
            ),
            (
                "one timing rescaled",
                [CMTimeMake(i64::MAX, i32::MAX), CMTimeMake(0, 1)],
                1,
                2,
                CMTime {
                    value: 9223372032559808508,
                    timescale: 1073741823,
                    flags: 3,
                    epoch: 0,
                },
            ),
        ];
        let block = unsafe { create_block() };
        for (label, durations, timing_count, sample_count, expected) in cases {
            let timing = durations.map(|duration| CMSampleTimingInfo {
                duration,
                presentation_time_stamp: CMTimeMake(0, 1),
                decode_time_stamp: CMTime::INVALID,
            });
            let mut sample = core::ptr::null();
            unsafe {
                assert_eq!(
                    CMSampleBufferCreateReady(
                        core::ptr::null(),
                        block,
                        core::ptr::null(),
                        sample_count,
                        timing_count,
                        timing.as_ptr(),
                        0,
                        core::ptr::null(),
                        &mut sample,
                    ),
                    0,
                    "{label}"
                );
                assert_eq!(CMSampleBufferGetDuration(sample), expected, "{label}");
                corefoundation::api::CFRelease(sample);
            }
        }
        unsafe { corefoundation::api::CFRelease(block) };
    }

    #[test]
    fn mixed_special_timestamps_follow_native_coremedia_order_for_pts_and_dts() {
        // Apple's two-entry oracle establishes this ordering: negative
        // infinity < numeric < indefinite < positive infinity, while invalid
        // timestamps are skipped. Numeric timestamps compare epochs first.
        let invalid = CMTime::INVALID;
        let indefinite = CMTime {
            flags: time::VALID | time::INDEFINITE,
            ..invalid
        };
        let positive = CMTime {
            flags: time::VALID | time::POSITIVE_INFINITY,
            ..invalid
        };
        let negative = CMTime {
            flags: time::VALID | time::NEGATIVE_INFINITY,
            ..invalid
        };
        let numeric = time::CMTimeMake(1, 30);
        let numeric_neg = time::CMTimeMake(-1, 30);
        let zero = time::CMTimeMake(0, 1);
        let epoch7 = CMTime {
            epoch: 7,
            ..numeric
        };
        let epoch8 = CMTime {
            epoch: 8,
            ..numeric
        };
        let times = [
            invalid,
            indefinite,
            positive,
            negative,
            numeric,
            numeric_neg,
            zero,
            epoch7,
            epoch8,
        ];
        let rank = [4u8, 2, 3, 0, 1, 1, 1, 1, 1];
        for (i, &a) in times.iter().enumerate() {
            for (j, &b) in times.iter().enumerate() {
                let expected = if i == 0 {
                    b
                } else if j == 0 {
                    a
                } else if rank[i] != rank[j] {
                    if rank[i] < rank[j] { a } else { b }
                } else if rank[i] == 1
                    && time::compare_numeric(b, a) == Some(core::cmp::Ordering::Less)
                {
                    b
                } else {
                    a
                };
                let timing = [a, b].map(|stamp| CMSampleTimingInfo {
                    duration: time::CMTimeMake(1, 30),
                    presentation_time_stamp: stamp,
                    decode_time_stamp: stamp,
                });
                let mut output = core::ptr::null();
                unsafe {
                    assert_eq!(
                        CMSampleBufferCreateReady(
                            core::ptr::null(),
                            core::ptr::null(),
                            core::ptr::null(),
                            2,
                            2,
                            timing.as_ptr(),
                            0,
                            core::ptr::null(),
                            &mut output,
                        ),
                        0,
                        "construct {i}, {j}"
                    );
                    assert_eq!(
                        CMSampleBufferGetPresentationTimeStamp(output),
                        expected,
                        "PTS {i}, {j}"
                    );
                    assert_eq!(
                        CMSampleBufferGetDecodeTimeStamp(output),
                        expected,
                        "DTS {i}, {j}"
                    );
                    corefoundation::api::CFRelease(output);
                }
            }
        }
        // The first invalid entry must not hide a later finite timestamp, and
        // an invalid-only sample must remain invalid.
        assert_eq!(earliest_time(&[], |x| x.duration), CMTime::INVALID);
    }

    #[test]
    fn overflowing_sample_size_metadata_is_preserved_exactly() {
        let block = unsafe { create_block() };
        let huge = usize::MAX;
        unsafe {
            for (count, sizes) in [(1, [huge, 0]), (2, [huge, 1]), (2, [huge, huge])] {
                let mut output = core::ptr::null();
                assert_eq!(
                    CMSampleBufferCreateReady(
                        core::ptr::null(),
                        block,
                        core::ptr::null(),
                        2,
                        0,
                        core::ptr::null(),
                        count,
                        sizes.as_ptr(),
                        &mut output,
                    ),
                    0,
                    "{count} entries {sizes:?}"
                );
                assert_eq!(CMSampleBufferGetSampleSize(output, 0), huge);
                assert_eq!(
                    CMSampleBufferGetSampleSize(output, 1),
                    if count == 1 { huge } else { sizes[1] }
                );
                let mut bytes = [0u8; 4];
                assert_eq!(
                    crate::block_buffer::CMBlockBufferCopyDataBytes(
                        block,
                        0,
                        4,
                        bytes.as_mut_ptr().cast(),
                    ),
                    0
                );
                corefoundation::api::CFRelease(output);
            }
            corefoundation::api::CFRelease(block);
            let mut output = core::ptr::null();
            assert_eq!(
                CMSampleBufferCreateReady(
                    corefoundation::api::standard_allocator(
                        corefoundation::api::StandardAllocator::Null
                    ),
                    core::ptr::null(),
                    core::ptr::null(),
                    1,
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null(),
                    &mut output,
                ),
                ALLOCATION_FAILED
            );
            assert!(output.is_null());
        }
    }

    #[test]
    fn sample_duration_matches_native_epoch_rounding_and_intermediate_overflow() {
        let cases = [
            (
                "epoch neutral lhs",
                CMTimeMake(1, 2),
                CMTime {
                    epoch: 7,
                    ..CMTimeMake(1, 3)
                },
                CMTime {
                    epoch: 7,
                    ..CMTimeMake(5, 6)
                },
            ),
            (
                "epoch neutral rhs",
                CMTime {
                    epoch: 7,
                    ..CMTimeMake(1, 2)
                },
                CMTimeMake(1, 3),
                CMTime {
                    epoch: 7,
                    ..CMTimeMake(5, 6)
                },
            ),
            (
                "matching nonzero epochs",
                CMTime {
                    epoch: 7,
                    ..CMTimeMake(1, 2)
                },
                CMTime {
                    epoch: 7,
                    ..CMTimeMake(1, 3)
                },
                CMTimeMake(5, 6),
            ),
            (
                "different nonzero epochs",
                CMTime {
                    epoch: 7,
                    ..CMTimeMake(1, 2)
                },
                CMTime {
                    epoch: 8,
                    ..CMTimeMake(1, 3)
                },
                CMTime::INVALID,
            ),
            (
                "rounded zero lhs",
                CMTime {
                    value: 0,
                    timescale: 2,
                    flags: 3,
                    epoch: 0,
                },
                CMTimeMake(1, 3),
                CMTime {
                    flags: 3,
                    ..CMTimeMake(1, 3)
                },
            ),
            (
                "rounded zero rhs",
                CMTimeMake(1, 3),
                CMTime {
                    value: 0,
                    timescale: 2,
                    flags: 3,
                    epoch: 0,
                },
                CMTime {
                    flags: 3,
                    ..CMTimeMake(1, 3)
                },
            ),
            (
                "overflow during conversion",
                CMTimeMake(-5420539530132427298, 7),
                CMTimeMake(2375011267000674195, 2),
                CMTime {
                    value: 2891999904369932385,
                    timescale: 7,
                    flags: 3,
                    epoch: 0,
                },
            ),
        ];
        for (label, a, b, expected) in cases {
            let timing = [a, b].map(|duration| CMSampleTimingInfo {
                duration,
                presentation_time_stamp: CMTimeMake(0, 1),
                decode_time_stamp: CMTime::INVALID,
            });
            let mut output = core::ptr::null();
            unsafe {
                assert_eq!(
                    CMSampleBufferCreateReady(
                        core::ptr::null(),
                        core::ptr::null(),
                        core::ptr::null(),
                        2,
                        2,
                        timing.as_ptr(),
                        0,
                        core::ptr::null(),
                        &mut output,
                    ),
                    0,
                    "{label}"
                );
                assert_eq!(CMSampleBufferGetDuration(output), expected, "{label}");
                corefoundation::api::CFRelease(output);
            }
        }
    }

    #[test]
    fn duration_preserves_native_coremedia_special_states() {
        let block = unsafe { create_block() };
        let numeric = CMTimeMake(2, 30);
        let indefinite = CMTime {
            flags: time::VALID | time::INDEFINITE,
            ..CMTime::INVALID
        };
        let positive = CMTime {
            flags: time::VALID | time::POSITIVE_INFINITY,
            ..CMTime::INVALID
        };
        let negative = CMTime {
            flags: time::VALID | time::NEGATIVE_INFINITY,
            ..CMTime::INVALID
        };
        let cases = [
            ("indefinite", indefinite, indefinite),
            ("positive infinity", positive, positive),
            ("negative infinity", negative, negative),
            ("invalid", CMTime::INVALID, CMTime::INVALID),
            ("numeric", numeric, numeric),
        ];
        for (label, duration, expected) in cases {
            let timing = CMSampleTimingInfo {
                duration,
                presentation_time_stamp: CMTimeMake(0, 30),
                decode_time_stamp: CMTime::INVALID,
            };
            let mut sample = core::ptr::null();
            unsafe {
                assert_eq!(
                    CMSampleBufferCreateReady(
                        core::ptr::null(),
                        block,
                        core::ptr::null(),
                        1,
                        1,
                        &timing,
                        0,
                        core::ptr::null(),
                        &mut sample,
                    ),
                    0,
                    "{label}"
                );
                assert_eq!(CMSampleBufferGetDuration(sample), expected, "{label}");
                corefoundation::api::CFRelease(sample);
            }
        }
        let cases = [
            ("indefinite + numeric", indefinite, numeric, indefinite),
            ("numeric + indefinite", numeric, indefinite, indefinite),
            ("positive + numeric", positive, numeric, positive),
            ("negative + numeric", negative, numeric, negative),
            ("opposite infinities", positive, negative, CMTime::INVALID),
            (
                "invalid + numeric",
                CMTime::INVALID,
                numeric,
                CMTime::INVALID,
            ),
            (
                "numeric + invalid",
                numeric,
                CMTime::INVALID,
                CMTime::INVALID,
            ),
            ("indefinite + positive", indefinite, positive, positive),
            ("positive + indefinite", positive, indefinite, positive),
            ("indefinite + negative", indefinite, negative, negative),
            ("positive + positive", positive, positive, positive),
            ("negative + negative", negative, negative, negative),
            ("two numeric", numeric, numeric, CMTimeMake(4, 30)),
        ];
        for (label, first, second, expected) in cases {
            let timing = [first, second].map(|duration| CMSampleTimingInfo {
                duration,
                presentation_time_stamp: CMTimeMake(0, 30),
                decode_time_stamp: CMTime::INVALID,
            });
            let mut sample = core::ptr::null();
            unsafe {
                assert_eq!(
                    CMSampleBufferCreateReady(
                        core::ptr::null(),
                        block,
                        core::ptr::null(),
                        2,
                        2,
                        timing.as_ptr(),
                        0,
                        core::ptr::null(),
                        &mut sample,
                    ),
                    0,
                    "{label}"
                );
                assert_eq!(CMSampleBufferGetDuration(sample), expected, "{label}");
                corefoundation::api::CFRelease(sample);
            }
        }
        unsafe { corefoundation::api::CFRelease(block) };
    }

    #[test]
    fn refuses_unsupported_sample_structure_allocator() {
        let mut sample = core::ptr::dangling::<c_void>();
        unsafe {
            assert_eq!(
                CMSampleBufferCreate(
                    core::ptr::dangling(),
                    core::ptr::null(),
                    1,
                    None,
                    core::ptr::null_mut(),
                    core::ptr::null(),
                    0,
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null(),
                    &mut sample,
                ),
                REQUIRED_PARAMETER
            );
        }
        assert!(sample.is_null());
    }

    #[test]
    fn accepts_standard_malloc_sample_allocator_and_rejects_null() {
        use corefoundation::api::{StandardAllocator, standard_allocator};
        let mut sample = core::ptr::null();
        unsafe {
            assert_eq!(
                CMSampleBufferCreate(
                    standard_allocator(StandardAllocator::Malloc),
                    core::ptr::null(),
                    1,
                    None,
                    core::ptr::null_mut(),
                    core::ptr::null(),
                    0,
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null(),
                    &mut sample,
                ),
                0
            );
            assert_eq!(
                corefoundation::api::CFGetTypeID(sample),
                CMSampleBufferGetTypeID()
            );
            corefoundation::api::CFRelease(sample);
            assert_eq!(
                CMSampleBufferCreate(
                    standard_allocator(StandardAllocator::Null),
                    core::ptr::null(),
                    1,
                    None,
                    core::ptr::null_mut(),
                    core::ptr::null(),
                    0,
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null(),
                    &mut sample,
                ),
                ALLOCATION_FAILED
            );
            assert!(sample.is_null());
        }
    }

    #[test]
    fn validates_sizes_counts_and_readiness() {
        let block = unsafe { create_block() };
        let mut result = core::ptr::dangling::<c_void>();
        // SAFETY: invalid arrays are rejected before dereference.
        unsafe {
            assert_eq!(
                CMSampleBufferCreate(
                    core::ptr::null(),
                    block,
                    1,
                    None,
                    core::ptr::null_mut(),
                    core::ptr::null(),
                    2,
                    3,
                    core::ptr::null(),
                    0,
                    core::ptr::null(),
                    &mut result
                ),
                INVALID_ENTRY_COUNT
            );
            assert!(result.is_null());
            assert_eq!(
                CMSampleBufferCreate(
                    core::ptr::null(),
                    block,
                    1,
                    None,
                    core::ptr::null_mut(),
                    core::ptr::null(),
                    1,
                    1,
                    core::ptr::null(),
                    0,
                    core::ptr::null(),
                    &mut result
                ),
                REQUIRED_PARAMETER
            );
            // Metadata may declare more bytes than the block currently holds.
            let oversize = [5usize];
            assert_eq!(
                CMSampleBufferCreateReady(
                    core::ptr::null(),
                    block,
                    core::ptr::null(),
                    1,
                    0,
                    core::ptr::null(),
                    1,
                    oversize.as_ptr(),
                    &mut result
                ),
                0
            );
            assert_eq!(CMSampleBufferGetSampleSize(result, 0), 5);
            assert_eq!(CMSampleBufferGetDataBuffer(result), block);
            let mut destination = [0u8; 5];
            assert_ne!(
                crate::block_buffer::CMBlockBufferCopyDataBytes(
                    block,
                    0,
                    5,
                    destination.as_mut_ptr().cast()
                ),
                0
            );
            corefoundation::api::CFRelease(result);
            let overflowing = [usize::MAX, 1];
            assert_eq!(
                CMSampleBufferCreateReady(
                    core::ptr::null(),
                    block,
                    core::ptr::null(),
                    2,
                    0,
                    core::ptr::null(),
                    2,
                    overflowing.as_ptr(),
                    &mut result
                ),
                0
            );
            assert_eq!(CMSampleBufferGetSampleSize(result, 0), usize::MAX);
            assert_eq!(CMSampleBufferGetSampleSize(result, 1), 1);
            corefoundation::api::CFRelease(result);
            assert_eq!(
                CMSampleBufferCreate(
                    core::ptr::null(),
                    block,
                    0,
                    None,
                    core::ptr::null_mut(),
                    core::ptr::null(),
                    1,
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null(),
                    &mut result
                ),
                0
            );
            assert_eq!(CMSampleBufferMakeDataReady(result), BUFFER_NOT_READY);
            assert_eq!(CMSampleBufferDataIsReady(result), 0);
            assert_eq!(CMSampleBufferSetDataReady(result), 0);
            assert_eq!(CMSampleBufferDataIsReady(result), 1);
            corefoundation::api::CFRelease(result);
            corefoundation::api::CFRelease(block);
        }
    }
}

exports::exports! {
    functions {
        "CMSampleBufferGetTypeID" => CMSampleBufferGetTypeID,
        "CMSampleBufferCreate" => CMSampleBufferCreate,
        "CMSampleBufferCreateReady" => CMSampleBufferCreateReady,
        "CMSampleBufferDataIsReady" => CMSampleBufferDataIsReady,
        "CMSampleBufferIsValid" => CMSampleBufferIsValid,
        "CMSampleBufferMakeDataReady" => CMSampleBufferMakeDataReady,
        "CMSampleBufferSetDataReady" => CMSampleBufferSetDataReady,
        "CMSampleBufferGetDataBuffer" => CMSampleBufferGetDataBuffer,
        "CMSampleBufferGetFormatDescription" => CMSampleBufferGetFormatDescription,
        "CMSampleBufferGetImageBuffer" => CMSampleBufferGetImageBuffer,
        "CMSampleBufferGetNumSamples" => CMSampleBufferGetNumSamples,
        "CMSampleBufferGetSampleAttachmentsArray" => CMSampleBufferGetSampleAttachmentsArray,
        "CMSampleBufferGetSampleSize" => CMSampleBufferGetSampleSize,
        "CMSampleBufferGetPresentationTimeStamp" => CMSampleBufferGetPresentationTimeStamp,
        "CMSampleBufferGetDecodeTimeStamp" => CMSampleBufferGetDecodeTimeStamp,
        "CMSampleBufferGetDuration" => CMSampleBufferGetDuration,
    }
}
