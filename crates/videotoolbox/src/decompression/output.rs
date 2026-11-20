//! The pixel buffers a session hands its callback.
//!
//! `destinationImageBufferAttributes` choose the format: 3-plane `y420` and
//! `f420`, NV12 `420v` and `420f`, or `BGRA`. A decoder's picture (NV12 or
//! I420) is copied into a fresh `CVPixelBuffer` of that format, its range
//! mapped where stream and format disagree, and converted to RGB for `BGRA`.

use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections::{self, Number};
use corevideo::pixel_buffer as pb;
use mediabackend::{ColorMatrix, ColorPrimaries, ColorTransfer, FrameLayout, PixelFormat};

use super::backend::Picture;

pub(crate) const Y420: u32 = u32::from_be_bytes(*b"y420");
pub(crate) const F420: u32 = u32::from_be_bytes(*b"f420");
pub(crate) const NV12_VIDEO: u32 = u32::from_be_bytes(*b"420v");
pub(crate) const NV12_FULL: u32 = u32::from_be_bytes(*b"420f");
pub(crate) const BGRA: u32 = u32::from_be_bytes(*b"BGRA");

/// The formats a session can output, in no order of preference.
const SUPPORTED: [u32; 5] = [Y420, F420, NV12_VIDEO, NV12_FULL, BGRA];

/// `kCVPixelBuffer*Key` texts.
pub(crate) const PIXEL_FORMAT_TYPE_KEY: &str = "PixelFormatType";
pub(crate) const WIDTH_KEY: &str = "Width";
pub(crate) const HEIGHT_KEY: &str = "Height";
pub(crate) const BYTES_PER_ROW_ALIGNMENT_KEY: &str = "BytesPerRowAlignment";
pub(crate) const IOSURFACE_PROPERTIES_KEY: &str = "IOSurfaceProperties";

/// `kCVPixelBufferLock_ReadOnly`'s absence: the session writes the planes.
const LOCK_FOR_WRITING: u64 = 0;
/// `kCVAttachmentMode_ShouldPropagate`.
const SHOULD_PROPAGATE: u32 = 1;

/// The widest `BytesPerRowAlignment` taken; more is not a row alignment.
const MAX_ROW_ALIGNMENT: i64 = 4096;

/// What the attributes asked for.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OutputSpec {
    /// The format asked for; `None` to choose from the first picture.
    format: Option<u32>,
    /// The attributes every buffer is made with, owned.
    attributes: usize,
}

impl Drop for OutputSpec {
    fn drop(&mut self) {
        // SAFETY: the dictionary `parse` made.
        unsafe { corefoundation::api::CFRelease(self.attributes as CFTypeRef) };
    }
}

pub(crate) fn key(text: &str) -> CFTypeRef {
    corefoundation::base::interned_name(text) as usize as CFTypeRef
}

fn attribute(attributes: CFTypeRef, name: &str) -> CFTypeRef {
    if attributes.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller's dictionary and a key made here.
    unsafe { corefoundation::api::CFDictionaryGetValue(attributes, key(name)) }
}

fn integer(value: CFTypeRef) -> Option<i64> {
    // SAFETY: a CF object from the caller's dictionary.
    unsafe { collections::number(value) }.map(|n| n.as_i64())
}

/// A format's four characters, as a diagnostic names it.
pub(crate) fn fourcc(format: u32) -> String {
    let bytes = format.to_be_bytes();
    match bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        true => bytes.iter().map(|&b| b as char).collect(),
        false => format!("{format:#010x}"),
    }
}

