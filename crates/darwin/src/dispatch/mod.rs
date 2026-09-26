#![cfg_attr(target_os = "macos", allow(dead_code))]

pub mod block;
pub mod data;
pub mod object;
pub mod queue;
pub mod source;
pub mod sync;
pub mod time;

use std::ffi::c_void;
use std::sync::atomic::{AtomicI64, Ordering};

/// The states a `dispatch_once_t` passes through.
///
/// Darwin's is a `long` the caller zero-initializes, so zero has to mean "not
/// yet". The other two values are this implementation's own: nothing reads a
/// `dispatch_once_t` except `dispatch_once`.
const NOT_YET: i64 = 0;
const RUNNING: i64 = 1;
const DONE: i64 = -1;

/// `void dispatch_once_f(dispatch_once_t *, void *context, dispatch_function_t)`.
///
/// # Safety
///
/// `predicate` must be a `long` that started at zero and is used for nothing
/// else, and `function` must be callable with `context`.
unsafe extern "C" fn once_f(
    predicate: *mut c_void,
    context: *mut c_void,
    function: Option<unsafe extern "C" fn(*mut c_void)>,
) {
    let Some(function) = function else {
        return;
    };
    // SAFETY: the caller's obligation for the predicate, and `function` is
    // callable with `context` by the same.
    unsafe { once_with(predicate, || function(context)) };
}

/// `void dispatch_once(dispatch_once_t *, dispatch_block_t)`.
///
/// The block is not copied: it has run before this returns, so it cannot outlive
/// the caller's frame the way an asynchronous one would.
///
/// # Safety
///
/// As [`once_f`], and `block` is a block whose `invoke` is callable here.
unsafe extern "C" fn once(predicate: *mut c_void, block: u64) {
    if block == 0 {
        return;
    }
    unsafe { once_with(predicate, || block::invoke(block)) };
}

/// Run `body` the first time this predicate is used, and never again.
///
/// # Safety
///
/// `predicate` must be a `long` that started at zero and is used for nothing
/// else, and `body` must be callable from this thread.
unsafe fn once_with(predicate: *mut c_void, body: impl FnOnce()) {
    if predicate.is_null() {
        return;
    }
    // SAFETY: the caller's obligation. `AtomicI64` and `long` have the same
    // size and alignment, and nothing else touches this word.
    let state = unsafe { &*(predicate as *const AtomicI64) };
    if state.load(Ordering::Acquire) == DONE {
        return;
    }
    // Spun rather than parked. A `dispatch_once` body is short by construction -
    // it is what a lazy global is initialized by - and a wait here would need a
    // futex per predicate, which is a table keyed on an address a caller may
    // free. Recursion into the same predicate is a deadlock on Darwin too.
    loop {
        match state.compare_exchange(NOT_YET, RUNNING, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => break,
            Err(DONE) => return,
            Err(_) => std::hint::spin_loop(),
        }
    }
    body();
    state.store(DONE, Ordering::Release);
}

/// `_dispatch_main_q`, and a note that the thread binding it is the main one.
///
/// Recorded here rather than at start-up because this is the call that runs on
/// the thread the program was loaded on, which is what `dispatch_get_main_queue`
/// means by main.
#[cfg(not(target_os = "macos"))]
fn main_queue() -> u64 {
    queue::note_main_thread();
    queue::main_queue()
}

#[cfg(target_os = "macos")]
exports::exports! {}

#[cfg(not(target_os = "macos"))]
exports::exports! {
    functions {
        "dispatch_queue_create" => queue::queue_create,
        "dispatch_queue_create_with_target" => queue::queue_create_with_target,
        "dispatch_queue_get_label" => queue::queue_get_label,
        "dispatch_get_global_queue" => queue::get_global_queue,
        "dispatch_async" => queue::async_,
        "dispatch_async_f" => queue::async_f,
        "dispatch_sync" => queue::sync,
        "dispatch_sync_f" => queue::sync_f,
        "dispatch_after" => queue::after,
        "dispatch_after_f" => queue::after_f,
        // `dispatch_resume` and `dispatch_suspend` take a `dispatch_object_t`,
        // which is a source or a queue; see `source`, which answers for a source
        // and declines for a queue because a queue is never suspended.
        "dispatch_resume" => source::resume,
        "dispatch_suspend" => source::suspend,
        // The `dispatch_source_*` family, which is one `DISPATCH_SOURCE_TYPE_MACH_RECV`
        // over a real Mach port and nothing else. See `source`'s module note for
        // the call sites and for the two source types it refuses by name.
        "dispatch_source_create" => source::create,
        "dispatch_source_set_event_handler" => source::set_event_handler,
        "dispatch_source_set_event_handler_f" => source::set_event_handler_f,
        "dispatch_source_set_cancel_handler" => source::set_cancel_handler,
        "dispatch_source_set_cancel_handler_f" => source::set_cancel_handler_f,
        "dispatch_source_cancel" => source::cancel,
        "dispatch_once" => once,
        "dispatch_once_f" => once_f,
        "dispatch_group_create" => sync::group_create,
        "dispatch_group_enter" => sync::group_enter,
        "dispatch_group_leave" => sync::group_leave,
        "dispatch_group_notify" => sync::group_notify,
        "dispatch_group_notify_f" => sync::group_notify_f,
        "dispatch_group_async_f" => sync::group_async_f,
        "dispatch_group_async" => sync::group_async,
        "dispatch_apply_f" => sync::apply_f,
        "dispatch_group_wait" => sync::group_wait,
        "dispatch_semaphore_create" => sync::semaphore_create,
        "dispatch_semaphore_signal" => sync::semaphore_signal,
        "dispatch_semaphore_wait" => sync::semaphore_wait,
        "dispatch_time" => time::time,
        "dispatch_data_create" => data::create,
        "dispatch_data_get_size" => data::get_size,
        "dispatch_retain" => object::retain_entry,
        "dispatch_release" => object::release_entry,
        "dispatch_set_context" => object::set_context,
        "dispatch_get_context" => object::get_context,
        "dispatch_set_finalizer_f" => object::set_finalizer_f,
    }
    data {
        "__dispatch_main_q" => main_queue(),
        "__dispatch_queue_attr_concurrent" => queue::concurrent_attr(),
        "__dispatch_data_destructor_free" => data::destructor_free(),
        // A source type is the *address* of a `dispatch_source_type_s` and
        // nothing else, so the three the guest imports are three distinct
        // addresses. Publishing the two this runtime refuses to build a source
        // from is deliberate: a guest that reads one and compares it, or branches
        // on it being non-null, gets the truth, and the refusal happens in
        // `dispatch_source_create` where the source would be made.
        "__dispatch_source_type_mach_recv" => source::mach_recv_token(),
        "__dispatch_source_type_proc" => source::proc_token(),
        "__dispatch_source_type_memorypressure" => source::memorypressure_token(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn count(_context: *mut c_void) {
        CALLS.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn the_body_runs_once_however_many_threads_ask() {
        let predicate = Box::leak(Box::new(0i64)) as *mut i64 as *mut c_void;
        let address = predicate as usize;
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(move || {
                    // SAFETY: a zero-initialized `long` this test owns.
                    unsafe { once_f(address as *mut c_void, std::ptr::null_mut(), Some(count)) };
                });
            }
        });
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        // And a null predicate is declined rather than dereferenced.
        // SAFETY: the null case returns before any load.
        unsafe { once_f(std::ptr::null_mut(), std::ptr::null_mut(), Some(count)) };
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    }
}
