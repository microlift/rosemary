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
        "copyFromBuffer:sourceOffset:sourceBytesPerRow:sourceBytesPerImage:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:" => copy_buffer_to_texture,
            c"v120@0:8@16Q24Q32Q40{MTLSize=QQQ}48@72Q80Q88{MTLOrigin=QQQ}96";
        "synchronizeResource:" => synchronize_resource, c"v24@0:8@16";
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
    let blit = unsafe { object::body::<NativeBlitEncoder>(receiver) };
    let encoder_device = unsafe { command_device(blit.command, std::ptr::null()) };
    if from.device != to.device || from.device != encoder_device {
        gpu::trace(
            "native blit buffers must belong to the command encoder's Metal device".to_string(),
        );
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

/// Copy between a texture region and linear buffer memory, the shared body
/// of both buffer/texture blits. Every guest value is validated first: Apple
/// Metal raises an Objective-C exception, which cannot unwind into the guest,
/// for a region or buffer range it cannot copy.
struct LinearCopy {
    texture: Id,
    slice: u64,
    level: u64,
    origin: encoder::MtlOrigin,
    size: encoder::MtlSize,
    buffer: Id,
    offset: u64,
    bytes_per_row: u64,
    bytes_per_image: u64,
}

impl LinearCopy {
    /// The host texture and buffer, if the copy is one Metal performs.
    fn validate(&self, blit: &NativeBlitEncoder) -> Option<(HostId, HostId)> {
        if !object::is_kind(self.texture, c"RosemaryNativeMTLTexture")
            || !object::is_kind(self.buffer, c"RosemaryNativeMTLBuffer")
        {
            gpu::trace("native buffer/texture blit requires native texture and buffer".to_string());
            return None;
        }
        let texture = unsafe { object::body::<texture::NativeTexture>(self.texture) };
        let buffer = unsafe { object::body::<NativeBuffer>(self.buffer) };
        let device = unsafe { command_device(blit.command, std::ptr::null()) };
        if texture.device != device || buffer.device != device {
            gpu::trace(
                "native buffer/texture blit resources must belong to encoder device".to_string(),
            );
            return None;
        }
        let query = |host: HostId, selector: &CStr| unsafe { host_send_u64(host, sel(selector)) };
        let pixel_format = query(texture.host, c"pixelFormat");
        let kind = query(texture.host, c"textureType");
        let layout = linear_layout(
            pixel_format,
            self.size,
            self.bytes_per_row,
            self.bytes_per_image,
        );
        let in_texture = layout.is_some_and(|layout| {
            region_in_texture(
                texture.host,
                kind,
                self.slice,
                self.level,
                self.origin,
                self.size,
                layout.block,
            )
        });
        let in_buffer = layout.is_some_and(|layout| {
            self.offset % layout.block_bytes == 0
                && self
                    .offset
                    .checked_add(layout.bytes)
                    .is_some_and(|end| end <= query(buffer.host, c"length"))
        });
        if query(texture.host, c"sampleCount") != 1 || !in_texture || !in_buffer {
            gpu::trace_once(format!(
                "native buffer/texture blit rejected: format={pixel_format} type={kind} \
                 level={} slice={} origin=({},{},{}) size={}x{}x{} offset={} \
                 bytesPerRow={} bytesPerImage={}",
                self.level,
                self.slice,
                self.origin.x,
                self.origin.y,
                self.origin.z,
                self.size.width,
                self.size.height,
                self.size.depth,
                self.offset,
                self.bytes_per_row,
                self.bytes_per_image,
            ));
            return None;
        }
        Some((texture.host, buffer.host))
    }
}

/// How a buffer lays out `size` texels of a format: whole compression blocks
/// per row, `bytes_per_row` apart, and images `bytes_per_image` apart.
#[derive(Clone, Copy)]
struct LinearLayout {
    block: (u64, u64),
    block_bytes: u64,
    /// The bytes the copy touches, from its first block to its last.
    bytes: u64,
}

fn linear_layout(
    pixel_format: u64,
    size: encoder::MtlSize,
    bytes_per_row: u64,
    bytes_per_image: u64,
) -> Option<LinearLayout> {
    let format = gpu::pixel_format(pixel_format);
    // A packed depth/stencil texture copies one aspect at a time, which only
    // the `options:` variants can name.
    if matches!(
        format,
        ash::vk::Format::UNDEFINED
            | ash::vk::Format::D24_UNORM_S8_UINT
            | ash::vk::Format::D32_SFLOAT_S8_UINT
    ) || size.width == 0
        || size.height == 0
        || size.depth == 0
    {
        return None;
    }
    let (block_width, block_height) = gpu::block_extent(format);
    let block = (u64::from(block_width), u64::from(block_height));
    let block_bytes = u64::from(gpu::texel_size(format));
    let row_bytes = size.width.div_ceil(block.0).checked_mul(block_bytes)?;
    let rows = size.height.div_ceil(block.1);
    let image_bytes = (rows - 1)
        .checked_mul(bytes_per_row)?
        .checked_add(row_bytes)?;
    if bytes_per_row < row_bytes {
        return None;
    }
    // Metal reads bytesPerImage only when the copy spans several images.
    let bytes = if size.depth == 1 {
        image_bytes
    } else {
        if bytes_per_image < rows.checked_mul(bytes_per_row)? {
            return None;
        }
        (size.depth - 1)
            .checked_mul(bytes_per_image)?
            .checked_add(image_bytes)?
    };
    Some(LinearLayout {
        block,
        block_bytes,
        bytes,
    })
}

/// Whether `origin`/`size` at `slice` and `level` lies inside the texture,
/// counting the whole compression blocks a mip tail is copied in.
fn region_in_texture(
    host: HostId,
    kind: u64,
    slice: u64,
    level: u64,
    origin: encoder::MtlOrigin,
    size: encoder::MtlSize,
    block: (u64, u64),
) -> bool {
    let query = |selector: &CStr| unsafe { host_send_u64(host, sel(selector)) };
    let Some(slices) = texture::slice_count(host, kind) else {
        return false;
    };
    if level >= query(c"mipmapLevelCount") || slice >= slices {
        return false;
    }
    let depth_ok = if kind == MTL_TEXTURE_TYPE_3D {
        within_mip_axis(query(c"depth"), level, 1, origin.z, size.depth)
    } else {
        origin.z == 0 && size.depth == 1
    };
    depth_ok
        && within_mip_axis(query(c"width"), level, block.0, origin.x, size.width)
        && within_mip_axis(query(c"height"), level, block.1, origin.y, size.height)
}

#[allow(clippy::too_many_arguments)]
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
    let blit = unsafe { object::body::<NativeBlitEncoder>(receiver) };
    let copy = LinearCopy {
        texture,
        slice,
        level,
        origin,
        size,
        buffer,
        offset,
        bytes_per_row,
        bytes_per_image,
    };
    let _pool = HostPool::new();
    let Some((host_texture, host_buffer)) = copy.validate(blit) else {
        return;
    };
    unsafe {
        host_send_void_texture_to_buffer(
            blit.host,
            sel(c"copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:"),
            host_texture, slice, level, origin, size, host_buffer, offset,
            bytes_per_row, bytes_per_image,
        );
    }
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn copy_buffer_to_texture(
    receiver: Id,
    _sel: Sel,
    buffer: Id,
    offset: u64,
    bytes_per_row: u64,
    bytes_per_image: u64,
    size: encoder::MtlSize,
    texture: Id,
    slice: u64,
    level: u64,
    origin: encoder::MtlOrigin,
) {
    let blit = unsafe { object::body::<NativeBlitEncoder>(receiver) };
    let copy = LinearCopy {
        texture,
        slice,
        level,
        origin,
        size,
        buffer,
        offset,
        bytes_per_row,
        bytes_per_image,
    };
    let _pool = HostPool::new();
    let Some((host_texture, host_buffer)) = copy.validate(blit) else {
        return;
    };
    unsafe {
        host_send_void_buffer_to_texture(
            blit.host,
            sel(c"copyFromBuffer:sourceOffset:sourceBytesPerRow:sourceBytesPerImage:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:"),
            host_buffer, offset, bytes_per_row, bytes_per_image, size,
            host_texture, slice, level, origin,
        );
    }
}

/// Only a managed resource has a CPU copy to bring up to date; for any other
/// storage mode there is nothing to synchronize.
unsafe extern "C" fn synchronize_resource(receiver: Id, _sel: Sel, resource: Id) {
    let host_resource = if object::is_kind(resource, c"RosemaryNativeMTLBuffer") {
        let buffer = unsafe { object::body::<NativeBuffer>(resource) };
        (buffer.device, buffer.host)
    } else if object::is_kind(resource, c"RosemaryNativeMTLTexture") {
        let texture = unsafe { object::body::<texture::NativeTexture>(resource) };
        (texture.device, texture.host)
    } else {
        gpu::trace("native Metal can only synchronize a native resource".to_string());
        return;
    };
    let blit = unsafe { object::body::<NativeBlitEncoder>(receiver) };
    if host_resource.0 != unsafe { command_device(blit.command, std::ptr::null()) } {
        gpu::trace("native synchronized resource belongs to another device".to_string());
        return;
    }
    let _pool = HostPool::new();
    if unsafe { host_send_u64(host_resource.1, sel(c"storageMode")) } != MTL_STORAGE_MODE_MANAGED {
        return;
    }
    unsafe { host_send_void_id(blit.host, sel(c"synchronizeResource:"), host_resource.1) };
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
    let (block_width, block_height) = gpu::block_extent(gpu::pixel_format(from_format));
    let block = (u64::from(block_width), u64::from(block_height));
    if from_format != to_format
        || query(from.host, c"sampleCount") != 1
        || query(to.host, c"sampleCount") != 1
        || size.width == 0
        || size.height == 0
        || size.depth == 0
        || !region_in_texture(
            from.host,
            from_type,
            source_slice,
            source_level,
            source_origin,
            size,
            block,
        )
        || !region_in_texture(
            to.host,
            to_type,
            destination_slice,
            destination_level,
            destination_origin,
            size,
            block,
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

    const MTL_PIXEL_FORMAT_R32_FLOAT: u64 = 55;
    const MTL_PIXEL_FORMAT_BC1_RGBA: u64 = 130;
    const MTL_PIXEL_FORMAT_DEPTH32_FLOAT_STENCIL8: u64 = 260;

    fn size(width: u64, height: u64, depth: u64) -> encoder::MtlSize {
        encoder::MtlSize {
            width,
            height,
            depth,
        }
    }

    #[test]
    fn linear_layout_spans_padded_rows_but_only_the_last_rows_own_bytes() {
        let layout = |format, extent, row, image| {
            linear_layout(format, extent, row, image).map(|layout| layout.bytes)
        };
        let rgba = MTL_PIXEL_FORMAT_RGBA8_UNORM;
        assert_eq!(layout(rgba, size(2, 2, 1), 256, 0), Some(256 + 8));
        assert_eq!(
            layout(MTL_PIXEL_FORMAT_R32_FLOAT, size(3, 1, 1), 12, 0),
            Some(12)
        );
        // One image ignores bytesPerImage; several need room for each row.
        assert_eq!(layout(rgba, size(2, 2, 2), 256, 512), Some(512 + 256 + 8));
        assert_eq!(layout(rgba, size(2, 2, 2), 256, 511), None);
        // A row stride shorter than one row of texels.
        assert_eq!(layout(rgba, size(2, 2, 1), 7, 0), None);
        // 6x6 BC1 is 2x2 blocks of eight bytes.
        assert_eq!(
            layout(MTL_PIXEL_FORMAT_BC1_RGBA, size(6, 6, 1), 16, 0),
            Some(32)
        );
        // A packed depth/stencil copy needs an aspect option; empty copies
        // and unknown formats copy nothing Metal accepts.
        assert_eq!(
            layout(MTL_PIXEL_FORMAT_DEPTH32_FLOAT_STENCIL8, size(1, 1, 1), 8, 0),
            None
        );
        assert_eq!(layout(rgba, size(0, 2, 1), 256, 0), None);
        assert_eq!(layout(u64::MAX, size(1, 1, 1), 256, 0), None);
    }

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
