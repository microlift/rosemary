//! CPU-backed CoreVideo pixel buffers.
//!
//! The storage is owned by a CF/Objective-C object so CFRelease, CVBufferRelease
//! and CVPixelBufferRelease all participate in the same reference count.

#![allow(non_snake_case)]

use std::alloc::{Layout, alloc_zeroed, dealloc as free_storage};
use std::ffi::{CStr, c_void};
use std::ptr::NonNull;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use corefoundation::base::CFTypeRef;
use objc::class::Class;

const SUCCESS: i32 = 0;
const ERROR: i32 = -6660;
const INVALID_ARGUMENT: i32 = -6661;
const ALLOCATION_FAILED: i32 = -6662;
const UNSUPPORTED: i32 = -6663;
const INVALID_PIXEL_FORMAT: i32 = -6680;
const INVALID_SIZE: i32 = -6681;
const INVALID_ATTRIBUTES: i32 = -6682;

const BGRA: u32 = u32::from_be_bytes(*b"BGRA");
const NV12_VIDEO: u32 = u32::from_be_bytes(*b"420v");
const NV12_FULL: u32 = u32::from_be_bytes(*b"420f");
const I420_VIDEO: u32 = u32::from_be_bytes(*b"y420");
const I420_FULL: u32 = u32::from_be_bytes(*b"f420");
#[cfg(test)]
const READ_ONLY: u64 = 1;
const DEFAULT_ALIGNMENT: usize = 64;
const MAX_STORAGE: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy)]
struct Plane {
    /// From the base address, for storage this runtime lays out.
    offset: usize,
    /// Where the plane's pixels are, once there is memory for them.
    address: *mut u8,
    width: usize,
    height: usize,
    row_bytes: usize,
}

struct Storage {
    // The logical base may be offset into the heap allocation when an
    // attribute requires a non-power-of-two address multiple.
    ptr: NonNull<u8>,
    allocation: NonNull<u8>,
    layout: Layout,
}

impl Storage {
    fn new(size: usize, alignment: usize) -> Result<Self, i32> {
        let total = size.checked_add(alignment - 1).ok_or(ALLOCATION_FAILED)?;
        if total > MAX_STORAGE {
            return Err(ALLOCATION_FAILED);
        }
        let layout =
            Layout::from_size_align(total, DEFAULT_ALIGNMENT).map_err(|_| ALLOCATION_FAILED)?;
        // SAFETY: nonzero, checked length and valid Layout alignment.
        let allocation = NonNull::new(unsafe { alloc_zeroed(layout) }).ok_or(ALLOCATION_FAILED)?;
        let Some(base) = aligned(allocation.as_ptr() as usize, alignment) else {
            // SAFETY: the allocation was obtained with this exact Layout.
            unsafe { free_storage(allocation.as_ptr(), layout) };
            return Err(ALLOCATION_FAILED);
        };
        // SAFETY: overallocated by alignment - 1 bytes and the aligned region
        // is exactly size bytes long, entirely within the heap allocation.
        let ptr = unsafe {
            NonNull::new_unchecked(allocation.as_ptr().add(base - allocation.as_ptr() as usize))
        };
        Ok(Self {
            ptr,
            allocation,
            layout,
        })
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        // SAFETY: allocation and Layout are unchanged since allocation.
        unsafe { free_storage(self.allocation.as_ptr(), self.layout) };
    }
}

/// What `CVPixelBufferCreateWithBytes` and its planar form hand back at
/// final release, so the caller can free memory it lent.
enum Release {
    Bytes {
        callback: BytesCallback,
        refcon: *mut c_void,
        base: *const c_void,
    },
    Planar {
        callback: PlanarBytesCallback,
        refcon: *mut c_void,
        data: *const c_void,
        size: usize,
        planes: Vec<*const c_void>,
    },
}

/// `CVPixelBufferReleaseBytesCallback`.
type BytesCallback = unsafe extern "C" fn(*mut c_void, *const c_void);
/// `CVPixelBufferReleasePlanarBytesCallback`.
type PlanarBytesCallback =
    unsafe extern "C" fn(*mut c_void, *const c_void, usize, usize, *const *const c_void);

impl Drop for Release {
    fn drop(&mut self) {
        // SAFETY: the callbacks and their arguments are the creator's own.
        unsafe {
            match self {
                Release::Bytes {
                    callback,
                    refcon,
                    base,
                } => callback(*refcon, *base),
                Release::Planar {
                    callback,
                    refcon,
                    data,
                    size,
                    planes,
                } => callback(*refcon, *data, *size, planes.len(), planes.as_ptr()),
            }
        }
    }
}

/// Where the pixels live.
enum Memory {
    // Never resized: C clients retain pointers to its pixels until final release.
    Owned(Storage),
    /// The creator's, handed back through its callback, if any.
    Lent {
        base: *mut u8,
        _release: Option<Release>,
    },
}

impl Memory {
    fn base(&self) -> *mut u8 {
        match self {
            Memory::Owned(storage) => storage.ptr.as_ptr(),
            Memory::Lent { base, .. } => *base,
        }
    }
}

struct State {
    width: usize,
    height: usize,
    format: u32,
    planes: Vec<Plane>,
    row_bytes: usize,
    memory: Memory,
    data_size: usize,
    locks: AtomicUsize,
    attachments: std::sync::Mutex<attachments::Attachments>,
}

fn class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let addr = *CLASS.get_or_init(|| {
        corefoundation::bootstrap();
        let class = corefoundation::boxed::register::<State>(c"__NSCVPixelBuffer", None, true);
        objc::class::ptr(class) as usize
    });
    // SAFETY: the registered class lives for the process.
    unsafe { &*(addr as *const Class) }
}

fn state<'a>(buffer: CFTypeRef) -> Option<&'a State> {
    if buffer.is_null() {
        return None;
    }
    // SAFETY: caller holds a live CF object; its class is checked first.
    unsafe { corefoundation::boxed::contents::<State>(buffer, class()) }.map(|s| &*s)
}

/// The host CoreVideo's entry point `name`, when `object` is a buffer or pool
/// the host made rather than this runtime.
///
/// With the builtin frameworks on a Mac, VideoToolbox and its pools are still
/// Apple's, so their buffers reach these entry points too. See
/// [`corefoundation::foreign`]: the alternative is reading Apple's object as
/// one of ours.
#[cfg(target_os = "macos")]
fn foreign(name: &CStr, object: CFTypeRef) -> Option<*mut c_void> {
    // SAFETY: null or an object, whose first word is readable.
    if !unsafe { corefoundation::foreign::is(object) } {
        return None;
    }
    if !corefoundation::foreign::forwarding() {
        objc::fail!(
            "{}: the object is not this runtime's, and ROSEMARY_NO_FOREIGN forbids passing it \
             to the CoreVideo that made it",
            name.to_string_lossy()
        );
    }
    let Some(f) = corefoundation::foreign::entry(name) else {
        objc::fail!(
            "{}: the object is not this runtime's and there is no host CoreVideo to pass it to",
            name.to_string_lossy()
        );
    };
    Some(f)
}

/// Off Darwin every buffer the guest holds is this runtime's.
#[cfg(not(target_os = "macos"))]
fn foreign(_name: &CStr, _object: CFTypeRef) -> Option<*mut c_void> {
    None
}

/// Refuse a foreign `object` in a call whose CF arguments or results are this
/// runtime's: the host CoreVideo cannot read those, and the guest could not
/// use what it would answer. There is no answer to give, so it is a refusal
/// by name rather than a quiet null.
#[cfg(target_os = "macos")]
fn refuse_foreign(name: &CStr, object: CFTypeRef) {
    // SAFETY: null or an object, whose first word is readable.
    if unsafe { corefoundation::foreign::is(object) } {
        objc::fail!(
            "{}: the object is the host CoreVideo's, and its CF arguments are this runtime's",
            name.to_string_lossy()
        );
    }
}

#[cfg(not(target_os = "macos"))]
fn refuse_foreign(_name: &CStr, _object: CFTypeRef) {}

/// Return the host CoreVideo's answer for a foreign `$object`; see [`foreign`].
macro_rules! forward_foreign {
    ($name:expr, $object:expr, fn($($ty:ty),*) -> $ret:ty, ($($arg:expr),*)) => {
        if let Some(f) = foreign($name, $object) {
            // SAFETY: CoreVideo's own declaration of this entry point, called
            // with an object it made.
            return unsafe {
                let f: unsafe extern "C" fn($($ty),*) -> $ret = std::mem::transmute(f);
                f($($arg),*)
            };
        }
    };
}

mod attachments;
mod metal_texture;
mod pool;

pub use attachments::*;
pub use metal_texture::*;
pub use pool::*;

