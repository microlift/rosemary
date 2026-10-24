use std::collections::VecDeque;
use std::ffi::{CString, c_char, c_void};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use super::block;
use super::object::{self, Function, Header, Kind};

/// What a queue holds: either a block copied to the heap, or a function and its
/// context. Both are guest code by the time they arrive here.
enum Work {
    /// A block copied by [`block::copy`] because it outlives its caller's frame:
    /// invoked, then released.
    Owned(u64),
    /// A block the caller keeps alive for the whole call, as `dispatch_sync` does.
    /// Invoked and *not* released - the reference belongs to the caller, and
    /// releasing it here would be one release too many.
    Borrowed(u64),
    Function(Function, u64),
}

impl Work {
    /// # Safety
    ///
    /// The block or function is callable from this thread - for guest code, its
    /// pointers have already been replaced by thunks.
    unsafe fn run(self) {
        // A pool worker is a thread this runtime started, so its thread pointer
        // is the host's and the block about to run is Darwin code: it reads
        // `pthread_self`, `errno` and its stack canary straight off the thread
        // pointer, and its allocator reads a thread identity the same way. Free
        // on a thread that is inside Darwin code already - the main queue's.
        let _darwin = crate::tls::DarwinThread::enter();
        match self {
            // SAFETY: the caller's guarantee. The release pairs with the copy that
            // put it on the queue.
            Work::Owned(b) => unsafe {
                block::invoke(b);
                block::release(b);
            },
            Work::Borrowed(b) => unsafe { block::invoke(b) },
            Work::Function(f, context) => unsafe { f(context as *mut c_void) },
        }
    }
}

/// One queued item, and whoever is waiting for it to finish.
struct Item {
    work: Work,
    /// Set by `dispatch_sync`, which cannot return until the work has run.
    done: Option<Arc<Done>>,
}

impl Item {
    /// # Safety
    ///
    /// As [`Work::run`].
    unsafe fn run(self) {
        let done = self.done;
        unsafe { self.work.run() };
        if let Some(done) = done {
            done.signal();
        }
    }
}

/// A one-shot completion, for `dispatch_sync` across threads.
#[derive(Default)]
struct Done {
    finished: Mutex<bool>,
    woken: Condvar,
}

impl Done {
    fn signal(&self) {
        *self.finished.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.woken.notify_all();
    }

