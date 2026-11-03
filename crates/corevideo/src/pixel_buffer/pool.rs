//! `CVPixelBufferPool`: buffers of one description, made on request.
//!
//! Nothing is recycled. A pool exists so a caller need not repeat the
//! attributes, and so allocation can be cheap; a fresh buffer each time
//! keeps the first and gives up only the second.

use std::sync::OnceLock;

use corefoundation::base::CFTypeRef;
use objc::class::Class;

use super::{
    CVPixelBufferCreate, CVPixelBufferRelease, INVALID_ARGUMENT, INVALID_ATTRIBUTES, SUCCESS,
    UNSUPPORTED, attribute, foreign, refuse_foreign,
};

/// `kCVReturnInvalidPoolAttributes`.
const INVALID_POOL_ATTRIBUTES: i32 = -6691;

struct Pool {
    /// Copies of what the pool was made with; the pool attributes may be null.
    attributes: CFTypeRef,
    buffer_attributes: CFTypeRef,
    width: usize,
    height: usize,
    format: u32,
}

impl Drop for Pool {
    fn drop(&mut self) {
        for object in [self.attributes, self.buffer_attributes] {
            if !object.is_null() {
                // SAFETY: a copy this pool made and owns.
                unsafe { corefoundation::api::CFRelease(object) };
            }
        }
    }
}

fn class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let addr = *CLASS.get_or_init(|| {
        corefoundation::bootstrap();
        let class = corefoundation::boxed::register::<Pool>(c"__NSCVPixelBufferPool", None, true);
        objc::class::ptr(class) as usize
    });
    // SAFETY: the registered class lives for the process.
    unsafe { &*(addr as *const Class) }
}

fn pool<'a>(object: CFTypeRef) -> Option<&'a Pool> {
    if object.is_null() {
        return None;
    }
    // SAFETY: caller holds a live CF object; its class is checked first.
    unsafe { corefoundation::boxed::contents::<Pool>(object, class()) }.map(|p| &*p)
}

/// A positive integer attribute.
fn dimension(attrs: CFTypeRef, name: &str) -> Option<usize> {
    let value = attribute(attrs, name);
    // SAFETY: null or a CF value from the attributes dictionary.
    let number = unsafe { corefoundation::collections::number(value) }?;
    usize::try_from(number.as_i64()).ok().filter(|n| *n > 0)
}

/// The pixel format, which may also be an array of them; the first is the one
/// a pool makes.
fn pixel_format(attrs: CFTypeRef) -> Option<u32> {
    let mut value = attribute(attrs, "PixelFormatType");
    // SAFETY: null or a CF value from the attributes dictionary.
    if let Some(array) = unsafe { corefoundation::collections::array(value) } {
        value = *array.values.first()?;
    }
    // SAFETY: as above.
    let number = unsafe { corefoundation::collections::number(value) }?;
    u32::try_from(number.as_i64()).ok()
}

fn copy(dictionary: CFTypeRef) -> CFTypeRef {
    // SAFETY: a dictionary the caller passed, checked to be one.
    unsafe { corefoundation::api::CFDictionaryCreateCopy(std::ptr::null(), dictionary) }
}

pub unsafe extern "C" fn CVPixelBufferPoolGetTypeID() -> usize {
    corefoundation::base::type_id(class())
}

