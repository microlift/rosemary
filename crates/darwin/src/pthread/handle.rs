//! The `pthread_t` a guest holds.
//!
//! A Darwin `pthread_t` points at a `struct _opaque_pthread_t`, and guest code
//! writes into it inline: `pthread_cleanup_push` is a macro that stores its
//! handler record at offset 8, `__cleanup_stack`. The host's own `pthread_t` is
//! glibc's thread control block, whose offset 8 is the DTV pointer, so handing
//! that out lets a cleanup handler replace it - and the next dynamic TLS access
//! on the thread (a GL driver's, say) reads through a stack frame. Each host
//! thread therefore gets a block of Darwin's size for the guest to hold, and
//! every call that takes a `pthread_t` maps it back first.

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    /// `sizeof(struct _opaque_pthread_t)`, in words.
    const WORDS: usize = 8192 / 8;

    /// `_PTHREAD_SIG`, which `__sig` holds for a live thread.
    const SIG: u64 = 0x5448_5244;

    /// `__cleanup_stack`, the word the cleanup macros write.
    const CLEANUP_STACK: usize = 1;

    #[derive(Default)]
    struct Maps {
        /// Host `pthread_t` to the block issued for it.
        by_host: HashMap<u64, u64>,
        /// The reverse.
        by_guest: HashMap<u64, u64>,
    }

    static MAPS: LazyLock<Mutex<Maps>> = LazyLock::new(Mutex::default);

    thread_local! {
        /// This thread's block, so `pthread_self` takes no lock once it is known:
        /// a guest calls it from signal handlers too.
        static SELF: Cell<u64> = const { Cell::new(0) };
    }

    /// The block standing for `host`, issued on first sight.
    ///
    /// Blocks are never freed. glibc reuses a finished thread's control block for
    /// a new thread, so the set of hosts - and of blocks - stays as small as the
    /// number of threads ever alive at once.
    pub fn for_host(host: u64) -> u64 {
        let mut maps = MAPS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&guest) = maps.by_host.get(&host) {
            return guest;
        }
        let mut block = vec![0u64; WORDS].into_boxed_slice();
        block[0] = SIG;
        let guest = Box::leak(block).as_mut_ptr() as u64;
        maps.by_host.insert(host, guest);
        maps.by_guest.insert(guest, host);
        guest
    }

    /// The host thread `guest` stands for. A value this module never issued is
    /// taken to be a host handle already, which is what one from before this
    /// module existed - or from a host library - is.
    pub fn host_of(guest: u64) -> u64 {
        let maps = MAPS.lock().unwrap_or_else(|e| e.into_inner());
        maps.by_guest.get(&guest).copied().unwrap_or(guest)
    }

    /// The calling thread's block.
    pub fn current() -> u64 {
        let cached = SELF.with(Cell::get);
        if cached != 0 {
            return cached;
        }
        // SAFETY: always callable; the handle is pointer-sized.
        let host: u64 = unsafe { core::mem::transmute_copy(&libc::pthread_self()) };
        let guest = for_host(host);
        SELF.with(|s| s.set(guest));
        guest
    }

    /// Start the calling thread with a clean block. A reused block still holds
    /// the last thread's handler chain if that thread exited inside one.
    pub fn begin() {
        let guest = current();
        // SAFETY: a block `for_host` leaked, `WORDS` long.
        unsafe { (guest as *mut u64).add(CLEANUP_STACK).write(0) };
    }
}

#[cfg(target_os = "macos")]
mod imp {
    pub fn for_host(host: u64) -> u64 {
        host
    }

    pub fn host_of(guest: u64) -> u64 {
        guest
    }

    pub fn current() -> u64 {
        // SAFETY: always callable; the handle is pointer-sized.
        unsafe { core::mem::transmute_copy(&libc::pthread_self()) }
    }

    pub fn begin() {}
}

pub use imp::{begin, current, for_host, host_of};

/// `pthread_t pthread_self(void)`.
pub unsafe extern "C" fn pthread_self() -> u64 {
    current()
}

/// `int pthread_equal(pthread_t, pthread_t)`, over the threads the two stand for
/// so a host handle and the block issued for it agree.
pub unsafe extern "C" fn pthread_equal(a: u64, b: u64) -> i32 {
    i32::from(host_of(a) == host_of(b))
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn a_cleanup_push_through_pthread_self_leaves_the_host_thread_alone() {
        // SAFETY: always callable; the handle is pointer-sized.
        let host: u64 = unsafe { core::mem::transmute_copy(&libc::pthread_self()) };
        // SAFETY: plain calls.
        let guest = unsafe { pthread_self() };
        assert_ne!(guest, host, "the guest must not hold glibc's control block");
        let dtv = unsafe { (host as *const u64).add(1).read() };
        // What `pthread_cleanup_push` expands to.
        let mut record = [0u64; 3];
        unsafe { (guest as *mut u64).add(1).write(record.as_mut_ptr() as u64) };
        assert_eq!(unsafe { (host as *const u64).add(1).read() }, dtv);
        unsafe { (guest as *mut u64).add(1).write(0) };
    }

    #[test]
    fn a_block_maps_back_to_its_thread() {
        let guest = current();
        // SAFETY: always callable; the handle is pointer-sized.
        let host: u64 = unsafe { core::mem::transmute_copy(&libc::pthread_self()) };
        assert_eq!(host_of(guest), host);
        assert_eq!(for_host(host), guest);
        assert_eq!(unsafe { pthread_equal(guest, host) }, 1);
        let other = std::thread::spawn(current)
            .join()
            .expect("the thread finishes");
        assert_eq!(unsafe { pthread_equal(guest, other) }, 0);
    }
}
