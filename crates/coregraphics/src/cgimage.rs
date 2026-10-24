//! `CGImage` and `CGImageSource`: pixels, masks, and encoded image data.

#![allow(non_snake_case)]

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;

use crate::cgcolor::{
    CGColorSpaceCreateDeviceGray, CGColorSpaceCreateDeviceRGB, CGColorSpaceRelease,
};
use crate::cgdataprovider::{
    CGDataProviderCreateWithData, CGDataProviderRelease, DataProvider, provider_bytes,
};

/// A `CGImage` over the bytes of a data provider, which it retains.
#[repr(C)]
pub(crate) struct Image {
    isa: *const c_void,
    width: usize,
    height: usize,
    bits_per_component: usize,
    bits_per_pixel: usize,
    bytes_per_row: usize,
    bitmap_info: u32,
    provider: CFTypeRef,
    space: CFTypeRef,
    /// Made by `CGImageMaskCreate`: its samples say where to paint, 0 being
    /// "paint" and full scale "do not".
    is_mask: bool,
    /// A mask's decode array, which maps a sample's 0 and full scale to the
    /// values above: `[0, 1]` unless the caller gave another.
    decode: [f64; 2],
}

/// An ImageIO source. The encoded data stays retained so decoding can be
/// deferred until `CGImageSourceCreateImageAtIndex`, as it is on Darwin.
#[repr(C)]
struct ImageSource {
    isa: *const c_void,
    data: CFTypeRef,
}

pub fn install() {
    let image = corefoundation::base::register(c"__CGImage", None, size_of::<Image>(), true);
    image.replace_method(
        objc::sel::get("dealloc"),
        image_dealloc as *const c_void,
        c"v16@0:8",
    );
    let source =
        corefoundation::base::register(c"__CGImageSource", None, size_of::<ImageSource>(), true);
    source.replace_method(
        objc::sel::get("dealloc"),
        image_source_dealloc as *const c_void,
        c"v16@0:8",
    );
}

unsafe extern "C" fn image_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__CGImage`, whose body is `Image`.
    unsafe {
        let image = &mut *(receiver as *mut Image);
        for object in [image.provider, image.space] {
            if !object.is_null() {
                corefoundation::api::CFRelease(object);
            }
        }
        image.provider = std::ptr::null();
        image.space = std::ptr::null();
        objc::arc::finalize(receiver);
    }
}

unsafe extern "C" fn image_source_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__CGImageSource`, whose body is `ImageSource`.
    unsafe {
        let source = &mut *(receiver as *mut ImageSource);
        corefoundation::api::CFRelease(source.data);
        source.data = std::ptr::null();
        objc::arc::finalize(receiver);
    }
}

/// Makes an ImageIO source over encoded image data.
pub unsafe extern "C" fn CGImageSourceCreateWithData(
    data: CFTypeRef,
    _options: CFTypeRef,
) -> CFTypeRef {
    // `CFDataGetBytePtr` rejects objects that are not this runtime's CFData.
    // An empty CFData has a non-null sentinel pointer, and is a valid source
    // even though it will not produce an image.
    if unsafe { corefoundation::api::CFDataGetBytePtr(data) }.is_null() {
        return std::ptr::null();
    }
    let class = objc::class::lookup(c"__CGImageSource").expect("installed during bootstrap");
    // SAFETY: `ImageSource` is the registered class layout and `data` was
    // checked above to be a live CFData.
    let source = unsafe { corefoundation::base::allocate::<ImageSource>(class) };
    unsafe {
        (*source).data = corefoundation::api::CFRetain(data);
    }
    source as CFTypeRef
}

