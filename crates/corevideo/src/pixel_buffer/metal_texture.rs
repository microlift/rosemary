//! `CVMetalTextureCache`: Metal textures made from pixel buffers, one plane
//! each.
//!
//! The texture is made with ordinary messages to the guest's `MTLDevice`, so
//! this crate needs nothing of the Metal implementation but its classes.
//!
//! FIXME (ACCURACY): the texture is a copy of the plane taken when it is made,
//! where Apple's aliases the buffer's IOSurface. A decoded frame is never
//! written after it is handed out, so on that path nothing can tell.

use std::sync::OnceLock;

use corefoundation::base::{CFTypeID, CFTypeRef};
use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use super::{BGRA, ERROR, I420_FULL, I420_VIDEO, INVALID_ARGUMENT, NV12_FULL, NV12_VIDEO, SUCCESS};

/// `MTLPixelFormat` values a plane can become.
pub const R8_UNORM: usize = 10;
pub const RG8_UNORM: usize = 30;
pub const RGBA8_UNORM: usize = 70;
pub const RGBA8_UNORM_SRGB: usize = 71;
pub const BGRA8_UNORM: usize = 80;
pub const BGRA8_UNORM_SRGB: usize = 81;

/// The bytes per pixel of a Metal format this cache can fill, or `None`.
fn texture_bytes_per_pixel(pixel_format: usize) -> Option<usize> {
    match pixel_format {
        R8_UNORM => Some(1),
        RG8_UNORM => Some(2),
        RGBA8_UNORM | RGBA8_UNORM_SRGB | BGRA8_UNORM | BGRA8_UNORM_SRGB => Some(4),
        _ => None,
    }
}

/// The bytes per element of a pixel buffer's plane.
fn plane_bytes_per_element(format: u32, plane: usize) -> Option<usize> {
    match (format, plane) {
        (BGRA, 0) => Some(4),
        (NV12_VIDEO | NV12_FULL, 0) => Some(1),
        (NV12_VIDEO | NV12_FULL, 1) => Some(2),
        (I420_VIDEO | I420_FULL, 0..=2) => Some(1),
        _ => None,
    }
}

/// `MTLRegion`: an origin and a size, three `NSUInteger` each. Forty-eight
/// bytes, so it travels in memory.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Region {
    pub x: usize,
    pub y: usize,
    pub z: usize,
    pub width: usize,
    pub height: usize,
    pub depth: usize,
}

struct Cache {
    /// The retained `id<MTLDevice>`.
    device: usize,
}

impl Drop for Cache {
    fn drop(&mut self) {
        objc::arc::release(self.device as Id);
    }
}

struct Texture {
    /// The retained `id<MTLTexture>`.
    texture: usize,
    /// The retained pixel buffer it was made from.
    source: usize,
}

impl Drop for Texture {
    fn drop(&mut self) {
        objc::arc::release(self.texture as Id);
        // SAFETY: the reference this texture took when it was made.
        unsafe { corefoundation::api::CFRelease(self.source as CFTypeRef) };
    }
}

fn registered<T: 'static>(slot: &OnceLock<usize>, name: &'static std::ffi::CStr) -> &'static Class {
    let address = *slot.get_or_init(|| {
        corefoundation::bootstrap();
        objc::class::ptr(corefoundation::boxed::register::<T>(name, None, true)) as usize
    });
    // SAFETY: the registered class lives for the process.
    unsafe { &*(address as *const Class) }
}

fn cache_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    registered::<Cache>(&CLASS, c"CVMetalTextureCache")
}

fn texture_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    registered::<Texture>(&CLASS, c"CVMetalTexture")
}

/// `CVReturn CVMetalTextureCacheCreate(CFAllocatorRef, CFDictionaryRef
/// cacheAttributes, id<MTLDevice>, CFDictionaryRef textureAttributes,
/// CVMetalTextureCacheRef *)`
pub unsafe extern "C" fn CVMetalTextureCacheCreate(
    _allocator: CFTypeRef,
    _cache_attributes: CFTypeRef,
    device: Id,
    _texture_attributes: CFTypeRef,
    out: *mut CFTypeRef,
) -> i32 {
    if out.is_null() || device.is_null() {
        return INVALID_ARGUMENT;
    }
    let cache = corefoundation::boxed::make(
        cache_class(),
        Cache {
            device: objc::arc::retain(device) as usize,
        },
    );
    // SAFETY: the caller's out-parameter.
    unsafe { out.write(cache) };
    SUCCESS
}

