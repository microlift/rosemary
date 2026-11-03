//! Linux: `process_vm_readv` and `process_vm_writev` on this process, which
//! the kernel answers without the ptrace check another process would need.
//! Where a sandbox refuses them, the pipe does the same job a page at a time.

use core::ffi::c_void;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd};

use crate::memory::piped;

pub const NORESERVE: libc::c_int = libc::MAP_NORESERVE;

/// Whether the calls were refused once, so the rest go straight to the pipe.
static REFUSED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn refused(errno: i32) -> bool {
    matches!(errno, libc::ENOSYS | libc::EPERM)
}

/// Copy as far as the kernel will, from `remote` into `local`, returning how far
/// that was. `None` when the call itself is unavailable.
fn transfer(write: bool, remote: usize, local: *mut c_void, len: usize) -> Option<usize> {
    let mut done = 0;
    while done < len {
        let local = libc::iovec {
            iov_base: local.wrapping_byte_add(done),
            iov_len: len - done,
        };
        let remote = libc::iovec {
            iov_base: (remote + done) as *mut c_void,
            iov_len: len - done,
        };
        // SAFETY: one iovec each side; the kernel checks both ranges.
        let n = unsafe {
            match write {
                false => libc::process_vm_readv(libc::getpid(), &local, 1, &remote, 1, 0),
                true => libc::process_vm_writev(libc::getpid(), &local, 1, &remote, 1, 0),
            }
        };
        if n <= 0 {
            if n < 0 && done == 0 && refused(crate::errno::get()) {
                REFUSED.store(true, core::sync::atomic::Ordering::Relaxed);
                return None;
            }
            break;
        }
        done += n as usize;
    }
    Some(done)
}

pub fn read(address: usize, out: &mut [u8]) -> usize {
    if !REFUSED.load(core::sync::atomic::Ordering::Relaxed)
        && let Some(done) = transfer(false, address, out.as_mut_ptr().cast(), out.len())
    {
        return done;
    }
    piped::read(address, out)
}

/// # Safety
///
/// As [`crate::memory::write`].
pub unsafe fn write(address: usize, data: &[u8]) -> usize {
    if !REFUSED.load(core::sync::atomic::Ordering::Relaxed)
        && let Some(done) = transfer(true, address, data.as_ptr().cast_mut().cast(), data.len())
    {
        return done;
    }
    // SAFETY: the caller's obligation.
    unsafe { piped::write(address, data) }
}

pub fn object() -> io::Result<OwnedFd> {
    // SAFETY: a NUL-terminated name, which only labels the object in `/proc`.
    match unsafe { libc::memfd_create(c"rosemary-memory-object".as_ptr(), libc::MFD_CLOEXEC) } {
        -1 => Err(io::Error::last_os_error()),
        // SAFETY: a descriptor the call just opened.
        fd => Ok(unsafe { OwnedFd::from_raw_fd(fd) }),
    }
}

pub fn exclude_from_fork(address: usize, len: usize) -> io::Result<()> {
    // SAFETY: advice about a range, which the kernel checks is mapped.
    match unsafe { libc::madvise(address as *mut c_void, len, libc::MADV_DONTFORK) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}
