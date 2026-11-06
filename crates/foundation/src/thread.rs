use std::sync::atomic::{AtomicBool, Ordering};

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// Set once a thread has been detached, which is the whole of what
/// `+isMultiThreaded` reports: a guest that made its own threads with
/// `pthread_create` gets NO here on Apple too.
static MULTITHREADED: AtomicBool = AtomicBool::new(false);

/// A retained object, so the sending thread cannot release what the new thread
/// is about to be handed.
struct Detached {
    target: usize,
    object: usize,
    selector: usize,
}

// SAFETY: the two objects are retained for the new thread's lifetime, and every
// runtime they belong to is shared across threads already.
unsafe impl Send for Detached {}

unsafe extern "C" fn detach(_class: Id, _sel: Sel, selector: Sel, target: Id, object: Id) {
    if target.is_null() || selector.is_null() {
        return;
    }
    MULTITHREADED.store(true, Ordering::Relaxed);
    let detached = Detached {
        target: objc::arc::retain(target) as usize,
        object: objc::arc::retain(object) as usize,
        selector: selector as usize,
    };
    std::thread::spawn(move || {
        // The guest reads errno and its thread-locals off the thread pointer,
        // which is the parent's until the new thread enters.
        let _darwin = darwin::tls::DarwinThread::enter();
        let detached = detached;
        let (target, object) = (detached.target as Id, detached.object as Id);
        let selector = detached.selector as Sel;
        // Apple gives a detached thread a pool of its own, and a method written
        // for one autoreleases into it.
        let pool = objc::arc::pool_push();
        // SAFETY: a live object, retained above.
        let method = unsafe { objc::object::class_of(target) }.and_then(|c| c.method(selector));
        let Some(method) = method else {
            objc::fail!(
                "+[NSThread detachNewThreadSelector:toTarget:withObject:]: the target does not \
                 respond to the selector"
            );
        };
        // SAFETY: a detached thread's method is `void (id, SEL, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(method.implementation());
            f(target, selector, object);
        }
        objc::arc::pool_pop(pool);
        objc::arc::release(object);
        objc::arc::release(target);
    });
}

unsafe extern "C" fn sleep_for_interval(_class: Id, _sel: Sel, seconds: f64) {
    if seconds > 0.0 {
        std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
    }
}

unsafe extern "C" fn is_multi_threaded(_class: Id, _sel: Sel) -> bool {
    MULTITHREADED.load(Ordering::Relaxed)
}

unsafe extern "C" fn is_main_thread(_receiver: Id, _sel: Sel) -> bool {
    // SAFETY: this asks about the calling thread, not an object pointer.
    unsafe { darwin::pthread::main_np() != 0 }
}

thread_local! {
    static CURRENT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The calling thread's `NSThread`, made the first time it is asked for.
///
/// Never released: a thread's object outlives every reference a program could
/// have kept to it, and nothing here learns when a foreign thread ends.
fn current() -> Id {
    CURRENT.with(|current| {
        if current.get() == 0 {
            // SAFETY: this asks about the calling thread.
            let object = match unsafe { darwin::pthread::main_np() } != 0 {
                true => main(),
                false => new_thread(),
            };
            current.set(object as usize);
        }
        current.get() as Id
    })
}

/// The main thread's `NSThread`, which any thread may ask for.
fn main() -> Id {
    static MAIN: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *MAIN.get_or_init(|| new_thread() as usize) as Id
}

fn new_thread() -> Id {
    let class = objc::class::lookup(c"NSThread").expect("declared abstract by this crate");
    objc::object::create_instance(class, 0)
}

unsafe extern "C" fn current_thread(_class: Id, _sel: Sel) -> Id {
    current()
}

unsafe extern "C" fn main_thread(_class: Id, _sel: Sel) -> Id {
    main()
}

/// `-threadDictionary`, one mutable dictionary per thread object.
///
/// Kept beside the object rather than in it, since a guest's own subclass
/// allocates instances this crate did not size.
unsafe extern "C" fn thread_dictionary(receiver: Id, _sel: Sel) -> Id {
    static DICTIONARIES: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());
    let mut dictionaries = DICTIONARIES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, dictionary)) = dictionaries.iter().find(|(t, _)| *t == receiver as usize) {
        return *dictionary as Id;
    }
    let dictionary = corefoundation::collections::new_dictionary(
        &[],
        corefoundation::callbacks::Policy::from_key_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        true,
    );
    dictionaries.push((receiver as usize, dictionary as usize));
    dictionary as Id
}

