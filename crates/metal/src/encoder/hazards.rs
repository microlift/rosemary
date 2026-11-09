//! Ordering render passes by the resources they share.
//!
//! Metal orders one pass after another only where they conflict: where one
//! writes a tracked resource the other reads or writes. Passes which share
//! nothing may overlap on the device. So a render pass which wrote nothing
//! but its attachments leaves no barrier behind it; its attachments, and the
//! textures it read, are remembered here, and the first later work which
//! touches them waits.
//!
//! A pass's attachments are known when it is made, but what it samples is
//! only known as its textures are bound, so its Vulkan render pass begins
//! at its first draw. A texture an earlier pass wrote which is bound after
//! drawing began ends the Vulkan pass, waits, and continues it.
//!
//! Only images are tracked. A pass which writes anything else waits for
//! every pass before it and ends with a full barrier, and so does every
//! compute and blit encoder, as do resources an argument buffer reaches
//! (`-useResource:`) and fence waits, which name no image.

use super::*;

/// Attachment writes and texture reads of render passes which ended since
/// the command buffer's last full barrier.
#[derive(Default)]
pub(super) struct Hazards {
    /// Images those passes wrote as attachments, with their aspects.
    written: Vec<(vk::Image, vk::ImageAspectFlags)>,
    /// Images they read which a later pass could attach.
    read: Vec<vk::Image>,
}

impl Hazards {
    fn is_empty(&self) -> bool {
        self.written.is_empty() && self.read.is_empty()
    }

    fn wrote(&self, image: vk::Image) -> bool {
        self.written.iter().any(|(written, _)| *written == image)
    }

    pub(super) fn note_written(&mut self, image: vk::Image, aspect: vk::ImageAspectFlags) {
        if !self.wrote(image) {
            self.written.push((image, aspect));
        }
    }

    fn note_read(&mut self, image: vk::Image) {
        if !self.read.contains(&image) {
            self.read.push(image);
        }
    }

    pub(super) fn clear(&mut self) {
        self.written.clear();
        self.read.clear();
    }
}

/// A render pass whose Vulkan pass begins at its first draw.
pub(super) struct LazyPass {
    pub target: PassTarget,
    pub area: vk::Rect2D,
    /// Whether the Vulkan pass has begun.
    pub begun: bool,
    /// Whether the pass changes its attachments with nothing drawn: a clear
    /// or a resolve.
    pub works_undrawn: bool,
    /// Images this pass reads which a later pass could attach.
    pub reads: Vec<vk::Image>,
    /// Whether a texture an earlier pass wrote has been bound since this
    /// pass was last ordered after the passes before it.
    pub read_hazard: bool,
}

/// The image a texture's memory is reached through, where no other image
/// shares that memory: `None` for a texture buffer, whose memory is a
/// buffer's, and for a texture placed in a heap, which another may alias.
pub(super) fn tracked_image(texture: Id) -> Option<vk::Image> {
    let mut texture = texture;
    while !texture.is_null() {
        // SAFETY: only textures of this crate's are bound or attached, and
        // a view retains the texture it sees into.
        let body = unsafe { object::body::<Texture>(texture) };
        if body.parent.is_null() {
            let image = &body.image;
            return (image.image != vk::Image::null() && image.placement.is_none())
                .then_some(image.image);
        }
        texture = body.parent;
    }
    None
}

/// Image barriers making attachment writes to `images` visible to `access`.
fn attachment_barriers(
    images: &[(vk::Image, vk::ImageAspectFlags)],
    access: vk::AccessFlags,
) -> Vec<vk::ImageMemoryBarrier<'static>> {
    images
        .iter()
        .map(|&(image, aspect)| {
            vk::ImageMemoryBarrier::default()
                .src_access_mask(
                    vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
                )
                .dst_access_mask(access)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: aspect,
                    base_mip_level: 0,
                    level_count: vk::REMAINING_MIP_LEVELS,
                    base_array_layer: 0,
                    layer_count: vk::REMAINING_ARRAY_LAYERS,
                })
        })
        .collect()
}

/// The stages a render pass reaches its attachments in, loads and stores
/// included.
const ATTACHMENT_STAGES: vk::PipelineStageFlags = vk::PipelineStageFlags::from_raw(
    vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS.as_raw()
        | vk::PipelineStageFlags::LATE_FRAGMENT_TESTS.as_raw()
        | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT.as_raw(),
);

