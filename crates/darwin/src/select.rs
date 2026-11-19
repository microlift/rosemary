#![cfg(not(target_os = "macos"))]

use std::ffi::{c_int, c_void};

/// `int select(int, fd_set *, fd_set *, fd_set *, struct timeval *)`
///
/// # Safety
///
/// Each of `readfds`, `writefds` and `errorfds` is a writable `fd_set` or null,
/// and `timeout` is a readable Darwin `struct timeval` or null.
unsafe extern "C" fn select(
    nfds: c_int,
    readfds: *mut c_void,
    writefds: *mut c_void,
    errorfds: *mut c_void,
    timeout: *const c_void,
) -> c_int {
    // SAFETY: the caller's guarantee; sixteen readable bytes when non-null.
    let mut host = match timeout.is_null() {
        true => None,
        false => Some(unsafe { crate::net::sockopt::timeval_to_host(timeout) }),
    };
    let timeout = match &mut host {
        Some(t) => &raw mut *t,
        None => core::ptr::null_mut(),
    };
    // SAFETY: the caller's sets, whose bytes are this host's, and a `timeval`
    // this function owns - which is what keeps the host's rewrite of the time
    // remaining off the guest's own.
    unsafe {
        libc::select(
            nfds,
            readfds.cast(),
            writefds.cast(),
            errorfds.cast(),
            timeout,
        )
    }
}

/// `int pselect(int, fd_set *, fd_set *, fd_set *, const struct timespec *,
/// const sigset_t *)`
///
/// # Safety
///
/// As [`select`], with `timeout` a `struct timespec` - the same two 64-bit
/// words on both platforms - and `sigmask` a readable Darwin `sigset_t`, which
/// is one 32-bit word.
unsafe extern "C" fn pselect(
    nfds: c_int,
    readfds: *mut c_void,
    writefds: *mut c_void,
    errorfds: *mut c_void,
    timeout: *const libc::timespec,
    sigmask: *const u32,
) -> c_int {
    // SAFETY: the caller's guarantee; one readable 32-bit word when non-null.
    let mut mask = match sigmask.is_null() {
        true => None,
        false => Some(crate::signal::mask_to_host(unsafe {
            sigmask.read_unaligned()
        })),
    };
    let mask = match &mut mask {
        Some(m) => &raw const *m,
        None => core::ptr::null(),
    };
    // SAFETY: the caller's sets and timeout, and a `sigset_t` this function
    // owns.
    unsafe {
        libc::pselect(
            nfds,
            readfds.cast(),
            writefds.cast(),
            errorfds.cast(),
            timeout,
            mask,
        )
    }
}

/// Darwin's `FD_SETSIZE`.
const FD_SETSIZE: c_int = 1024;

/// Whether `fd_set` lies on the calling thread's stack.
///
/// Unknown off Linux and FreeBSD, and answered "yes": the check exists to stop
/// a write past a fixed-size set, and a stack set is where that write does
/// damage.
fn on_this_stack(fd_set: *const c_void) -> bool {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        // SAFETY: the calling thread's own attributes, read into a local and
        // destroyed afterwards.
        unsafe {
            let mut attr: libc::pthread_attr_t = std::mem::zeroed();
            // FreeBSD spells this `pthread_attr_get_np`, and fills in an
            // attribute object the caller has already initialised.
            #[cfg(target_os = "freebsd")]
            let rc = match libc::pthread_attr_init(&mut attr) {
                0 => libc::pthread_attr_get_np(libc::pthread_self(), &mut attr),
                rc => rc,
            };
            #[cfg(target_os = "linux")]
            let rc = libc::pthread_getattr_np(libc::pthread_self(), &mut attr);
            if rc != 0 {
                return true;
            }
            let mut base: *mut c_void = std::ptr::null_mut();
            let mut size = 0usize;
            let got = libc::pthread_attr_getstack(&attr, &mut base, &mut size);
            libc::pthread_attr_destroy(&mut attr);
            if got != 0 {
                return true;
            }
            let (lo, hi) = (base as usize, base as usize + size);
            let (start, end) = (fd_set as usize, fd_set as usize + 128);
            start >= lo && end <= hi
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        let _ = fd_set;
        true
    }
}

/// `int __darwin_check_fd_set_overflow(int, const void *, int)`
///
/// What Darwin's `FD_SET`, `FD_CLR` and `FD_ISSET` ask before touching a set:
/// zero means leave it alone. Ported from xnu's libsyscall, less the fault
/// report it files first.
pub extern "C" fn check_fd_set_overflow(
    n: c_int,
    fd_set: *const c_void,
    unlimited_select: c_int,
) -> c_int {
    if n < 0 {
        return 0;
    }
    if n >= FD_SETSIZE && unlimited_select == 0 && on_this_stack(fd_set) {
        return 0;
    }
    1
}

