use std::sync::atomic::Ordering;

use super::consts::*;
use super::object::{self, Kind, Record, State};
use crate::wait_word::{self, Scope, WakeCount};

const STATE: u64 = 0;
const SLEEPERS: u64 = 4;
const WRITER: u64 = 8;
const BODY: usize = 16;

/// The top bit of the state word: somebody holds it for writing.
const WRITE_LOCKED: u32 = 1 << 31;
/// The rest of it counts readers.
const READERS: u32 = !WRITE_LOCKED;

const _: () = assert!(16 + BODY <= object::size::RWLOCK);

fn lay_out(record: &Record) {
    record.u32(STATE).store(0, Ordering::Relaxed);
    record.u32(SLEEPERS).store(0, Ordering::Relaxed);
    record.u64(WRITER).store(0, Ordering::Relaxed);
}

/// # Safety
///
/// `at` is a guest `pthread_rwlock_t *`.
unsafe fn live(at: u64) -> Option<Record> {
    if at == 0 {
        return None;
    }
    let record = unsafe { Record::at(at, Kind::RwLock) };
    record
        .live_or_init(|record, _| {
            lay_out(record);
            true
        })
        .then_some(record)
}

/// Sleep until the state word stops being `seen`.
///
/// The count is announced before the state is re-read, and an unlocker reads the count
/// after storing the state, so the two cannot both decide the other is absent.
fn sleep_until_changed(record: &Record, seen: u32) {
    record.u32(SLEEPERS).fetch_add(1, Ordering::SeqCst);
    if record.u32(STATE).load(Ordering::SeqCst) == seen {
        // SAFETY: the state word of a live read-write lock.
        let _ = unsafe { wait_word::wait_u32(record.u32(STATE), seen, None, Scope::Private) };
    }
    record.u32(SLEEPERS).fetch_sub(1, Ordering::Relaxed);
}

/// Release everybody waiting, because which of them can now proceed depends on what
/// they each wanted and only they can tell.
fn wake_waiters(record: &Record) {
    if record.u32(SLEEPERS).load(Ordering::SeqCst) != 0 {
        // SAFETY: the state word of a live read-write lock.
        let _ = unsafe { wait_word::wake_u32(record.u32(STATE), WakeCount::All, Scope::Private) };
    }
}

fn read_lock(record: &Record, try_only: bool) -> i32 {
    loop {
        let seen = record.u32(STATE).load(Ordering::Relaxed);
        if seen & WRITE_LOCKED != 0 {
            // Darwin checks the owner before it looks at `try_only`.
            if record.u64(WRITER).load(Ordering::Relaxed) == super::id() as u64 {
                return err::EDEADLK;
            }
            if try_only {
                return err::EBUSY;
            }
            sleep_until_changed(record, seen);
            continue;
        }
        if seen == READERS {
            // Darwin reports a reader count it cannot grow as `EAGAIN`.
            return err::EAGAIN;
        }
        if record
            .u32(STATE)
            .compare_exchange_weak(seen, seen + 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return 0;
        }
    }
}

fn write_lock(record: &Record, try_only: bool) -> i32 {
    let self_id = super::id() as u64;
    loop {
        let seen = record.u32(STATE).load(Ordering::Relaxed);
        if seen != 0 {
            if seen & WRITE_LOCKED != 0 && record.u64(WRITER).load(Ordering::Relaxed) == self_id {
                return err::EDEADLK;
            }
            if try_only {
                return err::EBUSY;
            }
            sleep_until_changed(record, seen);
            continue;
        }
        if record
            .u32(STATE)
            .compare_exchange_weak(0, WRITE_LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            record.u64(WRITER).store(self_id, Ordering::Relaxed);
            return 0;
        }
    }
}

fn unlock(record: &Record) -> i32 {
    let seen = record.u32(STATE).load(Ordering::Relaxed);
    if seen & WRITE_LOCKED != 0 {
        if record.u64(WRITER).load(Ordering::Relaxed) != super::id() as u64 {
            return err::EPERM;
        }
        record.u64(WRITER).store(0, Ordering::Relaxed);
        record.u32(STATE).store(0, Ordering::SeqCst);
        wake_waiters(record);
        return 0;
    }
    if seen == 0 {
        // Nobody holds it. POSIX leaves this undefined; Darwin reports it.
        return err::EPERM;
    }
    // Only a writer can be waiting on a lock that still has readers, and a writer
    // needs the count at zero - so this is the only reader unlock that has to wake.
    if record.u32(STATE).fetch_sub(1, Ordering::SeqCst) == 1 {
        wake_waiters(record);
    }
    0
}

