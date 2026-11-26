use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::block;
use crate::boxed;
use crate::define::{class_methods, methods};

/// `NSOperationQueueDefaultMaxConcurrentOperationCount`, which asks the queue to
/// decide for itself.
const DEFAULT_MAX_CONCURRENT: i64 = -1;

/// How many threads a queue that was left to decide will run at once.
const DEFAULT_WIDTH: usize = 4;

//

/// An operation's state, shared between the thread that queued it and the one
/// running it.
#[derive(Default)]
struct Operation {
    state: Mutex<Progress>,
    /// Signalled once `finished` is set, for `-waitUntilFinished`.
    done: Condvar,
    cancelled: AtomicBool,
}

#[derive(Default)]
struct Progress {
    /// Heap copies of `void (^)(void)`, run in order by `-main`. Empty for an
    /// operation whose work is an overridden `-main`.
    blocks: Vec<usize>,
    /// Work this crate queued itself, run after the blocks. A notification
    /// delivered to a queue is one: there is no block to copy, because the
    /// argument the handler wants is not one the caller passed.
    work: Option<Box<dyn FnOnce() + Send>>,
    completion: usize,
    executing: bool,
    finished: bool,
    /// Retained operations that must finish before this one is ready.
    dependencies: Vec<usize>,
    /// Queues holding an operation that depends on this one, told when it
    /// finishes so they look for work again.
    waiters: Vec<Weak<Queue>>,
    /// The queue this operation was added to, told when it is cancelled: a
    /// cancelled operation is ready whatever its dependencies say.
    queue: Option<Weak<Queue>>,
    /// `NSOperationQueuePriority`.
    priority: i64,
}

// SAFETY: the blocks are heap copies this module owns, and every runtime they
// reach is shared across threads already.
unsafe impl Send for Operation {}
unsafe impl Sync for Operation {}

impl Drop for Operation {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        for held in state.blocks.drain(..).chain([state.completion]) {
            objc::block::release(held as Id);
        }
        for dependency in state.dependencies.drain(..) {
            objc::arc::release(dependency as Id);
        }
    }
}

fn operation_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        let base = boxed::register::<Operation>(c"NSOperation", None);
        boxed::register::<Operation>(c"NSBlockOperation", Some(base));
        objc::class::ptr(base) as usize
    });
    // SAFETY: the cell holds a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

/// An operation's state, or `None` for anything that is not one.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
unsafe fn operation<'a>(object: Id) -> Option<&'a mut Operation> {
    unsafe { boxed::contents(object, operation_class()) }
}

/// A guest subclass of `NSOperation` reaches its storage through `[super init]`,
/// so the box is made here rather than at allocation.
unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an instance of a class registered for `Operation`.
    unsafe { boxed::fill(receiver, Operation::default()) };
    receiver
}

/// `-main`: run whatever blocks the operation carries.
///
/// A plain `NSOperation` has none, which is Apple's behaviour too - the class is
/// only useful subclassed, and the subclass overrides this.
unsafe extern "C" fn run_main(receiver: Id, _sel: Sel) {
    // SAFETY: a live object.
    let Some(operation) = (unsafe { operation(receiver) }) else {
        return;
    };
    let (blocks, work) = {
        let mut state = operation.state.lock().unwrap_or_else(|e| e.into_inner());
        (state.blocks.clone(), state.work.take())
    };
    for held in blocks {
        // SAFETY: an execution block is `void (^)(void)`.
        unsafe { block::call(held as Id) };
    }
    if let Some(work) = work {
        work();
    }
}

/// `-start`: the whole of an operation's life, as seen by whatever runs it.
///
/// `-main` is *sent* rather than called, because overriding it is how a subclass
/// says what the operation does.
unsafe extern "C" fn start(receiver: Id, _sel: Sel) {
    // SAFETY: a live object.
    let Some(operation) = (unsafe { operation(receiver) }) else {
        return;
    };
    {
        let mut state = operation.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.executing || state.finished {
            return;
        }
        // A cancelled operation still finishes: a queue and a
        // `-waitUntilFinished` are both waiting to be told it is over.
        state.executing = !operation.cancelled.load(Ordering::Acquire);
        if !state.executing {
            state.finished = true;
        }
    }
    if !operation.cancelled.load(Ordering::Acquire) {
        // SAFETY: `-main` is `void (id, SEL)`.
        unsafe { send_void(receiver, "main") };
    }
    let (completion, waiters) = {
        let mut state = operation.state.lock().unwrap_or_else(|e| e.into_inner());
        state.executing = false;
        state.finished = true;
        (state.completion, std::mem::take(&mut state.waiters))
    };
    operation.done.notify_all();
    for queue in waiters.iter().filter_map(Weak::upgrade) {
        look_for_work(&queue);
    }
    // SAFETY: a completion block is `void (^)(void)`.
    unsafe { block::call(completion as Id) };
}

