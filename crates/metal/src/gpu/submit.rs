use super::*;
use std::sync::atomic::Ordering;

/// One `VkSubmitInfo`: a command buffer, the timeline values it waits for
/// before it starts, and those it signals once it is done. A Metal command
/// buffer with events encoded inside it is several of these.
pub struct Batch {
    pub waits: Vec<(vk::Semaphore, u64)>,
    pub command_buffer: vk::CommandBuffer,
    pub signals: Vec<(vk::Semaphore, u64)>,
}

impl Batch {
    pub fn plain(command_buffer: vk::CommandBuffer) -> Self {
        Self {
            waits: Vec::new(),
            command_buffer,
            signals: Vec::new(),
        }
    }
}

/// A committed command buffer on its way to the queue.
pub struct Commit {
    pub batches: Vec<Batch>,
    pub fence: vk::Fence,
    /// Called with the commit's place in queue order once the batches reach
    /// the queue, or with why they did not, and the fence which signals once
    /// they are done: `fence`, or a later commit's submitted with it. Called
    /// with the queue held, so in that order, and so it must not take the
    /// queue itself.
    pub submitted: Box<dyn FnOnce(u64, Result<(), String>, vk::Fence) + Send>,
}

/// The most commits submitted together, so the first of a long run is not
/// held back until the last is recorded into the call.
const MERGED_COMMITS: usize = 64;

/// Commits waiting for the submission thread, oldest first.
pub(crate) struct Commits {
    pending: Mutex<VecDeque<Commit>>,
    ready: Condvar,
    worker: Once,
}

impl Default for Commits {
    fn default() -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
            worker: Once::new(),
        }
    }
}

impl Gpu {
    /// Hand a committed command buffer to the submission thread.
    ///
    /// Metal's `-commit` returns before the work reaches the device, and
    /// `vkQueueSubmit` costs tens of microseconds of driver and kernel time,
    /// so that time is spent off the thread which commits.
    pub(crate) fn commit(&'static self, commit: Commit) {
        self.commits.worker.call_once(|| {
            std::thread::Builder::new()
                .name("metal:submit".to_string())
                .spawn(move || {
                    crate::enter_darwin_thread();
                    self.submit_worker()
                })
                .expect("a thread for Metal submissions");
        });
        let mut pending = self.commits.pending.lock().expect("Metal commits");
        pending.push_back(commit);
        self.commits.ready.notify_one();
    }

    fn submit_worker(&self) {
        loop {
            {
                let mut pending = self.commits.pending.lock().expect("Metal commits");
                while pending.is_empty() {
                    pending = self.commits.ready.wait(pending).expect("Metal commits");
                }
            }
            drop(self.lock_queue());
            // Outside the queue lock: this destroys buffers and images.
            self.retire_deferred();
        }
    }