pub(super) fn count_barrier() {
    stats::count(Counter::Barriers);
    #[cfg(test)]
    TEST_RECORDED_BARRIERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

impl CommandBuffer {
    /// Wait for every pass which ended since the last full barrier, and make
    /// what they wrote to their attachments visible to everything after.
    pub(super) fn flush_hazards(&mut self) {
        if self.hazards.is_empty() {
            return;
        }
        let barriers = attachment_barriers(
            &self.hazards.written,
            vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
        );
        // SAFETY: recording into this command buffer, outside a render pass;
        // every attachment image lives in GENERAL.
        unsafe {
            self.gpu.device.cmd_pipeline_barrier(
                self.command_buffer,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &barriers,
            )
        };
        self.hazards.clear();
        count_barrier();
    }

    /// Order a pass's attachment accesses after earlier passes' writes to
    /// the same images. Only attachment stages wait, and the writes stay
    /// pending for a later read through a shader or a copy.
    fn order_attachment_writes(&mut self, images: &[(vk::Image, vk::ImageAspectFlags)]) {
        let barriers = attachment_barriers(
            images,
            vk::AccessFlags::COLOR_ATTACHMENT_READ
                | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
        );
        // SAFETY: as in `flush_hazards`.
        unsafe {
            self.gpu.device.cmd_pipeline_barrier(
                self.command_buffer,
                ATTACHMENT_STAGES,
                ATTACHMENT_STAGES,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &barriers,
            )
        };
        count_barrier();
    }
}

impl Encoder {
    /// Note that the render pass being recorded binds `texture` for reading.
    pub(super) fn note_read(&mut self, texture: Id) {
        if self.lazy.is_none() {
            return;
        }
        let Some(image) = tracked_image(texture) else {
            return;
        };
        let wrote = self.command().hazards.wrote(image);
        // SAFETY: a texture of this crate's, as `tracked_image` found.
        let usage = unsafe { object::body::<Texture>(texture) }.image.usage;
        let lazy = self.lazy.as_mut().expect("a lazy pass");
        lazy.read_hazard |= wrote;
        if usage.intersects(
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
        ) && !lazy.reads.contains(&image)
        {
            lazy.reads.push(image);
        }
    }

    /// Whether the pass must wait for every pass before it: it reads what
    /// one wrote, or writes memory other than its attachments, which an
    /// earlier pass may have read.
    fn must_flush(&self, lazy: &LazyPass) -> bool {
        let hazards = &self.command().hazards;
        lazy.read_hazard || (self.writes_elsewhere && !hazards.is_empty())
    }

    /// Begin the Vulkan render pass, ordered after the earlier passes it
    /// conflicts with. Once it has begun, a conflict which arose since ends
    /// it, waits, and continues it.
    ///
    /// `false` where the pass could not continue, and records nothing more.
    pub(super) fn ensure_pass(&mut self) -> bool {
        let Some(lazy) = self.lazy.as_ref() else {
            return true;
        };
        let must_flush = self.must_flush(lazy);
        let command = self.command();
        if lazy.begun {
            if !must_flush {
                return true;
            }
            end_pass(command.gpu, command.command_buffer);
            // What the pass drew so far is what its continuation loads.
            for &(image, aspect) in &self.attachments {
                command.hazards.note_written(image, aspect);
            }
            command.flush_hazards();
            let lazy = self.lazy.as_mut().expect("a lazy pass");
            lazy.read_hazard = false;
            let resumed = lazy
                .target
                .resume(command.gpu, command.command_buffer, lazy.area);
            match resumed {
                Ok(()) => stats::count(Counter::PassSplits),
                Err(reason) => {
                    // Beginning the original pass again would clear what it
                    // drew, so the rest of the pass is dropped instead.
                    gpu::trace(format!(
                        "render pass continuation creation failed, dropping the rest of the \
                         pass: {reason}"
                    ));
                    self.lazy = None;
                    self.in_render_pass = false;
                    return false;
                }
            }
            return true;
        }
        let rereads = self
            .attachments
            .iter()
            .any(|(image, _)| command.hazards.read.contains(image));
        if must_flush || rereads {
            command.flush_hazards();
        } else {
            let rewritten: Vec<_> = self
                .attachments
                .iter()
                .copied()
                .filter(|(image, _)| command.hazards.wrote(*image))
                .collect();
            if !rewritten.is_empty() {
                command.order_attachment_writes(&rewritten);
            }
        }
        let lazy = self.lazy.as_mut().expect("a lazy pass");
        lazy.read_hazard = false;
        lazy.begun = true;
        lazy.target
            .begin(command.gpu, command.command_buffer, lazy.area, false);
        true
    }

    /// End a render pass which began lazily. A pass which drew nothing and
    /// neither clears nor resolves changes nothing, and is not recorded.
    ///
    /// `false` where the pass was dropped.
    pub(super) fn end_lazy_pass(&mut self) -> bool {
        let Some(lazy) = self.lazy.as_ref() else {
            return true;
        };
        if !lazy.begun {
            if !lazy.works_undrawn {
                stats::count(Counter::EmptyPasses);
                return false;
            }
            // Only a split can fail, and this pass has not begun.
            self.ensure_pass();
        }
        let command = self.command();
        end_pass(command.gpu, command.command_buffer);
        true
    }

    /// Leave a lazily begun pass which wrote only its attachments to be
    /// ordered against later work by what it touched, rather than by a
    /// barrier now.
    pub(super) fn leave_pending(&mut self) {
        let Some(lazy) = self.lazy.take() else {
            return;
        };
        let hazards = &mut self.command().hazards;
        for &(image, aspect) in &self.attachments {
            hazards.note_written(image, aspect);
        }
        for image in lazy.reads {
            hazards.note_read(image);
        }
    }
}
