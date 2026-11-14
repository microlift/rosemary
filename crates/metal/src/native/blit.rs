use super::*;

pub(super) struct NativeBlitEncoder {
    host: HostId,
    command: Id,
}

pub(super) fn install() {
    let class = object::register::<NativeBlitEncoder>(c"RosemaryNativeMTLBlitCommandEncoder");
    methods!(class, {
        "copyFromBuffer:sourceOffset:toBuffer:destinationOffset:size:" => copy_buffer_to_buffer,
            c"v56@0:8@16Q24@32Q40Q48";
        "copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:" => copy_texture_to_buffer,
            c"v120@0:8@16Q24Q32{MTLOrigin=QQQ}40{MTLSize=QQQ}64@88Q96Q104Q112";
        "copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:" => copy_texture_to_texture,
            c"v144@0:8@16Q24Q32{MTLOrigin=QQQ}40{MTLSize=QQQ}64@88Q96Q104{MTLOrigin=QQQ}112";
        "generateMipmapsForTexture:" => generate_mipmaps, c"v24@0:8@16";
        "endEncoding" => end_encoding, c"v16@0:8";
        "device" => device, c"@16@0:8";
        "dealloc" => dealloc, c"v16@0:8";
    });
}

pub(super) unsafe extern "C" fn new_blit_encoder(receiver: Id, _sel: Sel) -> Id {
    let _pool = HostPool::new();
    let command = unsafe { object::body::<NativeCommandBuffer>(receiver) };
    let host = unsafe { host_send_id(command.host, sel(c"blitCommandEncoder")) };
    if host.is_null() {
        return std::ptr::null_mut();
    }
    let host = unsafe { host_retain(host) };
    objc::arc::retain(receiver);
    let guest = object::make(
        c"RosemaryNativeMTLBlitCommandEncoder",
        NativeBlitEncoder {
            host,
            command: receiver,
        },
    );
    if guest.is_null() {
        unsafe { host_release(host) };
        objc::arc::release(receiver);
    }
    objc::arc::autorelease(guest)
}

unsafe extern "C" fn device(receiver: Id, _sel: Sel) -> Id {
    let command = unsafe { object::body::<NativeBlitEncoder>(receiver) }.command;
    unsafe { command_device(command, std::ptr::null()) }
}

unsafe extern "C" fn copy_buffer_to_buffer(
    receiver: Id,
    _sel: Sel,
    source: Id,
    source_offset: u64,
    destination: Id,
    destination_offset: u64,
    size: u64,
) {
    if !object::is_kind(source, c"RosemaryNativeMTLBuffer")
        || !object::is_kind(destination, c"RosemaryNativeMTLBuffer")
    {
        gpu::trace("native blit requires native buffer wrappers".to_string());
        return;
    }
    let from = unsafe { object::body::<NativeBuffer>(source) };
    let to = unsafe { object::body::<NativeBuffer>(destination) };
    if from.device != to.device {
        gpu::trace("native blit requires buffers on the same Metal device".to_string());
        return;
    }
    let _pool = HostPool::new();
    let from_len = unsafe { host_send_u64(from.host, sel(c"length")) };
    let to_len = unsafe { host_send_u64(to.host, sel(c"length")) };
    let in_bounds = source_offset
        .checked_add(size)
        .is_some_and(|end| end <= from_len)
        && destination_offset
            .checked_add(size)
            .is_some_and(|end| end <= to_len);
    if !in_bounds {
        gpu::trace("native blit buffer range is out of bounds".to_string());
        return;
    }
    let blit = unsafe { object::body::<NativeBlitEncoder>(receiver) };
    unsafe {
        host_send_void_id_u64_id_u64_u64(
            blit.host,
            sel(c"copyFromBuffer:sourceOffset:toBuffer:destinationOffset:size:"),
            from.host,
            source_offset,
            to.host,
            destination_offset,
            size,
        );
    }
}

