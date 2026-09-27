use super::*;

/// Every parameter of a blit is an `NSUInteger` the guest chose, and `ash`
/// hands each one straight to the driver. A copy which names a level, a slice,
/// a region or a buffer range the resource does not have is undefined
/// behaviour in Vulkan, so each entry point below checks its own against the
/// resources first and records nothing when the check fails - which is what
/// Metal's own validation layer does, and the alternative is a corrupted
/// driver command or a panic across `extern "C"`.
fn refuse(what: &str, reason: impl std::fmt::Display) {
    gpu::trace(format!("{what}: {reason}, so the copy is not recorded"));
}

/// Whether a buffer holds `length` bytes from `offset`.
fn holds(buffer: &Buffer, offset: u64, length: u64) -> bool {
    offset
        .checked_add(length)
        .is_some_and(|end| end <= buffer.inner.length)
}

/// The buffer layout one Metal buffer/texture copy names, as Vulkan spells it:
/// the row length and image height in texels, and the bytes the copy reaches.
///
/// The two texel counts are exactly what this crate has always passed; only
/// the byte total is new, and it exists so the caller can check the range
/// against the buffer before `ash` hands the copy to the driver. `None` where
/// the guest's numbers overflow or do not fit a Vulkan `uint32_t`.
fn linear_bytes(
    image: &crate::gpu::GpuImage,
    size: &MtlSize,
    bytes_per_row: u64,
    bytes_per_image: u64,
) -> Option<(u64, u64, u64)> {
    let (block_width, block_height) = gpu::block_extent(image.format);
    let texel = u64::from(gpu::texel_size(image.format)).max(1);
    let row_texels = if bytes_per_row == 0 {
        size.width
    } else {
        bytes_per_row / texel * u64::from(block_width)
    };
    let rows = if bytes_per_image == 0 || bytes_per_row == 0 {
        size.height
    } else {
        bytes_per_image / bytes_per_row * u64::from(block_height)
    };
    u32::try_from(row_texels).ok()?;
    u32::try_from(rows).ok()?;
    // What the copy above addresses: the slices before the last one are a
    // whole `rows` of blocks each, and the last reaches only the region's own
    // height. Each step is checked, because every input is a guest
    // `NSUInteger`.
    let row_bytes = row_texels
        .max(size.width)
        .div_ceil(u64::from(block_width))
        .checked_mul(texel)?;
    let slice_rows = rows.max(size.height).div_ceil(u64::from(block_height));
    let last_rows = size.height.max(1).div_ceil(u64::from(block_height));
    let total = size
        .depth
        .max(1)
        .checked_sub(1)?
        .checked_mul(slice_rows)?
        .checked_add(last_rows)?
        .checked_mul(row_bytes)?;
    Some((row_texels, rows, total))
}

pub(super) unsafe extern "C" fn copy_buffer_to_buffer(
    receiver: Id,
    _sel: Sel,
    source: Id,
    source_offset: u64,
    destination: Id,
    destination_offset: u64,
    size: u64,
) {
    if source.is_null() || destination.is_null() || size == 0 {
        return;
    }
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    let command = encoder.command();
    // SAFETY: only buffers of this crate's reach a blit encoder.
    let (from, to) = unsafe {
        (
            object::body::<Buffer>(source),
            object::body::<Buffer>(destination),
        )
    };
    if !holds(from, source_offset, size) || !holds(to, destination_offset, size) {
        return refuse(
            "copyFromBuffer:toBuffer:",
            format!(
                "{size} bytes at {source_offset} of a {}-byte buffer and {destination_offset} of \
                 a {}-byte one",
                from.inner.length, to.inner.length
            ),
        );
    }
    let region = vk::BufferCopy::default()
        .src_offset(source_offset)
        .dst_offset(destination_offset)
        .size(size);
    // SAFETY: recording into this command buffer.
    unsafe {
        command.gpu.device.cmd_copy_buffer(
            command.command_buffer,
            from.inner.buffer,
            to.inner.buffer,
            &[region],
        )
    };
    command.retain(source);
    command.retain(destination);
}

