//! Work Metal performs outside any command buffer - putting a fresh texture
//! into its layout and `-replaceRegion:` copies - recorded into a batch the
//! next queue submission runs first, rather than run and waited for on the
//! spot.
//!
//! Waiting on the spot waited for far more than the copy: the queue runs in
//! order, so the fence of a one-off submission signals only once every frame
//! already queued ahead of it has finished. While a game streamed its world
//! in, that was a frame's worth of device time per texture, on the thread
//! which was busy loading.
//!
//! Every submission to the queue goes through [`Gpu::submit`] or
//! [`Gpu::submit_synchronized`], which flush the batch first, so nothing can
//! reach the device before the work Metal says already happened.
//!
//! The transfers in a batch overlap on the device unless one reaches what an
//! earlier one wrote: a game streaming its world in fills hundreds of mip
//! levels a frame, and a barrier after each drained the device as many
//! times ahead of the frame. One barrier closes the batch.

use std::collections::{HashMap, HashSet, VecDeque};

use super::*;

/// One command buffer of deferred transfers and what it holds until the
/// device has run it.
pub struct Batch {
    context: CommandContext,
    /// Chunks the staged bytes were cut from, and how far into the last one
    /// they reach.
    chunks: Vec<GpuBuffer>,
    chunk_used: u64,
    /// Staging too large for a chunk.
    buffers: Vec<GpuBuffer>,
    /// The images the recording names. One destroyed before the batch has
    /// run is kept until it has.
    images: HashSet<vk::Image>,
    doomed: Vec<GpuImage>,
    /// Likewise the private buffers it names, and those destroyed since.
    buffers_named: HashSet<vk::Buffer>,
    doomed_buffers: Vec<GpuBuffer>,
    /// What the recording wrote since its last barrier, by image or buffer
    /// handle: the mip level and array slice of each write, or `None` for a
    /// write to all of it.
    written: HashMap<u64, Option<Vec<(u32, u32)>>>,
    /// Whether anything was recorded since the last barrier.
    unordered: bool,
}

#[derive(Default)]
pub(crate) struct Deferred {
    /// Lock order: the Vulkan queue, then this, then `in_flight`.
    pending: Mutex<Option<Batch>>,
    in_flight: Mutex<VecDeque<Batch>>,
}

impl Batch {
    pub fn command_buffer(&self) -> vk::CommandBuffer {
        self.context.command_buffer
    }

    /// Note that the recording names `image` only to lay it out, behind a
    /// barrier of its own which orders whatever follows. Every other command
    /// on an image is noted with [`Self::touch_subresource`].
    ///
    /// The layout barrier waits for nothing before it, so an image placed in
    /// a heap is first ordered after earlier writes to that heap's memory.
    pub fn name(&mut self, gpu: &Gpu, image: vk::Image, heap: Option<&Placement>) {
        self.images.insert(image);
        self.touch_heap(gpu, heap);
    }

    /// Note that the next command recorded writes one mip level and array
    /// slice of `image`, bound into `heap` if it is placed in one.
    pub fn touch_subresource(
        &mut self,
        gpu: &Gpu,
        image: vk::Image,
        heap: Option<&Placement>,
        level: u32,
        slice: u32,
    ) {
        self.images.insert(image);
        self.touch_heap(gpu, heap);
        self.order(gpu, vk::Handle::as_raw(image), Some((level, slice)));
    }

    /// Note that the next command recorded writes all of `image`, which is
    /// in memory of its own or a range of a private-image heap.
    pub fn touch_image(&mut self, gpu: &Gpu, image: vk::Image) {
        self.images.insert(image);
        self.order(gpu, vk::Handle::as_raw(image), None);
    }

    /// Note that the next command recorded writes `buffer`, bound into
    /// `heap` if it is placed in one.
    pub fn touch_buffer(&mut self, gpu: &Gpu, buffer: vk::Buffer, heap: Option<&Placement>) {
        self.buffers_named.insert(buffer);
        self.touch_heap(gpu, heap);
        self.order(gpu, vk::Handle::as_raw(buffer), None);
    }

    /// Resources placed in one heap may alias, and their handles differ, so a
    /// write into a heap is ordered after every earlier one into it.
    fn touch_heap(&mut self, gpu: &Gpu, heap: Option<&Placement>) {
        if let Some(heap) = heap {
            self.order(gpu, vk::Handle::as_raw(heap.memory), None);
        }
    }

