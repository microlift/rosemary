//! What a render encoder's Vulkan pass begins with.
//!
//! On a device with dynamic rendering a pass names its attachments as it
//! begins, and a pipeline only their formats, so neither the pass's load,
//! store and resolve actions nor a framebuffer reach pipeline creation.
//! Older devices begin a cached render pass over a framebuffer made per pass.

use super::*;

/// One attachment of a pass begun with dynamic rendering.
#[derive(Clone, Copy)]
pub(super) struct RenderingAttachment {
    view: vk::ImageView,
    load: vk::AttachmentLoadOp,
    store: vk::AttachmentStoreOp,
    clear: vk::ClearValue,
    resolve_mode: vk::ResolveModeFlags,
    resolve_view: vk::ImageView,
}

impl RenderingAttachment {
    /// What the pass stored is what its continuation loads.
    fn info(&self, resume: bool) -> vk::RenderingAttachmentInfo<'static> {
        let load = if resume && self.store == vk::AttachmentStoreOp::STORE {
            vk::AttachmentLoadOp::LOAD
        } else {
            self.load
        };
        vk::RenderingAttachmentInfo::default()
            .image_view(self.view)
            .image_layout(vk::ImageLayout::GENERAL)
            .load_op(load)
            .store_op(self.store)
            .clear_value(self.clear)
            .resolve_mode(self.resolve_mode)
            .resolve_image_view(self.resolve_view)
            .resolve_image_layout(vk::ImageLayout::GENERAL)
    }
}

pub(super) enum PassTarget {
    RenderPass {
        key: RenderPassKey,
        pass: vk::RenderPass,
        framebuffer: vk::Framebuffer,
        clears: Vec<vk::ClearValue>,
    },
    Rendering {
        colors: Vec<RenderingAttachment>,
        depth: Option<RenderingAttachment>,
        stencil: Option<RenderingAttachment>,
    },
}

/// The views and clear values of one pass's attachments, in the order
/// `RenderPassKey` lists them: colours, then depth/stencil.
pub(super) struct AttachmentViews {
    pub views: Vec<vk::ImageView>,
    /// One per colour `RenderPassKey::color_resolves` names, in order.
    pub resolve_views: Vec<vk::ImageView>,
    pub depth_resolve_view: Option<vk::ImageView>,
    pub clears: Vec<vk::ClearValue>,
}

impl PassTarget {
    /// The target a pass of shape `key` over `attachments` begins with on
    /// `gpu`. A framebuffer made for it is the caller's to destroy once the
    /// command buffer has retired.
    pub(super) fn new(
        gpu: &Gpu,
        key: &RenderPassKey,
        attachments: AttachmentViews,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let AttachmentViews {
            mut views,
            resolve_views,
            depth_resolve_view,
            clears,
        } = attachments;
        if !gpu.dynamic_rendering {
            // The render pass declares the colour resolve attachments after
            // the depth one, and the depth resolve after those.
            views.extend(resolve_views);
            views.extend(depth_resolve_view);
            let pass = gpu
                .render_pass(key)
                .map_err(|reason| format!("render pass creation failed: {reason}"))?;
            let framebuffer = gpu
                .create_framebuffer(pass, &views, width, height, 1)
                .map_err(|reason| format!("framebuffer creation failed: {reason}"))?;
            return Ok(Self::RenderPass {
                key: key.clone(),
                pass,
                framebuffer,
                clears,
            });
        }
        let mut resolve_views = resolve_views.into_iter();
        let colors = key
            .colors
            .iter()
            .zip(&key.color_resolves)
            .enumerate()
            .map(|(index, (&(format, load, store), resolve))| {
                let (resolve_mode, resolve_view) = match resolve {
                    // Vulkan's render pass resolve takes sample zero of an
                    // integer format, which is what Metal leaves there too.
                    Some(_) if gpu::is_integer(format) => (
                        vk::ResolveModeFlags::SAMPLE_ZERO,
                        resolve_views.next().expect("a colour resolve view"),
                    ),
                    Some(_) => (
                        vk::ResolveModeFlags::AVERAGE,
                        resolve_views.next().expect("a colour resolve view"),
                    ),
                    None => (vk::ResolveModeFlags::NONE, vk::ImageView::null()),
                };
                RenderingAttachment {
                    view: views[index],
                    load,
                    store,
                    clear: clears[index],
                    resolve_mode,
                    resolve_view,
                }
            })
            .collect::<Vec<_>>();
        let (depth_format, stencil_format) = key.depth_stencil_formats();
        let depth_stencil = key.depth.map(|(_, load, store)| {
            let index = key.colors.len();
            let attachment = RenderingAttachment {
                view: views[index],
                load,
                store,
                clear: clears[index],
                resolve_mode: vk::ResolveModeFlags::NONE,
                resolve_view: vk::ImageView::null(),
            };
            let resolving = |mode: vk::ResolveModeFlags| {
                if mode.is_empty() {
                    attachment
                } else {
                    RenderingAttachment {
                        resolve_mode: mode,
                        resolve_view: depth_resolve_view.expect("a depth resolve view"),
                        ..attachment
                    }
                }
            };
            let depth_mode = key
                .depth_resolve
                .map_or(vk::ResolveModeFlags::NONE, |r| r.depth_mode);
            let stencil_mode = key
                .depth_resolve
                .map_or(vk::ResolveModeFlags::NONE, |r| r.stencil_mode);
            // A render pass attachment holds both aspects of its format, and
            // one the pass did not name is neither loaded nor stored.
            let (stencil_load, stencil_store) = key.stencil.unwrap_or((
                vk::AttachmentLoadOp::DONT_CARE,
                vk::AttachmentStoreOp::DONT_CARE,
            ));
            (
                resolving(depth_mode),
                RenderingAttachment {
                    load: stencil_load,
                    store: stencil_store,
                    ..resolving(stencil_mode)
                },
            )
        });
        Ok(Self::Rendering {
            colors,
            depth: depth_stencil
                .filter(|_| depth_format != vk::Format::UNDEFINED)
                .map(|(depth, _)| depth),
            stencil: depth_stencil
                .filter(|_| stencil_format != vk::Format::UNDEFINED)
                .map(|(_, stencil)| stencil),
        })
    }

