use super::*;

/// One `MTLRenderPassAttachmentDescriptor`.
#[derive(Clone, Copy)]
pub(crate) struct AttachmentDescriptor {
    pub texture: Id,
    pub level: u64,
    pub slice: u64,
    pub load_action: u64,
    pub store_action: u64,
    pub clear_color: [f64; 4],
    pub clear_depth: f64,
    pub clear_stencil: u32,
    pub resolve_texture: Id,
    pub resolve_level: u64,
    pub resolve_slice: u64,
    /// `MTLMultisampleDepthResolveFilter` on a depth attachment and
    /// `MTLMultisampleStencilResolveFilter` on a stencil one. A colour
    /// attachment has no filter and never reads it.
    pub resolve_filter: u64,
}

/// A `MTLStoreActionMultisampleResolve` from an attachment which has one
/// sample. Vulkan resolves only a multisampled attachment, so the destination
/// is filled with a copy instead, which is what a one-sample resolve is.
#[derive(Clone, Copy)]
pub(crate) struct PendingCopy {
    pub source: vk::Image,
    pub source_level: u32,
    pub source_slice: u32,
    pub destination: vk::Image,
    pub destination_level: u32,
    pub destination_slice: u32,
    pub width: u32,
    pub height: u32,
    pub aspect: vk::ImageAspectFlags,
}

/// One aspect copied through a buffer between a Metal depth/stencil texture
/// and the temporary combined Vulkan attachment used for a pass.
#[derive(Clone, Copy)]
pub(crate) struct DepthStencilBridge {
    pub original: vk::Image,
    pub original_level: u32,
    pub original_slice: u32,
    pub combined: vk::Image,
    pub width: u32,
    pub height: u32,
    pub aspect: vk::ImageAspectFlags,
    pub bytes_per_texel: u64,
    /// One transfer buffer reused for the inbound and outbound copies.
    pub staging: vk::Buffer,
    /// Preserve the original aspect before the pass. Clear and DontCare do
    /// not need an inbound copy.
    pub load: bool,
}

/// Record one direction of a separate-depth/stencil bridge.
///
/// The buffer is owned by the command until its submission retires. Going
/// through it is required because Vulkan image copies require the standalone
/// and combined depth/stencil formats to match.
pub(super) fn record_depth_stencil_bridge(
    command: &mut CommandBuffer,
    bridge: &mut DepthStencilBridge,
    into_combined: bool,
) -> Result<(), String> {
    let length = u64::from(bridge.width)
        .checked_mul(u64::from(bridge.height))
        .and_then(|pixels| pixels.checked_mul(bridge.bytes_per_texel))
        .ok_or("depth/stencil bridge size overflow")?;
    let buffer = if bridge.staging == vk::Buffer::null() {
        let staging = command.gpu.create_staging_buffer(length)?;
        let buffer = staging.buffer;
        command.staging.push(staging);
        bridge.staging = buffer;
        buffer
    } else {
        bridge.staging
    };
    let original = vk::ImageSubresourceLayers {
        aspect_mask: bridge.aspect,
        mip_level: bridge.original_level,
        base_array_layer: bridge.original_slice,
        layer_count: 1,
    };
    let combined = vk::ImageSubresourceLayers {
        aspect_mask: bridge.aspect,
        mip_level: 0,
        base_array_layer: 0,
        layer_count: 1,
    };
    let extent = vk::Extent3D {
        width: bridge.width,
        height: bridge.height,
        depth: 1,
    };
    let (source_image, source_subresource, destination_image, destination_subresource) =
        if into_combined {
            (bridge.original, original, bridge.combined, combined)
        } else {
            (bridge.combined, combined, bridge.original, original)
        };
    let from = vk::BufferImageCopy::default()
        .image_subresource(source_subresource)
        .image_extent(extent);
    let to = vk::BufferImageCopy::default()
        .image_subresource(destination_subresource)
        .image_extent(extent);
    let before = vk::MemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
        .dst_access_mask(vk::AccessFlags::TRANSFER_READ);
    let between = vk::MemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
        .dst_access_mask(vk::AccessFlags::TRANSFER_READ);
    let after = vk::MemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
        .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE);
    // SAFETY: recording outside a pass; both images stay in GENERAL and the
    // command owns the staging buffer until this submission retires.
    unsafe {
        command.gpu.device.cmd_pipeline_barrier(
            command.command_buffer,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[before],
            &[],
            &[],
        );
        command.gpu.device.cmd_copy_image_to_buffer(
            command.command_buffer,
            source_image,
            vk::ImageLayout::GENERAL,
            buffer,
            &[from],
        );
        command.gpu.device.cmd_pipeline_barrier(
            command.command_buffer,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[between],
            &[],
            &[],
        );
        command.gpu.device.cmd_copy_buffer_to_image(
            command.command_buffer,
            buffer,
            destination_image,
            vk::ImageLayout::GENERAL,
            &[to],
        );
        command.gpu.device.cmd_pipeline_barrier(
            command.command_buffer,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::DependencyFlags::empty(),
            &[after],
            &[],
            &[],
        );
    }
    Ok(())
}

pub(crate) const RENDER_TARGETS: usize = 8;

pub(crate) struct RenderPassDescriptor {
    pub colors: [AttachmentDescriptor; RENDER_TARGETS],
    pub depth: AttachmentDescriptor,
    pub stencil: AttachmentDescriptor,
    pub color_objects: Id,
    pub depth_object: Id,
    pub stencil_object: Id,
    pub visibility_result_buffer: Id,
    /// `MTLVisibilityResultType`.
    pub visibility_result_type: u64,
    pub render_target_array_length: u64,
}

/// `ROSEMARY_TRACE_PASSES=1`: each distinct render pass shape, once.
///
/// A frame's device time is spent in its passes, and a log which says only how
/// many there were cannot say which one to look at. The size, the sample count
/// and the load and store actions are what decide the cost of a pass before any
/// shader runs.
pub(super) fn trace_pass(key: &RenderPassKey, width: u32, height: u32) {
    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    #[static_init::dynamic]
    static ENABLED: bool = std::env::var_os("ROSEMARY_TRACE_PASSES").is_some();
    if !*ENABLED {
        return;
    }
    let line = format!(
        "render pass {width}x{height} samples {:?} colors {:?} resolves {:?} depth {:?}",
        key.samples, key.colors, key.color_resolves, key.depth
    );
    let mut seen = SEEN.lock().expect("render pass trace");
    if !seen.contains(&line) {
        seen.push(line.clone());
        gpu::trace(line);
    }
}

/// One dimension of a mip level, which is the level-zero one halved that many
/// times and never zero.
pub(super) fn level_extent(base: u32, level: u32) -> u32 {
    (base >> level.min(31)).max(1)
}

// `MTLLoadAction`.
const MTL_LOAD_ACTION_DONT_CARE: u64 = 0;
const MTL_LOAD_ACTION_LOAD: u64 = 1;
const MTL_LOAD_ACTION_CLEAR: u64 = 2;

// `MTLStoreAction`.
const MTL_STORE_ACTION_DONT_CARE: u64 = 0;
const MTL_STORE_ACTION_STORE: u64 = 1;
const MTL_STORE_ACTION_MULTISAMPLE_RESOLVE: u64 = 2;
const MTL_STORE_ACTION_STORE_AND_MULTISAMPLE_RESOLVE: u64 = 3;

// FIXME (ACCURACY): DONT_CARE semantics probably follow the same rule for loads as they
// do for stores that caused us lots of problems. TODO: should we just remove DONT_CARE here?
pub(super) fn load_op(metal: u64) -> vk::AttachmentLoadOp {
    match metal {
        MTL_LOAD_ACTION_LOAD => vk::AttachmentLoadOp::LOAD,
        MTL_LOAD_ACTION_CLEAR => vk::AttachmentLoadOp::CLEAR,
        _ => vk::AttachmentLoadOp::DONT_CARE,
    }
}

/// Whether an `MTLStoreAction` asks for a multisample resolve.
pub(super) fn resolves(metal: u64) -> bool {
    matches!(
        metal,
        MTL_STORE_ACTION_MULTISAMPLE_RESOLVE | MTL_STORE_ACTION_STORE_AND_MULTISAMPLE_RESOLVE
    )
}

/// What becomes of the attachment the pass rendered into: always `STORE`,
/// whatever `MTLStoreAction` the pass names.
///
/// Metal's `DontCare` is permission to discard, not an instruction to. On the
/// immediate-mode GPUs Metal runs on an attachment is plain memory whose
/// contents outlive the pass, so a game writes `DontCare` on every pass which
/// does not need the attachment kept and then reads that same texture later,
/// and it works. Vulkan's `DONT_CARE` is not a hint: on the NVIDIA driver this
/// runs on it invalidates the surface's compression metadata, and what the
/// texture then reads back as is a sparse, dithered version of whatever it held
/// before.
///
/// Storing costs the write-back of an attachment nothing reads again. It was
/// not measurable: at a matched 9,070 draws a second that scene spends 388 ms
/// of device time a second of wall clock discarding and 389 ms storing.
pub(super) fn store_op() -> vk::AttachmentStoreOp {
    vk::AttachmentStoreOp::STORE
}

