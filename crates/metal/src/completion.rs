use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, Once};

use objc::object::Id;

/// A handler waiting to run, with the command buffer it is passed. Addresses
/// rather than `Id`, because a raw pointer is not `Send` and these two are:
/// ownership of one reference to each moves to the worker with the entry.
struct Pending {
    block: usize,
    command_buffer: usize,
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
        // The guest may release the command buffer the moment `-commit` returns,
        // and the handler is handed it as its argument.
        command_buffer: objc::arc::retain(command_buffer) as usize,
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
        if let Some(invoke) = unsafe { objc::block::invoke(block) } {
            // SAFETY: a completion handler is `void (^)(id<MTLCommandBuffer>)`.
            let call: unsafe extern "C" fn(Id, Id) = unsafe { std::mem::transmute(invoke) };
            // SAFETY: the guest's handler, given the buffer it was registered on.
            unsafe { call(block, command_buffer) };
        }
        objc::arc::pool_pop(pool);
    }
    // The reference this entry owned, now spent.
    objc::block::release(block);
    objc::arc::release(command_buffer);
}
