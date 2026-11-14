use super::*;

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct Region {
    pub origin: [u64; 3],
    pub size: [u64; 3],
}

pub(super) struct NativeTexture {
    pub(super) host: HostId,
    pub(super) device: Id,
}

pub(super) fn install() {
    let class = object::register::<NativeTexture>(c"RosemaryNativeMTLTexture");
    methods!(class, {
        "device" => device, c"@16@0:8";
        "width" => width, c"Q16@0:8";
        "height" => height, c"Q16@0:8";
        "depth" => depth, c"Q16@0:8";
        "pixelFormat" => pixel_format, c"Q16@0:8";
        "textureType" => texture_type, c"Q16@0:8";
        "storageMode" => storage_mode, c"Q16@0:8";
        "usage" => usage, c"Q16@0:8";
        "mipmapLevelCount" => mipmap_level_count, c"Q16@0:8";
        "sampleCount" => sample_count, c"Q16@0:8";
        "arrayLength" => array_length, c"Q16@0:8";
        "replaceRegion:mipmapLevel:withBytes:bytesPerRow:" => replace_region,
            c"v88@0:8{MTLRegion={MTLOrigin=QQQ}{MTLSize=QQQ}}16Q64^v72Q80";
        "replaceRegion:mipmapLevel:slice:withBytes:bytesPerRow:bytesPerImage:" => replace_region_slice,
            c"v104@0:8{MTLRegion={MTLOrigin=QQQ}{MTLSize=QQQ}}16Q64Q72^v80Q88Q96";
        "getBytes:bytesPerRow:bytesPerImage:fromRegion:mipmapLevel:slice:" => get_bytes_slice,
            c"v104@0:8^v16Q24Q32{MTLRegion={MTLOrigin=QQQ}{MTLSize=QQQ}}40Q88Q96";
        "getBytes:bytesPerRow:fromRegion:mipmapLevel:" => get_bytes,
            c"v88@0:8^v16Q24{MTLRegion={MTLOrigin=QQQ}{MTLSize=QQQ}}32Q80";
        "dealloc" => dealloc, c"v16@0:8";
    });
}

/// Wrap a retained host `id<MTLTexture>` without allowing a host Objective-C
/// pointer to enter the guest runtime. Takes ownership of one host retain.
pub(super) fn wrap_host_texture(host: HostId, device: Id) -> Id {
    if host.is_null() {
        return std::ptr::null_mut();
    }
    objc::arc::retain(device);
    let guest = object::make(c"RosemaryNativeMTLTexture", NativeTexture { host, device });
    if guest.is_null() {
        unsafe { host_release(host) };
        objc::arc::release(device);
    }
    guest
}

pub(super) unsafe extern "C" fn new_texture(receiver: Id, _sel: Sel, descriptor: Id) -> Id {
    if !object::is_kind(descriptor, c"MTLTextureDescriptor") {
        gpu::trace("native Metal requires an MTLTextureDescriptor".to_string());
        return std::ptr::null_mut();
    }
    let described = *unsafe { object::body::<resource::TextureDescriptor>(descriptor) };
    // Apple's Metal framework raises an uncaught Objective-C assertion for
    // impossible texture dimensions instead of returning nil. Validate
    // guest-controlled sizes before constructing a host descriptor. The 2D
    // 16K and 3D 2K ceilings are the limits of the supported Apple Metal
    // devices; additional device-specific restrictions remain the host's job.
    let maximum_dimension = if described.texture_type == 7 {
        2048
    } else {
        16384
    };
    if described.width == 0
        || described.height == 0
        || described.depth == 0
        || described.width > maximum_dimension
        || described.height > maximum_dimension
        || described.depth > maximum_dimension
    {
        gpu::trace_once(format!(
            "native Metal rejected invalid texture dimensions {}x{}x{} (type {})",
            described.width, described.height, described.depth, described.texture_type,
        ));
        return std::ptr::null_mut();
    }
    let _pool = HostPool::new();
    let class = unsafe { host_get_class(c"MTLTextureDescriptor".as_ptr()) };
    if class.is_null() {
        return std::ptr::null_mut();
    }
    let host_descriptor = unsafe { host_send_id(class, sel(c"new")) };
    if host_descriptor.is_null() {
        return std::ptr::null_mut();
    }
    for (selector, value) in [
        (c"setTextureType:".as_ref(), described.texture_type),
        (c"setPixelFormat:".as_ref(), described.pixel_format),
        (c"setWidth:".as_ref(), described.width),
        (c"setHeight:".as_ref(), described.height),
        (c"setDepth:".as_ref(), described.depth),
        (
            c"setMipmapLevelCount:".as_ref(),
            described.mipmap_level_count,
        ),
        (c"setSampleCount:".as_ref(), described.sample_count),
        (c"setArrayLength:".as_ref(), described.array_length),
        (c"setStorageMode:".as_ref(), described.storage_mode),
        (c"setUsage:".as_ref(), described.usage),
    ] {
        unsafe { host_send_void_u64(host_descriptor, sel(selector), value) };
    }
    let host = unsafe {
        host_send_id_id(
            object::body::<NativeDevice>(receiver).host,
            sel(c"newTextureWithDescriptor:"),
            host_descriptor,
        )
    };
    unsafe { host_release(host_descriptor) };
    if host.is_null() {
        return std::ptr::null_mut();
    }
    wrap_host_texture(host, receiver)
}