impl OutputSpec {
    /// Read `destinationImageBufferAttributes` for a `width` by `height`
    /// stream, or say why the session cannot satisfy them.
    pub fn parse(attributes: CFTypeRef, width: usize, height: usize) -> Result<OutputSpec, String> {
        let mut format = None;
        let requested = attribute(attributes, PIXEL_FORMAT_TYPE_KEY);
        if !requested.is_null() {
            let formats = formats(requested)?;
            format = formats.iter().copied().find(|f| SUPPORTED.contains(f));
            if format.is_none() {
                let names: Vec<String> = formats.iter().map(|&f| fourcc(f)).collect();
                return Err(format!(
                    "no output format of {} is supported (y420, f420, 420v, 420f and BGRA are)",
                    names.join(", ")
                ));
            }
        }
        for (name, size) in [(WIDTH_KEY, width), (HEIGHT_KEY, height)] {
            let value = attribute(attributes, name);
            if value.is_null() {
                continue;
            }
            if integer(value) != Some(size as i64) {
                return Err(format!(
                    "a {name} of {} for a {width}x{height} stream: scaling is not supported",
                    integer(value).map_or("?".to_string(), |v| v.to_string())
                ));
            }
        }
        let mut pairs = Vec::new();
        let alignment = attribute(attributes, BYTES_PER_ROW_ALIGNMENT_KEY);
        if !alignment.is_null() {
            match integer(alignment) {
                Some(n @ 0..=MAX_ROW_ALIGNMENT) => pairs.push((
                    key(BYTES_PER_ROW_ALIGNMENT_KEY),
                    collections::new_number(Number::Integer(n)),
                )),
                _ => return Err("the BytesPerRowAlignment is not a size".to_string()),
            }
        }
        if !attribute(attributes, IOSURFACE_PROPERTIES_KEY).is_null() {
            let policy = Policy::from_type_callbacks();
            let empty =
                collections::new_dictionary(&[], Policy::from_key_callbacks(), policy, false);
            pairs.push((key(IOSURFACE_PROPERTIES_KEY), empty));
        }
        let made = collections::new_dictionary(
            &pairs,
            Policy::from_key_callbacks(),
            Policy::from_type_callbacks(),
            false,
        );
        for (_, value) in pairs {
            // SAFETY: values made above; the dictionary holds its own.
            unsafe { corefoundation::api::CFRelease(value) };
        }
        Ok(OutputSpec {
            format,
            attributes: made as usize,
        })
    }

    /// The format pictures come out in, fixed by the first one when nothing
    /// was asked for.
    pub(crate) fn resolve(&mut self, full_range: Option<bool>) -> u32 {
        *self.format.get_or_insert(match full_range {
            Some(true) => NV12_FULL,
            _ => NV12_VIDEO,
        })
    }
}

/// The pixel formats in a `PixelFormatType` value: a number, or an array of
/// them.
fn formats(value: CFTypeRef) -> Result<Vec<u32>, String> {
    if let Some(n) = integer(value) {
        return Ok(vec![n as u32]);
    }
    // SAFETY: a CF object from the caller's dictionary, read as an array once
    // it is known to be one.
    unsafe {
        if corefoundation::api::CFGetTypeID(value) != corefoundation::api::CFArrayGetTypeID() {
            return Err("the PixelFormatType is neither a number nor an array".to_string());
        }
        (0..corefoundation::api::CFArrayGetCount(value))
            .map(|i| {
                integer(corefoundation::api::CFArrayGetValueAtIndex(value, i))
                    .map(|n| n as u32)
                    .ok_or_else(|| "the PixelFormatType array holds a non-number".to_string())
            })
            .collect()
    }
}

/// A 256-entry table from one range to the other, or `None` when they are
/// the same. Luma video range is 16-235, chroma 16-240 around 128.
fn range_table(from_full: bool, to_full: bool, luma: bool) -> Option<[u8; 256]> {
    if from_full == to_full {
        return None;
    }
    let mut table = [0u8; 256];
    for (i, out) in table.iter_mut().enumerate() {
        let v = i as f64;
        let mapped = match (luma, to_full) {
            (true, false) => 16.0 + v * 219.0 / 255.0,
            (true, true) => (v - 16.0) * 255.0 / 219.0,
            (false, false) => 128.0 + (v - 128.0) * 224.0 / 255.0,
            (false, true) => 128.0 + (v - 128.0) * 255.0 / 224.0,
        };
        *out = mapped.round().clamp(0.0, 255.0) as u8;
    }
    Some(table)
}

fn map(table: &Option<[u8; 256]>, value: u8) -> u8 {
    table.as_ref().map_or(value, |t| t[usize::from(value)])
}

