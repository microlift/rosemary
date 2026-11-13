//! `thread_suspend`, `thread_resume`, `thread_get_state`, `thread_set_state`
//! and `thread_abort` for the threads of this process.
//!
//! No host besides Darwin has a call that stops another thread and hands over
//! its registers, so a suspension is a private signal: the target's handler
//! saves the context it interrupted, says so, and waits to be let go - writing
//! back whatever `thread_set_state` changed in the meantime. The callers this
//! exists for are garbage collectors stopping the world (Boehm's Darwin port,
//! which Mono uses), which then read every thread's stack pointer. Nothing on
//! the target's side takes a lock, since it may have been stopped holding any.
//!
//! The registers are the ones the thread was interrupted with. Under the
//! `native` backend that is guest code or a host function it called, on the
//! same stack either way, which is all a collector scanning that stack needs.
//!
//! `task_suspend` parks every thread the same way ([`suspend_task`]). It may be
//! asked for from another process, through the process server, in which case
//! it runs in the service signal's handler ([`super::service`]); so does
//! everything it touches here, which is why a slot's lock is a spin lock that
//! blocks this module's signals rather than a mutex. A parked thread leaves the
//! service signal open, so a suspended task still answers for its memory.

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

use super::kern::*;
use super::space::{self, Name};

/// Threads this process may have suspended at some point, which is a bound
/// on its threads alive at once rather than over its lifetime: a slot is
/// reused once its thread is gone.
const SLOTS: usize = 4096;

const RUNNING: u32 = 0;
/// The signal is on its way and the handler has not yet saved the context.
const REQUESTED: u32 = 1;
const PARKED: u32 = 2;
/// Let go, and the handler has not yet returned.
const RELEASED: u32 = 3;

struct Slot {
    /// Zero when free.
    tid: AtomicI32,
    /// Serializes the threads controlling this one; the target never takes it.
    control: Spin,
    /// Mach's suspend count. Only the 0 -> 1 and 1 -> 0 edges signal.
    count: AtomicU32,
    /// Held by a task suspension, which parks the thread whatever `count` is.
    task_held: AtomicBool,
    /// Signalled by a task suspension that has yet to see it parked.
    awaited: AtomicBool,
    phase: AtomicU32,
    /// Whether `context` was changed while parked and has to be restored.
    dirty: AtomicBool,
    /// A `thread_abort` that arrived while suspended, sent on the last resume.
    abort: AtomicBool,
    parked: Semaphore,
    released: Semaphore,
    /// Written by the target before `PARKED`, read and changed by its
    /// controller under `control` until `RELEASED`.
    context: UnsafeCell<Option<starnix::signal::Context>>,
}

// SAFETY: `context` is handed between the target and its controller by `phase`,
// as described on the field.
unsafe impl Sync for Slot {}

#[cfg(not(target_os = "macos"))]
/// An unnamed POSIX semaphore: the one wait whose post is async-signal-safe.
struct Semaphore(UnsafeCell<libc::sem_t>);

#[cfg(not(target_os = "macos"))]
impl Semaphore {
    fn new() -> Semaphore {
        // SAFETY: all zeroes is a valid `sem_t` to hand to `sem_init`.
        let sem = Semaphore(UnsafeCell::new(unsafe { std::mem::zeroed() }));
        // SAFETY: a process-private semaphore at an address that never moves,
        // since every `Slot` lives in one leaked allocation.
        unsafe { libc::sem_init(sem.0.get(), 0, 0) };
        sem
    }

    fn post(&self) {
        // SAFETY: initialized by `new`.
        unsafe { libc::sem_post(self.0.get()) };
    }

    /// Wait for a post; `false` if `timeout` passed first.
    fn wait(&self, timeout: Option<std::time::Duration>) -> bool {
        loop {
            // SAFETY: initialized by `new`; `deadline` is a valid timespec.
            let rc = unsafe {
                match timeout {
                    None => libc::sem_wait(self.0.get()),
                    Some(timeout) => {
                        let deadline = deadline(timeout);
                        libc::sem_timedwait(self.0.get(), &deadline)
                    }
                }
            };
            if rc == 0 {
                return true;
            }
            if starnix::errno::get() != libc::EINTR {
                return false;
            }
        }
    }

    fn try_wait(&self) -> bool {
        // SAFETY: initialized by `new`.
        unsafe { libc::sem_trywait(self.0.get()) == 0 }
    }
}

/// macOS rejects unnamed POSIX semaphores, so use a dispatch semaphore, whose
/// signal is likewise async-signal-safe.
#[cfg(target_os = "macos")]
struct Semaphore(*mut core::ffi::c_void);

// SAFETY: dispatch semaphores are thread-safe; the handle is never released.
#[cfg(target_os = "macos")]
unsafe impl Send for Semaphore {}
#[cfg(target_os = "macos")]
unsafe impl Sync for Semaphore {}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn dispatch_semaphore_create(value: isize) -> *mut core::ffi::c_void;
    fn dispatch_semaphore_signal(sem: *mut core::ffi::c_void) -> isize;
    fn dispatch_semaphore_wait(sem: *mut core::ffi::c_void, timeout: u64) -> isize;
    fn dispatch_time(when: u64, delta: i64) -> u64;
}

#[cfg(target_os = "macos")]
const DISPATCH_TIME_NOW: u64 = 0;
#[cfg(target_os = "macos")]
const DISPATCH_TIME_FOREVER: u64 = !0;

