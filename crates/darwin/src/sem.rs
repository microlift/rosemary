#![cfg(not(target_os = "macos"))]
//! Named POSIX semaphores, on the host's own, with Darwin's checks in front.
//!
//! Darwin's kernel refuses a starting value above `SEM_VALUE_MAX` (32767) and a
//! name longer than `PSEMNAMLEN`; glibc takes values up to `INT_MAX`. A guest
//! that passed garbage for the value would get a semaphore that is already
//! signalled thousands of times here and a failure there.
//!
//! A Darwin `sem_t *` is a descriptor number the kernel validates, so a call on
//! a bad handle - `SEM_FAILED` included - is `EBADF`. glibc dereferences it.
//! Every handle this module returned is remembered, and anything else is
//! refused before it reaches the host.
//!
//! Darwin has no unnamed semaphores: `sem_init`, `sem_destroy` and
//! `sem_getvalue` are `ENOSYS`. A Darwin `sem_t` is four bytes, and glibc's
//! `sem_init` would write far more than that into it.

use std::collections::BTreeMap;
use std::ffi::{CStr, c_char, c_int, c_uint};
use std::sync::{Mutex, MutexGuard};

/// `SEM_VALUE_MAX` from Darwin's `<sys/semaphore.h>`.
const SEM_VALUE_MAX: c_uint = 32767;

/// `PSEMNAMLEN` from `<sys/posix_sem.h>`, counted with the terminator as the
/// kernel's `copyinstr` counts it.
const PSEMNAMLEN: usize = 31;

/// Darwin's `SEM_FAILED`, which is `(sem_t *)-1` where glibc's is null.
pub const SEM_FAILED: *mut libc::sem_t = usize::MAX as *mut libc::sem_t;

/// Live handles and how many opens each one stands for. glibc hands one
/// mapping back for every open of a name, and counts the closes itself.
fn handles() -> MutexGuard<'static, BTreeMap<usize, usize>> {
    static OPEN: Mutex<BTreeMap<usize, usize>> = Mutex::new(BTreeMap::new());
    OPEN.lock().unwrap_or_else(|e| e.into_inner())
}

fn is_open(sem: *mut libc::sem_t) -> bool {
    handles().contains_key(&(sem as usize))
}

fn fail(error: i32) -> c_int {
    crate::errno::set(error);
    -1
}

/// `sem_open`, with the flags already the host's.
///
/// `creates` is whether the guest's flags asked for `O_CREAT`, which is when
/// the mode and value were passed at all.
///
/// # Safety
///
/// `name` is null or a NUL-terminated string.
pub unsafe fn open(
    name: *const c_char,
    host_oflag: c_int,
    creates: bool,
    mode: c_uint,
    value: c_uint,
) -> *mut libc::sem_t {
    if name.is_null() {
        fail(libc::EFAULT);
        return SEM_FAILED;
    }
    // SAFETY: non-null and NUL-terminated by contract.
    if unsafe { CStr::from_ptr(name) }.to_bytes().len() + 1 > PSEMNAMLEN {
        fail(libc::ENAMETOOLONG);
        return SEM_FAILED;
    }
    // Checked before the name is looked up, as the kernel does, so it fails
    // even for a semaphore that already exists.
    if creates && value > SEM_VALUE_MAX {
        fail(libc::EINVAL);
        return SEM_FAILED;
    }
    let Some(host) = (unsafe { crate::shm::host_name(name) }) else {
        return SEM_FAILED;
    };
    // SAFETY: a live NUL-terminated name, and the arguments `O_CREAT` asks for.
    let sem = unsafe {
        match creates {
            true => libc::sem_open(
                host.as_ptr(),
                host_oflag,
                mode as libc::mode_t as c_uint,
                value,
            ),
            false => libc::sem_open(host.as_ptr(), host_oflag),
        }
    };
    if sem == libc::SEM_FAILED || sem == SEM_FAILED {
        return SEM_FAILED;
    }
    *handles().entry(sem as usize).or_insert(0) += 1;
    sem
}

