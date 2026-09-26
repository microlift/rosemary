//! `os_unfair_lock`: libplatform's lock, on the host's futex.
//!
//! Chromium reaches this before `main`: PartitionAlloc's root lock is an
//! `os_unfair_lock` on macOS, and `AllocatorShimOverrideAppleDefaultZone` - a
//! static constructor in Chromium Framework - initializes the default partition
//! root, which takes that lock. Refused, the import was a jump through a zero
//! slot inside `dyld::run_initializers` (Framework `+0x5ddf2f1`, a call to
//! `_os_unfair_lock_trylock`).
//!
//! The lock is one 32-bit word the guest owns, statically initialized to zero
//! (`OS_UNFAIR_LOCK_INIT`), so the whole state lives in that word and nothing is
//! allocated per lock. The encoding is libplatform's: a held lock stores its
//! owner, and the owner's low bit is `OS_ULOCK_NOWAITERS_BIT` - set while nobody
//! sleeps on the lock, cleared once somebody does, so an uncontended unlock is a
//! single swap with no system call. Darwin's owner is the thread's Mach port
//! name, which always has that bit set; here it is [`crate::pthread::id`]
//! shifted up one, which frees the bit the same way. Nothing outside this file
//! reads the owner, and a guest is not entitled to: the field is
//! `_os_unfair_lock_opaque`.
//!
//! The misuses Darwin traps are trapped here too, with Darwin's words, because
//! each of them would otherwise be a silent deadlock or a lock that stops
//! excluding anyone: locking a lock this thread holds, unlocking one it does
//! not, and an assertion about ownership that does not hold.

// Compiled and tested on macOS, bound to nothing there: libplatform answers these.
#![cfg_attr(target_os = "macos", allow(dead_code))]

use std::sync::atomic::{AtomicU32, Ordering};

use crate::wait_word::{self, Scope, WakeCount};

/// Set in a held lock's word while no thread is waiting for it.
const NOWAITERS: u32 = 0x1;

/// `OS_UNFAIR_LOCK_DATA_SYNCHRONIZATION` and `OS_UNFAIR_LOCK_ADAPTIVE_SPIN`,
/// the two options `os_unfair_lock_lock_with_options` accepts. Both are
/// scheduler hints the host has no equivalent for, so both are accepted and
/// neither changes anything; any other bit is the misuse Darwin aborts on.
const OPTIONS: u32 = 0x0001_0000 | 0x0004_0000;

/// The value this thread stores in a lock it holds, with no waiters.
fn me() -> u32 {
    // A thread id fits in 31 bits on every host this builds for (Linux caps
    // `pid_max` at 2^22), so the shift loses nothing and the result is never
    // zero - which is the one value that means "unlocked".
    (crate::pthread::id() << 1) | NOWAITERS
}

/// The owner a lock word names, whatever its waiters bit says.
fn owner(word: u32) -> u32 {
    word | NOWAITERS
}

/// View the guest's lock as the word it is.
///
/// # Safety
///
/// `lock` is a live, 4-byte aligned `os_unfair_lock`, and every access to it
/// from now on is atomic - which the ABI guarantees, since the guest only ever
/// reaches it through these functions.
unsafe fn word<'a>(lock: *mut u32) -> &'a AtomicU32 {
    // SAFETY: the caller's contract, and `AtomicU32` has `u32`'s layout.
    unsafe { AtomicU32::from_ptr(lock) }
}

/// libplatform's `__LIBPLATFORM_CLIENT_CRASH__`: the message, then `abort`.
///
/// Written with `write(2)` rather than through Rust's `stderr`, which may be
/// locked by the very thread that is about to deadlock, and flushed by nothing
/// once `abort` runs.
fn crash(message: &str, value: u32) -> ! {
    let line = format!("BUG IN CLIENT OF LIBPLATFORM: {message} (0x{value:x})\n");
    // SAFETY: a live buffer of the given length; a short or failed write loses
    // the message, not the abort.
    unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
    // SAFETY: always sound; it does not return.
    unsafe { libc::abort() }
}