/// Decodes the sole image in a source. The formats supported here are the same
/// ones NSImage accepts through the shared raster decoder.
pub unsafe extern "C" fn CGImageSourceCreateImageAtIndex(
    source: CFTypeRef,
    index: usize,
    _options: CFTypeRef,
) -> CFTypeRef {
    let class = objc::class::lookup(c"__CGImageSource").expect("installed during bootstrap");
    // SAFETY: null or an object supplied by the guest.
    if index != 0 || source.is_null() || !unsafe { corefoundation::base::is_kind_of(source, class) }
    {
        return std::ptr::null();
    }
    // SAFETY: checked above to be an `ImageSource` with a retained CFData.
    let data = unsafe { (*(source as *const ImageSource)).data };
    let length = unsafe { corefoundation::api::CFDataGetLength(data) };
    let Ok(length) = usize::try_from(length) else {
        return std::ptr::null();
    };
    let bytes = unsafe { corefoundation::api::CFDataGetBytePtr(data) };
    if bytes.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the CFData owns at least `length` readable bytes.
    let encoded = unsafe { std::slice::from_raw_parts(bytes, length) };
    crate::raster::decode(encoded).map_or(std::ptr::null(), image_from_bitmap)
}

fn is_image(image: CFTypeRef) -> bool {
    let class = objc::class::lookup(c"__CGImage").expect("installed during bootstrap");
    // SAFETY: null or an object a guest received from this runtime.
    !image.is_null() && unsafe { corefoundation::base::is_kind_of(image, class) }
}

fn image_of<'a>(image: CFTypeRef) -> Option<&'a Image> {
    // SAFETY: checked to be a `__CGImage`, whose body is `Image`.
    is_image(image).then(|| unsafe { &*(image as *const Image) })
}

#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CGImageCreate(
    width: usize,
    height: usize,
    bits_per_component: usize,
    bits_per_pixel: usize,
    bytes_per_row: usize,
    space: CFTypeRef,
    bitmap_info: u32,
    provider: CFTypeRef,
    _decode: *const f64,
    _should_interpolate: u8,
    _intent: i32,
) -> CFTypeRef {
    let class = objc::class::lookup(c"__CGDataProvider").expect("installed during bootstrap");
    // SAFETY: null or an object a guest received from this runtime.
    if provider.is_null() || !unsafe { corefoundation::base::is_kind_of(provider, class) } {
        return std::ptr::null();
    }
    if width == 0 || height == 0 || bits_per_pixel == 0 {
        return std::ptr::null();
    }
    let class = objc::class::lookup(c"__CGImage").expect("installed during bootstrap");
    // SAFETY: `Image` is the registered class layout.
    let image = unsafe { corefoundation::base::allocate::<Image>(class) };
    // SAFETY: a fresh allocation; both retained objects are live.
    unsafe {
        *image = Image {
            isa: (*image).isa,
            width,
            height,
            bits_per_component,
            bits_per_pixel,
            bytes_per_row,
            bitmap_info,
            provider: corefoundation::api::CFRetain(provider),
            space: if space.is_null() {
                space
            } else {
                corefoundation::api::CFRetain(space)
            },
            is_mask: false,
            decode: [0.0, 1.0],
        };
    }
    image as CFTypeRef
}

/// A `CGImage` that owns `pixels`, laid out as the arguments say.
pub fn image_from_pixels(
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    bytes_per_row: usize,
    bitmap_info: u32,
) -> CFTypeRef {
    unsafe extern "C" fn release(info: *mut c_void, _data: *const c_void, _size: usize) {
        // SAFETY: the box `image_from_pixels` leaked into `info`.
        drop(unsafe { Box::from_raw(info as *mut Vec<u8>) });
    }
    let pixels = Box::new(pixels);
    let (data, size) = (pixels.as_ptr(), pixels.len());
    // SAFETY: the provider owns the box until its release callback frees it,
    // and the vector's buffer does not move while boxed.
    unsafe {
        let provider = CGDataProviderCreateWithData(
            Box::into_raw(pixels).cast(),
            data.cast(),
            size,
            Some(release),
        );
        let space = CGColorSpaceCreateDeviceRGB();
        let image = CGImageCreate(
            width,
            height,
            8,
            32,
            bytes_per_row,
            space,
            bitmap_info,
            provider,
            std::ptr::null(),
            0,
            0,
        );
        CGColorSpaceRelease(space);
        CGDataProviderRelease(provider);
        image
    }
}

