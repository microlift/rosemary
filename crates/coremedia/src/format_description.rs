//! `CMFormatDescription`, for H.264 video.
//!
//! The extensions dictionary carries what a decoder or a guest reads back: the
//! `avcC` atom, and the VUI fields CoreMedia mirrors as CoreVideo attachment
//! keys. Apple also records several QuickTime sample-description fields
//! (version, revision, quality) that nothing decodes, which are not made here.

#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::OnceLock;

use corefoundation::api::CFAllocatorRef;
use corefoundation::base::{CFTypeRef, interned_name};
use corefoundation::callbacks::Policy;
use corefoundation::collections::{self, Number, number_type};
use objc::class::Class;

use crate::h264;

pub type CMVideoFormatDescriptionRef = *mut c_void;

/// `kCMFormatDescriptionError_InvalidParameter`.
const INVALID_PARAMETER: i32 = -12710;
/// `kCMFormatDescriptionError_AllocationFailed`.
const ALLOCATION_FAILED: i32 = -12711;
/// `kCMFormatDescriptionBridgeError_InvalidParameter`, which is what a
/// parameter set CoreMedia cannot read gets.
const BRIDGE_INVALID_PARAMETER: i32 = -12712;
/// `kCMFormatDescriptionBridgeError_IncompatibleFormatDescription`.
const INCOMPATIBLE: i32 = -12716;
const SUCCESS: i32 = 0;

const VIDEO: u32 = u32::from_be_bytes(*b"vide");
const AVC1: u32 = u32::from_be_bytes(*b"avc1");

/// The `avcC` record's count fields: five bits of SPS, eight of PPS.
const MAX_SPS: usize = 31;
const MAX_PPS: usize = 255;

/// The extension keys and values this module writes, with their exported
/// symbols. The texts are Apple's: a guest compares against the import.
pub(crate) const KEYS: &[(&str, &str)] = &[
    (
        "_kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms",
        "SampleDescriptionExtensionAtoms",
    ),
    ("_kCMFormatDescriptionExtension_FormatName", "FormatName"),
    ("_kCMFormatDescriptionExtension_Depth", "Depth"),
    (
        "_kCMFormatDescriptionExtension_FullRangeVideo",
        "FullRangeVideo",
    ),
    (
        "_kCMFormatDescriptionExtension_PixelAspectRatio",
        "CVPixelAspectRatio",
    ),
    (
        "_kCMFormatDescriptionKey_PixelAspectRatioHorizontalSpacing",
        "HorizontalSpacing",
    ),
    (
        "_kCMFormatDescriptionKey_PixelAspectRatioVerticalSpacing",
        "VerticalSpacing",
    ),
    (
        "_kCMFormatDescriptionExtension_ColorPrimaries",
        "CVImageBufferColorPrimaries",
    ),
    (
        "_kCMFormatDescriptionExtension_TransferFunction",
        "CVImageBufferTransferFunction",
    ),
    (
        "_kCMFormatDescriptionExtension_YCbCrMatrix",
        "CVImageBufferYCbCrMatrix",
    ),
    (
        "_kCMFormatDescriptionColorPrimaries_ITU_R_709_2",
        "ITU_R_709_2",
    ),
    ("_kCMFormatDescriptionColorPrimaries_EBU_3213", "EBU_3213"),
    ("_kCMFormatDescriptionColorPrimaries_SMPTE_C", "SMPTE_C"),
    (
        "_kCMFormatDescriptionColorPrimaries_ITU_R_2020",
        "ITU_R_2020",
    ),
    (
        "_kCMFormatDescriptionTransferFunction_ITU_R_709_2",
        "ITU_R_709_2",
    ),
    (
        "_kCMFormatDescriptionTransferFunction_SMPTE_240M_1995",
        "SMPTE_240M_1995",
    ),
    (
        "_kCMFormatDescriptionTransferFunction_ITU_R_2020",
        "ITU_R_2020",
    ),
    (
        "_kCMFormatDescriptionTransferFunction_SMPTE_ST_2084_PQ",
        "SMPTE_ST_2084_PQ",
    ),
    (
        "_kCMFormatDescriptionTransferFunction_ITU_R_2100_HLG",
        "ITU_R_2100_HLG",
    ),
    (
        "_kCMFormatDescriptionYCbCrMatrix_ITU_R_709_2",
        "ITU_R_709_2",
    ),
    (
        "_kCMFormatDescriptionYCbCrMatrix_ITU_R_601_4",
        "ITU_R_601_4",
    ),
    (
        "_kCMFormatDescriptionYCbCrMatrix_SMPTE_240M_1995",
        "SMPTE_240M_1995",
    ),
    ("_kCMFormatDescriptionYCbCrMatrix_ITU_R_2020", "ITU_R_2020"),
];