/// Where one colour attachment is resolved to, when the store action asks for
/// a resolve and the destination can carry it.
///
/// Vulkan resolves only into an attachment of the same format, and Metal names
/// a mip level and array slice of the destination rather than the whole
/// texture. A destination this cannot express is reported and dropped: the
/// multisampled attachment is then stored instead, which is wrong but visible,
/// where a resolve nothing performs leaves the destination black.
pub(super) struct ResolveTarget {
    format: vk::Format,
    view: vk::ImageView,
    image: vk::Image,
    level: u32,
    slice: u32,
}

pub(super) fn resolve_attachment(
    color: &AttachmentDescriptor,
    format: vk::Format,
) -> Option<ResolveTarget> {
    if !resolves(color.store_action) || color.resolve_texture.is_null() {
        return None;
    }
    // SAFETY: only a texture of this crate's is attached.
    let texture = unsafe { object::body_ref::<Texture>(color.resolve_texture) };
    if texture.image.format != format {
        gpu::trace_once(format!(
            "render pass resolves {format:?} into {:?}, which Vulkan cannot do",
            texture.image.format
        ));
        return None;
    }
    let level = color.resolve_level as u32 + texture.image.base_level;
    let slice = color.resolve_slice as u32 + texture.image.base_layer;
    Some(ResolveTarget {
        format: texture.image.format,
        view: texture.attachment_view(level, slice),
        image: texture.image.image,
        level,
        slice,
    })
}

/// The Vulkan resolve mode one `MTLMultisampleDepthResolveFilter` asks for.
///
/// `Sample0` is 0, `Min` is 1 and `Max` is 2. Metal has no average filter for
/// depth, so `VK_RESOLVE_MODE_AVERAGE` is never chosen.
fn depth_filter(filter: u64) -> Option<vk::ResolveModeFlags> {
    match filter {
        0 => Some(vk::ResolveModeFlags::SAMPLE_ZERO),
        1 => Some(vk::ResolveModeFlags::MIN),
        2 => Some(vk::ResolveModeFlags::MAX),
        _ => None,
    }
}

/// The Vulkan resolve mode one `MTLMultisampleStencilResolveFilter` asks for.
///
/// `Sample0` is 0. `DepthResolvedSample` is 1 and means the stencil value of
/// whichever sample the depth filter chose, which Vulkan cannot express: its
/// stencil mode is independent of the depth one.
fn stencil_filter(filter: u64) -> Option<vk::ResolveModeFlags> {
    match filter {
        0 => Some(vk::ResolveModeFlags::SAMPLE_ZERO),
        _ => None,
    }
}

/// The resolve mode for each aspect of one depth/stencil resolve, or why this
/// device cannot perform the one the pass asks for.
///
/// `depth` and `stencil` are the Metal filters of the aspects which are being
/// resolved, and `None` an aspect which is not. Vulkan names one resolve
/// attachment with a mode per aspect, and a device may support only some of
/// the modes, only matching modes, or only resolving both aspects at once.
pub(super) fn resolve_modes(
    support: ResolveSupport,
    aspect: vk::ImageAspectFlags,
    depth: Option<u64>,
    stencil: Option<u64>,
) -> Result<(vk::ResolveModeFlags, vk::ResolveModeFlags), String> {
    let mut depth_mode = vk::ResolveModeFlags::NONE;
    if let Some(filter) = depth {
        if !aspect.contains(vk::ImageAspectFlags::DEPTH) {
            return Err("a depth resolve of an attachment which has no depth aspect".to_owned());
        }
        let mode = depth_filter(filter)
            .ok_or_else(|| format!("MTLMultisampleDepthResolveFilter {filter} is not a filter"))?;
        if !support.depth_modes.contains(mode) {
            return Err(format!(
                "a depth resolve needs {mode:?}, which this Vulkan device does not support"
            ));
        }
        depth_mode = mode;
    }
    let mut stencil_mode = vk::ResolveModeFlags::NONE;
    if let Some(filter) = stencil {
        if !aspect.contains(vk::ImageAspectFlags::STENCIL) {
            return Err(
                "a stencil resolve of an attachment which has no stencil aspect".to_owned(),
            );
        }
        let mode = stencil_filter(filter).ok_or_else(|| {
            format!(
                "MTLMultisampleStencilResolveFilter {filter} names a sample the depth filter                  chose, which Vulkan cannot express"
            )
        })?;
        if !support.stencil_modes.contains(mode) {
            return Err(format!(
                "a stencil resolve needs {mode:?}, which this Vulkan device does not support"
            ));
        }
        stencil_mode = mode;
    }
    if depth_mode.is_empty() && stencil_mode.is_empty() {
        return Err("a resolve of neither aspect".to_owned());
    }
    // The independence rules bind only where the attachment has both aspects:
    // with one aspect the other mode is not read.
    let both = aspect.contains(vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL);
    if both && depth_mode != stencil_mode {
        if depth_mode.is_empty() || stencil_mode.is_empty() {
            if !support.independent_none {
                return Err(
                    "a resolve of one aspect of a depth/stencil attachment and not the other,                      which this Vulkan device does not support"
                        .to_owned(),
                );
            }
        } else if !support.independent {
            return Err(format!(
                "a depth resolve with {depth_mode:?} and a stencil resolve with                  {stencil_mode:?}, where this Vulkan device needs one mode for both"
            ));
        }
    }
    Ok((depth_mode, stencil_mode))
}

/// Resolve filters on a one-sample attachment all select its only sample.
/// Validate the Metal enums, but no Vulkan depth/stencil-resolve feature is
/// needed because the operation is recorded as a plain copy.
fn one_sample_resolve_modes(
    depth: Option<u64>,
    stencil: Option<u64>,
) -> Result<(vk::ResolveModeFlags, vk::ResolveModeFlags), String> {
    let depth = match depth {
        Some(0..=2) => vk::ResolveModeFlags::SAMPLE_ZERO,
        Some(filter) => {
            return Err(format!(
                "MTLMultisampleDepthResolveFilter {filter} is not a filter"
            ));
        }
        None => vk::ResolveModeFlags::NONE,
    };
    let stencil = match stencil {
        Some(0..=1) => vk::ResolveModeFlags::SAMPLE_ZERO,
        Some(filter) => {
            return Err(format!(
                "MTLMultisampleStencilResolveFilter {filter} is not a filter"
            ));
        }
        None => vk::ResolveModeFlags::NONE,
    };
    Ok((depth, stencil))
}

/// Where the depth/stencil attachment is resolved to, when the pass asks for a
/// resolve and the device and the destination can carry it.
pub(super) struct DepthResolveTarget {
    pub resolve: DepthResolve,
    pub view: vk::ImageView,
    pub image: vk::Image,
    pub level: u32,
    pub slice: u32,
    pub texture: Id,
}

/// Texture views can name the same attachment without sharing a Metal handle.
fn same_texture_subresource(a: (Id, u64, u64), b: (Id, u64, u64)) -> bool {
    let ((a, a_level, a_slice), (b, b_level, b_slice)) = (a, b);
    if a.is_null() || b.is_null() {
        return false;
    }
    // SAFETY: only textures of this crate's are attached.
    let (a, b) = unsafe {
        (
            object::body_ref::<Texture>(a),
            object::body_ref::<Texture>(b),
        )
    };
    std::ptr::eq(a.gpu, b.gpu)
        && a.image.image == b.image.image
        && a.image.format == b.image.format
        && a_level
            .checked_add(u64::from(a.image.base_level))
            .zip(b_level.checked_add(u64::from(b.image.base_level)))
            .is_some_and(|(a, b)| a == b)
        && a_slice
            .checked_add(u64::from(a.image.base_layer))
            .zip(b_slice.checked_add(u64::from(b.image.base_layer)))
            .is_some_and(|(a, b)| a == b)
}

/// The resolve for a shared multisampled depth/stencil attachment.
///
/// Metal names a resolve destination on the depth attachment and another on
/// the stencil one; a Vulkan subpass has a single depth/stencil resolve
/// attachment with a mode per aspect, so the two must name the same subresource.
/// A resolve this cannot express is reported and dropped, which leaves the
/// destination as it was rather than holding one arbitrary sample.
fn depth_resolve(
    gpu: &Gpu,
    descriptor: &RenderPassDescriptor,
    source: &Texture,
) -> Option<DepthResolveTarget> {
    let wants = |attachment: &AttachmentDescriptor| {
        resolves(attachment.store_action) && !attachment.resolve_texture.is_null()
    };
    let depth = wants(&descriptor.depth).then_some(&descriptor.depth);
    let mut stencil = wants(&descriptor.stencil).then_some(&descriptor.stencil);
    if let (Some(from_depth), Some(from_stencil)) = (depth, stencil)
        && !same_texture_subresource(
            (
                from_depth.resolve_texture,
                from_depth.resolve_level,
                from_depth.resolve_slice,
            ),
            (
                from_stencil.resolve_texture,
                from_stencil.resolve_level,
                from_stencil.resolve_slice,
            ),
        )
    {
        gpu::trace_once(
            "a render pass names separate depth and stencil resolve subresources, which one Vulkan \
             resolve attachment cannot hold: the stencil one is ignored",
        );
        stencil = None;
    }
    let named = depth.or(stencil)?;
    // SAFETY: only a texture of this crate's is attached.
    let destination = unsafe { object::body_ref::<Texture>(named.resolve_texture) };
    let format = source.image.format;
    if destination.image.format != format {
        gpu::trace_once(format!(
            "render pass resolves depth {format:?} into {:?}, which Vulkan cannot do",
            destination.image.format
        ));
        return None;
    }
    let Some(resolving) = &gpu.depth_stencil_resolve else {
        gpu::trace_once(
            "a render pass asks for a depth or stencil resolve, which this Vulkan device cannot \
             perform",
        );
        return None;
    };
    // A device may resolve one aspect and not the other; where it may not,
    // dropping the aspect Metal asked for less loudly would silently change
    // what the pass writes, so the whole resolve is reported instead.
    let (depth_mode, stencil_mode) = match resolve_modes(
        resolving.support,
        source.image.aspect,
        depth.map(|attachment| attachment.resolve_filter),
        stencil.map(|attachment| attachment.resolve_filter),
    ) {
        Ok(modes) => modes,
        Err(reason) => {
            gpu::trace_once(format!("a render pass asks for {reason}"));
            return None;
        }
    };
    let level = named.resolve_level as u32 + destination.image.base_level;
    let slice = named.resolve_slice as u32 + destination.image.base_layer;
    Some(DepthResolveTarget {
        resolve: DepthResolve {
            format: destination.image.format,
            depth_mode,
            stencil_mode,
        },
        view: destination.attachment_view(level, slice),
        image: destination.image.image,
        level,
        slice,
        texture: named.resolve_texture,
    })
}