/// A `CGImage` over a decoded bitmap, which it takes ownership of.
///
/// Unpremultiplied RGBA with alpha last, big-endian component order - the
/// layout [`crate::raster::Bitmap`] already is, so nothing is converted.
pub fn image_from_bitmap(bitmap: crate::raster::Bitmap) -> CFTypeRef {
    unsafe extern "C" fn release(info: *mut c_void, _data: *const c_void, _size: usize) {
        // SAFETY: the box `image_from_bitmap` leaked into `info`.
        drop(unsafe { Box::from_raw(info as *mut Vec<u8>) });
    }
    const ALPHA_LAST: u32 = 3;
    let (width, height) = (bitmap.width as usize, bitmap.height as usize);
    let pixels = Box::new(bitmap.pixels);
    let (data, size) = (pixels.as_ptr(), pixels.len());
    // SAFETY: the provider owns the box until its release callback frees it,
    // and the vector's buffer does not move while boxed.
    unsafe {
        let provider = CGDataProviderCreateWithData(
            Box::into_raw(pixels).cast(),
            data.cast(),
            size,
            Some(release),
        );
        let space = CGColorSpaceCreateDeviceRGB();
        let image = CGImageCreate(
            width,
            height,
            8,
            32,
            width * 4,
            space,
            ALPHA_LAST,
            provider,
            std::ptr::null(),
            0,
            0,
        );
        CGColorSpaceRelease(space);
        CGDataProviderRelease(provider);
        image
    }
}

/// The pixels stay shared with the original: only the color space differs,
/// and nothing here color-manages.
pub unsafe extern "C" fn CGImageCreateCopyWithColorSpace(
    image: CFTypeRef,
    space: CFTypeRef,
) -> CFTypeRef {
    let Some(original) = image_of(image) else {
        return std::ptr::null();
    };
    unsafe {
        CGImageCreate(
            original.width,
            original.height,
            original.bits_per_component,
            original.bits_per_pixel,
            original.bytes_per_row,
            space,
            original.bitmap_info,
            original.provider,
            std::ptr::null(),
            0,
            0,
        )
    }
}

pub unsafe extern "C" fn CGImageRetain(image: CFTypeRef) -> CFTypeRef {
    if image.is_null() {
        return image;
    }
    unsafe { corefoundation::api::CFRetain(image) }
}

pub unsafe extern "C" fn CGImageRelease(image: CFTypeRef) {
    if !image.is_null() {
        unsafe { corefoundation::api::CFRelease(image) };
    }
}

pub unsafe extern "C" fn CGImageGetWidth(image: CFTypeRef) -> usize {
    image_of(image).map_or(0, |image| image.width)
}

pub unsafe extern "C" fn CGImageGetHeight(image: CFTypeRef) -> usize {
    image_of(image).map_or(0, |image| image.height)
}

pub unsafe extern "C" fn CGImageGetBitsPerComponent(image: CFTypeRef) -> usize {
    image_of(image).map_or(0, |image| image.bits_per_component)
}

pub unsafe extern "C" fn CGImageGetAlphaInfo(image: CFTypeRef) -> u32 {
    image_of(image).map_or(0, |image| image.bitmap_info & ALPHA_INFO_MASK)
}

pub unsafe extern "C" fn CGImageGetColorSpace(image: CFTypeRef) -> CFTypeRef {
    image_of(image).map_or(std::ptr::null(), |image| image.space)
}

const ALPHA_INFO_MASK: u32 = 0x1f;
/// `kCGImageAlphaOnly`.
const ALPHA_ONLY: u32 = 7;
const BYTE_ORDER_MASK: u32 = 0x7000;
const BYTE_ORDER_32_LITTLE: u32 = 0x2000;

