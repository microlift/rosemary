use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use objc::object::Id;
use objc::sel::Sel;

use crate::define::class_methods;
use crate::define::methods;

/// Set once a thread has been detached, so `+isMultiThreaded` can answer YES
/// for a guest that used `+detachNewThreadSelector:toTarget:withObject:`.
static DETACHED: AtomicBool = AtomicBool::new(false);

/// How many threads are currently registered, which is what `+isMultiThreaded`
/// reports on Apple: YES as soon as more than one thread exists.
///
/// Counted here rather than assumed from the host, because a guest that made
/// its own threads with `pthread_create` is genuinely multithreaded and Apple
/// answers YES for it. Answering NO would be a plausible-looking lie that
/// invites the guest down a single-threaded code path.
static LIVE: AtomicUsize = AtomicUsize::new(1);

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
    DETACHED.store(true, Ordering::Relaxed);
    LIVE.fetch_add(1, Ordering::Relaxed);
    let detached = Detached {
        target: objc::arc::retain(target) as usize,
        object: objc::arc::retain(object) as usize,
        selector: selector as usize,
    };
    std::thread::spawn(move || {
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
        LIVE.fetch_sub(1, Ordering::Relaxed);
    });
}

unsafe extern "C" fn sleep_for_interval(_class: Id, _sel: Sel, seconds: f64) {
    if seconds > 0.0 {
        std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
    }
}

unsafe extern "C" fn is_multi_threaded(_class: Id, _sel: Sel) -> bool {
    // A detached thread that has already finished still leaves the process
    // multithreaded for as long as it existed, so OR the two: `LIVE` alone
    // would fall back to NO once the detached thread exits.
    LIVE.load(Ordering::Relaxed) > 1 || DETACHED.load(Ordering::Relaxed)
}

// One `NSThread` per calling thread, which is the whole contract of
// `+[NSThread currentThread]`: the same object every time on one thread, and
// a different one on every other.
//
// Keyed on the *Linux tid* from `darwin::pthread::id()` and deliberately not
// on a mach port name. This tree has two disjoint thread namespaces by
// construction, and Chromium's start-up demonstrably uses the pthread one
// (`pthread_mach_thread_np(pthread_self())` in the same function), so the
// pthread tid is the identifier a guest can actually correlate with.
thread_local! {
    static CURRENT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The calling thread's `NSThread`, created on first use.
///
/// Retained for the life of the process and cached per thread. Apple also
/// keeps one object per thread alive, so handing back the same pointer is
/// truthful rather than a convenience: a guest is entitled to compare
/// `currentThread` across two calls and get equality.
fn current(class: &'static objc::class::Class) -> Id {
    CURRENT.with(|slot| {
        let existing = slot.get();
        if existing != 0 {
            return existing as Id;
        }
        let object = objc::object::create_instance(class, 0);
        objc::arc::retain(object);
        slot.set(object as usize);
        object
    })
}

/// The tid the process started on, which is the only thread that is the main
/// one. Captured at first use rather than at load time: under `dlopen` the
/// runtime's own start-up may already have run on the main thread, and the
/// first guest call is guaranteed to be a real guest thread.
/// The tid the process started on, which is the only thread that is the main one.
///
/// This was first-caller-wins, and that is a real defect rather than a harmless
/// shortcut: it makes the answer depend on **who asked first**, so a program
/// that touches `+[NSThread isMainThread]` from a helper thread before `main`
/// runs is told the helper is the main thread, and everything keyed off that -
/// which run loop is "current", which queue is the main queue - is wrong from
/// then on. Chromium starts threads early.
///
/// `std::process::id()` is the kernel's own answer rather than a guess about
/// arrival order: on Linux a process's initial thread has a tid equal to its
/// pid, so this is the main thread by definition and not by observation. It also
/// agrees with `corefoundation::runloop`'s main loop, which is keyed the same
/// way - the two used to be able to disagree, which is the same bug one level
/// up.
fn main_tid() -> u32 {
    std::process::id()
}

unsafe extern "C" fn is_main_thread(_class: Id, _sel: Sel) -> bool {
    darwin::pthread::id() == main_tid()
}

unsafe extern "C" fn current_thread(class: Id, _sel: Sel) -> Id {
    // SAFETY: `class` is `NSThread`, which this crate registered.
    let class = match unsafe { objc::class::from_ptr(class as *const objc::class::Class) } {
        Some(class) => class,
        None => objc::fail!("+[NSThread currentThread] was sent to nil"),
    };
    current(class)
}

// The per-thread `NSMutableDictionary` behind `-threadDictionary`.
//
// Chromium's start-up sends `setObject:forKeyedSubscript:` to the result
// (verified at `Framework+0x5d9b037`, selector resolved from the binary), so
// this has to be a *real* mutable dictionary, not an empty stand-in: a
// dictionary that silently discards the write would leave the guest believing
// it had stored something it had not.
thread_local! {
    static DICTIONARY: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

unsafe extern "C" fn thread_dictionary(_self: Id, _sel: Sel) -> Id {
    let class = objc::class::lookup(c"NSMutableDictionary")
        .expect("CoreFoundation registered NSMutableDictionary");
    DICTIONARY.with(|slot| {
        let existing = slot.get();
        if existing != 0 {
            return existing as Id;
        }
        // +[NSMutableDictionary dictionary] is the allocating entry point and
        // is already implemented in `collections`, so this is the same object
        // a guest would get by asking for it directly.
        let selector = objc::sel::get("dictionary");
        // SAFETY: the class is registered and `dictionary` is installed on its
        // metaclass by `collections::install`.
        let Some(method) = class.metaclass().method(selector) else {
            objc::fail!("+[NSMutableDictionary dictionary] is not implemented");
        };
        // SAFETY: `+dictionary` takes no arguments and returns a retained object.
        let object: Id = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(method.implementation());
            f(objc::class::ptr(class) as Id, selector)
        };
        if object.is_null() {
            objc::fail!("+[NSMutableDictionary dictionary] returned nil");
        }
        slot.set(object as usize);
        object
    })
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSThread").expect("declared abstract by this crate");
    methods!(class, {
        "threadDictionary" => thread_dictionary, c"@16@0:8";
    });
    class_methods!(class, {
        "currentThread" => current_thread, c"@16@0:8";
        "isMainThread" => is_main_thread, c"B16@0:8";
        "detachNewThreadSelector:toTarget:withObject:" => detach, c"v40@0:8:16@24@32";
        "sleepForTimeInterval:" => sleep_for_interval, c"v24@0:8d16";
        "isMultiThreaded" => is_multi_threaded, c"B16@0:8";
    });
}