/// `CMVideoDimensions`: eight bytes, returned in one register on both ABIs.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dimensions {
    pub width: i32,
    pub height: i32,
}

struct State {
    media_type: u32,
    subtype: u32,
    dimensions: Dimensions,
    /// Owned; never null.
    extensions: CFTypeRef,
    /// In `avcC` order, sequence sets first, which is the order
    /// `CMVideoFormatDescriptionGetH264ParameterSetAtIndex` indexes.
    parameter_sets: Vec<Vec<u8>>,
    nal_length: i32,
}

impl Drop for State {
    fn drop(&mut self) {
        // SAFETY: the reference `create` made.
        unsafe { corefoundation::api::CFRelease(self.extensions) };
    }
}

fn class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let addr = *CLASS.get_or_init(|| {
        corefoundation::bootstrap();
        let class = corefoundation::boxed::register::<State>(c"CMFormatDescription", None, true);
        objc::class::ptr(class) as usize
    });
    // SAFETY: the registered class lives for the process.
    unsafe { &*(addr as *const Class) }
}

fn state<'a>(description: CFTypeRef) -> Option<&'a State> {
    if description.is_null() {
        return None;
    }
    // SAFETY: a guest-supplied CF object, checked against our class.
    unsafe { corefoundation::boxed::contents::<State>(description, class()) }.map(|s| &*s)
}

fn key(text: &str) -> CFTypeRef {
    interned_name(text) as usize as CFTypeRef
}

/// A dictionary of CF objects, consuming the references `pairs` hold.
fn dictionary(pairs: Vec<(CFTypeRef, CFTypeRef)>) -> CFTypeRef {
    let policy = Policy::from_type_callbacks();
    let out = collections::new_dictionary(&pairs, Policy::from_key_callbacks(), policy, false);
    for (_, value) in pairs {
        // SAFETY: each value is a reference this module made; the dictionary
        // took its own.
        unsafe { corefoundation::api::CFRelease(value) };
    }
    out
}

fn integer(value: i64) -> CFTypeRef {
    collections::new_number_of_type(Number::Integer(value), number_type::SINT32)
}

fn retained(object: CFTypeRef) -> CFTypeRef {
    // SAFETY: a live constant; the caller's pairs consume one reference each.
    unsafe { corefoundation::api::CFRetain(object) }
}

/// CoreMedia's names for the H.264 VUI code points (H.264 tables E-3 to E-5)
/// it has names for. A code point without one is left out, as Apple does.
fn primaries(code: u8) -> Option<&'static str> {
    Some(match code {
        1 => "ITU_R_709_2",
        5 => "EBU_3213",
        6 => "SMPTE_C",
        9 => "ITU_R_2020",
        _ => return None,
    })
}

fn transfer(code: u8) -> Option<&'static str> {
    Some(match code {
        // SMPTE 170M is the BT.709 curve under another name.
        1 | 6 => "ITU_R_709_2",
        7 => "SMPTE_240M_1995",
        14 | 15 => "ITU_R_2020",
        16 => "SMPTE_ST_2084_PQ",
        18 => "ITU_R_2100_HLG",
        _ => return None,
    })
}

fn matrix(code: u8) -> Option<&'static str> {
    Some(match code {
        1 => "ITU_R_709_2",
        5 | 6 => "ITU_R_601_4",
        7 => "SMPTE_240M_1995",
        9 => "ITU_R_2020",
        _ => return None,
    })
}