/// Read back a 2D, single-sample RGBA8/BGRA8 render target to a shared
/// buffer. The narrow format and extent restrictions prevent forwarding an
/// unvalidated guest region into Apple's GPU driver.
unsafe extern "C" fn copy_texture_to_buffer(
    receiver: Id,
    _sel: Sel,
    texture: Id,
    slice: u64,
    level: u64,
    origin: encoder::MtlOrigin,
    size: encoder::MtlSize,
    buffer: Id,
    offset: u64,
    bytes_per_row: u64,
    bytes_per_image: u64,
) {
    if !object::is_kind(texture, c"RosemaryNativeMTLTexture")
        || !object::is_kind(buffer, c"RosemaryNativeMTLBuffer")
    {
        gpu::trace("native texture readback requires native texture and buffer".to_string());
        return;
    }
    let source = unsafe { object::body::<texture::NativeTexture>(texture) };
    let destination = unsafe { object::body::<NativeBuffer>(buffer) };
    let blit = unsafe { object::body::<NativeBlitEncoder>(receiver) };
    let command_device = unsafe { command_device(blit.command, std::ptr::null()) };
    if source.device != command_device || destination.device != command_device {
        gpu::trace("native texture readback resources must belong to encoder device".to_string());
        return;
    }
    let _pool = HostPool::new();
    let pixel_format = unsafe { host_send_u64(source.host, sel(c"pixelFormat")) };
    let format_supported = matches!(
        pixel_format,
        MTL_PIXEL_FORMAT_RGBA8_UNORM | MTL_PIXEL_FORMAT_BGRA8_UNORM
    );
    let texture_width = unsafe { host_send_u64(source.host, sel(c"width")) };
    let texture_height = unsafe { host_send_u64(source.host, sel(c"height")) };
    let texture_type = unsafe { host_send_u64(source.host, sel(c"textureType")) };
    let samples = unsafe { host_send_u64(source.host, sel(c"sampleCount")) };
    let image_end = bytes_per_row.checked_mul(size.height);
    let byte_end = offset.checked_add(image_end.unwrap_or(u64::MAX));
    let dest_len = unsafe { host_send_u64(destination.host, sel(c"length")) };
    let within_texture = origin
        .x
        .checked_add(size.width)
        .is_some_and(|n| n <= texture_width)
        && origin
            .y
            .checked_add(size.height)
            .is_some_and(|n| n <= texture_height);
    if !format_supported
        || texture_type != MTL_TEXTURE_TYPE_2D
        || samples != 1
        || slice != 0
        || level != 0
        || origin.z != 0
        || size.depth != 1
        || size.width == 0
        || size.height == 0
        || !within_texture
        || bytes_per_row < size.width.saturating_mul(4)
        || bytes_per_image < image_end.unwrap_or(u64::MAX)
        || !byte_end.is_some_and(|end| end <= dest_len)
    {
        gpu::trace(
            "native texture readback unsupported region, format or buffer range".to_string(),
        );
        return;
    }
    unsafe {
        host_send_void_texture_to_buffer(
            blit.host,
            sel(c"copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:"),
            source.host, slice, level, origin, size, destination.host, offset,
            bytes_per_row, bytes_per_image,
        );
    }
}

/// Metal copies block-compressed mip tails in whole compression blocks.
/// A 1x1 or 2x2 BC mip still names the complete 4x4 block in a blit region,
/// just as the Vulkan path's `GpuImage::region` already permits. For ordinary
/// formats `block` is one and the bound is the logical mip dimension.
fn padded_mip_extent(base: u64, level: u64, block: u64) -> Option<u64> {
    let mip = (base >> level.min(63)).max(1);
    mip.checked_add(block.checked_sub(1)?)
        .map(|n| n / block * block)
}