/// Send a nullary `void` message, so an override is what runs.
///
/// # Safety
///
/// `receiver` must be null or a live object whose method for `name`, if it has
/// one, is `void (id, SEL)`.
unsafe fn send_void(receiver: Id, name: &str) {
    let sel = objc::sel::get(name);
    let Some(method) = (unsafe { objc::object::class_of(receiver) }).and_then(|c| c.method(sel))
    else {
        return;
    };
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(method.implementation());
        f(receiver, sel);
    }
}

unsafe extern "C" fn cancel(receiver: Id, _sel: Sel) {
    // SAFETY: a live object.
    let Some(operation) = (unsafe { operation(receiver) }) else {
        return;
    };
    operation.cancelled.store(true, Ordering::Release);
    let queue = operation
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .queue
        .clone();
    if let Some(queue) = queue.as_ref().and_then(Weak::upgrade) {
        look_for_work(&queue);
    }
}

unsafe extern "C" fn is_cancelled(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: a live object.
    let cancelled = unsafe { operation(receiver) }
        .is_some_and(|operation| operation.cancelled.load(Ordering::Acquire));
    cancelled as u8
}

unsafe extern "C" fn is_executing(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: a live object.
    unsafe { operation(receiver) }
        .is_some_and(|o| o.state.lock().unwrap_or_else(|e| e.into_inner()).executing) as u8
}

unsafe extern "C" fn is_finished(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: a live object.
    unsafe { operation(receiver) }
        .is_some_and(|o| o.state.lock().unwrap_or_else(|e| e.into_inner()).finished) as u8
}

/// Whether the operation `object` has finished, by its own state.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
unsafe fn has_finished(object: usize) -> bool {
    unsafe { operation(object as Id) }
        .is_none_or(|o| o.state.lock().unwrap_or_else(|e| e.into_inner()).finished)
}

/// Ready once every dependency has finished, or once it is cancelled: a
/// cancelled operation still has to start, to finish.
///
/// FIXME (ACCURACY): a dependency's own `finished` state is read rather than
/// its `-isFinished` sent, so a subclass that overrides `-isFinished` without
/// finishing through `-start` is not waited for correctly.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
unsafe fn ready(object: usize) -> bool {
    // SAFETY: the caller's guarantee.
    let Some(operation) = (unsafe { operation(object as Id) }) else {
        return true;
    };
    if operation.cancelled.load(Ordering::Acquire) {
        return true;
    }
    let dependencies = operation
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .dependencies
        .clone();
    // SAFETY: each is a retained operation.
    dependencies.iter().all(|d| unsafe { has_finished(*d) })
}

unsafe extern "C" fn is_ready(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: a live object.
    unsafe { ready(receiver as usize) as u8 }
}

unsafe extern "C" fn add_dependency(receiver: Id, _sel: Sel, dependency: Id) {
    // SAFETY: a live object.
    let Some(operation) = (unsafe { operation(receiver) }) else {
        return;
    };
    if dependency.is_null() {
        return;
    }
    let mut state = operation.state.lock().unwrap_or_else(|e| e.into_inner());
    if !state.dependencies.contains(&(dependency as usize)) {
        state
            .dependencies
            .push(objc::arc::retain(dependency) as usize);
    }
}

unsafe extern "C" fn remove_dependency(receiver: Id, _sel: Sel, dependency: Id) {
    // SAFETY: a live object.
    let Some(operation) = (unsafe { operation(receiver) }) else {
        return;
    };
    let removed = {
        let mut state = operation.state.lock().unwrap_or_else(|e| e.into_inner());
        let at = state
            .dependencies
            .iter()
            .position(|d| *d == dependency as usize);
        at.map(|at| (state.dependencies.remove(at), state.queue.clone()))
    };
    let Some((removed, queue)) = removed else {
        return;
    };
    objc::arc::release(removed as Id);
    if let Some(queue) = queue.as_ref().and_then(Weak::upgrade) {
        look_for_work(&queue);
    }
}