fn extensions(sps: &h264::Sps, avcc: Vec<u8>) -> CFTypeRef {
    let atoms = dictionary(vec![(key("avcC"), collections::new_data(avcc))]);
    let mut pairs = vec![
        (key("SampleDescriptionExtensionAtoms"), atoms),
        (key("FormatName"), retained(key("H.264"))),
        (key("Depth"), integer(24)),
    ];
    if let Some(full) = sps.full_range {
        pairs.push((key("FullRangeVideo"), retained(collections::boolean(full))));
    }
    if let Some((horizontal, vertical)) = sps.sample_aspect {
        let ratio = dictionary(vec![
            (key("HorizontalSpacing"), integer(horizontal.into())),
            (key("VerticalSpacing"), integer(vertical.into())),
        ]);
        pairs.push((key("CVPixelAspectRatio"), ratio));
    }
    if let Some((p, t, m)) = sps.colour {
        let named = [
            ("CVImageBufferColorPrimaries", primaries(p)),
            ("CVImageBufferTransferFunction", transfer(t)),
            ("CVImageBufferYCbCrMatrix", matrix(m)),
        ];
        for (name, value) in named {
            if let Some(value) = value {
                pairs.push((key(name), retained(key(value))));
            }
        }
    }
    dictionary(pairs)
}

type Sets = Vec<Vec<u8>>;

/// The sets a guest passed, checked and sorted into sequence and picture sets.
///
/// # Safety
///
/// `pointers` and `sizes` must each cover `count` entries, and each pointer
/// its size in bytes.
unsafe fn read_sets(
    count: usize,
    pointers: *const *const u8,
    sizes: *const usize,
) -> Result<(Sets, Sets), i32> {
    // SAFETY: the caller's obligation.
    let (pointers, sizes) = unsafe {
        (
            std::slice::from_raw_parts(pointers, count),
            std::slice::from_raw_parts(sizes, count),
        )
    };
    let (mut sequence, mut picture) = (Vec::new(), Vec::new());
    for (&pointer, &size) in pointers.iter().zip(sizes) {
        if pointer.is_null() || size == 0 {
            return Err(INVALID_PARAMETER);
        }
        if size > u16::MAX as usize {
            return Err(BRIDGE_INVALID_PARAMETER);
        }
        // SAFETY: the caller's obligation.
        let set = unsafe { std::slice::from_raw_parts(pointer, size) }.to_vec();
        match set[0] & 0x1f {
            h264::SPS => sequence.push(set),
            h264::PPS => picture.push(set),
            _ => return Err(BRIDGE_INVALID_PARAMETER),
        }
    }
    if sequence.is_empty() || picture.is_empty() {
        return Err(BRIDGE_INVALID_PARAMETER);
    }
    if sequence.len() > MAX_SPS || picture.len() > MAX_PPS {
        return Err(INVALID_PARAMETER);
    }
    Ok((sequence, picture))
}

/// `OSStatus CMVideoFormatDescriptionCreateFromH264ParameterSets(CFAllocatorRef
/// allocator, size_t parameterSetCount, const uint8_t *const
/// *parameterSetPointers, const size_t *parameterSetSizes, int
/// NALUnitHeaderLength, CMFormatDescriptionRef *formatDescriptionOut)`
///
/// # Safety
///
/// As the header: `pointers` and `sizes` cover `count` entries, and `out` is
/// writable.
pub unsafe extern "C" fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
    _allocator: CFAllocatorRef,
    count: usize,
    pointers: *const *const u8,
    sizes: *const usize,
    nal_length: i32,
    out: *mut CFTypeRef,
) -> i32 {
    if out.is_null() || pointers.is_null() || sizes.is_null() || count < 2 {
        return INVALID_PARAMETER;
    }
    if !matches!(nal_length, 1 | 2 | 4) {
        return INVALID_PARAMETER;
    }
    // SAFETY: the caller's obligation.
    let (sequence, picture) = match unsafe { read_sets(count, pointers, sizes) } {
        Ok(sets) => sets,
        Err(status) => return status,
    };
    // The first sequence set is the one the record's profile fields describe,
    // as in `avcC`.
    let Some(sps) = h264::parse_sps(&sequence[0]) else {
        return BRIDGE_INVALID_PARAMETER;
    };
    let avcc = h264::avcc(&sps, &sequence, &picture, nal_length as u8);
    let extensions = extensions(&sps, avcc);
    if extensions.is_null() {
        return ALLOCATION_FAILED;
    }
    let state = State {
        media_type: VIDEO,
        subtype: AVC1,
        dimensions: Dimensions {
            width: sps.width as i32,
            height: sps.height as i32,
        },
        extensions,
        parameter_sets: sequence.into_iter().chain(picture).collect(),
        nal_length,
    };
    let description = corefoundation::boxed::make(class(), state);
    if description.is_null() {
        return ALLOCATION_FAILED;
    }
    // SAFETY: the caller's obligation.
    unsafe { *out = description };
    SUCCESS
}