fn lock(word: &AtomicU32) {
    let me = me();
    if word
        .compare_exchange(0, me, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
    {
        return;
    }
    // Contended. A thread that has slept once takes the lock with the waiters
    // bit already cleared: it cannot know whether it was the last sleeper, and
    // an extra wake on unlock is cheap where a lost one is a hang.
    loop {
        let current = word.load(Ordering::Relaxed);
        if current == 0 {
            if word
                .compare_exchange(0, me & !NOWAITERS, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
            continue;
        }
        if owner(current) == me {
            crash("Trying to recursively lock an os_unfair_lock", current);
        }
        let waiting = current & !NOWAITERS;
        if current != waiting
            && word
                .compare_exchange(current, waiting, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            continue;
        }
        // SAFETY: the guest's lock word, alive for as long as it is locked -
        // which it is, by the thread this one is waiting for. Any result means
        // "look again", which the loop does.
        let _ = unsafe { wait_word::wait_u32(word, waiting, None, Scope::Private) };
    }
}

fn trylock(word: &AtomicU32) -> bool {
    word.compare_exchange(0, me(), Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

fn unlock(word: &AtomicU32) {
    let me = me();
    let current = word.load(Ordering::Relaxed);
    if owner(current) != me {
        // Covers an unlocked lock too: its owner reads as `NOWAITERS`, which is
        // nobody's `me`.
        crash(
            "Unlock of an os_unfair_lock not owned by current thread",
            current,
        );
    }
    let previous = word.swap(0, Ordering::Release);
    if previous & NOWAITERS == 0 {
        // SAFETY: the guest's lock word, which it keeps alive past its unlock.
        let _ = unsafe { wait_word::wake_u32(word, WakeCount::One, Scope::Private) };
    }
}

/// `void os_unfair_lock_lock(os_unfair_lock_t lock)`
///
/// # Safety
///
/// `lock` is a live, aligned `os_unfair_lock`.
pub unsafe extern "C" fn os_unfair_lock_lock(lock: *mut u32) {
    // SAFETY: the caller's contract.
    self::lock(unsafe { word(lock) })
}

/// `void os_unfair_lock_lock_with_options(os_unfair_lock_t, os_unfair_lock_options_t)`
///
/// # Safety
///
/// As [`os_unfair_lock_lock`].
pub unsafe extern "C" fn os_unfair_lock_lock_with_options(lock: *mut u32, options: u32) {
    if options & !OPTIONS != 0 {
        crash("Invalid options", options);
    }
    // SAFETY: the caller's contract.
    self::lock(unsafe { word(lock) })
}

/// `bool os_unfair_lock_trylock(os_unfair_lock_t lock)`
///
/// False when the lock is held by anyone, this thread included - libplatform
/// does not diagnose a recursive trylock, it just fails it.
///
/// # Safety
///
/// As [`os_unfair_lock_lock`].
pub unsafe extern "C" fn os_unfair_lock_trylock(lock: *mut u32) -> bool {
    // SAFETY: the caller's contract.
    trylock(unsafe { word(lock) })
}

/// `void os_unfair_lock_unlock(os_unfair_lock_t lock)`
///
/// # Safety
///
/// As [`os_unfair_lock_lock`].
pub unsafe extern "C" fn os_unfair_lock_unlock(lock: *mut u32) {
    // SAFETY: the caller's contract.
    unlock(unsafe { word(lock) })
}

/// `void os_unfair_lock_assert_owner(const os_unfair_lock *lock)`
///
/// # Safety
///
/// As [`os_unfair_lock_lock`].
pub unsafe extern "C" fn os_unfair_lock_assert_owner(lock: *mut u32) {
    // SAFETY: the caller's contract.
    let current = unsafe { word(lock) }.load(Ordering::Relaxed);
    if owner(current) != me() {
        crash(
            "Assertion failed: Lock unexpectedly not owned by current thread",
            current,
        );
    }
}

/// `void os_unfair_lock_assert_not_owner(const os_unfair_lock *lock)`
///
/// # Safety
///
/// As [`os_unfair_lock_lock`].
pub unsafe extern "C" fn os_unfair_lock_assert_not_owner(lock: *mut u32) {
    // SAFETY: the caller's contract.
    let current = unsafe { word(lock) }.load(Ordering::Relaxed);
    if owner(current) == me() {
        crash(
            "Assertion failed: Lock unexpectedly owned by current thread",
            current,
        );
    }
}

/// Empty on macOS, where libplatform answers these.
#[cfg(target_os = "macos")]
exports::exports! {}

#[cfg(not(target_os = "macos"))]
exports::exports! {
    functions {
        "os_unfair_lock_lock" => os_unfair_lock_lock,
        "os_unfair_lock_lock_with_options" => os_unfair_lock_lock_with_options,
        "os_unfair_lock_trylock" => os_unfair_lock_trylock,
        "os_unfair_lock_unlock" => os_unfair_lock_unlock,
        "os_unfair_lock_assert_owner" => os_unfair_lock_assert_owner,
        "os_unfair_lock_assert_not_owner" => os_unfair_lock_assert_not_owner,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::sync::Arc;

    /// A lock in the guest's shape: one word, zero-initialized.
    fn fresh() -> Arc<AtomicU32> {
        Arc::new(AtomicU32::new(0))
    }

    #[test]
    fn a_lock_excludes_under_contention() {
        // The property a lock exists for. A non-atomic read-modify-write under
        // the lock loses increments the moment exclusion fails, so the count is
        // the proof - and the sleeping path is exercised, because eight threads
        // on one word contend.
        let lock = fresh();
        let counter = Arc::new(AtomicU32::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (lock, counter) = (Arc::clone(&lock), Arc::clone(&counter));
                std::thread::spawn(move || {
                    for _ in 0..20_000 {
                        // SAFETY: a live, aligned word.
                        unsafe { os_unfair_lock_lock(lock.as_ptr()) };
                        let seen = counter.load(Ordering::Relaxed);
                        std::hint::spin_loop();
                        counter.store(seen + 1, Ordering::Relaxed);
                        // SAFETY: as above, locked by this thread.
                        unsafe { os_unfair_lock_unlock(lock.as_ptr()) };
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(counter.load(Ordering::Relaxed), 8 * 20_000);
        assert_eq!(lock.load(Ordering::Relaxed), 0, "and it ends unlocked");
    }

    #[test]
    fn trylock_fails_while_held_and_succeeds_after() {
        let lock = fresh();
        // SAFETY: a live, aligned word throughout.
        unsafe {
            assert!(os_unfair_lock_trylock(lock.as_ptr()));
            // Held by this thread: trylock fails rather than recursing.
            assert!(!os_unfair_lock_trylock(lock.as_ptr()));
            let theirs = Arc::clone(&lock);
            let other = std::thread::spawn(move || os_unfair_lock_trylock(theirs.as_ptr()));
            assert!(!other.join().unwrap(), "and fails for another thread");
            os_unfair_lock_assert_owner(lock.as_ptr());
            os_unfair_lock_unlock(lock.as_ptr());
            os_unfair_lock_assert_not_owner(lock.as_ptr());
            assert!(os_unfair_lock_trylock(lock.as_ptr()));
            os_unfair_lock_unlock(lock.as_ptr());
        }
    }

    #[test]
    fn a_sleeper_is_woken_by_the_unlock() {
        // The waiters bit's whole job: an unlock after somebody slept must wake
        // them. A test that hangs here is a lost wake-up.
        let lock = fresh();
        // SAFETY: a live, aligned word.
        unsafe { os_unfair_lock_lock(lock.as_ptr()) };
        let theirs = Arc::clone(&lock);
        let sleeper = std::thread::spawn(move || {
            // SAFETY: a live, aligned word.
            unsafe {
                os_unfair_lock_lock(theirs.as_ptr());
                os_unfair_lock_unlock(theirs.as_ptr());
            }
        });
        // Until the sleeper has cleared the bit, it has not gone to sleep.
        while lock.load(Ordering::Relaxed) & NOWAITERS != 0 {
            std::thread::yield_now();
        }
        // SAFETY: locked by this thread.
        unsafe { os_unfair_lock_unlock(lock.as_ptr()) };
        sleeper.join().unwrap();
        assert_eq!(lock.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn options_the_header_defines_are_accepted() {
        let lock = fresh();
        // SAFETY: a live, aligned word.
        unsafe {
            os_unfair_lock_lock_with_options(lock.as_ptr(), 0x0001_0000);
            os_unfair_lock_unlock(lock.as_ptr());
            os_unfair_lock_lock_with_options(lock.as_ptr(), 0x0004_0000 | 0x0001_0000);
            os_unfair_lock_unlock(lock.as_ptr());
        }
    }

    const CHILD: &str = "DARWIN_UNFAIR_LOCK_CHILD";

    /// Re-run the calling test in a child with [`CHILD`] set, and return what
    /// it wrote and how it died. `None` where this harness cannot re-exec.
    fn in_child() -> Option<(String, std::process::ExitStatus)> {
        let exe = std::env::current_exe().ok()?;
        let name = std::thread::current().name()?.to_string();
        let out = std::process::Command::new(exe)
            .args(["--exact", &name, "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .ok()?;
        Some((
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status,
        ))
    }

    fn assert_died_saying(what: &str) {
        let Some((stderr, status)) = in_child() else {
            eprintln!("skipping: cannot re-exec this test binary");
            return;
        };
        assert!(stderr.contains(what), "expected {what:?}; got:\n{stderr}");
        assert_eq!(status.signal(), Some(libc::SIGABRT), "and to abort");
    }

    #[test]
    fn unlocking_a_lock_this_thread_does_not_hold_aborts() {
        if std::env::var_os(CHILD).is_some() {
            let lock = fresh();
            let theirs = Arc::clone(&lock);
            // SAFETY: a live, aligned word, locked by another thread.
            std::thread::spawn(move || unsafe { os_unfair_lock_lock(theirs.as_ptr()) })
                .join()
                .unwrap();
            // SAFETY: as above.
            unsafe { os_unfair_lock_unlock(lock.as_ptr()) };
            return;
        }
        assert_died_saying("Unlock of an os_unfair_lock not owned by current thread");
    }

    #[test]
    fn relocking_a_held_lock_aborts_rather_than_deadlocking() {
        if std::env::var_os(CHILD).is_some() {
            let lock = fresh();
            // SAFETY: a live, aligned word.
            unsafe {
                os_unfair_lock_lock(lock.as_ptr());
                os_unfair_lock_lock(lock.as_ptr());
            }
            return;
        }
        assert_died_saying("Trying to recursively lock an os_unfair_lock");
    }

    #[test]
    fn an_ownership_assertion_that_does_not_hold_aborts() {
        if std::env::var_os(CHILD).is_some() {
            let lock = fresh();
            // SAFETY: a live, aligned word, unlocked.
            unsafe { os_unfair_lock_assert_owner(lock.as_ptr()) };
            return;
        }
        assert_died_saying("Lock unexpectedly not owned by current thread");
    }

    #[test]
    fn an_option_the_header_does_not_define_aborts() {
        if std::env::var_os(CHILD).is_some() {
            let lock = fresh();
            // SAFETY: a live, aligned word.
            unsafe { os_unfair_lock_lock_with_options(lock.as_ptr(), 0x8) };
            return;
        }
        assert_died_saying("Invalid options");
    }
}