#[allow(clippy::too_many_arguments)]
pub(super) unsafe extern "C" fn copy_buffer_to_texture(
    receiver: Id,
    _sel: Sel,
    source: Id,
    source_offset: u64,
    bytes_per_row: u64,
    bytes_per_image: u64,
    size: MtlSize,
    destination: Id,
    slice: u64,
    level: u64,
    origin: MtlOrigin,
) {
    if source.is_null() || destination.is_null() {
        return;
    }
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    let command = encoder.command();
    // SAFETY: only resources of this crate's reach a blit encoder.
    let (from, to) = unsafe {
        (
            object::body::<Buffer>(source),
            object::body::<Texture>(destination),
        )
    };
    let checked = match to.image.region(gpu::Region {
        level,
        slice,
        origin: [origin.x, origin.y, origin.z],
        size: [size.width, size.height, size.depth],
    }) {
        Ok(checked) => checked,
        Err(reason) => return refuse("copyFromBuffer:toTexture:", reason),
    };
    let Some((row_texels, rows, total)) =
        linear_bytes(&to.image, &size, bytes_per_row, bytes_per_image)
    else {
        return refuse(
            "copyFromBuffer:toTexture:",
            format!("{bytes_per_row} bytes a row and {bytes_per_image} an image"),
        );
    };
    if !holds(from, source_offset, total) {
        return refuse(
            "copyFromBuffer:toTexture:",
            format!(
                "{total} bytes at {source_offset} of a {}-byte buffer",
                from.inner.length
            ),
        );
    }
    let region = vk::BufferImageCopy::default()
        .buffer_offset(source_offset)
        .buffer_row_length(row_texels as u32)
        .buffer_image_height(rows as u32)
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: to.image.aspect,
            mip_level: checked.level,
            base_array_layer: checked.slice,
            layer_count: 1,
        })
        .image_offset(vk::Offset3D {
            x: checked.origin[0] as i32,
            y: checked.origin[1] as i32,
            z: checked.origin[2] as i32,
        })
        .image_extent(vk::Extent3D {
            width: checked.size[0],
            height: checked.size[1],
            depth: checked.size[2],
        });
    // SAFETY: recording into this command buffer; the image is in GENERAL.
    unsafe {
        command.gpu.device.cmd_copy_buffer_to_image(
            command.command_buffer,
            from.inner.buffer,
            to.image.image,
            vk::ImageLayout::GENERAL,
            &[region],
        )
    };
    command.retain(source);
    command.retain(destination);
}

#[allow(clippy::too_many_arguments)]
pub(super) unsafe extern "C" fn copy_texture_to_buffer(
    receiver: Id,
    _sel: Sel,
    source: Id,
    slice: u64,
    level: u64,
    origin: MtlOrigin,
    size: MtlSize,
    destination: Id,
    destination_offset: u64,
    bytes_per_row: u64,
    bytes_per_image: u64,
) {
    if source.is_null() || destination.is_null() {
        return;
    }
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    let command = encoder.command();
    // SAFETY: only resources of this crate's reach a blit encoder.
    let (from, to) = unsafe {
        (
            object::body::<Texture>(source),
            object::body::<Buffer>(destination),
        )
    };
    let checked = match from.image.region(gpu::Region {
        level,
        slice,
        origin: [origin.x, origin.y, origin.z],
        size: [size.width, size.height, size.depth],
    }) {
        Ok(checked) => checked,
        Err(reason) => return refuse("copyFromTexture:toBuffer:", reason),
    };
    let Some((row_texels, rows, total)) =
        linear_bytes(&from.image, &size, bytes_per_row, bytes_per_image)
    else {
        return refuse(
            "copyFromTexture:toBuffer:",
            format!("{bytes_per_row} bytes a row and {bytes_per_image} an image"),
        );
    };
    if !holds(to, destination_offset, total) {
        return refuse(
            "copyFromTexture:toBuffer:",
            format!(
                "{total} bytes at {destination_offset} of a {}-byte buffer",
                to.inner.length
            ),
        );
    }
    let region = vk::BufferImageCopy::default()
        .buffer_offset(destination_offset)
        .buffer_row_length(row_texels as u32)
        .buffer_image_height(rows as u32)
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: from.image.aspect,
            mip_level: checked.level,
            base_array_layer: checked.slice,
            layer_count: 1,
        })
        .image_offset(vk::Offset3D {
            x: checked.origin[0] as i32,
            y: checked.origin[1] as i32,
            z: checked.origin[2] as i32,
        })
        .image_extent(vk::Extent3D {
            width: checked.size[0],
            height: checked.size[1],
            depth: checked.size[2],
        });
    // SAFETY: recording into this command buffer; the image is in GENERAL.
    unsafe {
        command.gpu.device.cmd_copy_image_to_buffer(
            command.command_buffer,
            from.image.image,
            vk::ImageLayout::GENERAL,
            to.inner.buffer,
            &[region],
        )
    };
    command.retain(source);
    command.retain(destination);
}