/// The origin still has to name a logical texel, even when the copy size
/// covers the padding of a compressed block at the mip's edge. Metal copies
/// compressed blocks as whole units; starting *inside* padding is invalid.
fn within_mip_axis(base: u64, level: u64, block: u64, origin: u64, span: u64) -> bool {
    let mip = (base >> level.min(63)).max(1);
    origin < mip
        && origin % block == 0
        && origin
            .checked_add(span)
            .zip(padded_mip_extent(base, level, block))
            .is_some_and(|(end, bound)| end <= bound)
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn copy_texture_to_texture(
    receiver: Id,
    _sel: Sel,
    source: Id,
    source_slice: u64,
    source_level: u64,
    source_origin: encoder::MtlOrigin,
    size: encoder::MtlSize,
    destination: Id,
    destination_slice: u64,
    destination_level: u64,
    destination_origin: encoder::MtlOrigin,
) {
    if !object::is_kind(source, c"RosemaryNativeMTLTexture")
        || !object::is_kind(destination, c"RosemaryNativeMTLTexture")
    {
        gpu::trace("native texture blit requires native textures".to_string());
        return;
    }
    let from = unsafe { object::body::<texture::NativeTexture>(source) };
    let to = unsafe { object::body::<texture::NativeTexture>(destination) };
    let blit = unsafe { object::body::<NativeBlitEncoder>(receiver) };
    let device = unsafe { command_device(blit.command, std::ptr::null()) };
    if from.device != device || to.device != device {
        gpu::trace("native texture blit requires textures on the encoder device".to_string());
        return;
    }
    let _pool = HostPool::new();
    let query = |host: HostId, selector: &CStr| unsafe { host_send_u64(host, sel(selector)) };
    let from_format = query(from.host, c"pixelFormat");
    let to_format = query(to.host, c"pixelFormat");
    let from_type = query(from.host, c"textureType");
    let to_type = query(to.host, c"textureType");
    let supported_type = |kind| matches!(kind, 2 | 3 | 5 | 6 | 7);
    let (block_width, block_height) = gpu::block_extent(gpu::pixel_format(from_format));
    let (block_width, block_height) = (u64::from(block_width), u64::from(block_height));
    let slice_count = |host: HostId, kind: u64| match kind {
        2 | 7 => 1,
        5 => 6,
        6 => query(host, c"arrayLength").saturating_mul(6),
        _ => query(host, c"arrayLength"),
    };
    let region_ok =
        |host: HostId, kind: u64, slice: u64, level: u64, origin: encoder::MtlOrigin| {
            let mipmaps = query(host, c"mipmapLevelCount");
            if level >= mipmaps || slice >= slice_count(host, kind) {
                return false;
            }
            (kind == 7 || (origin.z == 0 && size.depth == 1))
                && within_mip_axis(
                    query(host, c"width"),
                    level,
                    block_width,
                    origin.x,
                    size.width,
                )
                && within_mip_axis(
                    query(host, c"height"),
                    level,
                    block_height,
                    origin.y,
                    size.height,
                )
                && (kind != 7
                    || within_mip_axis(query(host, c"depth"), level, 1, origin.z, size.depth))
        };
    if from_format != to_format
        || !supported_type(from_type)
        || !supported_type(to_type)
        || query(from.host, c"sampleCount") != 1
        || query(to.host, c"sampleCount") != 1
        || size.width == 0
        || size.height == 0
        || size.depth == 0
        || !region_ok(
            from.host,
            from_type,
            source_slice,
            source_level,
            source_origin,
        )
        || !region_ok(
            to.host,
            to_type,
            destination_slice,
            destination_level,
            destination_origin,
        )
    {
        gpu::trace_once(format!(
            "native texture blit rejected: formats={from_format}/{to_format} types={from_type}/{to_type} samples={}/{} mips={source_level}/{destination_level} slices={source_slice}/{destination_slice} size={}x{}x{} sourceOrigin=({},{},{}) destinationOrigin=({},{},{})",
            query(from.host, c"sampleCount"),
            query(to.host, c"sampleCount"),
            size.width,
            size.height,
            size.depth,
            source_origin.x,
            source_origin.y,
            source_origin.z,
            destination_origin.x,
            destination_origin.y,
            destination_origin.z,
        ));
        return;
    }
    unsafe {
        host_send_void_texture_to_texture(
            blit.host,
            sel(c"copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:"),
            from.host, source_slice, source_level, source_origin, size,
            to.host, destination_slice, destination_level, destination_origin,
        )
    };
}

unsafe extern "C" fn generate_mipmaps(receiver: Id, _sel: Sel, guest: Id) {
    if !object::is_kind(guest, c"RosemaryNativeMTLTexture") {
        gpu::trace("native Metal mipmap generation requires a native texture".to_string());
        return;
    }
    let blit = unsafe { object::body::<NativeBlitEncoder>(receiver) };
    let tex = unsafe { object::body::<texture::NativeTexture>(guest) };
    if tex.device != unsafe { command_device(blit.command, std::ptr::null()) } {
        gpu::trace("native mipmap texture belongs to another Metal device".to_string());
        return;
    }
    let _pool = HostPool::new();
    unsafe { host_send_void_id(blit.host, sel(c"generateMipmapsForTexture:"), tex.host) };
}

unsafe extern "C" fn end_encoding(receiver: Id, _sel: Sel) {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeBlitEncoder>(receiver) }.host;
    unsafe { host_send_void(host, sel(c"endEncoding")) };
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let blit = unsafe { object::body::<NativeBlitEncoder>(receiver) };
    unsafe { host_release(blit.host) };
    objc::arc::release(blit.command);
    objc::arc::finalize(receiver);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_mip_tail_uses_the_full_block_while_uncompressed_stays_logical() {
        assert_eq!(gpu::block_extent(gpu::pixel_format(130)), (4, 4));
        assert_eq!(gpu::block_extent(gpu::pixel_format(134)), (4, 4));
        assert_eq!(padded_mip_extent(32, 4, 4), Some(4));
        assert_eq!(padded_mip_extent(32, 5, 4), Some(4));
        assert_eq!(padded_mip_extent(48, 2, 4), Some(12));
        assert_eq!(padded_mip_extent(24, 2, 4), Some(8));
        assert_eq!(padded_mip_extent(96, 3, 4), Some(12));
        assert_eq!(padded_mip_extent(48, 3, 4), Some(8));
        assert_eq!(padded_mip_extent(32, 4, 1), Some(2));
        assert_eq!(padded_mip_extent(24, 2, 1), Some(6));
        assert_eq!(padded_mip_extent(u64::MAX, 0, 4), None);
        assert!(within_mip_axis(32, 4, 4, 0, 4));
        assert!(within_mip_axis(48, 2, 4, 0, 12));
        assert!(within_mip_axis(24, 2, 4, 0, 8));
        assert!(!within_mip_axis(32, 4, 4, 2, 1)); // Origin inside padding
        assert!(!within_mip_axis(48, 2, 4, 4, 12)); // Extent past padded bound
        assert!(!within_mip_axis(48, 2, 4, 1, 4)); // Unaligned BC block
        assert!(!within_mip_axis(32, 4, 1, 0, 4)); // Uncompressed mip tail
        assert!(!within_mip_axis(32, 4, 4, u64::MAX, 4)); // Overflow
    }
}