/// `OSStatus CMVideoFormatDescriptionCreate(CFAllocatorRef allocator,
/// CMVideoCodecType codecType, int32_t width, int32_t height, CFDictionaryRef
/// extensions, CMFormatDescriptionRef *formatDescriptionOut)`
///
/// The extensions are copied as given.
///
/// FIXME (ACCURACY): an `avcC` atom in `extensions` is not read back into
/// H.264 parameter sets the way the parameter-set constructor's output is;
/// a description made here has none, so a decoder session opened from it
/// finds no decoder for AVC1 content. Tests exercise only that case so far.
///
/// # Safety
///
/// `extensions` is null or a dictionary, and `out` is writable.
pub unsafe extern "C" fn CMVideoFormatDescriptionCreate(
    _allocator: CFAllocatorRef,
    codec: u32,
    width: i32,
    height: i32,
    extensions: CFTypeRef,
    out: *mut CFTypeRef,
) -> i32 {
    if out.is_null() {
        return INVALID_PARAMETER;
    }
    let extensions = match extensions.is_null() {
        true => dictionary(Vec::new()),
        // SAFETY: the caller's object, checked to be a dictionary first.
        false => unsafe {
            if collections::dictionary(extensions).is_none() {
                return INVALID_PARAMETER;
            }
            corefoundation::api::CFDictionaryCreateCopy(std::ptr::null(), extensions)
        },
    };
    if extensions.is_null() {
        return ALLOCATION_FAILED;
    }
    let state = State {
        media_type: VIDEO,
        subtype: codec,
        dimensions: Dimensions { width, height },
        extensions,
        parameter_sets: Vec::new(),
        nal_length: 0,
    };
    let description = corefoundation::boxed::make(class(), state);
    if description.is_null() {
        return ALLOCATION_FAILED;
    }
    // SAFETY: the caller's obligation.
    unsafe { *out = description };
    SUCCESS
}

/// `OSStatus CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
/// CMFormatDescriptionRef videoDesc, size_t parameterSetIndex, const uint8_t
/// **parameterSetPointerOut, size_t *parameterSetSizeOut, size_t
/// *parameterSetCountOut, int *NALUnitHeaderLengthOut)`
///
/// The count and header length are written even for an index past the end,
/// which is how a caller asks for them without knowing how many there are.
///
/// # Safety
///
/// Each output is null or writable.
pub unsafe extern "C" fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
    description: CFTypeRef,
    index: usize,
    pointer_out: *mut *const u8,
    size_out: *mut usize,
    count_out: *mut usize,
    nal_length_out: *mut i32,
) -> i32 {
    let Some(state) = state(description) else {
        return INVALID_PARAMETER;
    };
    if state.subtype != AVC1 {
        return INCOMPATIBLE;
    }
    // SAFETY: each is null or writable, per the caller.
    unsafe {
        if !count_out.is_null() {
            *count_out = state.parameter_sets.len();
        }
        if !nal_length_out.is_null() {
            *nal_length_out = state.nal_length;
        }
    }
    let Some(set) = state.parameter_sets.get(index) else {
        // Only an index nobody reads from is fine past the end.
        return match pointer_out.is_null() && size_out.is_null() {
            true => SUCCESS,
            false => BRIDGE_INVALID_PARAMETER,
        };
    };
    // SAFETY: as above. The bytes live as long as the description.
    unsafe {
        if !pointer_out.is_null() {
            *pointer_out = set.as_ptr();
        }
        if !size_out.is_null() {
            *size_out = set.len();
        }
    }
    SUCCESS
}

pub unsafe extern "C" fn CMFormatDescriptionGetTypeID() -> usize {
    corefoundation::base::type_id(class())
}

pub unsafe extern "C" fn CMFormatDescriptionGetMediaType(description: CFTypeRef) -> u32 {
    state(description).map_or(0, |s| s.media_type)
}

