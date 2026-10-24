use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// Which object an allocation is, so a handle can be turned back into its type.
///
/// Checked rather than trusted: a guest that passes a semaphore to a queue call
/// would otherwise have its bytes reinterpreted as a queue.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Kind {
    Queue,
    Group,
    Semaphore,
    Data,
    Source,
}

/// The header every dispatch object begins with.
///
/// `#[repr(C)]` and first in each object, so a `*mut Header` recovered from a guest's
/// handle reads the right words whichever object it names.
#[repr(C)]
pub struct Header {
    /// Where Darwin keeps the Objective-C `isa`, since its dispatch objects are
    /// also Objective-C objects. It has to be a real class of this runtime's:
    /// ARC emits `objc_storeStrong` and friends for a `dispatch_queue_t`, and
    /// the Objective-C runtime cannot tell an object with a null `isa` from a
    /// pointer into zeroed memory. `OS_object` has no methods, so a message
    /// actually sent to one of these still fails - by selector rather than by
    /// misreading the fields below.
    isa: u64,
    kind: Kind,
    refs: AtomicUsize,
    /// The process owns it, as it does the main and global queues: Darwin
    /// ignores retains and releases of those, and ARC sends plenty.
    global: AtomicBool,
    context: AtomicU64,
    finalizer: AtomicU64,
}

impl Header {
    pub fn new(kind: Kind) -> Header {
        objc::arc::set_os_object_refcount(objc::arc::OsObjectRefcount {
            retain: |object| retain(object as u64),
            // SAFETY: ARC releases only what it holds a reference to.
            release: |object| unsafe { release(object as u64) },
            count: |object| retain_count(object as u64),
        });
        Header {
            isa: objc::class::ptr(objc::class::os_object_class()) as u64,
            kind,
            refs: AtomicUsize::new(1),
            global: AtomicBool::new(false),
            context: AtomicU64::new(0),
            finalizer: AtomicU64::new(0),
        }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }
}

/// Mark `handle` as the process's own, so no release can free it.
///
/// # Safety
///
/// As [`header`].
pub unsafe fn make_global(handle: u64) {
    if let Some(h) = unsafe { header(handle) } {
        h.global.store(true, Ordering::Release);
    }
}

/// A `dispatch_function_t`: what a finalizer and the `_f` entry points take.
pub type Function = unsafe extern "C" fn(*mut c_void);

/// The header a guest's handle names, or `None` if it names nothing usable.
///
/// # Safety
///
/// `handle` is null or an object this module allocated.
pub unsafe fn header<'a>(handle: u64) -> Option<&'a Header> {
    match handle {
        0 => None,
        // SAFETY: the caller's guarantee; every object begins with a `Header`.
        _ => Some(unsafe { &*(handle as *const Header) }),
    }
}

pub fn is_own(_handle: u64) -> bool {
    true
}

/// The header a guest's handle names, if it is of `kind`.
///
/// # Safety
///
/// As [`header`].
pub unsafe fn typed<'a>(handle: u64, kind: Kind) -> Option<&'a Header> {
    unsafe { header(handle) }.filter(|h| h.kind == kind)
}

/// Take one more reference. Returns the handle, as `dispatch_retain` does not.
pub fn retain(handle: u64) {
    // SAFETY: a live object, by the caller of the entry point above this.
    if let Some(h) = unsafe { header(handle) }
        && !h.global.load(Ordering::Acquire)
    {
        h.refs.fetch_add(1, Ordering::Relaxed);
    }
}

/// The reference count, as `-retainCount` reports it.
pub fn retain_count(handle: u64) -> usize {
    // SAFETY: a live object, by the caller of the entry point above this.
    match unsafe { header(handle) } {
        Some(h) if h.global.load(Ordering::Acquire) => usize::MAX,
        Some(h) => h.refs.load(Ordering::Relaxed),
        None => 0,
    }
}

/// Drop one reference, running the finalizer and freeing at zero.
///
/// # Safety
///
/// `handle` is null or a live object of this module's, and the caller is giving up a
/// reference it holds.
pub unsafe fn release(handle: u64) {
    let Some(h) = (unsafe { header(handle) }) else {
        return;
    };
    if h.global.load(Ordering::Acquire) {
        return;
    }
    if h.refs.fetch_sub(1, Ordering::AcqRel) != 1 {
        return;
    }
    let (finalizer, context) = (
        h.finalizer.load(Ordering::Acquire),
        h.context.load(Ordering::Acquire),
    );
    let target = super::queue::target(handle);
    let target = if target == 0 {
        unsafe { super::queue::get_global_queue(0, 0) }
    } else {
        target
    };
    retain(target);
    let kind = h.kind;
    // SAFETY: the refcount reached zero, so this reference is the only one and the
    // allocation is this module's own. Each arm names the type it was boxed as.
    unsafe {
        match kind {
            Kind::Queue => drop(Box::from_raw(handle as *mut super::queue::Queue)),
            Kind::Group => drop(Box::from_raw(handle as *mut super::sync::Group)),
            Kind::Semaphore => drop(Box::from_raw(handle as *mut super::sync::Semaphore)),
            Kind::Data => drop(Box::from_raw(handle as *mut super::data::Data)),
            Kind::Source => drop(Box::from_raw(handle as *mut super::source::Source)),
        }
    }
    if finalizer != 0 && context != 0 {
        unsafe {
            super::queue::async_f(target, context, finalizer);
        }
    }
    unsafe {
        release(target);
    }
}

