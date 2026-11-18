//! Ordering render passes and blits by the resources they share.
//!
//! Metal orders one pass after another only where they conflict: where one
//! writes a tracked resource the other reads or writes. Passes which share
//! nothing may overlap on the device. So a render pass which wrote nothing
//! but its attachments leaves no barrier behind it; its attachments, and the
//! textures it read, are remembered here, and the first later work which
//! touches them waits. A blit encoder which wrote only images is remembered
//! the same way, and each of its copies waits only for the earlier work it
//! conflicts with.
//!
//! A pass's attachments are known when it is made, but what it samples is
//! only known as its textures are bound, so its Vulkan render pass begins
//! at its first draw. A texture an earlier pass wrote which is bound after
//! drawing began ends the Vulkan pass, waits, and continues it.
//!
//! Only images are tracked. A pass or blit which writes anything else waits
//! for all the work before it and ends with a full barrier, and so does every
//! compute encoder, as do resources an argument buffer reaches
//! (`-useResource:`) and fence waits, which name no image.

use super::*;

/// The accesses a render pass writes its attachments through.
pub(super) const ATTACHMENT_WRITES: vk::AccessFlags = vk::AccessFlags::from_raw(
    vk::AccessFlags::COLOR_ATTACHMENT_WRITE.as_raw()
        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE.as_raw(),
);

/// An image written by work not yet ordered against what follows it.
#[derive(Clone, Copy)]
struct Written {
    image: vk::Image,
    aspect: vk::ImageAspectFlags,
    /// The accesses which wrote it.
    access: vk::AccessFlags,
}

/// Image writes and reads of render passes and blit encoders which ended
/// since the command buffer's last full barrier.
#[derive(Default)]
pub(super) struct Hazards {
    written: Vec<Written>,
    /// Images that work read, which later work may write.
    read: Vec<vk::Image>,
}

impl Hazards {
    fn is_empty(&self) -> bool {
        self.written.is_empty() && self.read.is_empty()
    }

    fn wrote(&self, image: vk::Image) -> bool {
        self.written.iter().any(|written| written.image == image)
    }

    /// Whether a copy wrote an image.
    fn copied(&self) -> bool {
        self.written
            .iter()
            .any(|written| written.access.contains(vk::AccessFlags::TRANSFER_WRITE))
    }

    pub(super) fn note_written(
        &mut self,
        image: vk::Image,
        aspect: vk::ImageAspectFlags,
        access: vk::AccessFlags,
    ) {
        match self
            .written
            .iter_mut()
            .find(|written| written.image == image)
        {
            Some(written) => {
                written.aspect |= aspect;
                written.access |= access;
            }
            None => self.written.push(Written {
                image,
                aspect,
                access,
            }),
        }
    }

    fn note_read(&mut self, image: vk::Image) {
        if !self.read.contains(&image) {
            self.read.push(image);
        }
    }

    /// Add what `other` touched.
    fn merge(&mut self, other: &Hazards) {
        for written in &other.written {
            self.note_written(written.image, written.aspect, written.access);
        }
        for &image in &other.read {
            self.note_read(image);
        }
    }

    pub(super) fn clear(&mut self) {
        self.written.clear();
        self.read.clear();
    }
}

/// What a blit reads or writes.
#[derive(Clone, Copy)]
pub(super) enum Operand {
    Buffer,
    /// A texture, through the aspects the copy names.
    Texture(Id, vk::ImageAspectFlags),
}

impl Operand {
    fn tracked(self) -> Option<(vk::Image, vk::ImageAspectFlags)> {
        match self {
            Operand::Buffer => None,
            Operand::Texture(texture, aspect) => {
                tracked_image(texture).map(|image| (image, aspect))
            }
        }
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
        let body = unsafe { object::body_ref::<Texture>(texture) };
        if body.parent.is_null() {
            let image = &body.image;
            return (image.image != vk::Image::null() && image.placement.is_none())
                .then_some(image.image);
        }
        texture = body.parent;
    }
    None
}

/// Image barriers making the writes to `images` visible to `access`.
fn image_barriers(
    images: &[Written],
    access: vk::AccessFlags,
) -> Vec<vk::ImageMemoryBarrier<'static>> {
    images
        .iter()
        .map(|written| {
            vk::ImageMemoryBarrier::default()
                .src_access_mask(written.access)
                .dst_access_mask(access)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(written.image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: written.aspect,
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
    /// Wait for all the work which ended since the last full barrier, and
    /// make the images it wrote visible to everything after.
    pub(super) fn flush_hazards(&mut self) {
        if self.hazards.is_empty() {
            return;
        }
        let barriers = image_barriers(
            &self.hazards.written,
            vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
        );
        // SAFETY: recording into this command buffer, outside a render pass;
        // every image lives in GENERAL.
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
    fn order_attachment_writes(&mut self, images: &[Written]) {
        let barriers = image_barriers(
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
        let lazy = self.lazy.as_mut().expect("a lazy pass");
        lazy.read_hazard |= wrote;
        // Every image can be a blit's destination.
        if !lazy.reads.contains(&image) {
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
                command
                    .hazards
                    .note_written(image, aspect, ATTACHMENT_WRITES);
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
        // Copies usually fill textures passes sample, and what this pass
        // samples is only known as it binds them: waiting for copies now is
        // cheaper than splitting the pass at a later binding.
        if must_flush || rereads || command.hazards.copied() {
            command.flush_hazards();
        } else {
            let rewritten: Vec<_> = self
                .attachments
                .iter()
                .filter(|(image, _)| command.hazards.wrote(*image))
                .map(|&(image, aspect)| Written {
                    image,
                    aspect,
                    access: ATTACHMENT_WRITES,
                })
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
            hazards.note_written(image, aspect, ATTACHMENT_WRITES);
        }
        for image in lazy.reads {
            hazards.note_read(image);
        }
    }

    /// Order a blit's copy from `source` to `destination` after the earlier
    /// work it conflicts with, and remember what it touches for the work
    /// after the encoder.
    ///
    /// Copies within one encoder are not ordered against each other, as they
    /// were not before blits were tracked.
    pub(super) fn track_copy(&mut self, source: Operand, destination: Operand) {
        let hazards = &self.command().hazards;
        let source = source.tracked();
        let destination = destination.tracked();
        // Earlier work writes only tracked images, which no buffer or placed
        // texture shares memory with, so only a tracked source can conflict.
        let conflicts = source.is_some_and(|(image, _)| hazards.wrote(image))
            || match destination {
                Some((image, _)) => hazards.wrote(image) || hazards.read.contains(&image),
                // Earlier work may have read memory no tracked image names.
                None => true,
            };
        if conflicts {
            self.command().flush_hazards();
        }
        match destination {
            Some((image, aspect)) => {
                self.copies
                    .note_written(image, aspect, vk::AccessFlags::TRANSFER_WRITE)
            }
            None => self.writes_elsewhere = true,
        }
        if let Some((image, _)) = source {
            self.copies.note_read(image);
        }
    }

    /// Leave what a blit encoder which wrote only tracked images touched to
    /// be ordered against later work by a later conflict.
    pub(super) fn leave_copies_pending(&mut self) {
        let copies = std::mem::take(&mut self.copies);
        self.command().hazards.merge(&copies);
    }
}