    fn wait(&self) {
        let mut finished = self.finished.lock().unwrap_or_else(|e| e.into_inner());
        while !*finished {
            finished = self.woken.wait(finished).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// `#[repr(C)]` is load-bearing, not decoration: [`super::object`] recovers this
/// object by casting a `*mut Header`, which is only the same address as the whole
/// object if `header` is guaranteed to come first. A plain Rust struct may reorder
/// its fields, and one that did would read another field's bytes as the refcount.
#[repr(C)]
pub struct Queue {
    /// Read through a `*mut Header` rather than through this field, which is what
    /// makes it look unused: it must be first, and its bytes are the object
    /// protocol's.
    #[allow(dead_code)]
    header: Header,
    label: CString,
    concurrent: bool,
    main: bool,
    target: u64,
    state: Mutex<State>,
}

struct State {
    pending: VecDeque<Item>,
    /// Held by the one worker allowed to run this serial queue's work. Unused for a
    /// concurrent queue, which has no such restriction.
    draining: bool,
    /// `dispatch_suspend`s not yet balanced by a resume. Work stays queued.
    suspended: u32,
}

/// The queue a handle names, if it names a live one.
///
/// # Safety
///
/// `handle` is null or an object this module allocated.
unsafe fn queue<'a>(handle: u64) -> Option<&'a Queue> {
    // SAFETY: the caller's guarantee, and the kind check makes the cast sound.
    unsafe { object::typed(handle, Kind::Queue) }.map(|h| {
        // SAFETY: a `Header` of kind `Queue` is the first field of a `Queue`.
        unsafe { &*(h as *const Header as *const Queue) }
    })
}

fn new(label: &str, concurrent: bool, main: bool) -> u64 {
    let queue = Box::new(Queue {
        header: Header::new(Kind::Queue),
        label: CString::new(label).unwrap_or_default(),
        concurrent,
        main,
        target: 0,
        state: Mutex::new(State {
            pending: VecDeque::new(),
            draining: false,
            suspended: 0,
        }),
    });
    Box::into_raw(queue) as u64
}

/// A queue the process owns, which no release can free.
fn global(label: &str, concurrent: bool, main: bool) -> u64 {
    let handle = new(label, concurrent, main);
    // SAFETY: just allocated by this module.
    unsafe { object::make_global(handle) };
    handle
}

/// A serial queue, for this module's own tests and callers.
pub fn create_serial(label: &str) -> u64 {
    new(label, false, false)
}

/// The main thread, as `pthread` numbers threads. Recorded rather than assumed:
/// `dispatch_sync` to the main queue has to know whether it is already there, and
/// answering that wrongly is either a deadlock or a broken ordering promise.
static MAIN_THREAD: AtomicU32 = AtomicU32::new(0);

/// Record the calling thread as the main one.
///
/// Called from `provide`, which runs on the main thread while the guest is being
/// linked - before any queue exists, let alone any work on one.
pub fn note_main_thread() {
    MAIN_THREAD.store(crate::pthread::id(), Ordering::Release);
}

fn on_main_thread() -> bool {
    MAIN_THREAD.load(Ordering::Acquire) == crate::pthread::id()
}

/// `_dispatch_main_q`, which a guest takes the address of.
///
/// `dispatch_get_main_queue()` is an inline function in Darwin's SDK, so a guest
/// never calls anything to get this - it references the object directly, and the
/// symbol has to resolve to a real one.
pub fn main_queue() -> u64 {
    static MAIN: OnceLock<u64> = OnceLock::new();
    *MAIN.get_or_init(|| global("com.apple.main-thread", false, true))
}

/// What wakes the main thread's run loop when work lands on the main queue.
static MAIN_WAKE: AtomicUsize = AtomicUsize::new(0);

/// Install the waker for the main queue.
///
/// On a Mac the main queue is a port the main run loop listens on, so work sent
/// to it from another thread ends the loop's wait at once. Without a waker it
/// waits for whatever else ends the wait - a frame posted by a worker sat out the
/// rest of the run loop's slice.
pub fn set_main_wake_hook(hook: fn()) {
    MAIN_WAKE.store(hook as usize, Ordering::Release);
    // Anything queued before the hook existed is still owed a turn.
    hook();
}

fn wake_main() {
    let hook = MAIN_WAKE.load(Ordering::Acquire);
    if hook != 0 {
        // SAFETY: only `set_main_wake_hook` stores here, and it stores a `fn()`.
        let hook: fn() = unsafe { std::mem::transmute(hook) };
        hook();
    }
}

/// Run everything the main queue is holding.
///
/// Called from whatever turns the main thread's run loop. Only the work already
/// queued on entry runs: a block that submits to the main queue would otherwise be
/// able to hold the run loop here forever, and starving the run loop starves drawing
/// and input along with it.
pub fn drain_main() {
    let handle = main_queue();
    // SAFETY: the main queue is this module's own and lives for the process.
    let Some(main) = (unsafe { queue(handle) }) else {
        return;
    };
    let mut budget = {
        let state = main.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.suspended > 0 {
            0
        } else {
            state.pending.len()
        }
    };
    while budget > 0 {
        let Some(item) = main
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .pop_front()
        else {
            return;
        };
        budget -= 1;
        // SAFETY: guest work whose pointers are thunks. Run with no lock held: it is
        // guest code, and it may submit to this very queue.
        run_tracked(handle, || unsafe { item.run() });
    }
}

thread_local! {
    /// The queues whose work this thread is currently running, innermost last.
    ///
    /// A stack rather than one value because a block may `dispatch_sync` to another
    /// queue, and the block that runs there is inside both.
    static CURRENT: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Run `f` with `handle` noted as the queue this thread is inside.
fn run_tracked<R>(handle: u64, f: impl FnOnce() -> R) -> R {
    struct Pop;
    impl Drop for Pop {
        fn drop(&mut self) {
            CURRENT.with(|c| {
                c.borrow_mut().pop();
            });
        }
    }
    CURRENT.with(|c| c.borrow_mut().push(handle));
    // Popped by the guard even if the work unwinds, so a queue is never left
    // falsely marked as this thread's.
    let _pop = Pop;
    f()
}

fn inside(handle: u64) -> bool {
    CURRENT.with(|c| c.borrow().contains(&handle))
}

fn current_label() -> *const c_char {
    let handle = CURRENT.with(|c| c.borrow().last().copied());
    // SAFETY: a handle this module put there, still live while this thread runs it.
    match handle.and_then(|h| unsafe { queue(h) }) {
        Some(q) => q.label.as_ptr(),
        None => c"".as_ptr(),
    }
}

struct Pool {
    state: Mutex<PoolState>,
    wake: Condvar,
}

struct PoolState {
    /// Queues with work to run. A queue may appear more than once; a duplicate
    /// finds nothing to do and costs one wakeup.
    runnable: VecDeque<u64>,
    idle: usize,
    threads: usize,
}

/// The ceiling on worker threads.
///
/// Generous because these threads are mostly *blocked*, not running: a block that
/// waits on a semaphore holds a worker while using no CPU, and a pool sized to the
/// core count would be entirely consumed by a handful of those.
fn max_threads() -> usize {
    // SAFETY: `sysconf` takes a name and returns a long.
    let cpus = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    (cpus.max(1) as usize * 4).clamp(8, 128)
}

fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| Pool {
        state: Mutex::new(PoolState {
            runnable: VecDeque::new(),
            idle: 0,
            threads: 0,
        }),
        wake: Condvar::new(),
    })
}

/// Hand `handle` to the pool, keeping it alive until a worker has drained it.
fn schedule(handle: u64) {
    object::retain(handle);
    let target = target(handle);
    if target != 0 {
        enqueue(target, Work::Function(run_targeted, handle));
        return;
    }
    let pool = pool();
    let mut state = pool.state.lock().unwrap_or_else(|e| e.into_inner());
    state.runnable.push_back(handle);
    // A new thread only when every existing one is busy: an idle worker is about to
    // take this, and spawning alongside it would grow the pool without bound.
    if state.idle == 0 && state.threads < max_threads() {
        state.threads += 1;
        drop(state);
        if std::thread::Builder::new()
            .name("dispatch".into())
            .stack_size(512 * 1024)
            .spawn(worker)
            .is_err()
        {
            // The thread did not start, so the count must not claim it did - and
            // whoever is already running has to be told there is work.
            let mut state = pool.state.lock().unwrap_or_else(|e| e.into_inner());
            state.threads -= 1;
            drop(state);
            pool.wake.notify_all();
        }
        return;
    }
    drop(state);
    pool.wake.notify_one();
}

/// How long an idle worker waits before retiring.
const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn worker() {
    let pool = pool();
    loop {
        let handle = {
            let mut state = pool.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(handle) = state.runnable.pop_front() {
                    break handle;
                }
                state.idle += 1;
                let (next, timeout) = pool
                    .wake
                    .wait_timeout(state, IDLE_TIMEOUT)
                    .unwrap_or_else(|e| e.into_inner());
                state = next;
                state.idle -= 1;
                if timeout.timed_out() && state.runnable.is_empty() {
                    state.threads -= 1;
                    return;
                }
            }
        };
        run_one(handle);
        // SAFETY: the reference `schedule` took for the pool.
        unsafe { object::release(handle) };
    }
}

/// Run at most one item from `handle`, respecting a serial queue's exclusion.
fn run_one(handle: u64) {
    // SAFETY: the pool holds a reference, so the queue is live.
    let Some(q) = (unsafe { queue(handle) }) else {
        return;
    };
    let item = {
        let mut state = q.state.lock().unwrap_or_else(|e| e.into_inner());
        // Another worker already owns this serial queue. It re-enqueues when it is
        // done, so the work is not lost by returning here.
        if (!q.concurrent && state.draining) || state.suspended > 0 {
            return;
        }
        match state.pending.pop_front() {
            None => return,
            Some(item) => {
                if !q.concurrent {
                    state.draining = true;
                }
                item
            }
        }
    };
    // A worker thread has no pool of its own, and Apple drains one around each
    // work item; without it whatever the work autoreleases leaks.
    let pool = objc::arc::pool_push();
    // SAFETY: guest work whose pointers are thunks. No lock is held: the work is
    // guest code and may submit to this queue or block on another.
    run_tracked(handle, || unsafe { item.run() });
    objc::arc::pool_pop(pool);
    let more = {
        let mut state = q.state.lock().unwrap_or_else(|e| e.into_inner());
        if !q.concurrent {
            state.draining = false;
        }
        !state.pending.is_empty()
    };
    if more {
        schedule(handle);
    }
}

/// Put `item` on `handle`.
fn submit(handle: u64, item: Item) {
    // SAFETY: the caller holds a reference for the duration of the call.
    let Some(q) = (unsafe { queue(handle) }) else {
        return;
    };
    q.state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pending
        .push_back(item);
    // The main queue is drained by the main thread alone; handing it to the pool
    // would run its work on the wrong thread, which is the one thing it promises.
    if q.main {
        wake_main();
    } else {
        schedule(handle);
    }
}

/// Submit work from outside, as the entry points below do.
fn enqueue(handle: u64, work: Work) {
    submit(handle, Item { work, done: None });
}

/// `dispatch_queue_t dispatch_queue_create(const char *label, dispatch_queue_attr_t)`.
///
/// A null attribute is `DISPATCH_QUEUE_SERIAL`; the only other one a guest can name
/// is the concurrent attribute this module publishes.
pub unsafe extern "C" fn queue_create(label: *const c_char, attr: u64) -> u64 {
    // SAFETY: a null or NUL-terminated C string from the guest.
    let label = unsafe { label_of(label) };
    new(&label, attr == concurrent_attr(), false)
}

/// `void dispatch_suspend(dispatch_object_t)`.
pub unsafe extern "C" fn suspend(handle: u64) {
    if super::source::suspend(handle) {
        return;
    }
    // SAFETY: the guest holds a reference for the duration of the call.
    if let Some(q) = unsafe { queue(handle) } {
        q.state.lock().unwrap_or_else(|e| e.into_inner()).suspended += 1;
    }
}

/// `void dispatch_resume(dispatch_object_t)`.
pub unsafe extern "C" fn resume(handle: u64) {
    if super::source::resume(handle) {
        return;
    }
    // SAFETY: the guest holds a reference for the duration of the call.
    let Some(q) = (unsafe { queue(handle) }) else {
        return;
    };
    let runnable = {
        let mut state = q.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.suspended == 0 {
            return;
        }
        state.suspended -= 1;
        state.suspended == 0 && !state.pending.is_empty()
    };
    if runnable {
        if q.main {
            wake_main();
        } else {
            schedule(handle);
        }
    }
}

/// `dispatch_queue_t dispatch_queue_create_with_target(const char *,
/// dispatch_queue_attr_t, dispatch_queue_t target)`.
///
pub unsafe extern "C" fn queue_create_with_target(
    label: *const c_char,
    attr: u64,
    target: u64,
) -> u64 {
    let handle = unsafe { queue_create(label, attr) };
    object::retain(target);
    unsafe {
        (*(handle as *mut Queue)).target = target;
    }
    handle
}

impl Drop for Queue {
    fn drop(&mut self) {
        unsafe {
            object::release(self.target);
        }
    }
}

pub(crate) fn target(handle: u64) -> u64 {
    unsafe { queue(handle) }.map_or(0, |q| q.target)
}

unsafe extern "C" fn run_targeted(context: *mut c_void) {
    let handle = context as u64;
    run_one(handle);
    unsafe {
        object::release(handle);
    }
}

/// # Safety
///
/// `label` is null or a NUL-terminated C string.
unsafe fn label_of(label: *const c_char) -> String {
    if label.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(label) }
        .to_string_lossy()
        .into_owned()
}

