//! Many threads, many iterations: the cases a single-shot unit test can pass
//! by luck. Each test keeps to well under a few seconds.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Barrier};

use super::consts::*;
use super::object::{self, Kind, Record};
use super::sync::{self, MutexAttr};
use super::testing::Guest;

type Mutex = Guest<{ object::size::MUTEX }>;
type Cond = Guest<{ object::size::COND }>;
type RwLock = Guest<{ object::size::RWLOCK }>;
type Attr = Guest<{ object::size::MUTEXATTR }>;
type Once = Guest<{ object::size::ONCE }>;

/// Plain memory the guest's locks are meant to protect. Any race shows up as
/// a lost update or a torn pair.
struct Racy<T>(UnsafeCell<T>);

unsafe impl<T: Send> Sync for Racy<T> {}

impl<T> Racy<T> {
    fn new(value: T) -> Arc<Self> {
        Arc::new(Racy(UnsafeCell::new(value)))
    }

    #[allow(clippy::mut_from_ref)]
    unsafe fn get(&self) -> &mut T {
        unsafe { &mut *self.0.get() }
    }
}

fn recursive_mutex() -> Arc<Mutex> {
    let attr = Attr::zeroed();
    let record = unsafe { Record::at(attr.at(), Kind::MutexAttr) };
    object::publish_cell(
        &record,
        MutexAttr {
            kind: PTHREAD_MUTEX_RECURSIVE,
            ..MutexAttr::default()
        }
        .packed_public(),
    );
    let m = Mutex::zeroed();
    assert_eq!(unsafe { sync::mutex_init(m.at(), attr.at()) }, 0);
    m
}

fn realtime_in(nanoseconds: i64) -> libc::timespec {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut now) };
    let total = now.tv_nsec + nanoseconds;
    libc::timespec {
        tv_sec: now.tv_sec + total / 1_000_000_000,
        tv_nsec: total % 1_000_000_000,
    }
}

