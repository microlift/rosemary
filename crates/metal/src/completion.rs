use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, Once};

use objc::object::Id;

/// A handler waiting to run, with the command buffer it is passed. Addresses
/// rather than `Id`, because a raw pointer is not `Send` and these two are:
/// ownership of one reference to each moves to the worker with the entry.
struct Pending {
    block: usize,
    command_buffer: usize,
    invoke: bool,
    /// When it was queued, while statistics are on.
    queued: Option<std::time::Instant>,
}

struct Queue {
    pending: Mutex<VecDeque<Pending>>,
    ready: Condvar,
}

static QUEUE: Queue = Queue {
    pending: Mutex::new(VecDeque::new()),
    ready: Condvar::new(),
};
static WORKER: Once = Once::new();

/// Hand one completion handler to the worker, which invokes and releases it.
///
/// # Safety
///
/// `block` is a block this crate copied and now gives away, and
/// `command_buffer` a live `MTLCommandBuffer` the caller has not released.
pub(crate) unsafe fn deliver(block: Id, command_buffer: Id) {
    enqueue_owned(block, objc::arc::retain(command_buffer), true);
}

/// Transfer one already-retained guest block *and* command-buffer reference
/// from an Apple Metal callback thread. No guest retain/release or block
/// operation is permitted on the calling thread.
///
/// # Safety
///
/// The caller gives the worker ownership of exactly one guest reference to
/// each object; both must remain live until the worker processes this entry.
#[cfg(target_os = "macos")]
pub(crate) unsafe fn deliver_owned(block: Id, command_buffer: Id) {
    debug_assert!(!block.is_null() && !command_buffer.is_null());
    enqueue_owned(block, command_buffer, true);
}

/// Transfer any remaining guest-owned references after an Apple host block is
/// disposed (including disposal without the callback ever being invoked).
/// This is safe to call from a native Metal callback thread, because the
/// worker alone finalizes the guest references with Darwin TLS installed.
///
/// # Safety
///
/// Non-null arguments are each exactly one live, owned guest reference.
#[cfg(target_os = "macos")]
pub(crate) unsafe fn retire_owned(block: Id, command_buffer: Id) {
    if block.is_null() && command_buffer.is_null() {
        return;
    }
    enqueue_owned(block, command_buffer, false);
}

fn enqueue_owned(block: Id, command_buffer: Id, invoke: bool) {
    WORKER.call_once(|| {
        // Detached deliberately: it runs for the life of the process, and there
        // is no point in the process at which Metal is known to be finished.
        std::thread::Builder::new()
            .name("metal:completed".to_string())
            .spawn(worker)
            .expect("a thread for Metal completion handlers");
    });
    let entry = Pending {
        block: block as usize,
        command_buffer: command_buffer as usize,
        invoke,
        queued: crate::stats::timer(),
    };
    let mut pending = QUEUE.pending.lock().expect("the completion queue");
    pending.push_back(entry);
    QUEUE.ready.notify_one();
}

fn worker() {
    loop {
        let entry = {
            let mut pending = QUEUE.pending.lock().expect("the completion queue");
            loop {
                match pending.pop_front() {
                    Some(entry) => break entry,
                    None => {
                        pending = QUEUE.ready.wait(pending).expect("the completion queue");
                    }
                }
            }
        };
        // SAFETY: one reference to each arrived with the entry.
        unsafe { run(entry) };
    }
}

/// # Safety
///
/// The entry carries one reference to a live block and one to a live command
/// buffer, both of which this consumes.
unsafe fn run(entry: Pending) {
    let block = entry.block as Id;
    let command_buffer = entry.command_buffer as Id;
    if let Some(queued) = entry.queued {
        let waited = queued.elapsed().as_micros() as u64;
        crate::stats::count(crate::stats::Counter::Handlers);
        crate::stats::add(crate::stats::Counter::HandlerQueueMicros, waited);
        crate::stats::peak(crate::stats::Counter::HandlerQueuePeakMicros, waited);
    }
    let running = crate::stats::timer();
    {
        // This thread is the runtime's, so Darwin code running on it needs the
        // thread-local array it reads `errno`, its stack canary and its
        // allocator identity out of. The dispatch pool does the same thing for
        // the same reason.
        let _darwin = darwin::tls::DarwinThread::enter();
        // Metal runs handlers on a dispatch queue, which drains a pool around
        // each one; without it whatever the handler autoreleases leaks.
        let pool = objc::arc::pool_push();
        // SAFETY: `block` was copied by `objc_retainBlock` and is still live.
        if entry.invoke
            && let Some(invoke) = unsafe { objc::block::invoke(block) }
        {
            // SAFETY: a completion handler is `void (^)(id<MTLCommandBuffer>)`.
            let call: unsafe extern "C" fn(Id, Id) = unsafe { std::mem::transmute(invoke) };
            // SAFETY: the guest's handler, given the buffer it was registered on.
            unsafe { call(block, command_buffer) };
        }
        objc::arc::pool_pop(pool);
        // The references this entry owned, now spent. The last reference to
        // either may run guest code: the block's dispose helper, or whatever
        // the guest associated with the command buffer.
        if !block.is_null() {
            objc::block::release(block);
        }
        if !command_buffer.is_null() {
            objc::arc::release(command_buffer);
        }
    }
    crate::stats::finish_timer(crate::stats::Counter::HandlerRunMicros, running);
}
