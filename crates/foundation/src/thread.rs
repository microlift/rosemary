//! `NSThread`, and the `NSObject` methods that hand a message to a thread.

use std::ffi::{CString, c_void};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use corefoundation::runloop::{self, RunLoop, Source};
use darwin::tls::key::Key;
use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::boxed;
use crate::define::{class_methods, methods};

pub const WILL_BECOME_MULTI_THREADED: &str = "NSWillBecomeMultiThreadedNotification";
/// Declared by Apple and never posted, there or here.
pub const DID_BECOME_SINGLE_THREADED: &str = "NSDidBecomeSingleThreadedNotification";
pub const WILL_EXIT: &str = "NSThreadWillExitNotification";

/// `NSQualityOfServiceDefault`. The other `NSQualityOfService` values are the
/// `qos_class_t`s they stand for.
const QOS_DEFAULT: i64 = -1;
const QOS_USER_INTERACTIVE: i64 = darwin::pthread::QOS_CLASS_USER_INTERACTIVE as i64;

/// What `+threadPriority` answers for a thread nobody has reprioritised.
const DEFAULT_PRIORITY: f64 = 0.5;

const MAIN_NAME: &str = "main";
/// The `number` in a thread's description; the main thread's is always 1.
const MAIN_NUMBER: usize = 1;

/// Set when the first `NSThread` starts. A program that only ever made threads
/// with `pthread_create` is not multithreaded by this definition, on Apple too.
static MULTITHREADED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    Created,
    Executing,
    Finished,
}

/// What `-main` sends. Owned until the thread ends, as Apple documents.
enum Work {
    None,
    Message {
        target: usize,
        selector: usize,
        argument: usize,
    },
    /// A heap copy of a `void (^)(void)`.
    Block(usize),
}

impl Work {
    fn release(self) {
        match self {
            Work::None => {}
            Work::Message {
                target, argument, ..
            } => {
                objc::arc::release(argument as Id);
                objc::arc::release(target as Id);
            }
            Work::Block(block) => objc::block::release(block as Id),
        }
    }
}

/// A message queued by `-performSelector:onThread:...`, with its target and
/// argument retained.
struct Perform {
    target: usize,
    selector: usize,
    argument: usize,
    /// As requested, the common-modes alias included.
    modes: Vec<String>,
    done: Option<Arc<Done>>,
}

impl Perform {
    fn release(self) {
        objc::arc::release(self.argument as Id);
        objc::arc::release(self.target as Id);
    }
}

#[derive(Default)]
struct Done {
    performed: Mutex<bool>,
    signal: Condvar,
}

impl Done {
    fn finish(&self) {
        *lock(&self.performed) = true;
        self.signal.notify_all();
    }