/// # Safety
///
/// `at` is a guest `pthread_rwlock_t *` and `attr` is null or a guest
/// `pthread_rwlockattr_t *`.
pub unsafe extern "C" fn rwlock_init(at: u64, attr: u64) -> i32 {
    if at == 0 {
        return err::EINVAL;
    }
    let Some(shared) = (unsafe { super::sync::simple_attr_pshared(attr, Kind::RwLockAttr) }) else {
        return err::EINVAL;
    };
    if shared == Some(PTHREAD_PROCESS_SHARED) {
        super::sync::say_unsupported("a process-shared read-write lock");
        return err::ENOTSUP;
    }
    let record = unsafe { Record::at(at, Kind::RwLock) };
    record.seize();
    record.publish(lay_out);
    0
}

/// # Safety
///
/// As [`rwlock_init`].
pub unsafe extern "C" fn rwlock_destroy(at: u64) -> i32 {
    if at == 0 {
        return err::EINVAL;
    }
    let record = unsafe { Record::at(at, Kind::RwLock) };
    let state = record.seize();
    match state {
        State::Invalid => err::EINVAL,
        State::Fresh(_) => {
            record.bury();
            0
        }
        State::Live
            if record.u32(STATE).load(Ordering::Acquire) != 0
                || record.u32(SLEEPERS).load(Ordering::Acquire) != 0 =>
        {
            record.restore(state);
            err::EBUSY
        }
        State::Live => {
            record.bury();
            0
        }
    }
}

macro_rules! rwlock_op {
    ($name:ident, $body:expr) => {
        /// # Safety
        ///
        /// As [`rwlock_init`].
        pub unsafe extern "C" fn $name(at: u64) -> i32 {
            match unsafe { live(at) } {
                #[allow(clippy::redundant_closure_call)]
                Some(record) => ($body)(&record),
                None => err::EINVAL,
            }
        }
    };
}