/// A decoded picture's planes, cropped to what is shown.
struct Source<'a> {
    y: &'a [u8],
    y_stride: usize,
    chroma: Chroma<'a>,
    width: usize,
    height: usize,
}

/// The chroma of a picture, read one sample at a time.
enum Chroma<'a> {
    Interleaved {
        uv: &'a [u8],
        stride: usize,
    },
    Separate {
        u: &'a [u8],
        u_stride: usize,
        v: &'a [u8],
        v_stride: usize,
    },
}

impl Chroma<'_> {
    fn at(&self, x: usize, y: usize) -> (u8, u8) {
        match *self {
            Chroma::Interleaved { uv, stride } => {
                (uv[y * stride + 2 * x], uv[y * stride + 2 * x + 1])
            }
            Chroma::Separate {
                u,
                u_stride,
                v,
                v_stride,
            } => (u[y * u_stride + x], v[y * v_stride + x]),
        }
    }
}

/// The planes of `picture`, checked to lie inside its bytes, from the crop's
/// corner. `None` for a layout this cannot read.
fn source<'a>(bytes: &'a [u8], layout: &FrameLayout) -> Option<Source<'a>> {
    let (x, y, width, height) = match layout.crop {
        Some(c) => (c.x, c.y, c.width, c.height),
        None => (0, 0, layout.width, layout.height),
    };
    // Chroma is subsampled two by two: a crop has to start on a chroma sample.
    if x % 2 != 0 || y % 2 != 0 || width == 0 || height == 0 {
        return None;
    }
    if x.checked_add(width)? > layout.width || y.checked_add(height)? > layout.height {
        return None;
    }
    let plane = |index: usize, x_bytes: usize, row: usize| -> Option<(&'a [u8], usize)> {
        let offset = *layout.offsets.get(index)?;
        let stride = *layout.strides.get(index)?;
        let span = layout.plane_byte_span(index)?;
        let whole = bytes.get(offset..offset.checked_add(span)?)?;
        Some((whole.get(row * stride + x_bytes..)?, stride))
    };
    let (y_plane, y_stride) = plane(0, x, y)?;
    let chroma = match layout.format {
        PixelFormat::Nv12 => {
            let (uv, stride) = plane(1, x, y / 2)?;
            Chroma::Interleaved { uv, stride }
        }
        PixelFormat::I420 => {
            let (u, u_stride) = plane(1, x / 2, y / 2)?;
            let (v, v_stride) = plane(2, x / 2, y / 2)?;
            Chroma::Separate {
                u,
                u_stride,
                v,
                v_stride,
            }
        }
        PixelFormat::Bgra => return None,
    };
    Some(Source {
        y: y_plane,
        y_stride,
        chroma,
        width,
        height,
    })
}

/// Why a picture could not be made.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MakeError {
    /// A decoded layout this cannot read.
    Layout,
    /// CoreVideo's `CVReturn`.
    CoreVideo(i32),
}

/// A pixel buffer `CVPixelBufferCreate` made, released unless handed on.
struct Made(CFTypeRef);

impl Drop for Made {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the reference creation gave.
            unsafe { pb::CVPixelBufferRelease(self.0) };
        }
    }
}

/// A new pixel buffer holding `picture` in the session's format, with one
/// reference for the caller, and the format it is in.
pub(crate) fn make_image(
    picture: &Picture,
    spec: &mut OutputSpec,
) -> Result<(CFTypeRef, u32), MakeError> {
    let layout = picture.layout;
    let source = source(picture.bytes, layout).ok_or(MakeError::Layout)?;
    let format = spec.resolve(layout.full_range);
    let (width, height) = (source.width, source.height);
    let mut image = Made(std::ptr::null());
    // SAFETY: an out-parameter and the session's own attributes.
    let status = unsafe {
        pb::CVPixelBufferCreate(
            std::ptr::null(),
            width,
            height,
            format,
            spec.attributes as CFTypeRef,
            &mut image.0,
        )
    };
    if status != 0 {
        return Err(MakeError::CoreVideo(status));
    }
    // SAFETY: the buffer just made, locked while it is written.
    let status = unsafe { pb::CVPixelBufferLockBaseAddress(image.0, LOCK_FOR_WRITING) };
    if status != 0 {
        return Err(MakeError::CoreVideo(status));
    }
    // SAFETY: as above; each plane's base and stride are the buffer's.
    unsafe { write(image.0, format, &source, layout) };
    // SAFETY: the lock taken above.
    unsafe { pb::CVPixelBufferUnlockBaseAddress(image.0, LOCK_FOR_WRITING) };
    attach_colour(image.0, layout);
    Ok((std::mem::replace(&mut image.0, std::ptr::null()), format))
}