/// How much of each pixel an image lets through when it is used as a mask, top
/// row first: an image mask's inverted samples, an image's alpha, or an opaque
/// image's luminance.
pub fn image_coverage(image: CFTypeRef) -> Option<(usize, usize, Vec<u8>)> {
    let is_mask = image_of(image)?.is_mask;
    let [low, high] = image_of(image)?.decode;
    let has_alpha = matches!(
        image_of(image)?.bitmap_info & ALPHA_INFO_MASK,
        1..=4 | ALPHA_ONLY
    );
    let (width, height, bgra) = image_bgra(image)?;
    let coverage = bgra
        .as_chunks::<4>().0.iter()
        .map(|p| match (is_mask, has_alpha) {
            (true, _) => {
                let decoded = low + f64::from(p[2]) / 255.0 * (high - low);
                ((1.0 - decoded).clamp(0.0, 1.0) * 255.0).round() as u8
            }
            (false, true) => p[3],
            (false, false) => {
                ((u32::from(p[2]) * 299 + u32::from(p[1]) * 587 + u32::from(p[0]) * 114) / 1000)
                    as u8
            }
        })
        .collect();
    Some((width, height, coverage))
}

/// Whether an image is an image mask, which paints the fill colour where its
/// samples allow rather than drawing pixels of its own.
pub fn is_image_mask(image: CFTypeRef) -> bool {
    image_of(image).is_some_and(|image| image.is_mask)
}

/// `CGImageRef CGImageMaskCreate(size_t width, size_t height, size_t bpc,
/// size_t bpp, size_t bytesPerRow, CGDataProviderRef, const CGFloat *decode,
/// bool shouldInterpolate)`
///
/// Eight-bit masks only; a one-bit mask is refused rather than read wrongly.
pub unsafe extern "C" fn CGImageMaskCreate(
    width: usize,
    height: usize,
    bits_per_component: usize,
    bits_per_pixel: usize,
    bytes_per_row: usize,
    provider: CFTypeRef,
    decode: *const f64,
    interpolate: u8,
) -> CFTypeRef {
    if bits_per_component != 8 || bits_per_pixel != 8 {
        return std::ptr::null();
    }
    // SAFETY: the caller's provider; a gray image of the same samples.
    let image = unsafe {
        let space = CGColorSpaceCreateDeviceGray();
        let image = CGImageCreate(
            width,
            height,
            8,
            8,
            bytes_per_row,
            space,
            0,
            provider,
            decode,
            interpolate,
            0,
        );
        CGColorSpaceRelease(space);
        image
    };
    if !image.is_null() {
        // SAFETY: an `Image` this call just made; `decode` is one pair for the
        // one component.
        unsafe {
            (*(image as *mut Image)).is_mask = true;
            if !decode.is_null() {
                (*(image as *mut Image)).decode = [*decode, *decode.add(1)];
            }
        }
    }
    image
}

/// `CGImageRef CGImageCreateWithImageInRect(CGImageRef, CGRect)`: the pixels
/// under the rectangle, which is in pixels from the top left and is cut to
/// the image.
pub unsafe extern "C" fn CGImageCreateWithImageInRect(
    image: CFTypeRef,
    rect: crate::CGRect,
) -> CFTypeRef {
    let Some((width, height, bgra)) = image_bgra(image) else {
        return std::ptr::null();
    };
    let left = rect.origin.x.floor().max(0.0) as usize;
    let top = rect.origin.y.floor().max(0.0) as usize;
    let right = ((rect.origin.x + rect.size.width).ceil().max(0.0) as usize).min(width);
    let bottom = ((rect.origin.y + rect.size.height).ceil().max(0.0) as usize).min(height);
    if left >= right || top >= bottom {
        return std::ptr::null();
    }
    let (w, h) = (right - left, bottom - top);
    let mut pixels = Vec::with_capacity(w * h * 4);
    for row in top..bottom {
        pixels.extend_from_slice(&bgra[(row * width + left) * 4..(row * width + right) * 4]);
    }
    let mask = image_of(image).filter(|i| i.is_mask).map(|i| i.decode);
    let image = image_from_pixels(pixels, w, h, w * 4, PREMULTIPLIED_BGRA);
    if let Some(decode) = mask
        && !image.is_null()
    {
        // SAFETY: a fresh image of this module's.
        unsafe {
            (*(image as *mut Image)).is_mask = true;
            (*(image as *mut Image)).decode = decode;
        }
    }
    image
}

/// `kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little`, the layout
/// [`image_bgra`] answers in.
pub const PREMULTIPLIED_BGRA: u32 = 2 | BYTE_ORDER_32_LITTLE;