/// The resolve destination one independently described depth or stencil
/// aspect names. A one-sample bridge uses this for the post-pass copy from
/// the original image; a lone aspect can use it directly.
fn aspect_resolve(
    gpu: &Gpu,
    attachment: &AttachmentDescriptor,
    format: vk::Format,
    aspect: vk::ImageAspectFlags,
) -> Option<DepthResolveTarget> {
    if !resolves(attachment.store_action) || attachment.resolve_texture.is_null() {
        return None;
    }
    let wanted = if aspect == vk::ImageAspectFlags::DEPTH {
        (Some(attachment.resolve_filter), None)
    } else if aspect == vk::ImageAspectFlags::STENCIL {
        (None, Some(attachment.resolve_filter))
    } else {
        gpu::trace_once(format!(
            "a separate depth/stencil resolve has unsupported aspect {aspect:?}"
        ));
        return None;
    };
    // SAFETY: only a texture of this crate's is attached.
    let destination = unsafe { object::body_ref::<Texture>(attachment.resolve_texture) };
    if destination.image.format != format {
        gpu::trace_once(format!(
            "render pass resolves {format:?} into {:?}, which Vulkan cannot do",
            destination.image.format
        ));
        return None;
    }
    // SAFETY: only a texture of this crate's is attached.
    let source = unsafe { object::body_ref::<Texture>(attachment.texture) };
    if source.image.samples == vk::SampleCountFlags::TYPE_1 {
        let (depth_mode, stencil_mode) = match one_sample_resolve_modes(wanted.0, wanted.1) {
            Ok(modes) => modes,
            Err(reason) => {
                gpu::trace_once(format!("a render pass asks for {reason}"));
                return None;
            }
        };
        let level = attachment.resolve_level as u32 + destination.image.base_level;
        let slice = attachment.resolve_slice as u32 + destination.image.base_layer;
        return Some(DepthResolveTarget {
            resolve: DepthResolve {
                format: destination.image.format,
                depth_mode,
                stencil_mode,
            },
            view: destination.attachment_view(level, slice),
            image: destination.image.image,
            level,
            slice,
            texture: attachment.resolve_texture,
        });
    }
    let Some(resolving) = &gpu.depth_stencil_resolve else {
        gpu::trace_once(
            "a render pass asks for a depth or stencil resolve, which this Vulkan device cannot perform",
        );
        return None;
    };
    let (depth_mode, stencil_mode) =
        match resolve_modes(resolving.support, aspect, wanted.0, wanted.1) {
            Ok(modes) => modes,
            Err(reason) => {
                gpu::trace_once(format!("a render pass asks for {reason}"));
                return None;
            }
        };
    let level = attachment.resolve_level as u32 + destination.image.base_level;
    let slice = attachment.resolve_slice as u32 + destination.image.base_layer;
    Some(DepthResolveTarget {
        resolve: DepthResolve {
            format: destination.image.format,
            depth_mode,
            stencil_mode,
        },
        view: destination.attachment_view(level, slice),
        image: destination.image.image,
        level,
        slice,
        texture: attachment.resolve_texture,
    })
}

/// What one `MTLRenderPassDescriptor`'s attachments amount to in Vulkan.
struct Attachments {
    key: RenderPassKey,
    /// Attachment views: colours, then depth/stencil.
    views: Vec<vk::ImageView>,
    /// The views of the colours which resolve, in order.
    resolve_views: Vec<vk::ImageView>,
    /// Which colour attachments resolve, as a bitmask over render targets.
    resolve_mask: u32,
    /// The view the depth/stencil attachment resolves into.
    depth_resolve_view: Option<vk::ImageView>,
    /// The independently attached stencil image's resolve view.
    stencil_resolve_view: Option<vk::ImageView>,
    /// Copies Metal's `MTLStoreActionStoreAndMultisampleResolve` implies.
    copies: Vec<PendingCopy>,
    /// Separate Metal depth/stencil images bridged through one combined
    /// Vulkan attachment before and after the pass.
    bridges: Vec<DepthStencilBridge>,
    capture_targets: Vec<crate::capture::Target>,
    clears: Vec<vk::ClearValue>,
    /// The smallest attachment extent, which is the pass's own.
    width: u32,
    height: u32,
    /// The textures the pass reads or writes, retained for its lifetime.
    textures: Vec<Id>,
    /// Fresh private textures whose creating references are transferred to
    /// the command after it retains them.
    owned_textures: Vec<Id>,
}

/// Record the sample count one attachment brings to a pass.
///
/// Metal requires every attachment of a pass to have the same one, and a
/// Vulkan render pass has a single `samples` for the whole subpass, so the
/// last attachment's count describes every one of them. A pass which mixes
/// them is malformed Metal and is named rather than silently described wrong.
fn samples(key: &mut RenderPassKey, count: vk::SampleCountFlags, first: bool) {
    if !first && key.samples != count {
        gpu::trace_once(
            "a render pass mixes attachment sample counts, which Metal does not allow: the \
             last attachment's count describes the whole pass",
        );
    }
    key.samples = count;
}