/// CVReturn CVPixelBufferPoolCreate(CFAllocatorRef, CFDictionaryRef
/// poolAttributes, CFDictionaryRef pixelBufferAttributes,
/// CVPixelBufferPoolRef *).
///
/// # Safety
/// The attributes must be CF objects or null; `out` must be writable.
pub unsafe extern "C" fn CVPixelBufferPoolCreate(
    allocator: CFTypeRef,
    attributes: CFTypeRef,
    buffer_attributes: CFTypeRef,
    out: *mut CFTypeRef,
) -> i32 {
    if out.is_null() {
        return INVALID_ARGUMENT;
    }
    // SAFETY: the caller's out-parameter.
    unsafe { *out = std::ptr::null() };
    if !corefoundation::api::supports_malloc_allocator(allocator.cast()) {
        return UNSUPPORTED;
    }
    let is_dictionary =
        // SAFETY: a CF object the caller passed.
        |d: CFTypeRef| unsafe { corefoundation::collections::dictionary(d) }.is_some();
    if !attributes.is_null() && !is_dictionary(attributes) {
        return INVALID_POOL_ATTRIBUTES;
    }
    if buffer_attributes.is_null() || !is_dictionary(buffer_attributes) {
        return INVALID_ATTRIBUTES;
    }
    let (Some(width), Some(height), Some(format)) = (
        dimension(buffer_attributes, "Width"),
        dimension(buffer_attributes, "Height"),
        pixel_format(buffer_attributes),
    ) else {
        return INVALID_ATTRIBUTES;
    };
    // One buffer now, so a description no buffer can be made from fails
    // here rather than at the first request.
    let mut probe = std::ptr::null();
    match unsafe {
        CVPixelBufferCreate(
            std::ptr::null(),
            width,
            height,
            format,
            buffer_attributes,
            &mut probe,
        )
    } {
        SUCCESS => unsafe { CVPixelBufferRelease(probe) },
        error => return error,
    }
    let made = corefoundation::boxed::make(
        class(),
        Pool {
            attributes: if attributes.is_null() {
                attributes
            } else {
                copy(attributes)
            },
            buffer_attributes: copy(buffer_attributes),
            width,
            height,
            format,
        },
    );
    if made.is_null() {
        return super::ALLOCATION_FAILED;
    }
    // SAFETY: the caller's out-parameter.
    unsafe { *out = made };
    SUCCESS
}

pub unsafe extern "C" fn CVPixelBufferPoolRetain(pool: CFTypeRef) -> CFTypeRef {
    if pool.is_null() {
        return pool;
    }
    // SAFETY: a live pool the caller passed.
    unsafe { corefoundation::api::CFRetain(pool) }
}

pub unsafe extern "C" fn CVPixelBufferPoolRelease(pool: CFTypeRef) {
    if !pool.is_null() {
        // SAFETY: a live pool the caller passed.
        unsafe { corefoundation::api::CFRelease(pool) };
    }
}

/// CFDictionaryRef CVPixelBufferPoolGetAttributes(CVPixelBufferPoolRef),
/// under the get rule.
pub unsafe extern "C" fn CVPixelBufferPoolGetAttributes(object: CFTypeRef) -> CFTypeRef {
    refuse_foreign(c"CVPixelBufferPoolGetAttributes", object);
    pool(object).map_or(std::ptr::null(), |p| p.attributes)
}

/// CFDictionaryRef CVPixelBufferPoolGetPixelBufferAttributes(CVPixelBufferPoolRef),
/// under the get rule.
pub unsafe extern "C" fn CVPixelBufferPoolGetPixelBufferAttributes(object: CFTypeRef) -> CFTypeRef {
    refuse_foreign(c"CVPixelBufferPoolGetPixelBufferAttributes", object);
    pool(object).map_or(std::ptr::null(), |p| p.buffer_attributes)
}

/// CVReturn CVPixelBufferPoolCreatePixelBuffer(CFAllocatorRef,
/// CVPixelBufferPoolRef, CVPixelBufferRef *).
///
/// # Safety
/// `out` must be writable.
pub unsafe extern "C" fn CVPixelBufferPoolCreatePixelBuffer(
    allocator: CFTypeRef,
    object: CFTypeRef,
    out: *mut CFTypeRef,
) -> i32 {
    // VideoToolbox's own pools are the host's on a Mac; see `foreign`.
    forward_foreign!(
        c"CVPixelBufferPoolCreatePixelBuffer",
        object,
        fn(CFTypeRef, CFTypeRef, *mut CFTypeRef) -> i32,
        (corefoundation::api::host_allocator(allocator), object, out)
    );
    let Some(pool) = pool(object) else {
        return INVALID_ARGUMENT;
    };
    unsafe {
        CVPixelBufferCreate(
            allocator,
            pool.width,
            pool.height,
            pool.format,
            pool.buffer_attributes,
            out,
        )
    }
}

/// CVReturn CVPixelBufferPoolCreatePixelBufferWithAuxAttributes(
/// CFAllocatorRef, CVPixelBufferPoolRef, CFDictionaryRef auxAttributes,
/// CVPixelBufferRef *).
///
/// The one auxiliary attribute is an allocation threshold, which a pool that
/// keeps nothing never reaches.
///
/// # Safety
/// As [`CVPixelBufferPoolCreatePixelBuffer`].
pub unsafe extern "C" fn CVPixelBufferPoolCreatePixelBufferWithAuxAttributes(
    allocator: CFTypeRef,
    object: CFTypeRef,
    aux: CFTypeRef,
    out: *mut CFTypeRef,
) -> i32 {
    if !aux.is_null() {
        refuse_foreign(
            c"CVPixelBufferPoolCreatePixelBufferWithAuxAttributes",
            object,
        );
    }
    unsafe { CVPixelBufferPoolCreatePixelBuffer(allocator, object, out) }
}