/// Every round starts the threads on a fresh static initializer at once, so
/// the lazy build is raced every time, for each mutex kind.
#[test]
fn racing_first_touches_of_static_mutexes_share_one_lock() {
    const THREADS: usize = 8;
    const EACH: u32 = 1_000;
    for round in 0..300 {
        let signature = match round % 3 {
            0 => object::sig::MUTEX_INIT,
            1 => object::sig::RECURSIVE_MUTEX_INIT,
            _ => object::sig::ERRORCHECK_MUTEX_INIT,
        };
        let m = Mutex::statically_initialised(signature);
        let counter = Racy::new(0u32);
        let start = Arc::new(Barrier::new(THREADS));
        let threads: Vec<_> = (0..THREADS)
            .map(|_| {
                let (m, counter, start) =
                    (Arc::clone(&m), Arc::clone(&counter), Arc::clone(&start));
                std::thread::spawn(move || {
                    start.wait();
                    for _ in 0..EACH {
                        unsafe {
                            assert_eq!(sync::mutex_lock(m.at()), 0);
                            *counter.get() += 1;
                            assert_eq!(sync::mutex_unlock(m.at()), 0);
                        }
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("a locking thread");
        }
        assert_eq!(
            unsafe { *counter.get() },
            THREADS as u32 * EACH,
            "round {round}"
        );
        assert_eq!(unsafe { sync::mutex_destroy(m.at()) }, 0);
    }
}

/// A trylock spinner and blocking lockers on one mutex: a successful trylock
/// excludes exactly as a lock does.
#[test]
fn trylock_excludes_like_lock() {
    const EACH: u32 = 20_000;
    let m = Mutex::statically_initialised(object::sig::MUTEX_INIT);
    let counter = Racy::new(0u32);
    let threads: Vec<_> = (0..6)
        .map(|which| {
            let (m, counter) = (Arc::clone(&m), Arc::clone(&counter));
            std::thread::spawn(move || {
                let mut done = 0;
                while done < EACH {
                    unsafe {
                        let rc = match which % 2 {
                            0 => sync::mutex_trylock(m.at()),
                            _ => sync::mutex_lock(m.at()),
                        };
                        if rc == err::EBUSY {
                            std::hint::spin_loop();
                            continue;
                        }
                        assert_eq!(rc, 0);
                        *counter.get() += 1;
                        assert_eq!(sync::mutex_unlock(m.at()), 0);
                    }
                    done += 1;
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(unsafe { *counter.get() }, 6 * EACH);
}

/// A bounded queue of plain memory, with producers and consumers on two
/// statically initialised condition variables and one static mutex. Every
/// item must come out exactly once, and nobody may hang.
#[test]
fn producers_and_consumers_hand_off_every_item() {
    const PRODUCERS: u64 = 3;
    const CONSUMERS: u64 = 3;
    const EACH: u64 = 20_000;
    const CAPACITY: usize = 4;
    let m = Mutex::statically_initialised(object::sig::MUTEX_INIT);
    let not_empty = Cond::statically_initialised(object::sig::COND_INIT);
    let not_full = Cond::statically_initialised(object::sig::COND_INIT);
    let queue = Racy::new(std::collections::VecDeque::<u64>::new());
    let consumed = Racy::new((0u64, 0u64));

    let mut threads = Vec::new();
    for p in 0..PRODUCERS {
        let (m, not_empty, not_full, queue) = (
            Arc::clone(&m),
            Arc::clone(&not_empty),
            Arc::clone(&not_full),
            Arc::clone(&queue),
        );
        threads.push(std::thread::spawn(move || {
            for i in 0..EACH {
                unsafe {
                    assert_eq!(sync::mutex_lock(m.at()), 0);
                    while queue.get().len() == CAPACITY {
                        assert_eq!(sync::cond_wait(not_full.at(), m.at()), 0);
                    }
                    queue.get().push_back(p * EACH + i + 1);
                    // Alternate signalling under and after the mutex.
                    if i % 2 == 0 {
                        assert_eq!(sync::cond_signal(not_empty.at()), 0);
                        assert_eq!(sync::mutex_unlock(m.at()), 0);
                    } else {
                        assert_eq!(sync::mutex_unlock(m.at()), 0);
                        assert_eq!(sync::cond_signal(not_empty.at()), 0);
                    }
                }
            }
        }));
    }
    for _ in 0..CONSUMERS {
        let (m, not_empty, not_full, queue, consumed) = (
            Arc::clone(&m),
            Arc::clone(&not_empty),
            Arc::clone(&not_full),
            Arc::clone(&queue),
            Arc::clone(&consumed),
        );
        threads.push(std::thread::spawn(move || {
            for _ in 0..EACH * PRODUCERS / CONSUMERS {
                unsafe {
                    assert_eq!(sync::mutex_lock(m.at()), 0);
                    while queue.get().is_empty() {
                        assert_eq!(sync::cond_wait(not_empty.at(), m.at()), 0);
                    }
                    let item = queue.get().pop_front().unwrap();
                    let (count, sum) = consumed.get();
                    *count += 1;
                    *sum += item;
                    assert_eq!(sync::cond_signal(not_full.at()), 0);
                    assert_eq!(sync::mutex_unlock(m.at()), 0);
                }
            }
        }));
    }
    for t in threads {
        t.join().expect("a producer or consumer");
    }
    let total = PRODUCERS * EACH;
    assert_eq!(unsafe { *consumed.get() }, (total, total * (total + 1) / 2));
    unsafe {
        assert_eq!(sync::cond_destroy(not_empty.at()), 0);
        assert_eq!(sync::cond_destroy(not_full.at()), 0);
        assert_eq!(sync::mutex_destroy(m.at()), 0);
    }
}

/// Short absolute deadlines racing real signals: a timed wait returns with the
/// mutex held whether it timed out or not, and a signalled item is never lost
/// to a timeout.
#[test]
fn timed_waits_racing_signals_keep_the_mutex_and_the_items() {
    const ITEMS: u32 = 10_000;
    let m = Mutex::statically_initialised(object::sig::MUTEX_INIT);
    let c = Cond::statically_initialised(object::sig::COND_INIT);
    let state = Racy::new((0u32, 0u32, false)); // produced, consumed, holder
    let mut threads = Vec::new();
    for _ in 0..3 {
        let (m, c, state) = (Arc::clone(&m), Arc::clone(&c), Arc::clone(&state));
        threads.push(std::thread::spawn(move || {
            loop {
                unsafe {
                    assert_eq!(sync::mutex_lock(m.at()), 0);
                    loop {
                        let (produced, consumed, _) = *state.get();
                        if consumed == ITEMS {
                            assert_eq!(sync::mutex_unlock(m.at()), 0);
                            return;
                        }
                        if produced > consumed {
                            break;
                        }
                        let deadline = realtime_in(50_000);
                        let rc = sync::cond_timedwait(c.at(), m.at(), &deadline as *const _ as u64);
                        assert!(rc == 0 || rc == err::ETIMEDOUT, "rc {rc}");
                        // Held again: nobody else may be inside.
                        let holder = &mut state.get().2;
                        assert!(!*holder, "two threads inside the mutex");
                        *holder = true;
                        *holder = false;
                    }
                    state.get().1 += 1;
                    assert_eq!(sync::mutex_unlock(m.at()), 0);
                }
            }
        }));
    }
    for _ in 0..ITEMS {
        unsafe {
            assert_eq!(sync::mutex_lock(m.at()), 0);
            state.get().0 += 1;
            assert_eq!(sync::mutex_unlock(m.at()), 0);
            assert_eq!(sync::cond_signal(c.at()), 0);
        }
    }
    unsafe { assert_eq!(sync::cond_broadcast(c.at()), 0) };
    for t in threads {
        t.join().expect("a timed waiter");
    }
    assert_eq!(unsafe { state.get().1 }, ITEMS);
}

/// A recursive mutex held once is fully released by a condition wait and
/// comes back held once; held twice, Darwin keeps it across the wait, and the
/// count is the same afterwards either way.
#[test]
fn a_recursive_mutex_keeps_its_count_across_a_condition_wait() {
    let m = recursive_mutex();
    let c = Cond::statically_initialised(object::sig::COND_INIT);
    let ready = Arc::new(AtomicBool::new(false));
    unsafe {
        assert_eq!(sync::mutex_lock(m.at()), 0);
        assert_eq!(sync::mutex_lock(m.at()), 0);
        assert_eq!(sync::mutex_unlock(m.at()), 0);
    }
    let signaller = {
        let (m, c, ready) = (Arc::clone(&m), Arc::clone(&c), Arc::clone(&ready));
        std::thread::spawn(move || unsafe {
            // Only possible because the wait really released it.
            assert_eq!(sync::mutex_lock(m.at()), 0);
            ready.store(true, Ordering::Relaxed);
            assert_eq!(sync::cond_signal(c.at()), 0);
            assert_eq!(sync::mutex_unlock(m.at()), 0);
        })
    };
    unsafe {
        while !ready.load(Ordering::Relaxed) {
            assert_eq!(sync::cond_wait(c.at(), m.at()), 0);
        }
    }
    signaller.join().unwrap();
    unsafe {
        assert_eq!(sync::mutex_unlock(m.at()), 0);
        assert_eq!(sync::mutex_unlock(m.at()), err::EPERM, "held exactly once");

        // Twice: the wait drops one level only, so another thread is still
        // shut out, and both levels are there afterwards.
        assert_eq!(sync::mutex_lock(m.at()), 0);
        assert_eq!(sync::mutex_lock(m.at()), 0);
        let other = {
            let m = Arc::clone(&m);
            std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(30);
                while std::time::Instant::now() < deadline {
                    assert_eq!(sync::mutex_trylock(m.at()), err::EBUSY);
                    std::thread::yield_now();
                }
            })
        };
        let deadline = realtime_in(40_000_000);
        assert_eq!(
            sync::cond_timedwait(c.at(), m.at(), &deadline as *const _ as u64),
            err::ETIMEDOUT
        );
        other.join().expect("the mutex stayed held");
        assert_eq!(sync::mutex_unlock(m.at()), 0);
        assert_eq!(sync::mutex_unlock(m.at()), 0);
        assert_eq!(sync::mutex_unlock(m.at()), err::EPERM);
        assert_eq!(sync::mutex_destroy(m.at()), 0);
    }
}

/// Threads pass a turn round a ring, each taking a recursive mutex twice and
/// waiting at depth one. Any lost release, lost wake or wrong count hangs or
/// miscounts.
#[test]
fn a_ring_of_recursive_waiters_passes_every_turn() {
    const THREADS: u32 = 4;
    const ROUNDS: u32 = 5_000;
    let m = recursive_mutex();
    let c = Cond::zeroed();
    assert_eq!(unsafe { sync::cond_init(c.at(), 0) }, 0);
    let state = Racy::new((0u32, 0u32)); // turn, counter
    let threads: Vec<_> = (0..THREADS)
        .map(|me| {
            let (m, c, state) = (Arc::clone(&m), Arc::clone(&c), Arc::clone(&state));
            std::thread::spawn(move || unsafe {
                for _ in 0..ROUNDS {
                    assert_eq!(sync::mutex_lock(m.at()), 0);
                    assert_eq!(sync::mutex_lock(m.at()), 0);
                    state.get().1 += 1;
                    assert_eq!(sync::mutex_unlock(m.at()), 0);
                    while state.get().0 != me {
                        assert_eq!(sync::cond_wait(c.at(), m.at()), 0);
                    }
                    state.get().0 = (me + 1) % THREADS;
                    assert_eq!(sync::cond_broadcast(c.at()), 0);
                    assert_eq!(sync::mutex_unlock(m.at()), 0);
                }
            })
        })
        .collect();
    for t in threads {
        t.join().expect("a ring member");
    }
    assert_eq!(unsafe { state.get().1 }, THREADS * ROUNDS);
}

/// Readers check a two-word invariant that writers break and restore inside
/// the lock: a reader overlapping a writer sees the two differ.
#[test]
fn readers_never_see_a_writer_half_way() {
    const EACH: u64 = 15_000;
    let lock = RwLock::statically_initialised(object::sig::RWLOCK_INIT);
    let pair = Racy::new((0u64, 0u64));
    let readers_inside = Arc::new(AtomicU32::new(0));
    let threads: Vec<_> = (0..8)
        .map(|which| {
            let (lock, pair, inside) = (
                Arc::clone(&lock),
                Arc::clone(&pair),
                Arc::clone(&readers_inside),
            );
            std::thread::spawn(move || {
                for i in 0..EACH {
                    unsafe {
                        if which % 4 == 0 {
                            let rc = match i % 2 {
                                0 => sync::rwlock_wrlock(lock.at()),
                                _ => loop {
                                    match sync::rwlock_trywrlock(lock.at()) {
                                        e if e == err::EBUSY => std::thread::yield_now(),
                                        rc => break rc,
                                    }
                                },
                            };
                            assert_eq!(rc, 0);
                            assert_eq!(inside.load(Ordering::SeqCst), 0, "a reader is inside");
                            let p = pair.get();
                            p.0 += 1;
                            std::hint::spin_loop();
                            p.1 += 1;
                        } else {
                            let rc = match i % 2 {
                                0 => sync::rwlock_rdlock(lock.at()),
                                _ => loop {
                                    match sync::rwlock_tryrdlock(lock.at()) {
                                        e if e == err::EBUSY => std::thread::yield_now(),
                                        rc => break rc,
                                    }
                                },
                            };
                            assert_eq!(rc, 0);
                            inside.fetch_add(1, Ordering::SeqCst);
                            let (a, b) = *pair.get();
                            assert_eq!(a, b, "a writer was half way");
                            inside.fetch_sub(1, Ordering::SeqCst);
                        }
                        assert_eq!(sync::rwlock_unlock(lock.at()), 0);
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().expect("a reader or writer");
    }
    assert_eq!(unsafe { *pair.get() }, (2 * EACH, 2 * EACH));
    assert_eq!(unsafe { sync::rwlock_destroy(lock.at()) }, 0);
}

/// Fresh once objects raced by many threads at once, round after round: the
/// routine runs once and its plain writes are visible to every caller.
#[test]
fn racing_pthread_once_runs_once_and_publishes() {
    const THREADS: usize = 8;
    for _ in 0..500 {
        let control = Once::statically_initialised(object::sig::ONCE_INIT);
        let data = Racy::new([0u64; 8]);
        let runs = Arc::new(AtomicU32::new(0));
        let start = Arc::new(Barrier::new(THREADS));
        let threads: Vec<_> = (0..THREADS)
            .map(|_| {
                let (control, data, runs, start) = (
                    Arc::clone(&control),
                    Arc::clone(&data),
                    Arc::clone(&runs),
                    Arc::clone(&start),
                );
                std::thread::spawn(move || {
                    start.wait();
                    let rc = unsafe {
                        sync::once(control.at(), || {
                            runs.fetch_add(1, Ordering::Relaxed);
                            *data.get() = [7; 8];
                        })
                    };
                    assert_eq!(rc, 0);
                    assert_eq!(unsafe { *data.get() }, [7; 8], "not yet published");
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(runs.load(Ordering::Relaxed), 1);
    }
}

/// Darwin's lock state lives in the object's own bytes, so a byte copy of an
/// initialised mutex is a second, independent mutex there. Refusing it would
/// leave a guest that copied one with no lock at all.
#[test]
fn a_copied_mutex_is_an_independent_mutex() {
    let original = Mutex::zeroed();
    unsafe { assert_eq!(sync::mutex_init(original.at(), 0), 0) };
    let copy = Mutex::copy_of(&original);
    let counter = Racy::new(0u32);
    unsafe {
        assert_eq!(sync::mutex_lock(original.at()), 0);
        assert_eq!(
            sync::mutex_trylock(copy.at()),
            0,
            "the copy is its own lock"
        );
        assert_eq!(sync::mutex_unlock(copy.at()), 0);
    }
    let threads: Vec<_> = (0..6)
        .map(|_| {
            let (copy, counter) = (Arc::clone(&copy), Arc::clone(&counter));
            std::thread::spawn(move || {
                for _ in 0..3_000 {
                    unsafe {
                        assert_eq!(sync::mutex_lock(copy.at()), 0);
                        *counter.get() += 1;
                        assert_eq!(sync::mutex_unlock(copy.at()), 0);
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(unsafe { *counter.get() }, 18_000);
    unsafe {
        assert_eq!(sync::mutex_unlock(original.at()), 0);
        assert_eq!(sync::mutex_destroy(copy.at()), 0);
        assert_eq!(sync::mutex_destroy(original.at()), 0);
    }
}

/// A copy of a locked recursive mutex is locked by the same owner, as a byte
/// copy is on Darwin, and a copied condition variable still works.
#[test]
fn copies_carry_their_state_with_them() {
    let original = recursive_mutex();
    unsafe { assert_eq!(sync::mutex_lock(original.at()), 0) };
    let copy = Mutex::copy_of(&original);
    unsafe {
        assert_eq!(
            sync::mutex_lock(copy.at()),
            0,
            "same owner, one level deeper"
        );
        assert_eq!(sync::mutex_unlock(copy.at()), 0);
        assert_eq!(sync::mutex_unlock(copy.at()), 0);
        assert_eq!(sync::mutex_unlock(copy.at()), err::EPERM);
        assert_eq!(sync::mutex_unlock(original.at()), 0);
    }

    let c = Cond::zeroed();
    unsafe { assert_eq!(sync::cond_init(c.at(), 0), 0) };
    let c = Cond::copy_of(&c);
    let m = Mutex::statically_initialised(object::sig::MUTEX_INIT);
    let ready = Arc::new(AtomicBool::new(false));
    let waiter = {
        let (c, m, ready) = (Arc::clone(&c), Arc::clone(&m), Arc::clone(&ready));
        std::thread::spawn(move || unsafe {
            assert_eq!(sync::mutex_lock(m.at()), 0);
            while !ready.load(Ordering::Relaxed) {
                assert_eq!(sync::cond_wait(c.at(), m.at()), 0);
            }
            assert_eq!(sync::mutex_unlock(m.at()), 0);
        })
    };
    unsafe {
        assert_eq!(sync::mutex_lock(m.at()), 0);
        ready.store(true, Ordering::Relaxed);
        assert_eq!(sync::cond_broadcast(c.at()), 0);
        assert_eq!(sync::mutex_unlock(m.at()), 0);
    }
    waiter.join().unwrap();
}