/// # Safety
///
/// `sem` is any value; only handles [`open`] returned reach the host.
pub unsafe extern "C" fn sem_close(sem: *mut libc::sem_t) -> c_int {
    {
        let mut open = handles();
        let Some(count) = open.get_mut(&(sem as usize)) else {
            return fail(libc::EBADF);
        };
        *count -= 1;
        if *count == 0 {
            open.remove(&(sem as usize));
        }
    }
    // SAFETY: a handle the host's `sem_open` returned and not yet closed.
    unsafe { libc::sem_close(sem) }
}

/// # Safety
///
/// As [`sem_close`].
pub unsafe extern "C" fn sem_wait(sem: *mut libc::sem_t) -> c_int {
    if !is_open(sem) {
        return fail(libc::EBADF);
    }
    crate::idle::reached(crate::idle::Idle::Wait);
    // SAFETY: an open handle.
    unsafe { libc::sem_wait(sem) }
}

/// # Safety
///
/// As [`sem_close`].
pub unsafe extern "C" fn sem_trywait(sem: *mut libc::sem_t) -> c_int {
    if !is_open(sem) {
        return fail(libc::EBADF);
    }
    // SAFETY: an open handle.
    unsafe { libc::sem_trywait(sem) }
}

/// # Safety
///
/// As [`sem_close`].
pub unsafe extern "C" fn sem_post(sem: *mut libc::sem_t) -> c_int {
    if !is_open(sem) {
        return fail(libc::EBADF);
    }
    // SAFETY: an open handle.
    unsafe { libc::sem_post(sem) }
}

/// # Safety
///
/// `name` is null or a NUL-terminated string.
pub unsafe extern "C" fn sem_unlink(name: *const c_char) -> c_int {
    if name.is_null() {
        return fail(libc::EFAULT);
    }
    // SAFETY: as above.
    if unsafe { CStr::from_ptr(name) }.to_bytes().len() + 1 > PSEMNAMLEN {
        return fail(libc::ENAMETOOLONG);
    }
    let Some(host) = (unsafe { crate::shm::host_name(name) }) else {
        return -1;
    };
    // SAFETY: a live NUL-terminated name.
    unsafe { libc::sem_unlink(host.as_ptr()) }
}

unsafe extern "C" fn sem_init(_sem: *mut c_int, _shared: c_int, _value: c_uint) -> c_int {
    fail(libc::ENOSYS)
}

unsafe extern "C" fn sem_destroy(_sem: *mut c_int) -> c_int {
    fail(libc::ENOSYS)
}

unsafe extern "C" fn sem_getvalue(_sem: *mut c_int, _value: *mut c_int) -> c_int {
    fail(libc::ENOSYS)
}