/// `const char *dispatch_queue_get_label(dispatch_queue_t)`.
///
/// A null queue is `DISPATCH_CURRENT_QUEUE_LABEL`, which asks about the queue the
/// caller is running on.
pub unsafe extern "C" fn queue_get_label(handle: u64) -> *const c_char {
    if handle == 0 {
        return current_label();
    }
    // SAFETY: the guest holds a reference for the duration of the call.
    match unsafe { queue(handle) } {
        Some(q) => q.label.as_ptr(),
        None => c"".as_ptr(),
    }
}

/// The global concurrent queues, one per quality of service.
///
/// Never released: they are the process's, and a guest that releases one - which
/// Darwin ignores - must not be able to free it.
fn global_queue(qos: i64) -> u64 {
    static GLOBALS: OnceLock<[u64; 5]> = OnceLock::new();
    let globals = GLOBALS.get_or_init(|| {
        [
            global("com.apple.root.background-qos", true, false),
            global("com.apple.root.utility-qos", true, false),
            global("com.apple.root.default-qos", true, false),
            global("com.apple.root.user-initiated-qos", true, false),
            global("com.apple.root.user-interactive-qos", true, false),
        ]
    });
    // A guest passes either a `qos_class_t` or one of the older
    // `DISPATCH_QUEUE_PRIORITY_*`, which are small signed numbers. The two spaces do
    // not collide, so one match answers both.
    use crate::pthread::{
        QOS_CLASS_BACKGROUND, QOS_CLASS_USER_INITIATED, QOS_CLASS_USER_INTERACTIVE,
        QOS_CLASS_UTILITY,
    };
    const PRIORITY_BACKGROUND: i64 = i16::MIN as i64;
    let index = match qos {
        n if n == QOS_CLASS_BACKGROUND as i64 => 0,
        n if n == QOS_CLASS_UTILITY as i64 => 1,
        n if n == QOS_CLASS_USER_INITIATED as i64 => 3,
        n if n == QOS_CLASS_USER_INTERACTIVE as i64 => 4,
        PRIORITY_BACKGROUND => 0,
        -2 => 1,
        2 => 3,
        0 | 0x15 => 2,
        _ => return 0,
    };
    globals[index]
}