    /// The framebuffer the command buffer has to destroy once it retires.
    pub(super) fn framebuffer(&self) -> Option<vk::Framebuffer> {
        match self {
            Self::RenderPass { framebuffer, .. } => Some(*framebuffer),
            Self::Rendering { .. } => None,
        }
    }

    /// Begin the pass over `area`; `secondary` a pass whose draws are
    /// recorded into secondary command buffers.
    pub(super) fn begin(
        &self,
        gpu: &Gpu,
        command_buffer: vk::CommandBuffer,
        area: vk::Rect2D,
        secondary: bool,
    ) {
        self.record(gpu, command_buffer, area, None, secondary);
    }

    /// Begin the pass's continuation after a split, which keeps what it drew
    /// so far. A render pass needs one of its own for that, which can fail.
    pub(super) fn resume(
        &self,
        gpu: &Gpu,
        command_buffer: vk::CommandBuffer,
        area: vk::Rect2D,
    ) -> Result<(), String> {
        let pass = match self {
            Self::RenderPass { key, .. } => {
                let mut key = key.clone();
                for color in &mut key.colors {
                    color.1 = vk::AttachmentLoadOp::LOAD;
                }
                if let Some(depth) = &mut key.depth {
                    depth.1 = vk::AttachmentLoadOp::LOAD;
                }
                if let Some(stencil) = &mut key.stencil {
                    stencil.0 = vk::AttachmentLoadOp::LOAD;
                }
                // Load operations aside the pass is the same one, so the
                // framebuffer and every pipeline made for it still fit.
                gpu.render_pass(&key)?
            }
            Self::Rendering { .. } => vk::RenderPass::null(),
        };
        self.record(gpu, command_buffer, area, Some(pass), false);
        Ok(())
    }

