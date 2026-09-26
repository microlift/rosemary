use super::*;

/// A surface format to present in.
///
/// `B8G8R8A8_UNORM` first, because that is `MTLPixelFormatBGRA8Unorm` - what
/// a `CAMetalLayer` defaults to and what a guest almost always renders - so
/// the blit is a copy rather than a conversion. Anything else the surface
/// offers still works: `vkCmdBlitImage` converts, which is the whole reason
/// presentation is a blit and not a copy.
pub(super) fn choose_format(formats: &[vk::SurfaceFormatKHR]) -> Option<vk::SurfaceFormatKHR> {
    let wanted = |format: vk::Format| {
        formats.iter().copied().find(|candidate| {
            candidate.format == format && candidate.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
        })
    };
    wanted(vk::Format::B8G8R8A8_UNORM)
        .or_else(|| wanted(vk::Format::R8G8B8A8_UNORM))
        .or_else(|| formats.first().copied())
}

/// Prefer low-latency, tear-free presentation where the surface supports it.
///
/// `FIFO` is required by Vulkan, so it is the safe fallback even if a broken
/// implementation returns an empty list.
pub(super) fn choose_present_mode(modes: &[vk::PresentModeKHR]) -> vk::PresentModeKHR {
    if modes.contains(&vk::PresentModeKHR::MAILBOX) {
        vk::PresentModeKHR::MAILBOX
    } else {
        vk::PresentModeKHR::FIFO
    }
}

/// `OPAQUE` where the surface offers it, which is what a game's window is -
/// unless the window's corners are to be `see_through`, which takes
/// `PRE_MULTIPLIED` wherever it is offered.
pub(super) fn composite_alpha(
    capabilities: &vk::SurfaceCapabilitiesKHR,
    see_through: bool,
) -> vk::CompositeAlphaFlagsKHR {
    let wanted = see_through.then_some(vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED);
    for candidate in wanted.into_iter().chain([
        vk::CompositeAlphaFlagsKHR::OPAQUE,
        vk::CompositeAlphaFlagsKHR::INHERIT,
        vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
        vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
    ]) {
        if capabilities.supported_composite_alpha.contains(candidate) {
            return candidate;
        }
    }
    vk::CompositeAlphaFlagsKHR::OPAQUE
}

/// A view's frame as two offsets into an image of `extent`, clipped to it.
///
/// A view may hang off the edge of its window - AppKit clips it to the
/// superview - and a blit whose destination leaves the image is invalid
/// rather than clipped, so the clipping happens here. `None` is a frame
/// wholly outside the window, which has no pixels to blit rather than a
/// rectangle to blit into: a scrolled-away view is the ordinary way to get
/// one, and Vulkan has no empty blit region.
pub(super) fn clamp(
    frame: &appkit::view::Frame,
    extent: vk::Extent2D,
) -> Option<[vk::Offset3D; 2]> {
    let left = frame.x.clamp(0, extent.width as i32);
    let top = frame.y.clamp(0, extent.height as i32);
    let right = frame
        .x
        .saturating_add(frame.width as i32)
        .clamp(0, extent.width as i32);
    let bottom = frame
        .y
        .saturating_add(frame.height as i32)
        .clamp(0, extent.height as i32);
    (right > left && bottom > top).then_some([
        vk::Offset3D {
            x: left,
            y: top,
            z: 0,
        },
        vk::Offset3D {
            x: right,
            y: bottom,
            z: 1,
        },
    ])
}

/// The whole of an image's level zero, as a blit's pair of offsets.
pub(super) fn extent_of(image: &GpuImage) -> [vk::Offset3D; 2] {
    [
        vk::Offset3D { x: 0, y: 0, z: 0 },
        vk::Offset3D {
            x: image.width.max(1) as i32,
            y: image.height.max(1) as i32,
            z: 1,
        },
    ]
}

/// One colour mip level, which is all any image here has.
pub(super) fn whole_color() -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        mip_level: 0,
        base_array_layer: 0,
        layer_count: 1,
    }
}

/// # Safety
///
/// Recording into `command_buffer`, with both images belonging to `device`
/// and in the layouts named.
pub(super) unsafe fn blit(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    source: vk::Image,
    from: [vk::Offset3D; 2],
    target: vk::Image,
    target_layout: vk::ImageLayout,
    to: [vk::Offset3D; 2],
) {
    let region = vk::ImageBlit::default()
        .src_subresource(whole_color())
        .src_offsets(from)
        .dst_subresource(whole_color())
        .dst_offsets(to);
    // Linear, because a drawable is not always the size of the rectangle it
    // lands in: a layer whose `drawableSize` disagrees with its view is
    // scaled here rather than rejected, which is what Core Animation does
    // with a layer's `contentsGravity` left at its default.
    unsafe {
        device.cmd_blit_image(
            command_buffer,
            source,
            vk::ImageLayout::GENERAL,
            target,
            target_layout,
            &[region],
            vk::Filter::LINEAR,
        )
    };
}

/// # Safety
///
/// Recording into `command_buffer`, with `image` belonging to `device`.
pub(super) unsafe fn barrier(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    image: vk::Image,
    from: vk::ImageLayout,
    to: vk::ImageLayout,
    source_access: vk::AccessFlags,
    target_access: vk::AccessFlags,
) {
    let barrier = vk::ImageMemoryBarrier::default()
        .old_layout(from)
        .new_layout(to)
        .src_access_mask(source_access)
        .dst_access_mask(target_access)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        });
    unsafe {
        device.cmd_pipeline_barrier(
            command_buffer,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        )
    };
}

/// Put a freshly acquired swapchain image where a blit can write it.
///
/// From `UNDEFINED` rather than from `PRESENT_SRC_KHR`: whatever the image
/// held is the frame before last and every pixel of it is about to be
/// overwritten, and discarding it is what lets a tiler avoid reloading the
/// old contents.
///
/// # Safety
///
/// As [`barrier`].
pub(super) unsafe fn transition_to_transfer_dst(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    image: vk::Image,
) {
    unsafe {
        barrier(
            device,
            command_buffer,
            image,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::AccessFlags::empty(),
            vk::AccessFlags::TRANSFER_WRITE,
        )
    };
}

/// Make writes performed by the guest submission visible to the following
/// presentation command buffer's transfer read.
///
/// # Safety
///
/// As [`barrier`].
pub(super) unsafe fn prepare_source(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    image: vk::Image,
) {
    let barrier = vk::ImageMemoryBarrier::default()
        .old_layout(vk::ImageLayout::GENERAL)
        .new_layout(vk::ImageLayout::GENERAL)
        .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
        .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        });
    unsafe {
        device.cmd_pipeline_barrier(
            command_buffer,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        )
    };
}

/// # Safety
///
/// As [`barrier`].
pub(super) unsafe fn transition_to_present(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    image: vk::Image,
) {
    unsafe {
        barrier(
            device,
            command_buffer,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::PRESENT_SRC_KHR,
            vk::AccessFlags::TRANSFER_WRITE,
            vk::AccessFlags::empty(),
        )
    };
}
