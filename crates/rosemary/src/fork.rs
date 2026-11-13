//! Keeping this runtime's own state usable in the child of a guest's `fork`.
//!
//! The child has only the thread that forked. A lock any other thread held at that
//! moment stays held in the child for ever, and Darwin's libdispatch, which parks a
//! contended Rust lock, traps outright when one is waited on there. So before the
//! fork the forking thread takes every lock this runtime's background work and code
//! cache take, waiting for whatever holds them to finish, and lets them go on both
//! sides afterwards.
//!
//! The child also stays on its one thread as far as this runtime is concerned: any
//! contended lock there would trap in libdispatch for the same reason, so it starts
//! no workers of its own.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set in the child of a fork.
static IN_CHILD: AtomicBool = AtomicBool::new(false);

/// Whether this process is the child of a fork, where this runtime starts no thread.
pub fn in_child() -> bool {
    IN_CHILD.load(Ordering::Relaxed)
}

/// What the forking thread holds from just before the fork to just after it.
struct Held {
    prelift: crate::prelift::ForkHold,
    cache: Option<crate::cache::ForkHold>,
    fault: crate::fault::ForkHold,
}

thread_local! {
    static HELD: RefCell<Option<Held>> = const { RefCell::new(None) };
}

extern "C" fn before_fork() {
    // Workers first: a job they finish may need the cache's locks.
    let prelift = crate::prelift::hold_for_fork();
    let cache = crate::thunk::published().map(|vm| vm.hold_cache_for_fork());
    let fault = crate::fault::hold_for_fork();
    HELD.with(|held| {
        *held.borrow_mut() = Some(Held {
            prelift,
            cache,
            fault,
        })
    });
}

fn release(in_child: bool) {
    let Some(held) = HELD.with(|held| held.borrow_mut().take()) else {
        return;
    };
    drop(held.fault);
    if let Some(cache) = held.cache {
        cache.release(in_child);
    }
    held.prelift.release(in_child);
}

extern "C" fn after_fork_in_parent() {
    release(false);
}

extern "C" fn after_fork_in_child() {
    IN_CHILD.store(true, Ordering::Relaxed);
    release(true);
}

/// Registers the fork handlers, once.
pub fn guard() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        // SAFETY: three functions that live as long as the process.
        unsafe {
            libc::pthread_atfork(
                Some(before_fork),
                Some(after_fork_in_parent),
                Some(after_fork_in_child),
            )
        };
    });
}
