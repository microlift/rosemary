//! Copies into and out of this process's own memory that fail instead of
//! faulting.
//!
//! The kernel does the copy, so an address that is not mapped, or not mapped
//! the way the copy needs, comes back as an error rather than a signal. Both
//! are async-signal-safe: no allocation, no locks.
//!
//! Also the anonymous shared-memory objects a Mach memory entry is made of,
//! and private anonymous memory committed only as it is touched.

use core::ffi::{c_int, c_void};
use core::ptr::NonNull;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};

use crate::sys;

/// Why a copy stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultKind {
    /// Nothing is mapped there.
    Unmapped,
    /// Something is, but not readable, or not writable, as the copy needed.
    Protection,
}

/// Where a copy stopped: `offset` bytes in, at a page of `kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    pub offset: usize,
    pub kind: FaultKind,
}

/// Copy `out.len()` bytes from `address` in this process into `out`.
///
/// On a fault, `out` holds what was copied before it.
pub fn read(address: usize, out: &mut [u8]) -> Result<(), Fault> {
    let copied = sys::memory::read(address, out);
    match copied == out.len() {
        true => Ok(()),
        false => Err(fault(address, copied)),
    }
}

/// Copy `data` to `address` in this process.
///
/// On a fault, the bytes before it have been written and the rest have not.
///
/// # Safety
///
/// Writing there must not break anything this process relies on; the copy is
/// checked against the page tables, not against Rust's aliasing rules.
pub unsafe fn write(address: usize, data: &[u8]) -> Result<(), Fault> {
    // SAFETY: the caller's obligation.
    let copied = unsafe { sys::memory::write(address, data) };
    match copied == data.len() {
        true => Ok(()),
        false => Err(fault(address, copied)),
    }
}

/// Whether the page at `address` is mapped at all. `msync` asks the map
/// without touching the page, and answers `ENOMEM` for a hole.
pub fn mapped(address: usize) -> bool {
    let page = crate::system::page_size() as usize;
    let start = address & !(page - 1);
    // SAFETY: `MS_ASYNC` on a page-aligned range only looks the range up.
    unsafe { libc::msync(start as *mut c_void, page, libc::MS_ASYNC) == 0 }
}

/// A new shared-memory object of `len` zero bytes, named nowhere: every
/// mapping of it shares the same pages, and it lives as long as a descriptor
/// for it or a mapping of it does.
pub fn object(len: u64) -> io::Result<OwnedFd> {
    let fd = sys::memory::object()?;
    let len = libc::off_t::try_from(len).map_err(|_| io::Error::from_raw_os_error(libc::EFBIG))?;
    // SAFETY: a descriptor this call just opened.
    match unsafe { libc::ftruncate(fd.as_raw_fd(), len) } {
        0 => Ok(fd),
        _ => Err(io::Error::last_os_error()),
    }
}

/// Leave `[address, address + len)` out of any child this process forks, as
/// Mach's `VM_INHERIT_NONE` does.
pub fn exclude_from_fork(address: usize, len: usize) -> io::Result<()> {
    sys::memory::exclude_from_fork(address, len)
}

/// The `mmap` flag that asks for a mapping not to be charged against commit
/// until it is touched, or `0` on a host that never charges up front.
pub const NORESERVE: c_int = sys::memory::NORESERVE;

/// `len` bytes of private, zero-filled memory at an address the kernel picks,
/// committed only as it is touched. `PROT_NONE` reserves address space alone.
pub fn anonymous(len: usize, prot: c_int) -> io::Result<NonNull<u8>> {
    // SAFETY: a new mapping at no fixed address cannot displace anything.
    let mapped = unsafe {
        libc::mmap(
            core::ptr::null_mut(),
            len,
            prot,
            libc::MAP_PRIVATE | libc::MAP_ANON | NORESERVE,
            -1,
            0,
        )
    };
    match mapped {
        libc::MAP_FAILED => Err(io::Error::last_os_error()),
        mapped => NonNull::new(mapped.cast()).ok_or_else(|| io::Error::other("mapped at null")),
    }
}

