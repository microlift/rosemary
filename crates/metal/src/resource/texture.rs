use super::*;
use crate::constants::*;

pub(crate) struct Texture {
    pub gpu: &'static Gpu,
    pub descriptor: TextureDescriptor,
    pub image: GpuImage,
    /// The texture a texture view sees into, retained, or null for a texture
    /// which owns its image.
    pub parent: Id,
    /// One view per mip level and array slice a render pass has attached,
    /// made on first use. A texture usually has one and a blur pyramid has one
    /// per level, so this is a short list rather than a map.
    attachment_views: Vec<((u32, u32), vk::ImageView)>,
    /// A texture buffer's view of its buffer's memory, and that buffer,
    /// retained; null for a texture with an image.
    pub texel_view: vk::BufferView,
    buffer: Id,
}

impl Texture {
    /// The framebuffer attachment for one mip level and array slice.
    pub fn attachment_view(&mut self, level: u32, slice: u32) -> vk::ImageView {
        if let Some((_, view)) = self
            .attachment_views
            .iter()
            .find(|(at, _)| *at == (level, slice))
        {
            return *view;
        }
        match self.gpu.create_attachment_view(&self.image, level, slice) {
            Ok(view) => {
                self.attachment_views.push(((level, slice), view));
                view
            }
            Err(reason) => {
                gpu::trace(format!("render pass attachment view failed: {reason}"));
                self.image.view
            }
        }
    }

    /// Whether this is a view of a buffer's memory, which has no image.
    pub fn is_texture_buffer(&self) -> bool {
        self.texel_view != vk::BufferView::null()
    }

    /// The bytes `-[MTLDevice currentAllocatedSize]` counts this texture as.
    /// Read from the image rather than the descriptor, so that the figure a
    /// deallocation subtracts is the one its creation added.
    pub fn accounted(&self) -> i64 {
        if !self.parent.is_null() {
            return 0;
        }
        i64::from(gpu::texel_size(self.image.format))
            * i64::from(self.image.width)
            * i64::from(self.image.height)
            * i64::from(self.image.layers)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct TextureDescriptor {
    pub texture_type: u64,
    pub pixel_format: u64,
    pub width: u64,
    pub height: u64,
    pub depth: u64,
    pub mipmap_level_count: u64,
    pub sample_count: u64,
    pub array_length: u64,
    pub storage_mode: u64,
    pub usage: u64,
}

impl TextureDescriptor {
    fn fresh() -> Self {
        // `-[MTLTextureDescriptor init]`'s documented defaults.
        Self {
            texture_type: MTL_TEXTURE_TYPE_2D,
            pixel_format: MTL_PIXEL_FORMAT_INVALID,
            width: 1,
            height: 1,
            depth: 1,
            mipmap_level_count: 1,
            sample_count: 1,
            array_length: 1,
            storage_mode: MTL_STORAGE_MODE_MANAGED,
            usage: gpu::USAGE_SHADER_READ,
        }
    }

    /// The Vulkan image shape a Metal texture type asks for.
    pub(super) fn shape(&self) -> (vk::ImageType, vk::ImageViewType, u32, bool) {
        match self.texture_type {
            0 => (vk::ImageType::TYPE_1D, vk::ImageViewType::TYPE_1D, 1, false),
            1 => (
                vk::ImageType::TYPE_1D,
                vk::ImageViewType::TYPE_1D_ARRAY,
                self.array_length.max(1) as u32,
                false,
            ),
            3 | 8 => (
                vk::ImageType::TYPE_2D,
                vk::ImageViewType::TYPE_2D_ARRAY,
                self.array_length.max(1) as u32,
                false,
            ),
            5 => (vk::ImageType::TYPE_2D, vk::ImageViewType::CUBE, 6, true),
            6 => (
                vk::ImageType::TYPE_2D,
                vk::ImageViewType::CUBE_ARRAY,
                6 * self.array_length.max(1) as u32,
                true,
            ),
            7 => (vk::ImageType::TYPE_3D, vk::ImageViewType::TYPE_3D, 1, false),
            // 2, 4 and 9 are the plain two-dimensional cases.
            _ => (vk::ImageType::TYPE_2D, vk::ImageViewType::TYPE_2D, 1, false),
        }
    }
}

/// `MTLRegion`: an origin and a size, both three `NSUInteger`s.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Region {
    pub origin: [u64; 3],
    pub size: [u64; 3],
}

/// `-[MTLTexture dealloc]`, for the same reason as a buffer's.
pub(super) unsafe extern "C" fn texture_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a texture of this crate's.
    let texture = unsafe { object::body::<Texture>(receiver) };
    crate::account(-texture.accounted());
    heap::leave(receiver);
    for (_, view) in std::mem::take(&mut texture.attachment_views) {
        // SAFETY: the last reference is gone, so nothing can attach it again,
        // and a command buffer which did holds a reference until it completes.
        if !leaking() {
            unsafe { texture.gpu.device.destroy_image_view(view, None) };
        }
    }
    if texture.texel_view != vk::BufferView::null() {
        if !leaking() {
            // SAFETY: as above.
            unsafe {
                texture
                    .gpu
                    .device
                    .destroy_buffer_view(texture.texel_view, None)
            };
        }
        objc::arc::release(std::mem::replace(&mut texture.buffer, std::ptr::null_mut()));
        objc::arc::finalize(receiver);
        return;
    }
    let image = std::mem::replace(&mut texture.image, GpuImage::none());
    if !leaking() {
        if texture.parent.is_null() {
            texture.gpu.destroy_image(image);
        } else {
            texture.gpu.destroy_view(image);
        }
    }
    objc::arc::release(std::mem::replace(&mut texture.parent, std::ptr::null_mut()));
    objc::arc::finalize(receiver);
}

