// Compiled and tested on macOS, bound to nothing there: the host's own kernel answers
// these. See [`symbol`].
#![cfg_attr(target_os = "macos", allow(dead_code))]

use std::ffi::c_void;
use std::sync::atomic::AtomicU32;

use rustix::time::ClockId;

use crate::wait_word::{self, Scope, WakeCount};

/// The operation, in the low byte of the first argument. From `<sys/ulock.h>`.
///
/// The whole range is written out even though the plain 32-bit private lock needs no
/// special handling: the validity check below is a range over these, so a name missing
/// here would be a valid opcode this module refused.
#[allow(dead_code)]
mod op {
    pub const MASK: u32 = 0x0000_00ff;

    pub const COMPARE_AND_WAIT: u32 = 1;
    pub const UNFAIR_LOCK: u32 = 2;
    pub const COMPARE_AND_WAIT_SHARED: u32 = 3;
    pub const UNFAIR_LOCK64_SHARED: u32 = 4;
    pub const COMPARE_AND_WAIT64: u32 = 5;
    pub const COMPARE_AND_WAIT64_SHARED: u32 = 6;
}

/// The flags, in the upper three bytes. Only the three that change behaviour here are
/// named; the rest are scheduler hints a host cannot act on and are ignored.
mod flag {
    pub const WAKE_ALL: u32 = 0x0000_0100;
    pub const WAKE_THREAD: u32 = 0x0000_0200;
    pub const WAKE_ALLOW_NON_OWNER: u32 = 0x0000_0400;
    pub const WAIT_WORKQ_DATA_CONTENTION: u32 = 0x0001_0000;
    pub const WAIT_CANCEL_POINT: u32 = 0x0002_0000;
    pub const WAIT_ADAPTIVE_SPIN: u32 = 0x0004_0000;
    pub const NO_ERRNO: u32 = 0x0100_0000;
    pub const DEADLINE: u32 = 0x0200_0000;

    pub const WAIT_MASK: u32 =
        NO_ERRNO | DEADLINE | WAIT_WORKQ_DATA_CONTENTION | WAIT_CANCEL_POINT | WAIT_ADAPTIVE_SPIN;
    pub const WAKE_MASK: u32 = NO_ERRNO | WAKE_ALL | WAKE_THREAD | WAKE_ALLOW_NON_OWNER;
}

#[inline]
fn valid_opcode(opcode: u32) -> bool {
    matches!(
        opcode,
        op::COMPARE_AND_WAIT
            | op::UNFAIR_LOCK
            | op::COMPARE_AND_WAIT_SHARED
            | op::COMPARE_AND_WAIT64
            | op::COMPARE_AND_WAIT64_SHARED
    )
}

/// Whether an opcode compares eight bytes rather than four.
#[inline]
fn is_64_bit(opcode: u32) -> bool {
    matches!(
        opcode,
        op::COMPARE_AND_WAIT64 | op::COMPARE_AND_WAIT64_SHARED | op::UNFAIR_LOCK64_SHARED
    )
}

/// Whether an opcode names memory another process may also be waiting on.
///
/// A private futex is the cheaper one, and is wrong if the word really is shared - so
/// this errs toward the shared form for the operations that say so.
#[inline]
fn is_shared(opcode: u32) -> bool {
    matches!(
        opcode,
        op::COMPARE_AND_WAIT_SHARED | op::COMPARE_AND_WAIT64_SHARED | op::UNFAIR_LOCK64_SHARED
    )
}

/// Report `error` the way `operation` asked for it.
///
/// With `ULF_NO_ERRNO` the negated error *is* the return value; without it, the libc
/// convention. A caller that asked for the first is one that cannot touch `errno`.
fn fail(operation: u32, error: i32) -> i32 {
    if operation & flag::NO_ERRNO != 0 {
        return -crate::errno::from_host(error);
    }
    crate::errno::set(error);
    -1
}