/// Gather what a descriptor's attachments amount to, without touching the
/// command buffer: the caller decides what to do when there are none.
fn gather_attachments(
    gpu: &'static Gpu,
    descriptor: &RenderPassDescriptor,
) -> Result<Attachments, String> {
    let mut key = RenderPassKey {
        colors: Vec::new(),
        color_slots: Vec::new(),
        color_resolves: Vec::new(),
        depth: None,
        stencil: None,
        depth_stencil_shared: false,
        depth_resolve: None,
        samples: vk::SampleCountFlags::TYPE_1,
    };
    let mut views = Vec::new();
    let mut resolve_views = Vec::new();
    let mut resolve_mask = 0u32;
    let mut depth_resolve_view = None;
    let mut stencil_resolve_view = None;
    let mut copies: Vec<PendingCopy> = Vec::new();
    let mut bridges = Vec::new();
    let mut capture_targets: Vec<crate::capture::Target> = Vec::new();
    let mut clears = Vec::new();
    let mut width = u32::MAX;
    let mut height = u32::MAX;
    let mut textures = Vec::new();
    let mut owned_textures = Vec::new();
    // The targets only matter to `ROSEMARY_PASS_OUT`, and naming each one
    // formats a string.
    let capturing = crate::capture::enabled();

    let last_color = descriptor
        .colors
        .iter()
        .rposition(|color| !color.texture.is_null());
    for (index, color) in descriptor.colors.iter().enumerate() {
        if last_color.is_none_or(|last| index > last) {
            break;
        }
        if color.texture.is_null() {
            key.color_slots.push(None);
            continue;
        }
        // SAFETY: only a texture of this crate's is attached.
        let texture = unsafe { object::body_ref::<Texture>(color.texture) };
        let format = texture.image.format;
        let first = key.colors.is_empty();
        samples(&mut key, texture.image.samples, first);
        // Absolute in the underlying image, which a texture view's are not.
        let level = color.level as u32 + texture.image.base_level;
        let color_slice = color.slice as u32 + texture.image.base_layer;
        views.push(texture.attachment_view(level, color_slice));
        let (base_width, base_height) = (texture.image.width, texture.image.height);
        let source_image = texture.image.image;
        let multisampled = texture.image.samples != vk::SampleCountFlags::TYPE_1;
        let resolve = resolve_attachment(color, format);
        let packed_index = key.colors.len();
        key.color_slots.push(Some(packed_index as u32));
        // A subpass resolve is the answer only for a multisampled attachment.
        // Where the attachment has one sample the same store action means a
        // plain copy, which is recorded once the pass has ended.
        let subpass_resolve = resolve.as_ref().filter(|_| multisampled);
        key.colors
            .push((format, load_op(color.load_action), store_op()));
        key.color_resolves
            .push(subpass_resolve.map(|target| target.format));
        if let Some(target) = &resolve {
            textures.push(color.resolve_texture);
            if multisampled {
                resolve_mask |= 1 << packed_index;
                resolve_views.push(target.view);
            } else {
                copies.push(PendingCopy {
                    source: source_image,
                    source_level: level,
                    source_slice: color_slice,
                    destination: target.image,
                    destination_level: target.level,
                    destination_slice: target.slice,
                    width: level_extent(base_width, level),
                    height: level_extent(base_height, level),
                    aspect: vk::ImageAspectFlags::COLOR,
                });
            }
        }
        if capturing {
            capture_targets.push(crate::capture::Target {
                image: source_image,
                aspect: vk::ImageAspectFlags::COLOR,
                level,
                slice: color_slice,
                width: level_extent(base_width, level),
                height: level_extent(base_height, level),
                format,
                samples: texture.image.samples,
                role: format!("c{index}"),
            });
        }
        if let Some(target) = resolve.as_ref().filter(|_| capturing) {
            capture_targets.push(crate::capture::Target {
                image: target.image,
                aspect: vk::ImageAspectFlags::COLOR,
                level: target.level,
                slice: target.slice,
                width: level_extent(base_width, level),
                height: level_extent(base_height, level),
                format: target.format,
                samples: vk::SampleCountFlags::TYPE_1,
                role: format!("r{index}"),
            });
        }
        clears.push(vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [
                    color.clear_color[0] as f32,
                    color.clear_color[1] as f32,
                    color.clear_color[2] as f32,
                    color.clear_color[3] as f32,
                ],
            },
        });
        width = width.min(level_extent(base_width, level));
        height = height.min(level_extent(base_height, level));
        textures.push(color.texture);
    }
    if !descriptor.depth.texture.is_null() {
        // SAFETY: only textures of this crate's are attached.
        let texture = unsafe { object::body_ref::<Texture>(descriptor.depth.texture) };
        if !texture.image.aspect.contains(vk::ImageAspectFlags::DEPTH) {
            return Err(format!(
                "depth attachment format {:?} has no depth aspect",
                texture.image.format
            ));
        }
    }
    if !descriptor.stencil.texture.is_null() {
        // SAFETY: only textures of this crate's are attached.
        let texture = unsafe { object::body_ref::<Texture>(descriptor.stencil.texture) };
        if !texture.image.aspect.contains(vk::ImageAspectFlags::STENCIL) {
            return Err(format!(
                "stencil attachment format {:?} has no stencil aspect",
                texture.image.format
            ));
        }
    }
    // Vulkan always requires depth and stencil to be aspects of one image.
    // Separate subresources are bridged through a temporary combined image.
    let depth = &descriptor.depth;
    let stencil = &descriptor.stencil;
    let shared_depth_stencil = same_texture_subresource(
        (depth.texture, depth.level, depth.slice),
        (stencil.texture, stencil.level, stencil.slice),
    );
    let bridge_texture = if !descriptor.depth.texture.is_null()
        && !descriptor.stencil.texture.is_null()
        && !shared_depth_stencil
    {
        // SAFETY: only textures of this crate's are attached.
        let depth = unsafe { object::body_ref::<Texture>(descriptor.depth.texture) };
        let stencil = unsafe { object::body_ref::<Texture>(descriptor.stencil.texture) };
        let combined_format = gpu::combined_depth_stencil_format(
            depth.image.format,
            stencil.image.format,
        )
        .ok_or_else(|| {
            format!(
                "separate depth {:?} and stencil {:?} textures have no lossless combined bridge",
                depth.image.format, stencil.image.format
            )
        })?;
        let depth_bytes = match depth.image.format {
            vk::Format::D16_UNORM | vk::Format::D16_UNORM_S8_UINT => 2,
            vk::Format::D32_SFLOAT
            | vk::Format::D32_SFLOAT_S8_UINT
            | vk::Format::X8_D24_UNORM_PACK32
            | vk::Format::D24_UNORM_S8_UINT => 4,
            format => {
                return Err(format!(
                    "depth format {format:?} has no bridge buffer representation"
                ));
            }
        };
        if depth.image.samples != vk::SampleCountFlags::TYPE_1
            || stencil.image.samples != vk::SampleCountFlags::TYPE_1
        {
            return Err(
                "multisampled separate depth and stencil textures have no lossless Vulkan bridge"
                    .to_owned(),
            );
        }
        let depth_level = descriptor.depth.level as u32 + depth.image.base_level;
        let stencil_level = descriptor.stencil.level as u32 + stencil.image.base_level;
        let depth_extent = (
            level_extent(depth.image.width, depth_level),
            level_extent(depth.image.height, depth_level),
        );
        let stencil_extent = (
            level_extent(stencil.image.width, stencil_level),
            level_extent(stencil.image.height, stencil_level),
        );
        if depth_extent != stencil_extent {
            return Err(format!(
                "separate depth and stencil extents differ: {depth_extent:?} and {stencil_extent:?}"
            ));
        }
        let combined = crate::resource::new_internal_attachment(
            gpu,
            combined_format,
            depth_extent.0,
            depth_extent.1,
        )?;
        // SAFETY: the texture this function just made.
        let image = unsafe { object::body_ref::<Texture>(combined) }.image.image;
        bridges.extend([
            DepthStencilBridge {
                original: depth.image.image,
                original_level: depth_level,
                original_slice: descriptor.depth.slice as u32 + depth.image.base_layer,
                combined: image,
                width: depth_extent.0,
                height: depth_extent.1,
                aspect: vk::ImageAspectFlags::DEPTH,
                bytes_per_texel: depth_bytes,
                staging: vk::Buffer::null(),
                load: load_op(descriptor.depth.load_action) == vk::AttachmentLoadOp::LOAD,
            },
            DepthStencilBridge {
                original: stencil.image.image,
                original_level: stencil_level,
                original_slice: descriptor.stencil.slice as u32 + stencil.image.base_layer,
                combined: image,
                width: stencil_extent.0,
                height: stencil_extent.1,
                aspect: vk::ImageAspectFlags::STENCIL,
                bytes_per_texel: 1,
                staging: vk::Buffer::null(),
                load: load_op(descriptor.stencil.load_action) == vk::AttachmentLoadOp::LOAD,
            },
        ]);
        textures.push(combined);
        owned_textures.push(combined);
        Some(combined)
    } else {
        None
    };
    let pass_depth_stencil_shared = shared_depth_stencil || bridge_texture.is_some();
    key.depth_stencil_shared = pass_depth_stencil_shared;
    let depth_attachment = (!descriptor.depth.texture.is_null()).then_some(&descriptor.depth);
    if let Some(attachment) = depth_attachment {
        // SAFETY: only a texture of this crate's is attached.
        let texture = unsafe { object::body_ref::<Texture>(attachment.texture) };
        // SAFETY: the bridge, when present, was made above and is a texture.
        let pass_texture = bridge_texture.map_or(texture, |bridge| unsafe {
            object::body_ref::<Texture>(bridge)
        });
        key.depth = Some((
            pass_texture.image.format,
            load_op(attachment.load_action),
            store_op(),
        ));
        let first = key.colors.is_empty();
        samples(&mut key, pass_texture.image.samples, first);
        let level = attachment.level as u32 + texture.image.base_level;
        let depth_slice = attachment.slice as u32 + texture.image.base_layer;
        views.push(match bridge_texture {
            Some(_) => pass_texture.attachment_view(0, 0),
            None => texture.attachment_view(level, depth_slice),
        });
        let format = texture.image.format;
        let multisampled = texture.image.samples != vk::SampleCountFlags::TYPE_1;
        let (base_width, base_height) = (texture.image.width, texture.image.height);
        let source_image = texture.image.image;
        let resolve = if shared_depth_stencil && multisampled {
            depth_resolve(gpu, descriptor, texture)
        } else {
            aspect_resolve(gpu, attachment, format, vk::ImageAspectFlags::DEPTH)
        };
        if let Some(resolve) = resolve {
            textures.push(resolve.texture);
            if multisampled {
                key.depth_resolve = Some(resolve.resolve);
                if !resolve.resolve.depth_mode.is_empty() {
                    depth_resolve_view = Some(resolve.view);
                }
                if !resolve.resolve.stencil_mode.is_empty() {
                    stencil_resolve_view = Some(resolve.view);
                }
            } else {
                // A subpass resolves only a multisampled attachment; with one
                // sample the same store action means a plain copy, as it does
                // for a colour attachment.
                for copied_aspect in [
                    (!resolve.resolve.depth_mode.is_empty()).then_some(vk::ImageAspectFlags::DEPTH),
                    (!resolve.resolve.stencil_mode.is_empty())
                        .then_some(vk::ImageAspectFlags::STENCIL),
                ]
                .into_iter()
                .flatten()
                {
                    copies.push(PendingCopy {
                        source: source_image,
                        source_level: level,
                        source_slice: depth_slice,
                        destination: resolve.image,
                        destination_level: resolve.level,
                        destination_slice: resolve.slice,
                        width: level_extent(base_width, level),
                        height: level_extent(base_height, level),
                        aspect: copied_aspect,
                    });
                }
            }
            if capturing {
                for (resolved_aspect, role, enabled) in [
                    (
                        vk::ImageAspectFlags::DEPTH,
                        "rd",
                        !resolve.resolve.depth_mode.is_empty(),
                    ),
                    (
                        vk::ImageAspectFlags::STENCIL,
                        "rs",
                        !resolve.resolve.stencil_mode.is_empty(),
                    ),
                ] {
                    if enabled {
                        capture_targets.push(crate::capture::Target {
                            image: resolve.image,
                            aspect: resolved_aspect,
                            level: resolve.level,
                            slice: resolve.slice,
                            width: level_extent(base_width, level),
                            height: level_extent(base_height, level),
                            format,
                            samples: vk::SampleCountFlags::TYPE_1,
                            role: role.to_owned(),
                        });
                    }
                }
            }
        }
        clears.push(vk::ClearValue {
            depth_stencil: vk::ClearDepthStencilValue {
                depth: descriptor.depth.clear_depth as f32,
                stencil: descriptor.stencil.clear_stencil,
            },
        });
        width = width.min(level_extent(texture.image.width, level));
        height = height.min(level_extent(texture.image.height, level));
        if capturing {
            capture_targets.push(crate::capture::Target {
                image: texture.image.image,
                aspect: vk::ImageAspectFlags::DEPTH,
                level,
                slice: depth_slice,
                width: level_extent(texture.image.width, level),
                height: level_extent(texture.image.height, level),
                format: texture.image.format,
                samples: texture.image.samples,
                role: "d".to_owned(),
            });
        }
        textures.push(attachment.texture);
    }
    if !descriptor.stencil.texture.is_null() {
        let attachment = &descriptor.stencil;
        // SAFETY: only a texture of this crate's is attached.
        let texture = unsafe { object::body_ref::<Texture>(attachment.texture) };
        // SAFETY: the bridge, when present, was made above and is a texture.
        let pass_texture = bridge_texture.map_or(texture, |bridge| unsafe {
            object::body_ref::<Texture>(bridge)
        });
        key.stencil = Some((
            pass_texture.image.format,
            load_op(attachment.load_action),
            store_op(),
        ));
        let first = key.colors.is_empty() && key.depth.is_none();
        samples(&mut key, pass_texture.image.samples, first);
        let level = attachment.level as u32 + texture.image.base_level;
        let stencil_slice = attachment.slice as u32 + texture.image.base_layer;
        if !pass_depth_stencil_shared {
            views.push(texture.attachment_view(level, stencil_slice));
            clears.push(vk::ClearValue {
                depth_stencil: vk::ClearDepthStencilValue {
                    depth: descriptor.depth.clear_depth as f32,
                    stencil: attachment.clear_stencil,
                },
            });
            let format = texture.image.format;
            let multisampled = texture.image.samples != vk::SampleCountFlags::TYPE_1;
            let source_image = texture.image.image;
            if let Some(resolve) =
                aspect_resolve(gpu, attachment, format, vk::ImageAspectFlags::STENCIL)
            {
                textures.push(resolve.texture);
                if multisampled {
                    match &mut key.depth_resolve {
                        Some(combined) => combined.stencil_mode = resolve.resolve.stencil_mode,
                        None => key.depth_resolve = Some(resolve.resolve),
                    }
                    stencil_resolve_view = Some(resolve.view);
                } else {
                    copies.push(PendingCopy {
                        source: source_image,
                        source_level: level,
                        source_slice: stencil_slice,
                        destination: resolve.image,
                        destination_level: resolve.level,
                        destination_slice: resolve.slice,
                        width: level_extent(texture.image.width, level),
                        height: level_extent(texture.image.height, level),
                        aspect: vk::ImageAspectFlags::STENCIL,
                    });
                }
                if capturing {
                    capture_targets.push(crate::capture::Target {
                        image: resolve.image,
                        aspect: vk::ImageAspectFlags::STENCIL,
                        level: resolve.level,
                        slice: resolve.slice,
                        width: level_extent(texture.image.width, level),
                        height: level_extent(texture.image.height, level),
                        format,
                        samples: vk::SampleCountFlags::TYPE_1,
                        role: "rs".to_owned(),
                    });
                }
            }
        } else if (bridge_texture.is_some()
            || (shared_depth_stencil && texture.image.samples == vk::SampleCountFlags::TYPE_1))
            && let Some(resolve) = aspect_resolve(
                gpu,
                attachment,
                texture.image.format,
                vk::ImageAspectFlags::STENCIL,
            )
        {
            // A one-sample store-resolve is a post-pass copy. With a bridge it
            // runs after the aspect was copied back into the original image;
            // a combined source can copy its stencil aspect directly.
            textures.push(resolve.texture);
            copies.push(PendingCopy {
                source: texture.image.image,
                source_level: level,
                source_slice: stencil_slice,
                destination: resolve.image,
                destination_level: resolve.level,
                destination_slice: resolve.slice,
                width: level_extent(texture.image.width, level),
                height: level_extent(texture.image.height, level),
                aspect: vk::ImageAspectFlags::STENCIL,
            });
            if capturing {
                capture_targets.push(crate::capture::Target {
                    image: resolve.image,
                    aspect: vk::ImageAspectFlags::STENCIL,
                    level: resolve.level,
                    slice: resolve.slice,
                    width: level_extent(texture.image.width, level),
                    height: level_extent(texture.image.height, level),
                    format: texture.image.format,
                    samples: vk::SampleCountFlags::TYPE_1,
                    role: "rs".to_owned(),
                });
            }
        }
        width = width.min(level_extent(texture.image.width, level));
        height = height.min(level_extent(texture.image.height, level));
        if capturing {
            capture_targets.push(crate::capture::Target {
                image: texture.image.image,
                aspect: vk::ImageAspectFlags::STENCIL,
                level,
                slice: stencil_slice,
                width: level_extent(texture.image.width, level),
                height: level_extent(texture.image.height, level),
                format: texture.image.format,
                samples: texture.image.samples,
                role: "s".to_owned(),
            });
        }
        textures.push(attachment.texture);
    }
    Ok(Attachments {
        key,
        views,
        resolve_views,
        resolve_mask,
        depth_resolve_view,
        stencil_resolve_view,
        copies,
        bridges,
        capture_targets,
        clears,
        width,
        height,
        textures,
        owned_textures,
    })
}

