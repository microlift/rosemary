use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(not(target_os = "macos"))]
use crate::malloc::{free as darwin_free, malloc as darwin_malloc};
#[cfg(target_os = "macos")]
use libc::{free as darwin_free, malloc as darwin_malloc};

static MALLOC: AtomicUsize = AtomicUsize::new(0);
static FREE: AtomicUsize = AtomicUsize::new(0);

/// Use `malloc` and `free` for blocks that cross to or from the guest.
///
/// Both must be callable directly from this process, which is true of the
/// native backend - where the guest's code *is* host code - and not of the
/// emulated one, where the guest's allocator is x86-64 that needs the run loop.
/// Nothing registers there, and Darwin's own `malloc` and `free` are used: the
/// zones of [`crate::malloc`] off Darwin, the host's on it.
pub fn register(malloc: usize, free: usize) {
    MALLOC.store(malloc, Ordering::Release);
    FREE.store(free, Ordering::Release);
}

/// Whether a guest allocator has been registered.
pub fn is_registered() -> bool {
    MALLOC.load(Ordering::Acquire) != 0 && FREE.load(Ordering::Acquire) != 0
}

/// `size` bytes from the allocator the guest also frees with.
///
/// # Safety
///
/// As `malloc`: the block is uninitialized, and null on failure.
pub unsafe fn malloc(size: usize) -> *mut c_void {
    let f = MALLOC.load(Ordering::Acquire);
    if f == 0 {
        // SAFETY: Darwin's `malloc`, with the caller's size.
        return unsafe { darwin_malloc(size) };
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
        // SAFETY: the caller's obligation. With nothing registered, Darwin's
        // `free` finds the zone that owns the block, whichever made it.
        return unsafe { darwin_free(block) };
    }
    // SAFETY: [`register`]'s contract - an address of `void(void *)` this
    // process can call.
    let free: unsafe extern "C" fn(*mut c_void) = unsafe { core::mem::transmute(f) };
    unsafe { free(block) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The emulated backend never reaches [`register`], so the unregistered
    /// path is a common one and has to work.
    #[test]
    fn without_a_guest_allocator_darwins_own_is_used() {
        // SAFETY: a size this test asks for and immediately frees.
        unsafe {
            let block = malloc(48);
            assert!(!block.is_null());
            block.cast::<u8>().write(7);
            free(block);
        }
    }

    #[test]
    fn a_registered_allocator_is_the_one_that_is_called() {
        use std::sync::atomic::AtomicU32;

        static FREED: AtomicU32 = AtomicU32::new(0);

        unsafe extern "C" fn counting_free(block: *mut c_void) {
            FREED.fetch_add(1, Ordering::Relaxed);
            // SAFETY: a block Darwin's `malloc` returned.
            unsafe { darwin_free(block) };
        }

        // Darwin's own pair, so that a block another test allocates on one
        // side of the switch is still freed by the allocator that made it.
        let (was_malloc, was_free) = (MALLOC.load(Ordering::Acquire), FREE.load(Ordering::Acquire));
        register(
            darwin_malloc as *const () as usize,
            counting_free as *const () as usize,
        );
        assert!(is_registered());
        // SAFETY: a size this test asks for and immediately frees.
        unsafe { free(malloc(16)) };
        assert_eq!(FREED.load(Ordering::Relaxed), 1);
        register(was_malloc, was_free);
    }
}