exports::exports! {
    functions {
        "sem_close" => sem_close,
        "sem_wait" => sem_wait,
        "sem_trywait" => sem_trywait,
        "sem_post" => sem_post,
        "sem_unlink" => sem_unlink,
        "sem_init" => sem_init,
        "sem_destroy" => sem_destroy,
        "sem_getvalue" => sem_getvalue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    const O_CREAT: c_int = libc::O_CREAT;
    const O_EXCL: c_int = libc::O_EXCL;

    fn name(tag: &str) -> std::ffi::CString {
        std::ffi::CString::new(format!("rmsem{}{tag}", std::process::id())).unwrap()
    }

    fn create(name: &CStr, value: c_uint) -> *mut libc::sem_t {
        unsafe { sem_unlink(name.as_ptr()) };
        unsafe { open(name.as_ptr(), O_CREAT | O_EXCL, true, 0o600, value) }
    }

    /// Darwin's kernel limit, not glibc's: a value a guest left as garbage
    /// fails instead of making a semaphore that is already signalled.
    #[test]
    fn a_value_above_darwins_limit_is_refused() {
        let n = name("max");
        for value in [SEM_VALUE_MAX + 1, 0x7fff_0000, u32::MAX] {
            assert_eq!(create(&n, value), SEM_FAILED);
            assert_eq!(crate::errno::get(), libc::EINVAL);
        }
        let sem = create(&n, SEM_VALUE_MAX);
        assert_ne!(sem, SEM_FAILED);
        unsafe {
            assert_eq!(sem_close(sem), 0);
            assert_eq!(sem_unlink(n.as_ptr()), 0);
        }
    }

    #[test]
    fn a_name_past_psemnamlen_is_too_long() {
        let long = std::ffi::CString::new("n".repeat(PSEMNAMLEN)).unwrap();
        assert_eq!(create(&long, 0), SEM_FAILED);
        assert_eq!(crate::errno::get(), libc::ENAMETOOLONG);
    }

    /// `SEM_FAILED` and anything else this module did not hand out is a bad
    /// descriptor, as on Darwin, rather than a pointer for glibc to follow.
    #[test]
    fn a_bad_handle_is_ebadf_rather_than_dereferenced() {
        let mut junk = [0u64; 4];
        for sem in [SEM_FAILED, std::ptr::null_mut(), junk.as_mut_ptr().cast()] {
            unsafe {
                assert_eq!(sem_post(sem), -1);
                assert_eq!(crate::errno::get(), libc::EBADF);
                assert_eq!(sem_wait(sem), -1);
                assert_eq!(sem_trywait(sem), -1);
                assert_eq!(sem_close(sem), -1);
            }
        }
        assert_eq!(junk, [0; 4]);
    }

    #[test]
    fn unnamed_semaphores_are_enosys_and_write_nothing() {
        let mut word = [0x5a5a_5a5au32; 8];
        unsafe {
            assert_eq!(sem_init(word.as_mut_ptr().cast(), 0, 1), -1);
            assert_eq!(crate::errno::get(), libc::ENOSYS);
            assert_eq!(sem_destroy(word.as_mut_ptr().cast()), -1);
        }
        assert_eq!(word, [0x5a5a_5a5a; 8]);
    }

    /// Two opens of one name are one semaphore, and a close of one leaves the
    /// other working.
    #[test]
    fn two_opens_of_a_name_share_one_count() {
        let n = name("share");
        let first = create(&n, 0);
        assert_ne!(first, SEM_FAILED);
        let second = unsafe { open(n.as_ptr(), 0, false, 0, 0) };
        assert_ne!(second, SEM_FAILED);
        unsafe {
            assert_eq!(sem_post(first), 0);
            assert_eq!(sem_trywait(second), 0);
            assert_eq!(sem_trywait(second), -1);
            assert_eq!(sem_close(first), 0);
            assert_eq!(sem_post(second), 0);
            assert_eq!(sem_trywait(second), 0);
            assert_eq!(sem_close(second), 0);
            assert_eq!(sem_post(second), -1, "both opens are closed");
            assert_eq!(sem_unlink(n.as_ptr()), 0);
        }
    }

    /// Producers and consumers handing tokens through one named semaphore:
    /// every post is taken exactly once.
    #[test]
    fn posts_and_waits_balance_across_threads() {
        const THREADS: u32 = 4;
        const EACH: u32 = 5_000;
        let n = name("pc");
        let sem = create(&n, 0) as usize;
        assert_ne!(sem, SEM_FAILED as usize);
        let taken = Arc::new(AtomicU32::new(0));
        let mut threads = Vec::new();
        for _ in 0..THREADS {
            threads.push(std::thread::spawn(move || {
                for _ in 0..EACH {
                    assert_eq!(unsafe { sem_post(sem as *mut _) }, 0);
                }
            }));
            let taken = Arc::clone(&taken);
            threads.push(std::thread::spawn(move || {
                for _ in 0..EACH {
                    assert_eq!(unsafe { sem_wait(sem as *mut _) }, 0);
                    taken.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(taken.load(Ordering::Relaxed), THREADS * EACH);
        unsafe {
            assert_eq!(sem_trywait(sem as *mut _), -1, "nothing left over");
            assert_eq!(sem_close(sem as *mut _), 0);
            assert_eq!(sem_unlink(n.as_ptr()), 0);
        }
    }
}
