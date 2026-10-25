//! `CGBitmapContext`: a `CGContext` over a caller's pixel buffer.

#![allow(non_snake_case)]

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;

use crate::quartz::{self, BitmapContext, State, context_of};

/// `kCGImageAlphaOnly`.
const ALPHA_ONLY: u32 = 7;

pub unsafe extern "C" fn CGBitmapContextCreate(
    data: *mut c_void,
    width: usize,
    height: usize,
    bits_per_component: usize,
    bytes_per_row: usize,
    space: CFTypeRef,
    bitmap_info: u32,
) -> CFTypeRef {
    if width == 0 || height == 0 || bits_per_component != 8 {
        return std::ptr::null();
    }
    let alpha = bitmap_info & quartz::ALPHA_INFO_MASK;
    let alpha_only = alpha == ALPHA_ONLY;
    // No space is RGB here unless the alpha is all there is.
    let components = crate::cgcolor::space_components(space).unwrap_or(3);
    let channels = match (alpha_only, components, alpha) {
        (true, _, _) => 1,
        (false, 1, 0) => 1,
        (false, 3, _) => 4,
        // Gray with alpha, CMYK and the like: refused, as Quartz refuses an
        // unsupported combination, rather than drawn in the wrong layout.
        _ => return std::ptr::null(),
    };
    let bytes_per_row = match bytes_per_row {
        0 => width * channels,
        n => n,
    };
    if bytes_per_row < width * channels {
        return std::ptr::null();
    }
    let mut owned = data.is_null().then(|| vec![0u8; bytes_per_row * height]);
    let data = owned
        .as_mut()
        .map_or(data.cast::<u8>(), |buffer| buffer.as_mut_ptr());
    let class = objc::class::lookup(c"__CGContext").expect("installed during bootstrap");
    // SAFETY: `BitmapContext` is the registered class layout.
    let context = unsafe { corefoundation::base::allocate::<BitmapContext>(class) };
    // SAFETY: a fresh allocation; the pixel storage is the caller's or owned
    // by the state, which lives as long as the context.
    unsafe {
        (*context).data = data;
        (*context).width = width;
        (*context).height = height;
        (*context).bytes_per_row = bytes_per_row;
        (*context).bits_per_component = bits_per_component;
        (*context).bitmap_info = bitmap_info;
        (*context).channels = channels;
        (*context).alpha_only = alpha_only;
        (*context).state = Box::into_raw(Box::new(State::new(owned)));
    }
    context as CFTypeRef
}

pub unsafe extern "C" fn CGBitmapContextGetData(context: CFTypeRef) -> *mut c_void {
    context_of(context).map_or(std::ptr::null_mut(), |context| context.data.cast())
}

pub unsafe extern "C" fn CGBitmapContextGetWidth(context: CFTypeRef) -> usize {
    context_of(context).map_or(0, |context| context.width)
}

pub unsafe extern "C" fn CGBitmapContextGetHeight(context: CFTypeRef) -> usize {
    context_of(context).map_or(0, |context| context.height)
}

/// `CGImageRef CGBitmapContextCreateImage(CGContextRef)`: a copy of the
/// pixels as they are now, so later drawing does not show through.
pub unsafe extern "C" fn CGBitmapContextCreateImage(context: CFTypeRef) -> CFTypeRef {
    let Some(ctx) = context_of(context) else {
        return std::ptr::null();
    };
    let Some(pixels) = quartz::snapshot_bgra(ctx) else {
        return std::ptr::null();
    };
    // A gray context's image has no alpha to read as coverage; every other
    // layout's does, an alpha-only one's being all it has.
    let info = match ctx.channels == 1 && !ctx.alpha_only {
        true => 6 | quartz::BYTE_ORDER_32_LITTLE,
        false => crate::cgimage::PREMULTIPLIED_BGRA,
    };
    crate::cgimage::image_from_pixels(pixels, ctx.width, ctx.height, ctx.width * 4, info)
}

pub unsafe extern "C" fn CGBitmapContextGetBitmapInfo(context: CFTypeRef) -> u32 {
    context_of(context).map_or(0, |context| context.bitmap_info)
}

pub unsafe extern "C" fn CGBitmapContextGetBitsPerComponent(context: CFTypeRef) -> usize {
    context_of(context).map_or(0, |context| context.bits_per_component)
}

pub unsafe extern "C" fn CGBitmapContextGetBitsPerPixel(context: CFTypeRef) -> usize {
    context_of(context).map_or(0, |context| context.channels * 8)
}

pub unsafe extern "C" fn CGBitmapContextGetBytesPerRow(context: CFTypeRef) -> usize {
    context_of(context).map_or(0, |context| context.bytes_per_row)
}

/// The context's space, which the caller does not own: device gray for a
/// gray context, none for an alpha-only one, and device RGB otherwise.
pub unsafe extern "C" fn CGBitmapContextGetColorSpace(context: CFTypeRef) -> CFTypeRef {
    static RGB: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    static GRAY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let Some(ctx) = context_of(context) else {
        return std::ptr::null();
    };
    // SAFETY: fresh spaces, kept for the process.
    let space = match (ctx.channels, ctx.alpha_only) {
        (1, true) => return std::ptr::null(),
        (1, false) => {
            *GRAY.get_or_init(|| unsafe { crate::cgcolor::CGColorSpaceCreateDeviceGray() as usize })
        }
        _ => *RGB.get_or_init(|| unsafe { crate::cgcolor::CGColorSpaceCreateDeviceRGB() as usize }),
    };
    space as CFTypeRef
}

exports::exports! {
    functions {
        "CGBitmapContextCreate" => CGBitmapContextCreate,
        "CGBitmapContextGetData" => CGBitmapContextGetData,
        "CGBitmapContextGetWidth" => CGBitmapContextGetWidth,
        "CGBitmapContextGetHeight" => CGBitmapContextGetHeight,
        "CGBitmapContextCreateImage" => CGBitmapContextCreateImage,
        "CGBitmapContextGetBitmapInfo" => CGBitmapContextGetBitmapInfo,
        "CGBitmapContextGetBitsPerComponent" => CGBitmapContextGetBitsPerComponent,
        "CGBitmapContextGetBitsPerPixel" => CGBitmapContextGetBitsPerPixel,
        "CGBitmapContextGetBytesPerRow" => CGBitmapContextGetBytesPerRow,
        "CGBitmapContextGetColorSpace" => CGBitmapContextGetColorSpace,
    }
}