/// What an `NSThread` made with `-init...` carries, by object address.
///
/// Kept beside the object for the reason `-threadDictionary` is: a guest's own
/// subclass allocates instances this crate did not size.
#[derive(Default)]
struct Instance {
    target: usize,
    selector: usize,
    argument: usize,
    block: usize,
    name: usize,
    stack_size: u64,
    priority: f64,
    quality: i64,
    started: bool,
    executing: bool,
    finished: bool,
    cancelled: bool,
}

static INSTANCES: std::sync::Mutex<Vec<(usize, Instance)>> = std::sync::Mutex::new(Vec::new());

fn with_instance<T>(receiver: Id, f: impl FnOnce(&mut Instance) -> T) -> T {
    let mut instances = INSTANCES.lock().unwrap_or_else(|e| e.into_inner());
    let index = match instances.iter().position(|(o, _)| *o == receiver as usize) {
        Some(index) => index,
        None => {
            instances.push((
                receiver as usize,
                Instance {
                    // Not made by `-init...`: the main thread or a foreign one.
                    executing: true,
                    priority: 0.5,
                    // NSQualityOfServiceDefault.
                    quality: -1,
                    ..Instance::default()
                },
            ));
            instances.len() - 1
        }
    };
    f(&mut instances[index].1)
}

fn instance_flag(receiver: Id, f: impl FnOnce(&Instance) -> bool) -> bool {
    let instances = INSTANCES.lock().unwrap_or_else(|e| e.into_inner());
    // The main thread, and one this crate made for a foreign thread, is running.
    let running = Instance {
        executing: true,
        ..Instance::default()
    };
    let instance = instances
        .iter()
        .find(|(o, _)| *o == receiver as usize)
        .map(|(_, i)| i);
    f(instance.unwrap_or(&running))
}

unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    if !receiver.is_null() {
        with_instance(receiver, |i| i.executing = false);
    }
    receiver
}

/// `-initWithTarget:selector:object:`; Apple retains the target and the object
/// until the thread exits.
unsafe extern "C" fn init_with_target(
    receiver: Id,
    _sel: Sel,
    target: Id,
    selector: Sel,
    argument: Id,
) -> Id {
    if receiver.is_null() {
        return receiver;
    }
    with_instance(receiver, |i| {
        i.executing = false;
        i.target = objc::arc::retain(target) as usize;
        i.selector = selector as usize;
        i.argument = objc::arc::retain(argument) as usize;
    });
    receiver
}

unsafe extern "C" fn init_with_block(receiver: Id, _sel: Sel, block: Id) -> Id {
    if receiver.is_null() {
        return receiver;
    }
    with_instance(receiver, |i| {
        i.executing = false;
        i.block = objc::block::copy(block) as usize;
    });
    receiver
}

/// `-main`: the block, or the target's selector with the object.
unsafe extern "C" fn thread_main(receiver: Id, _sel: Sel) {
    let (target, selector, argument, block) =
        with_instance(receiver, |i| (i.target, i.selector, i.argument, i.block));
    if block != 0 {
        // SAFETY: a copied `void (^)(void)`.
        if let Some(invoke) = unsafe { objc::block::invoke(block as Id) } {
            let f: unsafe extern "C" fn(Id) = unsafe { std::mem::transmute(invoke) };
            unsafe { f(block as Id) };
        }
    } else if target != 0 && selector != 0 {
        // SAFETY: the selector is `void (id, SEL, id)` by NSThread's contract,
        // sent through the dispatcher so forwarding and overrides apply.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            send(target as Id, selector as Sel, argument as Id);
        }
    }
}

/// Mark a thread this crate started as finished and drop what it held, once
/// its `-main` has returned or it has called `+exit`.
fn finish(thread: Id) {
    let (target, argument, block) = with_instance(thread, |i| {
        i.executing = false;
        i.finished = true;
        (
            std::mem::take(&mut i.target),
            std::mem::take(&mut i.argument),
            std::mem::take(&mut i.block),
        )
    });
    objc::arc::release(target as Id);
    objc::arc::release(argument as Id);
    if block != 0 {
        objc::block::release(block as Id);
    }
    objc::arc::release(thread);
}