unsafe extern "C" fn device(receiver: Id, _sel: Sel) -> Id {
    unsafe { object::body::<NativeTexture>(receiver) }.device
}

macro_rules! scalar {
    ($name:ident, $selector:literal) => {
        unsafe extern "C" fn $name(receiver: Id, _sel: Sel) -> u64 {
            let _pool = HostPool::new();
            let host = unsafe { object::body::<NativeTexture>(receiver) }.host;
            unsafe { host_send_u64(host, sel($selector)) }
        }
    };
}
scalar!(width, c"width");
scalar!(height, c"height");
scalar!(depth, c"depth");
scalar!(pixel_format, c"pixelFormat");
scalar!(texture_type, c"textureType");
scalar!(storage_mode, c"storageMode");
scalar!(usage, c"usage");
scalar!(mipmap_level_count, c"mipmapLevelCount");
scalar!(sample_count, c"sampleCount");
scalar!(array_length, c"arrayLength");

pub(super) unsafe extern "C" fn replace_region(
    receiver: Id,
    _sel: Sel,
    region: Region,
    level: u64,
    bytes: *const c_void,
    bytes_per_row: u64,
) {
    if bytes.is_null() || region.size[0] == 0 || region.size[1] == 0 {
        return;
    }
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeTexture>(receiver) }.host;
    unsafe {
        host_send_void_region_u64_ptr_u64(
            host,
            sel(c"replaceRegion:mipmapLevel:withBytes:bytesPerRow:"),
            region,
            level,
            bytes,
            bytes_per_row,
        );
    }
}

pub(super) unsafe extern "C" fn get_bytes(
    receiver: Id,
    _sel: Sel,
    bytes: *mut c_void,
    bytes_per_row: u64,
    region: Region,
    level: u64,
) {
    if bytes.is_null() || region.size[0] == 0 || region.size[1] == 0 {
        return;
    }
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeTexture>(receiver) }.host;
    unsafe {
        host_send_void_ptr_u64_region_u64(
            host,
            sel(c"getBytes:bytesPerRow:fromRegion:mipmapLevel:"),
            bytes,
            bytes_per_row,
            region,
            level,
        );
    }
}

unsafe extern "C" fn replace_region_slice(
    receiver: Id,
    _sel: Sel,
    region: Region,
    level: u64,
    slice: u64,
    bytes: *const c_void,
    bytes_per_row: u64,
    bytes_per_image: u64,
) {
    // Apple Metal ignores bytesPerImage for a one-image region, including a
    // selected 2D-array/cube slice. Roblox routinely passes zero there.
    if bytes.is_null()
        || region.size.contains(&0)
        || bytes_per_row == 0
        || (region.size[2] > 1 && bytes_per_image == 0)
    {
        let _pool = HostPool::new();
        let host = unsafe { object::body::<NativeTexture>(receiver) }.host;
        gpu::trace_once(format!(
            "native layered upload rejected: size={:?} bytesPerRow={bytes_per_row} bytesPerImage={bytes_per_image} mip={level} slice={slice} format={} kind={} bytesNull={}",
            region.size,
            unsafe { host_send_u64(host, sel(c"pixelFormat")) },
            unsafe { host_send_u64(host, sel(c"textureType")) },
            bytes.is_null(),
        ));
        return;
    }
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeTexture>(receiver) }.host;
    unsafe {
        host_send_void_region_u64_u64_ptr_u64_u64(
            host,
            sel(c"replaceRegion:mipmapLevel:slice:withBytes:bytesPerRow:bytesPerImage:"),
            region,
            level,
            slice,
            bytes,
            bytes_per_row,
            bytes_per_image,
        )
    };
}

unsafe extern "C" fn get_bytes_slice(
    receiver: Id,
    _sel: Sel,
    bytes: *mut c_void,
    bytes_per_row: u64,
    bytes_per_image: u64,
    region: Region,
    level: u64,
    slice: u64,
) {
    // Apple Metal ignores bytesPerImage for a one-image region, including a
    // selected 2D-array/cube slice. Roblox routinely passes zero there.
    if bytes.is_null()
        || region.size.contains(&0)
        || bytes_per_row == 0
        || (region.size[2] > 1 && bytes_per_image == 0)
    {
        gpu::trace("native layered texture readback received an empty region".to_string());
        return;
    }
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeTexture>(receiver) }.host;
    unsafe {
        host_send_void_ptr_u64_u64_region_u64_u64(
            host,
            sel(c"getBytes:bytesPerRow:bytesPerImage:fromRegion:mipmapLevel:slice:"),
            bytes,
            bytes_per_row,
            bytes_per_image,
            region,
            level,
            slice,
        )
    };
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let texture = unsafe { object::body::<NativeTexture>(receiver) };
    unsafe { host_release(texture.host) };
    objc::arc::release(texture.device);
    objc::arc::finalize(receiver);
}
