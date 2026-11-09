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

// FIXME (ACCURACY): DONT_CARE semantics probably follow the same rule for loads as they
// do for stores that caused us lots of problems. TODO: should we just remove DONT_CARE here?
pub(super) fn load_op(metal: u64) -> vk::AttachmentLoadOp {
    match metal {
        1 => vk::AttachmentLoadOp::LOAD,
        2 => vk::AttachmentLoadOp::CLEAR,
        _ => vk::AttachmentLoadOp::DONT_CARE,
    }
}

/// Whether an `MTLStoreAction` asks for a multisample resolve:
/// `MultisampleResolve` is 2 and `StoreAndMultisampleResolve` is 3.
pub(super) fn resolves(metal: u64) -> bool {
    metal == 2 || metal == 3
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
    let texture = unsafe { object::body::<Texture>(color.resolve_texture) };
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

/// The depth/stencil resolve one `MTLRenderPassDescriptor` asks for.
///
/// Metal names a resolve destination on the depth attachment and another on
/// the stencil one; a Vulkan subpass has a single depth/stencil resolve
/// attachment with a mode per aspect, so the two must name the same texture.
/// A resolve this cannot express is reported and dropped, which leaves the
/// destination as it was rather than holding one arbitrary sample.
///
/// `attached` is the texture the pass attached, which is the depth one where
/// it has both: the aspect whose texture is not attached is not resolved
/// either, because it is not rendered into.
fn depth_resolve(
    gpu: &Gpu,
    descriptor: &RenderPassDescriptor,
    attached: Id,
    format: vk::Format,
    aspect: vk::ImageAspectFlags,
) -> Option<DepthResolveTarget> {
    let wants = |attachment: &AttachmentDescriptor| {
        attachment.texture == attached
            && resolves(attachment.store_action)
            && !attachment.resolve_texture.is_null()
    };
    let depth = wants(&descriptor.depth).then_some(&descriptor.depth);
    let mut stencil = wants(&descriptor.stencil).then_some(&descriptor.stencil);
    if let (Some(from_depth), Some(from_stencil)) = (depth, stencil)
        && from_depth.resolve_texture != from_stencil.resolve_texture
    {
        gpu::trace_once(
            "a render pass names separate depth and stencil resolve textures, which one Vulkan \
             resolve attachment cannot hold: the stencil one is ignored",
        );
        stencil = None;
    }
    let named = depth.or(stencil)?;
    // SAFETY: only a texture of this crate's is attached.
    let destination = unsafe { object::body::<Texture>(named.resolve_texture) };
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
        aspect,
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
    /// Copies Metal's `MTLStoreActionStoreAndMultisampleResolve` implies.
    copies: Vec<PendingCopy>,
    capture_targets: Vec<crate::capture::Target>,
    clears: Vec<vk::ClearValue>,
    /// The smallest attachment extent, which is the pass's own.
    width: u32,
    height: u32,
    /// The textures the pass reads or writes, retained for its lifetime.
    textures: Vec<Id>,
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
fn gather_attachments(gpu: &Gpu, descriptor: &RenderPassDescriptor) -> Attachments {
    let mut key = RenderPassKey {
        colors: Vec::new(),
        color_resolves: Vec::new(),
        depth: None,
        stencil: None,
        depth_resolve: None,
        samples: vk::SampleCountFlags::TYPE_1,
    };
    let mut views = Vec::new();
    let mut resolve_views = Vec::new();
    let mut resolve_mask = 0u32;
    let mut depth_resolve_view = None;
    let mut copies: Vec<PendingCopy> = Vec::new();
    let mut capture_targets: Vec<crate::capture::Target> = Vec::new();
    let mut clears = Vec::new();
    let mut width = u32::MAX;
    let mut height = u32::MAX;
    let mut textures = Vec::new();
    // The targets only matter to `ROSEMARY_PASS_OUT`, and naming each one
    // formats a string.
    let capturing = crate::capture::enabled();

    // FIXME (ACCURACY): a render-target index with no texture closes up rather
    // than staying a gap, so a pass which attaches targets 0 and 2 describes
    // two attachments and the fragment shader's `[[color(2)]]` lands on the
    // second of them. Representing the gap needs a `VK_ATTACHMENT_UNUSED`
    // reference here and the matching hole in the pipeline's colour list.
    let last_color = descriptor
        .colors
        .iter()
        .rposition(|color| !color.texture.is_null());
    for (index, color) in descriptor.colors.iter().enumerate() {
        if color.texture.is_null() {
            if last_color.is_some_and(|last| index < last) {
                gpu::trace_once(
                    "a render pass leaves a gap in its colour attachments, which this runtime \
                     closes up: the targets after the gap are misrouted",
                );
            }
            continue;
        }
        // SAFETY: only a texture of this crate's is attached.
        let texture = unsafe { object::body::<Texture>(color.texture) };
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
                resolve_mask |= 1 << (key.colors.len() - 1);
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
                role: format!("c{}", key.colors.len() - 1),
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
                role: format!("r{}", key.colors.len() - 1),
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
    // FIXME (ACCURACY): Metal allows a depth texture and a separate stencil
    // texture on one pass; a Vulkan subpass has one depth/stencil attachment,
    // so only the depth one is attached and the stencil texture is neither
    // loaded nor stored. A combined depth/stencil texture named in both slots
    // - which is what a game that wants both normally does - is unaffected.
    if !descriptor.depth.texture.is_null()
        && !descriptor.stencil.texture.is_null()
        && descriptor.depth.texture != descriptor.stencil.texture
    {
        gpu::trace_once(
            "a render pass names separate depth and stencil textures, which one Vulkan \
             attachment cannot hold: the stencil texture is ignored",
        );
    }
    let depth_attachment = if !descriptor.depth.texture.is_null() {
        Some(&descriptor.depth)
    } else if !descriptor.stencil.texture.is_null() {
        Some(&descriptor.stencil)
    } else {
        None
    };
    if let Some(attachment) = depth_attachment {
        // SAFETY: only a texture of this crate's is attached.
        let texture = unsafe { object::body::<Texture>(attachment.texture) };
        key.depth = Some((
            texture.image.format,
            load_op(attachment.load_action),
            store_op(),
        ));
        if !descriptor.stencil.texture.is_null() {
            key.stencil = Some((load_op(descriptor.stencil.load_action), store_op()));
        }
        let first = key.colors.is_empty();
        samples(&mut key, texture.image.samples, first);
        let level = attachment.level as u32 + texture.image.base_level;
        let depth_slice = attachment.slice as u32 + texture.image.base_layer;
        views.push(texture.attachment_view(level, depth_slice));
        let (format, aspect) = (texture.image.format, texture.image.aspect);
        let multisampled = texture.image.samples != vk::SampleCountFlags::TYPE_1;
        let (base_width, base_height) = (texture.image.width, texture.image.height);
        let source_image = texture.image.image;
        if let Some(resolve) = depth_resolve(gpu, descriptor, attachment.texture, format, aspect) {
            textures.push(resolve.texture);
            if multisampled {
                key.depth_resolve = Some(resolve.resolve);
                depth_resolve_view = Some(resolve.view);
            } else {
                // A subpass resolves only a multisampled attachment; with one
                // sample the same store action means a plain copy, as it does
                // for a colour attachment.
                copies.push(PendingCopy {
                    source: source_image,
                    source_level: level,
                    source_slice: depth_slice,
                    destination: resolve.image,
                    destination_level: resolve.level,
                    destination_slice: resolve.slice,
                    width: level_extent(base_width, level),
                    height: level_extent(base_height, level),
                    aspect,
                });
            }
            if capturing {
                capture_targets.push(crate::capture::Target {
                    image: resolve.image,
                    aspect: vk::ImageAspectFlags::DEPTH,
                    level: resolve.level,
                    slice: resolve.slice,
                    width: level_extent(base_width, level),
                    height: level_extent(base_height, level),
                    format,
                    samples: vk::SampleCountFlags::TYPE_1,
                    role: "rd".to_owned(),
                });
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
    Attachments {
        key,
        views,
        resolve_views,
        resolve_mask,
        depth_resolve_view,
        copies,
        capture_targets,
        clears,
        width,
        height,
        textures,
    }
}

/// Metal's default render state for a pass, which a Vulkan command buffer
/// has to be told: dynamic state is not inherited by a secondary one.
pub(super) fn record_pass_defaults(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    area: vk::Rect2D,
    depth_bias: &mut RecordedDepthBias,
) {
    depth_bias.record(device, command_buffer, DepthBias::NONE);
    let (width, height) = (area.extent.width, area.extent.height);
    // SAFETY: recording into a command buffer inside the pass.
    unsafe {
        // Metal's default viewport and scissor cover the attachments, and the
        // viewport is flipped for the same reason `-setViewport:` flips.
        device.cmd_set_viewport(
            command_buffer,
            0,
            &[vk::Viewport {
                x: 0.0,
                y: height as f32,
                width: width as f32,
                height: -(height as f32),
                min_depth: 0.0,
                max_depth: 1.0,
            }],
        );
        device.cmd_set_scissor(command_buffer, 0, &[area]);
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

    let Attachments {
        key,
        views,
        resolve_views,
        resolve_mask,
        depth_resolve_view,
        copies,
        capture_targets,
        clears,
        width,
        height,
        textures,
    } = gather_attachments(gpu, descriptor);
    if views.is_empty() {
        gpu::trace("renderCommandEncoderWithDescriptor: has no attachments");
        return;
    }
    let width = width.max(1);
    let height = height.max(1);
    let attachment_views = AttachmentViews {
        views,
        resolve_views,
        depth_resolve_view,
        clears,
    };
    let target = match PassTarget::new(gpu, &key, attachment_views, width, height) {
        Ok(target) => target,
        Err(reason) => return gpu::trace(reason),
    };
    command.framebuffers.extend(target.framebuffer());
    for texture in textures {
        // SAFETY: only textures of this crate's are attached.
        let image = &unsafe { object::body::<Texture>(texture) }.image;
        encoder
            .attachments
            .push((image.image, gpu::aspect_of(image.format)));
        // Memory another resource may share is not ordered by its image.
        encoder.writes_elsewhere |= hazards::tracked_image(texture).is_none();
        command.retain(texture);
    }
    // Sub-encoders record into command buffers of their own, query results
    // land in a buffer, and a resolve copy writes through a transfer: each
    // reaches memory other than through the attachments.
    encoder.writes_elsewhere |=
        secondary || !descriptor.visibility_result_buffer.is_null() || !copies.is_empty();
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
    if !secondary && encoder.visibility.is_none() && encoder.capture_pass.is_none() {
        record_pass_defaults(
            &gpu.device,
            command.command_buffer,
            area,
            &mut encoder.depth_bias,
        );
        let is_clear = |op: vk::AttachmentLoadOp| op == vk::AttachmentLoadOp::CLEAR;
        let works_undrawn = key.colors.iter().any(|color| is_clear(color.1))
            || key.depth.is_some_and(|depth| is_clear(depth.1))
            || key.stencil.is_some_and(|stencil| is_clear(stencil.0))
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
        );
    }
    gpu::milestone("render pass");
    encoder.pending_copies = copies;
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
            local: AttachmentDescriptor::default(),
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
    Some(match element.slot {
        AttachmentSlot::Color(index) => &mut descriptor.colors[index],
        AttachmentSlot::Depth => &mut descriptor.depth,
        AttachmentSlot::Stencil => &mut descriptor.stencil,
    })
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
                if let Some(destination) = attachment_of(element) {
                    assign_attachment(destination, source);
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
    let value =
        match !value.is_null() && unsafe { object::body::<Texture>(value) }.is_texture_buffer() {
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
pub(super) fn assign_attachment(destination: &mut AttachmentDescriptor, source: Id) {
    let value = if is_attachment(source) {
        attachment_of(source).map_or_else(AttachmentDescriptor::default, |a| *a)
    } else {
        AttachmentDescriptor::default()
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
    let depth = unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() == b"setDepthAttachment:";
    assign_attachment(
        if depth {
            &mut descriptor.depth
        } else {
            &mut descriptor.stencil
        },
        value,
    );
}

/// `-init` of a standalone attachment descriptor.
pub(super) unsafe extern "C" fn attachment_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a freshly allocated element; its body is not yet established.
    unsafe {
        object::init(
            receiver,
            AttachmentElement::new(std::ptr::null_mut(), AttachmentSlot::Color(0)),
        )
    };
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