exports::exports! {
    functions {
        "select" => select,
        "pselect" => pselect,
        "__darwin_check_fd_set_overflow" => check_fd_set_overflow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fd_set_checks_refuse_negatives_and_stack_overflows_only() {
        let set = [0u8; 128];
        let heap = vec![0u8; 128];
        assert_eq!(check_fd_set_overflow(-1, set.as_ptr().cast(), 0), 0);
        assert_eq!(check_fd_set_overflow(3, set.as_ptr().cast(), 0), 1);
        assert_eq!(check_fd_set_overflow(2000, set.as_ptr().cast(), 0), 0);
        assert_eq!(check_fd_set_overflow(2000, set.as_ptr().cast(), 1), 1);
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        assert_eq!(check_fd_set_overflow(2000, heap.as_ptr().cast(), 0), 1);
        let _ = heap;
    }

    /// What makes the sets a passthrough: two different declarations over the
    /// same bytes, with the descriptor in the same bit of the same byte.
    #[test]
    fn an_fd_set_is_the_same_bytes_on_both_platforms() {
        assert_eq!(core::mem::size_of::<libc::fd_set>(), 128);
        assert!(cfg!(target_endian = "little"), "the claim rests on this");

        // Descriptor 33 is bit 1 of the second 32-bit word on Darwin and bit 33
        // of the first 64-bit word here, which is byte 4, bit 1 in both.
        // SAFETY: a live `fd_set`, zeroed before anything is set in it.
        let mut host: libc::fd_set = unsafe { core::mem::zeroed() };
        // SAFETY: a live `fd_set` and a descriptor number within `FD_SETSIZE`.
        unsafe { libc::FD_SET(33, &mut host) };
        // SAFETY: reading the same 128 bytes as an array of bytes, which is what
        // a guest's `fd_set` is.
        let bytes = unsafe { core::slice::from_raw_parts((&raw const host).cast::<u8>(), 128) };
        assert_eq!(bytes[4], 0b10);
        assert!(bytes.iter().enumerate().all(|(i, b)| i == 4 || *b == 0));
    }

    /// A pipe with a byte in it is readable, through the entry point a guest is
    /// handed and with a timeout in the guest's own sixteen-byte shape.
    #[test]
    fn a_readable_pipe_is_reported_and_the_guests_timeout_is_not_rewritten() {
        let mut ends = [0 as c_int; 2];
        // SAFETY: a two-element array, which is what `pipe` fills in.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        // SAFETY: the write end of a pipe this test owns.
        assert_eq!(unsafe { libc::write(ends[1], c"x".as_ptr().cast(), 1) }, 1);

        // SAFETY: a live `fd_set`, zeroed and then given one descriptor.
        let mut readfds: libc::fd_set = unsafe { core::mem::zeroed() };
        unsafe { libc::FD_SET(ends[0], &mut readfds) };

        // Darwin's `struct timeval`: eight bytes of seconds, four of
        // microseconds, four of padding left deliberately non-zero.
        let mut guest = [0u8; 16];
        guest[8..12].copy_from_slice(&500_000i32.to_le_bytes());
        guest[12..16].copy_from_slice(&(-1i32).to_le_bytes());

        // SAFETY: a live set and sixteen readable bytes of timeout.
        let ready = unsafe {
            select(
                ends[0] + 1,
                (&raw mut readfds).cast(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                guest.as_ptr().cast(),
            )
        };
        assert_eq!(ready, 1);
        // SAFETY: a live set and a descriptor within `FD_SETSIZE`.
        assert!(unsafe { libc::FD_ISSET(ends[0], &readfds) });
        assert_eq!(
            i32::from_le_bytes(guest[8..12].try_into().unwrap()),
            500_000,
            "Darwin's select leaves the caller's timeout alone"
        );

        // SAFETY: two descriptors this test opened and no longer uses.
        unsafe {
            libc::close(ends[0]);
            libc::close(ends[1]);
        }
    }

    /// The half of `select` that is not a passthrough: a guest's `tv_usec` is
    /// half a word, so a timeout handed across unconverted is a wait of days.
    #[test]
    fn a_timeout_that_expires_expires_in_the_guests_units() {
        // SAFETY: a live `fd_set` with nothing in it, which is a sleep.
        let empty: libc::fd_set = unsafe { core::mem::zeroed() };
        let mut guest = [0u8; 16];
        guest[8..12].copy_from_slice(&1_000i32.to_le_bytes());

        let started = std::time::Instant::now();
        // SAFETY: nothing is being waited on and the timeout is readable.
        let ready = unsafe {
            select(
                0,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                guest.as_ptr().cast(),
            )
        };
        assert_eq!(ready, 0, "nothing became ready");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "a millisecond read as a thousand seconds"
        );
        let _ = empty;
    }
}