fn refuse(what: &str) -> i32 {
    eprintln!("rosemary: CVMetalTextureCacheCreateTextureFromImage: {what}; no texture is made");
    INVALID_ARGUMENT
}

/// A new texture holding the rows at `bytes`, from `device`: +1 retained, or
/// null.
fn new_texture(
    device: Id,
    pixel_format: usize,
    width: usize,
    height: usize,
    bytes: *const u8,
    bytes_per_row: usize,
) -> Id {
    let Some(descriptor_class) = objc::class::lookup(c"MTLTextureDescriptor") else {
        return std::ptr::null_mut();
    };
    let pool = objc::arc::pool_push();
    // SAFETY: the selectors' own prototypes, sent to Metal's descriptor class
    // and the guest's device, and a region inside the plane at `bytes`.
    let texture = unsafe {
        let descriptor_for: unsafe extern "C" fn(Id, Sel, usize, usize, usize, u8) -> Id =
            std::mem::transmute(objc::msg::entry());
        let descriptor = descriptor_for(
            objc::class::ptr(descriptor_class) as Id,
            objc::sel::get("texture2DDescriptorWithPixelFormat:width:height:mipmapped:"),
            pixel_format,
            width,
            height,
            0,
        );
        let new_texture: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            std::mem::transmute(objc::msg::entry());
        let texture = match descriptor.is_null() {
            true => std::ptr::null_mut(),
            false => new_texture(
                device,
                objc::sel::get("newTextureWithDescriptor:"),
                descriptor,
            ),
        };
        if !texture.is_null() {
            let replace: unsafe extern "C" fn(Id, Sel, Region, usize, *const u8, usize) =
                std::mem::transmute(objc::msg::entry());
            replace(
                texture,
                objc::sel::get("replaceRegion:mipmapLevel:withBytes:bytesPerRow:"),
                Region {
                    width,
                    height,
                    depth: 1,
                    ..Region::default()
                },
                0,
                bytes,
                bytes_per_row,
            );
        }
        texture
    };
    objc::arc::pool_pop(pool);
    texture
}

/// `CVReturn CVMetalTextureCacheCreateTextureFromImage(CFAllocatorRef,
/// CVMetalTextureCacheRef, CVImageBufferRef, CFDictionaryRef
/// textureAttributes, MTLPixelFormat, size_t width, size_t height, size_t
/// planeIndex, CVMetalTextureRef *)`
///
/// The format has to match the plane's bytes per element and the size the
/// plane's size, since nothing here converts or scales.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CVMetalTextureCacheCreateTextureFromImage(
    _allocator: CFTypeRef,
    texture_cache: CFTypeRef,
    image: CFTypeRef,
    _texture_attributes: CFTypeRef,
    pixel_format: usize,
    width: usize,
    height: usize,
    plane_index: usize,
    out: *mut CFTypeRef,
) -> i32 {
    if out.is_null() {
        return INVALID_ARGUMENT;
    }
    // SAFETY: the caller's out-parameter.
    unsafe { out.write(std::ptr::null()) };
    // SAFETY: null or an object a guest received from this runtime.
    let Some(cache) =
        (unsafe { corefoundation::boxed::contents::<Cache>(texture_cache, cache_class()) })
    else {
        return refuse("the cache is not a CVMetalTextureCache");
    };
    let Some(state) = super::state(image) else {
        return refuse("the image is not a CVPixelBuffer");
    };
    let Some(plane) = state.planes.get(plane_index) else {
        return refuse(&format!(
            "plane {plane_index} of a buffer with {} planes",
            state.planes.len()
        ));
    };
    if (width, height) != (plane.width, plane.height) {
        return refuse(&format!(
            "{width}x{height} for a {}x{} plane; nothing scales",
            plane.width, plane.height
        ));
    }
    let element = plane_bytes_per_element(state.format, plane_index);
    if texture_bytes_per_pixel(pixel_format).is_none()
        || texture_bytes_per_pixel(pixel_format) != element
    {
        return refuse(&format!(
            "MTLPixelFormat {pixel_format} for a plane of {} bytes per element",
            element.unwrap_or(0)
        ));
    }
    let texture = new_texture(
        cache.device as Id,
        pixel_format,
        width,
        height,
        plane.address,
        plane.row_bytes,
    );
    if texture.is_null() {
        eprintln!(
            "rosemary: CVMetalTextureCacheCreateTextureFromImage: the device made no texture"
        );
        return ERROR;
    }
    // SAFETY: a live pixel buffer, which the texture keeps alive.
    let source = unsafe { corefoundation::api::CFRetain(image) };
    let made = corefoundation::boxed::make(
        texture_class(),
        Texture {
            texture: texture as usize,
            source: source as usize,
        },
    );
    // SAFETY: the caller's out-parameter.
    unsafe { out.write(made) };
    SUCCESS
}