#[cfg(target_os = "macos")]
impl Semaphore {
    fn new() -> Semaphore {
        Semaphore(unsafe { dispatch_semaphore_create(0) })
    }

    fn post(&self) {
        unsafe { dispatch_semaphore_signal(self.0) };
    }

    /// Wait for a post; `false` if `timeout` passed first.
    fn wait(&self, timeout: Option<std::time::Duration>) -> bool {
        let when = match timeout {
            None => DISPATCH_TIME_FOREVER,
            Some(t) => unsafe {
                dispatch_time(DISPATCH_TIME_NOW, t.as_nanos().min(i64::MAX as u128) as i64)
            },
        };
        unsafe { dispatch_semaphore_wait(self.0, when) == 0 }
    }

    fn try_wait(&self) -> bool {
        unsafe { dispatch_semaphore_wait(self.0, DISPATCH_TIME_NOW) == 0 }
    }
}

#[cfg(not(target_os = "macos"))]
/// `CLOCK_REALTIME` plus `timeout`, which is the clock `sem_timedwait` reads.
fn deadline(timeout: std::time::Duration) -> libc::timespec {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a writable timespec.
    unsafe { crate::syscall::clock_gettime(libc::CLOCK_REALTIME, &mut now) };
    let nanos = now.tv_nsec as u64 + u64::from(timeout.subsec_nanos());
    libc::timespec {
        tv_sec: now.tv_sec
            + timeout.as_secs() as libc::time_t
            + (nanos / 1_000_000_000) as libc::time_t,
        tv_nsec: (nanos % 1_000_000_000) as _,
    }
}

fn slots() -> &'static [Slot] {
    static SLOTS_TABLE: OnceLock<&'static [Slot]> = OnceLock::new();
    SLOTS_TABLE.get_or_init(|| {
        let table: Vec<Slot> = (0..SLOTS)
            .map(|_| Slot {
                tid: AtomicI32::new(0),
                control: Spin::new(),
                count: AtomicU32::new(0),
                task_held: AtomicBool::new(false),
                awaited: AtomicBool::new(false),
                phase: AtomicU32::new(RUNNING),
                dirty: AtomicBool::new(false),
                abort: AtomicBool::new(false),
                parked: Semaphore::new(),
                released: Semaphore::new(),
                context: UnsafeCell::new(None),
            })
            .collect();
        Box::leak(table.into_boxed_slice())
    })
}

/// `tid`'s slot, without claiming one. Lock-free, for the signal handler.
fn find(tid: i32) -> Option<&'static Slot> {
    slots()
        .iter()
        .find(|slot| slot.tid.load(Ordering::Acquire) == tid)
}

/// Whether thread `tid` of this process still exists. A syscall and nothing
/// else, so the signal handler may ask.
fn alive(tid: i32) -> bool {
    starnix::signal::send_to_thread(tid, 0).is_ok()
}

/// `tid`'s slot, claiming a free one - or one whose thread has ended - if it
/// has none yet. Lock-free, for the signal handler.
fn claim(tid: i32) -> Option<&'static Slot> {
    if let Some(slot) = find(tid) {
        return Some(slot);
    }
    for slot in slots() {
        let held = slot.tid.load(Ordering::Acquire);
        let free = held == 0
            || (slot.count.load(Ordering::Acquire) == 0
                && !slot.task_held.load(Ordering::Acquire)
                && slot.phase.load(Ordering::Acquire) == RUNNING
                && !alive(held));
        if free
            && slot
                .tid
                .compare_exchange(held, tid, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            // Two controllers claiming for one thread at once may each have
            // taken a slot: the first one in the table is the thread's.
            let first = find(tid).unwrap_or(slot);
            if !std::ptr::eq(first, slot) {
                slot.tid.store(0, Ordering::Release);
            }
            return Some(first);
        }
    }
    // The table is full of live threads.
    find(tid)
}

/// Give this thread its slot now, so a task suspension from the signal
/// handler finds it rather than claims it.
pub fn prepare_thread() {
    claim(crate::pthread::id() as i32);
}

/// A lock a signal handler may take: a spin on an atomic, held with this
/// module's signals blocked on the holding thread, so the holder is never
/// parked, nor sent into a handler that wants the same lock.
struct Spin(AtomicBool);

struct SpinGuard<'a> {
    lock: &'a Spin,
    mask: libc::sigset_t,
}

impl Spin {
    const fn new() -> Spin {
        Spin(AtomicBool::new(false))
    }

    /// Only the holder has the signals blocked: a thread still waiting for
    /// the lock can be parked, and a task suspension that holds it does not
    /// wait forever for a thread that waits for it.
    fn lock(&self) -> SpinGuard<'_> {
        loop {
            let mask = block_private_signals();
            if self
                .0
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return SpinGuard { lock: self, mask };
            }
            // SAFETY: the mask saved just above, then no preconditions.
            unsafe {
                libc::pthread_sigmask(libc::SIG_SETMASK, &mask, std::ptr::null_mut());
                libc::sched_yield();
            }
        }
    }
}

impl Drop for SpinGuard<'_> {
    fn drop(&mut self) {
        self.lock.0.store(false, Ordering::Release);
        // SAFETY: the mask `lock` saved on this thread.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &self.mask, std::ptr::null_mut()) };
    }
}

