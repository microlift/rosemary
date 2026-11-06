use std::ffi::c_void;
use std::sync::{Condvar, Mutex};

use super::block;
use super::object::{self, Function, Header, Kind};
use super::queue;
use super::time;

/// What a group runs when it empties: a queue and the work to put on it.
enum Notify {
    Block {
        queue: u64,
        block: u64,
    },
    Function {
        queue: u64,
        context: u64,
        function: Function,
    },
}

/// `#[repr(C)]` is load-bearing, not decoration: [`super::object`] recovers this
/// object by casting a `*mut Header`, which is only the same address as the whole
/// object if `header` is guaranteed to come first. A plain Rust struct may reorder
/// its fields, and one that did would read another field's bytes as the refcount.
#[repr(C)]
pub struct Group {
    /// Read through a `*mut Header` rather than through this field, which is what
    /// makes it look unused: it must be first, and its bytes are the object
    /// protocol's.
    #[allow(dead_code)]
    header: Header,
    state: Mutex<GroupState>,
    empty: Condvar,
}

struct GroupState {
    /// Outstanding `dispatch_group_enter`s, including those an `async` took.
    count: i64,
    /// Run and cleared when `count` reaches zero.
    notify: Vec<Notify>,
}

/// `#[repr(C)]` is load-bearing, not decoration: [`super::object`] recovers this
/// object by casting a `*mut Header`, which is only the same address as the whole
/// object if `header` is guaranteed to come first. A plain Rust struct may reorder
/// its fields, and one that did would read another field's bytes as the refcount.
#[repr(C)]
pub struct Semaphore {
    /// Read through a `*mut Header` rather than through this field, which is what
    /// makes it look unused: it must be first, and its bytes are the object
    /// protocol's.
    #[allow(dead_code)]
    header: Header,
    state: Mutex<SemaphoreState>,
    signalled: Condvar,
}

struct SemaphoreState {
    /// Darwin's value: negative is the number of threads waiting.
    value: i64,
    /// Signals owed to waiters and not yet claimed. The value alone cannot say
    /// which waiter a signal was for: with two waiting it goes from -2 to -1, and
    /// a woken waiter that read that as "not mine" slept through its own signal.
    wakeups: u64,
}

/// # Safety
///
/// `handle` is null or an object this module allocated.
unsafe fn group<'a>(handle: u64) -> Option<&'a Group> {
    // SAFETY: the caller's guarantee, and the kind check makes the cast sound.
    unsafe { object::typed(handle, Kind::Group) }
        // SAFETY: a `Header` of kind `Group` is the first field of a `Group`.
        .map(|h| unsafe { &*(h as *const Header as *const Group) })
}

/// # Safety
///
/// As above.
unsafe fn semaphore<'a>(handle: u64) -> Option<&'a Semaphore> {
    // SAFETY: the caller's guarantee, and the kind check makes the cast sound.
    unsafe { object::typed(handle, Kind::Semaphore) }
        // SAFETY: a `Header` of kind `Semaphore` is the first field of a `Semaphore`.
        .map(|h| unsafe { &*(h as *const Header as *const Semaphore) })
}

/// `dispatch_group_t dispatch_group_create(void)`.
pub unsafe extern "C" fn group_create() -> u64 {
    Box::into_raw(Box::new(Group {
        header: Header::new(Kind::Group),
        state: Mutex::new(GroupState {
            count: 0,
            notify: Vec::new(),
        }),
        empty: Condvar::new(),
    })) as u64
}

/// `void dispatch_group_enter(dispatch_group_t)`.
pub unsafe extern "C" fn group_enter(handle: u64) {
    // SAFETY: the guest holds a reference for the duration of the call.
    let Some(g) = (unsafe { group(handle) }) else {
        return;
    };
    let mut state = g.state.lock().unwrap_or_else(|e| e.into_inner());
    if state.count == 0 {
        object::retain(handle);
    }
    state.count += 1;
}