unsafe extern "C" fn dependencies(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a live object.
    let held: Vec<corefoundation::base::CFTypeRef> = unsafe { operation(receiver) }
        .map(|o| {
            let state = o.state.lock().unwrap_or_else(|e| e.into_inner());
            state.dependencies.iter().map(|d| *d as _).collect()
        })
        .unwrap_or_default();
    let array = corefoundation::collections::new_array(
        &held,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn queue_priority(receiver: Id, _sel: Sel) -> i64 {
    // SAFETY: a live object.
    unsafe { operation(receiver) }.map_or(0, |o| {
        o.state.lock().unwrap_or_else(|e| e.into_inner()).priority
    })
}

unsafe extern "C" fn set_queue_priority(receiver: Id, _sel: Sel, priority: i64) {
    // SAFETY: a live object.
    if let Some(operation) = unsafe { operation(receiver) } {
        operation
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .priority = priority;
    }
}

unsafe extern "C" fn is_concurrent(_receiver: Id, _sel: Sel) -> u8 {
    0
}

unsafe extern "C" fn wait_until_finished(receiver: Id, _sel: Sel) {
    // SAFETY: a live object.
    let Some(operation) = (unsafe { operation(receiver) }) else {
        return;
    };
    let mut state = operation.state.lock().unwrap_or_else(|e| e.into_inner());
    while !state.finished {
        state = operation
            .done
            .wait(state)
            .unwrap_or_else(|e| e.into_inner());
    }
}

unsafe extern "C" fn set_completion_block(receiver: Id, _sel: Sel, completion: Id) {
    // SAFETY: a live object.
    let Some(operation) = (unsafe { operation(receiver) }) else {
        return;
    };
    let copied = objc::block::copy(completion) as usize;
    let mut state = operation.state.lock().unwrap_or_else(|e| e.into_inner());
    objc::block::release(std::mem::replace(&mut state.completion, copied) as Id);
}

unsafe extern "C" fn completion_block(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a live object.
    let Some(operation) = (unsafe { operation(receiver) }) else {
        return std::ptr::null_mut();
    };
    operation
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .completion as Id
}

/// Add a `void (^)(void)` to what an operation will run, copied to the heap.
fn add_block(receiver: Id, execution: Id) {
    // SAFETY: a live object.
    let Some(operation) = (unsafe { operation(receiver) }) else {
        return;
    };
    let copied = objc::block::copy(execution) as usize;
    if copied == 0 {
        return;
    }
    operation
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .blocks
        .push(copied);
}

unsafe extern "C" fn add_execution_block(receiver: Id, _sel: Sel, execution: Id) {
    add_block(receiver, execution);
}

/// A `NSBlockOperation` around one block, autoreleased.
fn block_operation(execution: Id) -> Id {
    operation_class();
    let class = objc::class::lookup(c"NSBlockOperation").expect("registered by `operation_class`");
    let object = boxed::make(class, Operation::default());
    add_block(object, execution);
    objc::arc::autorelease(object)
}

unsafe extern "C" fn block_operation_with_block(_class: Id, _sel: Sel, execution: Id) -> Id {
    block_operation(execution)
}

/// An operation that runs `work` when it is started, owned by the caller.
///
/// For this crate's own use: a queue's interface is a block, and work that
/// starts on this side of the boundary has no block literal to hand it.
pub(crate) fn with_work(work: impl FnOnce() + Send + 'static) -> Id {
    operation_class();
    let class = objc::class::lookup(c"NSBlockOperation").expect("registered by `operation_class`");
    let object = boxed::make(class, Operation::default());
    // SAFETY: freshly made by this module.
    if let Some(operation) = unsafe { operation(object) } {
        operation
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .work = Some(Box::new(work));
    }
    object
}

/// Queue an operation, as `-addOperation:` does. For this crate's own use.
pub(crate) fn add_to_queue(queue: Id, operation: Id) {
    enqueue(queue, operation);
}

//

/// A queue's state, shared with the threads running its operations.
struct Queue {
    /// Whether this is the main queue, whose operations run on the run loop
    /// rather than on a thread of the queue's own.
    main: bool,
    pending: Mutex<Pending>,
    /// Signalled when an operation is added or the queue is resumed.
    work: Condvar,
    /// Signalled when an operation finishes, for `-waitUntilAllOperationsAreFinished`.
    idle: Condvar,
    /// The main queue's run loop source, kept so the loop holds a reference to
    /// something this queue can signal.
    source: OnceLock<Arc<corefoundation::runloop::Source>>,
    /// The object this is the state of, which `+currentQueue` hands back.
    object: OnceLock<usize>,
}

#[derive(Default)]
struct Pending {
    /// Retained operations, in the order they were added.
    operations: VecDeque<usize>,
    /// How many are running right now, which `max` limits.
    running: usize,
    /// Threads this queue has started, which never exceeds `width`.
    workers: usize,
    max: i64,
    suspended: bool,
    /// Set when the queue object is deallocated, so a worker waiting for work
    /// that can no longer come exits instead.
    shutdown: bool,
    name: String,
}

impl Pending {
    /// How many operations this queue may run at once.
    fn width(&self) -> usize {
        match self.max {
            n if n > 0 => n as usize,
            _ => DEFAULT_WIDTH,
        }
    }

    /// The next operation to run, if the queue is allowed to start one: the
    /// highest-priority ready one, the earliest added among equals.
    fn next(&mut self) -> Option<usize> {
        if self.suspended || self.running >= self.width() {
            return None;
        }
        let at = self
            .ready()
            .max_by_key(|&(at, operation)| {
                // SAFETY: a retained operation.
                (
                    unsafe { queue_priority(operation as Id, objc::sel::get("queuePriority")) },
                    std::cmp::Reverse(at),
                )
            })?
            .0;
        let operation = self.operations.remove(at)?;
        self.running += 1;
        Some(operation)
    }

    /// The queued operations that are ready, with where each is.
    fn ready(&self) -> impl Iterator<Item = (usize, usize)> {
        self.operations
            .iter()
            .copied()
            .enumerate()
            // SAFETY: each is a retained operation.
            .filter(|&(_, operation)| unsafe { ready(operation) })
    }
}

thread_local! {
    /// The queue object whose operation this thread is running, which
    /// `+currentQueue` answers. Unretained: `enqueue` holds a reference for as
    /// long as the operation is queued, so the object cannot go while this is
    /// set.
    static CURRENT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The queue object's own reference to its state.
///
/// A separate type because of the `Drop`: `-dealloc` releasing this is the only
/// moment a worker can be told that no more work is coming, and a worker that is
/// never told waits for the life of the process.
struct Handle(Arc<Queue>);

impl Drop for Handle {
    fn drop(&mut self) {
        self.0
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown = true;
        self.0.work.notify_all();
    }
}

fn queue_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        let class = boxed::register::<Handle>(c"NSOperationQueue", None);
        objc::class::ptr(class) as usize
    });
    // SAFETY: the cell holds a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

/// # Safety
///
/// `object` must be null or an object this runtime allocated.
unsafe fn queue(object: Id) -> Option<Arc<Queue>> {
    unsafe { boxed::contents::<Handle>(object, queue_class()) }.map(|handle| Arc::clone(&handle.0))
}

fn new_queue(main: bool) -> Arc<Queue> {
    Arc::new(Queue {
        main,
        pending: Mutex::new(Pending {
            max: DEFAULT_MAX_CONCURRENT,
            ..Pending::default()
        }),
        work: Condvar::new(),
        idle: Condvar::new(),
        source: OnceLock::new(),
        object: OnceLock::new(),
    })
}

/// Run one operation, with the queue on record as the current one.
fn run_one(state: &Arc<Queue>, operation: usize) {
    let object = state.object.get().copied().unwrap_or_default();
    let previous = CURRENT.with(|c| c.replace(object));
    // Apple gives an operation an autorelease pool of its own, and a `-main`
    // written for one autoreleases into it.
    let pool = objc::arc::pool_push();
    // SAFETY: a retained operation; `-start` is `void (id, SEL)`.
    unsafe { send_void(operation as Id, "start") };
    objc::arc::pool_pop(pool);
    objc::arc::release(operation as Id);
    CURRENT.with(|c| c.set(previous));
    // The reference `enqueue` took: a queue must outlive the operations it was
    // given, and nothing else here holds the object.
    objc::arc::release(object as Id);

    let mut pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
    pending.running -= 1;
    drop(pending);
    state.idle.notify_all();
    state.work.notify_one();
}

/// Everything the main queue can start this turn of the run loop.
///
/// One pass rather than a loop: an operation is entitled to add another, and
/// draining until empty would let the main thread never return to the loop.
unsafe extern "C" fn drain_main(info: *mut std::ffi::c_void) {
    // SAFETY: the `info` a main queue's source was made with is its own
    // `Arc<Queue>`, leaked for the life of the process.
    let state = unsafe { &*(info as *const Arc<Queue>) };
    let ready: Vec<usize> = {
        let mut pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
        std::iter::from_fn(|| pending.next()).collect()
    };
    for operation in ready {
        run_one(state, operation);
    }
    // Anything left over - the queue is at its width, or an operation added
    // one - is for the next turn, which has to be asked for.
    // One waiting on a dependency is not: the dependency's finishing asks.
    let pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
    if pending.ready().next().is_some() {
        signal_main(state);
    }
}

/// Have `state` start whatever it now can.
fn look_for_work(state: &Arc<Queue>) {
    match state.main {
        true => signal_main(state),
        false => {
            state.work.notify_all();
            start_worker(state);
        }
    }
}

fn signal_main(state: &Arc<Queue>) {
    let Some(source) = state.source.get() else {
        return;
    };
    source.signal();
    corefoundation::runloop::main().wake();
}

/// Start a worker for a queue that has work and room to run it.
fn start_worker(state: &Arc<Queue>) {
    {
        let mut pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending.workers >= pending.width() {
            state.work.notify_one();
            return;
        }
        pending.workers += 1;
    }
    let state = Arc::clone(state);
    std::thread::spawn(move || {
        // A worker runs guest operations, which read `errno`, thread-locals and
        // their allocator's thread identity off the thread pointer - and a new
        // thread's is its parent's until it enters. The dispatch pool does the same.
        let _darwin = darwin::tls::DarwinThread::enter();
        loop {
            let operation = {
                let mut pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if let Some(operation) = pending.next() {
                        break operation;
                    }
                    if pending.shutdown {
                        pending.workers -= 1;
                        return;
                    }
                    pending = state.work.wait(pending).unwrap_or_else(|e| e.into_inner());
                }
            };
            run_one(&state, operation);
        }
    });
}