/// Block the park signal and the service signal on this thread, returning the
/// mask to restore. Async-signal-safe.
fn block_private_signals() -> libc::sigset_t {
    // SAFETY: two local sets, initialised before use.
    unsafe {
        let mut block: libc::sigset_t = std::mem::zeroed();
        let mut old: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut block);
        for signal in [suspend_signal(), server::task::signal()]
            .into_iter()
            .flatten()
        {
            libc::sigaddset(&mut block, signal);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &block, &mut old);
        old
    }
}

/// The task's own suspend count, which Darwin keeps in the kernel's task and
/// this process keeps for itself: whoever suspends it, the threads that stop
/// are this process's.
static TASK_COUNT: AtomicU32 = AtomicU32::new(0);
static TASK_LOCK: Spin = Spin::new();

/// How many times a suspension lists the threads again for ones started while
/// it was parking the others.
const TASK_ROUNDS: usize = 8;

/// Set everything up a task suspension needs, so that one asked for from the
/// signal handler allocates nothing. False where the host has no signal for it.
pub fn prepare() -> bool {
    install().is_some()
}

/// `task_suspend` on this task: every thread but the caller's parks before this
/// returns, and the caller is expected to [`park_self`] next. Only the 0 -> 1
/// edge parks anything. Async-signal-safe once [`prepare`] has run.
///
/// FIXME (ACCURACY): Darwin holds the whole task at once. Here the threads are
/// listed and parked one by one, and listed again until no new one appears, so
/// a thread started in the last moment can still slip past.
pub fn suspend_task() -> i32 {
    let (Some(()), Some(signal)) = (install(), suspend_signal()) else {
        return KERN_FAILURE;
    };
    let _task = TASK_LOCK.lock();
    let count = TASK_COUNT.load(Ordering::Acquire);
    TASK_COUNT.store(count + 1, Ordering::Release);
    if count > 0 {
        return KERN_SUCCESS;
    }
    let me = crate::pthread::id() as i32;
    for _ in 0..TASK_ROUNDS {
        let mut fresh = false;
        let listed = starnix::signal::for_each_thread(&mut |tid| {
            if tid != me && hold(tid, signal) {
                fresh = true;
            }
        });
        await_held();
        if !listed || !fresh {
            break;
        }
    }
    KERN_SUCCESS
}

/// Hold `tid` for the task: true if it was not held for it before.
fn hold(tid: i32, signal: i32) -> bool {
    let Some(slot) = claim(tid) else {
        return false;
    };
    let _guard = slot.control.lock();
    if slot.task_held.swap(true, Ordering::AcqRel) {
        return false;
    }
    if slot.count.load(Ordering::Acquire) > 0 {
        // Parked already, by `thread_suspend`.
        return true;
    }
    // A thread resumed a moment ago may still be on its way out of the handler.
    while slot.phase.load(Ordering::Acquire) != RUNNING {
        // SAFETY: no preconditions.
        unsafe { libc::sched_yield() };
    }
    slot.phase.store(REQUESTED, Ordering::Release);
    slot.awaited.store(true, Ordering::Release);
    if starnix::signal::send_to_thread(tid, signal).is_err() {
        slot.awaited.store(false, Ordering::Release);
        slot.task_held.store(false, Ordering::Release);
        slot.phase.store(RUNNING, Ordering::Release);
    }
    true
}

/// Wait until every thread [`hold`] signalled has parked, or ended.
fn await_held() {
    for slot in slots() {
        if !slot.awaited.swap(false, Ordering::AcqRel) {
            continue;
        }
        let tid = slot.tid.load(Ordering::Acquire);
        while !slot.parked.wait(Some(PARK_POLL)) {
            if !alive(tid) {
                let _guard = slot.control.lock();
                slot.task_held.store(false, Ordering::Release);
                slot.phase.store(RUNNING, Ordering::Release);
                break;
            }
        }
    }
}

/// Park the calling thread for its task's suspension, until [`resume_task`]
/// lets it go - from another thread, or from another process through the
/// service handler of one of the threads parked already.
///
/// # Safety
///
/// `raw` is null, or the context of a signal handler running on this thread,
/// which `thread_get_state` reads while it is parked.
pub unsafe fn park_self(raw: *mut c_void) {
    let Some(slot) = claim(crate::pthread::id() as i32) else {
        return;
    };
    {
        let _task = TASK_LOCK.lock();
        if TASK_COUNT.load(Ordering::Acquire) == 0 {
            return;
        }
        let _guard = slot.control.lock();
        // Parked already, further up this thread's stack: a second suspension
        // served from inside the first one only counts.
        if slot.task_held.swap(true, Ordering::AcqRel) {
            return;
        }
        slot.phase.store(REQUESTED, Ordering::Release);
    }
    // SAFETY: on the slot's own thread, with the caller's context.
    unsafe { wait_parked(slot, raw) };
    // Nobody waited for that post: this thread parked itself.
    slot.parked.try_wait();
}

/// `task_resume` on this task. The last outstanding suspension releases every
/// thread it held that is not suspended on its own account; one with none
/// outstanding is refused, as Darwin refuses it. Async-signal-safe.
pub fn resume_task() -> i32 {
    let _task = TASK_LOCK.lock();
    let count = TASK_COUNT.load(Ordering::Acquire);
    if count == 0 {
        return KERN_FAILURE;
    }
    TASK_COUNT.store(count - 1, Ordering::Release);
    if count > 1 {
        return KERN_SUCCESS;
    }
    for slot in slots() {
        if !slot.task_held.load(Ordering::Acquire) {
            continue;
        }
        let _guard = slot.control.lock();
        if slot.task_held.swap(false, Ordering::AcqRel) && slot.count.load(Ordering::Acquire) == 0 {
            release(slot, slot.tid.load(Ordering::Acquire));
        }
    }
    KERN_SUCCESS
}