/// `dispatch_queue_t dispatch_get_global_queue(long identifier, unsigned long flags)`.
pub unsafe extern "C" fn get_global_queue(identifier: i64, flags: u64) -> u64 {
    if flags != 0 {
        return 0;
    }
    global_queue(identifier)
}

/// `_dispatch_queue_attr_concurrent`, which `DISPATCH_QUEUE_CONCURRENT` names.
///
/// Its address is the whole of it: nothing reads the object, and
/// [`queue_create`] only compares.
pub fn concurrent_attr() -> u64 {
    static ATTR: OnceLock<u64> = OnceLock::new();
    *ATTR.get_or_init(|| Box::into_raw(Box::new(0u64)) as u64)
}

/// `void dispatch_async(dispatch_queue_t, dispatch_block_t)`.
///
/// The block outlives this call, so it is copied. A copy that fails is dropped
/// rather than queued: queueing the original would run a block whose captures live
/// in the caller's frame, long after that frame is gone.
pub unsafe extern "C" fn async_(handle: u64, block: u64) {
    let Some(copied) = block::copy(block) else {
        return;
    };
    enqueue(handle, Work::Owned(copied));
}

/// Transfer an already copied block to the queue.
pub(crate) fn async_owned(handle: u64, block: u64) {
    enqueue(handle, Work::Owned(block));
}

