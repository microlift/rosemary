//! `MTLParallelRenderCommandEncoder`: one render pass whose draws several
//! threads record at once.
//!
//! The pass is begun on the command buffer for secondary command buffers.
//! Each sub-encoder records into a secondary command buffer of its own, owned
//! by an internal command buffer object of its own - so its descriptor pools,
//! upload chunks and retained objects are touched by its thread alone. The
//! parallel encoder executes them in the order they were made, which is
//! Metal's order, ends the pass, and adopts everything they hold so it
//! retires with the submission.

use super::*;

pub(crate) struct ParallelEncoder {
    /// The ordinary render encoder that began the pass on the primary command
    /// buffer, and ends it.
    inner: Id,
    /// Each sub-encoder's internal command buffer, in creation order.
    children: Vec<Id>,
}

pub(super) unsafe extern "C" fn new_parallel_encoder(
    receiver: Id,
    _sel: Sel,
    descriptor: Id,
) -> Id {
    if descriptor.is_null() {
        return std::ptr::null_mut();
    }
    let inner = new_encoder(receiver, Kind::Render);
    if inner.is_null() {
        return inner;
    }
    // SAFETY: `MTLRenderPassDescriptor` is this crate's class.
    let described = unsafe { object::body::<RenderPassDescriptor>(descriptor) };
    begin_render_pass(inner, described, true);
    objc::arc::autorelease(object::make(
        c"RosemaryMTLParallelRenderCommandEncoder",
        ParallelEncoder {
            inner: objc::arc::retain(inner),
            children: Vec::new(),
        },
    ))
}

/// `-[MTLParallelRenderCommandEncoder renderCommandEncoder]`.
pub(super) unsafe extern "C" fn new_sub_encoder(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a parallel encoder of this crate's.
    let parallel = unsafe { object::body::<ParallelEncoder>(receiver) };
    // SAFETY: the inner encoder is a render encoder of this crate's.
    let inner = unsafe { object::body::<Encoder>(parallel.inner) };
    let Some((pass, framebuffer, area)) = inner.inheritance else {
        // The pass never began: every sub-encoder records nothing.
        return std::ptr::null_mut();
    };
    let parent = inner.command();
    let gpu = parent.gpu;
    let context = match gpu.acquire_secondary() {
        Ok(context) => context,
        Err(reason) => {
            gpu::trace(reason);
            return std::ptr::null_mut();
        }
    };
    let inheritance = vk::CommandBufferInheritanceInfo::default()
        .render_pass(pass)
        .subpass(0)
        .framebuffer(framebuffer);
    let begin = vk::CommandBufferBeginInfo::default()
        .flags(
            vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT
                | vk::CommandBufferUsageFlags::RENDER_PASS_CONTINUE,
        )
        .inheritance_info(&inheritance);
    // SAFETY: the command buffer is fresh or was reset with its pool.
    if let Err(error) = unsafe {
        gpu.device
            .begin_command_buffer(context.command_buffer, &begin)
    } {
        gpu::trace(format!(
            "vkBeginCommandBuffer (secondary) failed with {error:?}"
        ));
        gpu.release_secondary(context);
        return std::ptr::null_mut();
    }
    record_pass_defaults(&gpu.device, context.command_buffer, area);
    let child = object::make(
        c"RosemaryMTLCommandBuffer",
        CommandBuffer {
            gpu,
            timestamps: vk::QueryPool::null(),
            queue: objc::arc::retain(parent.queue),
            pool: context.pool,
            command_buffer: context.command_buffer,
            fence: vk::Fence::null(),
            batches: Vec::new(),
            waits: Vec::new(),
            descriptor_pools: Vec::new(),
            retained: Vec::new(),
            retained_set: AddressSet::default(),
            uploads: Vec::new(),
            upload_used: 0,
            framebuffers: Vec::new(),
            secondaries: Vec::new(),
            secondary: true,
            secondary_closed: false,
            captures: Vec::new(),
            scheduled_handlers: Vec::new(),
            completed_handlers: Vec::new(),
            presents: Vec::new(),
            completion: Arc::new(CommandCompletion::new()),
            recording: true,
            profile: parent.profile.clone(),
            occlusion: Occlusion::default(),
        },
    );
    let encoder = new_encoder(child, Kind::Render);
    if encoder.is_null() {
        objc::arc::release(child);
        return encoder;
    }
    // SAFETY: the encoder just made.
    let sub = unsafe { object::body::<Encoder>(encoder) };
    sub.sub_encoder = true;
    sub.in_render_pass = true;
    sub.resolve_mask = inner.resolve_mask;
    sub.depth_resolve = inner.depth_resolve;
    sub.visibility = inner.visibility;
    parallel.children.push(child);
    encoder
}

