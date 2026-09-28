use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

static MALLOC: AtomicUsize = AtomicUsize::new(0);
static FREE: AtomicUsize = AtomicUsize::new(0);
static OWNS: AtomicUsize = AtomicUsize::new(0);

/// Use `malloc` and `free` for blocks that cross to or from the guest.
///
/// Both must be callable directly from this process, which is true of the
/// native backend - where the guest's code *is* host code - and not of the
/// emulated one, where the guest's allocator is x86-64 that needs the run loop.
/// Nothing registers there, and the host's own is used.
///
/// `owns` is `bool (*)(const void *)` - whether a pointer is one of the live
/// blocks `malloc` above returns - and `0` for **no such question can be
/// asked**. That is not a detail: an allocator that can say which pointers are
/// its own is what lets a caller be told "not a block" instead of being handed
/// to a `malloc_usable_size` that faults on a pointer from outside the heap.
/// See [`owns`] for what the two answers mean.
///
/// It is registered here, beside the allocator it describes, rather than
/// named in the callers: `darwin` is allocator-agnostic and this is the one
/// place that already is not.
pub fn register(malloc: usize, free: usize, owns: usize) {
    MALLOC.store(malloc, Ordering::Release);
    FREE.store(free, Ordering::Release);
    OWNS.store(owns, Ordering::Release);
}

/// Whether a guest allocator has been registered.
pub fn is_registered() -> bool {
    MALLOC.load(Ordering::Acquire) != 0 && FREE.load(Ordering::Acquire) != 0
}

/// The lock a test takes for as long as it has a registration installed.
///
/// **Test-only, and not for tidiness.** [`register`] writes three
/// process-global statics, so two tests that install one are deciding each
/// other's answers however the runner happens to schedule them - and a test
/// that can do that can pass for reasons that have nothing to do with what it
/// checks. Public so `malloc`'s tests, which install a registration of their
/// own, take the same one.
#[cfg(test)]
pub static ONE_REGISTRATION_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The current registration, as `(malloc, free, owns)` - the same three
/// arguments [`register`] takes, and `0` for each of them that is unset.
///
/// So that a caller in another module can put back what it found. The three are
/// process-global, and a test that installs a registration and does not restore
/// it decides the answer for every other test in the binary - which is a test
/// that passes for reasons that have nothing to do with what it checks.
pub fn registration() -> (usize, usize, usize) {
    (
        MALLOC.load(Ordering::Acquire),
        FREE.load(Ordering::Acquire),
        OWNS.load(Ordering::Acquire),
    )
}

/// Whether `ptr` is a live block of the allocator [`register`] was given.
///
/// Three answers, and the third is not the same as `Some(false)`:
///
/// - `Some(true)` - the registered predicate says it is a live block.
/// - `Some(false)` - it is not one. This is a real, positive statement from
///   the allocator, and it is what lets a caller answer Darwin's "no such
///   block" with the same zero Darwin answers rather than a fault.
/// - `None` - **no predicate is registered, and nothing is claimed about
///   `ptr`.** Either nothing was registered at all - the emulated backend, and
///   a program that never adopts a guest allocator, where there is one
///   allocator and it is the host's - or the one that registered deliberately
///   declined, which it must have said out loud. Collapsing this into
///   `Some(false)` would report "not a block" for every block, and
///   `Some(true)` would report "is a block" for every pointer including null.
///
/// A predicate is not required to have an answer for a pointer outside its
/// heap. mimalloc's `mi_is_in_heap_region` is the shape this is written
/// against: it is defined for **any** pointer and answers false for one it does
/// not own, rather than being undefined on it. A predicate that can fault on a
/// foreign pointer does not belong in this slot.
pub fn owns(ptr: *const c_void) -> Option<bool> {
    let f = OWNS.load(Ordering::Acquire);
    if f == 0 {
        return None;
    }
    // SAFETY: [`register`]'s contract - an address of `bool(const void *)`
    // this process can call, and defined for any pointer.
    let owns: unsafe extern "C" fn(*const c_void) -> bool =
        unsafe { core::mem::transmute(f) };
    // SAFETY: as above; the predicate is defined for every pointer.
    Some(unsafe { owns(ptr) })
}

/// `size` bytes from the allocator the guest also frees with.
///
/// # Safety
///
/// As `malloc`: the block is uninitialized, and null on failure.
pub unsafe fn malloc(size: usize) -> *mut c_void {
    let f = MALLOC.load(Ordering::Acquire);
    if f == 0 {
        // SAFETY: the host's own, with the caller's size.
        return unsafe { libc::malloc(size) };
    }
    // SAFETY: [`register`]'s contract - an address of `void *(size_t)` this
    // process can call.
    let malloc: unsafe extern "C" fn(usize) -> *mut c_void = unsafe { core::mem::transmute(f) };
    unsafe { malloc(size) }
}