#[allow(clippy::too_many_arguments)]
pub(super) unsafe extern "C" fn copy_texture_to_texture(
    receiver: Id,
    _sel: Sel,
    source: Id,
    source_slice: u64,
    source_level: u64,
    source_origin: MtlOrigin,
    size: MtlSize,
    destination: Id,
    destination_slice: u64,
    destination_level: u64,
    destination_origin: MtlOrigin,
) {
    if source.is_null() || destination.is_null() {
        return;
    }
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    let command = encoder.command();
    // SAFETY: only textures of this crate's reach a blit encoder.
    let (from, to) = unsafe {
        (
            object::body::<Texture>(source),
            object::body::<Texture>(destination),
        )
    };
    let source_region = gpu::Region {
        level: source_level,
        slice: source_slice,
        origin: [source_origin.x, source_origin.y, source_origin.z],
        size: [size.width, size.height, size.depth],
    };
    let destination_region = gpu::Region {
        level: destination_level,
        slice: destination_slice,
        origin: [
            destination_origin.x,
            destination_origin.y,
            destination_origin.z,
        ],
        size: [size.width, size.height, size.depth],
    };
    let (source_checked, destination_checked) = match (
        from.image.region(source_region),
        to.image.region(destination_region),
    ) {
        (Ok(source), Ok(destination)) => (source, destination),
        (Err(reason), _) | (_, Err(reason)) => {
            return refuse("copyFromTexture:toTexture:", reason);
        }
    };
    let region = vk::ImageCopy::default()
        .src_subresource(vk::ImageSubresourceLayers {
            aspect_mask: from.image.aspect,
            mip_level: source_checked.level,
            base_array_layer: source_checked.slice,
            layer_count: 1,
        })
        .src_offset(vk::Offset3D {
            x: source_checked.origin[0] as i32,
            y: source_checked.origin[1] as i32,
            z: source_checked.origin[2] as i32,
        })
        .dst_subresource(vk::ImageSubresourceLayers {
            aspect_mask: to.image.aspect,
            mip_level: destination_checked.level,
            base_array_layer: destination_checked.slice,
            layer_count: 1,
        })
        .dst_offset(vk::Offset3D {
            x: destination_checked.origin[0] as i32,
            y: destination_checked.origin[1] as i32,
            z: destination_checked.origin[2] as i32,
        })
        // One extent serves both images, and each clamps to its own level.
        .extent(vk::Extent3D {
            width: source_checked.size[0].min(destination_checked.size[0]),
            height: source_checked.size[1].min(destination_checked.size[1]),
            depth: source_checked.size[2].min(destination_checked.size[2]),
        });
    // SAFETY: recording into this command buffer; both images are in GENERAL.
    unsafe {
        command.gpu.device.cmd_copy_image(
            command.command_buffer,
            from.image.image,
            vk::ImageLayout::GENERAL,
            to.image.image,
            vk::ImageLayout::GENERAL,
            &[region],
        )
    };
    command.retain(source);
    command.retain(destination);
}

/// `-generateMipmapsForTexture:`: a chain of halving blits, which is what
/// Metal's own implementation does for a colour texture.
pub(super) unsafe extern "C" fn generate_mipmaps(receiver: Id, _sel: Sel, texture: Id) {
    if texture.is_null() {
        return;
    }
    // SAFETY: an encoder of this crate's.
    let encoder = unsafe { object::body::<Encoder>(receiver) };
    let command = encoder.command();
    // SAFETY: only a texture of this crate's reaches a blit encoder.
    let image = unsafe { object::body::<Texture>(texture) };
    let device = &command.gpu.device;
    let vk_command = command.command_buffer;
    let mut width = image.image.width as i32;
    let mut height = image.image.height as i32;
    for level in 1..image.image.levels {
        let next_width = (width / 2).max(1);
        let next_height = (height / 2).max(1);
        let blit = vk::ImageBlit::default()
            .src_subresource(vk::ImageSubresourceLayers {
                aspect_mask: image.image.aspect,
                mip_level: level - 1,
                base_array_layer: 0,
                layer_count: image.image.layers,
            })
            .src_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D {
                    x: width,
                    y: height,
                    z: 1,
                },
            ])
            .dst_subresource(vk::ImageSubresourceLayers {
                aspect_mask: image.image.aspect,
                mip_level: level,
                base_array_layer: 0,
                layer_count: image.image.layers,
            })
            .dst_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D {
                    x: next_width,
                    y: next_height,
                    z: 1,
                },
            ]);
        // SAFETY: recording into this command buffer; the image is in GENERAL.
        unsafe {
            device.cmd_blit_image(
                vk_command,
                image.image.image,
                vk::ImageLayout::GENERAL,
                image.image.image,
                vk::ImageLayout::GENERAL,
                &[blit],
                vk::Filter::LINEAR,
            )
        };
        encoder.barrier();
        width = next_width;
        height = next_height;
    }
    command.retain(texture);
}