/// `id<MTLTexture> CVMetalTextureGetTexture(CVMetalTextureRef)`: not retained.
pub unsafe extern "C" fn CVMetalTextureGetTexture(texture: CFTypeRef) -> Id {
    // SAFETY: null or an object a guest received from this runtime.
    unsafe { corefoundation::boxed::contents::<Texture>(texture, texture_class()) }
        .map_or(std::ptr::null_mut(), |t| t.texture as Id)
}

/// `void CVMetalTextureCacheFlush(CVMetalTextureCacheRef, CVOptionFlags)`:
/// nothing is cached.
pub unsafe extern "C" fn CVMetalTextureCacheFlush(_cache: CFTypeRef, _options: u64) {}

pub unsafe extern "C" fn CVMetalTextureCacheGetTypeID() -> CFTypeID {
    corefoundation::base::type_id(cache_class())
}

pub unsafe extern "C" fn CVMetalTextureGetTypeID() -> CFTypeID {
    corefoundation::base::type_id(texture_class())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;

    /// A fake Metal object's body: what was asked of it and what it was given.
    #[repr(C)]
    struct Fake {
        isa: *const c_void,
        pixel_format: usize,
        width: usize,
        height: usize,
        /// Rows written by `replaceRegion:`, `width * bpp` bytes each.
        bytes: *mut Vec<u8>,
    }

    fn fake(class: &'static Class, pixel_format: usize, width: usize, height: usize) -> Id {
        // SAFETY: `Fake` is the layout the fake classes were registered with.
        let object = unsafe { corefoundation::base::allocate::<Fake>(class) };
        // SAFETY: a fresh allocation; the vector is leaked with the object.
        unsafe {
            (*object).pixel_format = pixel_format;
            (*object).width = width;
            (*object).height = height;
            (*object).bytes = Box::into_raw(Box::default());
        }
        object as Id
    }

    fn body<'a>(object: Id) -> &'a Fake {
        // SAFETY: one of the fakes, whose body is `Fake`.
        unsafe { &*(object as *const Fake) }
    }

    unsafe extern "C" fn descriptor(
        _class: Id,
        _sel: Sel,
        pixel_format: usize,
        width: usize,
        height: usize,
        _mipmapped: u8,
    ) -> Id {
        let object = fake(classes().0, pixel_format, width, height);
        objc::arc::autorelease(object)
    }

    unsafe extern "C" fn make_texture(_device: Id, _sel: Sel, descriptor: Id) -> Id {
        let d = body(descriptor);
        fake(classes().2, d.pixel_format, d.width, d.height)
    }

    unsafe extern "C" fn replace(
        texture: Id,
        _sel: Sel,
        region: Region,
        level: usize,
        bytes: *const u8,
        bytes_per_row: usize,
    ) {
        let t = body(texture);
        assert_eq!(level, 0);
        assert_eq!(
            region,
            Region {
                width: t.width,
                height: t.height,
                depth: 1,
                ..Region::default()
            }
        );
        let bpp = texture_bytes_per_pixel(t.pixel_format).unwrap();
        // SAFETY: the vector this fake owns, and the caller's rows.
        unsafe {
            let out = &mut *t.bytes;
            for row in 0..region.height {
                out.extend_from_slice(std::slice::from_raw_parts(
                    bytes.add(row * bytes_per_row),
                    region.width * bpp,
                ));
            }
        }
    }

    /// The fake descriptor, device and texture classes, registered once. The
    /// descriptor takes Metal's name: this crate's tests have no Metal.
    fn classes() -> (&'static Class, &'static Class, &'static Class) {
        static CLASSES: OnceLock<(usize, usize, usize)> = OnceLock::new();
        let (d, v, t) = *CLASSES.get_or_init(|| {
            corefoundation::bootstrap();
            let register =
                |name| corefoundation::base::register(name, None, size_of::<Fake>(), false);
            let d = register(c"MTLTextureDescriptor");
            d.metaclass().add_method(
                objc::sel::get("texture2DDescriptorWithPixelFormat:width:height:mipmapped:"),
                descriptor as *const c_void,
                c"@48@0:8Q16Q24Q32c40",
            );
            let v = register(c"FakeMTLDevice");
            v.add_method(
                objc::sel::get("newTextureWithDescriptor:"),
                make_texture as *const c_void,
                c"@24@0:8@16",
            );
            let t = register(c"FakeMTLTexture");
            t.add_method(
                objc::sel::get("replaceRegion:mipmapLevel:withBytes:bytesPerRow:"),
                replace as *const c_void,
                c"v96@0:8{?=QQQQQQ}16Q64^v72Q80",
            );
            (
                objc::class::ptr(d) as usize,
                objc::class::ptr(v) as usize,
                objc::class::ptr(t) as usize,
            )
        });
        // SAFETY: leaked classes registered above.
        unsafe {
            (
                &*(d as *const Class),
                &*(v as *const Class),
                &*(t as *const Class),
            )
        }
    }

    fn texture_cache() -> CFTypeRef {
        let device = fake(classes().1, 0, 0, 0);
        let mut cache = std::ptr::null();
        // SAFETY: a live device and out-parameter.
        let status = unsafe {
            CVMetalTextureCacheCreate(
                std::ptr::null(),
                std::ptr::null(),
                device,
                std::ptr::null(),
                &mut cache,
            )
        };
        assert_eq!(status, SUCCESS);
        objc::arc::release(device);
        cache
    }

    fn texture_from(
        cache: CFTypeRef,
        image: CFTypeRef,
        pixel_format: usize,
        width: usize,
        height: usize,
        plane: usize,
    ) -> (i32, CFTypeRef) {
        let mut out = 1 as CFTypeRef;
        // SAFETY: objects this test made and a live out-parameter.
        let status = unsafe {
            CVMetalTextureCacheCreateTextureFromImage(
                std::ptr::null(),
                cache,
                image,
                std::ptr::null(),
                pixel_format,
                width,
                height,
                plane,
                &mut out,
            )
        };
        (status, out)
    }

    /// A buffer whose plane `p`'s row `y`, byte `x` holds `p * 64 + y + x`.
    fn patterned(format: u32, width: usize, height: usize) -> CFTypeRef {
        let mut buffer = std::ptr::null();
        // SAFETY: a live out-parameter, then the buffer it was given, written
        // inside each plane's rows.
        unsafe {
            assert_eq!(
                super::super::CVPixelBufferCreate(
                    std::ptr::null(),
                    width,
                    height,
                    format,
                    std::ptr::null(),
                    &mut buffer
                ),
                SUCCESS
            );
            let state = super::super::state(buffer).unwrap();
            for (p, plane) in state.planes.iter().enumerate() {
                let row = plane.width * plane_bytes_per_element(format, p).unwrap();
                for y in 0..plane.height {
                    for x in 0..row {
                        *plane.address.add(y * plane.row_bytes + x) = (p * 64 + y + x) as u8;
                    }
                }
            }
        }
        buffer
    }

    fn expected(plane: usize, width: usize, height: usize) -> Vec<u8> {
        (0..height)
            .flat_map(|y| (0..width).map(move |x| (plane * 64 + y + x) as u8))
            .collect()
    }

    fn texture_of(made: CFTypeRef) -> &'static Fake {
        // SAFETY: a CVMetalTexture this test made.
        let texture = unsafe { CVMetalTextureGetTexture(made) };
        assert!(!texture.is_null());
        body(texture)
    }

    fn written(fake: &Fake) -> &Vec<u8> {
        // SAFETY: the fake's own vector.
        unsafe { &*fake.bytes }
    }

    fn release(object: CFTypeRef) {
        // SAFETY: a reference the test holds.
        unsafe { corefoundation::api::CFRelease(object) };
    }

    #[test]
    fn luma_plane_becomes_an_r8_texture_with_its_bytes() {
        let cache = texture_cache();
        let image = patterned(NV12_VIDEO, 6, 4);
        let (status, made) = texture_from(cache, image, R8_UNORM, 6, 4, 0);
        assert_eq!(status, SUCCESS);
        // SAFETY: objects this test made.
        unsafe {
            assert_eq!(
                corefoundation::api::CFGetTypeID(made),
                CVMetalTextureGetTypeID()
            );
            assert_eq!(
                corefoundation::api::CFGetTypeID(cache),
                CVMetalTextureCacheGetTypeID()
            );
        }
        let texture = texture_of(made);
        assert_eq!(
            (texture.pixel_format, texture.width, texture.height),
            (R8_UNORM, 6, 4)
        );
        assert_eq!(written(texture), &expected(0, 6, 4));
        // The texture holds the buffer.
        release(image);
        assert!(super::super::state(image).is_some());
        release(made);
        release(cache);
    }

    #[test]
    fn nv12_chroma_plane_is_rg8_at_half_size() {
        let cache = texture_cache();
        let image = patterned(NV12_FULL, 7, 5);
        let (status, made) = texture_from(cache, image, RG8_UNORM, 4, 3, 1);
        assert_eq!(status, SUCCESS);
        let texture = texture_of(made);
        assert_eq!(
            (texture.pixel_format, texture.width, texture.height),
            (RG8_UNORM, 4, 3)
        );
        assert_eq!(written(texture), &expected(1, 8, 3));
        release(made);
        release(image);
        release(cache);
    }

    #[test]
    fn y420_cr_plane_is_plane_two() {
        let cache = texture_cache();
        let image = patterned(I420_VIDEO, 8, 8);
        let (status, made) = texture_from(cache, image, R8_UNORM, 4, 4, 2);
        assert_eq!(status, SUCCESS);
        assert_eq!(written(texture_of(made)), &expected(2, 4, 4));
        release(made);
        release(image);
        release(cache);
    }

    #[test]
    fn bgra_plane_zero_is_bgra8() {
        let cache = texture_cache();
        let image = patterned(BGRA, 3, 2);
        let (status, made) = texture_from(cache, image, BGRA8_UNORM, 3, 2, 0);
        assert_eq!(status, SUCCESS);
        let texture = texture_of(made);
        assert_eq!(texture.pixel_format, BGRA8_UNORM);
        assert_eq!(written(texture), &expected(0, 12, 2));
        release(made);
        release(image);
        release(cache);
    }

    #[test]
    fn wrong_size_format_or_plane_is_refused() {
        let cache = texture_cache();
        let nv12 = patterned(NV12_VIDEO, 8, 8);
        let bgra = patterned(BGRA, 8, 8);
        for (image, pixel_format, width, height, plane) in [
            (nv12, R8_UNORM, 8, 4, 0),
            (nv12, RG8_UNORM, 8, 8, 0),
            (nv12, R8_UNORM, 4, 4, 1),
            (nv12, RG8_UNORM, 4, 4, 2),
            (nv12, 999, 8, 8, 0),
            (bgra, BGRA8_UNORM, 8, 8, 1),
            (bgra, R8_UNORM, 8, 8, 0),
        ] {
            let (status, made) = texture_from(cache, image, pixel_format, width, height, plane);
            assert_eq!(
                status, INVALID_ARGUMENT,
                "format {pixel_format} {width}x{height} plane {plane}"
            );
            assert!(made.is_null());
        }
        assert_eq!(
            texture_from(std::ptr::null(), nv12, R8_UNORM, 8, 8, 0).0,
            INVALID_ARGUMENT
        );
        assert_eq!(
            texture_from(cache, cache, R8_UNORM, 8, 8, 0).0,
            INVALID_ARGUMENT
        );
        let mut out = std::ptr::null();
        // SAFETY: a live out-parameter.
        let status = unsafe {
            CVMetalTextureCacheCreate(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                &mut out,
            )
        };
        assert_eq!(status, INVALID_ARGUMENT, "a cache needs a device");
        release(nv12);
        release(bgra);
        release(cache);
    }
}
