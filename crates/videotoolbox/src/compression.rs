//! Compression sessions, of which there are none: no encoder exists here.
//!
//! `VTCompressionSessionCreate` answers as VideoToolbox does on a machine
//! without the codec, `kVTCouldNotFindVideoEncoderErr`, which a program
//! already handles by encoding some other way. Every other entry point only
//! ever receives null or a foreign pointer in place of a session. What a
//! session would be configured with is published so that building the
//! request works.

#![allow(non_snake_case)]

use std::ffi::c_void;

use corefoundation::api::CFAllocatorRef;
use corefoundation::base::CFTypeRef;
use coremedia::CMTime;

use crate::constants::{kVTCouldNotFindVideoEncoderErr, kVTInvalidSessionErr, kVTParameterErr};
use crate::types::OSStatus;

pub(crate) const KEYS: &[(&str, &str)] = &[
    (
        "_kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder",
        "EnableHardwareAcceleratedVideoEncoder",
    ),
    (
        "_kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder",
        "RequireHardwareAcceleratedVideoEncoder",
    ),
    (
        "_kVTCompressionPropertyKey_AllowFrameReordering",
        "AllowFrameReordering",
    ),
    (
        "_kVTCompressionPropertyKey_AverageBitRate",
        "AverageBitRate",
    ),
    (
        "_kVTCompressionPropertyKey_DataRateLimits",
        "DataRateLimits",
    ),
    (
        "_kVTCompressionPropertyKey_ExpectedFrameRate",
        "ExpectedFrameRate",
    ),
    (
        "_kVTCompressionPropertyKey_MaxFrameDelayCount",
        "MaxFrameDelayCount",
    ),
    (
        "_kVTCompressionPropertyKey_MaxKeyFrameInterval",
        "MaxKeyFrameInterval",
    ),
    (
        "_kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration",
        "MaxKeyFrameIntervalDuration",
    ),
    ("_kVTCompressionPropertyKey_ProfileLevel", "ProfileLevel"),
    ("_kVTCompressionPropertyKey_RealTime", "RealTime"),
    (
        "_kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder",
        "UsingHardwareAcceleratedVideoEncoder",
    ),
    ("_kVTEncodeFrameOptionKey_ForceKeyFrame", "ForceKeyFrame"),
    (
        "_kVTProfileLevel_H264_Baseline_AutoLevel",
        "H264_Baseline_AutoLevel",
    ),
    (
        "_kVTProfileLevel_H264_Main_AutoLevel",
        "H264_Main_AutoLevel",
    ),
    (
        "_kVTProfileLevel_H264_High_AutoLevel",
        "H264_High_AutoLevel",
    ),
    ("_kVTVideoEncoderList_CodecType", "CodecType"),
    ("_kVTVideoEncoderList_EncoderName", "EncoderName"),
    (
        "_kVTVideoEncoderList_IsHardwareAccelerated",
        "IsHardwareAccelerated",
    ),
];

/// `VTCompressionOutputCallback`, never called.
pub type VTCompressionOutputCallback = Option<unsafe extern "C" fn()>;

/// `OSStatus VTCompressionSessionCreate(CFAllocatorRef allocator, int32_t
/// width, int32_t height, CMVideoCodecType codecType, CFDictionaryRef
/// encoderSpecification, CFDictionaryRef sourceImageBufferAttributes,
/// CFAllocatorRef compressedDataAllocator, VTCompressionOutputCallback
/// outputCallback, void *outputCallbackRefCon, VTCompressionSessionRef
/// *compressionSessionOut)`
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn VTCompressionSessionCreate(
    _allocator: CFAllocatorRef,
    width: i32,
    height: i32,
    _codec: u32,
    _specification: CFTypeRef,
    _attributes: CFTypeRef,
    _compressed_allocator: CFAllocatorRef,
    _callback: VTCompressionOutputCallback,
    _refcon: *mut c_void,
    out: *mut CFTypeRef,
) -> OSStatus {
    if out.is_null() || width <= 0 || height <= 0 {
        return kVTParameterErr;
    }
    // SAFETY: the caller's out-parameter, checked above.
    unsafe { out.write(std::ptr::null()) };
    kVTCouldNotFindVideoEncoderErr
}

/// `CVPixelBufferPoolRef VTCompressionSessionGetPixelBufferPool(
/// VTCompressionSessionRef)`: no session exists to own one.
pub extern "C" fn VTCompressionSessionGetPixelBufferPool(_session: CFTypeRef) -> CFTypeRef {
    std::ptr::null()
}

/// `void VTCompressionSessionInvalidate(VTCompressionSessionRef)`
pub extern "C" fn VTCompressionSessionInvalidate(_session: CFTypeRef) {}

/// `OSStatus VTCompressionSessionPrepareToEncodeFrames(VTCompressionSessionRef)`
pub extern "C" fn VTCompressionSessionPrepareToEncodeFrames(_session: CFTypeRef) -> OSStatus {
    kVTInvalidSessionErr
}