/// Whether this task is suspended now.
pub fn task_suspended() -> bool {
    TASK_COUNT.load(Ordering::Acquire) > 0
}

/// Whether `slot`'s thread is held: by its own suspensions, or its task's.
fn held(slot: &Slot) -> bool {
    slot.count.load(Ordering::Acquire) > 0 || slot.task_held.load(Ordering::Acquire)
}

/// The signal that parks a thread.
fn suspend_signal() -> Option<i32> {
    starnix::signal::private_signal(0)
}

/// The signal `thread_abort` interrupts a blocking call with.
fn abort_signal() -> Option<i32> {
    starnix::signal::private_signal(1)
}

/// Install both handlers, once. `None` where the host has no signal to spare.
fn install() -> Option<()> {
    static INSTALLED: OnceLock<bool> = OnceLock::new();
    let installed = *INSTALLED.get_or_init(|| {
        let (Some(suspend), Some(abort)) = (suspend_signal(), abort_signal()) else {
            return false;
        };
        // Build the table now: the handler must never be the one to allocate it.
        slots();
        // SAFETY: handlers of this module's own on signals no guest can name.
        unsafe {
            // Restarting, so that a call the thread was blocked in carries on
            // after a resume as it does after a Mach one; and everything
            // blocked, so nothing runs on the thread while it is parked.
            // The server's work is the exception: a suspended task still
            // answers for its memory. Do not use SA_ONSTACK: a full saved
            // register/FPU context can overflow 8 KiB alternate stacks used
            // by host runtimes. Suspension is not a stack-overflow handler.
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = park as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
            libc::sigfillset(&mut action.sa_mask);
            if let Some(service) = server::task::signal() {
                libc::sigdelset(&mut action.sa_mask, service);
            }
            libc::sigaction(suspend, &action, std::ptr::null_mut());
            // Not restarting: interrupting the call is the whole point.
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = interrupt as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO;
            libc::sigaction(abort, &action, std::ptr::null_mut());
        }
        true
    });
    installed.then_some(())
}

/// The suspend signal's handler, on the thread being suspended.
extern "C" fn park(_signo: i32, _info: *mut libc::siginfo_t, raw: *mut c_void) {
    let errno = starnix::errno::get();
    let tid = crate::pthread::id() as i32;
    // A signal from a suspension that gave up on this thread finds no request.
    if let Some(slot) = find(tid)
        && slot.phase.load(Ordering::Acquire) == REQUESTED
    {
        // SAFETY: the kernel's context for this handler, on this thread.
        unsafe { wait_parked(slot, raw) };
    }
    starnix::errno::set(errno);
}

/// Publish `raw` as `slot`'s thread's context, wait to be released, and write
/// back whatever `thread_set_state` changed meanwhile.
///
/// # Safety
///
/// Called on `slot`'s own thread. `raw` is null, or the context of a signal
/// handler on this thread that has not returned.
unsafe fn wait_parked(slot: &Slot, raw: *mut c_void) {
    // SAFETY: `context` is this thread's to write until `PARKED` is published.
    unsafe { *slot.context.get() = starnix::signal::context(raw) };
    slot.phase.store(PARKED, Ordering::Release);
    slot.parked.post();
    slot.released.wait(None);
    if slot.dirty.swap(false, Ordering::AcqRel) {
        // SAFETY: the controller is done with `context` once it released this
        // thread, and `raw` is the caller's.
        if let Some(context) = unsafe { &*slot.context.get() } {
            unsafe { starnix::signal::set_context(raw, context) };
        }
    }
    slot.phase.store(RUNNING, Ordering::Release);
}

/// The abort signal's handler: being delivered is all it is for.
extern "C" fn interrupt(_signo: i32, _info: *mut libc::siginfo_t, _raw: *mut c_void) {}

/// The thread `thread` names, if it is one of this process's.
fn local_thread(thread: Name) -> Option<i32> {
    let (pid, tid) = space::thread_of(thread)?;
    (pid == crate::process::host_self()).then_some(tid)
}

/// How long to wait for a thread to park before checking it still exists.
const PARK_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// `kern_return_t thread_suspend(thread_act_t)`
pub extern "C" fn thread_suspend(thread: Name) -> i32 {
    let Some(tid) = local_thread(thread) else {
        return KERN_INVALID_ARGUMENT;
    };
    let (Some(()), Some(signal)) = (install(), suspend_signal()) else {
        return KERN_FAILURE;
    };
    let Some(slot) = claim(tid) else {
        return KERN_RESOURCE_SHORTAGE;
    };
    let guard = slot.control.lock();
    let count = slot.count.load(Ordering::Acquire);
    // Already parked, by an earlier suspension or by its task's: this one
    // only counts.
    if count > 0 || slot.task_held.load(Ordering::Acquire) {
        slot.count.store(count + 1, Ordering::Release);
        return KERN_SUCCESS;
    }
    // A thread resumed a moment ago may still be on its way out of the handler.
    while slot.phase.load(Ordering::Acquire) != RUNNING {
        std::thread::yield_now();
    }
    slot.count.store(1, Ordering::Release);
    slot.phase.store(REQUESTED, Ordering::Release);
    if tid == crate::pthread::id() as i32 {
        // The handler runs before the signal call returns, and parks until
        // another thread resumes this one - which needs `control`.
        drop(guard);
        if starnix::signal::send_to_thread(tid, signal).is_err() {
            slot.count.store(0, Ordering::Release);
            slot.phase.store(RUNNING, Ordering::Release);
            return KERN_FAILURE;
        }
        slot.parked.try_wait();
        return KERN_SUCCESS;
    }
    if starnix::signal::send_to_thread(tid, signal).is_err() {
        slot.count.store(0, Ordering::Release);
        slot.phase.store(RUNNING, Ordering::Release);
        return KERN_TERMINATED;
    }
    // Mach's suspension is done when the call returns, and a collector reads
    // the registers straight after: wait until they are saved.
    while !slot.parked.wait(Some(PARK_POLL)) {
        if !alive(tid) {
            slot.count.store(0, Ordering::Release);
            slot.phase.store(RUNNING, Ordering::Release);
            return KERN_TERMINATED;
        }
    }
    KERN_SUCCESS
}