/// Queue an operation, retained until it has run.
fn enqueue(receiver: Id, operation: Id) {
    if operation.is_null() {
        return;
    }
    // SAFETY: a live object.
    let Some(state) = (unsafe { queue(receiver) }) else {
        return;
    };
    objc::arc::retain(receiver);
    // SAFETY: a live object.
    if let Some(queued) = unsafe { self::operation(operation) } {
        let dependencies = {
            let mut own = queued.state.lock().unwrap_or_else(|e| e.into_inner());
            own.queue = Some(Arc::downgrade(&state));
            own.dependencies.clone()
        };
        for dependency in dependencies {
            // SAFETY: a retained operation.
            let Some(dependency) = (unsafe { self::operation(dependency as Id) }) else {
                continue;
            };
            let mut theirs = dependency.state.lock().unwrap_or_else(|e| e.into_inner());
            if !theirs.finished {
                theirs.waiters.push(Arc::downgrade(&state));
            }
        }
    }
    {
        let mut pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending
            .operations
            .push_back(objc::arc::retain(operation) as usize);
    }
    match state.main {
        true => signal_main(&state),
        false => start_worker(&state),
    }
}

unsafe extern "C" fn add_operation(receiver: Id, _sel: Sel, operation: Id) {
    enqueue(receiver, operation);
}