fn aligned(value: usize, alignment: usize) -> Option<usize> {
    let remainder = value % alignment;
    value.checked_add(if remainder == 0 {
        0
    } else {
        alignment - remainder
    })
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn lcm(a: usize, b: usize) -> Option<usize> {
    (a / gcd(a, b)).checked_mul(b)
}

fn key(text: &str) -> CFTypeRef {
    corefoundation::base::interned_name(text) as usize as CFTypeRef
}

fn attribute(attrs: CFTypeRef, name: &str) -> CFTypeRef {
    if attrs.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller provided a CFDictionary, and the key is a CFString.
    unsafe { corefoundation::api::CFDictionaryGetValue(attrs, key(name)) }
}

/// A numeric attribute as the CoreVideo allocator reads it: floating values
/// truncate toward zero, and absent or non-numeric values and NaN act like
/// zero. A negative value or an infinity fails as an allocation request, not
/// as a malformed key. CFBoolean shares a number-like representation in
/// CoreFoundation; `booleans` says whether this attribute reads it as one.
fn numeric_attribute(attrs: CFTypeRef, name: &str, booleans: bool) -> Result<usize, i32> {
    let value = attribute(attrs, name);
    let number = if value.is_null()
        || (!booleans
            && unsafe { corefoundation::api::CFGetTypeID(value) }
                == unsafe { corefoundation::api::CFBooleanGetTypeID() })
    {
        None
    } else {
        // SAFETY: the guest supplied a CF value from the attributes dictionary.
        unsafe { corefoundation::collections::number(value) }
    };
    let n = match number {
        Some(corefoundation::collections::Number::Integer(n)) => n,
        Some(corefoundation::collections::Number::Float(n)) if n.is_infinite() => {
            return Err(ALLOCATION_FAILED);
        }
        Some(corefoundation::collections::Number::Float(n)) => n as i64,
        None => 0,
    };
    usize::try_from(n).map_err(|_| ALLOCATION_FAILED)
}

/// An alignment multiple, where zero selects the default.
fn alignment_attribute(attrs: CFTypeRef, name: &str) -> Result<usize, i32> {
    match numeric_attribute(attrs, name, true)? {
        0 => Ok(DEFAULT_ALIGNMENT),
        multiple => Ok(multiple),
    }
}

fn requires_unsupported_backing(attrs: CFTypeRef) -> Result<bool, i32> {
    // IOSurface and GPU-compatibility requests are met by the same CPU
    // memory: every consumer of these buffers here reaches the pixels
    // through the base-address API. The IOSurface properties must still be
    // a dictionary, as CoreVideo validates them.
    let iosurface = attribute(attrs, "IOSurfaceProperties");
    if !iosurface.is_null()
        && unsafe { corefoundation::collections::dictionary(iosurface) }.is_none()
    {
        return Err(INVALID_ATTRIBUTES);
    }
    // CPU-backed BGRA and YCbCr planes are already accessible to software
    // CoreGraphics, regardless of the CF type of a compatibility hint.
    // Native CoreVideo ignores CFBoolean values for extended pixels.
    for name in [
        "ExtendedPixelsLeft",
        "ExtendedPixelsTop",
        "ExtendedPixelsRight",
        "ExtendedPixelsBottom",
    ] {
        if numeric_attribute(attrs, name, false)? != 0 {
            // Padding around the image is not laid out here.
            return Ok(true);
        }
    }
    let allocator = attribute(attrs, "MemoryAllocator");
    if !allocator.is_null() && !corefoundation::api::supports_malloc_allocator(allocator.cast()) {
        // Nonstandard callback allocators are represented by CF objects, but
        // their allocation callbacks are not implemented in CoreFoundation.
        if unsafe { corefoundation::api::CFGetTypeID(allocator) }
            != unsafe { corefoundation::api::CFAllocatorGetTypeID() }
        {
            return Err(INVALID_ATTRIBUTES);
        }
        return Ok(true);
    }
    Ok(false)
}

fn layout(
    width: usize,
    height: usize,
    format: u32,
    attrs: CFTypeRef,
) -> Result<(Vec<Plane>, usize, usize, usize), i32> {
    if width == 0 || height == 0 {
        return Err(INVALID_SIZE);
    }
    if !matches!(
        format,
        BGRA | NV12_VIDEO | NV12_FULL | I420_VIDEO | I420_FULL
    ) {
        return Err(INVALID_PIXEL_FORMAT);
    }
    let row_alignment = lcm(
        DEFAULT_ALIGNMENT,
        alignment_attribute(attrs, "BytesPerRowAlignment")?,
    )
    .ok_or(ALLOCATION_FAILED)?;
    // Preserve the CPU-friendly 64-byte minimum at *each plane*, not only
    // at the overall base. A non-power-of-two attribute is an additional
    // multiple, so round offsets to the checked LCM of both requirements.
    let plane_alignment = if format == BGRA {
        DEFAULT_ALIGNMENT
    } else {
        lcm(
            DEFAULT_ALIGNMENT,
            alignment_attribute(attrs, "PlaneAlignment")?,
        )
        .ok_or(ALLOCATION_FAILED)?
    };
    let chroma_width = width.div_ceil(2);
    let chroma_height = height.div_ceil(2);
    let dimensions: Vec<(usize, usize, usize)> = match format {
        BGRA => vec![(width, height, 4)],
        NV12_VIDEO | NV12_FULL => vec![(width, height, 1), (chroma_width, chroma_height, 2)],
        _ => vec![
            (width, height, 1),
            (chroma_width, chroma_height, 1),
            (chroma_width, chroma_height, 1),
        ],
    };
    let planar = dimensions.len() > 1;
    let header_len = if planar { dimensions.len() * 8 } else { 0 };
    let mut offset = aligned(header_len, plane_alignment).ok_or(INVALID_SIZE)?;
    let mut planes = Vec::with_capacity(dimensions.len());
    for (plane_width, plane_height, bytes_per_pixel) in dimensions {
        let minimum = plane_width
            .checked_mul(bytes_per_pixel)
            .ok_or(INVALID_SIZE)?;
        let row_bytes = aligned(minimum, row_alignment).ok_or(INVALID_SIZE)?;
        let len = row_bytes.checked_mul(plane_height).ok_or(INVALID_SIZE)?;
        offset = aligned(offset, plane_alignment).ok_or(INVALID_SIZE)?;
        if offset > i32::MAX as usize || row_bytes > u32::MAX as usize {
            return Err(INVALID_SIZE);
        }
        planes.push(Plane {
            offset,
            address: std::ptr::null_mut(),
            width: plane_width,
            height: plane_height,
            row_bytes,
        });
        offset = offset.checked_add(len).ok_or(INVALID_SIZE)?;
    }
    // The planar rowBytes is a buffer-wide envelope. The rounded-up stride
    // times the image height may exceed the last plane's end offset; the
    // entire advertised envelope must be safe to access from BaseAddress.
    let (row_bytes, size) = if planar {
        let row_bytes = offset.div_ceil(height);
        let envelope = row_bytes.checked_mul(height).ok_or(INVALID_SIZE)?;
        (row_bytes, envelope)
    } else {
        (planes[0].row_bytes, offset)
    };
    if size > MAX_STORAGE {
        // Valid dimensions may exceed this implementation's allocation cap.
        // Report memory exhaustion, not malformed dimensions.
        return Err(ALLOCATION_FAILED);
    }
    // Pointer alignment and row-byte divisibility are different contracts.
    // Plane addresses are multiples of the plane alignment, which already
    // includes the 64-byte minimum. Only the power-of-two part of a row
    // multiple can align every row start, so only that part reaches the
    // base; an odd factor would just inflate the allocation.
    let row_address = 1 << row_alignment.trailing_zeros();
    let address_alignment = lcm(plane_alignment, row_address).ok_or(ALLOCATION_FAILED)?;
    Ok((planes, row_bytes, size, address_alignment))
}

/// CVReturn CVPixelBufferCreate(CFAllocatorRef, size_t, size_t, OSType,
/// CFDictionaryRef, CVPixelBufferRef *).
///
/// # Safety
/// Attributes must be a CFDictionary or NULL; out must be writable.
pub unsafe extern "C" fn CVPixelBufferCreate(
    allocator: CFTypeRef,
    width: usize,
    height: usize,
    format: u32,
    attrs: CFTypeRef,
    out: *mut CFTypeRef,
) -> i32 {
    match unsafe { check_create(allocator, attrs, out) } {
        SUCCESS => {}
        error => return error,
    }
    let (planes, row_bytes, size, storage_alignment) = match layout(width, height, format, attrs) {
        Ok(layout) => layout,
        Err(error) => return error,
    };
    let data = match Storage::new(size, storage_alignment) {
        Ok(data) => data,
        Err(error) => return error,
    };
    let base = data.ptr.as_ptr();
    let mut planes = planes;
    for plane in &mut planes {
        // SAFETY: each plane is inside the checked allocation.
        plane.address = unsafe { base.add(plane.offset) };
    }
    if planes.len() > 1 {
        for (i, plane) in planes.iter().enumerate() {
            // CVPlanarComponentInfo is (int32_t offset, uint32_t rowBytes), BE.
            let at = i * 8;
            // SAFETY: descriptor is inside the checked allocation.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    (plane.offset as i32).to_be_bytes().as_ptr(),
                    base.add(at),
                    4,
                );
                std::ptr::copy_nonoverlapping(
                    (plane.row_bytes as u32).to_be_bytes().as_ptr(),
                    base.add(at + 4),
                    4,
                );
            }
        }
    }
    unsafe {
        publish(
            State {
                width,
                height,
                format,
                planes,
                row_bytes,
                memory: Memory::Owned(data),
                data_size: size,
                locks: AtomicUsize::new(0),
                attachments: Default::default(),
            },
            out,
        )
    }
}

/// Make the object for `state` and store it through `out`.
///
/// # Safety
/// `out` must be writable.
unsafe fn publish(state: State, out: *mut CFTypeRef) -> i32 {
    let buffer = corefoundation::boxed::make(class(), state);
    if buffer.is_null() {
        return ALLOCATION_FAILED;
    }
    // SAFETY: the caller's obligation.
    unsafe { *out = buffer };
    SUCCESS
}

/// What every create call checks before its own arguments.
///
/// # Safety
/// `out` must be null or writable; `attrs` null or a CF object.
unsafe fn check_create(allocator: CFTypeRef, attrs: CFTypeRef, out: *mut CFTypeRef) -> i32 {
    if out.is_null() {
        return INVALID_ARGUMENT;
    }
    // SAFETY: the caller's obligation.
    unsafe { *out = std::ptr::null() };
    if !corefoundation::api::supports_malloc_allocator(allocator.cast()) {
        return UNSUPPORTED;
    }
    if !attrs.is_null() && unsafe { corefoundation::collections::dictionary(attrs) }.is_none() {
        return INVALID_ATTRIBUTES;
    }
    match requires_unsupported_backing(attrs) {
        Ok(true) => UNSUPPORTED,
        Err(error) => error,
        Ok(false) => SUCCESS,
    }
}

/// CVReturn CVPixelBufferCreateWithBytes(CFAllocatorRef, size_t width, size_t
/// height, OSType, void *baseAddress, size_t bytesPerRow,
/// CVPixelBufferReleaseBytesCallback, void *releaseRefCon, CFDictionaryRef,
/// CVPixelBufferRef *).
///
/// The pixels stay the caller's; the callback hands them back at final
/// release. It is not called when creation fails.
///
/// # Safety
/// As [`CVPixelBufferCreate`]; `base` must hold `height` rows of
/// `bytes_per_row` until the callback runs.
pub unsafe extern "C" fn CVPixelBufferCreateWithBytes(
    allocator: CFTypeRef,
    width: usize,
    height: usize,
    format: u32,
    base: *mut c_void,
    bytes_per_row: usize,
    release: Option<BytesCallback>,
    refcon: *mut c_void,
    attrs: CFTypeRef,
    out: *mut CFTypeRef,
) -> i32 {
    match unsafe { check_create(allocator, attrs, out) } {
        SUCCESS => {}
        error => return error,
    }
    if width == 0 || height == 0 {
        return INVALID_SIZE;
    }
    // The planar formats have their own entry point.
    if format != BGRA {
        return INVALID_PIXEL_FORMAT;
    }
    let Some(data_size) = bytes_per_row.checked_mul(height) else {
        return INVALID_SIZE;
    };
    if base.is_null() || width.checked_mul(4).is_none_or(|row| bytes_per_row < row) {
        return INVALID_ARGUMENT;
    }
    let plane = Plane {
        offset: 0,
        address: base.cast(),
        width,
        height,
        row_bytes: bytes_per_row,
    };
    let release = release.map(|callback| Release::Bytes {
        callback,
        refcon,
        base,
    });
    unsafe {
        publish(
            State {
                width,
                height,
                format,
                planes: vec![plane],
                row_bytes: bytes_per_row,
                memory: Memory::Lent {
                    base: base.cast(),
                    _release: release,
                },
                data_size,
                locks: AtomicUsize::new(0),
                attachments: Default::default(),
            },
            out,
        )
    }
}

/// CVReturn CVPixelBufferCreateWithPlanarBytes(CFAllocatorRef, size_t width,
/// size_t height, OSType, void *dataPtr, size_t dataSize, size_t
/// numberOfPlanes, void *planeBaseAddress[], size_t planeWidth[], size_t
/// planeHeight[], size_t planeBytesPerRow[],
/// CVPixelBufferReleasePlanarBytesCallback, void *releaseRefCon,
/// CFDictionaryRef, CVPixelBufferRef *).
///
/// # Safety
/// As [`CVPixelBufferCreateWithBytes`], for each plane; the arrays must have
/// `plane_count` entries.
pub unsafe extern "C" fn CVPixelBufferCreateWithPlanarBytes(
    allocator: CFTypeRef,
    width: usize,
    height: usize,
    format: u32,
    data: *mut c_void,
    data_size: usize,
    plane_count: usize,
    plane_bases: *const *mut c_void,
    plane_widths: *const usize,
    plane_heights: *const usize,
    plane_rows: *const usize,
    release: Option<PlanarBytesCallback>,
    refcon: *mut c_void,
    attrs: CFTypeRef,
    out: *mut CFTypeRef,
) -> i32 {
    match unsafe { check_create(allocator, attrs, out) } {
        SUCCESS => {}
        error => return error,
    }
    if width == 0 || height == 0 {
        return INVALID_SIZE;
    }
    let expected = match format {
        NV12_VIDEO | NV12_FULL => 2,
        I420_VIDEO | I420_FULL => 3,
        _ => return INVALID_PIXEL_FORMAT,
    };
    if plane_count != expected
        || [
            plane_bases.cast::<c_void>(),
            plane_widths.cast(),
            plane_heights.cast(),
            plane_rows.cast(),
        ]
        .iter()
        .any(|p| p.is_null())
    {
        return INVALID_ARGUMENT;
    }
    let mut planes = Vec::with_capacity(plane_count);
    for i in 0..plane_count {
        // SAFETY: the caller's arrays, of `plane_count` entries each.
        let plane = unsafe {
            Plane {
                offset: 0,
                address: (*plane_bases.add(i)).cast(),
                width: *plane_widths.add(i),
                height: *plane_heights.add(i),
                row_bytes: *plane_rows.add(i),
            }
        };
        if plane.address.is_null() || plane.width == 0 || plane.height == 0 {
            return INVALID_ARGUMENT;
        }
        planes.push(plane);
    }
    let release = release.map(|callback| Release::Planar {
        callback,
        refcon,
        data,
        size: data_size,
        planes: planes
            .iter()
            .map(|p| p.address.cast_const().cast())
            .collect(),
    });
    unsafe {
        publish(
            State {
                width,
                height,
                format,
                planes,
                // Only a buffer whose base is a CVPlanarPixelBufferInfo this
                // runtime laid out has a buffer-wide row envelope.
                row_bytes: 0,
                memory: Memory::Lent {
                    base: data.cast(),
                    _release: release,
                },
                data_size,
                locks: AtomicUsize::new(0),
                attachments: Default::default(),
            },
            out,
        )
    }
}