/// `void dispatch_retain(dispatch_object_t)`.
pub unsafe extern "C" fn retain_entry(handle: u64) {
    retain(handle);
}

/// `void dispatch_release(dispatch_object_t)`.
pub unsafe extern "C" fn release_entry(handle: u64) {
    // SAFETY: the guest is giving up a reference it holds.
    unsafe { release(handle) };
}

/// `void dispatch_set_context(dispatch_object_t, void *)`.
pub unsafe extern "C" fn set_context(handle: u64, context: *mut c_void) {
    // SAFETY: a live object.
    if let Some(h) = unsafe { header(handle) } {
        h.context.store(context as u64, Ordering::Release);
    }
}

/// `void *dispatch_get_context(dispatch_object_t)`.
pub unsafe extern "C" fn get_context(handle: u64) -> *mut c_void {
    // SAFETY: a live object.
    match unsafe { header(handle) } {
        Some(h) => h.context.load(Ordering::Acquire) as *mut c_void,
        None => std::ptr::null_mut(),
    }
}

/// `void dispatch_set_finalizer_f(dispatch_object_t, dispatch_function_t)`.
pub unsafe extern "C" fn set_finalizer_f(handle: u64, finalizer: u64) {
    // SAFETY: a live object.
    if let Some(h) = unsafe { header(handle) } {
        h.finalizer.store(finalizer, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    fn wait_finalized(value: &AtomicU32) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while value.load(Ordering::Acquire) == 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    unsafe extern "C" fn finalize(context: *mut c_void) {
        unsafe {
            (*(context as *const AtomicU32)).store(1, Ordering::Release);
        }
    }

    /// The finalizer runs exactly once, at the last release, with the guest's own
    /// context - which is what a guest frees its own state in.
    #[test]
    fn the_last_release_finalizes() {
        let finalized = AtomicU32::new(0);
        let queue = super::super::queue::create_serial("finalizer test");
        // SAFETY: a live object of this module's.
        unsafe {
            set_context(queue, (&finalized as *const AtomicU32).cast_mut().cast());
            set_finalizer_f(queue, finalize as *const () as usize as u64);
            retain(queue);
            release(queue);
            assert_eq!(finalized.load(Ordering::Acquire), 0, "still referenced");
            release(queue);
        }
        wait_finalized(&finalized);
    }

    /// ARC reaches these objects - a guest's `dispatch_queue_t` in a strong
    /// variable is an `objc_storeStrong` - and the Objective-C runtime reads
    /// the first word to decide whether it has been handed an object at all.
    #[test]
    fn a_dispatch_object_wears_a_class_the_objc_runtime_knows() {
        let queue = super::super::queue::create_serial("isa test");
        // SAFETY: a live object of this module's.
        unsafe {
            let isa = header(queue).expect("header").isa;
            assert!(objc::class::is_known(isa as *const objc::class::Class));
            release(queue);
        }
    }

    /// ARC's retains and releases land on the same count `dispatch_release`
    /// reads, so a queue held by both survives until both let go.
    #[test]
    fn arc_and_dispatch_share_one_count() {
        let finalized = AtomicU32::new(0);
        let queue = super::super::queue::create_serial("shared count test");
        // SAFETY: a live object of this module's.
        unsafe {
            set_context(queue, (&finalized as *const AtomicU32).cast_mut().cast());
            set_finalizer_f(queue, finalize as *const () as usize as u64);
            objc::arc::retain(queue as objc::object::Id);
            assert_eq!(retain_count(queue), 2);
            release(queue);
            assert_eq!(finalized.load(Ordering::Acquire), 0, "ARC still holds it");
            objc::arc::release(queue as objc::object::Id);
        }
        wait_finalized(&finalized);
    }

    /// ARC balances `dispatch_get_main_queue()` with an `objc_release` the
    /// guest never retained for, which Darwin ignores.
    #[test]
    fn releasing_the_main_queue_leaves_it_alive() {
        let main = super::super::queue::main_queue();
        for _ in 0..3 {
            objc::arc::release(main as objc::object::Id);
            // SAFETY: the main queue is never freed.
            unsafe { release(main) };
        }
        // SAFETY: as above.
        assert!(unsafe { typed(main, Kind::Queue) }.is_some());
        assert_eq!(retain_count(main), usize::MAX);
    }

    /// A handle of the wrong kind must be refused rather than reinterpreted.
    #[test]
    fn a_handle_carries_its_kind() {
        let queue = super::super::queue::create_serial("kind test");
        // SAFETY: a live object.
        unsafe {
            assert!(typed(queue, Kind::Queue).is_some());
            assert!(typed(queue, Kind::Semaphore).is_none());
            assert!(typed(0, Kind::Queue).is_none());
            release(queue);
        }
    }
}