/// One plane of a locked pixel buffer, as rows to write.
struct Plane {
    base: *mut u8,
    stride: usize,
}

impl Plane {
    /// # Safety
    ///
    /// `buffer` must be locked and have plane `index` (0 for a packed one).
    unsafe fn of(buffer: CFTypeRef, index: usize, planar: bool) -> Plane {
        // SAFETY: as the caller promises.
        unsafe {
            match planar {
                true => Plane {
                    base: pb::CVPixelBufferGetBaseAddressOfPlane(buffer, index).cast(),
                    stride: pb::CVPixelBufferGetBytesPerRowOfPlane(buffer, index),
                },
                false => Plane {
                    base: pb::CVPixelBufferGetBaseAddress(buffer).cast(),
                    stride: pb::CVPixelBufferGetBytesPerRow(buffer),
                },
            }
        }
    }

    /// # Safety
    ///
    /// The plane must have row `row` with at least `len` bytes.
    unsafe fn row(&mut self, row: usize, len: usize) -> &mut [u8] {
        // SAFETY: as the caller promises.
        unsafe { std::slice::from_raw_parts_mut(self.base.add(row * self.stride), len) }
    }
}

/// Copy `source` into the locked `buffer` of `format`, made its size.
///
/// # Safety
///
/// `buffer` must be a locked pixel buffer of `format`, `source`'s size.
unsafe fn write(buffer: CFTypeRef, format: u32, source: &Source, layout: &FrameLayout) {
    let (width, height) = (source.width, source.height);
    let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
    let from_full = layout.full_range.unwrap_or(false);
    if format == BGRA {
        // SAFETY: a packed buffer `width` pixels of four bytes wide.
        let mut out = unsafe { Plane::of(buffer, 0, false) };
        to_bgra(source, layout, &mut out);
        return;
    }
    let to_full = matches!(format, F420 | NV12_FULL);
    let luma = range_table(from_full, to_full, true);
    let chroma = range_table(from_full, to_full, false);
    // SAFETY: plane 0 of a planar buffer, `width` by `height`.
    let mut y_out = unsafe { Plane::of(buffer, 0, true) };
    for row in 0..height {
        let from = &source.y[row * source.y_stride..row * source.y_stride + width];
        // SAFETY: a row of the luma plane.
        let to = unsafe { y_out.row(row, width) };
        match &luma {
            None => to.copy_from_slice(from),
            Some(t) => to
                .iter_mut()
                .zip(from)
                .for_each(|(d, &s)| *d = t[usize::from(s)]),
        }
    }
    match format {
        NV12_VIDEO | NV12_FULL => {
            // SAFETY: plane 1 of an NV12 buffer, interleaved and half size.
            let mut uv_out = unsafe { Plane::of(buffer, 1, true) };
            for row in 0..chroma_height {
                // SAFETY: a row of the chroma plane.
                let to = unsafe { uv_out.row(row, 2 * chroma_width) };
                for x in 0..chroma_width {
                    let (u, v) = source.chroma.at(x, row);
                    to[2 * x] = map(&chroma, u);
                    to[2 * x + 1] = map(&chroma, v);
                }
            }
        }
        _ => {
            // SAFETY: planes 1 and 2 of a three-plane buffer, half size.
            let (mut u_out, mut v_out) =
                unsafe { (Plane::of(buffer, 1, true), Plane::of(buffer, 2, true)) };
            for row in 0..chroma_height {
                // SAFETY: a row of each chroma plane.
                let (u_row, v_row) =
                    unsafe { (u_out.row(row, chroma_width), v_out.row(row, chroma_width)) };
                for x in 0..chroma_width {
                    let (u, v) = source.chroma.at(x, row);
                    u_row[x] = map(&chroma, u);
                    v_row[x] = map(&chroma, v);
                }
            }
        }
    }
}