    /// The queue, once every command buffer committed before this call is on
    /// it, so whatever is submitted next lands behind them as it would have
    /// had they been submitted when committed.
    pub(super) fn lock_queue(&self) -> MutexGuard<'_, vk::Queue> {
        let queue = self.queue.lock().expect("Vulkan queue");
        loop {
            let commits: Vec<Commit> = {
                let mut pending = self.commits.pending.lock().expect("Metal commits");
                let count = pending.len().min(MERGED_COMMITS);
                pending.drain(..count).collect()
            };
            let Some(last) = commits.last() else {
                break;
            };
            // One fence for all of them: it signals once the last is done,
            // and they retire in order, so it outlives every wait on it.
            let fence = last.fence;
            let batches: Vec<Batch> = commits
                .iter()
                .flat_map(|commit| commit.batches.iter())
                .map(|batch| Batch {
                    waits: batch.waits.clone(),
                    command_buffer: batch.command_buffer,
                    signals: batch.signals.clone(),
                })
                .collect();
            let result = crate::stats::timed(crate::stats::Counter::SubmitMicros, || {
                self.submit_locked(*queue, &batches, fence)
            });
            for commit in commits {
                (commit.submitted)(self.submissions.next_serial(), result.clone(), fence);
            }
        }
        queue
    }

    /// Test controlled device-wide queue contention while AppKit presents
    /// another window. No guest or production path calls this.
    #[cfg(test)]
    pub(crate) fn with_queue_locked_for_test(&self, body: impl FnOnce()) {
        let _queue = self.queue.lock().expect("Vulkan queue");
        body();
    }

    pub fn push_descriptor_api(&self) -> Option<&ash::khr::push_descriptor::Device> {
        self.push_descriptor_api.as_ref()
    }

    /// Submit one recorded command buffer.
    pub fn submit(
        &self,
        command_buffer: vk::CommandBuffer,
        fence: vk::Fence,
    ) -> Result<(), String> {
        self.submit_batches(&[Batch::plain(command_buffer)], fence)
    }

    /// Submit batches in order, `fence` signalling once all are done.
    pub fn submit_batches(&self, batches: &[Batch], fence: vk::Fence) -> Result<(), String> {
        let submitted = {
            let waiting = crate::stats::timer();
            let queue = self.lock_queue();
            crate::stats::finish_timer(crate::stats::Counter::SubmitLockMicros, waiting);
            self.submit_locked(*queue, batches, fence)
        };
        // Outside the queue lock: this destroys buffers and images.
        self.retire_deferred();
        submitted
    }

    /// [`Self::submit_batches`], with the queue already held.
    fn submit_locked(
        &self,
        queue: vk::Queue,
        batches: &[Batch],
        fence: vk::Fence,
    ) -> Result<(), String> {
        if self.lost.load(Ordering::Acquire) {
            return Err("Vulkan logical device is lost".into());
        }
        // Batches nothing separates - no event signalled after the first or
        // waited for before the second - share one `VkSubmitInfo`, which the
        // driver hands the kernel as one submission. Each submission costs
        // the kernel a validation of every host import, every time.
        let mut groups: Vec<(Vec<vk::CommandBuffer>, &Batch, &Batch)> = Vec::new();
        for batch in batches {
            match groups.last_mut() {
                Some((buffers, _, last)) if last.signals.is_empty() && batch.waits.is_empty() => {
                    buffers.push(batch.command_buffer);
                    *last = batch;
                }
                _ => groups.push((vec![batch.command_buffer], batch, batch)),
            }
        }
        let split = |pairs: &[(vk::Semaphore, u64)]| -> (Vec<vk::Semaphore>, Vec<u64>) {
            pairs.iter().copied().unzip()
        };
        let waits: Vec<_> = groups
            .iter()
            .map(|(_, first, _)| split(&first.waits))
            .collect();
        let signals: Vec<_> = groups
            .iter()
            .map(|(_, _, last)| split(&last.signals))
            .collect();
        let stages: Vec<_> = groups
            .iter()
            .map(|(_, first, _)| vec![vk::PipelineStageFlags::ALL_COMMANDS; first.waits.len()])
            .collect();
        let mut timelines: Vec<_> = waits
            .iter()
            .zip(&signals)
            .map(|((_, wait_values), (_, signal_values))| {
                vk::TimelineSemaphoreSubmitInfo::default()
                    .wait_semaphore_values(wait_values)
                    .signal_semaphore_values(signal_values)
            })
            .collect();
        let submits: Vec<_> = timelines
            .iter_mut()
            .zip(&groups)
            .enumerate()
            .map(|(index, (timeline, (buffers, first, last)))| {
                let submit = vk::SubmitInfo::default()
                    .command_buffers(buffers)
                    .wait_semaphores(&waits[index].0)
                    .wait_dst_stage_mask(&stages[index])
                    .signal_semaphores(&signals[index].0);
                // Only an event's semaphores are timelines; a plain batch
                // must not name the structure on a device without them.
                match first.waits.is_empty() && last.signals.is_empty() {
                    true => submit,
                    false => submit.push_next(timeline),
                }
            })
            .collect();
        crate::stats::timed(crate::stats::Counter::SubmitFlushMicros, || {
            self.flush_deferred(queue)
        });
        let driver = crate::stats::timer();
        // SAFETY: the caller holds the queue exclusively for the submission.
        unsafe { self.device.reset_fences(&[fence]) }
            .map_err(|error| format!("vkResetFences failed with {error:?}"))?;
        // SAFETY: as above; the command buffers have finished recording and
        // every borrowed array lives across the call.
        let submitted = unsafe { self.device.queue_submit(queue, &submits, fence) };
        crate::stats::finish_timer(crate::stats::Counter::SubmitDriverMicros, driver);
        if let Err(error) = submitted {
            if error == vk::Result::ERROR_DEVICE_LOST {
                crate::swapchain::record_device_loss(self);
            }
            return Err(format!("vkQueueSubmit failed with {error:?}"));
        }
        Ok(())
    }

    /// `VK_KHR_swapchain`'s device functions, where this device has them.
    pub fn swapchain_api(&self) -> Option<&ash::khr::swapchain::Device> {
        self.swapchain_api.as_ref()
    }

    /// Whether `vkQueuePresentKHR` can signal a fence when its objects are no
    /// longer in use by the presentation engine.
    pub fn has_present_fences(&self) -> bool {
        self.swapchain_maintenance1
    }

    /// Whether this device's queue can present to `surface`.
    ///
    /// Metal has one queue, so there is no second family to fall back on: a
    /// surface the graphics queue cannot present to is one this crate cannot
    /// use a swapchain for.
    pub fn can_present(&self, surface: vk::SurfaceKHR) -> bool {
        let Some(api) = crate::vulkan::surface_api() else {
            return false;
        };
        // SAFETY: a physical device and a surface of this instance's.
        unsafe {
            api.get_physical_device_surface_support(self.physical, self.queue_family, surface)
        }
        .unwrap_or(false)
    }

    /// Submit one command buffer with the semaphores presentation needs.
    ///
    /// [`Self::submit`] is the path a guest's command buffer takes and has no
    /// semaphores at all; a swapchain image cannot be written before the
    /// presentation engine is done with it, and cannot be presented before
    /// the write has landed, so both ends need one.
    pub fn submit_synchronized(
        &self,
        command_buffer: vk::CommandBuffer,
        fence: vk::Fence,
        wait: &[(vk::Semaphore, vk::PipelineStageFlags)],
        signal: &[vk::Semaphore],
    ) -> Result<(), String> {
        if self.lost.load(Ordering::Acquire) {
            return Err("Vulkan logical device is lost".into());
        }
        let buffers = [command_buffer];
        let wait_semaphores = wait.iter().map(|(s, _)| *s).collect::<Vec<_>>();
        let wait_stages = wait.iter().map(|(_, stage)| *stage).collect::<Vec<_>>();
        let submit = vk::SubmitInfo::default()
            .command_buffers(&buffers)
            .wait_semaphores(&wait_semaphores)
            .wait_dst_stage_mask(&wait_stages)
            .signal_semaphores(signal);
        let queue = self.lock_queue();
        self.flush_deferred(*queue);
        // SAFETY: the queue is held exclusively; the fence and semaphores
        // belong to this device.
        unsafe { self.device.reset_fences(&[fence]) }
            .map_err(|error| format!("vkResetFences failed with {error:?}"))?;
        // SAFETY: as above; the command buffer has finished recording and
        // every borrowed slice lives across the call.
        let result = unsafe { self.device.queue_submit(*queue, &[submit], fence) };
        if matches!(result, Err(vk::Result::ERROR_DEVICE_LOST)) {
            crate::swapchain::record_device_loss(self);
        }
        result.map_err(|error| format!("vkQueueSubmit failed with {error:?}"))
    }

    /// Recover from a failed vkQueueSubmit after successful image acquisition.
    /// A failed submission does not consume the acquisition semaphore; a
    /// synchronization-only submission waits for its WSI signal without
    /// referencing an image or replaying the failed command buffer.
    /// Reuse is permitted only after the recovery fence has signaled.
    pub fn consume_acquired_semaphore(
        &self,
        acquired: vk::Semaphore,
        fence: vk::Fence,
    ) -> Result<(), String> {
        if self.lost.load(Ordering::Acquire) {
            return Err("Vulkan logical device is lost".into());
        }
        let semaphores = [acquired];
        let stages = [vk::PipelineStageFlags::TOP_OF_PIPE];
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(&semaphores)
            .wait_dst_stage_mask(&stages);
        {
            let queue = self.lock_queue();
            // The failed render submission did not associate the fence with
            // queued work. Reset here even if recording failed before the
            // original vkResetFences operation.
            self.fence_result(
                unsafe { self.device.reset_fences(&[fence]) },
                "vkResetFences",
            )?;
            if let Err(error) = unsafe { self.device.queue_submit(*queue, &[submit], fence) } {
                if error == vk::Result::ERROR_DEVICE_LOST {
                    crate::swapchain::record_device_loss(self);
                }
                return Err(format!("vkQueueSubmit acquire recovery failed: {error:?}"));
            }
        }
        self.wait(fence)
    }

    /// Queue one swapchain image for presentation.
    ///
    /// The raw `VkResult` rather than a message, because `OUT_OF_DATE_KHR` is
    /// not a failure: it is how the presentation engine says the window
    /// changed size, and the caller answers by rebuilding the swapchain.
    pub fn present(
        &self,
        swapchain: vk::SwapchainKHR,
        image: u32,
        wait: vk::Semaphore,
        fence: Option<vk::Fence>,
    ) -> Result<bool, vk::Result> {
        if self.lost.load(Ordering::Acquire) {
            return Err(vk::Result::ERROR_DEVICE_LOST);
        }
        let api = self
            .swapchain_api
            .as_ref()
            .ok_or(vk::Result::ERROR_UNKNOWN)?;
        let semaphores = [wait];
        let swapchains = [swapchain];
        let images = [image];
        let mut info = vk::PresentInfoKHR::default()
            .wait_semaphores(&semaphores)
            .swapchains(&swapchains)
            .image_indices(&images);
        let fences;
        let mut fence_info;
        if let Some(fence) = fence {
            fences = [fence];
            fence_info = vk::SwapchainPresentFenceInfoEXT::default().fences(&fences);
            info = info.push_next(&mut fence_info);
        }
        let lock_timer = crate::stats::timer();
        let _commits = sdl3::compositor::guard();
        let _wsi = self.wsi.lock().expect("Vulkan WSI");
        let queue = self.lock_queue();
        crate::stats::finish_timer(crate::stats::Counter::QueuePresentLockMicros, lock_timer);
        // SAFETY: the queue is held exclusively, every borrowed slice lives
        // across the call, and the image was acquired from this swapchain.
        let result =
            crate::stats::timed(crate::stats::Counter::QueuePresentDriverMicros, || unsafe {
                api.queue_present(*queue, &info)
            });
        if matches!(result, Err(vk::Result::ERROR_DEVICE_LOST)) {
            crate::swapchain::record_device_loss(self);
        }
        result
    }

    /// Wait until the queue has finished everything submitted to it,
    /// presentation included.
    ///
    /// A fence covers the submission that signals it and nothing else. A
    /// presentation waits on a semaphore *after* that fence has signalled, so
    /// a swapchain's semaphores are still in use by the queue when its
    /// fences say the blits are done; only the queue itself going idle says
    /// they are free. Presentation calls this when it rebuilds or gives up a
    /// swapchain, which is a resize or a teardown and never a frame.
    pub fn wait_queue_idle(&self) {
        let idle = {
            let queue = self.lock_queue();
            // SAFETY: the queue is held exclusively for the wait.
            unsafe { self.device.queue_wait_idle(*queue) }.is_ok()
        };
        if idle {
            // Submitted immediate commands are also complete now; release
            // their owned resources without making another window submit.
            self.retire_immediate_quarantine();
        }
    }

    /// Every fence query and wait must publish device loss just like queue
    /// submission. The same device can back more than one SDL window.
    pub fn fence_result<T>(
        &self,
        result: Result<T, vk::Result>,
        operation: &str,
    ) -> Result<T, String> {
        result.map_err(|error| {
            if error == vk::Result::ERROR_DEVICE_LOST {
                crate::swapchain::record_device_loss(self);
            }
            format!("{operation} failed with {error:?}")
        })
    }

    pub fn fence_signaled(&self, fence: vk::Fence) -> Result<bool, String> {
        // SAFETY: this GPU owns the live fence.
        self.fence_result(
            unsafe { self.device.get_fence_status(fence) },
            "vkGetFenceStatus",
        )
    }

    pub fn wait_fence_timeout(&self, fence: vk::Fence, timeout: u64) -> Result<bool, String> {
        // SAFETY: this GPU owns the live fence.
        match unsafe { self.device.wait_for_fences(&[fence], true, timeout) } {
            Ok(()) => Ok(true),
            Err(vk::Result::TIMEOUT) => Ok(false),
            Err(error) => self.fence_result(Err(error), "vkWaitForFences"),
        }
    }

    /// Wait for a submitted command buffer's fence.
    pub fn wait(&self, fence: vk::Fence) -> Result<(), String> {
        crate::stats::timed(crate::stats::Counter::FenceWaitMicros, || {
            self.wait_fence_timeout(fence, u64::MAX)
                .and_then(|complete| {
                    complete
                        .then_some(())
                        .ok_or_else(|| "Vulkan fence did not signal before wait timeout".into())
                })
        })
    }

    #[cfg(test)]
    pub(crate) fn inject_immediate_wait_failure_for_test(&self) {
        self.fail_immediate_wait_after_submit
            .store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn immediate_quarantine_count_for_test(&self) -> usize {
        self.immediate.lock().unwrap().quarantined.len()
    }

    #[cfg(test)]
    pub(crate) fn wait_first_quarantined_for_test(&self) -> Result<(), String> {
        let fence = self
            .immediate
            .lock()
            .unwrap()
            .quarantined
            .first()
            .map(|context| context.fence)
            .ok_or("no quarantined immediate submission")?;
        self.wait(fence)
    }

    // Move signaled quarantines back into circulation without reclaiming
    // their descriptors or upload buffers while a GPU read is outstanding.
    // Hold the count against the bound while cleanup runs outside the lock.
    fn retire_immediate_quarantine(&self) {
        let mut ready = Vec::new();
        {
            let mut registry = self.immediate.lock().expect("immediate command recycler");
            let mut i = 0;
            while i < registry.quarantined.len() {
                let fence = registry.quarantined[i].fence;
                // SAFETY: fence belongs to this live device and has never
                // been destroyed or reset while quarantined.
                match unsafe { self.device.get_fence_status(fence) } {
                    Ok(true) => {
                        ready.push(registry.quarantined.swap_remove(i));
                    }
                    Ok(false) | Err(_) => i += 1,
                }
            }
            registry.retiring += ready.len();
        }
        let count = ready.len();
        for context in &mut ready {
            // Dropping an owned submission may free Vulkan buffers, charges
            // or return overlay passes. Never do that under the pool lock.
            drop(context.retained.take());
        }
        if count != 0 {
            let mut registry = self.immediate.lock().expect("immediate command recycler");
            registry.retiring -= count;
            registry.available.extend(ready);
            self.immediate_ready.notify_all();
        }
    }

    /// Record and run one command list synchronously.
    ///
    /// Metal performs some work outside any command buffer the guest holds:
    /// putting a fresh texture into a usable layout, and copying bytes a guest
    /// handed to `-replaceRegion:`. Those go through here.
    #[cfg(test)]
    pub fn one_shot(&self, body: impl FnOnce(&Device, vk::CommandBuffer)) -> Result<(), String> {
        self.one_shot_owned((), |device, cb, _| body(device, cb))
    }

    pub(crate) fn one_shot_owned<R: Send + 'static>(
        &self,
        resources: R,
        body: impl FnOnce(&Device, vk::CommandBuffer, &R),
    ) -> Result<(), String> {
        if self.lost.load(Ordering::Acquire) {
            return Err("Vulkan logical device is lost".into());
        }
        crate::stats::count(crate::stats::Counter::ImmediateCalls);
        crate::stats::timed(crate::stats::Counter::ImmediateMicros, || {
            self.one_shot_inner(resources, body)
        })
    }

    fn one_shot_inner<R: Send + 'static>(
        &self,
        resources: R,
        body: impl FnOnce(&Device, vk::CommandBuffer, &R),
    ) -> Result<(), String> {
        const MAX_IMMEDIATE_CONTEXTS: usize = 8;
        self.retire_immediate_quarantine();
        // DXVK-style command-list checkout: distinct windows record on
        // independent pools and wait on their own fences. Only vkQueueSubmit
        // retains Vulkan's mandatory per-queue external synchronization.
        let immediate = {
            let mut registry = self.immediate.lock().expect("immediate command recycler");
            loop {
                if let Some(available) = registry.available.pop() {
                    registry.in_use += 1;
                    break available;
                }
                if registry.in_use + registry.retiring + registry.quarantined.len()
                    < MAX_IMMEDIATE_CONTEXTS
                {
                    registry.in_use += 1;
                    drop(registry);
                    let created = (|| -> Result<Immediate, String> {
                        let pool_info = vk::CommandPoolCreateInfo::default()
                            .queue_family_index(self.queue_family)
                            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
                        let pool = unsafe { self.device.create_command_pool(&pool_info, None) }
                            .map_err(|error| {
                                format!("vkCreateCommandPool failed with {error:?}")
                            })?;
                        let pool = crate::gpu::Guard::new(pool, |pool| {
                            unsafe { self.device.destroy_command_pool(pool, None) };
                        });
                        let allocate = vk::CommandBufferAllocateInfo::default()
                            .command_pool(*pool)
                            .level(vk::CommandBufferLevel::PRIMARY)
                            .command_buffer_count(1);
                        let command_buffer =
                            unsafe { self.device.allocate_command_buffers(&allocate) }.map_err(
                                |error| format!("vkAllocateCommandBuffers failed with {error:?}"),
                            )?[0];
                        let fence = unsafe {
                            self.device
                                .create_fence(&vk::FenceCreateInfo::default(), None)
                        }
                        .map_err(|error| format!("vkCreateFence failed with {error:?}"))?;
                        Ok(Immediate {
                            pool: pool.take(),
                            command_buffer,
                            fence,
                            retained: None,
                        })
                    })();
                    if created.is_err() {
                        let mut registry =
                            self.immediate.lock().expect("immediate command recycler");
                        registry.in_use -= 1;
                        self.immediate_ready.notify_one();
                    }
                    break created?;
                }
                if registry.in_use == 0 {
                    return Err(
                        "immediate command pool exhausted by incomplete GPU submissions".into(),
                    );
                }
                registry = self
                    .immediate_ready
                    .wait(registry)
                    .expect("immediate command recycler");
            }
        };
        let mut slot = ImmediateLease {
            gpu: self,
            context: Some(immediate),
            completed: true, // No GPU submission exists until submit succeeds.
            resources: Some(resources),
        };
        let immediate = slot.context.as_ref().expect("checked out GPU command list");
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the mutex makes this the only user of the pool.
        unsafe {
            self.device
                .reset_command_pool(immediate.pool, vk::CommandPoolResetFlags::empty())
                .map_err(|error| format!("vkResetCommandPool failed with {error:?}"))?;
            self.device
                .begin_command_buffer(immediate.command_buffer, &begin)
                .map_err(|error| format!("vkBeginCommandBuffer failed with {error:?}"))?;
        }
        body(
            &self.device,
            immediate.command_buffer,
            slot.resources.as_ref().unwrap(),
        );
        // SAFETY: recording began above and has not ended.
        unsafe { self.device.end_command_buffer(immediate.command_buffer) }
            .map_err(|error| format!("vkEndCommandBuffer failed with {error:?}"))?;
        // A queue submission is the first point at which resource retirement
        // becomes uncertain. Recording/reset failures leave the owner safe to
        // recycle; a panic during the submit call must conservatively retain it.
        slot.completed = false;
        if let Err(error) = self.submit(immediate.command_buffer, immediate.fence) {
            // vkQueueSubmit did not queue the command buffer.
            slot.completed = true;
            return Err(error);
        }
        // Once submitted, a failed wait may mean the GPU is still reading the
        // command buffer. Only a confirmed fence completion recycles its pool.
        #[cfg(test)]
        if self
            .fail_immediate_wait_after_submit
            .swap(false, Ordering::AcqRel)
        {
            return Err("injected immediate fence wait error after queue submission".into());
        }
        self.wait(immediate.fence)?;
        slot.completed = true;
        Ok(())
    }
}

/// Keeps one command pool and its fence exclusively owned until the GPU
/// finishes its submission. Even a recording panic or failed fence wait may
/// not return a possibly active command buffer to the free recycler.
struct ImmediateLease<'a, R: Send + 'static> {
    gpu: &'a Gpu,
    context: Option<Immediate>,
    completed: bool,
    resources: Option<R>,
}

impl<R: Send + 'static> Drop for ImmediateLease<'_, R> {
    fn drop(&mut self) {
        let mut registry = self
            .gpu
            .immediate
            .lock()
            .expect("immediate command recycler");
        registry.in_use -= 1;
        if let Some(mut context) = self.context.take() {
            if self.completed {
                registry.available.push(context);
            } else {
                context.retained = self
                    .resources
                    .take()
                    .map(|resource| Box::new(resource) as Box<dyn Send>);
                registry.quarantined.push(context);
            }
        }
        self.gpu.immediate_ready.notify_one();
    }
}