/// `CGImageRef CGImageCreateWithPNGDataProvider(CGDataProviderRef, const
/// CGFloat *decode, bool, CGColorRenderingIntent)`
pub unsafe extern "C" fn CGImageCreateWithPNGDataProvider(
    provider: CFTypeRef,
    _decode: *const f64,
    _interpolate: u8,
    _intent: i32,
) -> CFTypeRef {
    provider_bytes(provider)
        .and_then(crate::raster::decode)
        .map_or(std::ptr::null(), image_from_bitmap)
}

/// A `CGImage`'s pixels as premultiplied B, G, R, A rows, top row first -
/// the layout the focused view's backing store composites in.
///
/// `None` for a pixel layout this does not decode, which is anything but
/// eight bits per component in three or four components.
pub fn image_bgra(image: CFTypeRef) -> Option<(usize, usize, Vec<u8>)> {
    let image = image_of(image)?;
    // SAFETY: a `__CGImage` holds a retained `__CGDataProvider`.
    let provider = unsafe { &*(image.provider as *const DataProvider) };
    if image.bits_per_component != 8 || provider.data.is_null() {
        return None;
    }
    let channels = match image.bits_per_pixel {
        32 => 4,
        24 => 3,
        8 => 1,
        _ => return None,
    };
    let stride = image.bytes_per_row.max(image.width * channels);
    let needed = stride
        .checked_mul(image.height - 1)?
        .checked_add(image.width * channels)?;
    if provider.size < needed {
        return None;
    }
    // SAFETY: the provider was created over `size` readable bytes, which the
    // check above shows cover every row.
    let bytes = unsafe { std::slice::from_raw_parts(provider.data, needed) };
    let alpha = image.bitmap_info & ALPHA_INFO_MASK;
    let little = image.bitmap_info & BYTE_ORDER_MASK == BYTE_ORDER_32_LITTLE;
    // `First` alphas (and `NoneSkipFirst`) lead the component order; the
    // component order is reversed in memory for a little-endian 32-bit image.
    let alpha_first = matches!(alpha, 2 | 4 | 6);
    let has_alpha = matches!(alpha, 1..=4);
    let premultiplied = matches!(alpha, 1 | 2);
    let mut out = vec![0u8; image.width * image.height * 4];
    for row in 0..image.height {
        let from = &bytes[row * stride..row * stride + image.width * channels];
        let to = &mut out[row * image.width * 4..(row + 1) * image.width * 4];
        for (source, target) in from.chunks_exact(channels).zip(to.chunks_exact_mut(4)) {
            let mut ordered = [0u8; 4];
            ordered[..channels].copy_from_slice(source);
            if little && channels == 4 {
                ordered.reverse();
            }
            let (a, r, g, b) = match (channels, alpha_first) {
                (4, true) => (ordered[0], ordered[1], ordered[2], ordered[3]),
                (4, false) => (ordered[3], ordered[0], ordered[1], ordered[2]),
                // `kCGImageAlphaOnly`: coverage and no colour.
                (1, _) if alpha == ALPHA_ONLY => (ordered[0], 0, 0, 0),
                // Gray, or an image mask read as one.
                (1, _) => (255, ordered[0], ordered[0], ordered[0]),
                _ => (255, ordered[0], ordered[1], ordered[2]),
            };
            let a = if has_alpha || (channels == 1 && alpha == ALPHA_ONLY) {
                a
            } else {
                255
            };
            let scale = |c: u8| match premultiplied || a == 255 {
                true => c.min(a),
                false => ((u16::from(c) * u16::from(a) + 127) / 255) as u8,
            };
            target.copy_from_slice(&[scale(b), scale(g), scale(r), a]);
        }
    }
    Some((image.width, image.height, out))
}