/// The new thread's start routine. The unwind ABI because `+exit` ends the
/// thread with `pthread_exit`, which glibc implements by unwinding through
/// here; a `std::thread` would abort on that instead.
extern "C-unwind" fn run(object: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    let _darwin = darwin::tls::DarwinThread::enter();
    let thread = object as Id;
    CURRENT.with(|current| current.set(object as usize));
    let pool = objc::arc::pool_push();
    // SAFETY: `-main` is `void (id, SEL)`.
    unsafe {
        let send: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
        send(thread, objc::sel::get("main"));
    }
    objc::arc::pool_pop(pool);
    finish(thread);
    std::ptr::null_mut()
}

/// `-start`: a new thread that sends the receiver `-main`, so a subclass's own
/// `-main` runs, with `+currentThread` answering the receiver there.
unsafe extern "C" fn start(receiver: Id, _sel: Sel) {
    if receiver.is_null() || with_instance(receiver, |i| std::mem::replace(&mut i.started, true)) {
        return;
    }
    let stack_size = with_instance(receiver, |i| {
        i.executing = true;
        i.stack_size
    });
    MULTITHREADED.store(true, Ordering::Relaxed);
    let object = objc::arc::retain(receiver);
    type Start = extern "C" fn(*mut std::ffi::c_void) -> *mut std::ffi::c_void;
    type StartUnwind = extern "C-unwind" fn(*mut std::ffi::c_void) -> *mut std::ffi::c_void;
    // SAFETY: the same signature, differing only in whether it may unwind,
    // which glibc's own forced unwinding needs.
    let routine = unsafe { std::mem::transmute::<StartUnwind, Start>(run) };
    // SAFETY: plain pthread calls; the attributes live until the thread is made.
    let spawned = unsafe {
        let mut attributes: libc::pthread_attr_t = std::mem::zeroed();
        libc::pthread_attr_init(&mut attributes);
        libc::pthread_attr_setdetachstate(&mut attributes, libc::PTHREAD_CREATE_DETACHED);
        // Apple's default for a secondary thread, and enough for a guest that
        // never asked: Rust's own default is smaller.
        let size = match stack_size {
            0 => 512 * 1024,
            size => size as usize,
        };
        libc::pthread_attr_setstacksize(&mut attributes, size.max(libc::PTHREAD_STACK_MIN));
        let mut handle: libc::pthread_t = 0;
        let made = libc::pthread_create(&mut handle, &attributes, routine, object.cast());
        libc::pthread_attr_destroy(&mut attributes);
        made == 0
    };
    if !spawned {
        with_instance(receiver, |i| {
            i.executing = false;
            i.finished = true;
        });
        objc::arc::release(object);
    }
}

unsafe extern "C" fn cancel(receiver: Id, _sel: Sel) {
    with_instance(receiver, |i| i.cancelled = true);
}

unsafe extern "C" fn is_cancelled(receiver: Id, _sel: Sel) -> bool {
    instance_flag(receiver, |i| i.cancelled)
}

unsafe extern "C" fn is_executing(receiver: Id, _sel: Sel) -> bool {
    instance_flag(receiver, |i| i.executing)
}

unsafe extern "C" fn is_finished(receiver: Id, _sel: Sel) -> bool {
    instance_flag(receiver, |i| i.finished)
}

unsafe extern "C" fn name(receiver: Id, _sel: Sel) -> Id {
    with_instance(receiver, |i| i.name) as Id
}

unsafe extern "C" fn set_name(receiver: Id, _sel: Sel, name: Id) {
    let copy = match name.is_null() {
        true => std::ptr::null_mut(),
        // SAFETY: an `NSString`; the property is `copy`.
        false => unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            send(name, objc::sel::get("copy"))
        },
    };
    let old = with_instance(receiver, |i| std::mem::replace(&mut i.name, copy as usize));
    objc::arc::release(old as Id);
}

unsafe extern "C" fn stack_size(receiver: Id, _sel: Sel) -> u64 {
    with_instance(receiver, |i| i.stack_size)
}

/// Apple rounds to a multiple of 4 KiB and ignores it once the thread started.
unsafe extern "C" fn set_stack_size(receiver: Id, _sel: Sel, size: u64) {
    with_instance(receiver, |i| {
        if !i.started {
            i.stack_size = size.div_ceil(4096) * 4096;
        }
    });
}