    /// Order a write to `part` of `handle` after an earlier write in this
    /// batch to the same memory, which Metal performed first.
    fn order(&mut self, gpu: &Gpu, handle: u64, part: Option<(u32, u32)>) {
        let overlaps = match (self.written.get(&handle), part) {
            (None, _) => false,
            (Some(None), _) | (Some(Some(_)), None) => true,
            (Some(Some(parts)), Some(part)) => parts.contains(&part),
        };
        if overlaps {
            self.barrier(gpu);
        }
        self.unordered = true;
        match (
            self.written
                .entry(handle)
                .or_insert_with(|| Some(Vec::new())),
            part,
        ) {
            (Some(parts), Some(part)) => parts.push(part),
            (entry, _) => *entry = None,
        }
    }

    /// Everything recorded so far completes, with its writes visible, before
    /// anything recorded after.
    fn barrier(&mut self, gpu: &Gpu) {
        if !self.unordered {
            return;
        }
        let memory = vk::MemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
            .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE);
        // SAFETY: the batch's command buffer is recording, and the batch lock
        // makes this the only thread recording into it.
        unsafe {
            gpu.device.cmd_pipeline_barrier(
                self.command_buffer(),
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[memory],
                &[],
                &[],
            )
        };
        self.written.clear();
        self.unordered = false;
    }

    /// Copy `bytes` somewhere the device can read them until the batch has
    /// run, answering the buffer and offset they landed at.
    pub fn stage(
        &mut self,
        gpu: &Gpu,
        bytes: &[u8],
        alignment: u64,
    ) -> Result<(vk::Buffer, u64), String> {
        let length = bytes.len() as u64;
        let (contents, buffer, offset) = if length <= UPLOAD_CHUNK {
            let offset = self.chunk_used.next_multiple_of(alignment.max(1));
            let offset = match self.chunks.last() {
                Some(_) if offset + length <= UPLOAD_CHUNK => offset,
                _ => {
                    self.chunks.push(gpu.acquire_upload_chunk()?);
                    0
                }
            };
            self.chunk_used = offset + length;
            let chunk = self.chunks.last().expect("a chunk was just ensured");
            (chunk.contents, chunk.buffer, offset)
        } else {
            let buffer = gpu.acquire_staging_buffer(length)?;
            let found = (buffer.contents, buffer.buffer, 0);
            self.buffers.push(buffer);
            found
        };
        // SAFETY: the mapping covers `offset + length` bytes, handed out once.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                contents.add(offset as usize),
                bytes.len(),
            )
        };
        Ok((buffer, offset))
    }
}

impl Gpu {
    /// Record `body` into the batch the next submission runs ahead of its own
    /// work.
    ///
    /// The caller must [`Batch::touch`] every image it records a command on.
    pub fn defer<R>(
        &self,
        body: impl FnOnce(&Gpu, &mut Batch) -> Result<R, String>,
    ) -> Result<R, String> {
        self.retire_deferred();
        let mut pending = self.deferred.pending.lock().expect("deferred transfers");
        if pending.is_none() {
            *pending = Some(self.open_batch()?);
        }
        let batch = pending.as_mut().expect("just opened");
        let result = body(self, batch);
        crate::stats::count(crate::stats::Counter::DeferredTransfers);
        result
    }

    fn open_batch(&self) -> Result<Batch, String> {
        // Not from the command-buffer budget: a guest blocked on that budget
        // waits for retirement, which may be waiting for this batch.
        let context = self.acquire_context_unbounded()?;
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the context's command buffer is fresh or was reset with its
        // pool.
        if let Err(error) = unsafe {
            self.device
                .begin_command_buffer(context.command_buffer, &begin)
        } {
            self.recycle_context(context);
            return Err(format!("vkBeginCommandBuffer failed with {error:?}"));
        }
        Ok(Batch {
            context,
            chunks: Vec::new(),
            chunk_used: 0,
            buffers: Vec::new(),
            images: HashSet::new(),
            doomed: Vec::new(),
            buffers_named: HashSet::new(),
            doomed_buffers: Vec::new(),
            written: HashMap::new(),
            unordered: false,
        })
    }