exports::exports! {
    functions {
        "CGImageCreate" => CGImageCreate,
        "CGImageMaskCreate" => CGImageMaskCreate,
        "CGImageCreateWithImageInRect" => CGImageCreateWithImageInRect,
        "CGImageCreateWithPNGDataProvider" => CGImageCreateWithPNGDataProvider,
        "CGImageCreateCopyWithColorSpace" => CGImageCreateCopyWithColorSpace,
        "CGImageRetain" => CGImageRetain,
        "CGImageRelease" => CGImageRelease,
        "CGImageGetWidth" => CGImageGetWidth,
        "CGImageGetHeight" => CGImageGetHeight,
        "CGImageGetBitsPerComponent" => CGImageGetBitsPerComponent,
        "CGImageGetAlphaInfo" => CGImageGetAlphaInfo,
        "CGImageGetColorSpace" => CGImageGetColorSpace,
        "CGImageSourceCreateWithData" => CGImageSourceCreateWithData,
        "CGImageSourceCreateImageAtIndex" => CGImageSourceCreateImageAtIndex,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cgdataprovider::{CGDataProviderCreateWithData, CGDataProviderRelease};

    #[test]
    fn image_source_decodes_its_first_image() {
        crate::bootstrap();
        // A one-pixel, 24-bit BMP. Its stored BGR pixel is red and the row is
        // padded to four bytes.
        let mut bmp = vec![0u8; 58];
        bmp[0..2].copy_from_slice(b"BM");
        bmp[2..6].copy_from_slice(&58u32.to_le_bytes());
        bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
        bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
        bmp[18..22].copy_from_slice(&1u32.to_le_bytes());
        bmp[22..26].copy_from_slice(&1u32.to_le_bytes());
        bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
        bmp[28..30].copy_from_slice(&24u16.to_le_bytes());
        bmp[54..58].copy_from_slice(&[0, 0, 255, 0]);
        // SAFETY: every object remains live until its matching release.
        unsafe {
            let data = corefoundation::api::CFDataCreate(
                std::ptr::null(),
                bmp.as_ptr(),
                bmp.len() as isize,
            );
            let source = CGImageSourceCreateWithData(data, std::ptr::null());
            corefoundation::api::CFRelease(data);
            assert!(!source.is_null());
            assert!(CGImageSourceCreateImageAtIndex(source, 1, std::ptr::null()).is_null());
            let image = CGImageSourceCreateImageAtIndex(source, 0, std::ptr::null());
            assert_eq!((CGImageGetWidth(image), CGImageGetHeight(image)), (1, 1));
            assert!(!CGImageGetColorSpace(image).is_null());
            CGImageRelease(image);
            corefoundation::api::CFRelease(source);
        }
    }

    #[test]
    fn image_over_little_endian_premultiplied_first_reads_as_bgra() {
        crate::bootstrap();
        static RELEASED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        unsafe extern "C" fn release(_info: *mut c_void, _data: *const c_void, size: usize) {
            RELEASED.store(size, std::sync::atomic::Ordering::SeqCst);
        }
        // 0xAARRGGBB in host (little) order: two pixels, opaque red and
        // half-transparent green.
        let pixels: [u32; 2] = [0xffff_0000, 0x8000_8000];
        // SAFETY: the provider is released before `pixels` goes out of scope.
        unsafe {
            let provider = CGDataProviderCreateWithData(
                std::ptr::null_mut(),
                pixels.as_ptr().cast(),
                8,
                Some(release),
            );
            let image = CGImageCreate(
                2,
                1,
                8,
                32,
                8,
                std::ptr::null(),
                0x2002,
                provider,
                std::ptr::null(),
                0,
                0,
            );
            CGDataProviderRelease(provider);
            assert_eq!(CGImageGetWidth(image), 2);
            let copy = CGImageCreateCopyWithColorSpace(image, std::ptr::null());
            CGImageRelease(image);
            let (width, height, bgra) = image_bgra(copy).expect("decodable");
            assert_eq!((width, height), (2, 1));
            assert_eq!(bgra, [0, 0, 0xff, 0xff, 0, 0x80, 0, 0x80]);
            assert_eq!(RELEASED.load(std::sync::atomic::Ordering::SeqCst), 0);
            CGImageRelease(copy);
            assert_eq!(RELEASED.load(std::sync::atomic::Ordering::SeqCst), 8);
        }
    }
}
