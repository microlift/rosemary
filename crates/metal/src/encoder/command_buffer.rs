use super::*;

pub(super) unsafe extern "C" fn new_command_buffer(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a command queue of this crate's.
    let queue = unsafe { object::body::<CommandQueue>(receiver) };
    let gpu = queue.gpu;
    let context = match gpu.acquire_context() {
        Ok(context) => context,
        Err(reason) => {
            gpu::trace(reason);
            return std::ptr::null_mut();
        }
    };
    let CommandContext {
        pool,
        command_buffer,
        fence,
    } = context;
    let begin =
        vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
    // SAFETY: the command buffer is fresh or was reset with its pool.
    if let Err(error) = unsafe { gpu.device.begin_command_buffer(command_buffer, &begin) } {
        gpu::trace(format!("vkBeginCommandBuffer failed with {error:?}"));
        gpu.release_context(CommandContext {
            pool,
            command_buffer,
            fence,
        });
        return std::ptr::null_mut();
    }
    let timestamps = gpu.timestamp_pool();
    if timestamps != vk::QueryPool::null() {
        // SAFETY: recording into the command buffer opened above.
        unsafe {
            gpu.device
                .cmd_reset_query_pool(command_buffer, timestamps, 0, 2);
            gpu.device.cmd_write_timestamp(
                command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                timestamps,
                0,
            );
        }
    }
    let profile = crate::profile::Profile::begin(gpu, command_buffer);
    stats::count(Counter::CommandBuffers);
    // `-commandBuffer` owns nothing it hands back: Metal vends an autoreleased
    // command buffer, and a guest which keeps one retains it itself.
    objc::arc::autorelease(object::make(
        c"RosemaryMTLCommandBuffer",
        CommandBuffer {
            gpu,
            timestamps,
            queue: objc::arc::retain(receiver),
            pool,
            command_buffer,
            fence,
            batches: Vec::new(),
            waits: Vec::new(),
            descriptor_pools: Vec::new(),
            retained: Vec::new(),
            retained_set: AddressSet::default(),
            uploads: Vec::new(),
            upload_used: 0,
            framebuffers: Vec::new(),
            secondaries: Vec::new(),
            secondary: false,
            secondary_closed: false,
            captures: Vec::new(),
            scheduled_handlers: Vec::new(),
            completed_handlers: Vec::new(),
            presents: Vec::new(),
            completion: Arc::new(CommandCompletion::new()),
            recording: true,
            profile,
            occlusion: Occlusion::default(),
        },
    ))
}

/// `-[MTLCommandBuffer dealloc]`.
///
/// A committed command buffer gave everything it held to its [`Submission`],
/// which retires it; what is left here belongs to one which was never
/// committed, and to the queue every command buffer retains.
pub(super) unsafe extern "C" fn command_buffer_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a command buffer of this crate's.
    let command = unsafe { object::body::<CommandBuffer>(receiver) };
    let gpu = command.gpu;
    if command.recording {
        command.recording = false;
        for framebuffer in std::mem::take(&mut command.framebuffers) {
            // SAFETY: nothing was submitted, so nothing refers to it.
            unsafe { gpu.device.destroy_framebuffer(framebuffer, None) };
        }
        for capture in std::mem::take(&mut command.captures) {
            gpu.destroy_buffer(capture.buffer);
        }
        for pool in std::mem::take(&mut command.descriptor_pools) {
            gpu.release_descriptor_pool(pool);
        }
        for chunk in std::mem::take(&mut command.uploads) {
            gpu.release_upload_chunk(chunk);
        }
        for secondary in std::mem::take(&mut command.secondaries) {
            gpu.release_secondary(secondary);
        }
        // Never submitted, so nothing allocated from the pool is pending.
        if let Some(first) = command.batches.first().map(|batch| batch.command_buffer) {
            let mut split: Vec<_> = command
                .batches
                .drain(1..)
                .map(|batch| batch.command_buffer)
                .collect();
            split.push(std::mem::replace(&mut command.command_buffer, first));
            command.batches.clear();
            // SAFETY: as above; all were allocated from this pool.
            unsafe { gpu.device.free_command_buffers(command.pool, &split) };
        }
        if command.secondary {
            gpu.release_secondary(gpu::SecondaryContext {
                pool: command.pool,
                command_buffer: command.command_buffer,
            });
        } else {
            gpu.release_context(CommandContext {
                pool: command.pool,
                command_buffer: command.command_buffer,
                fence: command.fence,
            });
        }
        gpu.release_timestamp_pool(std::mem::replace(
            &mut command.timestamps,
            vk::QueryPool::null(),
        ));
        for pool in command.occlusion.take_pools() {
            gpu.release_occlusion_pool(pool);
        }
        command.retained.clear();
        for drawable in command.presents.drain(..) {
            crate::present::complete(drawable.as_id());
        }
        command.scheduled_handlers.clear();
        command.completed_handlers.clear();
    }
    objc::arc::release(std::mem::replace(&mut command.queue, std::ptr::null_mut()));
    // The `Arc` in the body is Rust's, and nothing else here owns memory.
    // SAFETY: the body was established by `object::make` and is not read again.
    unsafe { std::ptr::drop_in_place(command as *mut CommandBuffer) };
    objc::arc::finalize(receiver);
}