/// `void dispatch_async_f(dispatch_queue_t, void *context, dispatch_function_t)`.
pub unsafe extern "C" fn async_f(handle: u64, context: u64, function: u64) {
    if function == 0 {
        return;
    }
    // SAFETY: a `dispatch_function_t`, called later with `context`.
    let f = unsafe { std::mem::transmute::<u64, Function>(function) };
    enqueue(handle, Work::Function(f, context));
}

/// Run `work` on `handle` and do not return until it has.
///
/// Three cases, and the first two are why this is not simply a submit-and-wait:
/// running on the queue already, or being the main thread and asked for the main
/// queue, would both wait for a drain that this thread is the one responsible for.
/// Darwin deadlocks on the first; running the work inline is what the caller wanted
/// and cannot be told apart from having waited.
fn sync_work(handle: u64, work: Work) {
    // SAFETY: the guest holds a reference for the duration of the call.
    let Some(q) = (unsafe { queue(handle) }) else {
        return;
    };
    if inside(handle) || (q.main && on_main_thread()) {
        // SAFETY: guest work whose pointers are thunks.
        return run_tracked(handle, || unsafe { work.run() });
    }
    let done = Arc::new(Done::default());
    submit(
        handle,
        Item {
            work,
            done: Some(Arc::clone(&done)),
        },
    );
    done.wait();
}