rwlock_op!(rwlock_rdlock, |r| read_lock(r, false));
rwlock_op!(rwlock_tryrdlock, |r| read_lock(r, true));
rwlock_op!(rwlock_wrlock, |r| write_lock(r, false));
rwlock_op!(rwlock_trywrlock, |r| write_lock(r, true));
rwlock_op!(rwlock_unlock, unlock);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pthread::testing::Guest;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;

    type RwLock = Guest<{ object::size::RWLOCK }>;

    #[test]
    fn readers_share_and_a_writer_excludes_everyone() {
        let lock = RwLock::zeroed();
        assert_eq!(unsafe { rwlock_init(lock.at(), 0) }, 0);
        assert_eq!(unsafe { rwlock_rdlock(lock.at()) }, 0);

        {
            let lock = Arc::clone(&lock);
            std::thread::spawn(move || unsafe {
                assert_eq!(rwlock_trywrlock(lock.at()), err::EBUSY, "a reader holds it");
                assert_eq!(rwlock_tryrdlock(lock.at()), 0, "readers share");
                assert_eq!(rwlock_unlock(lock.at()), 0);
            })
            .join()
            .expect("the second reader");
        }

        unsafe {
            assert_eq!(rwlock_unlock(lock.at()), 0);
            assert_eq!(rwlock_wrlock(lock.at()), 0, "and now it is free");
        }

        {
            let lock = Arc::clone(&lock);
            std::thread::spawn(move || unsafe {
                assert_eq!(rwlock_trywrlock(lock.at()), err::EBUSY);
                assert_eq!(rwlock_tryrdlock(lock.at()), err::EBUSY);
            })
            .join()
            .expect("the excluded thread");
        }

        unsafe {
            assert_eq!(rwlock_unlock(lock.at()), 0);
            assert_eq!(rwlock_destroy(lock.at()), 0);
        }
    }

    #[test]
    fn a_statically_initialised_lock_works_without_an_init_call() {
        let lock = RwLock::statically_initialised(object::sig::RWLOCK_INIT);
        unsafe {
            assert_eq!(rwlock_wrlock(lock.at()), 0);
            assert_eq!(rwlock_unlock(lock.at()), 0);
            assert_eq!(rwlock_destroy(lock.at()), 0);
        }
    }

    /// A blocked writer must actually be woken by the last reader leaving, and a
    /// blocked reader by the writer leaving - which is the part a lock that only ever
    /// spun would pass and a lock that forgot to wake would hang on.
    #[test]
    fn blocked_threads_are_woken_rather_than_left_asleep() {
        let lock = RwLock::zeroed();
        let progress = Arc::new(AtomicU32::new(0));
        assert_eq!(unsafe { rwlock_init(lock.at(), 0) }, 0);
        assert_eq!(unsafe { rwlock_rdlock(lock.at()) }, 0);

        let writer = {
            let (lock, progress) = (Arc::clone(&lock), Arc::clone(&progress));
            std::thread::spawn(move || unsafe {
                assert_eq!(rwlock_wrlock(lock.at()), 0);
                progress.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(20));
                assert_eq!(rwlock_unlock(lock.at()), 0);
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(progress.load(Ordering::SeqCst), 0, "the reader excludes it");

        let reader = {
            let (lock, progress) = (Arc::clone(&lock), Arc::clone(&progress));
            std::thread::spawn(move || unsafe {
                assert_eq!(rwlock_rdlock(lock.at()), 0);
                progress.fetch_add(1, Ordering::SeqCst);
                assert_eq!(rwlock_unlock(lock.at()), 0);
            })
        };

        assert_eq!(unsafe { rwlock_unlock(lock.at()) }, 0);
        writer.join().expect("the writer was woken");
        reader.join().expect("the reader was woken");
        assert_eq!(progress.load(Ordering::SeqCst), 2);
        assert_eq!(unsafe { rwlock_destroy(lock.at()) }, 0);
    }

    /// Many threads reading and writing at once must never see a writer overlap
    /// anybody: the counter is only ever touched by a thread that holds it alone.
    #[test]
    fn writers_never_overlap_a_reader() {
        let lock = RwLock::zeroed();
        let inside = Arc::new(AtomicU32::new(0));
        let value = Arc::new(AtomicU32::new(0));
        assert_eq!(unsafe { rwlock_init(lock.at(), 0) }, 0);

        let threads: Vec<_> = (0..8)
            .map(|which| {
                let (lock, inside, value) =
                    (Arc::clone(&lock), Arc::clone(&inside), Arc::clone(&value));
                std::thread::spawn(move || {
                    for _ in 0..2_000 {
                        if which % 2 == 0 {
                            assert_eq!(unsafe { rwlock_rdlock(lock.at()) }, 0);
                            assert_eq!(inside.load(Ordering::SeqCst), 0, "a writer overlapped");
                            assert_eq!(unsafe { rwlock_unlock(lock.at()) }, 0);
                        } else {
                            assert_eq!(unsafe { rwlock_wrlock(lock.at()) }, 0);
                            assert_eq!(inside.fetch_add(1, Ordering::SeqCst), 0);
                            let seen = value.load(Ordering::Relaxed);
                            value.store(seen + 1, Ordering::Relaxed);
                            assert_eq!(inside.fetch_sub(1, Ordering::SeqCst), 1);
                            assert_eq!(unsafe { rwlock_unlock(lock.at()) }, 0);
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(value.load(Ordering::Relaxed), 4 * 2_000, "a lost update");
        assert_eq!(unsafe { rwlock_destroy(lock.at()) }, 0);
    }

    #[test]
    fn unlocking_one_nobody_holds_is_refused() {
        let lock = RwLock::zeroed();
        unsafe {
            assert_eq!(rwlock_init(lock.at(), 0), 0);
            assert_eq!(rwlock_unlock(lock.at()), err::EPERM);
            assert_eq!(rwlock_wrlock(lock.at()), 0);
            assert_eq!(rwlock_wrlock(lock.at()), err::EDEADLK, "it is already ours");
            assert_eq!(rwlock_destroy(lock.at()), err::EBUSY, "still held");
            assert_eq!(rwlock_unlock(lock.at()), 0);
            assert_eq!(rwlock_destroy(lock.at()), 0);
        }
    }

    /// Darwin refuses any lock, tried or not, from the thread holding the write
    /// lock rather than letting it deadlock against itself.
    #[test]
    fn a_writer_asking_to_read_is_told_it_would_deadlock() {
        let lock = RwLock::statically_initialised(object::sig::RWLOCK_INIT);
        unsafe {
            assert_eq!(rwlock_wrlock(lock.at()), 0);
            assert_eq!(rwlock_rdlock(lock.at()), err::EDEADLK);
            assert_eq!(rwlock_tryrdlock(lock.at()), err::EDEADLK);
            assert_eq!(rwlock_trywrlock(lock.at()), err::EDEADLK);
            assert_eq!(rwlock_unlock(lock.at()), 0);
            assert_eq!(rwlock_rdlock(lock.at()), 0);
            assert_eq!(rwlock_unlock(lock.at()), 0);
        }
    }
}
