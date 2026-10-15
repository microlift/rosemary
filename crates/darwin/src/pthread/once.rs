use std::sync::atomic::Ordering;

use super::consts::*;
use super::object::{self, Kind, Record};
use crate::wait_word::{self, Scope, WakeCount};

const STATE: u64 = 0;
const SLEEPERS: u64 = 4;
const BODY: usize = 8;

const NEVER: u32 = 0;
const RUNNING: u32 = 1;
const DONE: u32 = 2;

const _: () = assert!(8 + BODY <= object::size::ONCE);

/// Run `body` at most once for the `pthread_once_t` at `at`.
///
/// Every caller that arrives while the routine is running waits for it to finish, so
/// that `pthread_once` returning means the thing it initialises is ready.
///
/// Not an `extern "C"` function, because the routine a guest passes is *guest code*
/// and reaching it needs a thunk. `rosemary::shims::pthread` supplies the call and
/// this supplies the once-and-only-once.
///
/// # Safety
///
/// `at` is a guest `pthread_once_t *`.
pub unsafe fn once<F: FnOnce()>(at: u64, body: F) -> i32 {
    if at == 0 {
        return err::EINVAL;
    }
    let record = unsafe { Record::at(at, Kind::Once) };
    let built = record.live_or_init(|record, _| {
        record.u32(STATE).store(NEVER, Ordering::Relaxed);
        record.u32(SLEEPERS).store(0, Ordering::Relaxed);
        true
    });
    if !built {
        return err::EINVAL;
    }

    let state = record.u32(STATE);
    if state.load(Ordering::Acquire) == DONE {
        return 0;
    }
    let mut body = Some(body);
    loop {
        match state.compare_exchange(NEVER, RUNNING, Ordering::Acquire, Ordering::Acquire) {
            Ok(_) => {
                // A routine that never returns leaves this `RUNNING` forever, which is
                // what a real `pthread_once` does too.
                body.take().expect("the routine runs once")();
                // Pair with the sleeper registration before deciding whether to wake.
                state.store(DONE, Ordering::SeqCst);
                if record.u32(SLEEPERS).load(Ordering::SeqCst) != 0 {
                    // SAFETY: the state word of a live `pthread_once_t`.
                    let _ = unsafe { wait_word::wake_u32(state, WakeCount::All, Scope::Private) };
                }
                return 0;
            }
            Err(DONE) => return 0,
            Err(_) => {
                record.u32(SLEEPERS).fetch_add(1, Ordering::SeqCst);
                if state.load(Ordering::SeqCst) == RUNNING {
                    let _ = unsafe { wait_word::wait_u32(state, RUNNING, None, Scope::Private) };
                }
                record.u32(SLEEPERS).fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
}

/// `int pthread_once(pthread_once_t *, void (*routine)(void))`
///
/// The entry point the *native* backend binds, where the guest's code is already host
/// code and the routine can simply be called. Under emulation the routine is x86-64
/// that needs a thunk, so `rosemary::shims::pthread` intercepts the call by name and
/// reaches [`once`] itself.
///
/// # Safety
///
/// `at` is a guest `pthread_once_t *` and `routine` is a function this process can
/// call.
pub unsafe extern "C" fn once_entry(at: u64, routine: Option<extern "C" fn()>) -> i32 {
    let Some(routine) = routine else {
        return err::EINVAL;
    };
    unsafe { once(at, || routine()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pthread::testing::Guest;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;

    type Once = Guest<{ object::size::ONCE }>;

    /// Exactly once, and every thread that asked waits for it - which is the whole
    /// contract, and the one a spin that returned early would break.
    #[test]
    fn it_runs_once_and_everyone_waits_for_it() {
        let control = Once::statically_initialised(object::sig::ONCE_INIT);
        let ran = Arc::new(AtomicU32::new(0));
        let finished = Arc::new(AtomicU32::new(0));

        let threads: Vec<_> = (0..8)
            .map(|_| {
                let control = Arc::clone(&control);
                let (ran, finished) = (Arc::clone(&ran), Arc::clone(&finished));
                std::thread::spawn(move || {
                    // SAFETY: a live `pthread_once_t` shared with the other threads.
                    let rc = unsafe {
                        once(control.at(), || {
                            // Slow on purpose: a waiter that did not wait would see
                            // the flag still unset.
                            std::thread::sleep(std::time::Duration::from_millis(30));
                            ran.fetch_add(1, Ordering::SeqCst);
                        })
                    };
                    assert_eq!(rc, 0);
                    assert_eq!(
                        ran.load(Ordering::SeqCst),
                        1,
                        "returned before the routine finished"
                    );
                    finished.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("a once thread");
        }
        assert_eq!(ran.load(Ordering::SeqCst), 1, "the routine ran once");
        assert_eq!(finished.load(Ordering::SeqCst), 8, "everyone got through");
    }

    /// Darwin's implementation ignores `__sig`, so a zero-filled control word is a
    /// valid never-run once object and not a broken one.
    #[test]
    fn a_zeroed_control_word_is_a_fresh_once() {
        let control = Once::zeroed();
        let ran = AtomicU32::new(0);
        // SAFETY: a live, correctly sized `pthread_once_t`.
        assert_eq!(
            unsafe {
                once(control.at(), || {
                    ran.fetch_add(1, Ordering::Relaxed);
                })
            },
            0
        );
        assert_eq!(unsafe { once(control.at(), || unreachable!()) }, 0);
        assert_eq!(ran.load(Ordering::Relaxed), 1);
    }
}
