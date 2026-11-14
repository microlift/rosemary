//! Asynchronous Metal completion/scheduling callbacks between the host block
//! runtime and Rosemary's built-in guest Objective-C block runtime.
//!
//! The host block never receives or calls a guest Objective-C object: its
//! native trampoline enqueues the guest block on Rosemary's existing Metal
//! completion worker, which installs Darwin TLS and an autorelease pool.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const HAS_COPY_DISPOSE: i32 = 1 << 25;
const HAS_SIGNATURE: i32 = 1 << 30;

struct HandlerCapture {
    guest_block: AtomicUsize,
    guest_command: usize,
}

impl Drop for HandlerCapture {
    fn drop(&mut self) {
        let block = self.guest_block.swap(0, Ordering::AcqRel);
        if block != 0 {
            objc::block::release(block as Id);
        }
        objc::arc::release(self.guest_command as Id);
    }
}

#[repr(C)]
struct HostBlock {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: extern "C" fn(*const HostBlock, HostId),
    descriptor: &'static HostBlockDescriptor,
    capture: *const HandlerCapture,
}

#[repr(C)]
struct HostBlockDescriptor {
    reserved: usize,
    size: usize,
    copy: unsafe extern "C" fn(*mut HostBlock, *const HostBlock),
    dispose: unsafe extern "C" fn(*mut HostBlock),
    signature: &'static [u8; 5],
}

static DESCRIPTOR: HostBlockDescriptor = HostBlockDescriptor {
    reserved: 0,
    size: std::mem::size_of::<HostBlock>(),
    copy: copy_capture,
    dispose: dispose_capture,
    signature: b"v@?@\0",
};

unsafe extern "C" fn copy_capture(_destination: *mut HostBlock, source: *const HostBlock) {
    let capture = unsafe { (*source).capture };
    if !capture.is_null() {
        unsafe { Arc::increment_strong_count(capture) };
    }
}

unsafe extern "C" fn dispose_capture(block: *mut HostBlock) {
    let capture = unsafe { (*block).capture };
    if !capture.is_null() {
        unsafe { Arc::decrement_strong_count(capture) };
    }
}

extern "C" fn finished(block: *const HostBlock, _host_command: HostId) {
    let capture = unsafe { (*block).capture };
    if capture.is_null() {
        return;
    }
    let capture = unsafe { &*capture };
    let guest_block = capture.guest_block.swap(0, Ordering::AcqRel);
    if guest_block != 0 {
        // The existing Metal worker owns the block and retains the guest
        // command until the callback has finished. It enters Darwin TLS and
        // an autorelease pool before invoking guest code.
        unsafe {
            crate::completion::deliver(guest_block as Id, capture.guest_command as Id);
        }
    }
}

fn native_stack_block_isa() -> *const c_void {
    static ISA: OnceLock<usize> = OnceLock::new();
    let address = *ISA.get_or_init(|| unsafe {
        let handle = libc::dlopen(
            c"/usr/lib/libSystem.B.dylib".as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        );
        if handle.is_null() {
            return 0;
        }
        libc::dlsym(handle, c"_NSConcreteStackBlock".as_ptr()) as usize
    });
    address as *const c_void
}

/// The guest block is already copied and this call transfers that reference.
/// A native stack block is copied synchronously by `add*Handler:`. Its copy
/// helper retains the capture, and its dispose helper eventually releases it.
/// No guest pointers are passed directly to Apple's Objective-C dispatch.
pub(super) fn register(host_command: HostId, guest_command: Id, guest_block: Id, scheduled: bool) {
    let isa = native_stack_block_isa();
    if isa.is_null() {
        gpu::trace("native Metal cannot find the host Blocks runtime".to_string());
        objc::block::release(guest_block);
        return;
    }
    let capture = Arc::new(HandlerCapture {
        guest_block: AtomicUsize::new(guest_block as usize),
        guest_command: objc::arc::retain(guest_command) as usize,
    });
    let raw = Arc::into_raw(capture);
    let block = HostBlock {
        isa,
        flags: HAS_COPY_DISPOSE | HAS_SIGNATURE,
        reserved: 0,
        invoke: finished,
        descriptor: &DESCRIPTOR,
        capture: raw,
    };
    let _pool = HostPool::new();
    let selector = if scheduled {
        c"addScheduledHandler:"
    } else {
        c"addCompletedHandler:"
    };
    unsafe {
        host_send_void_id(
            host_command,
            sel(selector),
            (&raw const block).cast_mut().cast(),
        )
    };
    // The retained host copy now owns its Arc strong count. The stack
    // literal's reference was only needed for the duration of registration.
    unsafe { drop(Arc::from_raw(raw)) };
}
