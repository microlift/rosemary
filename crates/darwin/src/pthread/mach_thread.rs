//! Darwin's thread-port <-> `pthread_t` translation.
//!
//! Both names exist because a thread on Darwin has two identities, and the
//! conversion between them is a **search**, not a cast. `libpthread` keeps the
//! Mach port of every thread it knows about in that thread's thread-specific
//! storage, and
//!
//! ```c
//! mach_port_t pthread_mach_thread_np(pthread_t t) {
//!         mach_port_t kport = MACH_PORT_NULL;
//!         (void)_pthread_is_valid(t, &kport);
//!         return kport;
//! }
//!
//! pthread_t pthread_from_mach_thread_np(mach_port_t kernel_thread) {
//!         pthread_t p = NULL;
//!         _pthread_lock_lock(&_pthread_list_lock);
//!         TAILQ_FOREACH(p, &__pthread_head, tl_plist) {
//!                 if (_pthread_tsd_slot(p, MACH_THREAD_SELF) == kernel_thread)
//!                         break;
//!         }
//!         _pthread_lock_unlock(&_pthread_list_lock);
//!         return p;
//! }
//! ```
//! (`apple-oss-distributions/libpthread`, `src/pthread.c:972-997`) walk the
//! thread list in both directions, and the second leaves `p` null when the port
//! names no thread. That last part is the contract: **`NULL` is the answer for a
//! port that is not a live thread here**, and the only wrong answer this module
//! can produce is a `pthread_t` for a thread that is not the one named - which
//! is what makes it a search, and not `(pthread_t)port`.
//!
//! It is also the pair Chromium reaches for. `base::profiler`'s
//! `SuspendableThreadDelegateMac` (Chromium `130.0.6723.91`,
//! `base/profiler/suspendable_thread_delegate_mac.h`) takes a `thread_act_t` -
//! the thread's Mach port - and its constructor inlines exactly
//!
//! ```c
//! stack_base_address_(pthread_get_stackaddr_np(pthread_from_mach_thread_np(thread_port_)))
//! ```
//!
//! into a stack-unwinding helper. The port it was given came from
//! `pthread_mach_thread_np(pthread_self())`. So a guest holding one of those
//! values asks for it back, and a runtime that cannot answer hands the unwinder
//! a `NULL` `pthread_t`: the walk then goes on with no stack base at all, which
//! is the safe failure, against one handed a plausible address that belongs to
//! some other thread.
//!
//! The host is x86-64 Linux and has no Mach ports. This tree's Mach-thread-port
//! namespace is [`super::id`] - a kernel thread id, or a Mach port on a Mac host
//! - and it is already what [`super::mach_thread_np`] hands out. So the
//! conversion is the inverse of the registry that is already there, and nothing
//! new is minted: a value that did not come out of that registry does not go
//! back into it.
//!
//! One namespace caveat, which is this module's contract rather than a bug in
//! it: [`crate::mach::task::mach_thread_self`] answers from
//! [`crate::mach::space`], whose names start at `0x103`, and no thread is
//! registered there. A guest that used *that* as a thread token and asked for it
//! back gets `NULL`, which is the honest answer for a namespace this runtime
//! keeps no registry for. Reconciling the two namespaces belongs in `mach`; a
//! cross-namespace answer here would be a wrong one.

use super::thread_ids;

