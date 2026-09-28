use std::sync::atomic::AtomicU32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    Private,
    Shared,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WakeCount {
    One,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitError {
    Interrupted,
    TimedOut,
    Changed,
    Fault(i32),
}

impl WaitError {
    pub(crate) fn raw_os_error(self) -> i32 {
        match self {
            WaitError::Interrupted => libc::EINTR,
            WaitError::TimedOut => libc::ETIMEDOUT,
            WaitError::Changed => libc::EAGAIN,
            WaitError::Fault(error) => error,
        }
    }
}

fn classify(error: i32) -> WaitError {
    match error {
        libc::EINTR => WaitError::Interrupted,
        libc::ETIMEDOUT => WaitError::TimedOut,
        libc::EAGAIN => WaitError::Changed,
        error => WaitError::Fault(error),
    }
}

/// Sleep while `word` still equals `expected`.
///
/// # Safety
///
/// `word` must remain at a stable, naturally aligned address for the call.
#[cfg(target_os = "linux")]
pub(crate) unsafe fn wait_u32(
    word: &AtomicU32,
    expected: u32,
    timeout_ns: Option<u64>,
    scope: Scope,
) -> Result<(), WaitError> {
    use rustix::thread::futex::{self, Flags, Timespec};

    let timeout = timeout_ns.map(|nanoseconds| Timespec {
        tv_sec: (nanoseconds / 1_000_000_000).min(i64::MAX as u64) as i64,
        tv_nsec: (nanoseconds % 1_000_000_000) as _,
    });
    let flags = match scope {
        Scope::Private => Flags::PRIVATE,
        Scope::Shared => Flags::empty(),
    };
    futex::wait(word, flags, expected, timeout.as_ref())
        .map_err(|error| classify(error.raw_os_error()))
}

/// Wake up to `count` threads sleeping on `word`.
///
/// # Safety
///
/// `word` must remain at a stable, naturally aligned address for the call.
#[cfg(target_os = "linux")]
pub(crate) unsafe fn wake_u32(
    word: &AtomicU32,
    count: WakeCount,
    scope: Scope,
) -> Result<usize, i32> {
    use rustix::thread::futex::{self, Flags};

    let flags = match scope {
        Scope::Private => Flags::PRIVATE,
        Scope::Shared => Flags::empty(),
    };
    let count = match count {
        WakeCount::One => 1,
        WakeCount::All => i32::MAX as u32,
    };
    futex::wake(word, flags, count)
        .map(|woken| woken as usize)
        .map_err(|error| error.raw_os_error())
}

/// Darwin's own futex, which is a kernel interface with no libc wrapper.
///
/// `ULF_NO_ERRNO` asks for `-errno` as the return value, which is what the callers
/// inside libplatform use and what keeps this off `errno` entirely.
#[cfg(target_os = "macos")]
mod ulock {
    use std::ffi::c_void;

    pub const COMPARE_AND_WAIT: u32 = 1;
    pub const COMPARE_AND_WAIT_SHARED: u32 = 3;
    pub const WAKE_ALL: u32 = 0x0000_0100;
    pub const NO_ERRNO: u32 = 0x0100_0000;

    unsafe extern "C" {
        /// A zero timeout means "forever"; the unit is microseconds.
        pub fn __ulock_wait(operation: u32, addr: *mut c_void, value: u64, timeout_us: u32) -> i32;
        pub fn __ulock_wake(operation: u32, addr: *mut c_void, wake_value: u64) -> i32;
    }
}

/// # Safety
///
/// As the Linux form above.
#[cfg(target_os = "macos")]
pub(crate) unsafe fn wait_u32(
    word: &AtomicU32,
    expected: u32,
    timeout_ns: Option<u64>,
    scope: Scope,
) -> Result<(), WaitError> {
    if word.load(std::sync::atomic::Ordering::Relaxed) != expected {
        return Err(WaitError::Changed);
    }
    // Zero is "forever", so a sub-microsecond timeout has to round *up* to one
    // microsecond rather than down to no timeout at all.
    let timeout_us = match timeout_ns {
        None => 0,
        Some(nanoseconds) => nanoseconds.div_ceil(1_000).clamp(1, u32::MAX as u64) as u32,
    };
    let operation = ulock_operation(scope) | ulock::NO_ERRNO;
    // SAFETY: the caller's guarantee that `word` is live and aligned for the call.
    let rc = unsafe {
        ulock::__ulock_wait(operation, word.as_ptr().cast(), expected as u64, timeout_us)
    };
    match rc {
        0.. => Ok(()),
        _ => Err(classify(-rc)),
    }
}

/// # Safety
///
/// As the Linux form above.
#[cfg(target_os = "macos")]
pub(crate) unsafe fn wake_u32(
    word: &AtomicU32,
    count: WakeCount,
    scope: Scope,
) -> Result<usize, i32> {
    let mut operation = ulock_operation(scope) | ulock::NO_ERRNO;
    if count == WakeCount::All {
        operation |= ulock::WAKE_ALL;
    }
    // SAFETY: the caller's guarantee that `word` is live and aligned for the call.
    let rc = unsafe { ulock::__ulock_wake(operation, word.as_ptr().cast(), 0) };
    match rc {
        // The kernel does not say how many it woke, only that it woke somebody.
        0.. => Ok(1),
        _ if -rc == libc::ENOENT => Ok(0),
        _ => Err(-rc),
    }
}

#[cfg(target_os = "macos")]
fn ulock_operation(scope: Scope) -> u32 {
    match scope {
        Scope::Private => ulock::COMPARE_AND_WAIT,
        Scope::Shared => ulock::COMPARE_AND_WAIT_SHARED,
    }
}

/// FreeBSD's futex, under the name its own libthr uses.
///
/// `UMTX_OP_WAIT_UINT*` compares a 32-bit word and sleeps, which is the whole
/// operation. The timeout is passed as a `struct _umtx_time` in `uaddr2`, with
/// its *size* in `uaddr` - the kernel tells that structure from the bare
/// `timespec` older callers pass by the length.
///
/// # Safety
///
/// As the Linux form above.
#[cfg(target_os = "freebsd")]
pub(crate) unsafe fn wait_u32(
    word: &AtomicU32,
    expected: u32,
    timeout_ns: Option<u64>,
    scope: Scope,
) -> Result<(), WaitError> {
    use std::sync::atomic::Ordering;

    // The kernel answers zero for a word that no longer holds `expected`, where
    // every other host here reports it distinctly - so it is checked first, and
    // the contract is the same on all of them. The kernel re-checks atomically,
    // so a word that changes after this point simply does not sleep.
    if word.load(Ordering::Acquire) != expected {
        return Err(WaitError::Changed);
    }
    let operation = match scope {
        Scope::Private => libc::UMTX_OP_WAIT_UINT_PRIVATE,
        Scope::Shared => libc::UMTX_OP_WAIT_UINT,
    };
    // Zeroed rather than optional so the structure outlives the call below.
    let mut timeout: libc::_umtx_time = unsafe { core::mem::zeroed() };
    if let Some(nanoseconds) = timeout_ns {
        timeout._timeout.tv_sec = (nanoseconds / 1_000_000_000).min(i64::MAX as u64) as _;
        timeout._timeout.tv_nsec = (nanoseconds % 1_000_000_000) as _;
        // `_flags` of zero is a relative timeout, which is what the callers
        // hand down; the clock has to be named even so.
        timeout._clockid = libc::CLOCK_MONOTONIC as u32;
    }
    let (size, at) = match timeout_ns {
        None => (core::ptr::null_mut(), core::ptr::null_mut()),
        Some(_) => (
            size_of::<libc::_umtx_time>() as *mut core::ffi::c_void,
            (&raw mut timeout).cast(),
        ),
    };
    // SAFETY: the caller's guarantee that `word` is live and aligned, and a
    // timeout this frame owns for the duration of the call.
    let rc = unsafe {
        libc::_umtx_op(
            word.as_ptr().cast(),
            operation,
            expected as libc::c_ulong,
            size,
            at,
        )
    };
    // A return of zero is permission to look again, which is what every caller
    // does - it covers both a real wake and a word that changed in the window
    // above.
    match rc {
        0 => Ok(()),
        _ => Err(classify(crate::errno::get())),
    }
}

/// # Safety
///
/// As the Linux form above.
#[cfg(target_os = "freebsd")]
pub(crate) unsafe fn wake_u32(
    word: &AtomicU32,
    count: WakeCount,
    scope: Scope,
) -> Result<usize, i32> {
    let operation = match scope {
        Scope::Private => libc::UMTX_OP_WAKE_PRIVATE,
        Scope::Shared => libc::UMTX_OP_WAKE,
    };
    let count = match count {
        WakeCount::One => 1,
        WakeCount::All => libc::c_int::MAX as libc::c_ulong,
    };
    // SAFETY: the caller's guarantee that `word` is live and aligned.
    let rc = unsafe {
        libc::_umtx_op(
            word.as_ptr().cast(),
            operation,
            count,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        )
    };
    match rc {
        // The kernel does not say how many it woke. Report a possible waiter,
        // as the fallback below does and for the same reason.
        0 => Ok(1),
        _ => Err(crate::errno::get()),
    }
}

/// Portability fallback for a host with neither futexes nor ulocks.
///
/// Correct and slow: a waiter polls with a bounded backoff rather than sleeping, so a
/// wake needs to do nothing at all. One deadline is computed up front rather than a
/// relative timeout being restarted after every spurious return.
///
/// # Safety
///
/// As the Linux form above.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
pub(crate) unsafe fn wait_u32(
    word: &AtomicU32,
    expected: u32,
    timeout_ns: Option<u64>,
    _scope: Scope,
) -> Result<(), WaitError> {
    use std::sync::atomic::Ordering;

    /// Long enough that a blocked thread is not burning a core, short enough that a
    /// wake is not visibly delayed.
    const POLL: std::time::Duration = std::time::Duration::from_micros(200);

    let deadline = timeout_ns.map(|nanoseconds| {
        std::time::Instant::now() + std::time::Duration::from_nanos(nanoseconds)
    });
    loop {
        if word.load(Ordering::Acquire) != expected {
            return Err(WaitError::Changed);
        }
        match deadline {
            Some(deadline) => {
                let now = std::time::Instant::now();
                if now >= deadline {
                    return Err(WaitError::TimedOut);
                }
                std::thread::sleep(POLL.min(deadline - now));
            }
            None => std::thread::sleep(POLL),
        }
        // A return is permission to look again, which is what every caller does.
        return Ok(());
    }
}

/// # Safety
///
/// As the Linux form above.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
pub(crate) unsafe fn wake_u32(
    _word: &AtomicU32,
    _count: WakeCount,
    _scope: Scope,
) -> Result<usize, i32> {
    // Polling waiters need no notification. Report a possible waiter, because this
    // cannot tell whether there was one and a caller that skips work on "nobody was
    // waiting" must not be told that wrongly.
    Ok(1)
}

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "macos", target_os = "freebsd")
))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn compare_mismatch_is_distinct_from_a_kernel_failure() {
        let word = AtomicU32::new(2);
        assert_eq!(
            unsafe { wait_u32(&word, 1, None, Scope::Private) },
            Err(WaitError::Changed)
        );
        assert_eq!(
            unsafe { wait_u32(&word, 1, None, Scope::Shared) },
            Err(WaitError::Changed)
        );
    }

    #[test]
    fn relative_timeout_expires_once() {
        let word = AtomicU32::new(1);
        let start = std::time::Instant::now();
        assert_eq!(
            unsafe { wait_u32(&word, 1, Some(20_000_000), Scope::Private) },
            Err(WaitError::TimedOut)
        );
        assert!(start.elapsed() >= std::time::Duration::from_millis(10));
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }

    /// A woken thread must actually return, and a thread nobody woke must not.
    #[test]
    fn a_waiter_returns_when_its_word_changes_and_somebody_wakes_it() {
        const WAITERS: u32 = 4;
        let word = Arc::new(AtomicU32::new(7));
        let entered = Arc::new(AtomicU32::new(0));
        let returned = Arc::new(AtomicU32::new(0));
        let threads: Vec<_> = (0..WAITERS)
            .map(|_| {
                let word = Arc::clone(&word);
                let entered = Arc::clone(&entered);
                let returned = Arc::clone(&returned);
                std::thread::spawn(move || {
                    entered.fetch_add(1, Ordering::Release);
                    while word.load(Ordering::Acquire) == 7 {
                        let _ = unsafe { wait_u32(&word, 7, None, Scope::Private) };
                    }
                    returned.fetch_add(1, Ordering::Release);
                })
            })
            .collect();
        while entered.load(Ordering::Acquire) != WAITERS {
            std::thread::yield_now();
        }

        word.store(8, Ordering::Release);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while returned.load(Ordering::Acquire) != WAITERS {
            let _ = unsafe { wake_u32(&word, WakeCount::All, Scope::Private) };
            assert!(
                std::time::Instant::now() < deadline,
                "every waiter returned"
            );
            std::thread::yield_now();
        }
        for thread in threads {
            thread.join().unwrap();
        }
    }

    /// Waking nobody says so, which is what Darwin's `ENOENT` means to its callers.
    #[test]
    fn waking_nobody_is_reported_as_nobody() {
        let word = AtomicU32::new(0);
        assert_eq!(
            unsafe { wake_u32(&word, WakeCount::All, Scope::Private) },
            Ok(0)
        );
    }
}