/// `kern_return_t thread_resume(thread_act_t)`
pub extern "C" fn thread_resume(thread: Name) -> i32 {
    let Some(tid) = local_thread(thread) else {
        return KERN_INVALID_ARGUMENT;
    };
    let Some(slot) = find(tid) else {
        return KERN_FAILURE;
    };
    let _guard = slot.control.lock();
    let count = slot.count.load(Ordering::Acquire);
    if count == 0 {
        return KERN_FAILURE;
    }
    slot.count.store(count - 1, Ordering::Release);
    // Its task's suspension still holds it.
    if count == 1 && !slot.task_held.load(Ordering::Acquire) {
        release(slot, tid);
    }
    KERN_SUCCESS
}

/// Let `slot`'s parked thread go. Called with its `control` held.
fn release(slot: &Slot, tid: i32) {
    slot.phase.store(RELEASED, Ordering::Release);
    slot.released.post();
    if slot.abort.swap(false, Ordering::AcqRel) {
        send_abort(tid);
    }
}

fn send_abort(tid: i32) {
    if let Some(signal) = abort_signal() {
        let _ = starnix::signal::send_to_thread(tid, signal);
    }
}

/// `kern_return_t thread_abort(thread_act_t)` and `thread_abort_safely`.
///
/// Darwin's makes the call the thread is blocked in return early. Here that
/// is a signal without `SA_RESTART`, which does the same to a host call; a
/// suspended thread gets it when it is resumed, which is when Darwin's would
/// see its call fail.
pub extern "C" fn thread_abort(thread: Name) -> i32 {
    let Some(tid) = local_thread(thread) else {
        return KERN_INVALID_ARGUMENT;
    };
    if install().is_none() {
        return KERN_FAILURE;
    }
    if let Some(slot) = find(tid) {
        let _guard = slot.control.lock();
        if held(slot) {
            slot.abort.store(true, Ordering::Release);
            return KERN_SUCCESS;
        }
    }
    send_abort(tid);
    KERN_SUCCESS
}

/// `kern_return_t thread_policy_set(thread_act_t, thread_policy_flavor_t,
/// thread_policy_t, mach_msg_type_number_t)`
///
/// Every flavour is a scheduling hint - affinity tags, precedence, time
/// constraints - and a scheduler that ignores one is within its rights, so
/// each is accepted and dropped.
pub extern "C" fn thread_policy_set(
    thread: Name,
    _flavor: u32,
    _policy: *const u32,
    _count: u32,
) -> i32 {
    match space::thread_of(thread) {
        Some(_) => KERN_SUCCESS,
        None => KERN_INVALID_ARGUMENT,
    }
}

const THREAD_EXTENDED_POLICY: u32 = 1;
const THREAD_TIME_CONSTRAINT_POLICY: u32 = 2;
const THREAD_PRECEDENCE_POLICY: u32 = 3;
const THREAD_AFFINITY_POLICY: u32 = 4;
const THREAD_LATENCY_QOS_POLICY: u32 = 7;
const THREAD_THROUGHPUT_QOS_POLICY: u32 = 8;

/// xnu's `std_quantum` in `mach_absolute_time` ticks, which are nanoseconds
/// here; a timeshare thread's constraint is one quantum and its computation
/// half of one.
const STD_QUANTUM: u32 = 10_000_000;

/// `kern_return_t thread_policy_get(thread_act_t, thread_policy_flavor_t,
/// thread_policy_t, mach_msg_type_number_t *, boolean_t *)`
///
/// [`thread_policy_set`] keeps nothing, so every thread reads back as a fresh
/// timeshare thread would. `get_default` is set where xnu sets it for such a
/// thread: the time-constraint and affinity flavours.
///
/// # Safety
///
/// `count` and `get_default` are writable, and `info` is writable for
/// `*count` 32-bit words.
pub unsafe extern "C" fn thread_policy_get(
    thread: Name,
    flavor: u32,
    info: *mut u32,
    count: *mut u32,
    get_default: *mut u32,
) -> i32 {
    if space::thread_of(thread).is_none() {
        return KERN_INVALID_ARGUMENT;
    }
    if info.is_null() || count.is_null() || get_default.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    let (words, defaulted): (&[u32], bool) = match flavor {
        // `timeshare`.
        THREAD_EXTENDED_POLICY => (&[1], false),
        // `period`, `computation`, `constraint`, `preemptible`.
        THREAD_TIME_CONSTRAINT_POLICY => (&[0, STD_QUANTUM / 2, STD_QUANTUM, 1], true),
        // `importance`.
        THREAD_PRECEDENCE_POLICY => (&[0], false),
        // `THREAD_AFFINITY_TAG_NULL`.
        THREAD_AFFINITY_POLICY => (&[0], true),
        // `LATENCY_QOS_TIER_UNSPECIFIED` and its throughput counterpart.
        THREAD_LATENCY_QOS_POLICY | THREAD_THROUGHPUT_QOS_POLICY => (&[0], false),
        _ => return KERN_INVALID_ARGUMENT,
    };
    // SAFETY: the caller's out-parameters.
    unsafe {
        if (*count as usize) < words.len() {
            // xnu answers a short extended-policy buffer by writing nothing.
            return match flavor {
                THREAD_EXTENDED_POLICY => KERN_SUCCESS,
                _ => KERN_INVALID_ARGUMENT,
            };
        }
        std::ptr::copy_nonoverlapping(words.as_ptr(), info, words.len());
        if defaulted {
            get_default.write(1);
        }
    }
    KERN_SUCCESS
}