/// End a sub-encoder's secondary command buffer, once.
pub(super) fn close_secondary(command: &mut CommandBuffer) {
    if !command.secondary || command.command_buffer == vk::CommandBuffer::null() {
        return;
    }
    command
        .occlusion
        .end(&command.gpu.device, command.command_buffer);
    // SAFETY: recording began when the sub-encoder was made.
    if let Err(error) = unsafe {
        command
            .gpu
            .device
            .end_command_buffer(command.command_buffer)
    } {
        gpu::trace(format!(
            "vkEndCommandBuffer (secondary) failed with {error:?}"
        ));
    }
    command.secondary_closed = true;
}

/// `-[MTLParallelRenderCommandEncoder endEncoding]`.
pub(super) unsafe extern "C" fn end_parallel_encoding(receiver: Id, sel: Sel) {
    // SAFETY: a parallel encoder of this crate's.
    let parallel = unsafe { object::body::<ParallelEncoder>(receiver) };
    // SAFETY: the inner encoder is a render encoder of this crate's.
    let inner = unsafe { object::body::<Encoder>(parallel.inner) };
    let parent = inner.command();
    let mut executed = Vec::with_capacity(parallel.children.len());
    for child in std::mem::take(&mut parallel.children) {
        // SAFETY: an internal command buffer this encoder made.
        let command = unsafe { object::body::<CommandBuffer>(child) };
        if !command.secondary_closed {
            // A sub-encoder the guest never ended. Metal calls that an error;
            // its recording so far is kept.
            close_secondary(command);
        }
        if command.recording {
            command.recording = false;
            executed.push(command.command_buffer);
            parent
                .descriptor_pools
                .append(&mut command.descriptor_pools);
            parent.uploads.append(&mut command.uploads);
            parent.occlusion.adopt(&mut command.occlusion);
            command.retained_set.clear();
            parent.retained.append(&mut command.retained);
            parent.secondaries.push(gpu::SecondaryContext {
                pool: command.pool,
                command_buffer: command.command_buffer,
            });
        }
        objc::arc::release(child);
    }
    if inner.in_render_pass && !executed.is_empty() {
        // SAFETY: recording into the primary, inside the pass it began for
        // secondary command buffers; each one has ended.
        unsafe {
            parent
                .gpu
                .device
                .cmd_execute_commands(parent.command_buffer, &executed)
        };
    }
    // SAFETY: the inner encoder is a render encoder of this crate's.
    unsafe { end_encoding(parallel.inner, sel) };
}

pub(super) unsafe extern "C" fn parallel_dealloc(receiver: Id, sel: Sel) {
    // SAFETY: a parallel encoder of this crate's.
    let parallel = unsafe { object::body::<ParallelEncoder>(receiver) };
    if !parallel.children.is_empty() {
        // Never ended: end it now, so the pass is closed before the command
        // buffer is committed or dropped.
        unsafe { end_parallel_encoding(receiver, sel) };
    }
    objc::arc::release(std::mem::replace(&mut parallel.inner, std::ptr::null_mut()));
    // SAFETY: the body was established by `object::make` and is not read again.
    unsafe { std::ptr::drop_in_place(parallel as *mut ParallelEncoder) };
    objc::arc::finalize(receiver);
}

pub(super) fn install() {
    let parallel = object::register::<ParallelEncoder>(c"RosemaryMTLParallelRenderCommandEncoder");
    methods!(parallel, {
        "renderCommandEncoder" => new_sub_encoder, c"@16@0:8";
        "endEncoding" => end_parallel_encoding, c"v16@0:8";
        "dealloc" => parallel_dealloc, c"v16@0:8";
        // Every attachment is stored whatever the pass named.
        "setColorStoreAction:atIndex:" => crate::no_op, c"v32@0:8Q16Q24";
        "setDepthStoreAction:" => crate::no_op, c"v24@0:8Q16";
        "setStencilStoreAction:" => crate::no_op, c"v24@0:8Q16";
        "pushDebugGroup:" => crate::no_op, c"v24@0:8@16";
        "popDebugGroup" => crate::no_op, c"v16@0:8";
        "insertDebugSignpost:" => crate::no_op, c"v24@0:8@16";
        "label" => crate::nil, c"@16@0:8";
        "setLabel:" => crate::no_op, c"v24@0:8@16";
    });
}