/// `pthread_t pthread_from_mach_thread_np(mach_port_t thread)`, Darwin's own.
///
/// The port names a thread; the answer is that thread's `pthread_t`, and **zero**
/// - `libpthread` returns `NULL` - when it names no thread this runtime knows.
///
/// # Safety
///
/// Called only through an import slot this crate defines.
pub unsafe extern "C" fn from_mach_thread_np(port: u32) -> u64 {
    // `MACH_PORT_NULL`: no thread has no port, and `libpthread`'s own list walk
    // would compare it against real ports and find nothing.
    if port == 0 {
        return 0;
    }
    // The caller's own thread first, and for the same reason Darwin's walk finds
    // it first: `__pthread_head` holds the main thread. Nothing in this tree
    // registers the process's *first* thread - it was started by the runtime, not
    // by `pthread_create` - and a sampling profiler's whole job is to sample one
    // of the threads that exist before anything calls `pthread_create`, so
    // taking the registry's word for it would report "no such thread" for the
    // only thread there is. A thread needs no registry entry to know its own
    // handle, and the answer cannot be stale, so this is both the common case
    // and the exact one.
    if port == super::id() {
        // SAFETY: `pthread_self` is infallible and its handle is pointer-sized,
        // which is the same eight bytes `super::thread_of` reads back.
        //
        // `for_host` is load-bearing and was missing here. `pthread_create` now
        // writes `handle::for_host(...)` into the caller's `pthread_t`, so a
        // guest handle is **not** the host's own pointer any more - it is a
        // per-thread block this runtime leaked. Returning the raw host bits here
        // answers a `pthread_t` from a different namespace than every other
        // function in `pthread` now speaks, and
        // `a_started_threads_port_converts_back_to_its_handle` fails 8 times out
        // of 8 with one host pointer on the left and another on the right. The
        // `thread_ids` answer below needs it for the same reason: that table is
        // keyed by host handles.
        return super::handle::for_host(super::thread_bits(unsafe {
            libc::pthread_self()
        }));
    }
    thread_ids::thread_of(port).map(super::handle::for_host).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pthread::{get_stackaddr_np, get_stacksize_np, mach_thread_np};

    /// The calling thread's own `pthread_t`, as the guest's `pthread_self`
    /// returns it.
    fn this_thread() -> u64 {
        // SAFETY: `pthread_self` is infallible and its handle is pointer-sized.
        // `for_host` for the same reason `from_mach_thread_np` needs it: a guest
        // handle is a block this runtime leaked, not the host pointer.
        crate::pthread::handle::for_host(crate::pthread::thread_bits(unsafe {
            libc::pthread_self()
        }))
    }

    /// The name has to reach the guest as an *address*, or the pair is worth
    /// nothing: this is the fault a Chromium 130 run died on. `pthread_` is
    /// denied wholesale by [`crate::divergent`], so publishing the function is
    /// the only thing that carves the name back out, and a name this crate
    /// publishes must never also be the host's.
    #[test]
    fn the_conversion_is_published_rather_than_left_to_the_deny_list() {
        let ours = crate::pthread::symbol("pthread_from_mach_thread_np")
            .expect("the guest's import slot for this name would be left zero");
        assert_ne!(
            ours,
            crate::host_symbol("pthread_from_mach_thread_np").unwrap_or(0),
            "the guest's slot would hold the host's, which reads the port wrong"
        );
        assert!(
            !crate::divergent::contains("pthread_from_mach_thread_np"),
            "a name this crate answers for cannot also be refused by family"
        );
    }

    /// What a guest observes for the conversion Chromium's profiler performs:
    /// its own port comes back as its own `pthread_t`, and that handle reads
    /// back as a stack that contains the frames the guest is standing in.
    ///
    /// Asserted the way the guest uses the answer. `pthread_get_stackaddr_np` is
    /// Darwin's *highest* stack address, and the only way to tell a real answer
    /// from a plausible one is that it is above the frame asking.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_guest_converts_its_own_thread_port_back_to_its_own_pthread_t() {
        // SAFETY: no arguments, so nothing a caller can get wrong. Zero means
        // "the calling thread", which is what a Darwin `pthread_mach_thread_np(0)`
        // would not do but this tree's does.
        let port = unsafe { mach_thread_np(0) };
        assert_ne!(port, 0, "a thread's own port is never the null port");

        // SAFETY: called only through an import slot this crate defines, which
        // is how the guest reaches it.
        let thread = unsafe { from_mach_thread_np(port) };
        assert_eq!(
            thread,
            this_thread(),
            "a live thread's own port must convert back to its own handle"
        );

        // The answer is a handle the stack calls accept, not merely a nonzero
        // word: this thread's stack top is above the frame doing the asking.
        // SAFETY: the handle is `pthread_self` as the guest sees it.
        let top = unsafe { get_stackaddr_np(thread) };
        let here = &port as *const _ as u64;
        assert!(
            top > here,
            "a stack top of {top:#x} is not above this frame at {here:#x}"
        );
        // And the pair round trips, which is what makes the two names one
        // identity: the port a guest gets back out of the handle it was just
        // given is the port it started with.
        // SAFETY: as above.
        assert_eq!(unsafe { mach_thread_np(thread) }, port);
    }

    /// A port from the other namespace, and the null port, both answer zero
    /// rather than inventing a handle. The guest's next call is
    /// `pthread_get_stackaddr_np`, so the property that matters is that *that*
    /// survives the null handle: glibc would read it as a `struct pthread *`.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_port_that_names_no_thread_is_refused_rather_than_guessed_at() {
        // SAFETY: as above. Zero is `MACH_PORT_NULL`.
        assert_eq!(unsafe { from_mach_thread_np(0) }, 0);

        // A port from the Mach namespace in `crate::mach::space`, which starts
        // at 0x103 and registers no thread.
        let foreign = crate::mach::task::mach_thread_self();
        assert_ne!(foreign, 0, "`mach_thread_self` always names something");
        // SAFETY: as above.
        assert_eq!(
            unsafe { from_mach_thread_np(foreign) },
            0,
            "a port from the other namespace named a thread"
        );

        // Zero is the answer for a thread nobody knows, and it must not become a
        // dereference. Both stack calls answer in a way no stack could be at.
        // SAFETY: the null handle, which is this module's own documented answer.
        let unknown = unsafe { from_mach_thread_np(0) };
        // SAFETY: as above.
        assert!(unsafe { get_stackaddr_np(unknown) } < 0x1000);
        // SAFETY: as above.
        assert_eq!(unsafe { get_stacksize_np(unknown) }, 0);
    }

    /// The direction that is not the caller's own: a thread this crate started
    /// is findable by the port it was handed at creation, which is what a guest
    /// does when it profiles a worker thread.
    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn a_started_threads_port_converts_back_to_its_handle() {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

        /// A worker that stays alive until the test has converted its port back
        /// and read the answer, so the handle is never stale while it is used.
        static SEEN_PORT: AtomicU32 = AtomicU32::new(0);
        static STARTED: AtomicBool = AtomicBool::new(false);
        extern "C" fn wait(_: u64) -> u64 {
            STARTED.store(true, Ordering::Release);
            while SEEN_PORT.load(Ordering::Relaxed) == 0 {
                std::hint::spin_loop();
            }
            0
        }

        STARTED.store(false, Ordering::Release);
        let mut thread = 0u64;
        // SAFETY: a writable `pthread_t`, null attributes, and a start routine
        // of this crate's own.
        let rc = unsafe {
            crate::pthread::create(&mut thread as *mut _ as u64, 0, wait as *const () as u64, 0)
        };
        assert_eq!(rc, 0, "the thread did not start");
        // SAFETY: a live handle this crate's `create` just wrote.
        let port = unsafe { mach_thread_np(thread) };
        assert_ne!(
            port, 0,
            "`create` publishes the child's id before it returns"
        );
        // SAFETY: as above.
        assert_eq!(
            unsafe { from_mach_thread_np(port) },
            thread,
            "a port this runtime handed out must convert back to that handle"
        );
        // SAFETY: as above.
        assert!(unsafe { get_stackaddr_np(thread) } > 0x1000);

        // Held open until here so the handle is a live thread's throughout.
        SEEN_PORT.store(1, Ordering::Relaxed);
        while !STARTED.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }
        // SAFETY: the handle `create` wrote, joined exactly once.
        assert_eq!(unsafe { crate::pthread::join(thread, 0) }, 0);
    }
}