/// Metal's default render state for a pass, which a Vulkan command buffer
/// has to be told: dynamic state is not inherited by a secondary one.
pub(super) fn record_pass_defaults(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    area: vk::Rect2D,
    depth_bias: &mut RecordedDepthBias,
    viewports: &mut ViewportState,
) {
    depth_bias.record(device, command_buffer, DepthBias::NONE);
    *viewports = ViewportState::new(area);
    // SAFETY: recording into a command buffer inside the pass.
    unsafe {
        device.cmd_set_blend_constants(command_buffer, &[0.0; 4]);
        device.cmd_set_stencil_reference(command_buffer, vk::StencilFaceFlags::FRONT_AND_BACK, 0);
    }
}

/// Begin the pass `descriptor` describes on `encoder_object`'s command
/// buffer. A parallel encoder's pass is begun for secondary command buffers,
/// so nothing else is recorded into it here.
pub(crate) fn begin_render_pass(
    encoder_object: Id,
    descriptor: &RenderPassDescriptor,
    secondary: bool,
) {
    // SAFETY: an encoder this crate just made.
    let encoder = unsafe { object::body::<Encoder>(encoder_object) };
    let command = encoder.command();
    let gpu = command.gpu;

    let attachments = match gather_attachments(gpu, descriptor) {
        Ok(attachments) => attachments,
        Err(reason) => {
            gpu::trace(format!("renderCommandEncoderWithDescriptor: {reason}"));
            return;
        }
    };
    let Attachments {
        key,
        views,
        resolve_views,
        resolve_mask,
        depth_resolve_view,
        stencil_resolve_view,
        copies,
        mut bridges,
        capture_targets,
        clears,
        width,
        height,
        textures,
        owned_textures,
    } = attachments;
    if views.is_empty() {
        for texture in owned_textures {
            objc::arc::release(texture);
        }
        gpu::trace("renderCommandEncoderWithDescriptor: has no attachments");
        return;
    }
    let width = width.max(1);
    let height = height.max(1);
    let attachment_views = AttachmentViews {
        views,
        resolve_views,
        depth_resolve_view,
        stencil_resolve_view,
        clears,
    };
    let target = match PassTarget::new(gpu, &key, attachment_views, width, height) {
        Ok(target) => target,
        Err(reason) => {
            for texture in owned_textures {
                objc::arc::release(texture);
            }
            return gpu::trace(reason);
        }
    };
    command.framebuffers.extend(target.framebuffer());
    for texture in textures {
        // SAFETY: only textures of this crate's are attached.
        let image = &unsafe { object::body_ref::<Texture>(texture) }.image;
        encoder
            .attachments
            .push((image.image, gpu::aspect_of(image.format)));
        // Memory another resource may share is not ordered by its image.
        encoder.writes_elsewhere |= hazards::tracked_image(texture).is_none();
        command.retain(texture);
    }
    for texture in owned_textures {
        objc::arc::release(texture);
    }
    // Sub-encoders record into command buffers of their own, query results
    // land in a buffer, and a resolve copy writes through a transfer: each
    // reaches memory other than through the attachments.
    encoder.writes_elsewhere |= secondary
        || !descriptor.visibility_result_buffer.is_null()
        || !copies.is_empty()
        || !bridges.is_empty();
    encoder.visibility = VisibilityTarget::of(descriptor);
    command.retain(descriptor.visibility_result_buffer);
    let area = vk::Rect2D {
        offset: vk::Offset2D { x: 0, y: 0 },
        extent: vk::Extent2D { width, height },
    };
    trace_pass(&key, width, height);
    encoder.capture_pass = crate::capture::claim();
    encoder.resolve_mask = resolve_mask;
    encoder.depth_resolve = key.depth_resolve;
    encoder.in_render_pass = true;
    // Queries and captures run in the pass as it is begun, and a parallel
    // pass's sub-encoders inherit it, so those begin now, behind every pass
    // before them. Every other pass begins at its first draw.
    if !secondary
        && encoder.visibility.is_none()
        && encoder.capture_pass.is_none()
        && bridges.is_empty()
    {
        record_pass_defaults(
            &gpu.device,
            command.command_buffer,
            area,
            &mut encoder.depth_bias,
            &mut encoder.viewports,
        );
        let is_clear = |op: vk::AttachmentLoadOp| op == vk::AttachmentLoadOp::CLEAR;
        let works_undrawn = key.colors.iter().any(|color| is_clear(color.1))
            || key.depth.is_some_and(|depth| is_clear(depth.1))
            || key.stencil.is_some_and(|stencil| is_clear(stencil.1))
            || resolve_mask != 0
            || key.depth_resolve.is_some()
            || !copies.is_empty();
        encoder.lazy = Some(LazyPass {
            target,
            area,
            begun: false,
            works_undrawn,
            reads: Vec::new(),
            read_hazard: false,
        });
        encoder.pending_copies = copies;
        encoder.capture_targets = capture_targets;
        return;
    }
    command.flush_hazards();
    for bridge in bridges.iter_mut().filter(|bridge| bridge.load) {
        if let Err(reason) = record_depth_stencil_bridge(command, bridge, true) {
            gpu::trace(format!("depth/stencil bridge load failed: {reason}"));
            encoder.in_render_pass = false;
            encoder.capture_pass = None;
            encoder.visibility = None;
            return;
        }
    }
    if let Some(pass) = encoder.capture_pass {
        for target in &capture_targets {
            if let Some(capture) =
                crate::capture::record(gpu, command.command_buffer, pass, "pre", target)
            {
                command.captures.push(capture);
            }
        }
    }
    target.begin(gpu, command.command_buffer, area, secondary);
    if secondary {
        encoder.inheritance = Some((target.inheritance(&key), area));
    } else {
        record_pass_defaults(
            &gpu.device,
            command.command_buffer,
            area,
            &mut encoder.depth_bias,
            &mut encoder.viewports,
        );
    }
    gpu::milestone("render pass");
    encoder.pending_copies = copies;
    encoder.pending_depth_stencil_bridges = bridges;
    encoder.capture_targets = capture_targets;
}