/// Free a block the guest allocated, or one [`malloc`] returned.
///
/// # Safety
///
/// `block` came from the guest's allocator - which is what the guest promised
/// when it said `DISPATCH_DATA_DESTRUCTOR_FREE` - and is not freed twice.
pub unsafe fn free(block: *mut c_void) {
    let f = FREE.load(Ordering::Acquire);
    if f == 0 {
        // SAFETY: the caller's obligation. With nothing registered there is one
        // allocator and it is the host's.
        return unsafe { libc::free(block) };
    }
    // SAFETY: [`register`]'s contract - an address of `void(void *)` this
    // process can call.
    let free: unsafe extern "C" fn(*mut c_void) = unsafe { core::mem::transmute(f) };
    unsafe { free(block) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Most programs have one allocator and never reach [`register`], so the
    /// unregistered path is the common one and has to work.
    #[test]
    fn without_a_guest_allocator_the_hosts_own_is_used() {
        // SAFETY: a size this test asks for and immediately frees.
        unsafe {
            let block = malloc(48);
            assert!(!block.is_null());
            block.cast::<u8>().write(7);
            free(block);
        }
    }

    /// The three registered values are process-global, and a test that changes
    /// one decides the answer for every other test running beside it. The
    /// parallel runner is exactly that case, so the tests that install a
    /// registration take [`ONE_REGISTRATION_AT_A_TIME`].
    ///
    /// The one address [`owns_everything_but`] refuses.
    ///
    /// A leaked one-byte box rather than a local: the address has to be a real,
    /// mapped, non-null address that is certainly not a heap block, and it has
    /// to still be that after the test returns. **One address for the whole
    /// binary, not one per call** - a fresh leak each time would hand back a
    /// different address every call, and the predicate would answer `true` to
    /// everything.
    fn refused() -> *const c_void {
        static REFUSED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        *REFUSED.get_or_init(|| Box::leak(Box::new(0u8)) as *const u8 as usize) as *const c_void
    }

    /// `bool(const void *)`: true for everything except [`refused`].
    ///
    /// Narrow on purpose. A predicate that answered `true` for everything would
    /// make every other test in this binary pass whatever the caller did with
    /// the answer, and one that answered `false` for everything would make them
    /// all fail; one address is enough to be a control and cannot quietly
    /// become a blanket.
    unsafe extern "C" fn owns_everything_but(p: *const c_void) -> bool {
        p != refused()
    }

    /// A registration, put back when the returned guard drops.
    ///
    /// Holds [`ONE_REGISTRATION_AT_A_TIME`] for as long as it is installed, so
    /// two of these never overlap however the runner schedules them.
    struct Registration {
        _one_at_a_time: std::sync::MutexGuard<'static, ()>,
        was: (usize, usize, usize),
    }

    impl Registration {
        fn install(malloc: usize, free: usize, owns: usize) -> Self {
            let one_at_a_time = ONE_REGISTRATION_AT_A_TIME
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let guard = Self {
                was: registration(),
                _one_at_a_time: one_at_a_time,
            };
            register(malloc, free, owns);
            guard
        }

        /// The host's own allocator with a predicate that refuses one address.
        fn with_predicate() -> Self {
            Self::install(
                libc::malloc as *const () as usize,
                libc::free as *const () as usize,
                owns_everything_but as *const () as usize,
            )
        }
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            register(self.was.0, self.was.1, self.was.2);
        }
    }

    #[test]
    fn a_registered_allocator_is_the_one_that_is_called() {
        use std::sync::atomic::AtomicU32;

        static FREED: AtomicU32 = AtomicU32::new(0);

        unsafe extern "C" fn counting_free(block: *mut c_void) {
            FREED.fetch_add(1, Ordering::Relaxed);
            // SAFETY: a block the host's own `malloc` returned.
            unsafe { libc::free(block) };
        }

        let _guard = Registration::install(
            libc::malloc as *const () as usize,
            counting_free as *const () as usize,
            owns_everything_but as *const () as usize,
        );
        assert!(is_registered());
        // SAFETY: a size this test asks for and immediately frees.
        unsafe { free(malloc(16)) };
        assert_eq!(FREED.load(Ordering::Relaxed), 1);
    }

    /// The registered predicate is the one that answers, and its "no" comes back
    /// as `Some(false)` rather than as the "no predicate" answer.
    #[test]
    fn the_registered_predicate_is_the_one_that_answers() {
        let _guard = Registration::with_predicate();
        // SAFETY: a size this test asks for and immediately frees.
        let block = unsafe { malloc(32) };
        assert_eq!(owns(block), Some(true));
        assert_eq!(owns(refused()), Some(false));
        // SAFETY: `block` came from `malloc` and is not used again.
        unsafe { free(block) };
    }

    /// With nothing registered the answer is `None` - **not** `Some(false)`.
    ///
    /// The two are different facts and conflating them is the hazard this
    /// function exists to avoid: a `None` read as `false` reports every live
    /// block as not one, and a `None` read as `true` reports a foreign pointer
    /// as one this host may hand to its own allocator.
    #[test]
    fn with_no_predicate_registered_the_answer_is_unknown_and_not_no() {
        let _guard = Registration::install(0, 0, 0);
        assert_eq!(owns(std::ptr::null()), None);
        assert_eq!(owns(refused()), None);
    }
}