// FIXME (ACCURACY): priority and quality of service are remembered, not applied
// to the host thread's scheduling.
unsafe extern "C" fn thread_priority(receiver: Id, _sel: Sel) -> f64 {
    with_instance(receiver, |i| i.priority)
}

unsafe extern "C" fn set_thread_priority(receiver: Id, _sel: Sel, priority: f64) {
    with_instance(receiver, |i| i.priority = priority.clamp(0.0, 1.0));
}

unsafe extern "C" fn quality_of_service(receiver: Id, _sel: Sel) -> i64 {
    with_instance(receiver, |i| i.quality)
}

unsafe extern "C" fn set_quality_of_service(receiver: Id, _sel: Sel, quality: i64) {
    with_instance(receiver, |i| i.quality = quality);
}

unsafe extern "C" fn class_thread_priority(_class: Id, _sel: Sel) -> f64 {
    // SAFETY: `+currentThread`'s object.
    unsafe { thread_priority(current(), objc::sel::get("threadPriority")) }
}

unsafe extern "C" fn class_set_thread_priority(_class: Id, _sel: Sel, priority: f64) -> bool {
    // SAFETY: as above.
    unsafe { set_thread_priority(current(), objc::sel::get("setThreadPriority:"), priority) };
    true
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let removed = {
        let mut instances = INSTANCES.lock().unwrap_or_else(|e| e.into_inner());
        instances
            .iter()
            .position(|(o, _)| *o == receiver as usize)
            .map(|index| instances.swap_remove(index).1)
    };
    if let Some(i) = removed {
        objc::arc::release(i.target as Id);
        objc::arc::release(i.argument as Id);
        objc::arc::release(i.name as Id);
        if i.block != 0 {
            objc::block::release(i.block as Id);
        }
    }
    objc::arc::finalize(receiver);
}