/// `int __ulock_wait(uint32_t operation, void *addr, uint64_t value, uint32_t timeout_us)`.
///
/// Blocks while the word at `addr` still equals `value`. The timeout is microseconds,
/// and zero means forever.
///
/// A value that no longer matches returns 0 rather than an error. Every caller of a
/// compare-and-wait re-reads the word and loops, so 0 - "you may have been woken, look
/// again" - is the answer that is correct for all of them, and the one that cannot
/// turn a race into a caller's error path.
///
/// # Safety
///
/// `addr` is a readable word of at least the operation's width.
pub unsafe extern "C" fn wait(
    operation: u32,
    addr: *mut c_void,
    value: u64,
    timeout_us: u32,
) -> i32 {
    unsafe {
        wait_ns(
            operation,
            addr,
            value,
            (timeout_us as u64).saturating_mul(1_000),
        )
    }
}

/// `int __ulock_wait2(uint32_t, void *, uint64_t, uint64_t timeout_ns, uint64_t)`.
///
/// The same call with a nanosecond timeout. The fifth argument names a thread to
/// attribute the wait to, for the scheduler's benefit; it changes no outcome here.
///
/// # Safety
///
/// As [`wait`].
pub unsafe extern "C" fn wait2(
    operation: u32,
    addr: *mut c_void,
    value: u64,
    timeout_ns: u64,
    _owner: u64,
) -> i32 {
    unsafe { wait_ns(operation, addr, value, timeout_ns) }
}

/// # Safety
///
/// As [`wait`].
unsafe fn wait_ns(operation: u32, addr: *mut c_void, value: u64, timeout_ns: u64) -> i32 {
    let opcode = operation & op::MASK;
    let flags = operation & !op::MASK;
    let alignment = if is_64_bit(opcode) { 8 } else { 4 };
    if !valid_opcode(opcode)
        || flags & !flag::WAIT_MASK != 0
        || addr.is_null()
        || (addr as usize) & (alignment - 1) != 0
    {
        return fail(operation, libc::EINVAL);
    }
    // A futex compares 32 bits, so the upper half of a 64-bit operation is checked
    // here instead. This narrows the race rather than closing it: the word can still
    // change between this load and the blocking call. That is survivable because the
    // thread that changes it goes on to call `__ulock_wake`, and because the caller
    // re-checks after every return.
    if is_64_bit(opcode) {
        // SAFETY: the caller states `addr` is a readable eight-byte word.
        if unsafe { (addr as *const u64).read_volatile() } != value {
            return 0;
        }
    }
    // SAFETY: the caller states `addr` is a readable word; a 64-bit operation was
    // just confirmed to have eight readable bytes, and the low four are the futex's
    // on a little-endian host.
    let timeout = if timeout_ns == 0 {
        None
    } else if flags & flag::DEADLINE != 0 {
        Some(timeout_ns.saturating_sub(monotonic_nanoseconds()))
    } else {
        Some(timeout_ns)
    };
    let scope = if is_shared(opcode) {
        Scope::Shared
    } else {
        Scope::Private
    };
    // SAFETY: validated above and guaranteed readable by the caller.
    let word = unsafe { AtomicU32::from_ptr(addr.cast()) };
    match unsafe { wait_word::wait_u32(word, value as u32, timeout, scope) } {
        Ok(()) => 0,
        Err(wait_word::WaitError::Changed) => 0,
        Err(error) => fail(operation, error.raw_os_error()),
    }
}