/// `(Kr, Kb)` of the picture's matrix.
fn coefficients(matrix: ColorMatrix, height: usize) -> (f64, f64) {
    const BT601: (f64, f64) = (0.299, 0.114);
    const BT709: (f64, f64) = (0.2126, 0.0722);
    const BT2020: (f64, f64) = (0.2627, 0.0593);
    const SMPTE240M: (f64, f64) = (0.212, 0.087);
    /// The height from which a picture that does not say is taken as HD.
    const HD_HEIGHT: usize = 720;
    match matrix {
        ColorMatrix::Bt709 => BT709,
        ColorMatrix::Bt601 => BT601,
        ColorMatrix::Bt2020 => BT2020,
        ColorMatrix::Smpte240m => SMPTE240M,
        // FIXME (ACCURACY): what Apple assumes for a stream that does not
        // say is not verified; this is the usual HD/SD split.
        _ if height >= HD_HEIGHT => BT709,
        _ => BT601,
    }
}

/// Y'CbCr to `BGRA`, in 16.16 fixed point.
///
/// FIXME (ACCURACY): chroma is upsampled nearest-neighbour, where Apple's
/// conversion interpolates.
fn to_bgra(source: &Source, layout: &FrameLayout, out: &mut Plane) {
    const ONE: f64 = 65536.0;
    const HALF: i64 = 32768;
    let full = layout.full_range.unwrap_or(false);
    let (kr, kb) = coefficients(layout.matrix, source.height);
    let kg = 1.0 - kr - kb;
    let (luma_scale, chroma_scale, luma_offset) = match full {
        true => (1.0, 1.0, 0),
        false => (255.0 / 219.0, 255.0 / 224.0, 16),
    };
    let fixed = |v: f64| (v * ONE).round() as i64;
    let ys = fixed(luma_scale);
    let r_cr = fixed(2.0 * (1.0 - kr) * chroma_scale);
    let b_cb = fixed(2.0 * (1.0 - kb) * chroma_scale);
    let g_cb = fixed(2.0 * kb * (1.0 - kb) / kg * chroma_scale);
    let g_cr = fixed(2.0 * kr * (1.0 - kr) / kg * chroma_scale);
    let clamp = |v: i64| ((v + HALF) >> 16).clamp(0, 255) as u8;
    for row in 0..source.height {
        // SAFETY: a row of the packed buffer, four bytes a pixel.
        let line = unsafe { out.row(row, 4 * source.width) };
        for x in 0..source.width {
            let luma = (i64::from(source.y[row * source.y_stride + x]) - luma_offset) * ys;
            let (cb, cr) = source.chroma.at(x / 2, row / 2);
            let (cb, cr) = (i64::from(cb) - 128, i64::from(cr) - 128);
            let pixel = &mut line[4 * x..4 * x + 4];
            pixel[0] = clamp(luma + b_cb * cb);
            pixel[1] = clamp(luma - g_cb * cb - g_cr * cr);
            pixel[2] = clamp(luma + r_cr * cr);
            pixel[3] = u8::MAX;
        }
    }
}

/// The `kCVImageBuffer*` value texts, where CoreVideo has one.
fn matrix_name(matrix: ColorMatrix) -> Option<&'static str> {
    Some(match matrix {
        ColorMatrix::Bt709 => "ITU_R_709_2",
        ColorMatrix::Bt601 => "ITU_R_601_4",
        ColorMatrix::Smpte240m => "SMPTE_240M_1995",
        ColorMatrix::Bt2020 => "ITU_R_2020",
        _ => return None,
    })
}