    /// `resumed` is the render pass a continuation begins, null where the
    /// device renders dynamically, and `None` for the pass's first beginning.
    fn record(
        &self,
        gpu: &Gpu,
        command_buffer: vk::CommandBuffer,
        area: vk::Rect2D,
        resumed: Option<vk::RenderPass>,
        secondary: bool,
    ) {
        match self {
            Self::RenderPass {
                pass,
                framebuffer,
                clears,
                ..
            } => {
                let mut begin = vk::RenderPassBeginInfo::default()
                    .render_pass(resumed.unwrap_or(*pass))
                    .framebuffer(*framebuffer)
                    .render_area(area);
                if resumed.is_none() {
                    begin = begin.clear_values(clears);
                }
                let contents = if secondary {
                    vk::SubpassContents::SECONDARY_COMMAND_BUFFERS
                } else {
                    vk::SubpassContents::INLINE
                };
                // SAFETY: recording into this command buffer, outside a pass.
                unsafe {
                    gpu.device
                        .cmd_begin_render_pass(command_buffer, &begin, contents)
                };
            }
            Self::Rendering {
                colors,
                depth,
                stencil,
            } => {
                let resume = resumed.is_some();
                let colors = colors
                    .iter()
                    .map(|color| color.info(resume))
                    .collect::<Vec<_>>();
                let depth = depth.map(|depth| depth.info(resume));
                let stencil = stencil.map(|stencil| stencil.info(resume));
                let mut info = vk::RenderingInfo::default()
                    .render_area(area)
                    .layer_count(1)
                    .color_attachments(&colors);
                if let Some(depth) = &depth {
                    info = info.depth_attachment(depth);
                }
                if let Some(stencil) = &stencil {
                    info = info.stencil_attachment(stencil);
                }
                if secondary {
                    info = info.flags(vk::RenderingFlags::CONTENTS_SECONDARY_COMMAND_BUFFERS);
                }
                // SAFETY: recording into this command buffer, outside a pass;
                // every borrowed attachment lives across the call.
                unsafe { gpu.device.cmd_begin_rendering(command_buffer, &info) };
            }
        }
    }

    /// What a secondary command buffer recording into this pass inherits.
    pub(super) fn inheritance(&self, key: &RenderPassKey) -> Inheritance {
        match self {
            Self::RenderPass {
                pass, framebuffer, ..
            } => Inheritance::RenderPass {
                pass: *pass,
                framebuffer: *framebuffer,
            },
            Self::Rendering { .. } => {
                let (depth, stencil) = key.depth_stencil_formats();
                Inheritance::Rendering {
                    colors: key.colors.iter().map(|color| color.0).collect(),
                    depth,
                    stencil,
                    samples: key.samples,
                }
            }
        }
    }
}

/// End the pass recording into `command_buffer`, however it began.
pub(super) fn end_pass(gpu: &Gpu, command_buffer: vk::CommandBuffer) {
    // SAFETY: a pass began in this command buffer, as `PassTarget::begin`
    // begins one on this device.
    unsafe {
        if gpu.dynamic_rendering {
            gpu.device.cmd_end_rendering(command_buffer);
        } else {
            gpu.device.cmd_end_render_pass(command_buffer);
        }
    }
}

/// The pass a parallel encoder began, as its sub-encoders' secondary command
/// buffers inherit it.
pub(super) enum Inheritance {
    RenderPass {
        pass: vk::RenderPass,
        framebuffer: vk::Framebuffer,
    },
    Rendering {
        colors: Vec<vk::Format>,
        depth: vk::Format,
        stencil: vk::Format,
        samples: vk::SampleCountFlags,
    },
}

impl Inheritance {
    /// Begin recording `command_buffer` as a continuation of this pass.
    pub(super) fn begin_secondary(
        &self,
        device: &ash::Device,
        command_buffer: vk::CommandBuffer,
    ) -> Result<(), vk::Result> {
        let mut rendering = vk::CommandBufferInheritanceRenderingInfo::default();
        let mut inheritance = vk::CommandBufferInheritanceInfo::default();
        match self {
            Self::RenderPass { pass, framebuffer } => {
                inheritance = inheritance
                    .render_pass(*pass)
                    .subpass(0)
                    .framebuffer(*framebuffer);
            }
            Self::Rendering {
                colors,
                depth,
                stencil,
                samples,
            } => {
                rendering = rendering
                    .color_attachment_formats(colors)
                    .depth_attachment_format(*depth)
                    .stencil_attachment_format(*stencil)
                    .rasterization_samples(*samples);
                inheritance = inheritance.push_next(&mut rendering);
            }
        }
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(
                vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT
                    | vk::CommandBufferUsageFlags::RENDER_PASS_CONTINUE,
            )
            .inheritance_info(&inheritance);
        // SAFETY: the command buffer is fresh or was reset with its pool, and
        // the inheritance structures live across the call.
        unsafe { device.begin_command_buffer(command_buffer, &begin) }
    }
}