/// `void dispatch_sync(dispatch_queue_t, dispatch_block_t)`.
///
/// No copy: the block cannot outlive a call that does not return until it has run.
pub unsafe extern "C" fn sync(handle: u64, block: u64) {
    if block == 0 {
        return;
    }
    sync_work(handle, Work::Borrowed(block));
}

/// `void dispatch_sync_f(dispatch_queue_t, void *context, dispatch_function_t)`.
pub unsafe extern "C" fn sync_f(handle: u64, context: u64, function: u64) {
    if function == 0 {
        return;
    }
    // SAFETY: a `dispatch_function_t`, called with `context` before this returns.
    let f = unsafe { std::mem::transmute::<u64, Function>(function) };
    sync_work(handle, Work::Function(f, context));
}

struct Timer {
    deadline: u64,
    /// Breaks ties so that two timers with one deadline keep submission order.
    seq: u64,
    queue: u64,
    work: Work,
}

// SAFETY: the block and function pointers a `Work` holds are process-wide code
// addresses and heap blocks, both valid on whichever thread runs them.
unsafe impl Send for Timer {}

struct Timers {
    state: Mutex<Armed>,
    wake: Condvar,
}

struct Armed {
    timers: Vec<Timer>,
    next_seq: u64,
}

fn timers() -> &'static Timers {
    static TIMERS: OnceLock<Timers> = OnceLock::new();
    TIMERS.get_or_init(|| Timers {
        state: Mutex::new(Armed {
            timers: Vec::new(),
            next_seq: 0,
        }),
        wake: Condvar::new(),
    })
}

/// Start the one thread that watches deadlines, if it is not running.
fn start_timer_thread() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let _ = std::thread::Builder::new()
            .name("dispatch-timer".into())
            .spawn(timer_thread);
    });
}