/// Run `f` on the saved context of `thread`, which must be suspended.
fn with_context(thread: Name, f: impl FnOnce(&mut starnix::signal::Context, &Slot) -> i32) -> i32 {
    let Some(tid) = local_thread(thread) else {
        return KERN_INVALID_ARGUMENT;
    };
    let Some(slot) = find(tid) else {
        return KERN_FAILURE;
    };
    let _guard = slot.control.lock();
    // A running thread has no saved context; `thread_get_state` suspends one
    // briefly rather than come here without it.
    if !held(slot) || slot.phase.load(Ordering::Acquire) != PARKED {
        return KERN_FAILURE;
    }
    // SAFETY: parked, and `control` is held.
    match unsafe { &mut *slot.context.get() } {
        Some(context) => f(context, slot),
        None => KERN_FAILURE,
    }
}

/// `kern_return_t thread_get_state(thread_act_t, thread_state_flavor_t,
/// thread_state_t, mach_msg_type_number_t *)`
///
/// # Safety
///
/// `out` is writable for `*count` 32-bit words, and `count` is writable.
pub unsafe extern "C" fn thread_get_state(
    thread: Name,
    flavor: i32,
    out: *mut u32,
    count: *mut u32,
) -> i32 {
    if out.is_null() || count.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    // Darwin answers for a running thread too, with a snapshot that is stale
    // as soon as it is read - and Boehm's collector asks for one after it has
    // already resumed the world. A snapshot is what a brief suspension gives.
    let calling = local_thread(thread) == Some(crate::pthread::id() as i32);
    let snapshot = !calling && !suspended(thread) && thread_suspend(thread) == KERN_SUCCESS;
    let rc = with_context(thread, |context, _| {
        let Some(words) = state::get(&context.machine, flavor) else {
            return KERN_INVALID_ARGUMENT;
        };
        // SAFETY: the caller's obligation.
        unsafe {
            if (*count as usize) < words.len() {
                return KERN_INVALID_ARGUMENT;
            }
            std::ptr::copy_nonoverlapping(words.as_ptr(), out, words.len());
            *count = words.len() as u32;
        }
        KERN_SUCCESS
    });
    if snapshot {
        thread_resume(thread);
    }
    rc
}

/// Whether `thread` is one of this process's and suspended now.
fn suspended(thread: Name) -> bool {
    local_thread(thread).and_then(find).is_some_and(held)
}

/// `kern_return_t thread_set_state(thread_act_t, thread_state_flavor_t,
/// thread_state_t, mach_msg_type_number_t)`
///
/// # Safety
///
/// `state` is readable for `count` 32-bit words.
pub unsafe extern "C" fn thread_set_state(
    thread: Name,
    flavor: i32,
    words: *const u32,
    count: u32,
) -> i32 {
    if words.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller's obligation.
    let words = unsafe { std::slice::from_raw_parts(words, count as usize) };
    with_context(thread, |context, slot| {
        if !state::set(&mut context.machine, flavor, words) {
            return KERN_INVALID_ARGUMENT;
        }
        slot.dirty.store(true, Ordering::Release);
        KERN_SUCCESS
    })
}

/// Darwin's x86-64 thread-state flavours, from `<mach/i386/thread_status.h>`.
#[cfg(target_arch = "x86_64")]
mod state {
    use starnix::signal::Machine;

    const X86_THREAD_STATE64: i32 = 4;
    const X86_FLOAT_STATE64: i32 = 5;
    const X86_THREAD_STATE: i32 = 7;
    const X86_FLOAT_STATE: i32 = 8;

    /// `x86_thread_state64_t`: 21 registers of 64 bits.
    const THREAD_WORDS: usize = 42;
    /// `x86_float_state64_t`: two reserved words, the `FXSAVE` image, one more.
    const FLOAT_WORDS: usize = 131;

    /// Darwin's register order, as indices into [`Machine::gpr`]'s.
    const ORDER: [usize; 16] = [0, 3, 1, 2, 7, 6, 5, 4, 8, 9, 10, 11, 12, 13, 14, 15];

    /// The image `FNINIT` and a fresh `MXCSR` leave, for a context the host
    /// saved none for.
    fn initial_fxsave() -> [u8; 512] {
        let mut image = [0u8; 512];
        image[0..2].copy_from_slice(&0x037fu16.to_le_bytes());
        image[24..28].copy_from_slice(&0x1f80u32.to_le_bytes());
        image
    }