/// `int __ulock_wake(uint32_t operation, void *addr, uint64_t wake_value)`.
///
/// Darwin reports "nobody was waiting" as `ENOENT`, and callers use it to skip work -
/// so a wake that reached no one must not be reported as success.
///
/// # Safety
///
/// `addr` is a readable word.
pub unsafe extern "C" fn wake(operation: u32, addr: *mut c_void, _wake_value: u64) -> i32 {
    let opcode = operation & op::MASK;
    let flags = operation & !op::MASK;
    if addr.is_null()
        || !valid_opcode(opcode)
        || flags & !flag::WAKE_MASK != 0
        || (flags & flag::WAKE_THREAD != 0
            && (flags & flag::WAKE_ALL != 0 || opcode == op::UNFAIR_LOCK))
        || (flags & flag::WAKE_ALLOW_NON_OWNER != 0 && opcode != op::UNFAIR_LOCK)
    {
        return fail(operation, libc::EINVAL);
    }
    // `ULF_WAKE_THREAD` names one thread by port, which a futex cannot single out.
    // Waking all of them is what makes the named thread certainly among the woken;
    // the others re-check their word and block again, which they must tolerate
    // anyway.
    let all = operation & (flag::WAKE_ALL | flag::WAKE_THREAD) != 0;
    // SAFETY: the caller states `addr` is a readable word.
    let scope = if is_shared(opcode) {
        Scope::Shared
    } else {
        Scope::Private
    };
    let count = if all { WakeCount::All } else { WakeCount::One };
    // SAFETY: validated above and guaranteed readable by the caller.
    let word = unsafe { AtomicU32::from_ptr(addr.cast()) };
    match unsafe { wait_word::wake_u32(word, count, scope) } {
        Ok(0) => fail(operation, libc::ENOENT),
        Ok(_) => 0,
        // The old Linux adapter collapsed a host wake failure into zero woken,
        // which Darwin reports as ENOENT. Preserve that exported behavior here.
        Err(_) => fail(operation, libc::ENOENT),
    }
}