    /// Submit the pending batch, if any, ahead of whatever the caller submits
    /// next. `queue` is the Vulkan queue, which the caller holds locked.
    pub(super) fn flush_deferred(&self, queue: vk::Queue) {
        let Some(mut batch) = self
            .deferred
            .pending
            .lock()
            .expect("deferred transfers")
            .take()
        else {
            return;
        };
        // The work submitted after the batch sees everything it wrote.
        batch.barrier(self);
        let command_buffer = batch.command_buffer();
        let buffers = [command_buffer];
        let submit = vk::SubmitInfo::default().command_buffers(&buffers);
        // SAFETY: recording began in `open_batch`, the queue is held
        // exclusively by the caller, and the fence is not in use.
        let submitted = unsafe {
            self.device
                .end_command_buffer(command_buffer)
                .and_then(|()| self.device.reset_fences(&[batch.context.fence]))
                .and_then(|()| {
                    self.device
                        .queue_submit(queue, &[submit], batch.context.fence)
                })
        };
        match submitted {
            Ok(()) => self
                .deferred
                .in_flight
                .lock()
                .expect("deferred transfers")
                .push_back(batch),
            Err(error) => {
                trace(format!(
                    "deferred texture transfers failed to submit with {error:?}: the textures \
                     they fill keep whatever they held"
                ));
                // Nothing of it reached the queue.
                self.retire_batch(batch);
            }
        }
    }

    /// Submit the pending batch now, taking the queue for it.
    fn flush_deferred_now(&self) {
        let queue = self.lock_queue();
        self.flush_deferred(*queue);
    }

    /// Give back every batch the device has finished, oldest first.
    pub(super) fn retire_deferred(&self) {
        self.trim_staging_buffers();
        loop {
            let batch = {
                let mut in_flight = self.deferred.in_flight.lock().expect("deferred transfers");
                let done = in_flight.front().is_some_and(|batch| {
                    // SAFETY: a fence of this device's, attached to a
                    // submission which reached the queue.
                    unsafe { self.device.get_fence_status(batch.context.fence) }.unwrap_or(false)
                });
                match done {
                    true => in_flight.pop_front(),
                    false => None,
                }
            };
            let Some(batch) = batch else {
                return;
            };
            self.retire_batch(batch);
        }
    }

    /// Release what one batch held. The device has run it, or never will.
    fn retire_batch(&self, batch: Batch) {
        let Batch {
            context,
            chunks,
            buffers,
            doomed,
            doomed_buffers,
            ..
        } = batch;
        self.recycle_context(context);
        for chunk in chunks {
            self.release_upload_chunk(chunk);
        }
        for buffer in buffers {
            self.release_staging_buffer(buffer);
        }
        for image in doomed {
            self.destroy_image_now(image);
        }
        for buffer in doomed_buffers {
            self.destroy_buffer_now(buffer);
        }
    }

    /// Hand `image` to the batch which still names it, answering it back
    /// where no batch does and it can be destroyed at once.
    pub(super) fn defer_destruction(&self, image: GpuImage) -> Option<GpuImage> {
        self.retire_deferred();
        {
            let mut pending = self.deferred.pending.lock().expect("deferred transfers");
            if let Some(batch) = pending
                .as_mut()
                .filter(|batch| batch.images.contains(&image.image))
            {
                batch.doomed.push(image);
                drop(pending);
                // Otherwise it waits for whatever the guest submits next,
                // which a guest tearing down may never do.
                self.flush_deferred_now();
                return None;
            }
        }
        // A batch leaves `pending` for `in_flight` and never the other way,
        // so one checked in this order cannot be missed in between.
        let mut in_flight = self.deferred.in_flight.lock().expect("deferred transfers");
        match in_flight
            .iter_mut()
            .find(|batch| batch.images.contains(&image.image))
        {
            Some(batch) => {
                batch.doomed.push(image);
                None
            }
            None => Some(image),
        }
    }

    /// As [`Gpu::defer_destruction`], for a private buffer.
    pub(super) fn defer_buffer_destruction(&self, buffer: GpuBuffer) -> Option<GpuBuffer> {
        self.retire_deferred();
        {
            let mut pending = self.deferred.pending.lock().expect("deferred transfers");
            if let Some(batch) = pending
                .as_mut()
                .filter(|batch| batch.buffers_named.contains(&buffer.buffer))
            {
                batch.doomed_buffers.push(buffer);
                drop(pending);
                self.flush_deferred_now();
                return None;
            }
        }
        let mut in_flight = self.deferred.in_flight.lock().expect("deferred transfers");
        match in_flight
            .iter_mut()
            .find(|batch| batch.buffers_named.contains(&buffer.buffer))
        {
            Some(batch) => {
                batch.doomed_buffers.push(buffer);
                None
            }
            None => Some(buffer),
        }
    }
}
