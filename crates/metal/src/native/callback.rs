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
    guest_command: AtomicUsize,
    progress: Arc<crate::completion::CallbackProgress>,
    scheduled: bool,
}

impl Drop for HandlerCapture {
    fn drop(&mut self) {
        let block = self.guest_block.swap(0, Ordering::AcqRel);
        let command = self.guest_command.swap(0, Ordering::AcqRel);
        if block == 0 && command == 0 {
            return; // Already transferred by invocation; do not account twice.
        }
        // Host block disposal can run on an Apple-owned thread with no Darwin
        // TLS or guest autorelease pool. Transfer ownership; never finalize a
        // guest object here, even if no completion handler ever ran.
        unsafe {
            crate::completion::retire_owned(
                block as Id,
                command as Id,
                Arc::clone(&self.progress),
                self.scheduled,
            )
        };
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
    let guest_command = capture.guest_command.swap(0, Ordering::AcqRel);
    if guest_block == 0 && guest_command == 0 {
        return;
    }
    // Both references were retained on the guest registration thread. Metal
    // may call this on an uninitialized native thread, so only queue the
    // owned references and do not touch guest Objective-C here.
    unsafe {
        if guest_block != 0 {
            crate::completion::deliver_owned(
                guest_block as Id,
                guest_command as Id,
                Arc::clone(&capture.progress),
                capture.scheduled,
            );
        } else {
            crate::completion::retire_owned(
                std::ptr::null_mut(),
                guest_command as Id,
                Arc::clone(&capture.progress),
                capture.scheduled,
            );
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
    let progress =
        Arc::clone(&unsafe { object::body::<NativeCommandBuffer>(guest_command) }.progress);
    progress.register(scheduled);
    let capture = Arc::new(HandlerCapture {
        guest_block: AtomicUsize::new(guest_block as usize),
        guest_command: AtomicUsize::new(objc::arc::retain(guest_command) as usize),
        progress,
        scheduled,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    static DISPOSALS: AtomicUsize = AtomicUsize::new(0);
    static RELEASED_ON_WORKER: AtomicBool = AtomicBool::new(false);

    #[repr(C)]
    struct GuestStackBlock {
        isa: *const c_void,
        flags: i32,
        reserved: i32,
        invoke: unsafe extern "C" fn(Id, Id),
        descriptor: &'static GuestBlockDescriptor,
    }

    #[repr(C)]
    struct GuestBlockDescriptor {
        reserved: usize,
        size: usize,
        copy: unsafe extern "C" fn(*mut c_void, *const c_void),
        dispose: unsafe extern "C" fn(*mut c_void),
    }

    unsafe extern "C" fn do_nothing(_block: Id, _command: Id) {}
    unsafe extern "C" fn copied_guest_block(_to: *mut c_void, _from: *const c_void) {}
    unsafe extern "C" fn disposed_guest_block(_block: *mut c_void) {
        RELEASED_ON_WORKER.store(
            std::thread::current().name() == Some("metal:completed"),
            Ordering::SeqCst,
        );
        DISPOSALS.fetch_add(1, Ordering::SeqCst);
    }

    static GUEST_DESCRIPTOR: GuestBlockDescriptor = GuestBlockDescriptor {
        reserved: 0,
        size: std::mem::size_of::<GuestStackBlock>(),
        copy: copied_guest_block,
        dispose: disposed_guest_block,
    };

    #[test]
    fn last_host_block_disposal_on_foreign_thread_retires_guest_references_on_worker() {
        if !super::super::enabled() {
            eprintln!("skipped: set ROSEMARY_METAL_BACKEND=native");
            return;
        }
        crate::bootstrap();
        DISPOSALS.store(0, Ordering::SeqCst);
        RELEASED_ON_WORKER.store(false, Ordering::SeqCst);
        let pool = objc::arc::pool_push();
        let device = super::super::default_device();
        let queue = unsafe { super::super::new_queue(device, std::ptr::null()) };
        let command = unsafe { super::super::new_command_buffer(queue, std::ptr::null()) };
        assert!(!command.is_null());

        let stack_class = objc::class::lookup(c"__NSStackBlock__").unwrap();
        let stack = GuestStackBlock {
            isa: objc::class::ptr(stack_class).cast(),
            flags: HAS_COPY_DISPOSE,
            reserved: 0,
            invoke: do_nothing,
            descriptor: &GUEST_DESCRIPTOR,
        };
        let guest_block = objc::block::copy((&raw const stack).cast_mut().cast());
        assert!(!guest_block.is_null());
        let progress =
            Arc::clone(&unsafe { object::body::<NativeCommandBuffer>(command) }.progress);
        progress.register(false);
        let capture = Arc::new(HandlerCapture {
            guest_block: AtomicUsize::new(guest_block as usize),
            guest_command: AtomicUsize::new(objc::arc::retain(command) as usize),
            progress,
            scheduled: false,
        });
        let raw = Arc::into_raw(capture);
        let host_stack = HostBlock {
            isa: native_stack_block_isa(),
            flags: HAS_COPY_DISPOSE | HAS_SIGNATURE,
            reserved: 0,
            invoke: finished,
            descriptor: &DESCRIPTOR,
            capture: raw,
        };
        assert!(!host_stack.isa.is_null());
        // Use Apple's actual Blocks runtime. The host copy helper holds the
        // capture, and its dispose helper runs on a foreign host thread.
        let library = unsafe {
            libc::dlopen(
                c"/usr/lib/libSystem.B.dylib".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_LOCAL,
            )
        };
        assert!(!library.is_null());
        let copy_symbol = unsafe { libc::dlsym(library, c"_Block_copy".as_ptr()) };
        let release_symbol = unsafe { libc::dlsym(library, c"_Block_release".as_ptr()) };
        assert!(!copy_symbol.is_null() && !release_symbol.is_null());
        let host_copy: unsafe extern "C" fn(*const c_void) -> *mut c_void =
            unsafe { std::mem::transmute(copy_symbol) };
        let host_release_block: unsafe extern "C" fn(*const c_void) =
            unsafe { std::mem::transmute(release_symbol) };
        let copied = unsafe { host_copy((&raw const host_stack).cast()) };
        assert!(!copied.is_null());
        unsafe { drop(Arc::from_raw(raw)) };
        let host_address = copied as usize;
        std::thread::Builder::new()
            .name("uninitialized-host-metal-callback".into())
            .spawn(move || unsafe { host_release_block(host_address as *const c_void) })
            .unwrap()
            .join()
            .unwrap();
        for _ in 0..200 {
            if DISPOSALS.load(Ordering::SeqCst) != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(DISPOSALS.load(Ordering::SeqCst), 1);
        assert!(
            RELEASED_ON_WORKER.load(Ordering::SeqCst),
            "guest block dispose helper ran outside the Darwin completion worker"
        );
        objc::arc::pool_pop(pool);
        objc::arc::release(queue);
    }
}