/// One thread for every timer, rather than a thread per `dispatch_after`.
///
/// It holds no lock while submitting, because a submit can spawn a worker and a
/// worker can add a timer.
fn timer_thread() {
    let timers = timers();
    loop {
        let mut due = Vec::new();
        {
            let mut state = timers.state.lock().unwrap_or_else(|e| e.into_inner());
            let now = super::time::now();
            let mut index = 0;
            while index < state.timers.len() {
                if state.timers[index].deadline <= now {
                    due.push(state.timers.swap_remove(index));
                } else {
                    index += 1;
                }
            }
            if due.is_empty() {
                // Sleep until the earliest deadline, or until something is added.
                let wait = state
                    .timers
                    .iter()
                    .map(|t| t.deadline.saturating_sub(now))
                    .min()
                    .map(std::time::Duration::from_nanos)
                    .unwrap_or(std::time::Duration::from_millis(50))
                    .min(std::time::Duration::from_millis(50));
                let _unused = timers
                    .wake
                    .wait_timeout(state, wait)
                    .unwrap_or_else(|e| e.into_inner());
                continue;
            }
        }
        // Order is by deadline, then by submission, so two timers that came due in
        // one pass run in the order a caller would expect.
        due.sort_by_key(|t| (t.deadline, t.seq));
        for timer in due {
            enqueue(timer.queue, timer.work);
            // SAFETY: the reference taken when the timer was armed.
            unsafe { object::release(timer.queue) };
        }
    }
}

fn arm(deadline: u64, handle: u64, work: Work) {
    // A deadline that never arrives would hold the block forever; Darwin never runs
    // it either, so the block is simply dropped.
    if deadline == super::time::FOREVER {
        if let Work::Owned(b) = work {
            block::release(b);
        }
        return;
    }
    // SAFETY: the queue must outlive the wait, so the timer holds a reference.
    object::retain(handle);
    start_timer_thread();
    let timers = timers();
    let mut state = timers.state.lock().unwrap_or_else(|e| e.into_inner());
    let seq = state.next_seq;
    state.next_seq += 1;
    state.timers.push(Timer {
        deadline,
        seq,
        queue: handle,
        work,
    });
    drop(state);
    timers.wake.notify_all();
}

/// `void dispatch_after(dispatch_time_t when, dispatch_queue_t, dispatch_block_t)`.
pub unsafe extern "C" fn after(when: u64, handle: u64, block: u64) {
    let Some(copied) = block::copy(block) else {
        return;
    };
    arm(when, handle, Work::Owned(copied));
}