#[inline]
fn monotonic_nanoseconds() -> u64 {
    let now = {
        let _host = crate::syscall::ScopedHostSyscallContext::new();
        rustix::time::clock_gettime(ClockId::Monotonic)
    };
    (now.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(now.tv_nsec as u64)
}

/// Empty on macOS, where the host's own kernel answers these.
#[cfg(target_os = "macos")]
exports::exports! {}

#[cfg(not(target_os = "macos"))]
exports::exports! {
    functions {
        // One underscore from C mangling on top of the two in the identifier.
        "__ulock_wait" => wait,
        "__ulock_wait2" => wait2,
        "__ulock_wake" => wake,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// The whole point: a thread blocked on a word wakes when the word changes and
    /// its owner wakes it. A test that hangs is a test that failed.
    #[test]
    fn a_waiter_wakes_when_the_word_changes() {
        let word = Arc::new(AtomicU32::new(7));
        let theirs = Arc::clone(&word);
        let waiter = std::thread::spawn(move || {
            let at = theirs.as_ptr() as *mut c_void;
            // Loop as every real caller does: a return is permission to re-check,
            // not proof the value changed.
            while theirs.load(Ordering::Acquire) == 7 {
                // SAFETY: `at` points at a live `AtomicU32`.
                unsafe { wait(op::COMPARE_AND_WAIT, at, 7, 0) };
            }
            theirs.load(Ordering::Acquire)
        });
        // Give the waiter time to reach the blocking call, so the wake below is the
        // thing that releases it rather than the initial compare.
        std::thread::sleep(std::time::Duration::from_millis(50));
        word.store(9, Ordering::Release);
        // SAFETY: the word is live for as long as this scope.
        unsafe {
            wake(
                op::COMPARE_AND_WAIT | flag::WAKE_ALL,
                word.as_ptr() as *mut c_void,
                0,
            )
        };
        assert_eq!(waiter.join().expect("the waiter wakes"), 9);
    }

    /// A word that already disagrees must not block at all.
    #[test]
    fn a_stale_value_returns_immediately() {
        let word = AtomicU32::new(1);
        let at = &word as *const _ as *mut c_void;
        // SAFETY: a live local. Would hang if this blocked, which is the assertion.
        assert_eq!(unsafe { wait(op::COMPARE_AND_WAIT, at, 2, 0) }, 0);
    }

    /// The 64-bit compare must see the half a futex cannot.
    #[test]
    fn a_64_bit_compare_checks_the_upper_half() {
        let word: u64 = 0xdead_0000_0000_0001;
        let at = &word as *const _ as *mut c_void;
        // The low 32 bits match and the high ones do not, so this must return
        // rather than block on the futex's narrower view.
        // SAFETY: a live eight-byte local.
        assert_eq!(unsafe { wait(op::COMPARE_AND_WAIT64, at, 1, 0) }, 0);
    }

    #[test]
    fn a_timeout_is_reported_and_bounded() {
        let word = AtomicU32::new(4);
        let at = &word as *const _ as *mut c_void;
        let started = std::time::Instant::now();
        // SAFETY: a live local.
        let rc = unsafe { wait(op::COMPARE_AND_WAIT, at, 4, 30_000) };
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "must not block forever"
        );
        #[cfg(target_os = "linux")]
        {
            assert_eq!(rc, -1);
            assert_eq!(crate::errno::get(), libc::ETIMEDOUT);
            crate::errno::set(0);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = rc;
    }

    /// `ULF_NO_ERRNO` callers run where `errno` may not be touched, so the error has
    /// to come back in the return value.
    #[test]
    fn no_errno_returns_the_error_instead_of_setting_it() {
        crate::errno::set(0);
        // An unknown opcode, which fails without blocking.
        // SAFETY: the address is never dereferenced on the refusing path.
        let rc = unsafe { wait(0xfe | flag::NO_ERRNO, 8 as *mut c_void, 0, 0) };
        assert_eq!(rc, -libc::EINVAL);
        assert_eq!(crate::errno::get(), 0, "errno must be untouched");
    }

    /// Darwin's callers read `ENOENT` as "nobody was waiting" and skip work on it.
    #[test]
    #[cfg(target_os = "linux")]
    fn waking_nobody_says_so() {
        let word = AtomicU32::new(0);
        // SAFETY: a live local.
        let rc = unsafe { wake(op::COMPARE_AND_WAIT, word.as_ptr() as *mut c_void, 0) };
        assert_eq!(rc, -1);
        assert_eq!(crate::errno::get(), libc::ENOENT);
        crate::errno::set(0);
    }

    #[test]
    fn invalid_opcode_flags_and_alignment_are_rejected() {
        let mut word = 1u64;
        let at = (&mut word as *mut u64).cast::<c_void>();
        // Opcode four is declared for historical/debugging purposes but is not
        // accepted by XNU's wait or wake syscall paths.
        assert_eq!(unsafe { wait(op::UNFAIR_LOCK64_SHARED, at, 1, 1) }, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
        assert_eq!(
            unsafe { wait(op::COMPARE_AND_WAIT | flag::WAKE_ALL, at, 1, 1) },
            -1
        );
        assert_eq!(crate::errno::get(), libc::EINVAL);
        assert_eq!(
            unsafe { wait(op::COMPARE_AND_WAIT, (at as *mut u8).add(1).cast(), 1, 1) },
            -1
        );
        assert_eq!(crate::errno::get(), libc::EINVAL);
        crate::errno::set(0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn an_absolute_deadline_is_not_treated_as_a_relative_timeout() {
        let word = AtomicU32::new(3);
        let past = monotonic_nanoseconds().saturating_sub(1);
        let started = std::time::Instant::now();
        // SAFETY: a live, aligned word. A past absolute deadline must time out now,
        // not after `past` nanoseconds (normally days since boot).
        let rc = unsafe {
            wait2(
                op::COMPARE_AND_WAIT | flag::DEADLINE,
                word.as_ptr().cast(),
                3,
                past,
                0,
            )
        };
        assert_eq!(rc, -1);
        assert_eq!(crate::errno::get(), libc::ETIMEDOUT);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        crate::errno::set(0);
    }
}