pub(crate) fn new_texture(gpu: &'static Gpu, descriptor: TextureDescriptor) -> Id {
    new_texture_in(gpu, descriptor, None)
}

/// A texture in memory of its own, or at an offset into a placement heap's.
pub(crate) fn new_texture_in(
    gpu: &'static Gpu,
    descriptor: TextureDescriptor,
    placed: Option<(std::sync::Arc<gpu::Placement>, u64)>,
) -> Id {
    let Some(description) = image_description(&descriptor) else {
        gpu::trace(format!(
            "newTextureWithDescriptor: has no Vulkan form for MTLPixelFormat {}",
            descriptor.pixel_format
        ));
        return std::ptr::null_mut();
    };
    let image = match gpu.create_image_in(&description, placed) {
        Ok(image) => image,
        Err(reason) => {
            gpu::trace(format!("newTextureWithDescriptor: failed: {reason}"));
            return std::ptr::null_mut();
        }
    };
    let texture = object::make(
        c"RosemaryMTLTexture",
        Texture {
            gpu,
            descriptor,
            image,
            parent: std::ptr::null_mut(),
            attachment_views: Vec::new(),
            texel_view: vk::BufferView::null(),
            buffer: std::ptr::null_mut(),
        },
    );
    if !texture.is_null() {
        // SAFETY: the texture this call just made.
        crate::account(unsafe { object::body::<Texture>(texture) }.accounted());
    }
    texture
}

/// The Vulkan image a texture descriptor asks for, or `None` for a pixel
/// format Vulkan has no form of.
pub(crate) fn image_description(descriptor: &TextureDescriptor) -> Option<ImageDescription> {
    let format = gpu::pixel_format(descriptor.pixel_format);
    if format == vk::Format::UNDEFINED {
        return None;
    }
    let (image_type, view_type, layers, cube) = descriptor.shape();
    Some(ImageDescription {
        image_type,
        view_type,
        format,
        width: descriptor.width.max(1) as u32,
        height: descriptor.height.max(1) as u32,
        depth: if image_type == vk::ImageType::TYPE_3D {
            descriptor.depth.max(1) as u32
        } else {
            1
        },
        levels: descriptor.mipmap_level_count.max(1) as u32,
        layers,
        samples: descriptor.sample_count,
        usage: descriptor.usage,
        cube,
        components: gpu::pixel_format_components(descriptor.pixel_format),
    })
}

/// Wrap an existing image as a Metal texture, for presentation, whose images
/// are created and destroyed by the layer rather than by `-newTexture...`.
pub(crate) fn drawable_texture(gpu: &'static Gpu, image: GpuImage, pixel_format: u64) -> Id {
    let descriptor = TextureDescriptor {
        texture_type: 2,
        pixel_format,
        width: u64::from(image.width),
        height: u64::from(image.height),
        depth: 1,
        mipmap_level_count: 1,
        sample_count: 1,
        array_length: 1,
        storage_mode: 2,
        usage: gpu::USAGE_RENDER_TARGET | gpu::USAGE_SHADER_READ,
    };
    object::make(
        c"RosemaryMTLTexture",
        Texture {
            gpu,
            descriptor,
            image,
            parent: std::ptr::null_mut(),
            attachment_views: Vec::new(),
            texel_view: vk::BufferView::null(),
            buffer: std::ptr::null_mut(),
        },
    )
}