    fn wait(&self) {
        let mut performed = lock(&self.performed);
        while !*performed {
            performed = self
                .signal
                .wait(performed)
                .unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// The thread's queue of performs, and the one run loop source that delivers
/// them: one per thread rather than per message, since a source, once made, is
/// never forgotten by `corefoundation`.
#[derive(Default)]
struct Performs {
    pending: Vec<Perform>,
    source: Option<Arc<Source>>,
    /// The modes, as requested, the source has been added to.
    modes: Vec<String>,
}

struct Thread {
    number: usize,
    status: Mutex<Status>,
    /// Whether `-start` made the thread, which then owns a reference to its own
    /// object and an autorelease pool until it ends.
    launched: AtomicBool,
    pool: AtomicUsize,
    cancelled: AtomicBool,
    work: Mutex<Work>,
    /// A retained `NSString`, or null.
    name: Mutex<usize>,
    /// Zero until `-setStackSize:` is given a size Darwin accepts.
    stack_size: AtomicUsize,
    /// An `f64`'s bits.
    priority: AtomicU64,
    quality_of_service: AtomicI64,
    /// A retained `NSMutableDictionary`, or null until asked for.
    dictionary: Mutex<usize>,
    /// The loop of the thread this object stands for, known once it runs.
    run_loop: OnceLock<Arc<RunLoop>>,
    performs: Mutex<Performs>,
}

impl Thread {
    fn new(number: usize, status: Status, name: Id, quality_of_service: i64) -> Thread {
        Thread {
            number,
            status: Mutex::new(status),
            launched: AtomicBool::new(false),
            pool: AtomicUsize::new(0),
            cancelled: AtomicBool::new(false),
            work: Mutex::new(Work::None),
            name: Mutex::new(name as usize),
            stack_size: AtomicUsize::new(0),
            priority: AtomicU64::new(DEFAULT_PRIORITY.to_bits()),
            quality_of_service: AtomicI64::new(quality_of_service),
            dictionary: Mutex::new(0),
            run_loop: OnceLock::new(),
            performs: Mutex::new(Performs::default()),
        }
    }

    /// A thread object made by `-init`.
    fn unstarted() -> Thread {
        Thread::new(
            next_number(),
            Status::Created,
            std::ptr::null_mut(),
            QOS_DEFAULT,
        )
    }

    fn status(&self) -> Status {
        *lock(&self.status)
    }

    fn set_work(&self, work: Work) {
        std::mem::replace(&mut *lock(&self.work), work).release();
    }

    /// Record the loop of the thread this object now stands for, and hand it
    /// whatever was queued before it was known.
    fn attach(&self, object: Id, run_loop: Arc<RunLoop>) {
        let _ = self.run_loop.set(run_loop);
        schedule(object, self);
    }
}

impl Drop for Thread {
    fn drop(&mut self) {
        let performs = std::mem::take(self.performs.get_mut().unwrap_or_else(|e| e.into_inner()));
        if let Some(source) = &performs.source {
            source.invalidate();
            if let Some(run_loop) = self.run_loop.get() {
                for mode in &performs.modes {
                    run_loop.remove_source(source, mode);
                }
            }
        }
        // A waiter on one of these is left waiting, as on Apple: the thread
        // that would have run it is gone.
        for perform in performs.pending {
            perform.release();
        }
        std::mem::replace(
            self.work.get_mut().unwrap_or_else(|e| e.into_inner()),
            Work::None,
        )
        .release();
        for object in [&mut self.name, &mut self.dictionary] {
            objc::arc::release(*object.get_mut().unwrap_or_else(|e| e.into_inner()) as Id);
        }
    }
}

/// Thread objects are numbered in creation order, after the main thread's.
fn next_number() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(MAIN_NUMBER + 1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn thread_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let ptr = *CLASS
        .get_or_init(|| objc::class::ptr(boxed::register::<Thread>(c"NSThread", None)) as usize);
    // SAFETY: the cell holds a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

/// A thread object's state, or `None` for anything that is not one - a guest
/// subclass whose `-init` never reached `NSThread`'s included.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
unsafe fn state<'a>(object: Id) -> Option<&'a Thread> {
    unsafe { boxed::contents::<Thread>(object, thread_class()) }.map(|thread| &*thread)
}

/// `objc_msgSend`, as a function of prototype `F`.
///
/// # Safety
///
/// `F` must be the `unsafe extern "C" fn` type of every method it is used to
/// send.
unsafe fn msg_send<F: Copy>() -> F {
    const { assert!(size_of::<F>() == size_of::<*const c_void>()) };
    // SAFETY: a code address, reinterpreted as the caller's prototype.
    unsafe { std::mem::transmute_copy(&objc::msg::entry()) }
}

/// The Darwin thread-specific data key holding each thread's object, with a
/// reference of its own.
///
/// Darwin's key rather than a Rust thread-local, as Apple's is a pthread key:
/// its destructor runs in Darwin's order with the other keys', and guest code
/// in a destructor that asks for the current thread still gets an answer.
fn key() -> Key {
    static KEY: OnceLock<u32> = OnceLock::new();
    Key::from_index(*KEY.get_or_init(|| match Key::create(Some(thread_ended)) {
        Ok(key) => key.index(),
        Err(error) => objc::fail!("NSThread: no thread-specific data key is left ({error})"),
    }))
}

/// The key's destructor: the thread is gone, and so is its reference.
unsafe extern "C" fn thread_ended(object: *mut c_void) {
    end(object as Id, false);
    objc::arc::release(object as Id);
}

/// The calling thread's object, made the first time it is asked for.
pub fn current() -> Id {
    let key = key();
    let found = key.get() as Id;
    if !found.is_null() {
        return found;
    }
    // SAFETY: asks about the calling thread.
    let object = match unsafe { darwin::pthread::main_np() } != 0 {
        true => objc::arc::retain(main()),
        false => boxed::make(
            thread_class(),
            Thread::new(
                next_number(),
                Status::Executing,
                std::ptr::null_mut(),
                QOS_DEFAULT,
            ),
        ),
    };
    key.set(object as *mut c_void);
    // SAFETY: a thread object this module made.
    if let Some(thread) = unsafe { state(object) } {
        thread.attach(object, runloop::current());
    }
    object
}

/// The main thread's object, which any thread may ask for.
pub fn main() -> Id {
    static MAIN: OnceLock<usize> = OnceLock::new();
    *MAIN.get_or_init(|| {
        let object = boxed::make(
            thread_class(),
            Thread::new(
                MAIN_NUMBER,
                Status::Executing,
                crate::string::from_str(MAIN_NAME),
                QOS_USER_INTERACTIVE,
            ),
        );
        // SAFETY: just made.
        if let Some(thread) = unsafe { state(object) } {
            thread.attach(object, runloop::main());
        }
        object as usize
    }) as Id
}

/// Post `NSWillBecomeMultiThreadedNotification`, once, before the first thread
/// starts.
///
/// The flag is set before posting, so an observer that starts a thread of its
/// own does not post again; another thread starting one meanwhile waits for the
/// observers to finish.
fn become_multithreaded() {
    static BECOMING: Mutex<()> = Mutex::new(());
    if MULTITHREADED.load(Ordering::Acquire) {
        return;
    }
    let _becoming = lock(&BECOMING);
    if MULTITHREADED.swap(true, Ordering::AcqRel) {
        return;
    }
    let pool = objc::arc::pool_push();
    crate::notification::post(
        WILL_BECOME_MULTI_THREADED,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
    objc::arc::pool_pop(pool);
}

/// Everything that happens as the calling thread stops being `object`'s:
/// `NSThreadWillExitNotification` if `notify`, then the thread is finished and
/// lets go of its work, its pool and its references to itself.
///
/// Called on the thread itself.
fn end(object: Id, notify: bool) {
    // SAFETY: a thread object this module handed out.
    let Some(thread) = (unsafe { state(object) }) else {
        return;
    };
    if notify {
        let pool = objc::arc::pool_push();
        crate::notification::post(WILL_EXIT, object, std::ptr::null_mut());
        objc::arc::pool_pop(pool);
    }
    // After the notification: an observer asking `-isExecuting` hears YES, as
    // on macOS.
    *lock(&thread.status) = Status::Finished;
    let launched = thread.launched.swap(false, Ordering::AcqRel);
    if launched {
        let pool = thread.pool.swap(0, Ordering::AcqRel);
        if pool != 0 {
            objc::arc::pool_pop(pool as *mut c_void);
        }
    }
    thread.set_work(Work::None);
    // Last, because either release may be the object's last.
    let key = key();
    if key.get() as Id == object {
        key.set(std::ptr::null_mut());
        objc::arc::release(object);
    }
    if launched {
        objc::arc::release(object);
    }
}

/// `-start`, and `+detachNewThread...` after it.
fn start(object: Id) {
    // SAFETY: the receiver of a message, so null or a live object.
    let Some(thread) = (unsafe { state(object) }) else {
        return;
    };
    {
        let mut status = lock(&thread.status);
        if *status != Status::Created {
            objc::fail!("*** -[NSThread start]: attempt to start the thread again");
        }
        if thread.cancelled.load(Ordering::Acquire) {
            *status = Status::Finished;
            return;
        }
        *status = Status::Executing;
    }
    become_multithreaded();
    thread.launched.store(true, Ordering::Release);
    let requested = match thread.stack_size.load(Ordering::Acquire) {
        0 => None,
        size => Some(size),
    };
    // The new thread's reference, given up in `end`.
    objc::arc::retain(object);
    if let Err(error) = darwin::pthread::spawn_detached(
        darwin::pthread::host_stack_size(requested),
        launch,
        object as *mut c_void,
    ) {
        objc::fail!("*** -[NSThread start]: the thread could not be created ({error})");
    }
}

/// A thread's result, which nothing joins to read. A constant, not a call to
/// `ptr::null_mut`: see [`launch`].
const NO_RESULT: *mut c_void = std::ptr::null_mut();

#[repr(C)]
struct Entry {
    send: unsafe extern "C" fn(Id, Sel),
    main: Sel,
}

/// The new thread's start routine.
///
/// Every call here is to an `extern "C"` function kept out of line, and nothing
/// is dropped: `+exit` and `pthread_exit` end the thread by a forced unwind
/// through this frame, which aborts if the frame has an unwind table - and any
/// call that may unwind, a Rust function's or an inlined one's, would give it
/// one.
extern "C" fn launch(context: *mut c_void) -> *mut c_void {
    let object = context as Id;
    // SAFETY: `start`'s reference, which this thread now owns.
    let entry = unsafe { begin(object) };
    // SAFETY: `-main` is `void (id, SEL)`.
    unsafe { (entry.send)(object, entry.main) };
    // SAFETY: as above.
    unsafe { finish(object) };
    NO_RESULT
}

/// Make the calling thread `object`'s, ready to run guest code.
///
/// # Safety
///
/// Called once, first, on the thread `start` made for `object`.
#[inline(never)]
unsafe extern "C" fn begin(object: Id) -> Entry {
    darwin::pthread::publish_self();
    darwin::mach::service::prepare_thread();
    // Never left: the thread runs guest code until it ends, and may end under
    // a frame that would have left it.
    std::mem::forget(darwin::tls::DarwinThread::enter());
    key().set(objc::arc::retain(object) as *mut c_void);
    // SAFETY: `start` accepted this object.
    if let Some(thread) = unsafe { state(object) } {
        thread.attach(object, runloop::current());
        let name = objc::arc::retain(*lock(&thread.name) as Id);
        apply_name(name);
        objc::arc::release(name);
        let quality_of_service = thread.quality_of_service.load(Ordering::Acquire);
        if quality_of_service != QOS_DEFAULT {
            // SAFETY: a class Darwin rejects is refused, not applied.
            unsafe { darwin::pthread::set_qos_class_self_np(quality_of_service as u32, 0) };
        }
        thread
            .pool
            .store(objc::arc::pool_push() as usize, Ordering::Release);
    }
    Entry {
        // SAFETY: `Entry::send` is the prototype `-main` is sent with.
        send: unsafe { msg_send() },
        main: objc::sel::get("main"),
    }
}

/// `-main` returned: the thread exits as `+exit` would have.
///
/// # Safety
///
/// As [`begin`], after it.
#[inline(never)]
unsafe extern "C" fn finish(object: Id) {
    end(object, true);
}

/// Give the calling thread `name`, so a debugger and `ps` see it.
fn apply_name(name: Id) {
    if name.is_null() {
        return;
    }
    // SAFETY: a string, or a guest's object that `-copy` gave.
    let mut bytes = unsafe { crate::string::bytes(name) };
    // A C string ends at the first NUL; `CString::new` would refuse it.
    if let Some(end) = bytes.iter().position(|&byte| byte == 0) {
        bytes.truncate(end);
    }
    let Ok(name) = CString::new(bytes) else {
        return;
    };
    // SAFETY: a NUL-terminated name that outlives the call.
    unsafe { darwin::pthread::setname_np(name.as_ptr() as u64) };
}

fn sleep_for(seconds: f64) {
    if seconds > 0.0 {
        std::thread::sleep(Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX));
    }
}

/// An unstarted thread object that will send `selector` to `target`.
fn with_target(receiver: Id, target: Id, selector: Sel, argument: Id) -> Id {
    // SAFETY: `-init` is `id (id, SEL)`, sent so a subclass's override runs.
    let receiver = unsafe {
        msg_send::<unsafe extern "C" fn(Id, Sel) -> Id>()(receiver, objc::sel::get("init"))
    };
    // SAFETY: what `-init` returned.
    if let Some(thread) = unsafe { state(receiver) } {
        thread.set_work(Work::Message {
            target: objc::arc::retain(target) as usize,
            selector: selector as usize,
            argument: objc::arc::retain(argument) as usize,
        });
    }
    receiver
}

/// As [`with_target`], running a copy of `block`.
fn with_block(receiver: Id, block: Id) -> Id {
    // SAFETY: as in `with_target`.
    let receiver = unsafe {
        msg_send::<unsafe extern "C" fn(Id, Sel) -> Id>()(receiver, objc::sel::get("init"))
    };
    // SAFETY: as in `with_target`.
    if let Some(thread) = unsafe { state(receiver) } {
        thread.set_work(Work::Block(objc::block::copy(block) as usize));
    }
    receiver
}

fn new_thread() -> Id {
    boxed::make(thread_class(), Thread::unstarted())
}

// Class methods.

unsafe extern "C" fn current_thread(_class: Id, _sel: Sel) -> Id {
    current()
}

unsafe extern "C" fn main_thread(_class: Id, _sel: Sel) -> Id {
    main()
}

unsafe extern "C" fn class_is_main_thread(_class: Id, _sel: Sel) -> bool {
    // SAFETY: asks about the calling thread.
    unsafe { darwin::pthread::main_np() != 0 }
}

unsafe extern "C" fn is_multi_threaded(_class: Id, _sel: Sel) -> bool {
    MULTITHREADED.load(Ordering::Acquire)
}

unsafe extern "C" fn detach_selector(
    _class: Id,
    _sel: Sel,
    selector: Sel,
    target: Id,
    argument: Id,
) {
    let thread = with_target(new_thread(), target, selector, argument);
    start(thread);
    objc::arc::release(thread);
}

unsafe extern "C" fn detach_block(_class: Id, _sel: Sel, block: Id) {
    let thread = with_block(new_thread(), block);
    start(thread);
    objc::arc::release(thread);
}

unsafe extern "C" fn sleep_for_interval(_class: Id, _sel: Sel, seconds: f64) {
    sleep_for(seconds);
}

unsafe extern "C" fn sleep_until_date(_class: Id, _sel: Sel, date: Id) {
    // SAFETY: `-timeIntervalSinceNow` is `double (id, SEL)`.
    if let Some(seconds) = unsafe { crate::send_scalar::<f64>(date, "timeIntervalSinceNow") } {
        sleep_for(seconds);
    }
}

/// `+exit`: the notification, then `pthread_exit`, whichever thread it is -
/// the main one included, which leaves the process to its other threads.
///
/// Only `extern "C"` calls, for the reason [`launch`] gives.
unsafe extern "C" fn exit(_class: Id, _sel: Sel) {
    // SAFETY: on the calling thread, which is the one ending.
    unsafe { exiting() };
    // SAFETY: ends the calling thread, which nothing here returns to.
    unsafe { libc::pthread_exit(NO_RESULT) }
}

#[inline(never)]
unsafe extern "C" fn exiting() {
    end(current(), true);
}

unsafe extern "C" fn class_thread_priority(_class: Id, _sel: Sel) -> f64 {
    // SAFETY: the calling thread's object.
    unsafe { state(current()) }.map_or(DEFAULT_PRIORITY, |thread| {
        f64::from_bits(thread.priority.load(Ordering::Acquire))
    })
}

unsafe extern "C" fn class_set_thread_priority(_class: Id, _sel: Sel, priority: f64) -> bool {
    // SAFETY: the calling thread's object.
    let Some(thread) = (unsafe { state(current()) }) else {
        return false;
    };
    // FIXME (ACCURACY): recorded and read back, not applied. A host's default
    // policy has a single priority, which is where Darwin's range maps.
    thread
        .priority
        .store(priority.clamp(0.0, 1.0).to_bits(), Ordering::Release);
    true
}

/// `+callStackReturnAddresses`: `NSNumber`s, innermost first, from the caller.
unsafe extern "C" fn call_stack_return_addresses(_class: Id, _sel: Sel) -> Id {
    let addresses =
        darwin::backtrace::return_addresses(call_stack_return_addresses as *const () as usize);
    array_of(addresses.into_iter().map(|address| {
        corefoundation::collections::new_number(corefoundation::collections::Number::Integer(
            address as i64,
        )) as Id
    }))
}

/// `+callStackSymbols`: one `backtrace_symbols` line per frame.
unsafe extern "C" fn call_stack_symbols(_class: Id, _sel: Sel) -> Id {
    let addresses = darwin::backtrace::return_addresses(call_stack_symbols as *const () as usize);
    array_of(addresses.into_iter().enumerate().map(|(index, address)| {
        crate::string::from_str(&darwin::backtrace::symbol_line(index, address))
    }))
}

/// An autoreleased `NSArray` of `objects`, which it takes ownership of.
fn array_of(objects: impl Iterator<Item = Id>) -> Id {
    let objects: Vec<_> = objects
        .map(|object| object as corefoundation::base::CFTypeRef)
        .collect();
    let array = corefoundation::collections::new_array(
        &objects,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for object in objects {
        objc::arc::release(object as Id);
    }
    objc::arc::autorelease(array as Id)
}

// Instance methods.

unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an instance of `NSThread` or a subclass, allocated for `Thread`.
    unsafe { boxed::fill(receiver, Thread::unstarted()) };
    receiver
}

unsafe extern "C" fn init_with_target(
    receiver: Id,
    _sel: Sel,
    target: Id,
    selector: Sel,
    argument: Id,
) -> Id {
    with_target(receiver, target, selector, argument)
}

unsafe extern "C" fn init_with_block(receiver: Id, _sel: Sel, block: Id) -> Id {
    with_block(receiver, block)
}

unsafe extern "C" fn start_thread(receiver: Id, _sel: Sel) {
    start(receiver);
}

#[repr(C)]
struct MainCall {
    send: Option<unsafe extern "C" fn(Id, Sel, Id)>,
    target: Id,
    selector: Sel,
    argument: Id,
    invoke: Option<unsafe extern "C" fn(Id)>,
    block: Id,
}

/// `-main`: send the target its message, or run the block.
///
/// Only `extern "C"` calls, for the reason [`launch`] gives: `+exit` from the
/// target's method unwinds through here too.
unsafe extern "C" fn run_main(receiver: Id, _sel: Sel) {
    // SAFETY: the receiver of a message.
    let call = unsafe { main_call(receiver) };
    if let Some(invoke) = call.invoke {
        // SAFETY: a `void (^)(void)` the thread holds a copy of.
        unsafe { invoke(call.block) };
    } else if let Some(send) = call.send {
        // SAFETY: a selector given to run on a thread takes one object.
        unsafe { send(call.target, call.selector, call.argument) };
    }
}

/// What `-main` is to call. The objects stay retained by the thread until it
/// ends, which is after `-main` returns.
///
/// # Safety
///
/// `receiver` is null or a live object.
#[inline(never)]
unsafe extern "C" fn main_call(receiver: Id) -> MainCall {
    let mut call = MainCall {
        send: None,
        target: std::ptr::null_mut(),
        selector: std::ptr::null(),
        argument: std::ptr::null_mut(),
        invoke: None,
        block: std::ptr::null_mut(),
    };
    // SAFETY: the caller's guarantee.
    let Some(thread) = (unsafe { state(receiver) }) else {
        return call;
    };
    match *lock(&thread.work) {
        Work::None => {}
        Work::Message {
            target,
            selector,
            argument,
        } => {
            // SAFETY: the prototype `MainCall::send` is used with.
            call.send = Some(unsafe { msg_send() });
            call.target = target as Id;
            call.selector = selector as Sel;
            call.argument = argument as Id;
        }
        Work::Block(block) => {
            // SAFETY: a block copy; a `void (^)(void)`'s invoke takes the block.
            call.invoke = unsafe { objc::block::invoke(block as Id) }
                .map(|invoke| unsafe { std::mem::transmute::<usize, _>(invoke) });
            call.block = block as Id;
        }
    }
    call
}

unsafe extern "C" fn cancel(receiver: Id, _sel: Sel) {
    // SAFETY: the receiver of a message.
    if let Some(thread) = unsafe { state(receiver) } {
        thread.cancelled.store(true, Ordering::Release);
    }
}

unsafe extern "C" fn is_cancelled(receiver: Id, _sel: Sel) -> bool {
    // SAFETY: the receiver of a message.
    unsafe { state(receiver) }.is_some_and(|thread| thread.cancelled.load(Ordering::Acquire))
}

unsafe extern "C" fn is_executing(receiver: Id, _sel: Sel) -> bool {
    // SAFETY: the receiver of a message.
    unsafe { state(receiver) }.is_some_and(|thread| thread.status() == Status::Executing)
}

unsafe extern "C" fn is_finished(receiver: Id, _sel: Sel) -> bool {
    // SAFETY: the receiver of a message.
    unsafe { state(receiver) }.is_some_and(|thread| thread.status() == Status::Finished)
}

unsafe extern "C" fn is_main_thread(receiver: Id, _sel: Sel) -> bool {
    receiver == main()
}

unsafe extern "C" fn name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver of a message.
    let Some(thread) = (unsafe { state(receiver) }) else {
        return std::ptr::null_mut();
    };
    let name = objc::arc::retain(*lock(&thread.name) as Id);
    objc::arc::autorelease(name)
}

/// `-setName:`, which names the OS thread too when the receiver is the
/// calling thread's: Darwin can only name the thread it is called on.
unsafe extern "C" fn set_name(receiver: Id, _sel: Sel, name: Id) {
    // SAFETY: the receiver of a message.
    let Some(thread) = (unsafe { state(receiver) }) else {
        return;
    };
    // SAFETY: `-copy` is `id (id, SEL)`; nil answers nil.
    let copy =
        unsafe { msg_send::<unsafe extern "C" fn(Id, Sel) -> Id>()(name, objc::sel::get("copy")) };
    let old = std::mem::replace(&mut *lock(&thread.name), copy as usize);
    objc::arc::release(old as Id);
    if receiver == current() && thread.status() == Status::Executing {
        apply_name(copy);
    }
}

unsafe extern "C" fn stack_size(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: the receiver of a message.
    match unsafe { state(receiver) }.map(|thread| thread.stack_size.load(Ordering::Acquire)) {
        Some(0) | None => darwin::pthread::attr::DEFAULT_STACK_SIZE,
        Some(size) => size,
    }
}

/// A size `pthread_attr_setstacksize` would refuse is ignored, as Apple's
/// `NSThread` ignores that call failing.
unsafe extern "C" fn set_stack_size(receiver: Id, _sel: Sel, size: usize) {
    // SAFETY: the receiver of a message.
    if let Some(thread) = unsafe { state(receiver) }
        && darwin::pthread::attr::valid_stack_size(size)
    {
        thread.stack_size.store(size, Ordering::Release);
    }
}

unsafe extern "C" fn thread_priority(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: the receiver of a message.
    unsafe { state(receiver) }.map_or(DEFAULT_PRIORITY, |thread| {
        f64::from_bits(thread.priority.load(Ordering::Acquire))
    })
}

unsafe extern "C" fn set_thread_priority(receiver: Id, _sel: Sel, priority: f64) {
    // SAFETY: the receiver of a message.
    if let Some(thread) = unsafe { state(receiver) } {
        // FIXME (ACCURACY): as `+setThreadPriority:`, recorded and not applied.
        thread
            .priority
            .store(priority.clamp(0.0, 1.0).to_bits(), Ordering::Release);
    }
}

unsafe extern "C" fn quality_of_service(receiver: Id, _sel: Sel) -> i64 {
    // SAFETY: the receiver of a message.
    unsafe { state(receiver) }.map_or(QOS_DEFAULT, |thread| {
        thread.quality_of_service.load(Ordering::Acquire)
    })
}

/// Ignored once the thread has started, which Apple documents as the point
/// after which it cannot change.
unsafe extern "C" fn set_quality_of_service(receiver: Id, _sel: Sel, quality_of_service: i64) {
    // SAFETY: the receiver of a message.
    if let Some(thread) = unsafe { state(receiver) } {
        let status = lock(&thread.status);
        if *status == Status::Created {
            thread
                .quality_of_service
                .store(quality_of_service, Ordering::Release);
        }
    }
}

/// `-threadDictionary`: owned by the thread object, made on first use.
unsafe extern "C" fn thread_dictionary(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver of a message.
    let Some(thread) = (unsafe { state(receiver) }) else {
        return std::ptr::null_mut();
    };
    let mut dictionary = lock(&thread.dictionary);
    if *dictionary == 0 {
        *dictionary = corefoundation::collections::new_dictionary(
            &[],
            corefoundation::callbacks::Policy::from_key_callbacks(),
            corefoundation::callbacks::Policy::from_type_callbacks(),
            true,
        ) as usize;
    }
    *dictionary as Id
}

/// `<NSThread: 0x...>{number = 1, name = main}`, as macOS prints it.
unsafe extern "C" fn description(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver of a message.
    let class = unsafe { objc::object::class_of(receiver) }
        // SAFETY: a class's name is a NUL-terminated string it owns.
        .map(|class| unsafe { std::ffi::CStr::from_ptr(class.name()) }.to_string_lossy())
        .unwrap_or_default();
    // SAFETY: the receiver of a message.
    let (number, name) = match unsafe { state(receiver) } {
        Some(thread) => {
            let name = *lock(&thread.name) as Id;
            let name = match name.is_null() {
                true => "(null)".to_string(),
                // SAFETY: a string the thread holds.
                false => {
                    String::from_utf8_lossy(&unsafe { crate::string::bytes(name) }).into_owned()
                }
            };
            (thread.number, name)
        }
        None => (0, "(null)".to_string()),
    };
    objc::arc::autorelease(crate::string::from_str(&format!(
        "<{class}: {receiver:p}>{{number = {number}, name = {name}}}"
    )))
}

// Performing a selector on a thread.

/// Queue a message for `object`'s thread's run loop, in `modes`, and wait for it
/// to run if `wait`.
///
/// A wait on the calling thread's own object runs the message at once; one on
/// another thread waits for that thread's loop to run in one of `modes`, as on
/// macOS - forever, if it never does.
fn perform_on(object: Id, target: Id, selector: Sel, argument: Id, wait: bool, modes: Vec<String>) {
    // SAFETY: the argument a caller named a thread with.
    let Some(thread) = (unsafe { state(object) }) else {
        return;
    };
    if modes.is_empty() {
        return;
    }
    if wait && object == current() {
        // SAFETY: a selector given to run on a thread takes one object.
        unsafe { msg_send::<unsafe extern "C" fn(Id, Sel, Id)>()(target, selector, argument) };
        return;
    }
    let done = wait.then(|| Arc::new(Done::default()));
    lock(&thread.performs).pending.push(Perform {
        target: objc::arc::retain(target) as usize,
        selector: selector as usize,
        argument: objc::arc::retain(argument) as usize,
        modes,
        done: done.clone(),
    });
    schedule(object, thread);
    if let Some(done) = done {
        done.wait();
    }
}

/// Put the thread's source in every mode a queued message asked for, and take
/// it out of every mode none does, then signal it if there is anything left.
///
/// Waits for the thread's loop to be known, which is when its thread runs.
fn schedule(object: Id, thread: &Thread) {
    let Some(run_loop) = thread.run_loop.get() else {
        return;
    };
    let mut performs = lock(&thread.performs);
    let mut wanted: Vec<String> = Vec::new();
    for mode in performs.pending.iter().flat_map(|perform| &perform.modes) {
        if !wanted.contains(mode) {
            wanted.push(mode.clone());
        }
    }
    if wanted.is_empty() && performs.source.is_none() {
        return;
    }
    let source = Arc::clone(performs.source.get_or_insert_with(|| {
        runloop::new_source_with_affinity(run_performs, object as usize, on_own_thread)
    }));
    for mode in &performs.modes {
        if !wanted.contains(mode) {
            run_loop.remove_source(&source, mode);
        }
    }
    for mode in &wanted {
        if !performs.modes.contains(mode) {
            run_loop.add_source(&source, mode);
        }
    }
    performs.modes = wanted;
    if !performs.pending.is_empty() {
        source.signal();
        run_loop.wake();
    }
}

/// Whether the calling thread is the one `info`, a thread object, stands for.
///
/// `NSRunLoop` here runs the main thread's loop from whichever thread asks, so
/// the main thread's source can be reached from another thread.
fn on_own_thread(info: usize) -> bool {
    current() == info as Id
}

/// The thread's source fired: run, in order, every queued message that may run
/// in the mode the loop is in.
unsafe extern "C" fn run_performs(info: *mut c_void) {
    let object = info as Id;
    // SAFETY: the source is invalidated before its thread object goes.
    let Some(thread) = (unsafe { state(object) }) else {
        return;
    };
    let Some(run_loop) = thread.run_loop.get() else {
        return;
    };
    let Some(mode) = run_loop.current_mode() else {
        return;
    };
    let due: Vec<Perform> = {
        let mut performs = lock(&thread.performs);
        let (due, kept) = std::mem::take(&mut performs.pending)
            .into_iter()
            .partition(|perform| {
                perform
                    .modes
                    .iter()
                    .any(|requested| run_loop.includes(requested, &mode))
            });
        performs.pending = kept;
        due
    };
    schedule(object, thread);
    // SAFETY: the prototype every perform is sent with.
    let send: unsafe extern "C" fn(Id, Sel, Id) = unsafe { msg_send() };
    for perform in due {
        let pool = objc::arc::pool_push();
        // SAFETY: a selector given to run on a thread takes one object; both
        // objects are retained by the queue entry.
        unsafe {
            send(
                perform.target as Id,
                perform.selector as Sel,
                perform.argument as Id,
            )
        };
        objc::arc::pool_pop(pool);
        if let Some(done) = &perform.done {
            done.finish();
        }
        perform.release();
    }
}

/// The run loop mode names in an `NSArray` of strings.
///
/// # Safety
///
/// `array` must be null or an array of strings.
unsafe fn mode_names(array: Id) -> Vec<String> {
    if array.is_null() {
        return Vec::new();
    }
    // SAFETY: `-count` is `NSUInteger (id, SEL)` and `-objectAtIndex:` is
    // `id (id, SEL, NSUInteger)`.
    unsafe {
        let count =
            msg_send::<unsafe extern "C" fn(Id, Sel) -> usize>()(array, objc::sel::get("count"));
        let object_at = msg_send::<unsafe extern "C" fn(Id, Sel, usize) -> Id>();
        (0..count)
            .map(|index| object_at(array, objc::sel::get("objectAtIndex:"), index))
            .map(|mode| String::from_utf8_lossy(&crate::string::bytes(mode)).into_owned())
            .collect()
    }
}

fn common_modes() -> Vec<String> {
    vec![runloop::COMMON_MODES.to_string()]
}

unsafe extern "C" fn perform_on_main_thread(
    receiver: Id,
    _sel: Sel,
    selector: Sel,
    argument: Id,
    wait: i8,
) {
    perform_on(
        main(),
        receiver,
        selector,
        argument,
        wait != 0,
        common_modes(),
    );
}

unsafe extern "C" fn perform_on_main_thread_in_modes(
    receiver: Id,
    _sel: Sel,
    selector: Sel,
    argument: Id,
    wait: i8,
    modes: Id,
) {
    // SAFETY: the caller's array of mode names.
    let modes = unsafe { mode_names(modes) };
    perform_on(main(), receiver, selector, argument, wait != 0, modes);
}

unsafe extern "C" fn perform_on_thread(
    receiver: Id,
    _sel: Sel,
    selector: Sel,
    thread: Id,
    argument: Id,
    wait: i8,
) {
    perform_on(
        thread,
        receiver,
        selector,
        argument,
        wait != 0,
        common_modes(),
    );
}

unsafe extern "C" fn perform_on_thread_in_modes(
    receiver: Id,
    _sel: Sel,
    selector: Sel,
    thread: Id,
    argument: Id,
    wait: i8,
    modes: Id,
) {
    // SAFETY: the caller's array of mode names.
    let modes = unsafe { mode_names(modes) };
    perform_on(thread, receiver, selector, argument, wait != 0, modes);
}

unsafe extern "C" fn perform_in_background(receiver: Id, _sel: Sel, selector: Sel, argument: Id) {
    let thread = with_target(new_thread(), receiver, selector, argument);
    start(thread);
    objc::arc::release(thread);
}

pub(crate) fn install() {
    let class = thread_class();
    class_methods!(class, {
        "currentThread" => current_thread, c"@16@0:8";
        "mainThread" => main_thread, c"@16@0:8";
        "isMainThread" => class_is_main_thread, c"B16@0:8";
        "isMultiThreaded" => is_multi_threaded, c"B16@0:8";
        "detachNewThreadSelector:toTarget:withObject:" => detach_selector, c"v40@0:8:16@24@32";
        "detachNewThreadWithBlock:" => detach_block, c"v24@0:8@?16";
        "sleepForTimeInterval:" => sleep_for_interval, c"v24@0:8d16";
        "sleepUntilDate:" => sleep_until_date, c"v24@0:8@16";
        "exit" => exit, c"v16@0:8";
        "threadPriority" => class_thread_priority, c"d16@0:8";
        "setThreadPriority:" => class_set_thread_priority, c"B24@0:8d16";
        "callStackReturnAddresses" => call_stack_return_addresses, c"@16@0:8";
        "callStackSymbols" => call_stack_symbols, c"@16@0:8";
    });
    methods!(class, {
        "init" => init, c"@16@0:8";
        "initWithTarget:selector:object:" => init_with_target, c"@40@0:8@16:24@32";
        "initWithBlock:" => init_with_block, c"@24@0:8@?16";
        "start" => start_thread, c"v16@0:8";
        "main" => run_main, c"v16@0:8";
        "cancel" => cancel, c"v16@0:8";
        "isCancelled" => is_cancelled, c"B16@0:8";
        "isExecuting" => is_executing, c"B16@0:8";
        "isFinished" => is_finished, c"B16@0:8";
        "isMainThread" => is_main_thread, c"B16@0:8";
        "name" => name, c"@16@0:8";
        "setName:" => set_name, c"v24@0:8@16";
        "stackSize" => stack_size, c"Q16@0:8";
        "setStackSize:" => set_stack_size, c"v24@0:8Q16";
        "threadPriority" => thread_priority, c"d16@0:8";
        "setThreadPriority:" => set_thread_priority, c"v24@0:8d16";
        "qualityOfService" => quality_of_service, c"q16@0:8";
        "setQualityOfService:" => set_quality_of_service, c"v24@0:8q16";
        "threadDictionary" => thread_dictionary, c"@16@0:8";
        "description" => description, c"@16@0:8";
    });

    let nsobject = objc::class::lookup(c"NSObject").expect("the runtime's own NSObject");
    methods!(nsobject, {
        "performSelectorOnMainThread:withObject:waitUntilDone:" => perform_on_main_thread,
            c"v36@0:8:16@24B32";
        "performSelectorOnMainThread:withObject:waitUntilDone:modes:"
            => perform_on_main_thread_in_modes, c"v48@0:8:16@24B32@40";
        "performSelector:onThread:withObject:waitUntilDone:" => perform_on_thread,
            c"v44@0:8:16@24@32B40";
        "performSelector:onThread:withObject:waitUntilDone:modes:"
            => perform_on_thread_in_modes, c"v56@0:8:16@24@32B40@48";
        "performSelectorInBackground:withObject:" => perform_in_background, c"v32@0:8:16@24";
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    fn sel(name: &str) -> Sel {
        objc::sel::get(name)
    }

    fn class_object() -> Id {
        crate::bootstrap();
        objc::class::ptr(thread_class()) as Id
    }

    type Imp = unsafe extern "C" fn(Id, Sel, Id);

    /// An instance of a class of this test's own, with these one-object methods.
    fn instance_of(name: &std::ffi::CStr, methods: &[(&str, Imp)]) -> Id {
        crate::bootstrap();
        let class = corefoundation::base::register(name, None, size_of::<usize>(), false);
        for (selector, imp) in methods {
            class.replace_method(sel(selector), *imp as *const c_void, c"v24@0:8@16");
        }
        objc::object::create_instance(class, 0)
    }

    /// `[[NSThread alloc] initWithTarget:selector:object:]`, sent.
    fn thread_with(target: Id, selector: &str, argument: Id) -> Id {
        // SAFETY: `+alloc` is `id (id, SEL)` and the initializer is
        // `id (id, SEL, id, SEL, id)`.
        unsafe {
            let object =
                msg_send::<unsafe extern "C" fn(Id, Sel) -> Id>()(class_object(), sel("alloc"));
            msg_send::<unsafe extern "C" fn(Id, Sel, Id, Sel, Id) -> Id>()(
                object,
                sel("initWithTarget:selector:object:"),
                target,
                sel(selector),
                argument,
            )
        }
    }

    fn send_bool(receiver: Id, selector: &str) -> bool {
        // SAFETY: every selector sent here is `BOOL (id, SEL)`.
        unsafe { msg_send::<unsafe extern "C" fn(Id, Sel) -> bool>()(receiver, sel(selector)) }
    }

    fn send_void(receiver: Id, selector: &str) {
        // SAFETY: every selector sent here is `void (id, SEL)`.
        unsafe { msg_send::<unsafe extern "C" fn(Id, Sel)>()(receiver, sel(selector)) }
    }

    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    static RAN_ON: AtomicUsize = AtomicUsize::new(0);
    static RAN_EXECUTING: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" fn record_current(_receiver: Id, _sel: Sel, _argument: Id) {
        let current = current();
        RAN_ON.store(current as usize, Ordering::SeqCst);
        RAN_EXECUTING.store(send_bool(current, "isExecuting"), Ordering::SeqCst);
    }

    #[test]
    fn a_started_thread_is_current_and_executing_while_its_target_runs() {
        let target = instance_of(
            c"RosemaryTestThreadCurrent",
            &[("run:", record_current as Imp)],
        );
        let thread = thread_with(target, "run:", std::ptr::null_mut());
        assert!(!send_bool(thread, "isExecuting") && !send_bool(thread, "isFinished"));

        send_void(thread, "start");
        wait_until("the thread to finish", || send_bool(thread, "isFinished"));

        assert_eq!(RAN_ON.load(Ordering::SeqCst), thread as usize);
        assert!(RAN_EXECUTING.load(Ordering::SeqCst));
        assert!(!send_bool(thread, "isExecuting"));
        assert!(send_bool(class_object(), "isMultiThreaded"));
        assert!(!send_bool(thread, "isMainThread"));
        objc::arc::release(thread);
        objc::arc::release(target);
    }

    static CANCELLED_RAN: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" fn cancelled_ran(_receiver: Id, _sel: Sel, _argument: Id) {
        CANCELLED_RAN.store(true, Ordering::SeqCst);
    }

    #[test]
    fn a_thread_cancelled_before_it_starts_finishes_without_running() {
        let target = instance_of(
            c"RosemaryTestThreadCancelled",
            &[("run:", cancelled_ran as Imp)],
        );
        let thread = thread_with(target, "run:", std::ptr::null_mut());
        send_void(thread, "cancel");
        send_void(thread, "start");
        assert!(send_bool(thread, "isCancelled"));
        assert!(send_bool(thread, "isFinished"));
        std::thread::sleep(Duration::from_millis(20));
        assert!(!CANCELLED_RAN.load(Ordering::SeqCst));
        objc::arc::release(thread);
        objc::arc::release(target);
    }

    #[derive(Clone, Copy)]
    struct ExitCall {
        send: unsafe extern "C" fn(Id, Sel),
        class: Id,
        exit: Sel,
    }

    /// Read by value from a frame that must make no Rust calls, so a plain
    /// static rather than a lock. Written before the thread that reads it starts.
    static mut EXIT_CALL: Option<ExitCall> = None;
    static AFTER_EXIT: AtomicBool = AtomicBool::new(false);
    static EXIT_HEARD: AtomicUsize = AtomicUsize::new(0);
    static EXIT_HEARD_EXECUTING: AtomicBool = AtomicBool::new(false);

    extern "C" fn mark_after_exit() {
        AFTER_EXIT.store(true, Ordering::SeqCst);
    }

    /// A target that ends its thread with `+exit`. Only `extern "C"` calls, as
    /// guest code would make: the forced unwind passes through this frame.
    unsafe extern "C" fn calls_exit(_receiver: Id, _sel: Sel, _argument: Id) {
        // SAFETY: written before this thread started.
        if let Some(call) = unsafe { EXIT_CALL } {
            // SAFETY: `+exit` is `void (id, SEL)`.
            unsafe { (call.send)(call.class, call.exit) };
        }
        mark_after_exit();
    }

    unsafe extern "C" fn heard_exit(_receiver: Id, _sel: Sel, notification: Id) {
        // SAFETY: `-object` is `id (id, SEL)`.
        let object = unsafe {
            msg_send::<unsafe extern "C" fn(Id, Sel) -> Id>()(notification, sel("object"))
        };
        EXIT_HEARD.store(object as usize, Ordering::SeqCst);
        EXIT_HEARD_EXECUTING.store(send_bool(object, "isExecuting"), Ordering::SeqCst);
    }

    fn center() -> Id {
        crate::notification::default_center_object()
    }

    #[test]
    fn exit_ends_the_thread_after_posting_will_exit_for_it() {
        let target = instance_of(c"RosemaryTestThreadExit", &[("run:", calls_exit as Imp)]);
        let observer = instance_of(
            c"RosemaryTestThreadExitObserver",
            &[("heard:", heard_exit as Imp)],
        );
        let thread = thread_with(target, "run:", std::ptr::null_mut());
        // SAFETY: no thread reads it yet.
        unsafe {
            EXIT_CALL = Some(ExitCall {
                send: msg_send(),
                class: class_object(),
                exit: sel("exit"),
            })
        };
        let name = crate::string::from_str(WILL_EXIT);
        // SAFETY: `-addObserver:selector:name:object:` is
        // `void (id, SEL, id, SEL, id, id)`.
        unsafe {
            msg_send::<unsafe extern "C" fn(Id, Sel, Id, Sel, Id, Id)>()(
                center(),
                sel("addObserver:selector:name:object:"),
                observer,
                sel("heard:"),
                name,
                thread,
            )
        };

        send_void(thread, "start");
        wait_until("the thread to finish", || send_bool(thread, "isFinished"));

        assert!(!AFTER_EXIT.load(Ordering::SeqCst), "+exit returned");
        assert_eq!(EXIT_HEARD.load(Ordering::SeqCst), thread as usize);
        assert!(EXIT_HEARD_EXECUTING.load(Ordering::SeqCst));
        // SAFETY: `-removeObserver:` is `void (id, SEL, id)`.
        unsafe {
            msg_send::<unsafe extern "C" fn(Id, Sel, Id)>()(
                center(),
                sel("removeObserver:"),
                observer,
            )
        };
        for object in [thread, target, observer, name] {
            objc::arc::release(object);
        }
    }

    static STOP_LOOP: AtomicBool = AtomicBool::new(false);
    static PERFORMED_ON: AtomicUsize = AtomicUsize::new(0);
    static PERFORMED_WITH: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn run_loop_until_stopped(_receiver: Id, _sel: Sel, _argument: Id) {
        let run_loop = runloop::current();
        while !STOP_LOOP.load(Ordering::SeqCst) {
            let result = run_loop.run_in_mode(runloop::DEFAULT_MODE, 0.05, true);
            if result == runloop::result::FINISHED {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    unsafe extern "C" fn note_performed(_receiver: Id, _sel: Sel, argument: Id) {
        PERFORMED_ON.store(current() as usize, Ordering::SeqCst);
        PERFORMED_WITH.store(argument as usize, Ordering::SeqCst);
    }

    #[test]
    fn a_waited_perform_on_a_thread_runs_on_that_threads_loop_before_returning() {
        let target = instance_of(
            c"RosemaryTestThreadPerform",
            &[
                ("spin:", run_loop_until_stopped as Imp),
                ("note:", note_performed as Imp),
            ],
        );
        let thread = thread_with(target, "spin:", std::ptr::null_mut());
        send_void(thread, "start");

        // SAFETY: `-performSelector:onThread:withObject:waitUntilDone:` is
        // `void (id, SEL, SEL, id, id, BOOL)`.
        unsafe {
            msg_send::<unsafe extern "C" fn(Id, Sel, Sel, Id, Id, bool)>()(
                target,
                sel("performSelector:onThread:withObject:waitUntilDone:"),
                sel("note:"),
                thread,
                target,
                true,
            )
        };
        assert_eq!(PERFORMED_ON.load(Ordering::SeqCst), thread as usize);
        assert_eq!(PERFORMED_WITH.load(Ordering::SeqCst), target as usize);

        STOP_LOOP.store(true, Ordering::SeqCst);
        wait_until("the thread to finish", || send_bool(thread, "isFinished"));
        objc::arc::release(thread);
        objc::arc::release(target);
    }

    const CUSTOM_MODE: &str = "RosemaryTestThreadPerformMode";
    static IN_CUSTOM_MODE: AtomicBool = AtomicBool::new(false);
    static STOP_MODE_LOOP: AtomicBool = AtomicBool::new(false);
    static MODE_PERFORMED_ON: AtomicUsize = AtomicUsize::new(0);

    /// Run this thread's loop in the default mode, or the custom one once told.
    unsafe extern "C" fn run_loop_in_chosen_mode(_receiver: Id, _sel: Sel, _argument: Id) {
        let run_loop = runloop::current();
        while !STOP_MODE_LOOP.load(Ordering::SeqCst) {
            let mode = match IN_CUSTOM_MODE.load(Ordering::SeqCst) {
                true => CUSTOM_MODE,
                false => runloop::DEFAULT_MODE,
            };
            if run_loop.run_in_mode(mode, 0.01, true) == runloop::result::FINISHED {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    unsafe extern "C" fn note_mode_performed(_receiver: Id, _sel: Sel, _argument: Id) {
        MODE_PERFORMED_ON.store(current() as usize, Ordering::SeqCst);
    }

    fn array_of_strings(strings: &[&str]) -> Id {
        let strings: Vec<_> = strings
            .iter()
            .map(|text| crate::string::from_str(text) as corefoundation::base::CFTypeRef)
            .collect();
        let array = corefoundation::collections::new_array(
            &strings,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        ) as Id;
        for string in strings {
            objc::arc::release(string as Id);
        }
        array
    }

    #[test]
    fn a_perform_runs_only_when_its_thread_is_in_a_mode_it_names() {
        let target = instance_of(
            c"RosemaryTestThreadModePerform",
            &[
                ("spin:", run_loop_in_chosen_mode as Imp),
                ("note:", note_mode_performed as Imp),
            ],
        );
        let thread = thread_with(target, "spin:", std::ptr::null_mut());
        send_void(thread, "start");
        let modes = array_of_strings(&[CUSTOM_MODE]);
        // SAFETY: the `...waitUntilDone:modes:` prototype.
        unsafe {
            msg_send::<unsafe extern "C" fn(Id, Sel, Sel, Id, Id, bool, Id)>()(
                target,
                sel("performSelector:onThread:withObject:waitUntilDone:modes:"),
                sel("note:"),
                thread,
                std::ptr::null_mut(),
                false,
                modes,
            )
        };
        objc::arc::release(modes);

        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(MODE_PERFORMED_ON.load(Ordering::SeqCst), 0);
        IN_CUSTOM_MODE.store(true, Ordering::SeqCst);
        wait_until("the perform", || {
            MODE_PERFORMED_ON.load(Ordering::SeqCst) != 0
        });
        assert_eq!(MODE_PERFORMED_ON.load(Ordering::SeqCst), thread as usize);

        STOP_MODE_LOOP.store(true, Ordering::SeqCst);
        wait_until("the thread to finish", || send_bool(thread, "isFinished"));
        objc::arc::release(thread);
        objc::arc::release(target);
    }

    static ON_MAIN: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" fn note_on_main(_receiver: Id, _sel: Sel, _argument: Id) {
        ON_MAIN.store(true, Ordering::SeqCst);
    }

    /// The main loop can be run from another thread here, by `NSRunLoop`; a
    /// main-thread perform still waits for the main thread.
    #[test]
    fn a_main_thread_perform_does_not_run_on_another_thread() {
        let receiver = instance_of(
            c"RosemaryTestThreadMainPerform",
            &[("note:", note_on_main as Imp)],
        );
        // SAFETY: the `...waitUntilDone:` prototype.
        unsafe {
            msg_send::<unsafe extern "C" fn(Id, Sel, Sel, Id, bool)>()(
                receiver,
                sel("performSelectorOnMainThread:withObject:waitUntilDone:"),
                sel("note:"),
                std::ptr::null_mut(),
                false,
            )
        };
        objc::arc::release(receiver);
        for _ in 0..5 {
            runloop::main().run_in_mode(runloop::DEFAULT_MODE, 0.01, true);
        }
        assert!(!ON_MAIN.load(Ordering::SeqCst));
    }

    static BLOCK_RAN_ON: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn block_body(_block: Id) {
        BLOCK_RAN_ON.store(current() as usize, Ordering::SeqCst);
    }

    #[test]
    fn a_detached_block_runs_on_a_thread_of_its_own() {
        let block = crate::block::fake::block(block_body as *const c_void);
        // SAFETY: `+detachNewThreadWithBlock:` is `void (id, SEL, id)`.
        unsafe {
            msg_send::<unsafe extern "C" fn(Id, Sel, Id)>()(
                class_object(),
                sel("detachNewThreadWithBlock:"),
                block.id(),
            )
        };
        wait_until("the block", || BLOCK_RAN_ON.load(Ordering::SeqCst) != 0);
        let ran_on = BLOCK_RAN_ON.load(Ordering::SeqCst) as Id;
        assert_ne!(ran_on, current());
        assert_ne!(ran_on, main());
    }

    #[test]
    fn a_foreign_threads_object_is_finished_and_released_when_the_thread_ends() {
        crate::bootstrap();
        let object = std::thread::spawn(|| objc::arc::retain(current()) as usize)
            .join()
            .expect("the thread") as Id;
        assert!(send_bool(object, "isFinished"));
        assert_eq!(objc::arc::retain_count(object), 1);
        objc::arc::release(object);
    }

    #[test]
    fn the_main_thread_is_named_main_and_numbered_one() {
        crate::bootstrap();
        // SAFETY: `+mainThread` and `-description` are `id (id, SEL)`.
        let description = unsafe {
            let main = msg_send::<unsafe extern "C" fn(Id, Sel) -> Id>()(
                class_object(),
                sel("mainThread"),
            );
            assert!(send_bool(main, "isMainThread"));
            msg_send::<unsafe extern "C" fn(Id, Sel) -> Id>()(main, sel("description"))
        };
        // SAFETY: a string.
        let description = String::from_utf8(unsafe { crate::string::bytes(description) }).unwrap();
        assert!(
            description.ends_with("{number = 1, name = main}"),
            "{description}"
        );
    }

    #[test]
    fn stack_size_takes_only_sizes_darwin_accepts() {
        crate::bootstrap();
        let thread = new_thread();
        let size = |thread| unsafe { stack_size(thread, sel("stackSize")) };
        assert_eq!(size(thread), darwin::pthread::attr::DEFAULT_STACK_SIZE);
        unsafe { set_stack_size(thread, sel("setStackSize:"), 1000) };
        assert_eq!(size(thread), darwin::pthread::attr::DEFAULT_STACK_SIZE);
        unsafe { set_stack_size(thread, sel("setStackSize:"), 1 << 20) };
        assert_eq!(size(thread), 1 << 20);
        objc::arc::release(thread);
    }

    #[test]
    fn a_name_is_copied_and_the_dictionary_is_the_threads_own() {
        crate::bootstrap();
        let thread = new_thread();
        let name = crate::string::from_str("worker");
        // SAFETY: the prototypes of the methods called.
        unsafe {
            assert!(super::name(thread, sel("name")).is_null());
            set_name(thread, sel("setName:"), name);
            let read = super::name(thread, sel("name"));
            assert_eq!(crate::string::bytes(read), b"worker");
            let dictionary = thread_dictionary(thread, sel("threadDictionary"));
            assert!(!dictionary.is_null());
            assert_eq!(
                thread_dictionary(thread, sel("threadDictionary")),
                dictionary
            );
            assert_ne!(
                thread_dictionary(main(), sel("threadDictionary")),
                dictionary
            );
        }
        objc::arc::release(name);
        objc::arc::release(thread);
    }
}