/// `void dispatch_group_leave(dispatch_group_t)`.
///
/// The notifications are taken out from under the lock and submitted after it is
/// dropped: submitting can spawn a worker, and a worker can call back in here.
pub unsafe extern "C" fn group_leave(handle: u64) {
    // SAFETY: the guest holds a reference for the duration of the call.
    let Some(g) = (unsafe { group(handle) }) else {
        return;
    };
    let ready = {
        let mut state = g.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.count == 0 {
            std::process::abort();
        }
        state.count -= 1;
        if state.count != 0 {
            return;
        }
        g.empty.notify_all();
        std::mem::take(&mut state.notify)
    };
    for notify in ready {
        submit(notify);
    }
    unsafe {
        object::release(handle);
    }
}

fn submit(notify: Notify) {
    match notify {
        // SAFETY: a queue handle and a block, both live: the notification holds a
        // reference to each until here.
        Notify::Block { queue: q, block: b } => unsafe {
            queue::async_(q, b);
            block::release(b);
            object::release(q);
        },
        Notify::Function {
            queue: q,
            context,
            function,
        } => unsafe {
            queue::async_f(q, context, function as usize as u64);
            object::release(q);
        },
    }
}

/// Arrange for `notify` to run once the group is empty, running it now if it already
/// is.
fn arm(handle: u64, notify: Notify) {
    // SAFETY: the guest holds a reference for the duration of the call.
    let Some(g) = (unsafe { group(handle) }) else {
        return;
    };
    let mut state = g.state.lock().unwrap_or_else(|e| e.into_inner());
    if state.count == 0 {
        drop(state);
        // An already-empty group notifies immediately, as Darwin's does; a caller
        // that entered nothing still expects its completion to run.
        return submit(notify);
    }
    state.notify.push(notify);
}

/// `void dispatch_group_notify(dispatch_group_t, dispatch_queue_t, dispatch_block_t)`.
///
/// The block is copied here rather than at submission: it has to survive until the
/// group empties, which is after this call returns.
pub unsafe extern "C" fn group_notify(handle: u64, q: u64, block: u64) {
    let Some(copied) = block::copy(block) else {
        return;
    };
    object::retain(q);
    arm(
        handle,
        Notify::Block {
            queue: q,
            block: copied,
        },
    );
}

/// `void dispatch_group_notify_f(dispatch_group_t, dispatch_queue_t, void *,
/// dispatch_function_t)`.
pub unsafe extern "C" fn group_notify_f(handle: u64, q: u64, context: u64, function: u64) {
    if function == 0 {
        return;
    }
    object::retain(q);
    // SAFETY: a `dispatch_function_t`, called later with `context`.
    let function = unsafe { std::mem::transmute::<u64, Function>(function) };
    arm(
        handle,
        Notify::Function {
            queue: q,
            context,
            function,
        },
    );
}

/// `void dispatch_group_async_f(dispatch_group_t, dispatch_queue_t, void *,
/// dispatch_function_t)`.
///
/// An enter that the work's completion leaves, which is the whole of what the group
/// form adds over a plain async.
pub unsafe extern "C" fn group_async_f(handle: u64, q: u64, context: u64, function: u64) {
    // SAFETY: the entry points below take live handles.
    unsafe {
        group_enter(handle);
        object::retain(handle);
        // The group is left by the trampoline once the guest's function returns.
        let boxed = Box::into_raw(Box::new((handle, context, function))) as u64;
        queue::async_f(q, boxed, leave_after as *const () as usize as u64);
    }
}

/// `void dispatch_apply_f(size_t iterations, dispatch_queue_t, void *context,
/// void (*work)(void *, size_t))`
///
/// Every iteration, in order, on the calling thread. libdispatch may spread
/// them over a concurrent queue's threads but promises only that all have run
/// when this returns, and a guest function called on a thread this runtime
/// did not start for the guest would find none of its thread state.
pub unsafe extern "C" fn apply_f(iterations: usize, _queue: u64, context: u64, work: u64) {
    if work == 0 {
        return;
    }
    // SAFETY: the guest's `void (*)(void *, size_t)`, called with its context.
    let f = unsafe { std::mem::transmute::<u64, unsafe extern "C" fn(*mut c_void, usize)>(work) };
    for index in 0..iterations {
        unsafe { f(context as *mut c_void, index) };
    }
}

/// `void dispatch_apply(size_t iterations, dispatch_queue_t, void (^block)(size_t))`.
///
/// The block form of [`apply_f`], run the same way and for the same reason. No
/// copy: every iteration has run before this returns.
pub unsafe extern "C" fn apply(iterations: usize, _queue: u64, block: u64) {
    for index in 0..iterations {
        // SAFETY: the guest's block, whose invoke takes the iteration index.
        unsafe { super::block::invoke_with_index(block, index) };
    }
}