pub(super) fn descriptor_field(descriptor: &TextureDescriptor, sel: Sel) -> u64 {
    // SAFETY: a selector the runtime owns for the life of the process.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"textureType" => descriptor.texture_type,
        b"pixelFormat" => descriptor.pixel_format,
        b"width" => descriptor.width,
        b"height" => descriptor.height,
        b"depth" => descriptor.depth,
        b"mipmapLevelCount" => descriptor.mipmap_level_count,
        b"sampleCount" => descriptor.sample_count,
        b"arrayLength" => descriptor.array_length,
        b"storageMode" => descriptor.storage_mode,
        b"usage" => descriptor.usage,
        _ => unreachable!("only registered descriptor getters reach this method"),
    }
}

pub(super) fn set_descriptor_field(descriptor: &mut TextureDescriptor, sel: Sel, value: u64) {
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"setTextureType:" => descriptor.texture_type = value,
        b"setPixelFormat:" => descriptor.pixel_format = value,
        b"setWidth:" => descriptor.width = value,
        b"setHeight:" => descriptor.height = value,
        b"setDepth:" => descriptor.depth = value,
        b"setMipmapLevelCount:" => descriptor.mipmap_level_count = value,
        b"setSampleCount:" => descriptor.sample_count = value,
        b"setArrayLength:" => descriptor.array_length = value,
        b"setStorageMode:" => descriptor.storage_mode = value,
        b"setUsage:" => descriptor.usage = value,
        _ => unreachable!("only registered descriptor setters reach this method"),
    }
}

pub(super) unsafe extern "C" fn descriptor_value(receiver: Id, sel: Sel) -> u64 {
    // SAFETY: a texture descriptor of this crate's.
    descriptor_field(unsafe { object::body::<TextureDescriptor>(receiver) }, sel)
}

pub(super) unsafe extern "C" fn set_descriptor_value(receiver: Id, sel: Sel, value: u64) {
    set_descriptor_field(
        unsafe { object::body::<TextureDescriptor>(receiver) },
        sel,
        value,
    )
}

pub(super) unsafe extern "C" fn texture_value(receiver: Id, sel: Sel) -> u64 {
    // SAFETY: a texture of this crate's.
    descriptor_field(
        &unsafe { object::body::<Texture>(receiver) }.descriptor,
        sel,
    )
}

pub(super) unsafe extern "C" fn descriptor_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a freshly allocated texture descriptor.
    *unsafe { object::body::<TextureDescriptor>(receiver) } = TextureDescriptor::fresh();
    receiver
}

pub(super) unsafe extern "C" fn descriptor_copy(
    receiver: Id,
    _sel: Sel,
    _zone: *mut std::ffi::c_void,
) -> Id {
    // SAFETY: a texture descriptor of this crate's, containing only values.
    let descriptor = *unsafe { object::body::<TextureDescriptor>(receiver) };
    // FIXME (ACCURACY): Copies always use the base class and bypass subclass initializers.
    object::make(c"MTLTextureDescriptor", descriptor)
}

/// The level count `mipmapped:YES` asks for: the full chain down to 1x1.
fn full_chain(width: u64, height: u64) -> u64 {
    u64::from(64 - width.max(height).max(1).leading_zeros())
}

fn autoreleased_descriptor(descriptor: TextureDescriptor) -> Id {
    let object = object::make(c"MTLTextureDescriptor", descriptor);
    // SAFETY: a fresh object this call owns.
    unsafe { corefoundation::api::CFAutorelease(object as corefoundation::base::CFTypeRef) as Id }
}

/// `+[MTLTextureDescriptor texture2DDescriptorWithPixelFormat:width:height:mipmapped:]`.
pub(super) unsafe extern "C" fn texture_2d_descriptor(
    _class: Id,
    _sel: Sel,
    pixel_format: u64,
    width: u64,
    height: u64,
    mipmapped: bool,
) -> Id {
    autoreleased_descriptor(TextureDescriptor {
        pixel_format,
        width,
        height,
        mipmap_level_count: if mipmapped {
            full_chain(width, height)
        } else {
            1
        },
        ..TextureDescriptor::fresh()
    })
}

/// `+[MTLTextureDescriptor textureCubeDescriptorWithPixelFormat:size:mipmapped:]`.
pub(super) unsafe extern "C" fn texture_cube_descriptor(
    _class: Id,
    _sel: Sel,
    pixel_format: u64,
    size: u64,
    mipmapped: bool,
) -> Id {
    autoreleased_descriptor(TextureDescriptor {
        texture_type: MTL_TEXTURE_TYPE_CUBE,
        pixel_format,
        width: size,
        height: size,
        mipmap_level_count: if mipmapped { full_chain(size, size) } else { 1 },
        ..TextureDescriptor::fresh()
    })
}

