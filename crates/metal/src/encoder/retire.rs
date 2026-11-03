use super::*;

/// One owned Objective-C reference represented by its transferable address.
/// The wrapper releases exactly once unless ownership is explicitly handed to
/// another executor with [`Self::into_raw`].
pub(super) struct OwnedObjcRef(usize);

// SAFETY: Objective-C reference counts are thread-safe, and uses of the
// pointee still obey each framework object's own synchronization contract.
unsafe impl Send for OwnedObjcRef {}

impl OwnedObjcRef {
    pub(super) fn retain(object: Id) -> Self {
        Self(objc::arc::retain(object) as usize)
    }

    pub(super) fn as_id(&self) -> Id {
        self.0 as Id
    }

    pub(super) fn into_raw(self) -> Id {
        let object = self.as_id();
        std::mem::forget(self);
        object
    }
}

impl Drop for OwnedObjcRef {
    fn drop(&mut self) {
        objc::arc::release(self.as_id());
    }
}

/// One owned Objective-C block reference.
pub(super) struct OwnedBlock(usize);

// SAFETY: copied Objective-C blocks may be transferred to the handler worker.
unsafe impl Send for OwnedBlock {}

impl OwnedBlock {
    pub(super) fn from_retained(block: Id) -> Self {
        Self(block as usize)
    }

    pub(super) fn into_raw(self) -> Id {
        let block = self.0 as Id;
        std::mem::forget(self);
        block
    }
}

impl Drop for OwnedBlock {
    fn drop(&mut self) {
        objc::block::release(self.0 as Id);
    }
}

pub(super) struct CommandCompletion {
    state: Mutex<CommandState>,
    changed: Condvar,
}

impl CommandCompletion {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(CommandState::default()),
            changed: Condvar::new(),
        }
    }

    pub(super) fn get(&self) -> u64 {
        self.state.lock().expect("Metal command state").status
    }

    pub(super) fn set(&self, status: u64) {
        self.state.lock().expect("Metal command state").status = status;
        self.changed.notify_all();
    }

    pub(super) fn finish(&self, status: u64, gpu_times: Option<(f64, f64)>) {
        let mut state = self.state.lock().expect("Metal command state");
        if let Some((start, end)) = gpu_times {
            state.gpu_start_time = start;
            state.gpu_end_time = end;
        }
        state.status = status;
        self.changed.notify_all();
    }

    pub(super) fn gpu_time(&self, end: bool) -> f64 {
        let state = self.state.lock().expect("Metal command state");
        if end {
            state.gpu_end_time
        } else {
            state.gpu_start_time
        }
    }

    pub(super) fn wait_for(&self, predicate: impl Fn(u64) -> bool) {
        let mut state = self.state.lock().expect("Metal command state");
        while !predicate(state.status) {
            state = self.changed.wait(state).expect("Metal command state");
        }
    }
}

#[derive(Default)]
pub(super) struct CommandState {
    status: u64,
    gpu_start_time: f64,
    gpu_end_time: f64,
}

/// Everything whose lifetime is tied to one Vulkan submission.
pub(crate) struct Submission {
    pub(super) gpu: &'static Gpu,
    pub(super) timestamps: vk::QueryPool,
    pub(super) occlusion_pools: Vec<vk::QueryPool>,
    pub(super) pool: vk::CommandPool,
    /// The context's command buffer, and those an encoded event split off.
    pub(super) command_buffer: vk::CommandBuffer,
    pub(super) split_buffers: Vec<vk::CommandBuffer>,
    pub(crate) fence: vk::Fence,
    pub(super) descriptor_pools: Vec<vk::DescriptorPool>,
    pub(super) retained: Vec<OwnedObjcRef>,
    pub(super) uploads: Vec<GpuBuffer>,
    pub(super) framebuffers: Vec<vk::Framebuffer>,
    /// The parallel render sub-encoders' command buffers this one executed.
    pub(super) secondaries: Vec<gpu::SecondaryContext>,
    pub(super) captures: Vec<crate::capture::Capture>,
    pub(super) completed_handlers: Vec<OwnedBlock>,
    pub(super) presents: Vec<OwnedObjcRef>,
    pub(super) object: OwnedObjcRef,
    pub(crate) serial: u64,
    pub(super) completion: Arc<CommandCompletion>,
    /// Host time when Vulkan accepted this submission, used if this queue has
    /// no device timestamps or reading them fails.
    pub(super) scheduled_time: f64,
    pub(super) profile: Option<Arc<crate::profile::Profile>>,
}