/// `void dispatch_group_async(dispatch_group_t, dispatch_queue_t, dispatch_block_t)`.
///
/// The block is copied, as `dispatch_async` copies one, and run through the
/// function form with a trampoline that invokes and then releases it.
pub unsafe extern "C" fn group_async(handle: u64, q: u64, block: u64) {
    let Some(copied) = super::block::copy(block) else {
        return;
    };
    // SAFETY: a live group and queue; the trampoline takes the copy.
    unsafe { group_async_f(handle, q, copied, run_block as *const () as usize as u64) };
}

/// Invoke and release a block `group_async` copied.
unsafe extern "C" fn run_block(block: *mut c_void) {
    // SAFETY: the copy `group_async` made, handed back here exactly once.
    unsafe { super::block::invoke(block as u64) };
    super::block::release(block as u64);
}

/// Runs the guest's function, then leaves the group it was entered into.
unsafe extern "C" fn leave_after(boxed: *mut c_void) {
    // SAFETY: the box `group_async_f` leaked, handed back here exactly once.
    let (handle, context, function) = unsafe { *Box::from_raw(boxed as *mut (u64, u64, u64)) };
    if function != 0 {
        // SAFETY: a `dispatch_function_t` and the context the guest gave with it.
        unsafe { std::mem::transmute::<u64, Function>(function)(context as *mut c_void) };
    }
    // SAFETY: the enter and retain taken in `group_async_f`.
    unsafe {
        group_leave(handle);
        object::release(handle);
    }
}

/// `long dispatch_group_wait(dispatch_group_t, dispatch_time_t)`.
///
/// Zero if the group emptied, non-zero if the deadline passed first.
pub unsafe extern "C" fn group_wait(handle: u64, timeout: u64) -> i64 {
    // SAFETY: the guest holds a reference for the duration of the call.
    let Some(g) = (unsafe { group(handle) }) else {
        return 0;
    };
    let mut state = g.state.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if state.count == 0 {
            return 0;
        }
        match time::remaining(timeout) {
            None => {
                state = g.empty.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            Some(0) => return 1,
            Some(left) => {
                let (next, expired) = g
                    .empty
                    .wait_timeout(state, std::time::Duration::from_nanos(left))
                    .unwrap_or_else(|e| e.into_inner());
                state = next;
                if expired.timed_out() && state.count != 0 {
                    return 1;
                }
            }
        }
    }
}

/// `dispatch_semaphore_t dispatch_semaphore_create(long value)`.
pub unsafe extern "C" fn semaphore_create(value: i64) -> u64 {
    // Darwin refuses a negative start: it would be a semaphore no signal could ever
    // lift, and the caller has miscounted rather than asked for that.
    if value < 0 {
        return 0;
    }
    Box::into_raw(Box::new(Semaphore {
        header: Header::new(Kind::Semaphore),
        state: Mutex::new(SemaphoreState { value, wakeups: 0 }),
        signalled: Condvar::new(),
    })) as u64
}

/// `long dispatch_semaphore_signal(dispatch_semaphore_t)`.
///
/// Non-zero if a thread was woken, which is what Darwin returns and what a caller
/// uses to decide whether it has just handed off work.
pub unsafe extern "C" fn semaphore_signal(handle: u64) -> i64 {
    // SAFETY: the guest holds a reference for the duration of the call.
    let Some(s) = (unsafe { semaphore(handle) }) else {
        return 0;
    };
    let mut state = s.state.lock().unwrap_or_else(|e| e.into_inner());
    let woke = state.value < 0;
    state.value += 1;
    if woke {
        state.wakeups += 1;
        drop(state);
        s.signalled.notify_one();
    }
    woke as i64
}

