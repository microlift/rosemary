use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::base::{CFIndex, CFTypeRef};
use crate::runloop::Source;

pub type Callback = unsafe extern "C" fn(CFTypeRef, u64, CFTypeRef, *const c_void, *mut c_void);

#[repr(C)]
pub struct Context {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: *const c_void,
    pub release: *const c_void,
    pub copy_description: *const c_void,
}

struct Socket {
    native: i32,
    callback_types: u64,
    enabled: AtomicU64,
    flags: AtomicU64,
    callback: Option<Callback>,
    info: usize,
    valid: AtomicBool,
    pending: AtomicU64,
    source: Mutex<Option<Weak<Source>>>,
}

unsafe impl Send for Socket {}
unsafe impl Sync for Socket {}

static SOCKETS: Mutex<Vec<Weak<Socket>>> = Mutex::new(Vec::new());

unsafe fn borrow(socket: CFTypeRef) -> Option<std::mem::ManuallyDrop<Arc<Socket>>> {
    if socket.is_null() || !crate::base::is_opaque(socket) {
        return None;
    }
    // SAFETY: CFSocket handles below are leaked `Arc<Socket>` pointers.
    Some(std::mem::ManuallyDrop::new(unsafe {
        Arc::from_raw(socket.cast::<Socket>())
    }))
}

pub unsafe extern "C" fn create_with_native(
    _allocator: CFTypeRef,
    native: i32,
    callback_types: u64,
    callback: Option<Callback>,
    context: *const Context,
) -> CFTypeRef {
    if native < 0 {
        return std::ptr::null();
    }
    let info = match context.is_null() {
        true => 0,
        // SAFETY: the caller supplied a live CFSocketContext.
        false => unsafe { (*context).info as usize },
    };
    let socket = Arc::new(Socket {
        native,
        callback_types,
        enabled: AtomicU64::new(callback_types),
        flags: AtomicU64::new(0x80 | callback_types),
        callback,
        info,
        valid: AtomicBool::new(true),
        pending: AtomicU64::new(0),
        source: Mutex::new(None),
    });
    SOCKETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Arc::downgrade(&socket));
    let handle = Arc::into_raw(socket).cast::<c_void>();
    crate::base::note_opaque(handle);
    handle
}

unsafe extern "C" fn perform(info: *mut c_void) {
    let Some(socket) = (unsafe { borrow(info) }) else {
        return;
    };
    let kind = socket.pending.swap(0, Ordering::AcqRel);
    if kind == 0 || !socket.valid.load(Ordering::Acquire) {
        return;
    }
    // A callback is one-shot unless its matching automatic-reenable flag is
    // set. Qt clears those flags and explicitly enables a notifier again after
    // processing it.
    if socket.flags.load(Ordering::Acquire) & kind == 0 {
        socket.enabled.fetch_and(!kind, Ordering::AcqRel);
    }
    if let Some(callback) = socket.callback {
        // SAFETY: the creator supplied this CFSocket callback and its context.
        unsafe {
            callback(
                info,
                kind,
                std::ptr::null(),
                std::ptr::null(),
                socket.info as *mut c_void,
            )
        };
    }
}

pub unsafe extern "C" fn create_run_loop_source(
    _allocator: CFTypeRef,
    socket: CFTypeRef,
    order: CFIndex,
) -> CFTypeRef {
    let Some(socket) = (unsafe { borrow(socket) }) else {
        return std::ptr::null();
    };
    let source = crate::runloop::new_source_ordered(
        Some(perform),
        Arc::as_ptr(&socket) as usize,
        order,
    );
    *socket.source.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::downgrade(&source));
    // `new_source_ordered` has already recorded the handle as one of ours, which
    // says nothing about who holds a reference to it. `leak` is the same
    // `Arc::into_raw` plus the release, so a guest that `CFRelease`s the source
    // it was handed ends the reference it was handed.
    //
    // A bare `Arc::into_raw` here cannot, and this one leaks unconditionally:
    // `note` leaves `refs` at `None`, so `release_opaque` answers
    // `CRef::Unknown`, and nothing in this crate ever rebuilds an `Arc<Source>`
    // from this address - the `Weak` above is the only other reference and it
    // never keeps the allocation alive. Every source this function hands out
    // was a `Source` that could never be dropped.
    crate::runloop::leak(source)
}

pub unsafe extern "C" fn get_native(socket: CFTypeRef) -> i32 {
    unsafe { borrow(socket) }.map_or(-1, |s| s.native)
}

pub unsafe extern "C" fn is_valid(socket: CFTypeRef) -> u8 {
    unsafe { borrow(socket) }.is_some_and(|s| s.valid.load(Ordering::Acquire)) as u8
}

pub unsafe extern "C" fn get_flags(socket: CFTypeRef) -> u64 {
    unsafe { borrow(socket) }.map_or(0, |s| s.flags.load(Ordering::Acquire))
}

pub unsafe extern "C" fn set_flags(socket: CFTypeRef, flags: u64) {
    if let Some(socket) = unsafe { borrow(socket) } {
        socket.flags.store(flags, Ordering::Release);
    }
}

pub unsafe extern "C" fn enable_callbacks(socket: CFTypeRef, kinds: u64) {
    if let Some(socket) = unsafe { borrow(socket) } {
        socket
            .enabled
            .fetch_or(kinds & socket.callback_types, Ordering::AcqRel);
    }
}

pub unsafe extern "C" fn disable_callbacks(socket: CFTypeRef, kinds: u64) {
    if let Some(socket) = unsafe { borrow(socket) } {
        socket.enabled.fetch_and(!kinds, Ordering::AcqRel);
    }
}

