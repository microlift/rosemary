use std::sync::atomic::Ordering;

use super::consts::*;
use super::object::{self, Kind, Record, State};
use crate::wait_word::{self, Scope, WaitError, WakeCount};

const SEQUENCE: u64 = 0;
const SLEEPERS: u64 = 4;
const BODY: usize = 8;

const _: () = assert!(16 + BODY <= object::size::COND);

const NANOSECONDS: i64 = 1_000_000_000;

fn lay_out(record: &Record) {
    record.u32(SEQUENCE).store(0, Ordering::Relaxed);
    record.u32(SLEEPERS).store(0, Ordering::Relaxed);
}

/// # Safety
///
/// `at` is a guest `pthread_cond_t *`.
unsafe fn live(at: u64) -> Option<Record> {
    if at == 0 {
        return None;
    }
    let record = unsafe { Record::at(at, Kind::Cond) };
    record
        .live_or_init(|record, _| {
            lay_out(record);
            true
        })
        .then_some(record)
}

/// When a timed wait gives up.
///
/// `pthread_cond_timedwait` takes an absolute `CLOCK_REALTIME` deadline, which has to
/// be re-measured against the wall clock on every pass so that a clock the guest moves
/// is honoured. `pthread_cond_timedwait_relative_np` takes an interval, which is
/// pinned to the monotonic clock once and never re-measured, because an interval is
/// not supposed to stretch when somebody sets the date.
#[derive(Clone, Copy)]
enum Until {
    Realtime(libc::timespec),
    Monotonic(std::time::Instant),
}

impl Until {
    /// Nanoseconds left, or `None` if the deadline has passed.
    fn remaining(self) -> Option<u64> {
        match self {
            Until::Realtime(deadline) => {
                let mut now = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                };
                // SAFETY: a live local; failure leaves the zero initialiser, which
                // reads as a deadline far in the future rather than as an expiry.
                unsafe { crate::syscall::clock_gettime(libc::CLOCK_REALTIME, &mut now) };
                let seconds = deadline.tv_sec.checked_sub(now.tv_sec)?;
                let nanoseconds = (seconds as i128) * (NANOSECONDS as i128)
                    + (deadline.tv_nsec as i128 - now.tv_nsec as i128);
                (nanoseconds > 0).then(|| nanoseconds.min(u64::MAX as i128) as u64)
            }
            Until::Monotonic(deadline) => deadline
                .checked_duration_since(std::time::Instant::now())
                .filter(|left| !left.is_zero())
                .map(|left| left.as_nanos().min(u64::MAX as u128) as u64),
        }
    }
}

/// Release `mutex`, wait to be signalled, and take `mutex` again.
///
/// The mutex is reacquired before returning on every path that released it, including
/// timeout and spurious wakeup, which is what POSIX requires of all of them.
///
/// # Safety
///
/// `at` is a guest `pthread_cond_t *` and `mutex` a guest `pthread_mutex_t *` the
/// calling thread holds.
unsafe fn wait(at: u64, mutex: u64, until: Option<Until>) -> i32 {
    let Some(record) = (unsafe { live(at) }) else {
        return err::EINVAL;
    };
    let sequence = record.u32(SEQUENCE);

    // Read under the guest mutex, so a signal that beats this thread into the kernel
    // is one this thread's compare will notice.
    let expected = sequence.load(Ordering::Relaxed);
    // Announced before the mutex is released, and ordered against the signaller's own
    // read of it: one of the two sees the other, so a wake is never skipped for a
    // waiter that was on its way in.
    record.u32(SLEEPERS).fetch_add(1, Ordering::SeqCst);

    // SAFETY: the caller states it holds this mutex.
    let unlock_rc = unsafe { super::mutex::mutex_unlock(mutex) };
    if unlock_rc != 0 {
        record.u32(SLEEPERS).fetch_sub(1, Ordering::Relaxed);
        return unlock_rc;
    }

    let mut rc = 0;
    while sequence.load(Ordering::SeqCst) == expected {
        let timeout = match until {
            None => None,
            Some(until) => match until.remaining() {
                Some(left) => Some(left),
                None => {
                    rc = err::ETIMEDOUT;
                    break;
                }
            },
        };
        // A condition wait *is* a cancellation point on Darwin, but this crate does
        // not implement cancellation; `EINTR` and spurious returns both mean "look
        // again", which is what the loop does.
        // SAFETY: the sequence word of a live condition variable.
        if let Err(WaitError::TimedOut) =
            unsafe { wait_word::wait_u32(sequence, expected, timeout, Scope::Private) }
        {
            // Re-check before believing it: a signal and the timeout can land
            // together, and a signal consumed but reported as a timeout is lost.
            if sequence.load(Ordering::SeqCst) == expected {
                rc = err::ETIMEDOUT;
                break;
            }
        }
    }
    record.u32(SLEEPERS).fetch_sub(1, Ordering::Relaxed);

    // SAFETY: the caller's guarantee about `mutex`, which this released above.
    let lock_rc = unsafe { super::mutex::mutex_lock(mutex) };
    if lock_rc != 0 {
        return lock_rc;
    }
    rc
}