    fn words_of(bytes: &[u8]) -> Vec<u32> {
        bytes
            .chunks(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    fn bytes_of(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn thread(machine: &Machine) -> Vec<u32> {
        let mut bytes = Vec::with_capacity(THREAD_WORDS * 4);
        for index in ORDER {
            bytes.extend_from_slice(&machine.gpr[index].to_le_bytes());
        }
        for value in [
            machine.rip,
            machine.rflags,
            u64::from(machine.cs),
            u64::from(machine.fs),
            u64::from(machine.gs),
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        words_of(&bytes)
    }

    fn float(machine: &Machine) -> Vec<u32> {
        let mut bytes = vec![0u8; 8];
        bytes.extend_from_slice(&machine.fxsave.unwrap_or_else(initial_fxsave));
        bytes.extend_from_slice(&[0; 4]);
        words_of(&bytes)
    }

    /// The flavour's words, header included for the two that carry one.
    pub fn get(machine: &Machine, flavor: i32) -> Option<Vec<u32>> {
        Some(match flavor {
            X86_THREAD_STATE64 => thread(machine),
            X86_FLOAT_STATE64 => float(machine),
            X86_THREAD_STATE => {
                let mut words = vec![X86_THREAD_STATE64 as u32, THREAD_WORDS as u32];
                words.extend(thread(machine));
                words
            }
            X86_FLOAT_STATE => {
                let mut words = vec![X86_FLOAT_STATE64 as u32, FLOAT_WORDS as u32];
                words.extend(float(machine));
                words
            }
            _ => return None,
        })
    }

    /// Write a flavour's words into `machine`. The segment selectors are
    /// read-only, as Darwin's are to `thread_set_state` in practice.
    pub fn set(machine: &mut Machine, flavor: i32, words: &[u32]) -> bool {
        match flavor {
            X86_THREAD_STATE64 if words.len() >= THREAD_WORDS => {
                let bytes = bytes_of(&words[..THREAD_WORDS]);
                let reg =
                    |i: usize| u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap());
                for (at, index) in ORDER.iter().enumerate() {
                    machine.gpr[*index] = reg(at);
                }
                machine.rip = reg(16);
                machine.rflags = reg(17);
                true
            }
            X86_FLOAT_STATE64 if words.len() >= FLOAT_WORDS => {
                let bytes = bytes_of(&words[..FLOAT_WORDS]);
                let mut image = [0u8; 512];
                image.copy_from_slice(&bytes[8..520]);
                machine.fxsave = Some(image);
                true
            }
            X86_THREAD_STATE | X86_FLOAT_STATE if words.len() >= 2 => {
                let (inner, count) = (words[0] as i32, words[1] as usize);
                matches!(inner, X86_THREAD_STATE64 | X86_FLOAT_STATE64)
                    && words.len() >= 2 + count
                    && set(machine, inner, &words[2..2 + count])
            }
            _ => false,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn machine() -> Machine {
            Machine {
                gpr: std::array::from_fn(|i| 0x100 + i as u64),
                rip: 0xdead,
                rflags: 0x202,
                cs: 0x33,
                fs: 0,
                gs: 0,
                trapno: 0,
                err: 0,
                fault_address: 0,
                fxsave: None,
            }
        }

        #[test]
        fn thread_state_is_in_darwins_register_order() {
            let words = get(&machine(), X86_THREAD_STATE64).unwrap();
            assert_eq!(words.len(), THREAD_WORDS);
            let reg = |i: usize| u64::from(words[i * 2]) | (u64::from(words[i * 2 + 1]) << 32);
            // rax, rbx, rcx, rdx, rdi, rsi, rbp, rsp
            assert_eq!(
                (0..8).map(reg).collect::<Vec<_>>(),
                [0x100, 0x103, 0x101, 0x102, 0x107, 0x106, 0x105, 0x104]
            );
            assert_eq!(reg(16), 0xdead);
            assert_eq!(reg(17), 0x202);
        }

        #[test]
        fn a_state_written_back_reads_the_same() {
            let mut target = machine();
            let mut words = get(&target, X86_THREAD_STATE).unwrap();
            assert_eq!(words.len(), 44);
            // rsp, after the two header words.
            words[2 + 14] = 0x1234;
            assert!(set(&mut target, X86_THREAD_STATE, &words));
            assert_eq!(target.gpr[4], 0x1234);
            assert_eq!(get(&target, X86_THREAD_STATE).unwrap(), words);
        }

        #[test]
        fn float_state_carries_the_fxsave_image() {
            let words = get(&machine(), X86_FLOAT_STATE64).unwrap();
            assert_eq!(words.len(), FLOAT_WORDS);
            // fcw, then mxcsr, after the two reserved words.
            assert_eq!(words[2] & 0xffff, 0x037f);
            assert_eq!(words[2 + 6], 0x1f80);
        }

        #[test]
        fn a_short_buffer_or_unknown_flavor_is_refused() {
            let mut target = machine();
            assert!(!set(&mut target, X86_THREAD_STATE64, &[0; 10]));
            assert!(get(&target, 99).is_none());
        }
    }
}

/// Other architectures' flavours are not translated yet.
#[cfg(not(target_arch = "x86_64"))]
mod state {
    use starnix::signal::Machine;

    pub fn get(_machine: &Machine, _flavor: i32) -> Option<Vec<u32>> {
        None
    }

    pub fn set(_machine: &mut Machine, _flavor: i32, _words: &[u32]) -> bool {
        false
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    /// The suspension handler must not consume a host runtime's small
    /// alternate signal stack while saving the full register/FPU context.
    #[test]
    fn suspension_handler_does_not_use_the_alternate_signal_stack() {
        assert!(install().is_some());
        let signal = suspend_signal().expect("private suspension signal");
        // SAFETY: querying an existing disposition without installing one.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::sigaction(signal, std::ptr::null(), &mut action) },
            0
        );
        assert_eq!(action.sa_flags & libc::SA_ONSTACK, 0);
        assert_ne!(action.sa_flags & libc::SA_SIGINFO, 0);
    }

    /// Whatever `thread_policy_set` was given, a thread reads back as a
    /// timeshare thread with the default time constraint.
    #[test]
    fn a_thread_reports_the_default_timeshare_policy() {
        let me = crate::mach::task::mach_thread_self();
        let mut info = [u32::MAX; 4];
        let mut count = 4;
        let mut default = 0;
        let rc = unsafe {
            thread_policy_get(
                me,
                THREAD_TIME_CONSTRAINT_POLICY,
                info.as_mut_ptr(),
                &mut count,
                &mut default,
            )
        };
        assert_eq!(rc, KERN_SUCCESS);
        assert_eq!(info, [0, STD_QUANTUM / 2, STD_QUANTUM, 1]);
        assert_eq!(default, 1);

        let mut info = [0u32; 1];
        let mut count = 1;
        let mut default = 0;
        let rc = unsafe {
            thread_policy_get(
                me,
                THREAD_EXTENDED_POLICY,
                info.as_mut_ptr(),
                &mut count,
                &mut default,
            )
        };
        assert_eq!(rc, KERN_SUCCESS);
        assert_eq!((info[0], default), (1, 0));

        // Too short for the flavour, and a flavour that does not exist.
        let rc = unsafe {
            thread_policy_get(
                me,
                THREAD_TIME_CONSTRAINT_POLICY,
                info.as_mut_ptr(),
                &mut count,
                &mut default,
            )
        };
        assert_eq!(rc, KERN_INVALID_ARGUMENT);
        let rc = unsafe { thread_policy_get(me, 99, info.as_mut_ptr(), &mut count, &mut default) };
        assert_eq!(rc, KERN_INVALID_ARGUMENT);
    }

    /// A thread spinning on a counter stops advancing it while suspended, its
    /// state names a stack pointer inside its own stack, and it carries on
    /// after a resume.
    #[test]
    fn a_suspended_thread_stops_and_reports_its_stack() {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        static STOP: AtomicBool = AtomicBool::new(false);
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let marker = 0u8;
            tx.send((crate::pthread::id() as i32, &marker as *const u8 as u64))
                .unwrap();
            while !STOP.load(Ordering::Relaxed) {
                COUNTER.fetch_add(1, Ordering::Relaxed);
            }
        });
        let (tid, stack) = rx.recv().unwrap();
        let name = space::insert_thread(crate::process::host_self(), tid);

        assert_eq!(thread_suspend(name), KERN_SUCCESS);
        let before = COUNTER.load(Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(COUNTER.load(Ordering::Relaxed), before, "still running");

        let mut words = [0u32; 42];
        let mut count = words.len() as u32;
        let rc = unsafe { thread_get_state(name, 4, words.as_mut_ptr(), &mut count) };
        assert_eq!(rc, KERN_SUCCESS);
        assert_eq!(count, 42);
        let rsp = u64::from(words[14]) | (u64::from(words[15]) << 32);
        assert!(
            rsp < stack && stack - rsp < 1 << 20,
            "rsp {rsp:#x} vs {stack:#x}"
        );

        // Nested: two suspends need two resumes.
        assert_eq!(thread_suspend(name), KERN_SUCCESS);
        assert_eq!(thread_resume(name), KERN_SUCCESS);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(COUNTER.load(Ordering::Relaxed), before, "resumed too early");
        assert_eq!(thread_resume(name), KERN_SUCCESS);
        assert_eq!(thread_resume(name), KERN_FAILURE);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while COUNTER.load(Ordering::Relaxed) == before && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_ne!(COUNTER.load(Ordering::Relaxed), before, "never resumed");
        STOP.store(true, Ordering::Relaxed);
        worker.join().unwrap();
    }

    /// A running thread can be asked too, as on Darwin, and is left running.
    #[test]
    fn a_running_thread_reports_a_snapshot() {
        static STOP: AtomicBool = AtomicBool::new(false);
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            tx.send(crate::pthread::id() as i32).unwrap();
            while !STOP.load(Ordering::Relaxed) {
                std::hint::spin_loop();
            }
        });
        let tid = rx.recv().unwrap();
        let name = space::insert_thread(crate::process::host_self(), tid);
        let mut words = [0u32; 42];
        let mut count = words.len() as u32;
        let rc = unsafe { thread_get_state(name, 4, words.as_mut_ptr(), &mut count) };
        assert_eq!(rc, KERN_SUCCESS);
        assert!(!suspended(name));
        STOP.store(true, Ordering::Relaxed);
        worker.join().unwrap();
    }

    /// Except the caller itself, which cannot stop to be read.
    #[test]
    fn the_calling_thread_has_no_state_to_read() {
        let name = space::insert_thread(crate::process::host_self(), crate::pthread::id() as i32);
        let mut words = [0u32; 42];
        let mut count = words.len() as u32;
        let rc = unsafe { thread_get_state(name, 4, words.as_mut_ptr(), &mut count) };
        assert_eq!(rc, KERN_FAILURE);
    }
}