pub(super) unsafe extern "C" fn command_buffer_queue(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a command buffer of this crate's.
    unsafe { object::body::<CommandBuffer>(receiver) }.queue
}

pub(super) unsafe extern "C" fn command_buffer_status(receiver: Id, _sel: Sel) -> u64 {
    unsafe { object::body::<CommandBuffer>(receiver) }
        .completion
        .get()
}

pub(super) unsafe extern "C" fn command_buffer_gpu_start_time(receiver: Id, _sel: Sel) -> f64 {
    unsafe { object::body::<CommandBuffer>(receiver) }
        .completion
        .gpu_time(false)
}

pub(super) unsafe extern "C" fn command_buffer_gpu_end_time(receiver: Id, _sel: Sel) -> f64 {
    unsafe { object::body::<CommandBuffer>(receiver) }
        .completion
        .gpu_time(true)
}

pub(super) unsafe extern "C" fn add_handler(receiver: Id, sel: Sel, block: Id) {
    let command = unsafe { object::body::<CommandBuffer>(receiver) };
    // SAFETY: the guest passed a block.
    let retained = unsafe { objc::shorthand::objc_retainBlock(block) };
    if retained.is_null() {
        return;
    }
    // SAFETY: a selector the runtime owns.
    let scheduled = unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() == b"addScheduledHandler:";
    match scheduled {
        true => command
            .scheduled_handlers
            .push(OwnedBlock::from_retained(retained)),
        false => command
            .completed_handlers
            .push(OwnedBlock::from_retained(retained)),
    }
}