fn fault(address: usize, offset: usize) -> Fault {
    let kind = match mapped(address.wrapping_add(offset)) {
        true => FaultKind::Protection,
        false => FaultKind::Unmapped,
    };
    Fault { offset, kind }
}

/// [`object`] where the host has no anonymous one: a POSIX object under a name
/// no other process uses, unlinked as soon as it is open.
#[cfg_attr(any(target_os = "linux", target_os = "freebsd"), allow(dead_code))]
pub(crate) mod named {
    use std::io;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::sync::atomic::{AtomicU32, Ordering};

    pub fn object() -> io::Result<OwnedFd> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            // SAFETY: `getpid` has no preconditions.
            let name = format!("/rosemary.{}.{n}\0", unsafe { libc::getpid() });
            let mode: libc::mode_t = 0o600;
            // Variadic on macOS, where the mode arrives promoted.
            #[cfg(target_os = "macos")]
            let mode = mode as libc::c_uint;
            // SAFETY: a NUL-terminated name; `O_EXCL` so nothing else's
            // object is opened.
            let fd = unsafe {
                libc::shm_open(
                    name.as_ptr().cast(),
                    libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                    mode,
                )
            };
            if fd < 0 {
                match io::Error::last_os_error() {
                    e if e.raw_os_error() == Some(libc::EEXIST) => continue,
                    e => return Err(e),
                }
            }
            // SAFETY: the name just created, and the descriptor just opened.
            unsafe {
                libc::shm_unlink(name.as_ptr().cast());
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                return Ok(OwnedFd::from_raw_fd(fd));
            }
        }
    }
}

/// The copy any host can do: through a pipe, whose `write` and `read` answer
/// `EFAULT` for a bad buffer where a plain copy would fault. One page at a time,
/// so a short copy names the page that stopped it and never fills the pipe.
pub(crate) mod piped {
    use core::ffi::c_void;

    struct Pipe([libc::c_int; 2]);

    impl Pipe {
        fn new() -> Option<Pipe> {
            let mut fds = [0; 2];
            // SAFETY: a writable two-element array.
            (unsafe { libc::pipe(fds.as_mut_ptr()) } == 0).then_some(Pipe(fds))
        }

        /// Throw away whatever a failed copy left in the pipe.
        fn drain(&self, mut left: usize) {
            let mut scratch = [0u8; 256];
            while left > 0 {
                let n = left.min(scratch.len());
                // SAFETY: a local buffer of at least `n` bytes.
                let got = unsafe { libc::read(self.0[0], scratch.as_mut_ptr().cast(), n) };
                if got <= 0 {
                    return;
                }
                left -= got as usize;
            }
        }
    }

    impl Drop for Pipe {
        fn drop(&mut self) {
            // SAFETY: two descriptors this value opened.
            unsafe {
                libc::close(self.0[0]);
                libc::close(self.0[1]);
            }
        }
    }

    /// The length of the next step from `at`: to the end of its page, at most.
    fn step(at: usize, left: usize) -> usize {
        let page = crate::system::page_size() as usize;
        left.min(page - at % page)
    }

    pub fn read(address: usize, out: &mut [u8]) -> usize {
        let Some(pipe) = Pipe::new() else {
            return 0;
        };
        let mut done = 0;
        while done < out.len() {
            let n = step(address + done, out.len() - done);
            // SAFETY: the kernel checks the source; the pipe is empty and a
            // page fits in it.
            let put = unsafe { libc::write(pipe.0[1], (address + done) as *const c_void, n) };
            if put != n as isize {
                pipe.drain(put.max(0) as usize);
                return done;
            }
            // SAFETY: `out` has room for `n` more bytes at `done`.
            let got = unsafe { libc::read(pipe.0[0], out[done..].as_mut_ptr().cast(), n) };
            if got != n as isize {
                return done;
            }
            done += n;
        }
        done
    }

