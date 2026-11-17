use super::*;

pub(super) struct PendingCommand {
    object: OwnedObjcRef,
    commit: Option<gpu::Commit>,
}

impl CommandQueue {
    /// Publish only the committed prefix, preserving reservations across threads.
    pub(super) fn submit_ready(&self, command: Id, commit: Option<gpu::Commit>) {
        let mut released = Vec::new();
        {
            let mut pending = self.pending.lock().expect("Metal command queue");
            let index = pending
                .iter()
                .position(|entry| entry.object.as_id() == command)
                .expect("a committed buffer was enqueued");
            match commit {
                Some(commit) => pending[index].commit = Some(commit),
                // A failed recording must not strand later reservations.
                None => released.push(pending.remove(index).unwrap().object),
            }
            while pending.front().is_some_and(|entry| entry.commit.is_some()) {
                let entry = pending.pop_front().unwrap();
                // Keep the queue locked until GPU submission order is reserved.
                self.gpu.commit(entry.commit.unwrap());
                released.push(entry.object);
            }
        }
        // Releasing a command buffer can run guest code.
        drop(released);
    }
}

pub(super) unsafe extern "C" fn enqueue(receiver: Id, _sel: Sel) {
    // SAFETY: a live command buffer and the queue it retains.
    let command = unsafe { object::body_ref::<CommandBuffer>(receiver) };
    if command.completion.get() != 0 {
        return;
    }
    let queue = unsafe { object::body_ref::<CommandQueue>(command.queue) };
    let mut pending = queue.pending.lock().expect("Metal command queue");
    pending.push_back(PendingCommand {
        object: OwnedObjcRef::retain(receiver),
        commit: None,
    });
    command.completion.set(1); // MTLCommandBufferStatusEnqueued
}

pub(crate) fn new_command_queue(gpu: &'static Gpu, device: Id) -> Id {
    object::make(
        c"RosemaryMTLCommandQueue",
        CommandQueue {
            gpu,
            device: objc::arc::retain(device),
            pending: Mutex::default(),
        },
    )
}

/// `-[MTLCommandQueue dealloc]`.
pub(super) unsafe extern "C" fn queue_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a command queue of this crate's.
    let queue = unsafe { object::body::<CommandQueue>(receiver) };
    objc::arc::release(std::mem::replace(&mut queue.device, std::ptr::null_mut()));
    // SAFETY: its command buffers no longer retain this queue.
    unsafe { std::ptr::drop_in_place(queue as *mut CommandQueue) };
    objc::arc::finalize(receiver);
}

pub(super) unsafe extern "C" fn queue_device(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a command queue of this crate's.
    unsafe { object::body_ref::<CommandQueue>(receiver) }.device
}

/// `MTLCommandBufferDescriptor`.
pub(super) struct CommandBufferDescriptor {
    retained_references: bool,
    error_options: u64,
}

pub(super) unsafe extern "C" fn command_buffer_descriptor_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a descriptor straight from `+alloc`.
    unsafe {
        object::init(
            receiver,
            CommandBufferDescriptor {
                retained_references: true,
                error_options: 0,
            },
        )
    };
    receiver
}

pub(super) unsafe extern "C" fn retained_references(receiver: Id, _sel: Sel) -> bool {
    // SAFETY: a descriptor of this crate's.
    unsafe { object::body::<CommandBufferDescriptor>(receiver) }.retained_references
}

pub(super) unsafe extern "C" fn set_retained_references(receiver: Id, _sel: Sel, value: bool) {
    // SAFETY: as above.
    unsafe { object::body::<CommandBufferDescriptor>(receiver) }.retained_references = value;
}

pub(super) unsafe extern "C" fn error_options(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: as above.
    unsafe { object::body::<CommandBufferDescriptor>(receiver) }.error_options
}

pub(super) unsafe extern "C" fn set_error_options(receiver: Id, _sel: Sel, value: u64) {
    // SAFETY: as above.
    unsafe { object::body::<CommandBufferDescriptor>(receiver) }.error_options = value;
}

/// `-commandBufferWithDescriptor:`. Every command buffer retains what it
/// uses, which unretained references allow; per-encoder execution status
/// is not reported, so `errorOptions` adds nothing to a failure's error.
pub(super) unsafe extern "C" fn new_command_buffer_with_descriptor(
    receiver: Id,
    sel: Sel,
    _descriptor: Id,
) -> Id {
    // SAFETY: forwards to the plain form on the same queue.
    unsafe { new_command_buffer(receiver, sel) }
}