/// `-[MTLTextureDescriptor resourceOptions]`: the storage mode is the only
/// part of `MTLResourceOptions` a descriptor here keeps.
pub(super) unsafe extern "C" fn descriptor_resource_options(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: a texture descriptor of this crate's.
    unsafe { object::body::<TextureDescriptor>(receiver) }.storage_mode << 4
}

/// `-[MTLTextureDescriptor setResourceOptions:]`. The CPU cache mode and
/// hazard tracking bits describe nothing a Vulkan image here has.
pub(super) unsafe extern "C" fn set_descriptor_resource_options(
    receiver: Id,
    _sel: Sel,
    options: u64,
) {
    // SAFETY: a texture descriptor of this crate's.
    unsafe { object::body::<TextureDescriptor>(receiver) }.storage_mode = (options >> 4) & 0xf;
}

/// `-[MTLTexture replaceRegion:mipmapLevel:withBytes:bytesPerRow:]`: the
/// first slice, one image.
pub(super) unsafe extern "C" fn replace_region_level(
    receiver: Id,
    sel: Sel,
    region: Region,
    level: u64,
    bytes: *const u8,
    bytes_per_row: u64,
) {
    unsafe { replace_region(receiver, sel, region, level, 0, bytes, bytes_per_row, 0) }
}

/// `-[MTLTexture replaceRegion:mipmapLevel:slice:withBytes:bytesPerRow:bytesPerImage:]`.
///
/// Metal copies immediately, outside any command buffer. The bytes are staged
/// at once and the copy is recorded ahead of the next submission, which is the
/// first thing that can observe the texture: see `gpu::deferred`.
pub(super) unsafe extern "C" fn replace_region(
    receiver: Id,
    _sel: Sel,
    region: Region,
    level: u64,
    slice: u64,
    bytes: *const u8,
    bytes_per_row: u64,
    bytes_per_image: u64,
) {
    if bytes.is_null() {
        return;
    }
    // SAFETY: a texture of this crate's.
    let texture = unsafe { object::body::<Texture>(receiver) };
    if texture.is_texture_buffer() {
        // Its bytes are its buffer's, which a guest writes through
        // `-contents`; an image copy here would name a null `VkImage`.
        return gpu::trace("replaceRegion: of a texture buffer is not supported");
    }
    // Every number below this line came from the guest. A region outside the
    // texture, or a size which overflows, is a Metal programming error and
    // becomes a diagnostic here rather than an invalid `vkCmdCopyBufferToImage`
    // or a panic across `extern "C"`.
    let checked = match texture.image.region(gpu::Region {
        level,
        slice,
        origin: region.origin,
        size: region.size,
    }) {
        Ok(checked) => checked,
        Err(reason) => return gpu::trace(format!("replaceRegion: {reason}")),
    };
    let [width, height, depth] = region.size.map(|axis| axis.max(1));
    let (block_width, block_height) = gpu::block_extent(texture.image.format);
    let texel = u64::from(gpu::texel_size(texture.image.format));
    let rows = height.div_ceil(u64::from(block_height));
    let row_bytes = if bytes_per_row != 0 {
        bytes_per_row
    } else {
        width.div_ceil(u64::from(block_width)) * texel
    };
    // Metal reads `bytesPerImage` only for a copy with more than one slice to
    // it, and says to pass zero for a 2D texture; a guest which passes the
    // whole mip level's size anyway must not make this stage - and read from
    // the guest pointer - more than the region.
    //
    // This matters because the encoding for this selector did not describe
    // `bytesPerImage` at all until recently, so the argument never arrived.
    let image_bytes = if bytes_per_image != 0 && depth > 1 {
        bytes_per_image
    } else {
        row_bytes * rows
    };
    let Some(total) = image_bytes.checked_mul(depth) else {
        return gpu::trace(format!(
            "replaceRegion: {image_bytes} bytes an image over {depth} images overflows"
        ));
    };
    if total == 0 {
        return;
    }
    // The staging buffer is `total` bytes and the copy below reads
    // `row_length * image_height * depth` of it, so a `bytesPerImage` which
    // does not cover its own rows would have Vulkan read past the allocation.
    let row_length = row_bytes / texel.max(1) * u64::from(block_width);
    let image_height = rows * u64::from(block_height);
    if image_bytes < row_bytes * rows
        || u32::try_from(row_length).is_err()
        || u32::try_from(image_height).is_err()
    {
        return gpu::trace(format!(
            "replaceRegion: {bytes_per_row} bytes a row and {bytes_per_image} an image do not \
             describe a {width}x{height}x{depth} region"
        ));
    }
    // SAFETY: the guest promised `total` readable bytes. They are copied
    // into staging before this returns, as Metal copies them.
    let source = unsafe { std::slice::from_raw_parts(bytes, total as usize) };
    // A buffer offset of a copy into an image must be a whole number of texel
    // blocks, and of four bytes for a depth/stencil one.
    let alignment = lcm(texel.max(1), 16);
    let image = texture.image.image;
    let aspect = texture.image.aspect;
    let heap = texture.image.placement.as_deref();
    let result = texture.gpu.defer(|gpu, batch| {
        let (buffer, offset) = batch.stage(gpu, source, alignment)?;
        batch.touch_subresource(gpu, image, heap, checked.level, checked.slice);
        let copy = vk::BufferImageCopy::default()
            .buffer_offset(offset)
            .buffer_row_length(row_length as u32)
            .buffer_image_height(image_height as u32)
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: aspect,
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
        // SAFETY: recording into the batch's command buffer, which the batch
        // lock gives this thread alone; the image is in GENERAL from creation.
        unsafe {
            gpu.device.cmd_copy_buffer_to_image(
                batch.command_buffer(),
                buffer,
                image,
                vk::ImageLayout::GENERAL,
                &[copy],
            )
        };
        Ok(())
    });
    if let Err(reason) = result {
        gpu::trace(format!("replaceRegion: copy failed: {reason}"));
    }
}

