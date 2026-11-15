use std::collections::VecDeque;
#[cfg(target_os = "macos")]
use std::sync::Arc;
use std::sync::{Condvar, Mutex, Once};

use objc::object::Id;

/// A handler waiting to run, with the command buffer it is passed. Addresses
/// rather than `Id`, because a raw pointer is not `Send` and these two are:
/// ownership of one reference to each moves to the worker with the entry.
struct Pending {
    block: usize,
    command_buffer: usize,
    invoke: bool,
    #[cfg(target_os = "macos")]
    progress: Option<(Arc<CallbackProgress>, bool)>,
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

/// Per-command-buffer count of guest handlers not yet finished on Rosemary's
/// Darwin-initialized callback worker. Index 0 = scheduled, 1 = completed.
#[cfg(target_os = "macos")]
pub(crate) struct CallbackProgress {
    pending: Mutex<[usize; 2]>,
    finished: Condvar,
}

#[cfg(target_os = "macos")]
impl CallbackProgress {
    pub(crate) fn new() -> Self {
        Self {
            pending: Mutex::new([0, 0]),
            finished: Condvar::new(),
        }
    }

    pub(crate) fn register(&self, scheduled: bool) {
        let mut count = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        count[usize::from(!scheduled)] += 1;
    }

    fn complete(&self, scheduled: bool) {
        let mut count = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let slot = &mut count[usize::from(!scheduled)];
        // A panic here would end the only completion worker and strand every
        // later handler and waiter in the process, so report and go on.
        match slot.checked_sub(1) {
            Some(remaining) => *slot = remaining,
            None => eprintln!("rosemary: Metal: a command buffer handler finished twice"),
        }
        self.finished.notify_all();
    }

    /// After Apple Metal's host wait has completed, every host handler has
    /// already queued its guest work. Wait for those guest handlers too.
    pub(crate) fn wait(&self, scheduled_only: bool) {
        if on_completion_worker() {
            let own = ACTIVE_PROGRESS
                .with(|active| active.borrow().contains(&(self as *const Self as usize)));
            if own {
                // Waiting for the currently executing callback to finish is
                // impossible; Metal permits a handler to inspect its status.
                return;
            }
            // A guest completion callback may wait for *another* buffer. This
            // is the sole worker, so it delivers queued handlers itself until
            // that buffer's are done, oldest first, as it would have anyway.
            loop {
                let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
                let unfinished = pending[0] != 0 || (!scheduled_only && pending[1] != 0);
                drop(pending);
                if !unfinished {
                    return;
                }
                // The worker has already entered Darwin TLS.
                unsafe { run(next_entry()) };
            }
        }
        let mut count = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        while count[0] != 0 || (!scheduled_only && count[1] != 0) {
            count = self.finished.wait(count).unwrap_or_else(|e| e.into_inner());
        }
    }
}

#[cfg(target_os = "macos")]
thread_local! {
    static IN_CALLBACK_WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    // A reentrant wait for our *own* command cannot wait for this callback
    // to finish. Other commands can be serviced by the same worker.
    static ACTIVE_PROGRESS: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(target_os = "macos")]
fn on_completion_worker() -> bool {
    IN_CALLBACK_WORKER.with(|executing| executing.get())
}

/// Hand one completion handler to the worker, which invokes and releases it.
///
/// # Safety
///
/// `block` is a block this crate copied and now gives away, and
/// `command_buffer` a live `MTLCommandBuffer` the caller has not released.
pub(crate) unsafe fn deliver(block: Id, command_buffer: Id) {
    enqueue(Pending {
        block: block as usize,
        command_buffer: objc::arc::retain(command_buffer) as usize,
        invoke: true,
        #[cfg(target_os = "macos")]
        progress: None,
        queued: crate::stats::timer(),
    });
}

/// Transfer the already-retained guest references into the Darwin worker,
/// which completes the per-command callback accounting after guest code runs.
///
/// # Safety
///
/// Caller transfers exactly one guest reference to both arguments.
#[cfg(target_os = "macos")]
pub(crate) unsafe fn deliver_owned(
    block: Id,
    command_buffer: Id,
    progress: Arc<CallbackProgress>,
    scheduled: bool,
) {
    debug_assert!(!block.is_null() && !command_buffer.is_null());
    enqueue(Pending {
        block: block as usize,
        command_buffer: command_buffer as usize,
        invoke: true,
        progress: Some((progress, scheduled)),
        queued: crate::stats::timer(),
    });
}

/// Finalize a host block that was disposed without invoking its guest handler.
/// This also counts the cancellation as finished so waiters are not stranded.
///
/// # Safety
///
/// Non-null arguments each transfer exactly one live guest reference.
#[cfg(target_os = "macos")]
pub(crate) unsafe fn retire_owned(
    block: Id,
    command_buffer: Id,
    progress: Arc<CallbackProgress>,
    scheduled: bool,
) {
    enqueue(Pending {
        block: block as usize,
        command_buffer: command_buffer as usize,
        invoke: false,
        progress: Some((progress, scheduled)),
        queued: crate::stats::timer(),
    });
}

fn enqueue(entry: Pending) {
    WORKER.call_once(|| {
        std::thread::Builder::new()
            .name("metal:completed".to_string())
            .spawn(worker)
            .expect("a thread for Metal completion handlers");
    });
    let mut pending = QUEUE.pending.lock().expect("the completion queue");
    pending.push_back(entry);
    QUEUE.ready.notify_one();
}

fn worker() {
    loop {
        // SAFETY: one reference to each arrived with the entry.
        unsafe { run(next_entry()) };
    }
}

/// The oldest queued handler, waiting for one if there is none.
fn next_entry() -> Pending {
    let mut pending = QUEUE.pending.lock().expect("the completion queue");
    loop {
        match pending.pop_front() {
            Some(entry) => return entry,
            None => pending = QUEUE.ready.wait(pending).expect("the completion queue"),
        }
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
    #[cfg(target_os = "macos")]
    let prior_worker_state = IN_CALLBACK_WORKER.with(|executing| executing.replace(true));
    #[cfg(target_os = "macos")]
    ACTIVE_PROGRESS.with(|active| {
        active.borrow_mut().push(
            entry
                .progress
                .as_ref()
                .map_or(0, |(progress, _)| Arc::as_ptr(progress) as usize),
        )
    });
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
    #[cfg(target_os = "macos")]
    if let Some((progress, scheduled)) = entry.progress {
        progress.complete(scheduled);
    }
    #[cfg(target_os = "macos")]
    ACTIVE_PROGRESS.with(|active| {
        active.borrow_mut().pop();
    });
    #[cfg(target_os = "macos")]
    IN_CALLBACK_WORKER.with(|executing| executing.set(prior_worker_state));
    crate::stats::finish_timer(crate::stats::Counter::HandlerRunMicros, running);
}