/// void CVPixelBufferGetExtendedPixels(CVPixelBufferRef, size_t *left,
/// size_t *right, size_t *top, size_t *bottom): none, since a buffer that
/// asks for any is refused.
pub unsafe extern "C" fn CVPixelBufferGetExtendedPixels(
    buffer: CFTypeRef,
    left: *mut usize,
    right: *mut usize,
    top: *mut usize,
    bottom: *mut usize,
) {
    forward_foreign!(
        c"CVPixelBufferGetExtendedPixels",
        buffer,
        fn(CFTypeRef, *mut usize, *mut usize, *mut usize, *mut usize) -> (),
        (buffer, left, right, top, bottom)
    );
    for out in [left, right, top, bottom] {
        if !out.is_null() {
            // SAFETY: the caller's out-parameter.
            unsafe { *out = 0 };
        }
    }
}

/// `CGSize`, as both architectures lay it out.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

/// CGSize CVImageBufferGetEncodedSize(CVImageBufferRef): the full buffer.
pub unsafe extern "C" fn CVImageBufferGetEncodedSize(buffer: CFTypeRef) -> CGSize {
    forward_foreign!(
        c"CVImageBufferGetEncodedSize",
        buffer,
        fn(CFTypeRef) -> CGSize,
        (buffer)
    );
    state(buffer).map_or(CGSize::default(), |s| CGSize {
        width: s.width as f64,
        height: s.height as f64,
    })
}

/// CGSize CVImageBufferGetDisplaySize(CVImageBufferRef).
///
/// FIXME (ACCURACY): Apple's answer is the clean aperture scaled by the pixel
/// aspect ratio attachments; this is the encoded size, which it equals for a
/// buffer with neither.
pub unsafe extern "C" fn CVImageBufferGetDisplaySize(buffer: CFTypeRef) -> CGSize {
    forward_foreign!(
        c"CVImageBufferGetDisplaySize",
        buffer,
        fn(CFTypeRef) -> CGSize,
        (buffer)
    );
    unsafe { CVImageBufferGetEncodedSize(buffer) }
}

/// CFTypeRef CVBufferRetain(CVBufferRef). Shares CoreFoundation ownership.
pub unsafe extern "C" fn CVBufferRetain(buffer: CFTypeRef) -> CFTypeRef {
    if buffer.is_null() {
        return buffer;
    }
    // SAFETY: a live CF-backed CVBuffer passed by the caller.
    unsafe { corefoundation::api::CFRetain(buffer) }
}

pub unsafe extern "C" fn CVBufferRelease(buffer: CFTypeRef) {
    if !buffer.is_null() {
        // SAFETY: a live CF-backed CVBuffer passed by the caller.
        unsafe { corefoundation::api::CFRelease(buffer) };
    }
}

pub unsafe extern "C" fn CVPixelBufferRetain(buffer: CFTypeRef) -> CFTypeRef {
    unsafe { CVBufferRetain(buffer) }
}

pub unsafe extern "C" fn CVPixelBufferRelease(buffer: CFTypeRef) {
    unsafe { CVBufferRelease(buffer) }
}

pub unsafe extern "C" fn CVPixelBufferGetTypeID() -> usize {
    corefoundation::base::type_id(class())
}

pub unsafe extern "C" fn CVPixelBufferGetWidth(buffer: CFTypeRef) -> usize {
    forward_foreign!(
        c"CVPixelBufferGetWidth",
        buffer,
        fn(CFTypeRef) -> usize,
        (buffer)
    );
    state(buffer).map_or(0, |s| s.width)
}

pub unsafe extern "C" fn CVPixelBufferGetHeight(buffer: CFTypeRef) -> usize {
    forward_foreign!(
        c"CVPixelBufferGetHeight",
        buffer,
        fn(CFTypeRef) -> usize,
        (buffer)
    );
    state(buffer).map_or(0, |s| s.height)
}

pub unsafe extern "C" fn CVPixelBufferGetPixelFormatType(buffer: CFTypeRef) -> u32 {
    forward_foreign!(
        c"CVPixelBufferGetPixelFormatType",
        buffer,
        fn(CFTypeRef) -> u32,
        (buffer)
    );
    state(buffer).map_or(0, |s| s.format)
}

pub unsafe extern "C" fn CVPixelBufferIsPlanar(buffer: CFTypeRef) -> u8 {
    forward_foreign!(
        c"CVPixelBufferIsPlanar",
        buffer,
        fn(CFTypeRef) -> u8,
        (buffer)
    );
    state(buffer).is_some_and(|s| s.planes.len() > 1) as u8
}

pub unsafe extern "C" fn CVPixelBufferGetPlaneCount(buffer: CFTypeRef) -> usize {
    forward_foreign!(
        c"CVPixelBufferGetPlaneCount",
        buffer,
        fn(CFTypeRef) -> usize,
        (buffer)
    );
    state(buffer).map_or(0, |s| {
        if s.planes.len() > 1 {
            s.planes.len()
        } else {
            0
        }
    })
}

pub unsafe extern "C" fn CVPixelBufferGetBytesPerRow(buffer: CFTypeRef) -> usize {
    forward_foreign!(
        c"CVPixelBufferGetBytesPerRow",
        buffer,
        fn(CFTypeRef) -> usize,
        (buffer)
    );
    state(buffer).map_or(0, |s| s.row_bytes)
}

pub unsafe extern "C" fn CVPixelBufferGetDataSize(buffer: CFTypeRef) -> usize {
    forward_foreign!(
        c"CVPixelBufferGetDataSize",
        buffer,
        fn(CFTypeRef) -> usize,
        (buffer)
    );
    state(buffer).map_or(0, |s| s.data_size)
}

pub unsafe extern "C" fn CVPixelBufferGetWidthOfPlane(buffer: CFTypeRef, i: usize) -> usize {
    forward_foreign!(
        c"CVPixelBufferGetWidthOfPlane",
        buffer,
        fn(CFTypeRef, usize) -> usize,
        (buffer, i)
    );
    state(buffer)
        .and_then(|s| s.planes.get(i))
        .map_or(0, |p| p.width)
}

pub unsafe extern "C" fn CVPixelBufferGetHeightOfPlane(buffer: CFTypeRef, i: usize) -> usize {
    forward_foreign!(
        c"CVPixelBufferGetHeightOfPlane",
        buffer,
        fn(CFTypeRef, usize) -> usize,
        (buffer, i)
    );
    state(buffer)
        .and_then(|s| s.planes.get(i))
        .map_or(0, |p| p.height)
}

pub unsafe extern "C" fn CVPixelBufferGetBytesPerRowOfPlane(buffer: CFTypeRef, i: usize) -> usize {
    forward_foreign!(
        c"CVPixelBufferGetBytesPerRowOfPlane",
        buffer,
        fn(CFTypeRef, usize) -> usize,
        (buffer, i)
    );
    state(buffer)
        .and_then(|s| s.planes.get(i))
        .map_or(0, |p| p.row_bytes)
}

fn base_address(buffer: CFTypeRef, plane: Option<usize>) -> *mut c_void {
    let Some(state) = state(buffer) else {
        return std::ptr::null_mut();
    };
    // Storage itself is stable, but CoreVideo only exposes an address while
    // at least one base-address lock is outstanding.
    if state.locks.load(Ordering::Acquire) == 0 {
        return std::ptr::null_mut();
    }
    match plane {
        None => state.memory.base().cast(),
        Some(i) => state
            .planes
            .get(i)
            .map_or(std::ptr::null_mut(), |p| p.address.cast()),
    }
}

pub unsafe extern "C" fn CVPixelBufferGetBaseAddress(buffer: CFTypeRef) -> *mut c_void {
    forward_foreign!(
        c"CVPixelBufferGetBaseAddress",
        buffer,
        fn(CFTypeRef) -> *mut c_void,
        (buffer)
    );
    base_address(buffer, None)
}

pub unsafe extern "C" fn CVPixelBufferGetBaseAddressOfPlane(
    buffer: CFTypeRef,
    i: usize,
) -> *mut c_void {
    forward_foreign!(
        c"CVPixelBufferGetBaseAddressOfPlane",
        buffer,
        fn(CFTypeRef, usize) -> *mut c_void,
        (buffer, i)
    );
    base_address(buffer, Some(i))
}

/// Only bit 0 is a documented mode; native CoreVideo ignores other bits.
/// Unlocks are counted regardless of whether their flags match the lock.
pub unsafe extern "C" fn CVPixelBufferLockBaseAddress(buffer: CFTypeRef, flags: u64) -> i32 {
    forward_foreign!(
        c"CVPixelBufferLockBaseAddress",
        buffer,
        fn(CFTypeRef, u64) -> i32,
        (buffer, flags)
    );
    let Some(state) = state(buffer) else {
        return INVALID_ARGUMENT;
    };
    // An overflowing count is refused rather than wrapped back to unlocked.
    match state
        .locks
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
    {
        Ok(_) => SUCCESS,
        Err(_) => ERROR,
    }
}

pub unsafe extern "C" fn CVPixelBufferUnlockBaseAddress(buffer: CFTypeRef, flags: u64) -> i32 {
    forward_foreign!(
        c"CVPixelBufferUnlockBaseAddress",
        buffer,
        fn(CFTypeRef, u64) -> i32,
        (buffer, flags)
    );
    let Some(state) = state(buffer) else {
        return INVALID_ARGUMENT;
    };
    match state
        .locks
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
    {
        Ok(_) => SUCCESS,
        Err(_) => ERROR,
    }
}

/// A CPU-allocated CVPixelBuffer has no IOSurface backing.
///
/// FIXME (ACCURACY): a buffer created with kCVPixelBufferIOSurfacePropertiesKey
/// is IOSurface-backed on macOS; here it is CPU memory and this answers NULL,
/// which callers must already handle for buffers made without that key.
pub unsafe extern "C" fn CVPixelBufferGetIOSurface(buffer: CFTypeRef) -> CFTypeRef {
    forward_foreign!(
        c"CVPixelBufferGetIOSurface",
        buffer,
        fn(CFTypeRef) -> CFTypeRef,
        (buffer)
    );
    std::ptr::null()
}