pub unsafe extern "C" fn invalidate(socket: CFTypeRef) {
    let Some(socket) = (unsafe { borrow(socket) }) else {
        return;
    };
    if socket.valid.swap(false, Ordering::AcqRel)
        && socket.flags.load(Ordering::Acquire) & 0x80 != 0
    {
        // SAFETY: this CFSocket owns the native descriptor under this flag.
        unsafe { libc::close(socket.native) };
    }
    if let Some(source) = socket
        .source
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(Weak::upgrade)
    {
        source.invalidate();
    }
}

/// Mark ready sockets before a run loop services its sources.
pub(crate) fn poll() {
    let sockets: Vec<Arc<Socket>> = {
        let mut registry = SOCKETS.lock().unwrap_or_else(|e| e.into_inner());
        registry.retain(|socket| socket.strong_count() != 0);
        registry.iter().filter_map(Weak::upgrade).collect()
    };
    for socket in sockets {
        let enabled = socket.enabled.load(Ordering::Acquire);
        if !socket.valid.load(Ordering::Acquire) || enabled == 0 {
            continue;
        }
        let mut descriptor = libc::pollfd {
            fd: socket.native,
            events: ((enabled & 1 != 0) as i16 * libc::POLLIN)
                | ((enabled & 8 != 0) as i16 * libc::POLLOUT),
            revents: 0,
        };
        // SAFETY: one live pollfd and a nonblocking timeout.
        if unsafe { libc::poll(&mut descriptor, 1, 0) } <= 0 {
            continue;
        }
        let kind = if descriptor.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0
            && enabled & 1 != 0
        {
            1
        } else if descriptor.revents & (libc::POLLOUT | libc::POLLERR) != 0 && enabled & 8 != 0 {
            8
        } else {
            0
        };
        if kind != 0 {
            socket.pending.store(kind, Ordering::Release);
            if let Some(source) = socket
                .source
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .and_then(Weak::upgrade)
            {
                source.signal();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real descriptor, because `create_with_native` takes one and
    /// `invalidate` closes it.
    fn descriptor() -> libc::c_int {
        // SAFETY: a `NUL`-terminated path this module supplies, and a read-only
        // nonblocking open of a character device that always exists.
        let fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
        assert!(fd >= 0, "/dev/null is openable");
        fd
    }

    /// A source this module hands out is the C world's to give back.
    ///
    /// `CFSocketCreateRunLoopSource` returns a +1 reference the caller owns and is
    /// expected to `CFRelease`, and this module had no way for that to happen:
    /// `Arc::into_raw` gives the reference away without recording where it is
    /// given back, and `note` - which `new_source_ordered` does call - records
    /// only *that* the address is one of ours, leaving the entry's `refs` at
    /// `None`. So a guest's `CFRelease` found no counted reference to give back.
    ///
    /// **That is worse than the leak it looks like, and the difference is worth
    /// being exact about.** `CFRelease` does not stop on `CRef::Unknown`; it
    /// goes on to `forwarded("CFRelease", …)` and then to
    /// `objc::arc::release`, whose last resort is `deallocate` - a Rust
    /// `Arc<Source>` allocation handed to the Objective-C deallocation path
    /// (`clear_weak_references`, then `dispose_instance`).
    ///
    /// **What this test measured, and what it read.** Measured, by reverting the
    /// one line above to `Arc::into_raw` and running this: the guest's
    /// `CFRelease` did **not** end the reference - the `Weak` still upgraded
    /// afterwards, where with `leak` it does not. Read, not measured, because the
    /// test does not assert it: that the fall-through reaches `deallocate` at
    /// all, and what `dispose_instance` does to a struct this crate laid out.
    /// Nothing here rebuilds an `Arc` from the handle, so the C world's half was
    /// the only strong reference - which is why "the reference was not ended" is
    /// the whole of what the assertion claims and not more.
    ///
    /// The assertion is on that observable consequence rather than on a table
    /// entry, because a table entry is an implementation detail and a reference
    /// the guest gave back that is still outstanding is the thing that matters:
    /// the `Weak` is the only other handle on this allocation, so it upgrades
    /// while the guest holds the source and stops upgrading once the guest has
    /// given it back.
    #[test]
    fn a_source_the_guest_gives_back_is_freed_and_one_it_keeps_is_not() {
        // SAFETY: a null allocator, a real descriptor, no callback and no context.
        let socket = unsafe {
            create_with_native(
                std::ptr::null(),
                descriptor(),
                0,
                None,
                std::ptr::null(),
            )
        };
        assert!(!socket.is_null(), "a non-negative descriptor makes a socket");
        // SAFETY: a socket this module made.
        let handle = unsafe { create_run_loop_source(std::ptr::null(), socket, 0) };
        assert!(!handle.is_null(), "a live socket is listenable");

        // SAFETY: a socket this module made, and the returned `Arc` is
        // `ManuallyDrop` because the C world still owns the reference.
        let weak = unsafe { borrow(socket) }
            .expect("the socket is in the table")
            .source
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .expect("create_run_loop_source recorded the source");
        assert!(
            weak.upgrade().is_some(),
            "the source is alive while the guest holds it"
        );

        // SAFETY: a `CFRunLoopSourceRef` this module handed out.
        unsafe { crate::api::CFRelease(handle) };

        assert!(
            weak.upgrade().is_none(),
            "the guest gave the reference back and the Source is still allocated. \
             The C world's half was never recorded, so `release_opaque` answered \
             `CRef::Unknown` and the release fell through to the forwarding path \
             instead of ending the reference it was handed"
        );
    }
}