pub unsafe extern "C" fn CMFormatDescriptionGetMediaSubType(description: CFTypeRef) -> u32 {
    state(description).map_or(0, |s| s.subtype)
}

/// Not retained, under the Get rule.
pub unsafe extern "C" fn CMFormatDescriptionGetExtensions(description: CFTypeRef) -> CFTypeRef {
    state(description).map_or(std::ptr::null(), |s| s.extensions)
}

pub unsafe extern "C" fn CMFormatDescriptionGetExtension(
    description: CFTypeRef,
    key: CFTypeRef,
) -> CFTypeRef {
    match (state(description), key.is_null()) {
        // SAFETY: our own dictionary, and a guest CF key.
        (Some(s), false) => unsafe { corefoundation::api::CFDictionaryGetValue(s.extensions, key) },
        _ => std::ptr::null(),
    }
}

pub unsafe extern "C" fn CMFormatDescriptionEqual(a: CFTypeRef, b: CFTypeRef) -> u8 {
    if a == b {
        return 1;
    }
    let (Some(a), Some(b)) = (state(a), state(b)) else {
        return 0;
    };
    // The extensions are made from the sets and NAL length alone, so these
    // decide what comparing the dictionaries would.
    let same = a.media_type == b.media_type
        && a.subtype == b.subtype
        && a.parameter_sets == b.parameter_sets
        && a.nal_length == b.nal_length;
    same as u8
}

pub unsafe extern "C" fn CMVideoFormatDescriptionGetDimensions(
    description: CFTypeRef,
) -> Dimensions {
    state(description)
        .filter(|s| s.media_type == VIDEO)
        .map_or(Dimensions::default(), |s| s.dimensions)
}