/// `-[MTLCommandBuffer commit]`: end recording and submit asynchronously.
pub(super) unsafe extern "C" fn commit(receiver: Id, _sel: Sel) {
    // SAFETY: a command buffer of this crate's.
    let command = unsafe { object::body::<CommandBuffer>(receiver) };
    if !command.recording {
        return;
    }
    command.recording = false;
    let gpu = command.gpu;
    if command.timestamps != vk::QueryPool::null() {
        // SAFETY: recording into this command buffer, which has not ended.
        unsafe {
            gpu.device.cmd_write_timestamp(
                command.command_buffer,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                command.timestamps,
                1,
            )
        };
    }
    // Everything the command buffer holds moves into the submission before
    // recording is ended, because a `Submission` is the only thing which knows
    // how to give it all back: `dealloc` cleans up only a command buffer which
    // is still recording, so a failure between the two would otherwise strand
    // the pool, the fence, the descriptor pools, the retained objects and
    // every handler for good.
    let mut batches = std::mem::take(&mut command.batches);
    batches.push(gpu::Batch {
        waits: std::mem::take(&mut command.waits),
        command_buffer: command.command_buffer,
        signals: Vec::new(),
    });
    let mut submission = Some(Submission {
        gpu,
        timestamps: std::mem::replace(&mut command.timestamps, vk::QueryPool::null()),
        occlusion_pools: command.occlusion.take_pools(),
        pool: command.pool,
        command_buffer: batches[0].command_buffer,
        split_buffers: batches[1..]
            .iter()
            .map(|batch| batch.command_buffer)
            .collect(),
        fence: command.fence,
        descriptor_pools: std::mem::take(&mut command.descriptor_pools),
        retained: {
            command.retained_set.clear();
            std::mem::take(&mut command.retained)
        },
        uploads: std::mem::take(&mut command.uploads),
        framebuffers: std::mem::take(&mut command.framebuffers),
        secondaries: std::mem::take(&mut command.secondaries),
        captures: std::mem::take(&mut command.captures),
        completed_handlers: std::mem::take(&mut command.completed_handlers),
        presents: std::mem::take(&mut command.presents),
        object: OwnedObjcRef::retain(receiver),
        serial: 0,
        completion: Arc::clone(&command.completion),
        scheduled_time: 0.0,
        profile: command.profile.take(),
    });
    // SAFETY: recording began when the command buffer was made.
    if let Err(error) = unsafe { gpu.device.end_command_buffer(command.command_buffer) } {
        gpu::trace(format!("vkEndCommandBuffer failed with {error:?}"));
        command.completion.set(5); // MTLCommandBufferStatusError
        for block in std::mem::take(&mut command.scheduled_handlers) {
            // SAFETY: the retained block reference is transferred to the
            // handler worker, and `receiver` is live during this call.
            unsafe { crate::completion::deliver(block.into_raw(), receiver) };
        }
        // Nothing reached the queue, so the fence never signals: `finish`
        // returns every resource and delivers the completion handlers with
        // the error status.
        submission
            .take()
            .expect("unrecorded command")
            .finish(Err(format!("vkEndCommandBuffer failed with {error:?}")));
        return;
    }
    command.completion.set(2); // MTLCommandBufferStatusCommitted
    gpu.submissions.start(gpu);
    let completion = Arc::clone(&command.completion);
    let submitted = stats::timed(Counter::SubmitMicros, || {
        gpu.submit_ordered(&batches, command.fence, |serial| {
            let pending = submission.as_mut().expect("pending submission");
            pending.serial = serial;
            pending.scheduled_time = darwin::mach::media_time();
            completion.set(3); // MTLCommandBufferStatusScheduled
            for block in std::mem::take(&mut command.scheduled_handlers) {
                // SAFETY: the retained block reference is transferred to the
                // handler worker, and `receiver` is live during this call.
                unsafe { crate::completion::deliver(block.into_raw(), receiver) };
            }
            submission
                .as_mut()
                .expect("pending submission")
                .schedule_presents();
            gpu.submissions
                .push(submission.take().expect("pending submission"));
        })
    });
    match submitted {
        Ok(()) => {}
        Err(reason) => {
            gpu::trace(format!("command buffer submission failed: {reason}"));
            command.completion.set(5); // MTLCommandBufferStatusError
            for block in std::mem::take(&mut command.scheduled_handlers) {
                // SAFETY: as above. The error status is installed by `finish`.
                unsafe { crate::completion::deliver(block.into_raw(), receiver) };
            }
            submission
                .take()
                .expect("unsubmitted command")
                .finish(Err(reason));
        }
    }
}

pub(super) unsafe extern "C" fn wait_until_completed(receiver: Id, _sel: Sel) {
    // SAFETY: a command buffer of this crate's.
    unsafe { object::body::<CommandBuffer>(receiver) }
        .completion
        .wait_for(|status| status >= 4);
}

pub(super) unsafe extern "C" fn wait_until_scheduled(receiver: Id, _sel: Sel) {
    // SAFETY: as above. Error is also terminal and must release the waiter.
    unsafe { object::body::<CommandBuffer>(receiver) }
        .completion
        .wait_for(|status| status >= 3);
}