/// `-[MTLTexture getBytes:bytesPerRow:fromRegion:mipmapLevel:]`: the
/// first slice, one image.
pub(super) unsafe extern "C" fn get_bytes(
    receiver: Id,
    sel: Sel,
    bytes: *mut u8,
    bytes_per_row: u64,
    region: Region,
    level: u64,
) {
    unsafe { get_bytes_slice(receiver, sel, bytes, bytes_per_row, 0, region, level, 0) }
}

// A failed post-submit Vulkan wait must retain both the GPU staging buffer
// and the source MTLTexture. The caller's guest destination is only touched
// after confirmed completion, never while the GPU may still be writing.
struct TextureReadbackOwner {
    gpu: &'static Gpu,
    staging: Option<gpu::GpuBuffer>,
    texture: usize,
}

impl Drop for TextureReadbackOwner {
    fn drop(&mut self) {
        if let Some(staging) = self.staging.take() {
            self.gpu.release_staging_buffer(staging);
        }
        objc::arc::release(self.texture as Id);
    }
}

/// `-[MTLTexture getBytes:bytesPerRow:bytesPerImage:fromRegion:mipmapLevel:slice:]`.
///
/// Metal copies on the spot and leaves ordering against GPU writes to the
/// guest, which must have waited for them. The copy here is submitted and
/// waited for at once, behind the deferred uploads `Gpu::submit` flushes, so
/// a `-replaceRegion:` made before this call is read back.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe extern "C" fn get_bytes_slice(
    receiver: Id,
    _sel: Sel,
    bytes: *mut u8,
    bytes_per_row: u64,
    bytes_per_image: u64,
    region: Region,
    level: u64,
    slice: u64,
) {
    if bytes.is_null() {
        return;
    }
    // SAFETY: a texture of this crate's.
    let texture = unsafe { object::body::<Texture>(receiver) };
    if texture.is_texture_buffer() {
        // As for `-replaceRegion:`: there is no image to copy from.
        return gpu::trace("getBytes: of a texture buffer is not supported".to_string());
    }
    let image = &texture.image;
    if image.samples != vk::SampleCountFlags::TYPE_1 {
        return gpu::trace("getBytes: of a multisampled texture".to_string());
    }
    // Vulkan copies one aspect at a time and Metal defines no interleaved
    // layout for both, so a combined depth/stencil read is refused.
    if image
        .aspect
        .contains(vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL)
    {
        return gpu::trace(format!(
            "getBytes: of a combined depth/stencil texture ({:?})",
            image.format
        ));
    }
    let checked = match image.region(gpu::Region {
        level,
        slice,
        origin: region.origin,
        size: region.size,
    }) {
        Ok(checked) => checked,
        Err(reason) => return gpu::trace(format!("getBytes: {reason}")),
    };
    let (block_width, block_height) = gpu::block_extent(image.format);
    let texel = u64::from(gpu::texel_size(image.format)).max(1);
    // The staging copy is tightly packed; the guest's strides apply only when
    // the rows are copied out, so a stride which is not a whole number of
    // texels still works.
    let tight_row = u64::from(checked.size[0].div_ceil(block_width)) * texel;
    let rows = u64::from(checked.size[1].div_ceil(block_height));
    let images = u64::from(checked.size[2]);
    let tight_image = tight_row * rows;
    let row_stride = if bytes_per_row != 0 {
        bytes_per_row
    } else {
        tight_row
    };
    // As for `-replaceRegion:`, `bytesPerImage` only means something with
    // more than one image to the copy.
    let image_stride = if bytes_per_image != 0 && images > 1 {
        bytes_per_image
    } else {
        row_stride * rows
    };
    if row_stride < tight_row || image_stride < row_stride * rows {
        return gpu::trace(format!(
            "getBytes: {bytes_per_row} bytes a row and {bytes_per_image} an image do not \
             describe a {}x{}x{images} region",
            checked.size[0], checked.size[1]
        ));
    }
    let staging = match texture.gpu.acquire_staging_buffer(tight_image * images) {
        Ok(staging) => staging,
        Err(reason) => return gpu::trace(format!("getBytes: staging failed: {reason}")),
    };
    let copy = vk::BufferImageCopy::default()
        .buffer_offset(0)
        .buffer_row_length(0)
        .buffer_image_height(0)
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: image.aspect,
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
    let (vk_image, gpu) = (image.image, texture.gpu);
    let staging = std::sync::Arc::new(std::sync::Mutex::new(TextureReadbackOwner {
        gpu,
        staging: Some(staging),
        texture: objc::arc::retain(receiver) as usize,
    }));
    let result = gpu.one_shot_owned(
        std::sync::Arc::clone(&staging),
        |device, command_buffer, owned| {
            let buffer = owned.lock().unwrap().staging.as_ref().unwrap().buffer;
            let before = vk::MemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ);
            let after = vk::MemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::HOST_READ);
            // SAFETY: recording into the immediate command buffer, which the call
            // owns; every image this runtime makes stays in GENERAL.
            unsafe {
                device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[before],
                    &[],
                    &[],
                );
                device.cmd_copy_image_to_buffer(
                    command_buffer,
                    vk_image,
                    vk::ImageLayout::GENERAL,
                    buffer,
                    &[copy],
                );
                device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::HOST,
                    vk::DependencyFlags::empty(),
                    &[after],
                    &[],
                    &[],
                );
            }
        },
    );
    match result {
        Err(reason) => gpu::trace(format!("getBytes: copy failed: {reason}")),
        Ok(()) => {
            let contents = staging.lock().unwrap().staging.as_ref().unwrap().contents;
            if row_stride == tight_row && image_stride == tight_image {
                // SAFETY: as below, for a region whose strides leave no gaps.
                unsafe {
                    std::ptr::copy_nonoverlapping(contents, bytes, (tight_image * images) as usize)
                };
                return;
            }
            for image in 0..images {
                for row in 0..rows {
                    let from = (image * tight_image + row * tight_row) as usize;
                    let to = (image * image_stride + row * row_stride) as usize;
                    // SAFETY: the copy completed into coherent memory covering
                    // the tight layout, and the guest promised its region at
                    // its own strides.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            contents.add(from),
                            bytes.add(to),
                            tight_row as usize,
                        )
                    };
                }
            }
        }
    }
}

