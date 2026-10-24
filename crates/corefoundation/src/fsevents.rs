use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::base::{CFIndex, CFTypeRef};

type Callback =
    unsafe extern "C" fn(*const Stream, *mut c_void, usize, *mut c_void, *const u64, *const u32);

#[repr(C)]
pub struct Context {
    version: CFIndex,
    info: *mut c_void,
    retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<unsafe extern "C" fn(*const c_void)>,
    copy_description: *const c_void,
}

pub struct Stream {
    _callback: Option<Callback>,
    info: *const c_void,
    release: Option<unsafe extern "C" fn(*const c_void)>,
    valid: AtomicBool,
    started: AtomicBool,
}

pub unsafe extern "C" fn create(
    _allocator: CFTypeRef,
    callback: Option<Callback>,
    context: *const Context,
    _paths: CFTypeRef,
    _since_when: u64,
    _latency: f64,
    _flags: u32,
) -> *const Stream {
    let (info, release) = if context.is_null() {
        (std::ptr::null(), None)
    } else {
        // SAFETY: the caller supplied a readable FSEventStreamContext.
        let context = unsafe { &*context };
        let info = match context.retain {
            // SAFETY: retain is the ownership callback the caller supplied for info.
            Some(retain) => unsafe { retain(context.info) },
            None => context.info,
        };
        (info, context.release)
    };
    Box::into_raw(Box::new(Stream {
        _callback: callback,
        info,
        release,
        valid: AtomicBool::new(true),
        started: AtomicBool::new(false),
    }))
}

/// `FSEventStreamCreateRelativeToDevice`: as [`create`], with paths relative
/// to a device's root.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn create_relative_to_device(
    allocator: CFTypeRef,
    callback: Option<Callback>,
    context: *const Context,
    _device: i32,
    paths: CFTypeRef,
    since_when: u64,
    latency: f64,
    flags: u32,
) -> *const Stream {
    // SAFETY: the caller's obligations are `create`'s.
    unsafe {
        create(
            allocator, callback, context, paths, since_when, latency, flags,
        )
    }
}

/// `FSEventStreamScheduleWithRunLoop`: where the callback would be called.
pub unsafe extern "C" fn schedule_with_run_loop(
    _stream: *const Stream,
    _run_loop: CFTypeRef,
    _mode: CFTypeRef,
) {
}

/// `FSEventStreamEventId FSEventsGetCurrentEventId(void)`.
///
/// FIXME (ACCURACY): no stream here ever reports an event, so the id is a
/// counter that only has to move forward between calls.
pub extern "C" fn get_current_event_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub unsafe extern "C" fn set_dispatch_queue(_stream: *const Stream, _queue: *mut c_void) {}

pub unsafe extern "C" fn start(stream: *const Stream) -> u8 {
    // SAFETY: FSEvents requires a live stream reference.
    let Some(stream) = (unsafe { stream.as_ref() }) else {
        return 0;
    };
    if !stream.valid.load(Ordering::Acquire) {
        return 0;
    }
    stream.started.store(true, Ordering::Release);
    1
}

pub unsafe extern "C" fn stop(stream: *const Stream) {
    // SAFETY: null is tolerated; otherwise FSEvents requires a live reference.
    if let Some(stream) = unsafe { stream.as_ref() } {
        stream.started.store(false, Ordering::Release);
    }
}

pub unsafe extern "C" fn flush_sync(_stream: *const Stream) {}

pub unsafe extern "C" fn invalidate(stream: *const Stream) {
    // SAFETY: null is tolerated; otherwise FSEvents requires a live reference.
    if let Some(stream) = unsafe { stream.as_ref() } {
        stream.started.store(false, Ordering::Release);
        stream.valid.store(false, Ordering::Release);
    }
}

pub unsafe extern "C" fn release(stream: *const Stream) {
    if stream.is_null() {
        return;
    }
    // SAFETY: FSEventStreamRelease consumes one create-rule stream reference.
    let stream = unsafe { Box::from_raw(stream.cast_mut()) };
    if let Some(release) = stream.release {
        // SAFETY: this balances the context retain performed by create.
        unsafe { release(stream.info) };
    }
}

exports::exports! {
    functions {
        "FSEventStreamCreate" => create,
        "FSEventStreamCreateRelativeToDevice" => create_relative_to_device,
        "FSEventStreamScheduleWithRunLoop" => schedule_with_run_loop,
        "FSEventStreamUnscheduleFromRunLoop" => schedule_with_run_loop,
        "FSEventsGetCurrentEventId" => get_current_event_id,
        "FSEventStreamSetDispatchQueue" => set_dispatch_queue,
        "FSEventStreamStart" => start,
        "FSEventStreamStop" => stop,
        "FSEventStreamFlushSync" => flush_sync,
        "FSEventStreamInvalidate" => invalidate,
        "FSEventStreamRelease" => release,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static RETAINS: AtomicUsize = AtomicUsize::new(0);
    static RELEASES: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn retain(info: *const c_void) -> *const c_void {
        RETAINS.fetch_add(1, Ordering::Relaxed);
        info
    }

    unsafe extern "C" fn release_info(_info: *const c_void) {
        RELEASES.fetch_add(1, Ordering::Relaxed);
    }

    #[test]
    fn stream_lifecycle_balances_context_ownership() {
        RETAINS.store(0, Ordering::Relaxed);
        RELEASES.store(0, Ordering::Relaxed);
        let context = Context {
            version: 0,
            info: 1usize as *mut c_void,
            retain: Some(retain),
            release: Some(release_info),
            copy_description: std::ptr::null(),
        };

        // SAFETY: the context and returned stream remain live for every call.
        unsafe {
            let stream = create(
                std::ptr::null(),
                None,
                &context,
                std::ptr::null(),
                0,
                0.0,
                0,
            );
            assert_eq!(RETAINS.load(Ordering::Relaxed), 1);
            assert_eq!(start(stream), 1);
            invalidate(stream);
            assert_eq!(start(stream), 0);
            release(stream);
        }
        assert_eq!(RELEASES.load(Ordering::Relaxed), 1);
    }
}