    /// # Safety
    ///
    /// As [`super::write`].
    pub unsafe fn write(address: usize, data: &[u8]) -> usize {
        let Some(pipe) = Pipe::new() else {
            return 0;
        };
        let mut done = 0;
        while done < data.len() {
            let n = step(address + done, data.len() - done);
            // SAFETY: `data` holds `n` more bytes at `done`; a page fits in
            // the empty pipe.
            let put = unsafe { libc::write(pipe.0[1], data[done..].as_ptr().cast(), n) };
            if put != n as isize {
                return done;
            }
            // SAFETY: the kernel checks the destination; the caller vouches
            // for writing there.
            let got = unsafe { libc::read(pipe.0[0], (address + done) as *mut c_void, n) };
            if got != n as isize {
                pipe.drain(n - got.max(0) as usize);
                return done;
            }
            done += n;
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(prot: libc::c_int) -> (*mut u8, usize) {
        let page = crate::system::page_size() as usize;
        // SAFETY: a fresh anonymous mapping of two pages.
        let at = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                2 * page,
                prot,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        assert_ne!(at, libc::MAP_FAILED);
        (at.cast(), page)
    }

    #[test]
    fn anonymous_memory_reads_zero_and_takes_writes() {
        let page = crate::system::page_size() as usize;
        let at = anonymous(page, libc::PROT_READ | libc::PROT_WRITE).unwrap();
        // SAFETY: a page of read-write memory just mapped.
        unsafe {
            assert_eq!(*at.as_ptr(), 0);
            *at.as_ptr() = 1;
            libc::munmap(at.as_ptr().cast(), page);
        }
    }

    #[test]
    fn copies_both_ways_across_a_page_boundary() {
        let (at, page) = map(libc::PROT_READ | libc::PROT_WRITE);
        let start = at as usize + page - 3;
        unsafe { write(start, b"rosemary").unwrap() };
        let mut back = [0u8; 8];
        read(start, &mut back).unwrap();
        assert_eq!(&back, b"rosemary");
        unsafe { libc::munmap(at.cast(), 2 * page) };
    }

    #[test]
    fn a_hole_is_unmapped_and_stops_where_it_starts() {
        let (at, page) = map(libc::PROT_READ | libc::PROT_WRITE);
        // Leave the second page a hole.
        unsafe { libc::munmap(at.add(page).cast(), page) };
        let mut out = [0u8; 16];
        let fault = read(at as usize + page - 8, &mut out).unwrap_err();
        assert_eq!(
            fault,
            Fault {
                offset: 8,
                kind: FaultKind::Unmapped
            }
        );
        unsafe { libc::munmap(at.cast(), page) };
    }

    #[test]
    fn a_read_only_page_refuses_a_write_with_protection() {
        let (at, page) = map(libc::PROT_READ);
        let fault = unsafe { write(at as usize, b"x") }.unwrap_err();
        assert_eq!(fault.kind, FaultKind::Protection);
        assert_eq!(fault.offset, 0);
        let mut out = [1u8; 4];
        read(at as usize, &mut out).unwrap();
        assert_eq!(out, [0; 4]);
        unsafe { libc::munmap(at.cast(), 2 * page) };
    }

    #[test]
    fn an_unreadable_page_refuses_a_read_with_protection() {
        let (at, page) = map(libc::PROT_NONE);
        let mut out = [0u8; 4];
        assert_eq!(
            read(at as usize, &mut out).unwrap_err().kind,
            FaultKind::Protection
        );
        unsafe { libc::munmap(at.cast(), 2 * page) };
    }

    #[test]
    fn the_pipe_fallback_agrees() {
        let (at, page) = map(libc::PROT_READ);
        unsafe { libc::munmap(at.add(page).cast(), page) };
        assert_eq!(unsafe { piped::write(at as usize, b"abc") }, 0);
        let mut out = [9u8; 4];
        assert_eq!(piped::read(at as usize + page - 2, &mut out), 2);
        unsafe { libc::munmap(at.cast(), page) };
    }
}