fn lcm(a: u64, b: u64) -> u64 {
    let gcd = {
        let (mut x, mut y) = (a, b);
        while y != 0 {
            (x, y) = (y, x % y);
        }
        x
    };
    a / gcd * b
}

/// `NSRange`, as the view methods take their level and slice ranges.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ViewRange {
    location: u64,
    length: u64,
}

/// `-newTextureViewWithPixelFormat:`: every level and slice, another format.
pub(super) unsafe extern "C" fn new_texture_view(receiver: Id, sel: Sel, pixel_format: u64) -> Id {
    // SAFETY: a texture of this crate's.
    let texture = unsafe { object::body::<Texture>(receiver) };
    let (levels, slices) = (texture.image.levels, texture.image.layers);
    let texture_type = texture.descriptor.texture_type;
    unsafe {
        new_texture_view_range(
            receiver,
            sel,
            pixel_format,
            texture_type,
            ViewRange {
                location: 0,
                length: u64::from(levels),
            },
            ViewRange {
                location: 0,
                length: u64::from(slices),
            },
        )
    }
}

/// `-newTextureViewWithPixelFormat:textureType:levels:slices:`.
///
/// The ranges are relative to the receiver, which may itself be a view.
pub(super) unsafe extern "C" fn new_texture_view_range(
    receiver: Id,
    _sel: Sel,
    pixel_format: u64,
    texture_type: u64,
    levels: ViewRange,
    slices: ViewRange,
) -> Id {
    texture_view(
        receiver,
        pixel_format,
        texture_type,
        levels,
        slices,
        IDENTITY_SWIZZLE,
    )
}

/// `MTLTextureSwizzleChannels`: each output channel's `MTLTextureSwizzle`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct SwizzleChannels {
    red: u8,
    green: u8,
    blue: u8,
    alpha: u8,
}