unsafe extern "C" fn add_operation_with_block(receiver: Id, _sel: Sel, execution: Id) {
    enqueue(receiver, block_operation(execution));
}

unsafe extern "C" fn add_operations_wait(receiver: Id, _sel: Sel, operations: Id, wait: u8) {
    // SAFETY: an array, or something that is not one - in which case there is
    // nothing to add.
    let members: Vec<Id> = match unsafe {
        corefoundation::collections::array(operations as corefoundation::base::CFTypeRef)
    } {
        Some(array) => array.values.iter().map(|v| *v as Id).collect(),
        None => return,
    };
    for operation in &members {
        enqueue(receiver, *operation);
    }
    if wait == 0 {
        return;
    }
    for operation in members {
        // SAFETY: a live operation; `-waitUntilFinished` is `void (id, SEL)`.
        unsafe { send_void(operation, "waitUntilFinished") };
    }
}

unsafe extern "C" fn operation_count(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: a live object.
    let Some(state) = (unsafe { queue(receiver) }) else {
        return 0;
    };
    let pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
    pending.operations.len() + pending.running
}

unsafe extern "C" fn operations(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a live object.
    let Some(state) = (unsafe { queue(receiver) }) else {
        return std::ptr::null_mut();
    };
    let queued: Vec<corefoundation::base::CFTypeRef> = state
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .operations
        .iter()
        .map(|o| *o as corefoundation::base::CFTypeRef)
        .collect();
    let array = corefoundation::collections::new_array(
        &queued,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn cancel_all_operations(receiver: Id, _sel: Sel) {
    // SAFETY: a live object.
    let Some(state) = (unsafe { queue(receiver) }) else {
        return;
    };
    let queued: Vec<usize> = state
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .operations
        .iter()
        .copied()
        .collect();
    for operation in queued {
        // SAFETY: a retained operation; `-cancel` is `void (id, SEL)`.
        unsafe { send_void(operation as Id, "cancel") };
    }
}

/// `-waitUntilAllOperationsAreFinished`.
///
/// On the main queue this has to *run the loop*: the operations it is waiting
/// for are serviced by the thread doing the waiting, so blocking on a condition
/// variable here is a deadlock.
unsafe extern "C" fn wait_until_all_finished(receiver: Id, _sel: Sel) {
    // SAFETY: a live object.
    let Some(state) = (unsafe { queue(receiver) }) else {
        return;
    };
    if state.main {
        loop {
            {
                let pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
                if pending.operations.is_empty() && pending.running == 0 {
                    return;
                }
            }
            corefoundation::runloop::main().run_in_mode(
                corefoundation::runloop::DEFAULT_MODE,
                0.01,
                true,
            );
        }
    }
    let mut pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
    while !pending.operations.is_empty() || pending.running != 0 {
        pending = state.idle.wait(pending).unwrap_or_else(|e| e.into_inner());
    }
}

unsafe extern "C" fn max_concurrent(receiver: Id, _sel: Sel) -> i64 {
    // SAFETY: a live object.
    let Some(state) = (unsafe { queue(receiver) }) else {
        return DEFAULT_MAX_CONCURRENT;
    };
    let max = state.pending.lock().unwrap_or_else(|e| e.into_inner()).max;
    max
}

unsafe extern "C" fn set_max_concurrent(receiver: Id, _sel: Sel, max: i64) {
    // SAFETY: a live object.
    let Some(state) = (unsafe { queue(receiver) }) else {
        return;
    };
    state.pending.lock().unwrap_or_else(|e| e.into_inner()).max = max;
    if !state.main {
        start_worker(&state);
    }
}

unsafe extern "C" fn is_suspended(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: a live object.
    let Some(state) = (unsafe { queue(receiver) }) else {
        return 0;
    };
    let suspended = state
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .suspended;
    suspended as u8
}

unsafe extern "C" fn set_suspended(receiver: Id, _sel: Sel, suspended: u8) {
    // SAFETY: a live object.
    let Some(state) = (unsafe { queue(receiver) }) else {
        return;
    };
    state
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .suspended = suspended != 0;
    if suspended != 0 {
        return;
    }
    match state.main {
        true => signal_main(&state),
        false => start_worker(&state),
    }
}

unsafe extern "C" fn name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a live object.
    let Some(state) = (unsafe { queue(receiver) }) else {
        return std::ptr::null_mut();
    };
    let name = state
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .name
        .clone();
    objc::arc::autorelease(crate::string::from_str(&name))
}

unsafe extern "C" fn set_name(receiver: Id, _sel: Sel, value: Id) {
    // SAFETY: a live object.
    let Some(state) = (unsafe { queue(receiver) }) else {
        return;
    };
    // SAFETY: a live object, or nothing - in which case the name is empty.
    let bytes = unsafe { crate::string::bytes(value) };
    state.pending.lock().unwrap_or_else(|e| e.into_inner()).name =
        String::from_utf8_lossy(&bytes).into_owned();
}

unsafe extern "C" fn queue_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an instance of a class registered for `Arc<Queue>`.
    unsafe { boxed::fill(receiver, Handle(new_queue(false))) };
    if let Some(state) = unsafe { queue(receiver) } {
        state.object.set(receiver as usize).ok();
    }
    receiver
}