impl Submission {
    pub(crate) fn transient_bytes(&self) -> u64 {
        self.uploads
            .iter()
            .map(|buffer| buffer.length)
            .chain(self.captures.iter().map(|capture| capture.buffer.length))
            .sum()
    }

    /// Hand the drawables this command buffer presents to the device's
    /// presentation thread, in submission order. Called once the guest work
    /// is on the queue, so the presentation copies land behind it.
    pub(super) fn schedule_presents(&mut self) {
        for drawable in self.presents.drain(..) {
            crate::present::enqueue(drawable.into_raw());
        }
    }

    pub(crate) fn wait_and_finish(self) {
        let result = self.gpu.wait(self.fence);
        stats::timed(Counter::FenceWakeToRetiredMicros, || self.finish(result));
    }

    pub(crate) fn finish(mut self, result: Result<(), String>) {
        if let Err(reason) = &result {
            gpu::trace(format!("command buffer completion failed: {reason}"));
        }
        let mut gpu_times = None;
        if result.is_ok() && self.timestamps != vk::QueryPool::null() {
            let mut ticks = [0u64; 2];
            // SAFETY: the submission completed, so both queries have results.
            let read = unsafe {
                self.gpu.device.get_query_pool_results(
                    self.timestamps,
                    0,
                    &mut ticks,
                    vk::QueryResultFlags::TYPE_64,
                )
            };
            if read.is_ok() {
                let elapsed = timestamp_delta(ticks[0], ticks[1], self.gpu.timestamp_valid_bits())
                    as f64
                    * self.gpu.timestamp_period()
                    / 1e9;
                let end = darwin::mach::media_time();
                gpu_times = Some((end - elapsed, end));
                if stats::enabled() {
                    stats::add(Counter::GpuMicros, (elapsed * 1e6) as u64);
                }
            }
        }
        if result.is_ok() && gpu_times.is_none() {
            let end = darwin::mach::media_time();
            gpu_times = Some((self.scheduled_time.min(end), end));
        }
        self.gpu.release_timestamp_pool(self.timestamps);
        for pool in self.occlusion_pools.drain(..) {
            self.gpu.release_occlusion_pool(pool);
        }
        if let Some(profile) = self.profile.take() {
            profile.finish(self.gpu, result.is_ok());
        }
        // Only a command buffer which never reached the queue still holds
        // drawables here; its frames are never shown.
        for drawable in self.presents.drain(..) {
            crate::present::complete(drawable.as_id());
        }
        // The work has completed, so everything the recording referred to and every
        // transient object it created can go.
        crate::capture::finish(self.gpu, std::mem::take(&mut self.captures));
        for framebuffer in self.framebuffers.drain(..) {
            // SAFETY: the submission finished before this point.
            unsafe { self.gpu.device.destroy_framebuffer(framebuffer, None) };
        }
        for pool in self.descriptor_pools.drain(..) {
            // The sets allocated from it are no longer in use.
            self.gpu.release_descriptor_pool(pool);
        }
        for chunk in self.uploads.drain(..) {
            self.gpu.release_upload_chunk(chunk);
        }
        for secondary in self.secondaries.drain(..) {
            self.gpu.release_secondary(secondary);
        }
        self.retained.clear();
        if !self.split_buffers.is_empty() {
            // SAFETY: the submission finished, or never reached the queue,
            // and these were allocated from this pool.
            unsafe {
                self.gpu
                    .device
                    .free_command_buffers(self.pool, &self.split_buffers)
            };
        }
        // The submission finished, or never reached the queue.
        self.gpu.release_context(CommandContext {
            pool: self.pool,
            command_buffer: self.command_buffer,
            fence: self.fence,
        });
        self.completion
            .finish(if result.is_ok() { 4 } else { 5 }, gpu_times);
        let object = self.object.as_id();
        for block in self.completed_handlers.drain(..) {
            // SAFETY: `objc_retainBlock` returned a live block, whose reference this
            // gives away, and `object` is retained by this submission.
            unsafe { crate::completion::deliver(block.into_raw(), object) };
        }
    }
}

pub(super) fn timestamp_delta(start: u64, end: u64, valid_bits: u32) -> u64 {
    let mask = match valid_bits {
        64.. => u64::MAX,
        bits => (1u64 << bits) - 1,
    };
    end.wrapping_sub(start) & mask
}