/// A copy of this runtime's `buffer` made by the host CoreVideo, for a host
/// framework that reads pixel buffers itself: VideoToolbox's encoder, on a
/// Mac with the builtin frameworks. Retained; `None` when `buffer` is not
/// this runtime's or the host cannot make one. Attachments stay behind, as
/// their values are this runtime's objects.
#[cfg(target_os = "macos")]
pub fn host_copy(buffer: CFTypeRef) -> Option<CFTypeRef> {
    type Create =
        unsafe extern "C" fn(CFTypeRef, usize, usize, u32, CFTypeRef, *mut CFTypeRef) -> i32;
    type Lock = unsafe extern "C" fn(CFTypeRef, u64) -> i32;
    type Base = unsafe extern "C" fn(CFTypeRef) -> *mut u8;
    type Rows = unsafe extern "C" fn(CFTypeRef) -> usize;
    type PlaneBase = unsafe extern "C" fn(CFTypeRef, usize) -> *mut u8;
    type PlaneRows = unsafe extern "C" fn(CFTypeRef, usize) -> usize;
    // SAFETY: null or an object, whose first word is readable.
    if unsafe { corefoundation::foreign::is(buffer) } {
        return None;
    }
    let state = state(buffer)?;
    let entry = corefoundation::foreign::entry;
    // SAFETY: CoreVideo's own declarations of these entry points.
    let (create, lock, unlock, base, rows, plane_base, plane_rows) = unsafe {
        (
            std::mem::transmute::<*mut c_void, Create>(entry(c"CVPixelBufferCreate")?),
            std::mem::transmute::<*mut c_void, Lock>(entry(c"CVPixelBufferLockBaseAddress")?),
            std::mem::transmute::<*mut c_void, Lock>(entry(c"CVPixelBufferUnlockBaseAddress")?),
            std::mem::transmute::<*mut c_void, Base>(entry(c"CVPixelBufferGetBaseAddress")?),
            std::mem::transmute::<*mut c_void, Rows>(entry(c"CVPixelBufferGetBytesPerRow")?),
            std::mem::transmute::<*mut c_void, PlaneBase>(entry(
                c"CVPixelBufferGetBaseAddressOfPlane",
            )?),
            std::mem::transmute::<*mut c_void, PlaneRows>(entry(
                c"CVPixelBufferGetBytesPerRowOfPlane",
            )?),
        )
    };
    let planar = state.planes.len() > 1;
    let mut copy = std::ptr::null();
    // SAFETY: the host's functions, called on the buffer the host just made
    // with this one's size and format; each row copied is no longer than
    // either buffer's stride, and both hold `plane.height` of them.
    unsafe {
        if create(
            std::ptr::null(),
            state.width,
            state.height,
            state.format,
            std::ptr::null(),
            &mut copy,
        ) != SUCCESS
        {
            return None;
        }
        if lock(copy, 0) != SUCCESS {
            corefoundation::api::CFRelease(copy);
            return None;
        }
        for (i, plane) in state.planes.iter().enumerate() {
            let (to, to_rows) = match planar {
                true => (plane_base(copy, i), plane_rows(copy, i)),
                false => (base(copy), rows(copy)),
            };
            let row = plane.row_bytes.min(to_rows);
            for y in 0..plane.height {
                std::ptr::copy_nonoverlapping(
                    plane.address.add(y * plane.row_bytes),
                    to.add(y * to_rows),
                    row,
                );
            }
        }
        unlock(copy, 0);
    }
    Some(copy)
}

