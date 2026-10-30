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

/// The calling thread's `NSThread`, made the first time it is asked for.
///
/// Never released: a thread's object outlives every reference a program could
/// have kept to it, and nothing here learns when a foreign thread ends.
fn current() -> Id {
    thread_local! {
        static CURRENT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
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

/// What an `NSThread` holds besides its dictionary, beside the object for the
/// same reason [`thread_dictionary`] is.
struct Attributes {
    /// `threadPriority`, 0.0 to 1.0. This host's default scheduler has no
    /// static priorities for an unprivileged process to map the range onto, so
    /// the value is the thread's to read back rather than a scheduling change.
    priority: f64,
    /// `name`, a retained copy, or null.
    name: usize,
    /// `qualityOfService`, an `NSQualityOfService`.
    quality: i64,
}

impl Default for Attributes {
    fn default() -> Attributes {
        Attributes {
            // The documented default.
            priority: 0.5,
            name: 0,
            // `NSQualityOfServiceDefault`.
            quality: -1,
        }
    }
}

fn with_attributes<R>(thread: Id, f: impl FnOnce(&mut Attributes) -> R) -> R {
    static ATTRIBUTES: std::sync::Mutex<Vec<(usize, Attributes)>> =
        std::sync::Mutex::new(Vec::new());
    let mut all = ATTRIBUTES.lock().unwrap_or_else(|e| e.into_inner());
    let index = match all.iter().position(|(t, _)| *t == thread as usize) {
        Some(index) => index,
        None => {
            all.push((thread as usize, Attributes::default()));
            all.len() - 1
        }
    };
    f(&mut all[index].1)
}

unsafe extern "C" fn thread_priority(receiver: Id, _sel: Sel) -> f64 {
    with_attributes(receiver, |a| a.priority)
}

unsafe extern "C" fn set_thread_priority(receiver: Id, _sel: Sel, priority: f64) {
    with_attributes(receiver, |a| a.priority = priority.clamp(0.0, 1.0));
}

/// `+threadPriority`: the calling thread's.
unsafe extern "C" fn current_priority(_class: Id, _sel: Sel) -> f64 {
    with_attributes(current(), |a| a.priority)
}

/// `+setThreadPriority:`, which answers whether the assignment took.
unsafe extern "C" fn set_current_priority(_class: Id, _sel: Sel, priority: f64) -> bool {
    with_attributes(current(), |a| a.priority = priority.clamp(0.0, 1.0));
    true
}

unsafe extern "C" fn name(receiver: Id, _sel: Sel) -> Id {
    with_attributes(receiver, |a| a.name as Id)
}

/// `-setName:`, a copy. Naming the calling thread names it to the system too,
/// as Foundation does with `pthread_setname_np`, so a debugger or crash report
/// shows it.
unsafe extern "C" fn set_name(receiver: Id, _sel: Sel, name: Id) {
    // SAFETY: the caller's string, or nil.
    let text =
        unsafe { corefoundation::string::as_string(name as corefoundation::base::CFTypeRef) }
            .map(|s| String::from_utf16_lossy(&s.utf16()));
    let copy = match &text {
        Some(text) => crate::string::from_str(text),
        None => objc::arc::retain(name),
    };
    let old = with_attributes(receiver, |a| std::mem::replace(&mut a.name, copy as usize));
    objc::arc::release(old as Id);
    if receiver != current() {
        return;
    }
    if let Some(text) = text {
        // The host keeps fifteen bytes and a NUL; cut on a character boundary.
        let mut end = text.len().min(15);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if let Ok(c) = std::ffi::CString::new(&text[..end]) {
            // SAFETY: the calling thread and a NUL-terminated name.
            unsafe { libc::pthread_setname_np(libc::pthread_self(), c.as_ptr()) };
        }
    }
}

unsafe extern "C" fn quality_of_service(receiver: Id, _sel: Sel) -> i64 {
    with_attributes(receiver, |a| a.quality)
}

unsafe extern "C" fn set_quality_of_service(receiver: Id, _sel: Sel, quality: i64) {
    with_attributes(receiver, |a| a.quality = quality);
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSThread").expect("declared abstract by this crate");
    class_methods!(class, {
        "detachNewThreadSelector:toTarget:withObject:" => detach, c"v40@0:8:16@24@32";
        "sleepForTimeInterval:" => sleep_for_interval, c"v24@0:8d16";
        "isMultiThreaded" => is_multi_threaded, c"B16@0:8";
        "isMainThread" => is_main_thread, c"B16@0:8";
        "currentThread" => current_thread, c"@16@0:8";
        "mainThread" => main_thread, c"@16@0:8";
        "threadPriority" => current_priority, c"d16@0:8";
        "setThreadPriority:" => set_current_priority, c"B24@0:8d16";
    });
    methods!(class, {
        "isMainThread" => is_main_thread, c"B16@0:8";
        "threadDictionary" => thread_dictionary, c"@16@0:8";
        "threadPriority" => thread_priority, c"d16@0:8";
        "setThreadPriority:" => set_thread_priority, c"v24@0:8d16";
        "name" => name, c"@16@0:8";
        "setName:" => set_name, c"v24@0:8@16";
        "qualityOfService" => quality_of_service, c"q16@0:8";
        "setQualityOfService:" => set_quality_of_service, c"v24@0:8q16";
    });
}