pub(super) unsafe extern "C" fn present_drawable(receiver: Id, _sel: Sel, drawable: Id) {
    if drawable.is_null() {
        return;
    }
    // SAFETY: a command buffer of this crate's.
    let command = unsafe { object::body::<CommandBuffer>(receiver) };
    if crate::present::submit(drawable) {
        command.presents.push(OwnedObjcRef::retain(drawable));
    }
}

pub(super) unsafe extern "C" fn present_drawable_at_time(
    receiver: Id,
    sel: Sel,
    drawable: Id,
    _at: f64,
) {
    // SAFETY: forwards to the untimed form; the runtime presents immediately.
    unsafe { present_drawable(receiver, sel, drawable) }
}

/// `-encodeSignalEvent:value:`: the event reaches `value` once everything
/// encoded before it has completed.
pub(super) unsafe extern "C" fn encode_signal_event(
    receiver: Id,
    _sel: Sel,
    event: Id,
    value: u64,
) {
    // SAFETY: a command buffer of this crate's.
    let command = unsafe { object::body::<CommandBuffer>(receiver) };
    if let Some(semaphore) = event_semaphore(command, event) {
        split(command, vec![(semaphore, value)], Vec::new());
    }
}

/// `-encodeWaitForEvent:value:`: nothing encoded after this starts until the
/// event reaches `value`.
pub(super) unsafe extern "C" fn encode_wait_for_event(
    receiver: Id,
    _sel: Sel,
    event: Id,
    value: u64,
) {
    // SAFETY: a command buffer of this crate's.
    let command = unsafe { object::body::<CommandBuffer>(receiver) };
    if let Some(semaphore) = event_semaphore(command, event) {
        split(command, Vec::new(), vec![(semaphore, value)]);
    }
}

/// The timeline behind `event`, which the command buffer keeps alive until
/// its work is done.
fn event_semaphore(command: &mut CommandBuffer, event: Id) -> Option<vk::Semaphore> {
    if !command.recording || command.secondary {
        return None;
    }
    if !object::is_kind(event, c"RosemaryMTLEvent")
        && !object::is_kind(event, c"RosemaryMTLSharedEvent")
    {
        gpu::trace("an event encoded into a command buffer is not an MTLEvent of this device");
        return None;
    }
    command.retain(event);
    // SAFETY: an event of this crate's, checked above.
    Some(unsafe { object::body::<crate::event::Event>(event) }.semaphore)
}

/// End the recording so far as one batch, signalling `signals` once it is
/// done, and continue in a new command buffer that first waits for `waits`.
/// Vulkan waits and signals only between submitted batches, and Metal allows
/// either between any two encoders.
fn split(
    command: &mut CommandBuffer,
    signals: Vec<(vk::Semaphore, u64)>,
    waits: Vec<(vk::Semaphore, u64)>,
) {
    let gpu = command.gpu;
    let allocate = vk::CommandBufferAllocateInfo::default()
        .command_pool(command.pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: the pool belongs to this recording command buffer.
    let next = match unsafe { gpu.device.allocate_command_buffers(&allocate) } {
        Ok(buffers) => buffers[0],
        Err(error) => {
            gpu::trace(format!(
                "vkAllocateCommandBuffers for an event failed with {error:?}"
            ));
            return;
        }
    };
    let begin =
        vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
    // SAFETY: freshly allocated; the current one is still recording, and no
    // encoder is open, which Metal requires around an event.
    let switched = unsafe { gpu.device.begin_command_buffer(next, &begin) }
        .and_then(|()| unsafe { gpu.device.end_command_buffer(command.command_buffer) });
    if let Err(error) = switched {
        gpu::trace(format!(
            "splitting a command buffer at an event failed with {error:?}"
        ));
        // SAFETY: never submitted.
        unsafe { gpu.device.free_command_buffers(command.pool, &[next]) };
        return;
    }
    command.batches.push(gpu::Batch {
        waits: std::mem::replace(&mut command.waits, waits),
        command_buffer: std::mem::replace(&mut command.command_buffer, next),
        signals,
    });
}