/// Off Darwin there is no host CoreVideo.
#[cfg(not(target_os = "macos"))]
pub fn host_copy(_buffer: CFTypeRef) -> Option<CFTypeRef> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create(w: usize, h: usize, format: u32) -> CFTypeRef {
        let mut out = std::ptr::null();
        assert_eq!(
            unsafe {
                CVPixelBufferCreate(std::ptr::null(), w, h, format, std::ptr::null(), &mut out)
            },
            SUCCESS
        );
        assert!(!out.is_null());
        out
    }

    #[test]
    fn bgra_stride_lock_and_cf_ownership() {
        let b = create(7, 5, BGRA);
        unsafe {
            assert_eq!(CVPixelBufferGetWidth(b), 7);
            assert_eq!(CVPixelBufferGetHeight(b), 5);
            assert_eq!(CVPixelBufferGetPixelFormatType(b), BGRA);
            assert_eq!(CVPixelBufferGetBytesPerRow(b), 64);
            assert_eq!(CVPixelBufferGetDataSize(b), 320);
            assert_eq!(CVPixelBufferGetPlaneCount(b), 0);
            assert_eq!(CVPixelBufferIsPlanar(b), 0);
            assert_eq!(CVPixelBufferGetWidthOfPlane(b, 0), 7);
            assert_eq!(CVPixelBufferGetHeightOfPlane(b, 0), 5);
            assert_eq!(CVPixelBufferGetBytesPerRowOfPlane(b, 0), 64);
            assert!(CVPixelBufferGetBaseAddress(b).is_null());
            assert!(CVPixelBufferGetBaseAddressOfPlane(b, 0).is_null());
            assert_eq!(CVPixelBufferGetWidthOfPlane(b, 1), 0);
            assert_eq!(CVPixelBufferGetHeightOfPlane(b, 1), 0);
            assert_eq!(CVPixelBufferGetBytesPerRowOfPlane(b, 1), 0);
            assert_eq!(
                corefoundation::api::CFGetTypeID(b),
                CVPixelBufferGetTypeID()
            );
            assert_eq!(CVPixelBufferLockBaseAddress(b, 0), SUCCESS);
            let ptr = CVPixelBufferGetBaseAddress(b).cast::<u8>();
            assert!(!ptr.is_null());
            assert_eq!(CVPixelBufferGetBaseAddressOfPlane(b, 0), ptr.cast());
            assert!(CVPixelBufferGetBaseAddressOfPlane(b, 1).is_null());
            *ptr = 37;
            assert_eq!(CVPixelBufferUnlockBaseAddress(b, 1), SUCCESS);
            assert!(CVPixelBufferGetBaseAddress(b).is_null());
            assert!(CVPixelBufferGetBaseAddressOfPlane(b, 0).is_null());
            assert_eq!(CVPixelBufferUnlockBaseAddress(b, 0), ERROR);
            assert_eq!(CVPixelBufferUnlockBaseAddress(b, 0), ERROR);
            assert_eq!(CVPixelBufferLockBaseAddress(b, 2), SUCCESS);
            assert_eq!(CVPixelBufferUnlockBaseAddress(b, 2), SUCCESS);
            assert_eq!(CVPixelBufferUnlockBaseAddress(b, 2), ERROR);
            let retained = CVBufferRetain(b);
            assert_eq!(retained, b);
            CVPixelBufferRelease(b);
            assert_eq!(CVPixelBufferGetWidth(retained), 7);
            CVBufferRelease(retained);
        }
    }

    #[test]
    fn plane_zero_and_lock_visibility_match_native_corevideo() {
        for (format, count) in [
            (BGRA, 0),
            (NV12_VIDEO, 2),
            (NV12_FULL, 2),
            (I420_VIDEO, 3),
            (I420_FULL, 3),
        ] {
            let b = create(7, 5, format);
            unsafe {
                // Packed BGRA reports zero planes but still exposes index 0.
                assert_eq!(CVPixelBufferGetPlaneCount(b), count);
                assert_eq!(CVPixelBufferGetWidthOfPlane(b, 0), 7);
                assert_eq!(CVPixelBufferGetHeightOfPlane(b, 0), 5);
                assert_eq!(CVPixelBufferGetBytesPerRowOfPlane(b, 0), 64);
                let missing = if count == 0 { 1 } else { count };
                assert_eq!(CVPixelBufferGetWidthOfPlane(b, missing), 0);
                assert_eq!(CVPixelBufferGetHeightOfPlane(b, missing), 0);
                assert_eq!(CVPixelBufferGetBytesPerRowOfPlane(b, missing), 0);
                assert!(CVPixelBufferGetBaseAddress(b).is_null());
                assert!(CVPixelBufferGetBaseAddressOfPlane(b, 0).is_null());
                assert!(CVPixelBufferGetBaseAddressOfPlane(b, missing).is_null());
                assert_eq!(CVPixelBufferLockBaseAddress(b, 0), SUCCESS);
                let base = CVPixelBufferGetBaseAddress(b);
                let plane = CVPixelBufferGetBaseAddressOfPlane(b, 0);
                assert!(!base.is_null());
                assert!(!plane.is_null());
                if count == 0 {
                    assert_eq!(base, plane);
                }
                assert!(CVPixelBufferGetBaseAddressOfPlane(b, missing).is_null());
                assert_eq!(CVPixelBufferLockBaseAddress(b, READ_ONLY), SUCCESS);
                assert_eq!(CVPixelBufferGetBaseAddress(b), base);
                assert_eq!(CVPixelBufferGetBaseAddressOfPlane(b, 0), plane);
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, READ_ONLY), SUCCESS);
                assert_eq!(CVPixelBufferGetBaseAddress(b), base);
                assert_eq!(CVPixelBufferGetBaseAddressOfPlane(b, 0), plane);
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, 0), SUCCESS);
                assert!(CVPixelBufferGetBaseAddress(b).is_null());
                assert!(CVPixelBufferGetBaseAddressOfPlane(b, 0).is_null());
                // Lock count does not hide metadata or permanently invalidate storage.
                assert_eq!(CVPixelBufferGetWidthOfPlane(b, 0), 7);
                assert_eq!(CVPixelBufferLockBaseAddress(b, READ_ONLY), SUCCESS);
                assert_eq!(CVPixelBufferGetBaseAddress(b), base);
                assert_eq!(CVPixelBufferGetBaseAddressOfPlane(b, 0), plane);
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, READ_ONLY), SUCCESS);
                assert!(CVPixelBufferGetBaseAddressOfPlane(b, 0).is_null());
                CVPixelBufferRelease(b);
            }
        }
    }

    #[test]
    fn planar_offsets_and_dimensions_are_consistent() {
        for (fmt, expected) in [
            (NV12_VIDEO, 2),
            (NV12_FULL, 2),
            (I420_VIDEO, 3),
            (I420_FULL, 3),
        ] {
            let b = create(7, 5, fmt);
            unsafe {
                assert_eq!(CVPixelBufferIsPlanar(b), 1);
                assert_eq!(CVPixelBufferGetPlaneCount(b), expected);
                assert_eq!(CVPixelBufferLockBaseAddress(b, READ_ONLY), SUCCESS);
                let base = CVPixelBufferGetBaseAddress(b).cast::<u8>();
                assert!(!base.is_null());
                for i in 0..expected {
                    let plane = CVPixelBufferGetBaseAddressOfPlane(b, i);
                    assert!(!plane.is_null());
                    let offset = u32::from_be_bytes(
                        std::slice::from_raw_parts(base.add(i * 8), 4)
                            .try_into()
                            .unwrap(),
                    ) as usize;
                    let stride = u32::from_be_bytes(
                        std::slice::from_raw_parts(base.add(i * 8 + 4), 4)
                            .try_into()
                            .unwrap(),
                    ) as usize;
                    assert_eq!(offset, plane as usize - base as usize);
                    assert_eq!(stride, CVPixelBufferGetBytesPerRowOfPlane(b, i));
                    assert_eq!(
                        CVPixelBufferGetWidthOfPlane(b, i),
                        if i == 0 { 7 } else { 4 }
                    );
                    assert_eq!(
                        CVPixelBufferGetHeightOfPlane(b, i),
                        if i == 0 { 5 } else { 3 }
                    );
                    assert!(
                        offset + stride * CVPixelBufferGetHeightOfPlane(b, i)
                            <= CVPixelBufferGetDataSize(b)
                    );
                }
                assert_eq!(
                    CVPixelBufferGetBaseAddressOfPlane(b, expected),
                    std::ptr::null_mut()
                );
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, 0), SUCCESS);
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, READ_ONLY), ERROR);
                CVPixelBufferRelease(b);
            }
        }
    }

    #[test]
    fn planar_stride_envelope_is_entirely_backed_and_bounded() {
        // A tall, narrow buffer can have fewer plane bytes than its rounded
        // buffer-wide row stride implies. Test the entire public envelope.
        for format in [NV12_VIDEO, NV12_FULL, I420_VIDEO, I420_FULL] {
            for (width, height) in [(7, 1), (7, 5), (7, 1000), (7, 1001), (1, 32_768)] {
                let b = create(width, height, format);
                unsafe {
                    let row_bytes = CVPixelBufferGetBytesPerRow(b);
                    let envelope = row_bytes.checked_mul(height).unwrap();
                    let data_size = CVPixelBufferGetDataSize(b);
                    assert_eq!(data_size, envelope, "format={format:#x} {width}x{height}");
                    assert!(CVPixelBufferGetBaseAddress(b).is_null());
                    assert_eq!(CVPixelBufferLockBaseAddress(b, 0), SUCCESS);
                    let ptr = CVPixelBufferGetBaseAddress(b).cast::<u8>();
                    assert!(!ptr.is_null());
                    // The final advertised byte must be writable and owned by
                    // the pixel buffer, not the heap's alignment overrun.
                    let last = ptr.add(envelope - 1);
                    *last = 0xA5;
                    assert_eq!(*last, 0xA5);
                    for i in 0..CVPixelBufferGetPlaneCount(b) {
                        let plane = CVPixelBufferGetBaseAddressOfPlane(b, i).cast::<u8>();
                        let start = plane.offset_from(ptr) as usize;
                        let plane_size = CVPixelBufferGetBytesPerRowOfPlane(b, i)
                            .checked_mul(CVPixelBufferGetHeightOfPlane(b, i))
                            .unwrap();
                        assert!(start.checked_add(plane_size).unwrap() <= data_size);
                    }
                    assert_eq!(CVPixelBufferUnlockBaseAddress(b, 0), SUCCESS);
                    CVPixelBufferRelease(b);
                }
            }
        }
        // The advertised envelope, not just the sum of plane storage, is
        // subject to the existing allocation limit before allocating memory.
        for format in [NV12_VIDEO, I420_VIDEO] {
            assert!(matches!(
                layout(1, MAX_STORAGE / DEFAULT_ALIGNMENT, format, std::ptr::null()),
                Err(ALLOCATION_FAILED)
            ));
        }
    }

    #[test]
    fn unbalanced_and_nested_unlocks_match_native_corevideo() {
        for format in [BGRA, NV12_VIDEO, I420_VIDEO] {
            let b = create(7, 5, format);
            unsafe {
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, 0), ERROR);
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, READ_ONLY), ERROR);
                assert_eq!(CVPixelBufferLockBaseAddress(b, 0), SUCCESS);
                assert_eq!(CVPixelBufferLockBaseAddress(b, 0), SUCCESS);
                // Native CoreVideo counts outstanding locks regardless of
                // the read-only flag given to an individual unlock.
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, READ_ONLY), SUCCESS);
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, 0), SUCCESS);
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, 0), ERROR);
                assert_eq!(CVPixelBufferLockBaseAddress(b, READ_ONLY), SUCCESS);
                assert_eq!(CVPixelBufferLockBaseAddress(b, READ_ONLY), SUCCESS);
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, 0), SUCCESS);
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, READ_ONLY), SUCCESS);
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, READ_ONLY), ERROR);
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, 2), ERROR);
                assert_eq!(
                    CVPixelBufferUnlockBaseAddress(std::ptr::null(), 0),
                    INVALID_ARGUMENT
                );
                CVPixelBufferRelease(b);
            }
        }
    }

    #[test]
    fn creation_refuses_invalid_sizes_formats_and_requests() {
        unsafe {
            let mut out = 1usize as CFTypeRef;
            assert_eq!(
                CVPixelBufferCreate(std::ptr::null(), 0, 5, BGRA, std::ptr::null(), &mut out),
                INVALID_SIZE
            );
            assert!(out.is_null());
            assert_eq!(
                CVPixelBufferCreate(
                    std::ptr::null(),
                    usize::MAX,
                    1,
                    BGRA,
                    std::ptr::null(),
                    &mut out
                ),
                INVALID_SIZE
            );
            assert_eq!(
                CVPixelBufferCreate(
                    std::ptr::null(),
                    2,
                    2,
                    0x12345678,
                    std::ptr::null(),
                    &mut out
                ),
                INVALID_PIXEL_FORMAT
            );
            assert_eq!(
                CVPixelBufferCreate(
                    std::ptr::null(),
                    2,
                    2,
                    BGRA,
                    std::ptr::null(),
                    std::ptr::null_mut()
                ),
                INVALID_ARGUMENT
            );
            assert_eq!(
                CVPixelBufferLockBaseAddress(std::ptr::null(), 0),
                INVALID_ARGUMENT
            );
        }
    }

    #[test]
    fn requested_alignments_and_gpu_backing_requests() {
        use corefoundation::callbacks::Policy;
        use corefoundation::collections::number_type;

        fn attrs(name: &str, value: CFTypeRef) -> CFTypeRef {
            let policy = Policy::from_type_callbacks();
            corefoundation::collections::new_dictionary(
                &[(key(name), value)],
                policy,
                policy,
                false,
            )
        }

        unsafe {
            let alignment: i64 = 128;
            let number = corefoundation::api::CFNumberCreate(
                std::ptr::null(),
                number_type::SINT64,
                (&alignment as *const i64).cast(),
            );
            let dictionary = attrs("BytesPerRowAlignment", number);
            let mut out = std::ptr::null();
            assert_eq!(
                CVPixelBufferCreate(std::ptr::null(), 7, 5, BGRA, dictionary, &mut out),
                SUCCESS
            );
            assert_eq!(CVPixelBufferGetBytesPerRow(out), 128);
            assert_eq!(CVPixelBufferLockBaseAddress(out, 0), SUCCESS);
            assert_eq!((CVPixelBufferGetBaseAddress(out) as usize) % 128, 0);
            assert!(!CVPixelBufferGetBaseAddress(out).is_null());
            assert_eq!(CVPixelBufferUnlockBaseAddress(out, 0), SUCCESS);
            CVPixelBufferRelease(out);
            corefoundation::api::CFRelease(dictionary);
            corefoundation::api::CFRelease(number);

            let alignment: i64 = 256;
            let number = corefoundation::api::CFNumberCreate(
                std::ptr::null(),
                number_type::SINT64,
                (&alignment as *const i64).cast(),
            );
            let dictionary = attrs("PlaneAlignment", number);
            assert_eq!(
                CVPixelBufferCreate(std::ptr::null(), 7, 5, NV12_VIDEO, dictionary, &mut out),
                SUCCESS
            );
            assert_eq!(CVPixelBufferLockBaseAddress(out, 0), SUCCESS);
            for i in 0..2 {
                assert!(!CVPixelBufferGetBaseAddressOfPlane(out, i).is_null());
                assert_eq!(
                    (CVPixelBufferGetBaseAddressOfPlane(out, i) as usize) % 256,
                    0
                );
            }
            assert_eq!(CVPixelBufferUnlockBaseAddress(out, 0), SUCCESS);
            CVPixelBufferRelease(out);
            corefoundation::api::CFRelease(dictionary);
            corefoundation::api::CFRelease(number);

            let dictionary = attrs(
                "OpenGLCompatibility",
                corefoundation::collections::boolean(true),
            );
            assert_eq!(
                CVPixelBufferCreate(std::ptr::null(), 7, 5, BGRA, dictionary, &mut out),
                SUCCESS
            );
            CVPixelBufferRelease(out);
            corefoundation::api::CFRelease(dictionary);

            let empty = corefoundation::collections::new_dictionary(
                &[],
                Policy::from_type_callbacks(),
                Policy::from_type_callbacks(),
                false,
            );
            // The usual way to ask for a Metal- or GL-shareable buffer.
            let dictionary = attrs("IOSurfaceProperties", empty);
            for format in [BGRA, NV12_VIDEO, I420_VIDEO] {
                assert_eq!(
                    CVPixelBufferCreate(std::ptr::null(), 7, 5, format, dictionary, &mut out),
                    SUCCESS
                );
                CVPixelBufferRelease(out);
            }
            corefoundation::api::CFRelease(dictionary);
            corefoundation::api::CFRelease(empty);

            let bogus_attrs_number: i64 = 8;
            let bogus_attrs = corefoundation::api::CFNumberCreate(
                std::ptr::null(),
                number_type::SINT64,
                (&bogus_attrs_number as *const i64).cast(),
            );
            assert_eq!(
                CVPixelBufferCreate(std::ptr::null(), 7, 5, BGRA, bogus_attrs, &mut out),
                INVALID_ATTRIBUTES
            );
            corefoundation::api::CFRelease(bogus_attrs);
        }
    }

    #[test]
    fn narrow_large_dimensions_are_not_artificially_rejected() {
        unsafe {
            for (width, height) in [
                (16_384, 1),
                (16_385, 1),
                (20_000, 1),
                (1, 20_000),
                (1, 32_768),
            ] {
                let mut out = std::ptr::null();
                assert_eq!(
                    CVPixelBufferCreate(
                        std::ptr::null(),
                        width,
                        height,
                        BGRA,
                        std::ptr::null(),
                        &mut out
                    ),
                    SUCCESS,
                    "{width}x{height}"
                );
                assert_eq!(CVPixelBufferGetWidth(out), width);
                assert_eq!(CVPixelBufferGetHeight(out), height);
                CVPixelBufferRelease(out);
            }
            let mut out = std::ptr::null();
            assert_eq!(
                CVPixelBufferCreate(
                    std::ptr::null(),
                    0,
                    20_000,
                    BGRA,
                    std::ptr::null(),
                    &mut out
                ),
                INVALID_SIZE
            );
            assert!(out.is_null());
        }
    }

    #[test]
    fn standard_malloc_allocators_and_software_graphics_flags_are_accepted() {
        use corefoundation::callbacks::Policy;
        let policy = Policy::from_type_callbacks();
        unsafe {
            for kind in [
                corefoundation::api::StandardAllocator::SystemDefault,
                corefoundation::api::StandardAllocator::Malloc,
                corefoundation::api::StandardAllocator::MallocZone,
            ] {
                let allocator = corefoundation::api::standard_allocator(kind);
                assert!(!allocator.is_null());
                let mut out = std::ptr::null();
                assert_eq!(
                    CVPixelBufferCreate(allocator, 7, 5, BGRA, std::ptr::null(), &mut out),
                    SUCCESS
                );
                CVPixelBufferRelease(out);
                let attrs = corefoundation::collections::new_dictionary(
                    &[(key("MemoryAllocator"), allocator.cast())],
                    policy,
                    policy,
                    false,
                );
                assert_eq!(
                    CVPixelBufferCreate(std::ptr::null(), 7, 5, BGRA, attrs, &mut out),
                    SUCCESS
                );
                CVPixelBufferRelease(out);
                corefoundation::api::CFRelease(attrs);
            }
            let mut out = std::ptr::null();
            assert_eq!(
                CVPixelBufferCreate(
                    corefoundation::api::standard_allocator(
                        corefoundation::api::StandardAllocator::Null
                    ),
                    7,
                    5,
                    BGRA,
                    std::ptr::null(),
                    &mut out
                ),
                UNSUPPORTED
            );
            assert!(out.is_null());
            let invalid_number: i64 = 1;
            let wrong_type = corefoundation::api::CFNumberCreate(
                std::ptr::null(),
                corefoundation::collections::number_type::SINT64,
                (&invalid_number as *const i64).cast(),
            );
            for name in ["CGBitmapContextCompatibility", "CGImageCompatibility"] {
                let non_boolean = corefoundation::collections::new_dictionary(
                    &[(key(name), wrong_type)],
                    policy,
                    policy,
                    false,
                );
                assert_eq!(
                    CVPixelBufferCreate(std::ptr::null(), 7, 5, BGRA, non_boolean, &mut out),
                    SUCCESS,
                    "{name} numeric hint"
                );
                CVPixelBufferRelease(out);
                corefoundation::api::CFRelease(non_boolean);
            }
            corefoundation::api::CFRelease(wrong_type);
            for key_name in ["CGBitmapContextCompatibility", "CGImageCompatibility"] {
                let attrs = corefoundation::collections::new_dictionary(
                    &[(key(key_name), corefoundation::collections::boolean(true))],
                    policy,
                    policy,
                    false,
                );
                for format in [BGRA, NV12_VIDEO, NV12_FULL, I420_VIDEO, I420_FULL] {
                    assert_eq!(
                        CVPixelBufferCreate(std::ptr::null(), 7, 5, format, attrs, &mut out),
                        SUCCESS,
                        "{key_name} {format:#x}"
                    );
                    CVPixelBufferRelease(out);
                }
                corefoundation::api::CFRelease(attrs);
            }
        }
    }

    #[test]
    fn extended_pixel_number_conversions_are_native_compatible() {
        use corefoundation::callbacks::Policy;
        use corefoundation::collections::number_type;
        let policy = Policy::from_type_callbacks();
        unsafe {
            for name in [
                "ExtendedPixelsLeft",
                "ExtendedPixelsRight",
                "ExtendedPixelsTop",
                "ExtendedPixelsBottom",
            ] {
                for value in [
                    0.0_f64,
                    0.3,
                    0.9,
                    -0.9,
                    f64::NAN,
                    -1.0,
                    -1.2,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    1.0,
                    1.2,
                ] {
                    let number = corefoundation::api::CFNumberCreate(
                        std::ptr::null(),
                        number_type::FLOAT64,
                        (&value as *const f64).cast(),
                    );
                    let attrs = corefoundation::collections::new_dictionary(
                        &[(key(name), number)],
                        policy,
                        policy,
                        false,
                    );
                    let expected = if value.is_infinite() || value <= -1.0 {
                        ALLOCATION_FAILED
                    } else if value >= 1.0 {
                        // Nonzero extended pixels require storage this CPU-only
                        // implementation intentionally does not provide.
                        UNSUPPORTED
                    } else {
                        SUCCESS
                    };
                    for format in [BGRA, NV12_VIDEO, I420_VIDEO] {
                        let mut out = 1usize as CFTypeRef;
                        assert_eq!(
                            CVPixelBufferCreate(std::ptr::null(), 7, 5, format, attrs, &mut out),
                            expected,
                            "{name}={value} format={format:#x}"
                        );
                        if expected == SUCCESS {
                            CVPixelBufferRelease(out);
                        } else {
                            assert!(out.is_null());
                        }
                    }
                    corefoundation::api::CFRelease(attrs);
                    corefoundation::api::CFRelease(number);
                }
                for value in [
                    key("extended-value-string"),
                    corefoundation::collections::boolean(true),
                ] {
                    let attrs = corefoundation::collections::new_dictionary(
                        &[(key(name), value)],
                        policy,
                        policy,
                        false,
                    );
                    let mut out = std::ptr::null();
                    assert_eq!(
                        CVPixelBufferCreate(std::ptr::null(), 7, 5, BGRA, attrs, &mut out),
                        SUCCESS,
                        "{name} non-CFNumber value"
                    );
                    CVPixelBufferRelease(out);
                    corefoundation::api::CFRelease(attrs);
                }
            }
        }
    }

    #[test]
    fn compatibility_value_types_are_hints_not_malformed_attributes() {
        use corefoundation::callbacks::Policy;
        use corefoundation::collections::number_type;
        let policy = Policy::from_type_callbacks();
        unsafe {
            let one: i64 = 1;
            let number = corefoundation::api::CFNumberCreate(
                std::ptr::null(),
                number_type::SINT64,
                (&one as *const i64).cast(),
            );
            for name in [
                "CGBitmapContextCompatibility",
                "CGImageCompatibility",
                "OpenGLCompatibility",
                "MetalCompatibility",
                "IOSurfaceCoreAnimationCompatibility",
            ] {
                for value in [key("compatibility-string"), number] {
                    let attrs = corefoundation::collections::new_dictionary(
                        &[(key(name), value)],
                        policy,
                        policy,
                        false,
                    );
                    for format in [BGRA, NV12_VIDEO, I420_VIDEO] {
                        let mut out = std::ptr::null();
                        assert_eq!(
                            CVPixelBufferCreate(std::ptr::null(), 7, 5, format, attrs, &mut out),
                            SUCCESS,
                            "{name} value={value:p} format={format:#x}"
                        );
                        CVPixelBufferRelease(out);
                    }
                    corefoundation::api::CFRelease(attrs);
                }
                if !matches!(
                    name,
                    "CGBitmapContextCompatibility" | "CGImageCompatibility"
                ) {
                    let required = corefoundation::collections::new_dictionary(
                        &[(key(name), corefoundation::collections::boolean(true))],
                        policy,
                        policy,
                        false,
                    );
                    let mut out = std::ptr::null();
                    assert_eq!(
                        CVPixelBufferCreate(std::ptr::null(), 7, 5, BGRA, required, &mut out),
                        SUCCESS,
                        "{name} genuine GPU request"
                    );
                    CVPixelBufferRelease(out);
                    corefoundation::api::CFRelease(required);
                }
            }
            corefoundation::api::CFRelease(number);
        }
    }

    #[test]
    fn requested_plane_alignments_preserve_the_64_byte_cpu_minimum() {
        use corefoundation::callbacks::Policy;
        use corefoundation::collections::number_type;
        let policy = Policy::from_type_callbacks();
        unsafe {
            for format in [NV12_VIDEO, NV12_FULL, I420_VIDEO, I420_FULL] {
                for multiple in [0_i64, 1, 3, 24, 65, 4097] {
                    let number = corefoundation::api::CFNumberCreate(
                        std::ptr::null(),
                        number_type::SINT64,
                        (&multiple as *const i64).cast(),
                    );
                    let attrs = corefoundation::collections::new_dictionary(
                        &[(key("PlaneAlignment"), number)],
                        policy,
                        policy,
                        false,
                    );
                    let b = {
                        let mut b = std::ptr::null();
                        assert_eq!(
                            CVPixelBufferCreate(std::ptr::null(), 7, 5, format, attrs, &mut b),
                            SUCCESS,
                            "format={format:#x} alignment={multiple}"
                        );
                        b
                    };
                    assert_eq!(CVPixelBufferLockBaseAddress(b, 0), SUCCESS);
                    let base = CVPixelBufferGetBaseAddress(b) as usize;
                    let size = CVPixelBufferGetDataSize(b);
                    assert_eq!(base % DEFAULT_ALIGNMENT, 0);
                    for i in 0..CVPixelBufferGetPlaneCount(b) {
                        let address = CVPixelBufferGetBaseAddressOfPlane(b, i) as usize;
                        assert_ne!(address, 0);
                        assert_eq!(address % DEFAULT_ALIGNMENT, 0);
                        if multiple > 0 {
                            assert_eq!(address % multiple as usize, 0);
                        }
                        let offset = address - base;
                        let len = CVPixelBufferGetBytesPerRowOfPlane(b, i)
                            .checked_mul(CVPixelBufferGetHeightOfPlane(b, i))
                            .unwrap();
                        assert!(offset.checked_add(len).unwrap() <= size);
                    }
                    assert!(CVPixelBufferGetBytesPerRow(b).checked_mul(5).unwrap() <= size);
                    assert_eq!(CVPixelBufferUnlockBaseAddress(b, 0), SUCCESS);
                    CVPixelBufferRelease(b);
                    corefoundation::api::CFRelease(attrs);
                    corefoundation::api::CFRelease(number);
                }
            }
        }
    }

    #[test]
    fn unknown_lock_flag_bits_are_ignored_without_unbalancing_locks() {
        let b = create(7, 5, BGRA);
        unsafe {
            for flags in [0, 1, 2, 3, 4, 7, 8, 16, u64::MAX] {
                assert_eq!(CVPixelBufferLockBaseAddress(b, flags), SUCCESS);
                assert!(!CVPixelBufferGetBaseAddress(b).is_null());
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, flags), SUCCESS);
                assert!(CVPixelBufferGetBaseAddress(b).is_null());
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, flags), ERROR);
            }
            assert_eq!(CVPixelBufferLockBaseAddress(b, 2), SUCCESS);
            assert_eq!(CVPixelBufferLockBaseAddress(b, 8), SUCCESS);
            assert_eq!(CVPixelBufferUnlockBaseAddress(b, 3), SUCCESS);
            assert!(!CVPixelBufferGetBaseAddress(b).is_null());
            assert_eq!(CVPixelBufferUnlockBaseAddress(b, u64::MAX), SUCCESS);
            assert!(CVPixelBufferGetBaseAddress(b).is_null());
            CVPixelBufferRelease(b);
        }
    }

    #[test]
    fn malformed_attributes_and_allocation_limit_fail_without_publishing() {
        use corefoundation::callbacks::Policy;
        use corefoundation::collections::number_type;

        unsafe {
            let mut out = 1usize as CFTypeRef;
            // The dimensions are legal but the allocation exceeds the CPU cap.
            assert_eq!(
                CVPixelBufferCreate(
                    std::ptr::null(),
                    20_000,
                    20_000,
                    BGRA,
                    std::ptr::null(),
                    &mut out,
                ),
                ALLOCATION_FAILED
            );
            assert!(out.is_null());

            let policy = Policy::from_type_callbacks();
            for (alignment, expected) in [(0_i64, SUCCESS), (-1, ALLOCATION_FAILED)] {
                let number = corefoundation::api::CFNumberCreate(
                    std::ptr::null(),
                    number_type::SINT64,
                    (&alignment as *const i64).cast(),
                );
                let attrs = corefoundation::collections::new_dictionary(
                    &[(key("BytesPerRowAlignment"), number)],
                    policy,
                    policy,
                    false,
                );
                out = 1usize as CFTypeRef;
                assert_eq!(
                    CVPixelBufferCreate(std::ptr::null(), 4, 4, BGRA, attrs, &mut out),
                    expected,
                    "alignment={alignment}"
                );
                if expected == SUCCESS {
                    assert_eq!(CVPixelBufferGetBytesPerRow(out), DEFAULT_ALIGNMENT);
                    CVPixelBufferRelease(out);
                } else {
                    assert!(out.is_null());
                }
                corefoundation::api::CFRelease(attrs);
                corefoundation::api::CFRelease(number);
            }

            let not_dictionary = corefoundation::api::CFNumberCreate(
                std::ptr::null(),
                number_type::SINT64,
                (&1_i64 as *const i64).cast(),
            );
            let attrs = corefoundation::collections::new_dictionary(
                &[(key("IOSurfaceProperties"), not_dictionary)],
                policy,
                policy,
                false,
            );
            assert_eq!(
                CVPixelBufferCreate(std::ptr::null(), 4, 4, BGRA, attrs, &mut out),
                INVALID_ATTRIBUTES
            );
            assert!(out.is_null());
            corefoundation::api::CFRelease(attrs);
            corefoundation::api::CFRelease(not_dictionary);
        }
    }

    #[test]
    fn gpu_requests_get_cpu_backing_and_custom_allocators_are_refused() {
        use corefoundation::callbacks::Policy;
        use corefoundation::collections::number_type;
        let policy = Policy::from_type_callbacks();
        let mut out = std::ptr::null();
        let true_value = corefoundation::collections::boolean(true);
        let false_value = corefoundation::collections::boolean(false);
        unsafe {
            for name in [
                "OpenGLCompatibility",
                "OpenGLESCompatibility",
                "MetalCompatibility",
                "OpenGLTextureCacheCompatibility",
                "OpenGLESTextureCacheCompatibility",
                "IOSurfaceCoreAnimationCompatibility",
                "IOSurfaceOpenGLFBOCompatibility",
                "IOSurfaceOpenGLESFBOCompatibility",
                "IOSurfaceOpenGLTextureCompatibility",
                "IOSurfaceOpenGLESTextureCompatibility",
                "IOSurfacePurgeable",
            ] {
                let required = corefoundation::collections::new_dictionary(
                    &[(key(name), true_value)],
                    policy,
                    policy,
                    false,
                );
                assert_eq!(
                    CVPixelBufferCreate(std::ptr::null(), 7, 5, BGRA, required, &mut out),
                    SUCCESS,
                    "{name}"
                );
                assert_eq!(CVPixelBufferLockBaseAddress(out, 0), SUCCESS);
                assert!(!CVPixelBufferGetBaseAddress(out).is_null(), "{name}");
                assert_eq!(CVPixelBufferUnlockBaseAddress(out, 0), SUCCESS);
                assert!(CVPixelBufferGetIOSurface(out).is_null(), "{name}");
                CVPixelBufferRelease(out);
                corefoundation::api::CFRelease(required);

                let not_required = corefoundation::collections::new_dictionary(
                    &[(key(name), false_value)],
                    policy,
                    policy,
                    false,
                );
                assert_eq!(
                    CVPixelBufferCreate(std::ptr::null(), 7, 5, BGRA, not_required, &mut out),
                    SUCCESS,
                    "{name} false"
                );
                CVPixelBufferRelease(out);
                corefoundation::api::CFRelease(not_required);
                out = std::ptr::null();
            }
            let custom = corefoundation::api::CFAllocatorCreate(std::ptr::null(), std::ptr::null());
            assert!(!custom.is_null());
            assert_eq!(
                CVPixelBufferCreate(custom, 7, 5, BGRA, std::ptr::null(), &mut out),
                UNSUPPORTED
            );
            assert!(out.is_null());
            let alloc_attrs = corefoundation::collections::new_dictionary(
                &[(key("MemoryAllocator"), custom)],
                policy,
                policy,
                false,
            );
            assert_eq!(
                CVPixelBufferCreate(std::ptr::null(), 7, 5, BGRA, alloc_attrs, &mut out),
                UNSUPPORTED
            );
            assert!(out.is_null());
            corefoundation::api::CFRelease(alloc_attrs);
            corefoundation::api::CFRelease(custom);

            let wrong: i64 = 1;
            let number = corefoundation::api::CFNumberCreate(
                std::ptr::null(),
                number_type::SINT64,
                (&wrong as *const i64).cast(),
            );
            for (name, expected) in [
                ("MemoryAllocator", INVALID_ATTRIBUTES),
                ("IOSurfaceCoreAnimationCompatibility", SUCCESS),
            ] {
                let bad_attrs = corefoundation::collections::new_dictionary(
                    &[(key(name), number)],
                    policy,
                    policy,
                    false,
                );
                assert_eq!(
                    CVPixelBufferCreate(std::ptr::null(), 7, 5, BGRA, bad_attrs, &mut out),
                    expected,
                    "{name} type"
                );
                if expected == SUCCESS {
                    CVPixelBufferRelease(out);
                } else {
                    assert!(out.is_null());
                }
                corefoundation::api::CFRelease(bad_attrs);
            }
            corefoundation::api::CFRelease(number);
        }
    }

    #[test]
    fn arbitrary_positive_row_and_plane_multiples() {
        use corefoundation::callbacks::Policy;
        use corefoundation::collections::number_type;
        let policy = Policy::from_type_callbacks();
        unsafe {
            for (name, multiple) in [
                ("BytesPerRowAlignment", 3_i64),
                ("BytesPerRowAlignment", 24_i64),
                ("BytesPerRowAlignment", 8192_i64),
                ("PlaneAlignment", 1_i64),
                ("PlaneAlignment", 3_i64),
                ("PlaneAlignment", 24_i64),
                ("PlaneAlignment", 65_i64),
                ("PlaneAlignment", 4097_i64),
                ("PlaneAlignment", 8192_i64),
            ] {
                let number = corefoundation::api::CFNumberCreate(
                    std::ptr::null(),
                    number_type::SINT64,
                    (&multiple as *const i64).cast(),
                );
                let attrs = corefoundation::collections::new_dictionary(
                    &[(key(name), number)],
                    policy,
                    policy,
                    false,
                );
                let mut out = std::ptr::null();
                assert_eq!(
                    CVPixelBufferCreate(std::ptr::null(), 7, 5, I420_VIDEO, attrs, &mut out),
                    SUCCESS,
                    "{name}={multiple}"
                );
                assert_eq!(CVPixelBufferLockBaseAddress(out, 0), SUCCESS);
                let base = CVPixelBufferGetBaseAddress(out) as usize;
                for i in 0..3 {
                    let plane = CVPixelBufferGetBaseAddressOfPlane(out, i) as usize;
                    let row_bytes = CVPixelBufferGetBytesPerRowOfPlane(out, i);
                    assert!(plane > base);
                    assert_eq!(plane % DEFAULT_ALIGNMENT, 0);
                    if name == "PlaneAlignment" {
                        assert_eq!(plane % multiple as usize, 0);
                    } else {
                        assert_eq!(row_bytes % multiple as usize, 0);
                    }
                    assert!(
                        plane - base + row_bytes * CVPixelBufferGetHeightOfPlane(out, i)
                            <= CVPixelBufferGetDataSize(out)
                    );
                }
                assert_eq!(CVPixelBufferUnlockBaseAddress(out, 0), SUCCESS);
                CVPixelBufferRelease(out);
                corefoundation::api::CFRelease(attrs);
                corefoundation::api::CFRelease(number);
            }
        }
    }

    #[test]
    fn numeric_alignment_attributes_match_macos_conversions() {
        use corefoundation::callbacks::Policy;
        use corefoundation::collections::number_type;
        let policy = Policy::from_type_callbacks();
        unsafe {
            // Match native CoreVideo's CFNumber-to-integer truncation and its
            // 64-byte row minimum for 7x5 packed and planar buffers.
            for (name, format, numeric, expected_status, expected_stride) in [
                ("BytesPerRowAlignment", BGRA, 0.0_f64, SUCCESS, 64),
                ("BytesPerRowAlignment", BGRA, 0.5, SUCCESS, 64),
                ("BytesPerRowAlignment", BGRA, -0.5, SUCCESS, 64),
                ("BytesPerRowAlignment", BGRA, 1.9, SUCCESS, 64),
                ("BytesPerRowAlignment", BGRA, 3.5, SUCCESS, 192),
                ("BytesPerRowAlignment", BGRA, 63.9, SUCCESS, 4032),
                ("BytesPerRowAlignment", BGRA, 64.0, SUCCESS, 64),
                ("BytesPerRowAlignment", BGRA, 65.1, SUCCESS, 4160),
                ("BytesPerRowAlignment", BGRA, -1.0, ALLOCATION_FAILED, 0),
                ("BytesPerRowAlignment", BGRA, -1.5, ALLOCATION_FAILED, 0),
                ("BytesPerRowAlignment", NV12_VIDEO, 0.0, SUCCESS, 64),
                ("BytesPerRowAlignment", NV12_VIDEO, 3.5, SUCCESS, 192),
                (
                    "BytesPerRowAlignment",
                    NV12_VIDEO,
                    -1.0,
                    ALLOCATION_FAILED,
                    0,
                ),
                ("PlaneAlignment", NV12_VIDEO, 0.0, SUCCESS, 64),
                ("PlaneAlignment", NV12_VIDEO, 3.5, SUCCESS, 64),
                ("PlaneAlignment", NV12_VIDEO, -0.5, SUCCESS, 64),
                ("PlaneAlignment", NV12_VIDEO, -1.0, ALLOCATION_FAILED, 0),
                ("PlaneAlignment", NV12_VIDEO, -1.5, ALLOCATION_FAILED, 0),
                // Packed formats do not have per-plane alignment requirements.
                ("PlaneAlignment", BGRA, -1.0, SUCCESS, 64),
            ] {
                let value = corefoundation::api::CFNumberCreate(
                    std::ptr::null(),
                    number_type::FLOAT64,
                    (&numeric as *const f64).cast(),
                );
                let attrs = corefoundation::collections::new_dictionary(
                    &[(key(name), value)],
                    policy,
                    policy,
                    false,
                );
                let mut out = 1usize as CFTypeRef;
                assert_eq!(
                    CVPixelBufferCreate(std::ptr::null(), 7, 5, format, attrs, &mut out),
                    expected_status,
                    "{name}={numeric} format={format:#x}",
                );
                if expected_status == SUCCESS {
                    let stride = if format == BGRA {
                        CVPixelBufferGetBytesPerRow(out)
                    } else {
                        CVPixelBufferGetBytesPerRowOfPlane(out, 0)
                    };
                    assert_eq!(
                        stride, expected_stride,
                        "{name}={numeric} format={format:#x}"
                    );
                    CVPixelBufferRelease(out);
                } else {
                    assert!(out.is_null(), "{name}={numeric} format={format:#x}");
                }
                corefoundation::api::CFRelease(attrs);
                corefoundation::api::CFRelease(value);
            }
            // An integer CFNumber uses the same zero and negative CVReturn
            // rules for both alignment keys on planar buffers.
            for (name, multiple, expected) in [
                ("BytesPerRowAlignment", 0_i64, SUCCESS),
                ("PlaneAlignment", 0_i64, SUCCESS),
                ("BytesPerRowAlignment", -1_i64, ALLOCATION_FAILED),
                ("PlaneAlignment", -1_i64, ALLOCATION_FAILED),
            ] {
                let value = corefoundation::api::CFNumberCreate(
                    std::ptr::null(),
                    number_type::SINT64,
                    (&multiple as *const i64).cast(),
                );
                let attrs = corefoundation::collections::new_dictionary(
                    &[(key(name), value)],
                    policy,
                    policy,
                    false,
                );
                let mut out = 1usize as CFTypeRef;
                assert_eq!(
                    CVPixelBufferCreate(std::ptr::null(), 7, 5, NV12_VIDEO, attrs, &mut out),
                    expected,
                    "{name}={multiple}"
                );
                if expected == SUCCESS {
                    assert_eq!(CVPixelBufferGetBytesPerRowOfPlane(out, 0), 64);
                    CVPixelBufferRelease(out);
                } else {
                    assert!(out.is_null());
                }
                corefoundation::api::CFRelease(attrs);
                corefoundation::api::CFRelease(value);
            }
        }
    }

    #[test]
    fn row_alignment_preserves_corevideo_minimum_stride() {
        use corefoundation::callbacks::Policy;
        use corefoundation::collections::number_type;
        let policy = Policy::from_type_callbacks();
        unsafe {
            for (multiple, expected) in [
                (1_i64, 64_usize),
                (3, 192),
                (24, 192),
                (8192, 8192),
                (4097, 262208),
            ] {
                let n = corefoundation::api::CFNumberCreate(
                    std::ptr::null(),
                    number_type::SINT64,
                    (&multiple as *const i64).cast(),
                );
                let attrs = corefoundation::collections::new_dictionary(
                    &[(key("BytesPerRowAlignment"), n)],
                    policy,
                    policy,
                    false,
                );
                let mut out = std::ptr::null();
                assert_eq!(
                    CVPixelBufferCreate(std::ptr::null(), 7, 5, BGRA, attrs, &mut out),
                    SUCCESS
                );
                assert_eq!(
                    CVPixelBufferGetBytesPerRow(out),
                    expected,
                    "multiple={multiple}"
                );
                CVPixelBufferRelease(out);
                corefoundation::api::CFRelease(attrs);
                corefoundation::api::CFRelease(n);
            }
        }
    }

    #[test]
    fn repeated_lifetimes_and_planar_pointer_bounds() {
        for _ in 0..128 {
            let b = create(1, 1, I420_VIDEO);
            unsafe {
                assert_eq!(CVPixelBufferGetPlaneCount(b), 3);
                assert_eq!(CVPixelBufferLockBaseAddress(b, 0), SUCCESS);
                for plane in 0..3 {
                    let ptr = CVPixelBufferGetBaseAddressOfPlane(b, plane).cast::<u8>();
                    assert!(!ptr.is_null());
                    assert_eq!(*ptr, 0);
                    *ptr = (plane + 1) as u8;
                }
                assert_eq!(
                    CVPixelBufferGetBaseAddressOfPlane(b, usize::MAX),
                    std::ptr::null_mut()
                );
                assert_eq!(CVPixelBufferGetWidthOfPlane(b, usize::MAX), 0);
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, 0), SUCCESS);
                assert_eq!(CVPixelBufferLockBaseAddress(b, READ_ONLY), SUCCESS);
                for plane in 0..3 {
                    assert_eq!(
                        *CVPixelBufferGetBaseAddressOfPlane(b, plane).cast::<u8>(),
                        (plane + 1) as u8
                    );
                }
                assert_eq!(CVPixelBufferUnlockBaseAddress(b, READ_ONLY), SUCCESS);
                let retain = corefoundation::api::CFRetain(b);
                CVPixelBufferRelease(b);
                assert_eq!(CVPixelBufferGetDataSize(retain), 256);
                corefoundation::api::CFRelease(retain);
            }
        }
    }

    #[test]
    fn concurrent_retention_and_read_only_locks() {
        let b = create(2, 2, BGRA);
        let address = b as usize;
        let threads: Vec<_> = (0..6)
            .map(|_| {
                std::thread::spawn(move || {
                    let ptr = address as CFTypeRef;
                    unsafe {
                        let retained = CVPixelBufferRetain(ptr);
                        for _ in 0..100 {
                            assert_eq!(CVPixelBufferLockBaseAddress(retained, READ_ONLY), SUCCESS);
                            assert!(!CVPixelBufferGetBaseAddress(retained).is_null());
                            assert_eq!(
                                CVPixelBufferUnlockBaseAddress(retained, READ_ONLY),
                                SUCCESS
                            );
                        }
                        CVPixelBufferRelease(retained);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        unsafe { CVPixelBufferRelease(b) };
    }

    #[test]
    fn an_odd_row_multiple_does_not_inflate_the_allocation() {
        use corefoundation::callbacks::Policy;
        use corefoundation::collections::number_type;
        let policy = Policy::from_type_callbacks();
        // 4097 makes each row 262208 bytes; 1023 rows sit just under the cap,
        // and aligning the base to the whole row multiple would cross it.
        let multiple: i64 = 4097;
        unsafe {
            let number = corefoundation::api::CFNumberCreate(
                std::ptr::null(),
                number_type::SINT64,
                (&multiple as *const i64).cast(),
            );
            let attrs = corefoundation::collections::new_dictionary(
                &[(key("BytesPerRowAlignment"), number)],
                policy,
                policy,
                false,
            );
            let mut out = std::ptr::null();
            assert_eq!(
                CVPixelBufferCreate(std::ptr::null(), 16384, 1023, BGRA, attrs, &mut out),
                SUCCESS
            );
            assert_eq!(CVPixelBufferGetBytesPerRow(out), 262208);
            assert_eq!(CVPixelBufferLockBaseAddress(out, 0), SUCCESS);
            assert_eq!(
                CVPixelBufferGetBaseAddress(out) as usize % DEFAULT_ALIGNMENT,
                0
            );
            assert_eq!(CVPixelBufferUnlockBaseAddress(out, 0), SUCCESS);
            CVPixelBufferRelease(out);
            corefoundation::api::CFRelease(attrs);
            corefoundation::api::CFRelease(number);
        }
    }

    /// What VideoToolbox's output callback hands a guest on a Mac: a buffer
    /// from Apple's CoreVideo, which has to be answered by Apple's.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_host_buffer_is_answered_by_the_host_corevideo() {
        type Create =
            unsafe extern "C" fn(CFTypeRef, usize, usize, u32, CFTypeRef, *mut CFTypeRef) -> i32;
        // SAFETY: a NUL-terminated path, and a handle deliberately leaked.
        let handle = unsafe {
            libc::dlopen(
                c"/System/Library/Frameworks/CoreVideo.framework/CoreVideo".as_ptr(),
                libc::RTLD_LAZY,
            )
        };
        assert!(!handle.is_null());
        let create = corefoundation::foreign::entry(c"CVPixelBufferCreate").unwrap();
        unsafe {
            let create: Create = std::mem::transmute(create);
            let mut host = std::ptr::null();
            assert_eq!(
                create(
                    std::ptr::null(),
                    7,
                    5,
                    NV12_VIDEO,
                    std::ptr::null(),
                    &mut host
                ),
                SUCCESS
            );
            assert!(corefoundation::foreign::is(host));
            assert_eq!(CVPixelBufferGetWidth(host), 7);
            assert_eq!(CVPixelBufferGetPlaneCount(host), 2);
            assert_eq!(CVPixelBufferGetWidthOfPlane(host, 1), 4);
            assert_eq!(CVPixelBufferLockBaseAddress(host, 0), SUCCESS);
            assert!(!CVPixelBufferGetBaseAddressOfPlane(host, 1).is_null());
            assert_eq!(CVPixelBufferUnlockBaseAddress(host, 0), SUCCESS);
            CVPixelBufferRelease(host);
        }
    }

    #[test]
    fn a_buffer_over_the_callers_bytes_hands_them_back_at_final_release() {
        unsafe extern "C" fn release(refcon: *mut c_void, base: *const c_void) {
            // SAFETY: the test's own counter and bytes.
            unsafe {
                assert_eq!(*refcon.cast::<*const c_void>(), base);
                *refcon.cast::<*const c_void>() = std::ptr::null();
            }
        }
        let mut bytes = vec![0u8; 32 * 5];
        let base: *mut c_void = bytes.as_mut_ptr().cast();
        let mut seen: *const c_void = base;
        unsafe {
            let mut out = std::ptr::null();
            for (format, row, expected) in [
                (NV12_VIDEO, 32, INVALID_PIXEL_FORMAT),
                (BGRA, 27, INVALID_ARGUMENT),
            ] {
                assert_eq!(
                    CVPixelBufferCreateWithBytes(
                        std::ptr::null(),
                        7,
                        5,
                        format,
                        base,
                        row,
                        Some(release),
                        (&mut seen as *mut *const c_void).cast(),
                        std::ptr::null(),
                        &mut out,
                    ),
                    expected
                );
            }
            // A failed create does not call back.
            assert_eq!(seen, base);
            assert_eq!(
                CVPixelBufferCreateWithBytes(
                    std::ptr::null(),
                    7,
                    5,
                    BGRA,
                    base,
                    32,
                    Some(release),
                    (&mut seen as *mut *const c_void).cast(),
                    std::ptr::null(),
                    &mut out,
                ),
                SUCCESS
            );
            assert_eq!(CVPixelBufferGetBytesPerRow(out), 32);
            assert_eq!(CVPixelBufferGetDataSize(out), 160);
            assert_eq!(
                CVImageBufferGetEncodedSize(out),
                CGSize {
                    width: 7.0,
                    height: 5.0
                }
            );
            assert_eq!(
                CVImageBufferGetDisplaySize(out),
                CVImageBufferGetEncodedSize(out)
            );
            let mut extended = [9usize; 4];
            let [l, r, t, b] = extended.each_mut().map(|e| e as *mut usize);
            CVPixelBufferGetExtendedPixels(out, l, r, t, b);
            assert_eq!(extended, [0; 4]);
            assert_eq!(CVPixelBufferLockBaseAddress(out, 0), SUCCESS);
            assert_eq!(CVPixelBufferGetBaseAddress(out), base);
            assert_eq!(CVPixelBufferGetBaseAddressOfPlane(out, 0), base);
            assert_eq!(CVPixelBufferUnlockBaseAddress(out, 0), SUCCESS);
            let retained = CVPixelBufferRetain(out);
            CVPixelBufferRelease(out);
            assert_eq!(seen, base);
            CVPixelBufferRelease(retained);
            assert!(seen.is_null());
        }
    }

    #[test]
    fn a_buffer_over_the_callers_planes_reports_them_and_hands_them_back() {
        static RELEASED: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn release(
            _refcon: *mut c_void,
            _data: *const c_void,
            size: usize,
            count: usize,
            planes: *const *const c_void,
        ) {
            assert_eq!((size, count), (99, 2));
            // SAFETY: the array the buffer keeps for this call.
            assert!(!unsafe { *planes.add(1) }.is_null());
            RELEASED.fetch_add(1, Ordering::Relaxed);
        }
        let mut luma = vec![0u8; 8 * 5];
        let mut chroma = vec![0u8; 8 * 3];
        let bases: [*mut c_void; 2] = [luma.as_mut_ptr().cast(), chroma.as_mut_ptr().cast()];
        let (widths, heights, rows) = ([7usize, 4], [5usize, 3], [8usize, 8]);
        unsafe {
            let mut out = std::ptr::null();
            let create = |format, count, out: &mut CFTypeRef| {
                CVPixelBufferCreateWithPlanarBytes(
                    std::ptr::null(),
                    7,
                    5,
                    format,
                    std::ptr::null_mut(),
                    99,
                    count,
                    bases.as_ptr(),
                    widths.as_ptr(),
                    heights.as_ptr(),
                    rows.as_ptr(),
                    Some(release),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    out,
                )
            };
            assert_eq!(create(BGRA, 2, &mut out), INVALID_PIXEL_FORMAT);
            assert_eq!(create(NV12_VIDEO, 3, &mut out), INVALID_ARGUMENT);
            assert_eq!(create(NV12_VIDEO, 2, &mut out), SUCCESS);
            assert_eq!(CVPixelBufferGetPlaneCount(out), 2);
            assert_eq!(CVPixelBufferGetWidthOfPlane(out, 1), 4);
            assert_eq!(CVPixelBufferGetBytesPerRowOfPlane(out, 1), 8);
            assert_eq!(CVPixelBufferGetDataSize(out), 99);
            assert_eq!(CVPixelBufferLockBaseAddress(out, 0), SUCCESS);
            assert!(CVPixelBufferGetBaseAddress(out).is_null());
            assert_eq!(CVPixelBufferGetBaseAddressOfPlane(out, 1), bases[1]);
            assert_eq!(CVPixelBufferUnlockBaseAddress(out, 0), SUCCESS);
            assert_eq!(RELEASED.load(Ordering::Relaxed), 0);
            CVPixelBufferRelease(out);
            assert_eq!(RELEASED.load(Ordering::Relaxed), 1);
        }
    }

    /// What VideoToolbox's encoder is handed for one of this runtime's buffers.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_host_copy_carries_every_plane() {
        // SAFETY: a NUL-terminated path, and a handle deliberately leaked.
        let handle = unsafe {
            libc::dlopen(
                c"/System/Library/Frameworks/CoreVideo.framework/CoreVideo".as_ptr(),
                libc::RTLD_LAZY,
            )
        };
        assert!(!handle.is_null());
        let ours = create(7, 5, NV12_VIDEO);
        unsafe {
            assert_eq!(CVPixelBufferLockBaseAddress(ours, 0), SUCCESS);
            for plane in 0..2 {
                let rows = CVPixelBufferGetBytesPerRowOfPlane(ours, plane);
                let base = CVPixelBufferGetBaseAddressOfPlane(ours, plane).cast::<u8>();
                for y in 0..CVPixelBufferGetHeightOfPlane(ours, plane) {
                    *base.add(y * rows) = (plane * 16 + y) as u8;
                }
            }
            assert_eq!(CVPixelBufferUnlockBaseAddress(ours, 0), SUCCESS);
            assert!(host_copy(std::ptr::null()).is_none());
            let copy = host_copy(ours).expect("the host CoreVideo makes a copy");
            assert!(corefoundation::foreign::is(copy));
            assert!(host_copy(copy).is_none());
            // Read back through the forwarding accessors, which are the host's.
            assert_eq!(CVPixelBufferGetHeightOfPlane(copy, 1), 3);
            assert_eq!(CVPixelBufferLockBaseAddress(copy, 0), SUCCESS);
            for plane in 0..2 {
                let rows = CVPixelBufferGetBytesPerRowOfPlane(copy, plane);
                let base = CVPixelBufferGetBaseAddressOfPlane(copy, plane).cast::<u8>();
                for y in 0..CVPixelBufferGetHeightOfPlane(copy, plane) {
                    assert_eq!(*base.add(y * rows), (plane * 16 + y) as u8);
                }
            }
            assert_eq!(CVPixelBufferUnlockBaseAddress(copy, 0), SUCCESS);
            CVPixelBufferRelease(copy);
            CVPixelBufferRelease(ours);
        }
    }
}