pub(super) unsafe extern "C" fn render_pass_descriptor_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a render pass descriptor of this crate's. `-init` reaches a fresh
    // one, whose body is zero, and a reused one, which still owns what it holds.
    unsafe { release_pass_resources(receiver) };
    let descriptor = unsafe { object::body::<RenderPassDescriptor>(receiver) };
    // Attachment objects vended before stay this descriptor's, and read the
    // reset values through it.
    let (color_objects, depth_object, stencil_object) = (
        descriptor.color_objects,
        descriptor.depth_object,
        descriptor.stencil_object,
    );
    *descriptor = RenderPassDescriptor {
        color_objects,
        depth_object,
        stencil_object,
        ..RenderPassDescriptor::default()
    };
    receiver
}

/// Give up the textures and visibility buffer a render pass descriptor holds.
///
/// # Safety
///
/// `receiver` must be a `MTLRenderPassDescriptor` of this crate's.
unsafe fn release_pass_resources(receiver: Id) {
    let descriptor = unsafe { object::body::<RenderPassDescriptor>(receiver) };
    let mut attachments: Vec<&mut AttachmentDescriptor> = descriptor.colors.iter_mut().collect();
    attachments.push(&mut descriptor.depth);
    attachments.push(&mut descriptor.stencil);
    for attachment in attachments {
        objc::arc::release(std::mem::replace(
            &mut attachment.texture,
            std::ptr::null_mut(),
        ));
        objc::arc::release(std::mem::replace(
            &mut attachment.resolve_texture,
            std::ptr::null_mut(),
        ));
    }
    objc::arc::release(std::mem::replace(
        &mut descriptor.visibility_result_buffer,
        std::ptr::null_mut(),
    ));
}

/// Give up every reference a render pass descriptor holds, as it goes.
///
/// # Safety
///
/// `receiver` must be a `MTLRenderPassDescriptor` of this crate's.
pub(super) unsafe fn release_pass_contents(receiver: Id) {
    unsafe { release_pass_resources(receiver) };
    let descriptor = unsafe { object::body::<RenderPassDescriptor>(receiver) };
    // The attachment objects point back at this descriptor, and a guest may
    // hold one - or the collection - after releasing the descriptor. Orphan
    // each before it goes, so a later setter is a no-op rather than a write
    // through a dangling pointer.
    let colors = std::mem::replace(&mut descriptor.color_objects, std::ptr::null_mut());
    crate::resource::orphan(colors);
    objc::arc::release(colors);
    for slot in [&mut descriptor.depth_object, &mut descriptor.stencil_object] {
        let element = std::mem::replace(slot, std::ptr::null_mut());
        if !element.is_null() {
            // SAFETY: an `RosemaryMTLRenderPassAttachmentDescriptor` this descriptor
            // made.
            unsafe { crate::resource::orphan_element::<AttachmentElement>(element) };
        }
        objc::arc::release(element);
    }
}

/// `-[MTLRenderPassDescriptor dealloc]`.
pub(super) unsafe extern "C" fn render_pass_descriptor_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a render pass descriptor of this crate's.
    unsafe { release_pass_contents(receiver) };
    objc::arc::finalize(receiver);
}

pub(super) unsafe extern "C" fn pass_visibility_result_buffer(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a render pass descriptor of this crate's.
    unsafe { object::body::<RenderPassDescriptor>(receiver) }.visibility_result_buffer
}

pub(super) unsafe extern "C" fn set_pass_visibility_result_buffer(
    receiver: Id,
    _sel: Sel,
    buffer: Id,
) {
    let descriptor = unsafe { object::body::<RenderPassDescriptor>(receiver) };
    let previous = std::mem::replace(
        &mut descriptor.visibility_result_buffer,
        objc::arc::retain(buffer),
    );
    objc::arc::release(previous);
}

/// `-setRenderTargetArrayLength:`. A pass here attaches the one slice each
/// attachment names, which is a layered pass of one layer.
pub(super) unsafe extern "C" fn set_render_target_array_length(
    receiver: Id,
    _sel: Sel,
    length: u64,
) {
    if length > 1 {
        gpu::trace_once("a layered render pass is unsupported; only one slice is rendered");
    }
    // SAFETY: a render pass descriptor of this crate's.
    unsafe { object::body::<RenderPassDescriptor>(receiver) }.render_target_array_length = length;
}

pub(super) unsafe extern "C" fn render_target_array_length(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: a render pass descriptor of this crate's.
    unsafe { object::body::<RenderPassDescriptor>(receiver) }.render_target_array_length
}

/// `+[MTLRenderPassDescriptor renderPassDescriptor]`.
pub(super) unsafe extern "C" fn new_render_pass_descriptor(_class: Id, _sel: Sel) -> Id {
    let object = object::make(c"MTLRenderPassDescriptor", RenderPassDescriptor::default());
    // SAFETY: a fresh object this call owns.
    unsafe { corefoundation::api::CFAutorelease(object as corefoundation::base::CFTypeRef) as Id }
}

/// Which attachment of a render pass descriptor one element names.
#[derive(Clone, Copy)]
pub(crate) struct AttachmentElement {
    pub owner: Id,
    pub slot: AttachmentSlot,
    /// The attachment of an element with no owner: a standalone
    /// `MTLRenderPass...AttachmentDescriptor`, or one whose descriptor has
    /// gone. Its textures are retained, and released by the element's
    /// `-dealloc`.
    pub local: AttachmentDescriptor,
}

impl AttachmentElement {
    fn new(owner: Id, slot: AttachmentSlot) -> Self {
        Self {
            owner,
            slot,
            local: slot.defaults(),
        }
    }
}