/// # Safety
///
/// `at` is a guest `pthread_cond_t *` and `attr` is null or a guest
/// `pthread_condattr_t *`.
pub unsafe extern "C" fn cond_init(at: u64, attr: u64) -> i32 {
    if at == 0 {
        return err::EINVAL;
    }
    let Some(shared) = (unsafe { super::sync::simple_attr_pshared(attr, Kind::CondAttr) }) else {
        return err::EINVAL;
    };
    if shared == Some(PTHREAD_PROCESS_SHARED) {
        super::sync::say_unsupported("a process-shared condition variable");
        return err::ENOTSUP;
    }
    let record = unsafe { Record::at(at, Kind::Cond) };
    record.seize();
    record.publish(lay_out);
    0
}

/// # Safety
///
/// As [`cond_init`].
pub unsafe extern "C" fn cond_destroy(at: u64) -> i32 {
    if at == 0 {
        return err::EINVAL;
    }
    let record = unsafe { Record::at(at, Kind::Cond) };
    let state = record.seize();
    match state {
        State::Invalid => err::EINVAL,
        State::Fresh(_) => {
            record.bury();
            0
        }
        State::Live if record.u32(SLEEPERS).load(Ordering::Acquire) != 0 => {
            record.restore(state);
            err::EBUSY
        }
        State::Live => {
            record.bury();
            0
        }
    }
}

/// Bump the sequence, then wake, if there is anybody to wake.
///
/// Order matters: the bump is what a waiter racing into the kernel compares against,
/// and the sleeper count is read *after* it so that this and the waiter cannot both
/// conclude the other is not there.
fn signal_record(record: &Record, count: WakeCount) -> i32 {
    let sequence = record.u32(SEQUENCE);
    sequence.fetch_add(1, Ordering::SeqCst);
    if record.u32(SLEEPERS).load(Ordering::SeqCst) != 0 {
        // SAFETY: the sequence word of a live condition variable.
        let _ = unsafe { wait_word::wake_u32(sequence, count, Scope::Private) };
    }
    0
}

/// # Safety
///
/// As [`cond_init`].
pub unsafe extern "C" fn cond_signal(at: u64) -> i32 {
    match unsafe { live(at) } {
        Some(record) => signal_record(&record, WakeCount::One),
        None => err::EINVAL,
    }
}

/// # Safety
///
/// As [`cond_init`].
pub unsafe extern "C" fn cond_broadcast(at: u64) -> i32 {
    match unsafe { live(at) } {
        Some(record) => signal_record(&record, WakeCount::All),
        None => err::EINVAL,
    }
}

/// # Safety
///
/// As [`cond_init`], with `mutex` a guest `pthread_mutex_t *` the caller holds.
pub unsafe extern "C" fn cond_wait(at: u64, mutex: u64) -> i32 {
    unsafe { wait(at, mutex, None) }
}