const MTL_TEXTURE_SWIZZLE_ZERO: u8 = 0;
const MTL_TEXTURE_SWIZZLE_ONE: u8 = 1;
const MTL_TEXTURE_SWIZZLE_RED: u8 = 2;
const MTL_TEXTURE_SWIZZLE_GREEN: u8 = 3;
const MTL_TEXTURE_SWIZZLE_BLUE: u8 = 4;
const MTL_TEXTURE_SWIZZLE_ALPHA: u8 = 5;

const IDENTITY_SWIZZLE: SwizzleChannels = SwizzleChannels {
    red: MTL_TEXTURE_SWIZZLE_RED,
    green: MTL_TEXTURE_SWIZZLE_GREEN,
    blue: MTL_TEXTURE_SWIZZLE_BLUE,
    alpha: MTL_TEXTURE_SWIZZLE_ALPHA,
};

/// `-newTextureViewWithPixelFormat:textureType:levels:slices:swizzle:`.
pub(super) unsafe extern "C" fn new_texture_view_swizzled(
    receiver: Id,
    _sel: Sel,
    pixel_format: u64,
    texture_type: u64,
    levels: ViewRange,
    slices: ViewRange,
    swizzle: SwizzleChannels,
) -> Id {
    texture_view(
        receiver,
        pixel_format,
        texture_type,
        levels,
        slices,
        swizzle,
    )
}

/// The swizzle applied over what the format itself maps each channel to,
/// or `None` for a channel value `MTLTextureSwizzle` does not have.
fn compose_swizzle(
    format: vk::ComponentMapping,
    swizzle: SwizzleChannels,
) -> Option<vk::ComponentMapping> {
    use vk::ComponentSwizzle as S;
    // A format's identity channel is the channel itself.
    let resolve = |mapped: S, own: S| if mapped == S::IDENTITY { own } else { mapped };
    let source = [
        resolve(format.r, S::R),
        resolve(format.g, S::G),
        resolve(format.b, S::B),
        resolve(format.a, S::A),
    ];
    let pick = |channel: u8| match channel {
        MTL_TEXTURE_SWIZZLE_ZERO => Some(S::ZERO),
        MTL_TEXTURE_SWIZZLE_ONE => Some(S::ONE),
        MTL_TEXTURE_SWIZZLE_RED..=MTL_TEXTURE_SWIZZLE_ALPHA => {
            Some(source[usize::from(channel - MTL_TEXTURE_SWIZZLE_RED)])
        }
        _ => None,
    };
    Some(vk::ComponentMapping {
        r: pick(swizzle.red)?,
        g: pick(swizzle.green)?,
        b: pick(swizzle.blue)?,
        a: pick(swizzle.alpha)?,
    })
}

fn texture_view(
    receiver: Id,
    pixel_format: u64,
    texture_type: u64,
    levels: ViewRange,
    slices: ViewRange,
    swizzle: SwizzleChannels,
) -> Id {
    // SAFETY: a texture of this crate's.
    let texture = unsafe { object::body::<Texture>(receiver) };
    if texture.is_texture_buffer() {
        gpu::trace("rejecting newTextureViewWithPixelFormat: of a texture buffer");
        return std::ptr::null_mut();
    }
    let format = gpu::pixel_format(pixel_format);
    if format == vk::Format::UNDEFINED {
        gpu::trace(format!(
            "newTextureViewWithPixelFormat: has no Vulkan form for MTLPixelFormat {pixel_format}"
        ));
        return std::ptr::null_mut();
    }
    let in_range = |range: ViewRange, count: u32| {
        range.length > 0
            && range
                .location
                .checked_add(range.length)
                .is_some_and(|end| end <= u64::from(count))
    };
    if !in_range(levels, texture.image.levels) || !in_range(slices, texture.image.layers) {
        gpu::trace(format!(
            "newTextureViewWithPixelFormat: levels {}+{} and slices {}+{} of a texture with {} \
             and {}",
            levels.location,
            levels.length,
            slices.location,
            slices.length,
            texture.image.levels,
            texture.image.layers
        ));
        return std::ptr::null_mut();
    }
    let descriptor = TextureDescriptor {
        texture_type,
        pixel_format,
        mipmap_level_count: levels.length,
        array_length: match texture_type {
            MTL_TEXTURE_TYPE_CUBE => 1,
            6 => (slices.length / 6).max(1),
            _ => slices.length,
        },
        ..texture.descriptor
    };
    let (_, view_type, _, _) = descriptor.shape();
    let Some(components) = compose_swizzle(gpu::pixel_format_components(pixel_format), swizzle)
    else {
        gpu::trace("newTextureViewWithPixelFormat: a swizzle channel is not an MTLTextureSwizzle");
        return std::ptr::null_mut();
    };
    let base_level = texture.image.base_level + levels.location as u32;
    let base_layer = texture.image.base_layer + slices.location as u32;
    let image = match texture.gpu.create_texture_view(
        &texture.image,
        format,
        view_type,
        base_level..base_level + levels.length as u32,
        base_layer..base_layer + slices.length as u32,
        components,
        gpu::shader_aspect(pixel_format),
    ) {
        Ok(image) => image,
        Err(reason) => {
            gpu::trace(format!("newTextureViewWithPixelFormat: failed: {reason}"));
            return std::ptr::null_mut();
        }
    };
    // A view of a view sees into the texture that owns the image.
    let owner = if texture.parent.is_null() {
        receiver
    } else {
        texture.parent
    };
    object::make(
        c"RosemaryMTLTexture",
        Texture {
            gpu: texture.gpu,
            descriptor,
            image,
            parent: objc::arc::retain(owner),
            attachment_views: Vec::new(),
            texel_view: vk::BufferView::null(),
            buffer: std::ptr::null_mut(),
        },
    )
}