/// void CVPixelBufferPoolFlush(CVPixelBufferPoolRef, CVPixelBufferPoolFlushFlags):
/// there is nothing kept to flush.
pub unsafe extern "C" fn CVPixelBufferPoolFlush(object: CFTypeRef, flags: u64) {
    forward_foreign!(
        c"CVPixelBufferPoolFlush",
        object,
        fn(CFTypeRef, u64) -> (),
        (object, flags)
    );
}

#[cfg(test)]
mod tests {
    use super::super::{
        BGRA, CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
        INVALID_PIXEL_FORMAT, NV12_VIDEO, key,
    };
    use super::*;
    use corefoundation::callbacks::Policy;
    use corefoundation::collections::number_type;

    fn number(value: i64) -> CFTypeRef {
        // SAFETY: an `i64` is what SINT64 reads.
        unsafe {
            corefoundation::api::CFNumberCreate(
                std::ptr::null(),
                number_type::SINT64,
                (&value as *const i64).cast(),
            )
        }
    }

    fn attributes(pairs: &[(&str, CFTypeRef)]) -> CFTypeRef {
        let policy = Policy::from_type_callbacks();
        let pairs: Vec<_> = pairs.iter().map(|(k, v)| (key(k), *v)).collect();
        corefoundation::collections::new_dictionary(&pairs, policy, policy, false)
    }

    #[test]
    fn a_pool_makes_buffers_of_its_description() {
        let formats = corefoundation::collections::new_array(
            &[number(i64::from(NV12_VIDEO)), number(i64::from(BGRA))],
            Policy::from_type_callbacks(),
            false,
        );
        let attrs = attributes(&[
            ("Width", number(7)),
            ("Height", number(5)),
            ("PixelFormatType", formats),
        ]);
        unsafe {
            let mut pool = std::ptr::null();
            assert_eq!(
                CVPixelBufferPoolCreate(std::ptr::null(), std::ptr::null(), attrs, &mut pool),
                SUCCESS
            );
            assert_eq!(
                corefoundation::api::CFGetTypeID(pool),
                CVPixelBufferPoolGetTypeID()
            );
            assert!(CVPixelBufferPoolGetAttributes(pool).is_null());
            let kept = CVPixelBufferPoolGetPixelBufferAttributes(pool);
            assert_eq!(corefoundation::api::CFDictionaryGetCount(kept), 3);
            let mut out = std::ptr::null();
            for _ in 0..2 {
                assert_eq!(
                    CVPixelBufferPoolCreatePixelBuffer(std::ptr::null(), pool, &mut out),
                    SUCCESS
                );
                assert_eq!(CVPixelBufferGetWidth(out), 7);
                assert_eq!(CVPixelBufferGetHeight(out), 5);
                assert_eq!(CVPixelBufferGetPixelFormatType(out), NV12_VIDEO);
                CVPixelBufferRelease(out);
            }
            CVPixelBufferPoolFlush(pool, 0);
            assert_eq!(
                CVPixelBufferPoolCreatePixelBuffer(std::ptr::null(), std::ptr::null(), &mut out),
                INVALID_ARGUMENT
            );
            CVPixelBufferPoolRelease(pool);
        }
    }

    #[test]
    fn a_pool_needs_a_size_and_a_format_it_can_make() {
        unsafe {
            let mut pool = std::ptr::null();
            for (attrs, expected) in [
                (std::ptr::null(), INVALID_ATTRIBUTES),
                (
                    attributes(&[("Width", number(7)), ("Height", number(5))]),
                    INVALID_ATTRIBUTES,
                ),
                (
                    attributes(&[
                        ("Width", number(7)),
                        ("Height", number(5)),
                        ("PixelFormatType", number(0x7a7a_7a7a)),
                    ]),
                    INVALID_PIXEL_FORMAT,
                ),
            ] {
                assert_eq!(
                    CVPixelBufferPoolCreate(std::ptr::null(), std::ptr::null(), attrs, &mut pool),
                    expected
                );
                assert!(pool.is_null());
            }
        }
    }
}