/// The one main queue, whose operations the main run loop runs.
unsafe extern "C" fn main_queue(_class: Id, _sel: Sel) -> Id {
    static QUEUE: OnceLock<usize> = OnceLock::new();
    *QUEUE.get_or_init(|| {
        let state = new_queue(true);
        let object = boxed::make(queue_class(), Handle(Arc::clone(&state)));
        objc::arc::retain(object);
        state.object.set(object as usize).ok();
        // The source's `info` is the queue's own `Arc`, leaked: the main queue
        // is never released, and a source outliving the state it points at
        // would be a callback into freed memory on the run loop's next turn.
        let info = Box::into_raw(Box::new(Arc::clone(&state)));
        let source = corefoundation::runloop::new_source(Some(drain_main), info as usize);
        corefoundation::runloop::main().add_source(&source, corefoundation::runloop::DEFAULT_MODE);
        state.source.set(source).ok();
        object as usize
    }) as Id
}

/// `+currentQueue`: the queue whose operation this thread is running, or nil on
/// a thread that is not running one - which is what Apple answers too.
unsafe extern "C" fn current_queue(_class: Id, _sel: Sel) -> Id {
    CURRENT.with(|c| c.get()) as Id
}

pub(crate) fn install() {
    let operation = operation_class();
    methods!(operation, {
        "init" => init, c"@16@0:8";
        "start" => start, c"v16@0:8";
        "main" => run_main, c"v16@0:8";
        "cancel" => cancel, c"v16@0:8";
        "isCancelled" => is_cancelled, c"B16@0:8";
        "isExecuting" => is_executing, c"B16@0:8";
        "isFinished" => is_finished, c"B16@0:8";
        "isReady" => is_ready, c"B16@0:8";
        "addDependency:" => add_dependency, c"v24@0:8@16";
        "removeDependency:" => remove_dependency, c"v24@0:8@16";
        "dependencies" => dependencies, c"@16@0:8";
        "queuePriority" => queue_priority, c"q16@0:8";
        "setQueuePriority:" => set_queue_priority, c"v24@0:8q16";
        "isConcurrent" => is_concurrent, c"B16@0:8";
        "isAsynchronous" => is_concurrent, c"B16@0:8";
        "waitUntilFinished" => wait_until_finished, c"v16@0:8";
        "completionBlock" => completion_block, c"@?16@0:8";
        "setCompletionBlock:" => set_completion_block, c"v24@0:8@?16";
    });

    let block_class =
        objc::class::lookup(c"NSBlockOperation").expect("registered by `operation_class`");
    class_methods!(block_class, {
        "blockOperationWithBlock:" => block_operation_with_block, c"@24@0:8@?16";
    });
    methods!(block_class, {
        "addExecutionBlock:" => add_execution_block, c"v24@0:8@?16";
    });

    let queue = queue_class();
    class_methods!(queue, {
        "mainQueue" => main_queue, c"@16@0:8";
        "currentQueue" => current_queue, c"@16@0:8";
    });
    methods!(queue, {
        "init" => queue_init, c"@16@0:8";
        "addOperation:" => add_operation, c"v24@0:8@16";
        "addOperationWithBlock:" => add_operation_with_block, c"v24@0:8@?16";
        "addOperations:waitUntilFinished:" => add_operations_wait, c"v32@0:8@16B24";
        "operations" => operations, c"@16@0:8";
        "operationCount" => operation_count, c"Q16@0:8";
        "cancelAllOperations" => cancel_all_operations, c"v16@0:8";
        "waitUntilAllOperationsAreFinished" => wait_until_all_finished, c"v16@0:8";
        "maxConcurrentOperationCount" => max_concurrent, c"q16@0:8";
        "setMaxConcurrentOperationCount:" => set_max_concurrent, c"v24@0:8q16";
        "isSuspended" => is_suspended, c"B16@0:8";
        "setSuspended:" => set_suspended, c"v20@0:8B16";
        "name" => name, c"@16@0:8";
        "setName:" => set_name, c"v24@0:8@16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    use crate::block::fake;

    fn send0(receiver: Id, name: &str) -> Id {
        let sel = objc::sel::get(name);
        // SAFETY: a live object, and each selector below returns an object.
        unsafe {
            let method = objc::object::class_of(receiver)
                .and_then(|c| c.method(sel))
                .unwrap_or_else(|| panic!("no method for {name}"));
            let f: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(method.implementation());
            f(receiver, sel)
        }
    }

    fn new_object(name: &std::ffi::CStr) -> Id {
        crate::bootstrap();
        let class = objc::class::lookup(name).expect("registered by this module");
        send0(objc::class::ptr(class) as Id, "new")
    }

    /// Wait for a counter to reach `wanted`, or give up.
    fn until(counter: &AtomicUsize, wanted: usize) -> bool {
        for _ in 0..500 {
            if counter.load(Ordering::SeqCst) >= wanted {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    static BACKGROUND: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn count_background(_block: Id) {
        BACKGROUND.fetch_add(1, Ordering::SeqCst);
    }

    /// The whole of a background queue: a block added to one runs, and the
    /// queue can be waited on until it has.
    #[test]
    fn a_queued_block_runs_and_the_queue_can_be_waited_on() {
        let queue = new_object(c"NSOperationQueue");
        BACKGROUND.store(0, Ordering::SeqCst);
        let block = fake::block(count_background as *const std::ffi::c_void);
        let sel = objc::sel::get("addOperationWithBlock:");
        // SAFETY: the prototype is the method's, and the block is a valid
        // literal whose `invoke` takes only itself.
        unsafe {
            for _ in 0..3 {
                add_operation_with_block(queue, sel, block.id());
            }
            wait_until_all_finished(queue, objc::sel::get("waitUntilAllOperationsAreFinished"));
        }
        assert_eq!(BACKGROUND.load(Ordering::SeqCst), 3);
        // SAFETY: `-operationCount` is `NSUInteger (id, SEL)`.
        assert_eq!(
            unsafe { operation_count(queue, objc::sel::get("operationCount")) },
            0
        );
        objc::arc::release(queue);
    }

    static SERIAL: AtomicUsize = AtomicUsize::new(0);
    static OVERLAPPED: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn one_at_a_time(_block: Id) {
        if SERIAL.fetch_add(1, Ordering::SeqCst) != 0 {
            OVERLAPPED.fetch_add(1, Ordering::SeqCst);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        SERIAL.fetch_sub(1, Ordering::SeqCst);
    }

    /// A queue told to run one at a time does, which is the whole of what a
    /// program asking for a serial queue is relying on.
    #[test]
    fn a_serial_queue_runs_one_operation_at_a_time() {
        let queue = new_object(c"NSOperationQueue");
        SERIAL.store(0, Ordering::SeqCst);
        OVERLAPPED.store(0, Ordering::SeqCst);
        let block = fake::block(one_at_a_time as *const std::ffi::c_void);
        // SAFETY: each prototype is its method's.
        unsafe {
            set_max_concurrent(queue, objc::sel::get("setMaxConcurrentOperationCount:"), 1);
            for _ in 0..6 {
                add_operation_with_block(
                    queue,
                    objc::sel::get("addOperationWithBlock:"),
                    block.id(),
                );
            }
            wait_until_all_finished(queue, objc::sel::get("waitUntilAllOperationsAreFinished"));
        }
        assert_eq!(OVERLAPPED.load(Ordering::SeqCst), 0, "two ran at once");
        objc::arc::release(queue);
    }

    /// The order the two operations below ran in, as their tags.
    static ORDER: Mutex<Vec<u8>> = Mutex::new(Vec::new());

    unsafe extern "C" fn first(_block: Id) {
        ORDER.lock().unwrap_or_else(|e| e.into_inner()).push(1);
    }

    unsafe extern "C" fn second(_block: Id) {
        ORDER.lock().unwrap_or_else(|e| e.into_inner()).push(2);
    }

    /// An operation waits for its dependency, even when it was queued first
    /// on a queue that could run both at once.
    #[test]
    fn an_operation_runs_after_its_dependency() {
        let queue = new_object(c"NSOperationQueue");
        ORDER.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let (a, b) = (
            fake::block(first as *const std::ffi::c_void),
            fake::block(second as *const std::ffi::c_void),
        );
        let (earlier, later) = (block_operation(a.id()), block_operation(b.id()));
        // SAFETY: each prototype is its method's.
        unsafe {
            add_dependency(later, objc::sel::get("addDependency:"), earlier);
            assert_eq!(is_ready(later, objc::sel::get("isReady")), 0);
            add_operation(queue, objc::sel::get("addOperation:"), later);
            add_operation(queue, objc::sel::get("addOperation:"), earlier);
            wait_until_all_finished(queue, objc::sel::get("waitUntilAllOperationsAreFinished"));
            assert_eq!(is_ready(later, objc::sel::get("isReady")), 1);
        }
        assert_eq!(*ORDER.lock().unwrap_or_else(|e| e.into_inner()), [1, 2]);
        objc::arc::release(queue);
    }

    static MAIN: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn count_main(_block: Id) {
        MAIN.fetch_add(1, Ordering::SeqCst);
    }

    /// The main queue runs its operations from the run loop, not before it -
    /// which is the ordering a program that touches the UI from a worker is
    /// depending on.
    #[test]
    fn the_main_queue_runs_its_operations_when_the_loop_is_turned() {
        crate::bootstrap();
        // SAFETY: `+mainQueue` is `id (Class, SEL)`.
        let queue = unsafe { main_queue(std::ptr::null_mut(), objc::sel::get("mainQueue")) };
        assert!(!queue.is_null());
        MAIN.store(0, Ordering::SeqCst);
        let block = fake::block(count_main as *const std::ffi::c_void);
        // SAFETY: the prototype is the method's.
        unsafe {
            add_operation_with_block(queue, objc::sel::get("addOperationWithBlock:"), block.id());
        }
        for _ in 0..200 {
            if MAIN.load(Ordering::SeqCst) == 1 {
                break;
            }
            corefoundation::runloop::main().run_in_mode(
                corefoundation::runloop::DEFAULT_MODE,
                0.01,
                true,
            );
        }
        assert_eq!(
            MAIN.load(Ordering::SeqCst),
            1,
            "the main queue did not run its operation"
        );
    }

    static CANCELLED: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn count_cancelled(_block: Id) {
        CANCELLED.fetch_add(1, Ordering::SeqCst);
    }

    /// An operation cancelled before it starts does not run, but does finish:
    /// a queue and a `-waitUntilFinished` are both waiting to be told so.
    #[test]
    fn a_cancelled_operation_finishes_without_running() {
        crate::bootstrap();
        CANCELLED.store(0, Ordering::SeqCst);
        let block = fake::block(count_cancelled as *const std::ffi::c_void);
        // SAFETY: each prototype is its method's.
        unsafe {
            let operation = block_operation_with_block(
                std::ptr::null_mut(),
                objc::sel::get("blockOperationWithBlock:"),
                block.id(),
            );
            cancel(operation, objc::sel::get("cancel"));
            start(operation, objc::sel::get("start"));
            assert_eq!(is_cancelled(operation, objc::sel::get("isCancelled")), 1);
            assert_eq!(is_finished(operation, objc::sel::get("isFinished")), 1);
        }
        assert_eq!(CANCELLED.load(Ordering::SeqCst), 0, "a cancelled block ran");
    }

    /// `+currentQueue` names the queue whose operation is running, and nothing
    /// on a thread that is running none.
    #[test]
    fn the_current_queue_is_the_one_running_the_operation() {
        crate::bootstrap();
        // SAFETY: `+currentQueue` is `id (Class, SEL)`.
        assert!(
            unsafe { current_queue(std::ptr::null_mut(), objc::sel::get("currentQueue")) }
                .is_null()
        );

        static SEEN: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn record(_block: Id) {
            let queue =
                unsafe { current_queue(std::ptr::null_mut(), objc::sel::get("currentQueue")) };
            SEEN.store(queue as usize, Ordering::SeqCst);
        }

        let queue = new_object(c"NSOperationQueue");
        SEEN.store(0, Ordering::SeqCst);
        let block = fake::block(record as *const std::ffi::c_void);
        // SAFETY: the prototype is the method's.
        unsafe {
            add_operation_with_block(queue, objc::sel::get("addOperationWithBlock:"), block.id());
        }
        assert!(until(&SEEN, 1), "the operation never ran");
        assert_eq!(SEEN.load(Ordering::SeqCst), queue as usize);
        objc::arc::release(queue);
    }
}