/// `void dispatch_after_f(dispatch_time_t, dispatch_queue_t, void *,
/// dispatch_function_t)`.
pub unsafe extern "C" fn after_f(when: u64, handle: u64, context: u64, function: u64) {
    if function == 0 {
        return;
    }
    // SAFETY: a `dispatch_function_t`, called later with `context`.
    let f = unsafe { std::mem::transmute::<u64, Function>(function) };
    arm(when, handle, Work::Function(f, context));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static ORDER: Mutex<Vec<u64>> = Mutex::new(Vec::new());

    /// Counts into whatever `AtomicU32` the caller passed as its context, so each
    /// test owns its own counter and the suite can run in parallel.
    unsafe extern "C" fn bump(context: *mut c_void) {
        // SAFETY: every caller below passes the address of a live `AtomicU32`.
        unsafe { &*(context as *const AtomicU32) }.fetch_add(1, Ordering::AcqRel);
    }

    fn wait_for(counter: &AtomicU32, target: u32) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while counter.load(Ordering::Acquire) < target {
            assert!(std::time::Instant::now() < deadline, "work never ran");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    unsafe extern "C" fn note(context: *mut c_void) {
        ORDER
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(context as u64);
        // Long enough that a concurrent queue would interleave, so the ordering
        // assertion below is actually testing the serial guarantee.
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    #[test]
    fn async_f_runs_on_another_thread() {
        let counter = AtomicU32::new(0);
        let at = &counter as *const _ as u64;
        let q = create_serial("async test");
        // SAFETY: a live queue, a host function, and a counter that outlives the wait.
        unsafe {
            for _ in 0..16 {
                async_f(q, at, bump as *const () as usize as u64);
            }
        }
        wait_for(&counter, 16);
        // SAFETY: giving up the create reference.
        unsafe { object::release(q) };
    }

    /// The promise a serial queue exists for: submission order, one at a time, even
    /// though any of the pool's threads may pick it up.
    #[test]
    fn a_serial_queue_keeps_order() {
        ORDER.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let q = create_serial("order test");
        // SAFETY: a live queue and a host function.
        unsafe {
            for i in 1..=8u64 {
                async_f(q, i, note as *const () as usize as u64);
            }
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if ORDER.lock().unwrap_or_else(|e| e.into_inner()).len() == 8 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "queue never drained");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(
            *ORDER.lock().unwrap_or_else(|e| e.into_inner()),
            (1..=8u64).collect::<Vec<_>>()
        );
        // SAFETY: giving up the create reference.
        unsafe { object::release(q) };
    }

    /// `dispatch_sync` must not return before the work has run.
    #[test]
    fn sync_f_has_already_run_when_it_returns() {
        let counter = AtomicU32::new(0);
        let q = create_serial("sync test");
        // SAFETY: a live queue, a host function, and a live counter.
        unsafe {
            sync_f(
                q,
                &counter as *const _ as u64,
                bump as *const () as usize as u64,
            );
            assert_eq!(counter.load(Ordering::Acquire), 1);
            object::release(q);
        }
    }

    /// A `dispatch_sync` from inside the same serial queue is a deadlock on Darwin.
    /// Running it inline is the useful answer, and this is what proves it does not
    /// instead wait for a drain that cannot happen.
    #[test]
    fn sync_onto_the_current_queue_runs_inline() {
        static NESTED: AtomicU64 = AtomicU64::new(0);
        static QUEUE: AtomicU64 = AtomicU64::new(0);

        unsafe extern "C" fn outer(_context: *mut c_void) {
            let q = QUEUE.load(Ordering::Acquire);
            // SAFETY: the queue is live for the test's duration.
            unsafe { sync_f(q, 0, inner as *const () as usize as u64) };
        }
        unsafe extern "C" fn inner(_context: *mut c_void) {
            NESTED.store(1, Ordering::Release);
        }

        let q = create_serial("reentrant test");
        QUEUE.store(q, Ordering::Release);
        // SAFETY: a live queue and host functions.
        unsafe { async_f(q, 0, outer as *const () as usize as u64) };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while NESTED.load(Ordering::Acquire) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "a nested sync deadlocked"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // SAFETY: giving up the create reference.
        unsafe { object::release(q) };
    }

    /// The main queue must not be drained by the pool, only by the main thread.
    #[test]
    fn the_main_queue_waits_for_a_drain() {
        let counter = AtomicU32::new(0);
        note_main_thread();
        // SAFETY: the main queue is live for the process, and so is the counter.
        unsafe {
            async_f(
                main_queue(),
                &counter as *const _ as u64,
                bump as *const () as usize as u64,
            )
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(
            counter.load(Ordering::Acquire),
            0,
            "no worker may run main-queue work"
        );
        drain_main();
        assert_eq!(counter.load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_timer_runs_after_its_deadline_and_not_before() {
        let counter = AtomicU32::new(0);
        let q = create_serial("timer test");
        let deadline =
            unsafe { crate::dispatch::time::time(crate::dispatch::time::NOW, 60_000_000) };
        // SAFETY: a live queue, a host function, and a counter that outlives the wait.
        unsafe {
            after_f(
                deadline,
                q,
                &counter as *const _ as u64,
                bump as *const () as usize as u64,
            )
        };
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(counter.load(Ordering::Acquire), 0, "fired early");
        wait_for(&counter, 1);
        // SAFETY: giving up the create reference.
        unsafe { object::release(q) };
    }

    #[test]
    fn a_global_queue_is_the_same_object_each_time() {
        // SAFETY: no memory is touched.
        unsafe {
            assert_eq!(get_global_queue(0, 0), get_global_queue(0, 0));
            assert_ne!(get_global_queue(0, 0), main_queue());
        }
    }

    #[test]
    fn a_label_survives_the_round_trip() {
        let label = c"com.example.q";
        // SAFETY: a live C string, and the queue is released below.
        unsafe {
            let q = queue_create(label.as_ptr(), 0);
            assert_eq!(std::ffi::CStr::from_ptr(queue_get_label(q)), label);
            object::release(q);
        }
    }
}