/// `+exit`: tell observers, then end the calling thread without returning.
///
/// Nothing with a destructor may be live in this frame: `pthread_exit` ends the
/// thread from under it, as it does for the guest's own start routine. The
/// unwind ABI because glibc's `pthread_exit` force-unwinds through this frame.
unsafe extern "C-unwind" fn exit_thread(_class: Id, _sel: Sel) -> ! {
    // SAFETY: `+currentThread` is `id (Class, SEL)`.
    let thread = unsafe { current_thread(std::ptr::null_mut(), objc::sel::get("currentThread")) };
    crate::notification::post("NSThreadWillExitNotification", thread, std::ptr::null_mut());
    // `-start`'s own bookkeeping after `-main` will not run: say it here.
    if with_instance(thread, |i| i.started) {
        finish(thread);
    }
    // SAFETY: ends this thread; never returns.
    unsafe { libc::pthread_exit(std::ptr::null_mut()) }
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSThread").expect("declared abstract by this crate");
    class_methods!(class, {
        "detachNewThreadSelector:toTarget:withObject:" => detach, c"v40@0:8:16@24@32";
        "sleepForTimeInterval:" => sleep_for_interval, c"v24@0:8d16";
        "isMultiThreaded" => is_multi_threaded, c"B16@0:8";
        "exit" => exit_thread, c"v16@0:8";
        "isMainThread" => is_main_thread, c"B16@0:8";
        "currentThread" => current_thread, c"@16@0:8";
        "mainThread" => main_thread, c"@16@0:8";
        "threadPriority" => class_thread_priority, c"d16@0:8";
        "setThreadPriority:" => class_set_thread_priority, c"B24@0:8d16";
    });
    methods!(class, {
        "isMainThread" => is_main_thread, c"B16@0:8";
        "threadDictionary" => thread_dictionary, c"@16@0:8";
        "init" => init, c"@16@0:8";
        "initWithTarget:selector:object:" => init_with_target, c"@40@0:8@16:24@32";
        "initWithBlock:" => init_with_block, c"@24@0:8@?16";
        "main" => thread_main, c"v16@0:8";
        "start" => start, c"v16@0:8";
        "cancel" => cancel, c"v16@0:8";
        "isCancelled" => is_cancelled, c"B16@0:8";
        "isExecuting" => is_executing, c"B16@0:8";
        "isFinished" => is_finished, c"B16@0:8";
        "name" => name, c"@16@0:8";
        "setName:" => set_name, c"v24@0:8@16";
        "stackSize" => stack_size, c"Q16@0:8";
        "setStackSize:" => set_stack_size, c"v24@0:8Q16";
        "threadPriority" => thread_priority, c"d16@0:8";
        "setThreadPriority:" => set_thread_priority, c"v24@0:8d16";
        "qualityOfService" => quality_of_service, c"q16@0:8";
        "setQualityOfService:" => set_quality_of_service, c"v24@0:8q16";
        "dealloc" => dealloc, c"v16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    static AFTER_EXIT: AtomicUsize = AtomicUsize::new(0);

    extern "C-unwind" fn exits_early(_: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
        // SAFETY: `+exit` takes no arguments and does not return.
        unsafe { exit_thread(std::ptr::null_mut(), objc::sel::get("exit")) };
        #[allow(unreachable_code)]
        {
            AFTER_EXIT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::ptr::null_mut()
        }
    }

    #[test]
    fn exit_ends_the_calling_thread() {
        crate::bootstrap();
        // glibc unwinds the thread through the start routine on exit, which the
        // plain C ABI would abort on.
        type Start = extern "C" fn(*mut std::ffi::c_void) -> *mut std::ffi::c_void;
        type StartUnwind = extern "C-unwind" fn(*mut std::ffi::c_void) -> *mut std::ffi::c_void;
        // SAFETY: the same signature, differing only in whether it may unwind.
        let start = unsafe { std::mem::transmute::<StartUnwind, Start>(exits_early) };
        let mut thread: libc::pthread_t = 0;
        // SAFETY: a plain host thread running a function that exits it.
        unsafe {
            assert_eq!(
                libc::pthread_create(&mut thread, std::ptr::null(), start, std::ptr::null_mut()),
                0
            );
            assert_eq!(libc::pthread_join(thread, std::ptr::null_mut()), 0);
        }
        assert_eq!(AFTER_EXIT.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
    use crate::block::fake;

    static RAN_ON: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn record(_block: Id) {
        // SAFETY: `+currentThread` takes no arguments.
        let thread =
            unsafe { current_thread(std::ptr::null_mut(), objc::sel::get("currentThread")) };
        RAN_ON.store(thread as usize, Ordering::SeqCst);
    }

    #[test]
    fn a_started_thread_runs_its_block_as_the_current_thread_and_finishes() {
        crate::bootstrap();
        let block = fake::block(record as *const std::ffi::c_void);
        let class = objc::class::lookup(c"NSThread").unwrap();
        let thread = objc::object::create_instance(class, 0);
        // SAFETY: a fresh NSThread and a global block.
        unsafe {
            init_with_block(thread, objc::sel::get("initWithBlock:"), block.id());
            assert!(!is_executing(thread, objc::sel::get("isExecuting")));
            set_name(thread, objc::sel::get("setName:"), std::ptr::null_mut());
            start(thread, objc::sel::get("start"));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !unsafe { is_finished(thread, objc::sel::get("isFinished")) } {
            assert!(
                std::time::Instant::now() < deadline,
                "the thread never finished"
            );
            std::thread::yield_now();
        }
        assert_eq!(RAN_ON.load(Ordering::SeqCst), thread as usize);
        assert!(!unsafe { is_executing(thread, objc::sel::get("isExecuting")) });
        objc::arc::release(thread);
    }

    unsafe extern "C" fn exit_from_block(_block: Id) {
        // SAFETY: `+exit` takes no arguments and does not return.
        unsafe { exit_thread(std::ptr::null_mut(), objc::sel::get("exit")) };
    }

    #[test]
    fn a_thread_that_exits_is_finished() {
        crate::bootstrap();
        let block = fake::block(exit_from_block as *const std::ffi::c_void);
        let class = objc::class::lookup(c"NSThread").unwrap();
        let thread = objc::object::create_instance(class, 0);
        objc::arc::retain(thread);
        // SAFETY: a fresh NSThread and a global block.
        unsafe {
            init_with_block(thread, objc::sel::get("initWithBlock:"), block.id());
            start(thread, objc::sel::get("start"));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while unsafe { is_executing(thread, objc::sel::get("isExecuting")) } {
            assert!(
                std::time::Instant::now() < deadline,
                "the thread never stopped"
            );
            std::thread::yield_now();
        }
        assert!(unsafe { is_finished(thread, objc::sel::get("isFinished")) });
        objc::arc::release(thread);
    }
}