/// `OSStatus VTCompressionSessionCompleteFrames(VTCompressionSessionRef,
/// CMTime completeUntilPresentationTimeStamp)`. The time travels in memory;
/// `rosemary`'s CoreMedia shim moves it from the guest's stack.
pub extern "C" fn VTCompressionSessionCompleteFrames(
    _session: CFTypeRef,
    _complete_until: CMTime,
) -> OSStatus {
    kVTInvalidSessionErr
}

/// `OSStatus VTCompressionSessionEncodeFrame(VTCompressionSessionRef,
/// CVImageBufferRef imageBuffer, CMTime presentationTimeStamp, CMTime
/// duration, CFDictionaryRef frameProperties, void *sourceFrameRefcon,
/// VTEncodeInfoFlags *infoFlagsOut)`
pub unsafe extern "C" fn VTCompressionSessionEncodeFrame(
    _session: CFTypeRef,
    _image: CFTypeRef,
    _presentation_time_stamp: CMTime,
    _duration: CMTime,
    _frame_properties: CFTypeRef,
    _source_frame_refcon: *mut c_void,
    info_flags_out: *mut u32,
) -> OSStatus {
    if !info_flags_out.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { info_flags_out.write(0) };
    }
    kVTInvalidSessionErr
}

/// `OSStatus VTCopyVideoEncoderList(CFDictionaryRef options, CFArrayRef
/// *listOfVideoEncodersOut)`: no encoder exists, so the list is empty, and the
/// caller owns it.
pub unsafe extern "C" fn VTCopyVideoEncoderList(
    _options: CFTypeRef,
    out: *mut CFTypeRef,
) -> OSStatus {
    if out.is_null() {
        return kVTParameterErr;
    }
    // SAFETY: an empty array of CF objects.
    let list = unsafe {
        corefoundation::api::CFArrayCreate(
            std::ptr::null(),
            std::ptr::null(),
            0,
            &corefoundation::callbacks::TYPE_VALUE_CALLBACKS,
        )
    };
    // SAFETY: the caller's out-parameter, checked above.
    unsafe { out.write(list) };
    0
}

exports::exports! {
    functions {
        "VTCompressionSessionCreate" => VTCompressionSessionCreate,
        "VTCompressionSessionGetPixelBufferPool" => VTCompressionSessionGetPixelBufferPool,
        "VTCompressionSessionInvalidate" => VTCompressionSessionInvalidate,
        "VTCompressionSessionPrepareToEncodeFrames" => VTCompressionSessionPrepareToEncodeFrames,
        "VTCompressionSessionCompleteFrames" => VTCompressionSessionCompleteFrames,
        "VTCompressionSessionEncodeFrame" => VTCompressionSessionEncodeFrame,
        "VTCopyVideoEncoderList" => VTCopyVideoEncoderList,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A non-null pointer nothing here made.
    const FOREIGN_SESSION: CFTypeRef = 0x1234 as CFTypeRef;

    #[test]
    fn a_compression_session_finds_no_encoder() {
        let mut session: CFTypeRef = FOREIGN_SESSION;
        // SAFETY: a live out-parameter.
        let status = unsafe {
            VTCompressionSessionCreate(
                std::ptr::null(),
                1280,
                720,
                u32::from_be_bytes(*b"avc1"),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                None,
                std::ptr::null_mut(),
                &mut session,
            )
        };
        assert_eq!(status, kVTCouldNotFindVideoEncoderErr);
        assert!(session.is_null());
    }

    #[test]
    fn a_call_without_a_real_session_is_invalid() {
        let time = coremedia::CMTimeMake(0, 1);
        let mut info = u32::MAX;
        assert!(VTCompressionSessionGetPixelBufferPool(FOREIGN_SESSION).is_null());
        assert_eq!(
            VTCompressionSessionPrepareToEncodeFrames(FOREIGN_SESSION),
            kVTInvalidSessionErr
        );
        assert_eq!(
            VTCompressionSessionCompleteFrames(FOREIGN_SESSION, time),
            kVTInvalidSessionErr
        );
        // SAFETY: a live out-parameter.
        let status = unsafe {
            VTCompressionSessionEncodeFrame(
                FOREIGN_SESSION,
                std::ptr::null(),
                time,
                time,
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut info,
            )
        };
        assert_eq!((status, info), (kVTInvalidSessionErr, 0));
        VTCompressionSessionInvalidate(FOREIGN_SESSION);
    }

    #[test]
    fn the_encoder_list_is_empty_and_the_callers() {
        corefoundation::bootstrap();
        let mut list: CFTypeRef = std::ptr::null();
        // SAFETY: a live out-parameter, then the array it received.
        unsafe {
            assert_eq!(VTCopyVideoEncoderList(std::ptr::null(), &mut list), 0);
            assert_eq!(
                VTCopyVideoEncoderList(std::ptr::null(), std::ptr::null_mut()),
                kVTParameterErr
            );
            assert_eq!(corefoundation::api::CFArrayGetCount(list), 0);
            corefoundation::api::CFRelease(list);
        }
    }
}