/// `long dispatch_semaphore_wait(dispatch_semaphore_t, dispatch_time_t)`.
///
/// Zero if the semaphore was taken, non-zero if the deadline passed first - and on
/// that path the count must be put back, or every later wait is short by one.
pub unsafe extern "C" fn semaphore_wait(handle: u64, timeout: u64) -> i64 {
    // SAFETY: the guest holds a reference for the duration of the call.
    let Some(s) = (unsafe { semaphore(handle) }) else {
        return 0;
    };
    let mut state = s.state.lock().unwrap_or_else(|e| e.into_inner());
    state.value -= 1;
    if state.value >= 0 {
        return 0;
    }
    // Outside the lock, so a signaller is not held up by the hook. This waiter
    // is already counted, and a signal meanwhile leaves a wakeup for it.
    drop(state);
    crate::idle::reached(crate::idle::Idle::Wait);
    let mut state = s.state.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if state.wakeups > 0 {
            state.wakeups -= 1;
            return 0;
        }
        match time::remaining(timeout) {
            None => {
                state = s.signalled.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            Some(0) => break,
            Some(left) => {
                let (next, expired) = s
                    .signalled
                    .wait_timeout(state, std::time::Duration::from_nanos(left))
                    .unwrap_or_else(|e| e.into_inner());
                state = next;
                if expired.timed_out() && state.wakeups == 0 {
                    break;
                }
            }
        }
    }
    // Checked under the same lock a signal takes, so no wakeup is owed to this
    // waiter: withdrawing it leaves every signal for someone still waiting.
    state.value += 1;
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Counts into whatever `AtomicU32` the caller passed as its context, so each
    /// test owns its own counter and the suite can run in parallel.
    unsafe extern "C" fn bump(context: *mut c_void) {
        // SAFETY: every caller below passes the address of a live `AtomicU32`.
        unsafe { &*(context as *const AtomicU32) }.fetch_add(1, Ordering::AcqRel);
    }

    #[test]
    fn apply_hands_every_index_to_the_block_in_order() {
        static SEEN: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());
        extern "C" fn body(_block: u64, index: usize) {
            SEEN.lock().unwrap().push(index);
        }
        #[repr(C)]
        struct Literal {
            isa: u64,
            flags: u32,
            reserved: u32,
            invoke: u64,
            descriptor: u64,
        }
        let literal = Literal {
            isa: 0,
            flags: 0,
            reserved: 0,
            invoke: body as *const () as usize as u64,
            descriptor: 0,
        };
        // SAFETY: a live literal whose invoke takes the block and an index.
        unsafe { apply(4, 0, &literal as *const Literal as u64) };
        assert_eq!(*SEEN.lock().unwrap(), [0, 1, 2, 3]);
    }

    fn wait_for(counter: &AtomicU32, what: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while counter.load(Ordering::Acquire) == 0 {
            assert!(std::time::Instant::now() < deadline, "{what}");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// A group's notification runs once the last participant leaves, and not before.
    #[test]
    fn a_group_notifies_when_it_empties() {
        let counter = AtomicU32::new(0);
        let at = &counter as *const _ as u64;
        let q = queue::create_serial("group test");
        // SAFETY: live objects, a host function, and a counter that outlives them.
        unsafe {
            let g = group_create();
            group_enter(g);
            group_enter(g);
            group_notify_f(g, q, at, bump as *const () as usize as u64);
            group_leave(g);
            std::thread::sleep(std::time::Duration::from_millis(20));
            assert_eq!(
                counter.load(Ordering::Acquire),
                0,
                "one participant remains"
            );
            group_leave(g);
            wait_for(&counter, "never notified");
            object::release(g);
            object::release(q);
        }
    }

    /// A notification armed on an already-empty group still has to run.
    #[test]
    fn an_empty_group_notifies_at_once() {
        let counter = AtomicU32::new(0);
        let q = queue::create_serial("empty group test");
        // SAFETY: live objects, a host function, and a counter that outlives them.
        unsafe {
            let g = group_create();
            group_notify_f(
                g,
                q,
                &counter as *const _ as u64,
                bump as *const () as usize as u64,
            );
            wait_for(&counter, "never notified");
            object::release(g);
            object::release(q);
        }
    }

    #[test]
    fn a_group_wait_returns_when_it_empties_and_times_out_otherwise() {
        // SAFETY: live objects.
        unsafe {
            let g = group_create();
            group_enter(g);
            let soon = time::time(time::NOW, 20_000_000);
            assert_eq!(group_wait(g, soon), 1, "still occupied, so it times out");
            group_leave(g);
            assert_eq!(group_wait(g, time::FOREVER), 0);
            object::release(g);
        }
    }

    /// The counting property: a semaphore of one is a mutex, and the second waiter
    /// only proceeds once the first signals.
    #[test]
    fn a_semaphore_counts() {
        // SAFETY: live objects.
        unsafe {
            let s = semaphore_create(1);
            assert_eq!(semaphore_wait(s, time::FOREVER), 0, "the first is free");
            let soon = time::time(time::NOW, 20_000_000);
            assert_eq!(semaphore_wait(s, soon), 1, "the second must wait");
            semaphore_signal(s);
            assert_eq!(semaphore_wait(s, time::FOREVER), 0, "and then it is free");
            object::release(s);
        }
    }

    /// A timed-out wait must put its count back, or the semaphore leaks a permit and
    /// every later waiter is short by one.
    #[test]
    fn a_timed_out_wait_does_not_consume_a_permit() {
        // SAFETY: live objects.
        unsafe {
            let s = semaphore_create(0);
            let soon = time::time(time::NOW, 10_000_000);
            assert_eq!(semaphore_wait(s, soon), 1);
            semaphore_signal(s);
            assert_eq!(
                semaphore_wait(s, time::FOREVER),
                0,
                "the signal after a timeout must still be takeable"
            );
            object::release(s);
        }
    }

    /// A waiter parked with no deadline wakes when another thread signals.
    #[test]
    fn a_signal_wakes_a_blocked_waiter() {
        // SAFETY: live objects, and the handle outlives both threads.
        unsafe {
            let s = semaphore_create(0);
            let signaller = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(30));
                semaphore_signal(s);
            });
            assert_eq!(semaphore_wait(s, time::FOREVER), 0);
            signaller.join().expect("the signaller finishes");
            object::release(s);
        }
    }

    /// With two waiters parked, each signal must release one of them. Life Is
    /// Strange's engine mutex hung here when a woken waiter saw the value still
    /// negative and went back to sleep.
    #[test]
    fn each_signal_releases_one_of_several_waiters() {
        // SAFETY: live objects, and the handle outlives every thread.
        unsafe {
            let s = semaphore_create(0);
            let (done, finished) = std::sync::mpsc::channel();
            let waiters: Vec<_> = (0..2)
                .map(|_| {
                    let done = done.clone();
                    std::thread::spawn(move || {
                        let deadline = time::time(time::NOW, 5_000_000_000);
                        done.send(semaphore_wait(s, deadline))
                            .expect("the test listens");
                    })
                })
                .collect();
            let patience = std::time::Duration::from_secs(2);
            std::thread::sleep(std::time::Duration::from_millis(50));
            for _ in 0..2 {
                assert_eq!(semaphore_signal(s), 1, "a waiter is parked");
                // One signal at a time, so the woken waiter looks before the next.
                assert_eq!(finished.recv_timeout(patience), Ok(0), "and it wakes");
            }
            for waiter in waiters {
                waiter.join().expect("the waiter finishes");
            }
            object::release(s);
        }
    }

    /// Producers signal and consumers wait, some with short deadlines that
    /// expire and put their count back: every signal is taken exactly once.
    #[test]
    fn signals_and_waits_balance_with_timeouts_in_the_mix() {
        const PAIRS: u64 = 4;
        const EACH: u64 = 5_000;
        let s = unsafe { semaphore_create(0) };
        let taken = std::sync::Arc::new(AtomicU32::new(0));
        let mut threads = Vec::new();
        for which in 0..PAIRS {
            threads.push(std::thread::spawn(move || {
                for _ in 0..EACH {
                    unsafe { semaphore_signal(s) };
                }
            }));
            let taken = std::sync::Arc::clone(&taken);
            threads.push(std::thread::spawn(move || {
                let mut got = 0;
                while got < EACH {
                    let deadline = match which % 2 {
                        0 => time::FOREVER,
                        _ => unsafe { time::time(time::NOW, 20_000) },
                    };
                    if unsafe { semaphore_wait(s, deadline) } == 0 {
                        got += 1;
                        taken.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }));
        }
        for t in threads {
            t.join().expect("a producer or consumer");
        }
        assert_eq!(taken.load(Ordering::Relaxed) as u64, PAIRS * EACH);
        unsafe {
            assert_eq!(semaphore_wait(s, time::NOW), 1, "nothing left over");
            object::release(s);
        }
    }
}