impl crate::resource::Element for AttachmentElement {
    fn owner(&mut self) -> &mut Id {
        &mut self.owner
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum AttachmentSlot {
    Color(usize),
    Depth,
    Stencil,
}

impl AttachmentSlot {
    /// A fresh attachment of this slot: Metal's defaults differ between a
    /// colour attachment and a depth or stencil one.
    pub(super) fn defaults(self) -> AttachmentDescriptor {
        let (load_action, store_action) = match self {
            Self::Color(_) => (MTL_LOAD_ACTION_DONT_CARE, MTL_STORE_ACTION_STORE),
            Self::Depth | Self::Stencil => (MTL_LOAD_ACTION_CLEAR, MTL_STORE_ACTION_DONT_CARE),
        };
        AttachmentDescriptor {
            texture: std::ptr::null_mut(),
            level: 0,
            slice: 0,
            load_action,
            store_action,
            clear_color: [0.0, 0.0, 0.0, 1.0],
            clear_depth: 1.0,
            clear_stencil: 0,
            resolve_texture: std::ptr::null_mut(),
            resolve_level: 0,
            resolve_slice: 0,
            resolve_filter: 0,
        }
    }
}

impl RenderPassDescriptor {
    fn attachment(&mut self, slot: AttachmentSlot) -> &mut AttachmentDescriptor {
        match slot {
            AttachmentSlot::Color(index) => &mut self.colors[index],
            AttachmentSlot::Depth => &mut self.depth,
            AttachmentSlot::Stencil => &mut self.stencil,
        }
    }
}

/// The attachment one element names: its descriptor's, or its own.
pub(super) fn attachment_of<'a>(element: Id) -> Option<&'a mut AttachmentDescriptor> {
    // SAFETY: an attachment element of this crate's.
    let element = unsafe { object::body::<AttachmentElement>(element) };
    if element.owner.is_null() {
        return Some(&mut element.local);
    }
    // SAFETY: the element holds the descriptor which made it, which orphans
    // every element it vended before it goes.
    let descriptor = unsafe { object::body::<RenderPassDescriptor>(element.owner) };
    Some(descriptor.attachment(element.slot))
}

pub(super) unsafe extern "C" fn pass_color_attachments(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a render pass descriptor of this crate's.
    let descriptor = unsafe { object::body::<RenderPassDescriptor>(receiver) };
    if !descriptor.color_objects.is_null() {
        return descriptor.color_objects;
    }
    let elements = (0..RENDER_TARGETS)
        .map(|index| {
            object::make(
                c"RosemaryMTLRenderPassAttachmentDescriptor",
                AttachmentElement::new(receiver, AttachmentSlot::Color(index)),
            )
        })
        .collect::<Vec<_>>();
    let collection = object::make(
        c"RosemaryMTLIndexedCollection",
        crate::resource::IndexedCollection {
            elements,
            // SAFETY: every element here is an
            // `RosemaryMTLRenderPassAttachmentDescriptor`.
            orphan: |element| unsafe {
                crate::resource::orphan_element::<AttachmentElement>(element)
            },
            assign: |element, source| {
                // SAFETY: an attachment element of this crate's.
                let slot = unsafe { object::body::<AttachmentElement>(element) }.slot;
                if let Some(destination) = attachment_of(element) {
                    assign_attachment(destination, source, slot);
                }
            },
        },
    );
    unsafe { object::body::<RenderPassDescriptor>(receiver) }.color_objects = collection;
    collection
}

pub(super) unsafe extern "C" fn pass_single_attachment(receiver: Id, sel: Sel) -> Id {
    // SAFETY: a render pass descriptor of this crate's.
    let descriptor = unsafe { object::body::<RenderPassDescriptor>(receiver) };
    // SAFETY: a selector the runtime owns.
    let depth = unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() == b"depthAttachment";
    let existing = if depth {
        descriptor.depth_object
    } else {
        descriptor.stencil_object
    };
    if !existing.is_null() {
        return existing;
    }
    let element = object::make(
        c"RosemaryMTLRenderPassAttachmentDescriptor",
        AttachmentElement::new(
            receiver,
            if depth {
                AttachmentSlot::Depth
            } else {
                AttachmentSlot::Stencil
            },
        ),
    );
    let descriptor = unsafe { object::body::<RenderPassDescriptor>(receiver) };
    if depth {
        descriptor.depth_object = element;
    } else {
        descriptor.stencil_object = element;
    }
    element
}

pub(super) unsafe extern "C" fn set_attachment_object(receiver: Id, sel: Sel, value: Id) {
    let Some(attachment) = attachment_of(receiver) else {
        return;
    };
    // SAFETY: a selector the runtime owns.
    let slot = match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"setTexture:" => &mut attachment.texture,
        b"setResolveTexture:" => &mut attachment.resolve_texture,
        _ => return,
    };
    // A texture buffer has no image to attach: Metal refuses it as a render
    // target, and every attachment path here reads the image.
    // SAFETY: only a texture of this crate's is attached.
    // A native-Metal passthrough texture uses a different guest object layout.
    // Only Vulkan-backed textures have the private `Texture` body and its
    // Vulkan texture-buffer restriction; do not interpret a native wrapper
    // as that body. The native backend validates render target compatibility.
    let value = match object::is_kind(value, c"RosemaryMTLTexture")
        && unsafe { object::body_ref::<Texture>(value) }.is_texture_buffer()
    {
        true => {
            gpu::trace("rejecting a texture buffer as a render pass attachment");
            std::ptr::null_mut()
        }
        false => value,
    };
    objc::arc::release(std::mem::replace(slot, objc::arc::retain(value)));
}

pub(super) unsafe extern "C" fn set_attachment_number(receiver: Id, sel: Sel, value: u64) {
    let Some(attachment) = attachment_of(receiver) else {
        return;
    };
    // SAFETY: a selector the runtime owns.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"setLevel:" => attachment.level = value,
        b"setSlice:" => attachment.slice = value,
        b"setLoadAction:" => attachment.load_action = value,
        b"setStoreAction:" => attachment.store_action = value,
        b"setClearStencil:" => attachment.clear_stencil = value as u32,
        b"setResolveLevel:" => attachment.resolve_level = value,
        b"setResolveSlice:" => attachment.resolve_slice = value,
        // One field for both: an attachment is the depth one or the stencil
        // one, and each has only its own filter.
        b"setDepthResolveFilter:" | b"setStencilResolveFilter:" => {
            attachment.resolve_filter = value
        }
        b"setDepthPlane:" => (),
        _ => (),
    }
}

pub(super) unsafe extern "C" fn set_attachment_clear_color(
    receiver: Id,
    _sel: Sel,
    color: ClearColor,
) {
    if let Some(attachment) = attachment_of(receiver) {
        attachment.clear_color = [color.red, color.green, color.blue, color.alpha];
    }
}

pub(super) unsafe extern "C" fn set_attachment_clear_depth(receiver: Id, _sel: Sel, depth: f64) {
    if let Some(attachment) = attachment_of(receiver) {
        attachment.clear_depth = depth;
    }
}

/// Whether `object` is an attachment element of any of this crate's classes.
fn is_attachment(object: Id) -> bool {
    ATTACHMENT_CLASSES
        .iter()
        .any(|name| crate::object::is_kind(object, name))
}

pub(super) const ATTACHMENT_CLASSES: [&std::ffi::CStr; 5] = [
    c"RosemaryMTLRenderPassAttachmentDescriptor",
    c"MTLRenderPassAttachmentDescriptor",
    c"MTLRenderPassColorAttachmentDescriptor",
    c"MTLRenderPassDepthAttachmentDescriptor",
    c"MTLRenderPassStencilAttachmentDescriptor",
];

/// Copy `source`'s attachment, or the defaults where it is nil, over
/// `destination`, which keeps its own references to the textures.
pub(super) fn assign_attachment(
    destination: &mut AttachmentDescriptor,
    source: Id,
    slot: AttachmentSlot,
) {
    let value = if is_attachment(source) {
        attachment_of(source).map_or_else(|| slot.defaults(), |a| *a)
    } else {
        slot.defaults()
    };
    objc::arc::retain(value.texture);
    objc::arc::retain(value.resolve_texture);
    let previous = std::mem::replace(destination, value);
    objc::arc::release(previous.texture);
    objc::arc::release(previous.resolve_texture);
}

/// `-[MTLRenderPassDescriptor setDepthAttachment:]` and its stencil twin.
pub(super) unsafe extern "C" fn set_pass_single_attachment(receiver: Id, sel: Sel, value: Id) {
    // SAFETY: a render pass descriptor of this crate's.
    let descriptor = unsafe { object::body::<RenderPassDescriptor>(receiver) };
    // SAFETY: a selector the runtime owns.
    let slot = match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"setDepthAttachment:" => AttachmentSlot::Depth,
        _ => AttachmentSlot::Stencil,
    };
    assign_attachment(descriptor.attachment(slot), value, slot);
}

/// `-init` of a standalone attachment descriptor.
pub(super) unsafe extern "C" fn attachment_init(receiver: Id, _sel: Sel) -> Id {
    let slot = if object::is_kind(receiver, c"MTLRenderPassDepthAttachmentDescriptor") {
        AttachmentSlot::Depth
    } else if object::is_kind(receiver, c"MTLRenderPassStencilAttachmentDescriptor") {
        AttachmentSlot::Stencil
    } else {
        AttachmentSlot::Color(0)
    };
    // SAFETY: a freshly allocated element; its body is not yet established.
    unsafe { object::init(receiver, AttachmentElement::new(std::ptr::null_mut(), slot)) };
    receiver
}

/// An element's own textures: a standalone one holds them, and an orphaned
/// one may have been given some after its descriptor went.
pub(super) unsafe extern "C" fn attachment_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: an attachment element of this crate's.
    let element = unsafe { object::body::<AttachmentElement>(receiver) };
    objc::arc::release(element.local.texture);
    objc::arc::release(element.local.resolve_texture);
    objc::arc::finalize(receiver);
}