fn primaries_name(primaries: ColorPrimaries) -> Option<&'static str> {
    Some(match primaries {
        ColorPrimaries::Bt709 => "ITU_R_709_2",
        // BT.470 System B/G is the code point CoreVideo calls EBU 3213.
        ColorPrimaries::Bt470bg => "EBU_3213",
        ColorPrimaries::Smpte170m | ColorPrimaries::SmpteC => "SMPTE_C",
        ColorPrimaries::Bt2020 => "ITU_R_2020",
        _ => return None,
    })
}

fn transfer_name(transfer: ColorTransfer) -> Option<&'static str> {
    Some(match transfer {
        // SMPTE 170M's curve is BT.709's.
        ColorTransfer::Bt709 | ColorTransfer::Bt601 => "ITU_R_709_2",
        ColorTransfer::Smpte240m => "SMPTE_240M_1995",
        ColorTransfer::Bt202010 | ColorTransfer::Bt202012 => "ITU_R_2020",
        ColorTransfer::Smpte2084 => "SMPTE_ST_2084_PQ",
        ColorTransfer::AribStdB67 => "ITU_R_2100_HLG",
        _ => return None,
    })
}

/// Describe the picture's colour on the buffer, as the attachments a
/// consumer that propagates them reads.
fn attach_colour(image: CFTypeRef, layout: &FrameLayout) {
    let pairs = [
        ("CVImageBufferYCbCrMatrix", matrix_name(layout.matrix)),
        (
            "CVImageBufferColorPrimaries",
            primaries_name(layout.primaries),
        ),
        (
            "CVImageBufferTransferFunction",
            transfer_name(layout.transfer),
        ),
    ];
    for (name, value) in pairs {
        if let Some(value) = value {
            // SAFETY: a buffer this module made, and two interned strings.
            unsafe { pb::CVBufferSetAttachment(image, key(name), key(value), SHOULD_PROPAGATE) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dictionary(pairs: &[(&str, i64)]) -> CFTypeRef {
        corefoundation::bootstrap();
        let entries: Vec<(CFTypeRef, CFTypeRef)> = pairs
            .iter()
            .map(|(k, n)| (key(k), collections::new_number(Number::Integer(*n))))
            .collect();
        collections::new_dictionary(
            &entries,
            Policy::from_key_callbacks(),
            Policy::from_type_callbacks(),
            false,
        )
    }

    #[test]
    fn range_tables_map_the_ends_of_each_range() {
        let up = range_table(false, true, true).unwrap();
        assert_eq!((up[16], up[235], up[0], up[255]), (0, 255, 0, 255));
        let down = range_table(true, false, true).unwrap();
        assert_eq!((down[0], down[255]), (16, 235));
        let chroma_down = range_table(true, false, false).unwrap();
        assert_eq!(
            (chroma_down[0], chroma_down[128], chroma_down[255]),
            (16, 128, 240)
        );
        let chroma_up = range_table(false, true, false).unwrap();
        // 16 is half a step above full range's 0, which rounds up.
        assert_eq!(
            (chroma_up[16], chroma_up[128], chroma_up[240]),
            (1, 128, 255)
        );
        assert!(range_table(true, true, true).is_none());
    }

    #[test]
    fn unsupported_or_scaled_outputs_are_refused() {
        let why =
            OutputSpec::parse(dictionary(&[(PIXEL_FORMAT_TYPE_KEY, 24)]), 64, 64).unwrap_err();
        assert!(why.contains("no output format"), "{why}");
        let why = OutputSpec::parse(dictionary(&[(WIDTH_KEY, 32)]), 64, 64).unwrap_err();
        assert!(why.contains("scaling"), "{why}");
        let same = dictionary(&[
            (WIDTH_KEY, 64),
            (HEIGHT_KEY, 48),
            (PIXEL_FORMAT_TYPE_KEY, i64::from(BGRA)),
        ]);
        assert_eq!(OutputSpec::parse(same, 64, 48).unwrap().format, Some(BGRA));
        assert_eq!(
            OutputSpec::parse(std::ptr::null(), 64, 48).unwrap().format,
            None
        );
    }

    #[test]
    fn a_format_is_named_by_its_four_characters() {
        assert_eq!(fourcc(Y420), "y420");
        assert_eq!(fourcc(24), "0x00000018");
    }
}