/// `struct timespec` is a `time_t` and a `long` on both platforms, so the guest's
/// deadline is read as it stands rather than converted.
///
/// # Safety
///
/// As [`cond_wait`], with `abstime` a readable `struct timespec *`.
pub unsafe extern "C" fn cond_timedwait(at: u64, mutex: u64, abstime: u64) -> i32 {
    if abstime == 0 {
        return err::EINVAL;
    }
    // SAFETY: the caller states `abstime` is a readable `timespec`.
    let deadline = unsafe { *(abstime as usize as *const libc::timespec) };
    if !(0..NANOSECONDS).contains(&deadline.tv_nsec) {
        return err::EINVAL;
    }
    unsafe { wait(at, mutex, Some(Until::Realtime(deadline))) }
}

/// `pthread_cond_timedwait_relative_np`, which is Darwin's and takes an *interval*.
///
/// # Safety
///
/// As [`cond_timedwait`], with `reltime` a readable `struct timespec *`.
pub unsafe extern "C" fn cond_timedwait_relative(at: u64, mutex: u64, reltime: u64) -> i32 {
    if reltime == 0 {
        // A null interval means "wait forever", which is what Darwin does.
        return unsafe { cond_wait(at, mutex) };
    }
    // SAFETY: the caller states `reltime` is a readable `timespec`.
    let interval = unsafe { *(reltime as usize as *const libc::timespec) };
    if interval.tv_sec < 0 || !(0..NANOSECONDS).contains(&interval.tv_nsec) {
        return err::EINVAL;
    }
    let interval = std::time::Duration::new(interval.tv_sec as u64, interval.tv_nsec as u32);
    let Some(deadline) = std::time::Instant::now().checked_add(interval) else {
        // Further away than this host's clock can name, which is "forever".
        return unsafe { cond_wait(at, mutex) };
    };
    unsafe { wait(at, mutex, Some(Until::Monotonic(deadline))) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pthread::mutex::{mutex_destroy, mutex_init, mutex_lock, mutex_unlock};
    use crate::pthread::testing::Guest;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;

    type Cond = Guest<{ object::size::COND }>;
    type Mutex = Guest<{ object::size::MUTEX }>;

    /// The whole contract: a waiter that went to sleep before the predicate changed
    /// wakes up once it has, and holds the mutex again when it returns.
    #[test]
    fn a_broadcast_wakes_every_waiter() {
        const WAITERS: u32 = 4;
        let cond = Cond::zeroed();
        let mutex = Mutex::zeroed();
        let ready = Arc::new(AtomicU32::new(0));
        let woke = Arc::new(AtomicU32::new(0));
        assert_eq!(unsafe { cond_init(cond.at(), 0) }, 0);
        assert_eq!(unsafe { mutex_init(mutex.at(), 0) }, 0);

        let threads: Vec<_> = (0..WAITERS)
            .map(|_| {
                let (cond, mutex) = (Arc::clone(&cond), Arc::clone(&mutex));
                let (ready, woke) = (Arc::clone(&ready), Arc::clone(&woke));
                std::thread::spawn(move || unsafe {
                    assert_eq!(mutex_lock(mutex.at()), 0);
                    while ready.load(Ordering::Relaxed) == 0 {
                        assert_eq!(cond_wait(cond.at(), mutex.at()), 0);
                    }
                    woke.fetch_add(1, Ordering::Relaxed);
                    assert_eq!(mutex_unlock(mutex.at()), 0);
                })
            })
            .collect();

        // Give them time to get into the kernel, so this is a real wake rather than
        // a predicate that was already true.
        std::thread::sleep(std::time::Duration::from_millis(50));
        unsafe {
            assert_eq!(mutex_lock(mutex.at()), 0);
            ready.store(1, Ordering::Relaxed);
            assert_eq!(cond_broadcast(cond.at()), 0);
            assert_eq!(mutex_unlock(mutex.at()), 0);
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(woke.load(Ordering::Relaxed), WAITERS);
        unsafe {
            assert_eq!(cond_destroy(cond.at()), 0);
            assert_eq!(mutex_destroy(mutex.at()), 0);
        }
    }

    /// A signal that lands after the mutex is released but before the waiter sleeps
    /// is the lost-wakeup window the sequence counter exists to close. Asked many
    /// times with the signaller racing the waiter, because once would prove nothing.
    #[test]
    fn a_signal_in_the_release_and_sleep_window_is_not_lost() {
        for _ in 0..200 {
            let cond = Cond::zeroed();
            let mutex = Mutex::zeroed();
            let predicate = Arc::new(AtomicU32::new(0));
            assert_eq!(unsafe { cond_init(cond.at(), 0) }, 0);
            assert_eq!(unsafe { mutex_init(mutex.at(), 0) }, 0);

            let waiter = {
                let (cond, mutex) = (Arc::clone(&cond), Arc::clone(&mutex));
                let predicate = Arc::clone(&predicate);
                std::thread::spawn(move || unsafe {
                    assert_eq!(mutex_lock(mutex.at()), 0);
                    while predicate.load(Ordering::Relaxed) == 0 {
                        assert_eq!(cond_wait(cond.at(), mutex.at()), 0);
                    }
                    assert_eq!(mutex_unlock(mutex.at()), 0);
                })
            };
            unsafe {
                assert_eq!(mutex_lock(mutex.at()), 0);
                predicate.store(1, Ordering::Relaxed);
                assert_eq!(mutex_unlock(mutex.at()), 0);
                // Signalled without the mutex held, which POSIX allows and which is
                // the arrangement that makes the window widest.
                assert_eq!(cond_signal(cond.at()), 0);
            }
            waiter.join().expect("the waiter is not stuck");
            unsafe {
                assert_eq!(cond_destroy(cond.at()), 0);
                assert_eq!(mutex_destroy(mutex.at()), 0);
            }
        }
    }

    #[test]
    fn an_absolute_deadline_that_has_passed_times_out_without_waiting() {
        let cond = Cond::zeroed();
        let mutex = Mutex::zeroed();
        unsafe {
            assert_eq!(cond_init(cond.at(), 0), 0);
            assert_eq!(mutex_init(mutex.at(), 0), 0);
            assert_eq!(mutex_lock(mutex.at()), 0);
            let past = libc::timespec {
                tv_sec: 1,
                tv_nsec: 0,
            };
            let start = std::time::Instant::now();
            assert_eq!(
                cond_timedwait(cond.at(), mutex.at(), &past as *const _ as u64),
                err::ETIMEDOUT
            );
            assert!(start.elapsed() < std::time::Duration::from_secs(1));
            // And the mutex is held again, which is what a timeout still promises.
            assert_eq!(mutex_unlock(mutex.at()), 0);
            assert_eq!(cond_destroy(cond.at()), 0);
            assert_eq!(mutex_destroy(mutex.at()), 0);
        }
    }

    #[test]
    fn a_deadline_in_the_future_waits_for_it() {
        let cond = Cond::zeroed();
        let mutex = Mutex::zeroed();
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe {
            assert_eq!(cond_init(cond.at(), 0), 0);
            assert_eq!(mutex_init(mutex.at(), 0), 0);
            assert_eq!(mutex_lock(mutex.at()), 0);
            crate::syscall::clock_gettime(libc::CLOCK_REALTIME, &mut now);
            let mut deadline = libc::timespec {
                tv_sec: now.tv_sec,
                tv_nsec: now.tv_nsec + 30_000_000,
            };
            if deadline.tv_nsec >= NANOSECONDS {
                deadline.tv_nsec -= NANOSECONDS;
                deadline.tv_sec += 1;
            }
            let start = std::time::Instant::now();
            assert_eq!(
                cond_timedwait(cond.at(), mutex.at(), &deadline as *const _ as u64),
                err::ETIMEDOUT
            );
            assert!(start.elapsed() >= std::time::Duration::from_millis(20));
            assert!(start.elapsed() < std::time::Duration::from_secs(5));
            assert_eq!(mutex_unlock(mutex.at()), 0);
            assert_eq!(cond_destroy(cond.at()), 0);
            assert_eq!(mutex_destroy(mutex.at()), 0);
        }
    }

    /// Destroying a condition variable somebody is waiting on is undefined in POSIX
    /// and `EBUSY` here, rather than a waiter left on a buried object.
    #[test]
    fn destroying_a_waited_on_condition_is_refused() {
        let cond = Cond::zeroed();
        let mutex = Mutex::zeroed();
        let predicate = Arc::new(AtomicU32::new(0));
        assert_eq!(unsafe { cond_init(cond.at(), 0) }, 0);
        assert_eq!(unsafe { mutex_init(mutex.at(), 0) }, 0);
        let waiter = {
            let (cond, mutex) = (Arc::clone(&cond), Arc::clone(&mutex));
            let predicate = Arc::clone(&predicate);
            std::thread::spawn(move || unsafe {
                assert_eq!(mutex_lock(mutex.at()), 0);
                while predicate.load(Ordering::Relaxed) == 0 {
                    assert_eq!(cond_wait(cond.at(), mutex.at()), 0);
                }
                assert_eq!(mutex_unlock(mutex.at()), 0);
            })
        };
        // SAFETY: a live condition variable.
        let record = unsafe { Record::at(cond.at(), Kind::Cond) };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while record.u32(SLEEPERS).load(Ordering::Acquire) == 0 {
            assert!(std::time::Instant::now() < deadline, "the waiter arrived");
            std::thread::yield_now();
        }
        assert_eq!(unsafe { cond_destroy(cond.at()) }, err::EBUSY);
        unsafe {
            assert_eq!(mutex_lock(mutex.at()), 0);
            predicate.store(1, Ordering::Relaxed);
            assert_eq!(cond_broadcast(cond.at()), 0);
            assert_eq!(mutex_unlock(mutex.at()), 0);
        }
        waiter.join().unwrap();
        assert_eq!(unsafe { cond_destroy(cond.at()) }, 0);
        assert_eq!(unsafe { mutex_destroy(mutex.at()) }, 0);
    }

    /// A relative timeout is Darwin's own call. The assertion is that it *times out*
    /// rather than returning at once or hanging, and with Darwin's `ETIMEDOUT` - 60,
    /// not this host's 110, which is the number `libc++` compares against before
    /// deciding a timed wait failed.
    #[test]
    fn a_relative_timed_wait_times_out() {
        let cond = Cond::zeroed();
        let mutex = Mutex::zeroed();
        let interval = libc::timespec {
            tv_sec: 0,
            tv_nsec: 20_000_000,
        };
        // SAFETY: two live objects and a live `timespec`.
        unsafe {
            assert_eq!(cond_init(cond.at(), 0), 0);
            assert_eq!(mutex_init(mutex.at(), 0), 0);
            assert_eq!(mutex_lock(mutex.at()), 0);
            let start = std::time::Instant::now();
            assert_eq!(
                cond_timedwait_relative(cond.at(), mutex.at(), &interval as *const _ as u64),
                err::ETIMEDOUT
            );
            assert!(
                start.elapsed() >= std::time::Duration::from_millis(15),
                "it waited"
            );
            assert_eq!(mutex_unlock(mutex.at()), 0);
            assert_eq!(cond_destroy(cond.at()), 0);
            assert_eq!(mutex_destroy(mutex.at()), 0);
        }
    }

    #[test]
    fn a_relative_timed_wait_rejects_invalid_intervals() {
        let cond = Cond::zeroed();
        let mutex = Mutex::zeroed();
        // SAFETY: two live objects and live `timespec` locals.
        unsafe {
            assert_eq!(cond_init(cond.at(), 0), 0);
            assert_eq!(mutex_init(mutex.at(), 0), 0);
            assert_eq!(mutex_lock(mutex.at()), 0);
            for interval in [
                libc::timespec {
                    tv_sec: -1,
                    tv_nsec: 0,
                },
                libc::timespec {
                    tv_sec: 0,
                    tv_nsec: -1,
                },
                libc::timespec {
                    tv_sec: 0,
                    tv_nsec: NANOSECONDS,
                },
            ] {
                assert_eq!(
                    cond_timedwait_relative(cond.at(), mutex.at(), &interval as *const _ as u64),
                    err::EINVAL
                );
            }
            assert_eq!(mutex_unlock(mutex.at()), 0);
            assert_eq!(cond_destroy(cond.at()), 0);
            assert_eq!(mutex_destroy(mutex.at()), 0);
        }
    }
}
