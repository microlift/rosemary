#![allow(non_snake_case)]

use crate::constants::{kVTCouldNotFindVideoDecoderErr, kVTVideoDecoderMalfunctionErr};
use crate::types::{OSStatus, VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionSessionRef};
use corefoundation::{api::CFAllocatorRef, collections::CFDictionaryRef};
use coremedia::format_description::CMVideoFormatDescriptionRef;
use coremedia::sample_buffer::CMSampleBufferRef;

pub(crate) const KEYS: &[(&str, &str)] = &[
    (
        "_kVTCompressionPropertyKey_AllowFrameReordering",
        "AllowFrameReordering",
    ),
    (
        "_kVTCompressionPropertyKey_AverageBitRate",
        "AverageBitRate",
    ),
    (
        "_kVTCompressionPropertyKey_MaxKeyFrameInterval",
        "MaxKeyFrameInterval",
    ),
    ("_kVTCompressionPropertyKey_RealTime", "RealTime"),
    (
        "_kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder",
        "UsingHardwareAcceleratedVideoEncoder",
    ),
    ("_kVTDecompressionPropertyKey_RealTime", "RealTime"),
    (
        "_kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder",
        "UsingHardwareAcceleratedVideoDecoder",
    ),
    (
        "_kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder",
        "EnableHardwareAcceleratedVideoDecoder",
    ),
    (
        "_kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder",
        "RequireHardwareAcceleratedVideoDecoder",
    ),
    ("_kVTVideoEncoderList_CodecType", "CodecType"),
];

#[repr(C)]
pub struct VTDecompressionOutputCallbackRecord {
    pub decompression_output_callback: Option<unsafe extern "C" fn()>,
    pub decompression_output_ref_con: *mut std::ffi::c_void,
}

pub unsafe extern "C" fn VTDecompressionSessionCreate(
    _allocator: CFAllocatorRef,
    _videoFormatDescription: CMVideoFormatDescriptionRef,
    _videoDecoderSpecification: CFDictionaryRef,
    _destinationImageBufferAttributes: CFDictionaryRef,
    _outputCallback: *const VTDecompressionOutputCallbackRecord,
    _decompressionSessionOut: *mut VTDecompressionSessionRef,
) -> OSStatus {
    kVTCouldNotFindVideoDecoderErr
}

pub unsafe extern "C" fn VTDecompressionSessionDecodeFrame(
    _session: VTDecompressionSessionRef,
    _sampleBuffer: CMSampleBufferRef,
    _decodeFlags: VTDecodeFrameFlags,
    _sourceFrameRefCon: *mut std::ffi::c_void,
    _infoFlagsOut: *mut VTDecodeInfoFlags,
) -> OSStatus {
    kVTVideoDecoderMalfunctionErr
}

pub unsafe extern "C" fn VTDecompressionSessionInvalidate(_session: VTDecompressionSessionRef) {}

pub unsafe extern "C" fn VTDecompressionSessionWaitForAsynchronousFrames(
    _session: VTDecompressionSessionRef,
) -> OSStatus {
    kVTVideoDecoderMalfunctionErr
}

pub unsafe extern "C" fn VTDecompressionSessionCanAcceptFormatDescription(
    _session: VTDecompressionSessionRef,
    _newFormatDescription: CMVideoFormatDescriptionRef,
) -> bool {
    false
}

exports::exports! {
    functions {
        "VTDecompressionSessionCreate" => VTDecompressionSessionCreate,
        "VTDecompressionSessionDecodeFrame" => VTDecompressionSessionDecodeFrame,
        "VTDecompressionSessionInvalidate" => VTDecompressionSessionInvalidate,
        "VTDecompressionSessionWaitForAsynchronousFrames" => VTDecompressionSessionWaitForAsynchronousFrames,
        "VTDecompressionSessionCanAcceptFormatDescription" => VTDecompressionSessionCanAcceptFormatDescription,
    }
}