pub(super) unsafe extern "C" fn attachment_object(receiver: Id, sel: Sel) -> Id {
    let Some(attachment) = attachment_of(receiver) else {
        return std::ptr::null_mut();
    };
    // SAFETY: a selector the runtime owns.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"texture" => attachment.texture,
        b"resolveTexture" => attachment.resolve_texture,
        _ => std::ptr::null_mut(),
    }
}

pub(super) unsafe extern "C" fn attachment_number(receiver: Id, sel: Sel) -> u64 {
    let Some(attachment) = attachment_of(receiver) else {
        return 0;
    };
    // SAFETY: a selector the runtime owns.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"level" => attachment.level,
        b"slice" => attachment.slice,
        b"loadAction" => attachment.load_action,
        b"storeAction" => attachment.store_action,
        b"clearStencil" => u64::from(attachment.clear_stencil),
        b"resolveLevel" => attachment.resolve_level,
        b"resolveSlice" => attachment.resolve_slice,
        b"depthResolveFilter" | b"stencilResolveFilter" => attachment.resolve_filter,
        _ => 0,
    }
}

pub(super) unsafe extern "C" fn attachment_clear_color(receiver: Id, _sel: Sel) -> ClearColor {
    let [red, green, blue, alpha] = attachment_of(receiver).map_or([0.0; 4], |a| a.clear_color);
    ClearColor {
        red,
        green,
        blue,
        alpha,
    }
}

pub(super) unsafe extern "C" fn attachment_clear_depth(receiver: Id, _sel: Sel) -> f64 {
    attachment_of(receiver).map_or(1.0, |a| a.clear_depth)
}

pub(super) unsafe extern "C" fn new_compute_encoder(receiver: Id, _sel: Sel) -> Id {
    new_encoder(receiver, Kind::Compute)
}

/// `MTLDispatchTypeConcurrent`.
const DISPATCH_CONCURRENT: u64 = 1;

pub(super) unsafe extern "C" fn new_compute_encoder_with_dispatch_type(
    receiver: Id,
    _sel: Sel,
    dispatch_type: u64,
) -> Id {
    let encoder = new_encoder(receiver, Kind::Compute);
    if !encoder.is_null() {
        // SAFETY: the encoder just made.
        unsafe { object::body::<Encoder>(encoder) }.concurrent =
            dispatch_type == DISPATCH_CONCURRENT;
    }
    encoder
}

pub(super) unsafe extern "C" fn new_blit_encoder(receiver: Id, _sel: Sel) -> Id {
    new_encoder(receiver, Kind::Blit)
}

pub(super) unsafe extern "C" fn new_render_encoder(receiver: Id, _sel: Sel, descriptor: Id) -> Id {
    if descriptor.is_null() {
        return std::ptr::null_mut();
    }
    let encoder = new_encoder(receiver, Kind::Render);
    if encoder.is_null() {
        return encoder;
    }
    // SAFETY: `MTLRenderPassDescriptor` is this crate's class.
    let described = unsafe { object::body::<RenderPassDescriptor>(descriptor) };
    begin_render_pass(encoder, described, false);
    encoder
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEPTH_STENCIL: vk::ImageAspectFlags = vk::ImageAspectFlags::from_raw(
        vk::ImageAspectFlags::DEPTH.as_raw() | vk::ImageAspectFlags::STENCIL.as_raw(),
    );

    /// A device which supports every mode for both aspects independently.
    fn everything() -> ResolveSupport {
        let all = vk::ResolveModeFlags::SAMPLE_ZERO
            | vk::ResolveModeFlags::AVERAGE
            | vk::ResolveModeFlags::MIN
            | vk::ResolveModeFlags::MAX;
        ResolveSupport {
            depth_modes: all,
            stencil_modes: all,
            independent_none: true,
            independent: true,
        }
    }

    /// The least a device supporting the feature may report: sample zero for
    /// both aspects, resolved together and with the same mode.
    fn minimal() -> ResolveSupport {
        ResolveSupport {
            depth_modes: vk::ResolveModeFlags::SAMPLE_ZERO,
            stencil_modes: vk::ResolveModeFlags::SAMPLE_ZERO,
            independent_none: false,
            independent: false,
        }
    }

    #[test]
    fn depth_resolve_filters_become_vulkan_modes() {
        let expected = [
            vk::ResolveModeFlags::SAMPLE_ZERO,
            vk::ResolveModeFlags::MIN,
            vk::ResolveModeFlags::MAX,
        ];
        for (filter, mode) in expected.into_iter().enumerate() {
            let (depth, stencil) = resolve_modes(
                everything(),
                vk::ImageAspectFlags::DEPTH,
                Some(filter as u64),
                None,
            )
            .expect("a supported depth filter");
            assert_eq!(depth, mode);
            assert_eq!(stencil, vk::ResolveModeFlags::NONE);
        }
    }

    #[test]
    fn one_sample_resolves_need_no_device_resolve_feature() {
        for depth_filter in 0..=2 {
            let (depth, stencil) =
                one_sample_resolve_modes(Some(depth_filter), Some(1)).expect("valid filters");
            assert_eq!(depth, vk::ResolveModeFlags::SAMPLE_ZERO);
            assert_eq!(stencil, vk::ResolveModeFlags::SAMPLE_ZERO);
        }
        assert!(one_sample_resolve_modes(Some(3), None).is_err());
        assert!(one_sample_resolve_modes(None, Some(2)).is_err());
    }

    #[test]
    fn both_aspects_resolve_with_sample_zero_on_a_minimal_device() {
        let (depth, stencil) =
            resolve_modes(minimal(), DEPTH_STENCIL, Some(0), Some(0)).expect("sample zero");
        assert_eq!(depth, vk::ResolveModeFlags::SAMPLE_ZERO);
        assert_eq!(stencil, vk::ResolveModeFlags::SAMPLE_ZERO);
    }

    #[test]
    fn a_resolve_mode_the_device_lacks_is_rejected() {
        let error = resolve_modes(minimal(), vk::ImageAspectFlags::DEPTH, Some(1), None)
            .expect_err("minimum is optional");
        assert!(error.contains("MIN"), "{error}");
    }

    #[test]
    fn the_depth_resolved_stencil_sample_filter_is_rejected() {
        let error = resolve_modes(everything(), DEPTH_STENCIL, Some(0), Some(1))
            .expect_err("no Vulkan stencil mode follows the depth filter");
        assert!(
            error.contains("MTLMultisampleStencilResolveFilter 1"),
            "{error}"
        );
    }

    #[test]
    fn resolving_one_aspect_alone_needs_independent_resolve_none() {
        assert!(resolve_modes(minimal(), DEPTH_STENCIL, Some(0), None).is_err());
        let support = ResolveSupport {
            independent_none: true,
            ..minimal()
        };
        let (depth, stencil) =
            resolve_modes(support, DEPTH_STENCIL, Some(0), None).expect("one aspect alone");
        assert_eq!(depth, vk::ResolveModeFlags::SAMPLE_ZERO);
        assert_eq!(stencil, vk::ResolveModeFlags::NONE);
    }

    #[test]
    fn two_modes_on_one_attachment_need_independent_resolve() {
        let support = ResolveSupport {
            depth_modes: vk::ResolveModeFlags::SAMPLE_ZERO | vk::ResolveModeFlags::MIN,
            independent_none: true,
            ..minimal()
        };
        let error = resolve_modes(support, DEPTH_STENCIL, Some(1), Some(0))
            .expect_err("differing modes need independent resolve");
        assert!(error.contains("one mode for both"), "{error}");
        let support = ResolveSupport {
            independent: true,
            ..support
        };
        assert!(resolve_modes(support, DEPTH_STENCIL, Some(1), Some(0)).is_ok());
    }

    #[test]
    fn an_aspect_the_attachment_does_not_have_is_rejected() {
        let error = resolve_modes(everything(), vk::ImageAspectFlags::DEPTH, None, Some(0))
            .expect_err("no stencil aspect");
        assert!(error.contains("no stencil aspect"), "{error}");
    }

    #[test]
    fn attachments_held_across_init_are_orphaned_when_their_descriptor_goes() {
        crate::bootstrap();
        let pass = object::make(c"MTLRenderPassDescriptor", RenderPassDescriptor::default());
        // SAFETY: the registered methods of a descriptor this test made.
        unsafe {
            let colors = objc::arc::retain(pass_color_attachments(
                pass,
                objc::sel::get("colorAttachments"),
            ));
            let depth = objc::arc::retain(pass_single_attachment(
                pass,
                objc::sel::get("depthAttachment"),
            ));
            let color = object::body::<crate::resource::IndexedCollection>(colors).elements[0];
            set_attachment_number(color, objc::sel::get("setLoadAction:"), 2);
            render_pass_descriptor_init(pass, objc::sel::get("init"));
            assert_eq!(
                pass_color_attachments(pass, objc::sel::get("colorAttachments")),
                colors
            );
            assert_eq!(attachment_of(color).map(|a| a.load_action), Some(0));

            objc::arc::release(pass);
            for element in [color, depth] {
                assert!(object::body::<AttachmentElement>(element).owner.is_null());
            }
            objc::arc::release(colors);
            objc::arc::release(depth);
        }
    }
}