pub(super) unsafe extern "C" fn texture_parent(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a texture of this crate's.
    unsafe { object::body::<Texture>(receiver) }.parent
}

/// `+[MTLTextureDescriptor textureBufferDescriptorWithPixelFormat:width:resourceOptions:usage:]`.
pub(super) unsafe extern "C" fn texture_buffer_descriptor(
    _class: Id,
    _sel: Sel,
    pixel_format: u64,
    width: u64,
    resource_options: u64,
    usage: u64,
) -> Id {
    let mut descriptor = TextureDescriptor {
        texture_type: MTL_TEXTURE_TYPE_TEXTURE_BUFFER,
        pixel_format,
        width,
        usage,
        ..TextureDescriptor::fresh()
    };
    descriptor.storage_mode = (resource_options >> 4) & 0xf;
    autoreleased_descriptor(descriptor)
}

/// `-[MTLBuffer newTextureWithDescriptor:offset:bytesPerRow:]`: a texture
/// buffer seeing `width` texels of the buffer from `offset`, which is a
/// Vulkan texel buffer view.
pub(super) unsafe extern "C" fn buffer_new_texture(
    receiver: Id,
    _sel: Sel,
    descriptor: Id,
    offset: u64,
    _bytes_per_row: u64,
) -> Id {
    if descriptor.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a buffer of this crate's, and `MTLTextureDescriptor` is this
    // crate's class.
    let buffer = unsafe { object::body::<Buffer>(receiver) };
    let descriptor = *unsafe { object::body::<TextureDescriptor>(descriptor) };
    if descriptor.texture_type != MTL_TEXTURE_TYPE_TEXTURE_BUFFER {
        gpu::trace(
            "newTextureWithDescriptor:offset:bytesPerRow: of a linear 2D texture is not supported",
        );
        return std::ptr::null_mut();
    }
    let format = gpu::pixel_format(descriptor.pixel_format);
    if format == vk::Format::UNDEFINED {
        gpu::trace(format!(
            "newTextureWithDescriptor:offset:bytesPerRow: has no Vulkan form for MTLPixelFormat {}",
            descriptor.pixel_format
        ));
        return std::ptr::null_mut();
    }
    let range = descriptor.width * u64::from(gpu::texel_size(format));
    if offset
        .checked_add(range)
        .is_none_or(|end| end > buffer.inner.length)
    {
        gpu::trace("newTextureWithDescriptor:offset:bytesPerRow: reaches past the buffer");
        return std::ptr::null_mut();
    }
    let gpu = buffer.gpu;
    // A binding has no stated extent, so the device sees the whole buffer.
    let info = vk::BufferViewCreateInfo::default()
        .buffer(buffer.device_buffer())
        .format(format)
        .offset(offset)
        .range(range);
    // SAFETY: a live buffer of this device's, created with texel buffer usage,
    // and a range checked to be inside it.
    let texel_view = match unsafe { gpu.device.create_buffer_view(&info, None) } {
        Ok(view) => view,
        Err(error) => {
            gpu::trace(format!("vkCreateBufferView failed with {error:?}"));
            return std::ptr::null_mut();
        }
    };
    object::make(
        c"RosemaryMTLTexture",
        Texture {
            gpu,
            descriptor,
            image: GpuImage {
                format,
                width: descriptor.width.max(1) as u32,
                ..GpuImage::none()
            },
            parent: std::ptr::null_mut(),
            attachment_views: Vec::new(),
            texel_view,
            buffer: objc::arc::retain(receiver),
        },
    )
}