exports::exports! {
    functions {
        "CMVideoFormatDescriptionCreateFromH264ParameterSets" => CMVideoFormatDescriptionCreateFromH264ParameterSets,
        "CMVideoFormatDescriptionGetH264ParameterSetAtIndex" => CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
        "CMVideoFormatDescriptionGetDimensions" => CMVideoFormatDescriptionGetDimensions,
        "CMFormatDescriptionGetTypeID" => CMFormatDescriptionGetTypeID,
        "CMFormatDescriptionGetMediaType" => CMFormatDescriptionGetMediaType,
        "CMFormatDescriptionGetMediaSubType" => CMFormatDescriptionGetMediaSubType,
        "CMFormatDescriptionGetExtensions" => CMFormatDescriptionGetExtensions,
        "CMFormatDescriptionGetExtension" => CMFormatDescriptionGetExtension,
        "CMFormatDescriptionEqual" => CMFormatDescriptionEqual,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::h264::tests::{PPS_1080P_HIGH, SPS_720P_FULL_RANGE, SPS_1080P_HIGH};

    fn create(sets: &[&[u8]], nal_length: i32) -> Result<CFTypeRef, i32> {
        let pointers: Vec<*const u8> = sets.iter().map(|s| s.as_ptr()).collect();
        let sizes: Vec<usize> = sets.iter().map(|s| s.len()).collect();
        let mut out = std::ptr::null();
        let status = unsafe {
            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                std::ptr::null(),
                sets.len(),
                pointers.as_ptr(),
                sizes.as_ptr(),
                nal_length,
                &mut out,
            )
        };
        match status {
            SUCCESS => Ok(out),
            status => Err(status),
        }
    }

    fn extension(description: CFTypeRef, name: &str) -> CFTypeRef {
        unsafe { CMFormatDescriptionGetExtension(description, key(name)) }
    }

    #[test]
    fn describes_h264_video_by_its_cropped_size() {
        let d = create(&[SPS_1080P_HIGH, PPS_1080P_HIGH], 4).expect("creates");
        unsafe {
            assert_eq!(
                corefoundation::api::CFGetTypeID(d),
                CMFormatDescriptionGetTypeID()
            );
            assert_eq!(CMFormatDescriptionGetMediaType(d), VIDEO);
            assert_eq!(CMFormatDescriptionGetMediaSubType(d), AVC1);
            assert_eq!(
                CMVideoFormatDescriptionGetDimensions(d),
                Dimensions {
                    width: 1920,
                    height: 1080
                }
            );
            corefoundation::api::CFRelease(d);
        }
    }

    #[test]
    fn hands_back_the_parameter_sets_sequence_first() {
        // Passed picture-first; `avcC` order puts the sequence set at 0.
        let d = create(&[PPS_1080P_HIGH, SPS_1080P_HIGH], 2).expect("creates");
        unsafe {
            let (mut count, mut nal_length) = (0, 0);
            let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                d,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut count,
                &mut nal_length,
            );
            assert_eq!((status, count, nal_length), (SUCCESS, 2, 2));
            let (mut pointer, mut size) = (std::ptr::null(), 0);
            for (i, expected) in [SPS_1080P_HIGH, PPS_1080P_HIGH].into_iter().enumerate() {
                let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    d,
                    i,
                    &mut pointer,
                    &mut size,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                assert_eq!(status, SUCCESS);
                assert_eq!(std::slice::from_raw_parts(pointer, size), expected);
            }
            let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                d,
                2,
                &mut pointer,
                &mut size,
                &mut count,
                std::ptr::null_mut(),
            );
            assert_eq!((status, count), (BRIDGE_INVALID_PARAMETER, 2));
            corefoundation::api::CFRelease(d);
        }
    }

    #[test]
    fn records_the_avcc_atom_among_the_extensions() {
        let d = create(&[SPS_1080P_HIGH, PPS_1080P_HIGH], 4).expect("creates");
        unsafe {
            let atoms = extension(d, "SampleDescriptionExtensionAtoms");
            let avcc = corefoundation::api::CFDictionaryGetValue(atoms, key("avcC"));
            let bytes = collections::data(avcc).expect("avcC is data");
            let sps = h264::parse_sps(SPS_1080P_HIGH).unwrap();
            let expected = h264::avcc(
                &sps,
                &[SPS_1080P_HIGH.to_vec()],
                &[PPS_1080P_HIGH.to_vec()],
                4,
            );
            assert_eq!(*bytes, expected);
            assert_eq!(extension(d, "FormatName"), key("H.264"));
            assert!(extension(d, "FullRangeVideo").is_null());
            corefoundation::api::CFRelease(d);
        }
    }

    #[test]
    fn mirrors_the_vui_range_and_matrix() {
        let d = create(&[SPS_720P_FULL_RANGE, PPS_1080P_HIGH], 4).expect("creates");
        assert_eq!(extension(d, "FullRangeVideo"), collections::boolean(true));
        assert_eq!(extension(d, "CVImageBufferYCbCrMatrix"), key("ITU_R_709_2"));
        unsafe { corefoundation::api::CFRelease(d) };
    }

    #[test]
    fn equal_descriptions_compare_equal() {
        let a = create(&[SPS_1080P_HIGH, PPS_1080P_HIGH], 4).unwrap();
        let b = create(&[SPS_1080P_HIGH, PPS_1080P_HIGH], 4).unwrap();
        let c = create(&[SPS_1080P_HIGH, PPS_1080P_HIGH], 2).unwrap();
        unsafe {
            assert_eq!(CMFormatDescriptionEqual(a, b), 1);
            // The NAL length is in the `avcC` record.
            assert_eq!(CMFormatDescriptionEqual(a, c), 0);
            for d in [a, b, c] {
                corefoundation::api::CFRelease(d);
            }
        }
    }

    #[test]
    fn refuses_incomplete_or_unreadable_parameter_sets() {
        assert_eq!(
            create(&[SPS_1080P_HIGH], 4),
            Err(INVALID_PARAMETER),
            "fewer than two sets"
        );
        assert_eq!(
            create(&[SPS_1080P_HIGH, SPS_1080P_HIGH], 4),
            Err(BRIDGE_INVALID_PARAMETER),
            "no picture set"
        );
        assert_eq!(
            create(&[SPS_1080P_HIGH, PPS_1080P_HIGH], 3),
            Err(INVALID_PARAMETER),
            "a NAL length CoreMedia does not allow"
        );
        assert_eq!(
            create(&[&SPS_1080P_HIGH[..4], PPS_1080P_HIGH], 4),
            Err(BRIDGE_INVALID_PARAMETER),
            "a truncated sequence set"
        );
        assert_eq!(
            create(&[&[0x65, 0x88], PPS_1080P_HIGH], 4),
            Err(BRIDGE_INVALID_PARAMETER),
            "a slice is not a parameter set"
        );
    }
}
