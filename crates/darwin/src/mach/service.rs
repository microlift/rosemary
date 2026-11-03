//! The guest's half of the process server's calls to it: a private signal
//! handler that fetches the work the server queued for this task, does it,
//! and answers (see `server::task` for the other half, and `docs/server.md`).
//!
//! The signal is a realtime one Darwin has no number for, so no guest can
//! install a handler for it, block it, or send it: its `sigaction`, masks and
//! `kill` only ever speak Darwin's 31 signals. Only the server's own signal
//! (or this process re-raising one it could not take yet) is acted on.
//!
//! The handler may interrupt anything, holding any lock, on any stack, so:
//!
//! * `errno` is saved on entry and restored on the way out;
//! * nothing allocates or locks, and nothing touches stdio: the frames are
//!   built and read in two static buffers, over raw `send`/`recv`;
//! * the work runs on a stack of its own, mapped when the thread started (or,
//!   for a thread this runtime did not start, on first use), not on whatever
//!   small stack the guest left the thread with. The stack pointer is back to
//!   the kernel's before the handler returns, which `sigreturn` expects.
//!
//! One thread services at a time. Another one the signal lands on meanwhile
//! leaves a note, and the servicing thread fetches again before it lets go.

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

use server::codec::Malformed;
use server::task::{self as work, Batch, CHUNK, Full, Op, Outcome, Sink};

use super::kern::*;
use super::local;

/// Whether this process can be reached by the server at all.
static INSTALLED: OnceLock<bool> = OnceLock::new();

/// Install the handler, once, and ready this thread to run it. False on a host
/// with no private signal to spare, where the server is told not to queue
/// anything for this process.
pub fn install() -> bool {
    *INSTALLED.get_or_init(|| {
        let Some(signal) = work::signal() else {
            // FIXME (ACCURACY): a macOS host has no realtime signals. Sharing a
            // guest-visible one - acting on it when the server sent it and
            // passing it on to the guest otherwise - would reach it.
            return false;
        };
        // Task suspension runs in this handler; set it up outside it.
        super::thread::prepare();
        prepare_thread();
        // SAFETY: a handler of this module's own, on a signal no guest names.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = entry as *const () as usize;
            // No `SA_ONSTACK`: the work moves to a stack of its own anyway, and
            // a guest's alternate stack may be the smallest the guest could get
            // away with.
            action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
            // Everything blocked while servicing - a parked thread must not
            // hold the queue - except the faults, which are not deferrable.
            libc::sigfillset(&mut action.sa_mask);
            for fault in [
                libc::SIGSEGV,
                libc::SIGBUS,
                libc::SIGILL,
                libc::SIGFPE,
                libc::SIGTRAP,
            ] {
                libc::sigdelset(&mut action.sa_mask, fault);
            }
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
        true
    })
}

/// Ready the calling thread to service: its own stack for the handler, and its
/// slot for task suspension. Called as every thread starts; a thread that
/// skipped it is readied in the handler instead.
pub fn prepare_thread() {
    if INSTALLED.get() == Some(&false) || work::signal().is_none() {
        return;
    }
    let _ = stack_top(crate::pthread::id() as i32);
    super::thread::prepare_thread();
}

/// Service stacks, by thread. Big enough for the handler and the copies it
/// makes; the frames themselves are in [`BUFFERS`].
const STACK_SIZE: usize = 128 << 10;
const STACK_SLOTS: usize = 4096;

struct StackSlot {
    /// Zero when never used.
    tid: AtomicI32,
    /// The mapping, guard page first; zero until mapped.
    base: AtomicUsize,
}

static STACKS: [StackSlot; STACK_SLOTS] = [const {
    StackSlot {
        tid: AtomicI32::new(0),
        base: AtomicUsize::new(0),
    }
}; STACK_SLOTS];

fn guard_size() -> usize {
    super::info::page_size() as usize
}

/// The top of `tid`'s service stack, mapping one if it has none. A slot whose
/// thread has ended is taken over with its stack. Async-signal-safe.
fn stack_top(tid: i32) -> Option<usize> {
    let top = |slot: &StackSlot| {
        let base = slot.base.load(Ordering::Acquire);
        (base != 0).then(|| base + guard_size() + STACK_SIZE)
    };
    if let Some(slot) = STACKS
        .iter()
        .find(|slot| slot.tid.load(Ordering::Acquire) == tid)
    {
        return top(slot);
    }
    for slot in &STACKS {
        let held = slot.tid.load(Ordering::Acquire);
        let free = held == 0 || starnix::signal::send_to_thread(held, 0).is_err();
        if !free
            || slot
                .tid
                .compare_exchange(held, tid, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            continue;
        }
        if slot.base.load(Ordering::Acquire) == 0 {
            let length = guard_size() + STACK_SIZE;
            // SAFETY: a fresh anonymous mapping, kept for the process's life.
            let base = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    length,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANON,
                    -1,
                    0,
                )
            };
            if base == libc::MAP_FAILED {
                slot.tid.store(0, Ordering::Release);
                return None;
            }
            // SAFETY: the first page of the mapping just made, as a guard.
            unsafe { libc::mprotect(base, guard_size(), libc::PROT_NONE) };
            slot.base.store(base as usize, Ordering::Release);
        }
        return top(slot);
    }
    None
}

/// Call `body(argument)` with the stack pointer at `top`, and put the caller's
/// back before returning.
///
/// # Safety
///
/// `top` is the 16-byte-aligned top of a stack nothing else is using, big
/// enough for `body`, which must not unwind.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn on_stack(
    _argument: *mut c_void,
    _body: extern "C" fn(*mut c_void),
    _top: usize,
) {
    core::arch::naked_asm!(
        "push rbp",
        "mov rbp, rsp",
        "mov rsp, rdx",
        "call rsi",
        "mov rsp, rbp",
        "pop rbp",
        "ret",
    );
}

/// The arm64 form of the x86-64 `on_stack`.
///
/// # Safety
///
/// As the x86-64 form.
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn on_stack(
    _argument: *mut c_void,
    _body: extern "C" fn(*mut c_void),
    _top: usize,
) {
    core::arch::naked_asm!(
        "stp x29, x30, [sp, #-16]!",
        "mov x29, sp",
        "mov sp, x2",
        "blr x1",
        "mov sp, x29",
        "ldp x29, x30, [sp], #16",
        "ret",
    );
}

/// Run `body(argument)` on this thread's service stack, or on the stack it is
/// on when no service stack can be had.
fn on_service_stack(body: extern "C" fn(*mut c_void), argument: *mut c_void) {
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    if let Some(top) = stack_top(crate::pthread::id() as i32) {
        // Already on it: a thread parked by its task's suspension takes the
        // signal again, nested, and the outer frames are still live up there.
        let here = &raw const top as usize;
        if (top - STACK_SIZE..top).contains(&here) {
            body(argument);
            return;
        }
        // SAFETY: this thread's own stack, unused while it is outside the
        // handler, which blocks re-entry; `body` is `extern "C"` and aborts
        // rather than unwinds.
        unsafe { on_stack(argument, body, top & !15) };
        return;
    }
    body(argument);
}

/// The handler the kernel enters.
extern "C" fn entry(_signo: i32, info: *mut libc::siginfo_t, raw: *mut c_void) {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    let errno = starnix::errno::get();
    // SAFETY: the kernel's own `siginfo_t`.
    let info = unsafe { starnix::signal::info(info) };
    let me = crate::process::host_self();
    let server = server::client::server_pid();
    let ours = info.code == starnix::signal::Code::Queue
        && info.pid != 0
        && (info.pid == server || info.pid == me);
    if ours {
        on_service_stack(service, raw);
    }
    starnix::errno::set(errno);
}

/// One thread services at a time; [`AGAIN`] is how another says it was
/// signalled meanwhile.
static BUSY: AtomicBool = AtomicBool::new(false);
static AGAIN: AtomicBool = AtomicBool::new(false);

/// The service proper, on the service stack. `raw` is the handler's context.
extern "C" fn service(raw: *mut c_void) {
    if BUSY.swap(true, Ordering::Acquire) {
        AGAIN.store(true, Ordering::Release);
        return;
    }
    let mut park = false;
    loop {
        AGAIN.store(false, Ordering::Release);
        park |= drain();
        BUSY.store(false, Ordering::Release);
        if !AGAIN.load(Ordering::Acquire) || BUSY.swap(true, Ordering::Acquire) {
            break;
        }
    }
    if park {
        // Answered and let go of the queue first, and open to the signal again
        // while parked - this may be the only thread, and whatever comes next,
        // the resume included, still has to be served. Returning from the
        // handler puts the mask back.
        // SAFETY: a local set, and this handler's own context on this thread.
        unsafe {
            if let Some(signal) = work::signal() {
                let mut open: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut open);
                libc::sigaddset(&mut open, signal);
                libc::pthread_sigmask(libc::SIG_UNBLOCK, &open, std::ptr::null_mut());
            }
            super::thread::park_self(raw);
        }
    }
}

/// The two frames' worth of memory the handler works in, only ever touched
/// while [`BUSY`] is held.
struct Buffers {
    input: UnsafeCell<[u8; INPUT]>,
    output: UnsafeCell<[u8; OUTPUT]>,
}

// SAFETY: reached only by the thread holding `BUSY`.
unsafe impl Sync for Buffers {}

/// A fetched batch: up to `BATCH` bytes, beyond a first piece that may be a
/// whole chunk on its own, and the frame around them.
const INPUT: usize = work::BATCH + CHUNK + 4096;
/// Completions, flushed whenever the next might not fit.
const OUTPUT: usize = 2 * CHUNK + 4096;

static BUFFERS: Buffers = Buffers {
    input: UnsafeCell::new([0; INPUT]),
    output: UnsafeCell::new([0; OUTPUT]),
};

/// The connection broke, or said something this end cannot read.
struct Broken;

/// Fetch and do work until the server has none left. True if a suspension
/// asked this thread to park once it is done.
fn drain() -> bool {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    let fd = server::client::service_fd();
    if fd < 0 {
        // Not open yet, or lost: the next one opened raises the signal again.
        server::client::missed();
        return false;
    }
    let mut park = false;
    match fetch_and_do(fd, &mut park) {
        Ok(()) => {}
        Err(Broken) => server::client::service_broken(fd),
    }
    park
}

fn fetch_and_do(fd: i32, park: &mut bool) -> Result<(), Broken> {
    // SAFETY: `BUSY` is held; nothing else reaches these.
    let (input, output) = unsafe { (&mut *BUFFERS.input.get(), &mut *BUFFERS.output.get()) };
    loop {
        let mut request = [0u8; 8];
        let mut fetch = Sink::new(&mut request);
        fetch.fetch().map_err(|Full| Broken)?;
        send_all(fd, fetch.finish())?;
        let payload = recv_frame(fd, input)?;
        let batch = Batch::parse(payload).map_err(|Malformed| Broken)?;
        if batch.is_empty() {
            return Ok(());
        }
        let mut answers = Answers::new(&mut output[..])?;
        for piece in batch {
            let piece = piece.map_err(|Malformed| Broken)?;
            answers.answer(fd, piece.id, piece.op, park)?;
        }
        answers.flush(fd)?;
    }
}

/// Completions being gathered into one `Complete` frame.
struct Answers<'a> {
    sink: Sink<'a>,
    /// Where the count is, to fill in once it is known.
    at: usize,
    count: u32,
}

impl<'a> Answers<'a> {
    fn new(buffer: &'a mut [u8]) -> Result<Self, Broken> {
        let mut sink = Sink::new(buffer);
        let at = sink.complete(0).map_err(|Full| Broken)?;
        Ok(Answers { sink, at, count: 0 })
    }

    fn flush(&mut self, fd: i32) -> Result<(), Broken> {
        if self.count == 0 {
            return Ok(());
        }
        self.sink.recount(self.at, self.count);
        send_all(fd, self.sink.finish())?;
        // `Reply::Done`, which carries nothing to act on. Read into a buffer of
        // its own: the batch being answered is still in the input one.
        let mut done = [0u8; 64];
        recv_frame(fd, &mut done)?;
        self.sink.truncate(0);
        self.at = self.sink.complete(0).map_err(|Full| Broken)?;
        self.count = 0;
        Ok(())
    }

    /// Make sure `bytes` more fit, flushing what is gathered if not.
    fn room(&mut self, fd: i32, bytes: usize) -> Result<(), Broken> {
        if self.sink.room() < bytes {
            self.flush(fd)?;
        }
        Ok(())
    }

    fn answer(&mut self, fd: i32, id: u64, op: Op<'_>, park: &mut bool) -> Result<(), Broken> {
        // Id, tag, and the largest fixed-size outcome.
        const HEAD: usize = 8 + 1 + 8;
        let outcome = match op {
            Op::Read { address, size } => {
                let size = size.min(CHUNK as u64) as usize;
                self.room(fd, HEAD + 4 + size)?;
                let mark = self.sink.mark();
                let out = self.sink.data(id, size).map_err(|Full| Broken)?;
                match local::read(address, out) {
                    Ok(()) => {
                        self.count += 1;
                        return Ok(());
                    }
                    Err(code) => {
                        self.sink.truncate(mark);
                        Outcome::Failed(code)
                    }
                }
            }
            // SAFETY: the guest's memory, which the guest the server let ask
            // may write as it may on Darwin.
            Op::Write { address, data } => match unsafe { local::write(address, data) } {
                Ok(()) => Outcome::Done,
                Err(code) => Outcome::Failed(code),
            },
            Op::Protect {
                address,
                size,
                set_maximum,
                protection,
            } => code(local::protect(address, size, set_maximum, protection)),
            Op::Allocate {
                address,
                size,
                flags,
            } => match local::allocate(address, size, flags) {
                Ok(address) => Outcome::Address(address),
                Err(code) => Outcome::Failed(code),
            },
            // SAFETY: as for a write.
            Op::Deallocate { address, size } => code(unsafe { local::deallocate(address, size) }),
            // SAFETY: as for a write.
            Op::Copy { source, size, dest } => code(unsafe { local::copy(source, size, dest) }),
            Op::Suspend => {
                let rc = super::thread::suspend_task();
                *park |= rc == KERN_SUCCESS;
                code(rc)
            }
            Op::Resume => code(super::thread::resume_task()),
        };
        self.room(fd, HEAD)?;
        self.sink.completion(id, outcome).map_err(|Full| Broken)?;
        self.count += 1;
        Ok(())
    }
}

fn code(rc: i32) -> Outcome {
    match rc {
        KERN_SUCCESS => Outcome::Done,
        rc => Outcome::Failed(rc),
    }
}

fn send_all(fd: i32, mut bytes: &[u8]) -> Result<(), Broken> {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    const FLAGS: i32 = libc::MSG_NOSIGNAL;
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    const FLAGS: i32 = 0;
    while !bytes.is_empty() {
        // SAFETY: a live buffer of the stated length.
        let n = unsafe { libc::send(fd, bytes.as_ptr().cast(), bytes.len(), FLAGS) };
        if n < 0 {
            if starnix::errno::get() == libc::EINTR {
                continue;
            }
            return Err(Broken);
        }
        bytes = &bytes[n as usize..];
    }
    Ok(())
}

fn recv_exact(fd: i32, buffer: &mut [u8]) -> Result<(), Broken> {
    let mut done = 0;
    while done < buffer.len() {
        // SAFETY: the rest of a live buffer.
        let n = unsafe {
            libc::recv(
                fd,
                buffer[done..].as_mut_ptr().cast(),
                buffer.len() - done,
                0,
            )
        };
        match n {
            0 => return Err(Broken),
            n if n < 0 => {
                if starnix::errno::get() != libc::EINTR {
                    return Err(Broken);
                }
            }
            n => done += n as usize,
        }
    }
    Ok(())
}

/// One frame's payload, read into `buffer`.
fn recv_frame(fd: i32, buffer: &mut [u8]) -> Result<&[u8], Broken> {
    let mut len = [0u8; 4];
    recv_exact(fd, &mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    let payload = buffer.get_mut(..len).ok_or(Broken)?;
    recv_exact(fd, payload)?;
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn record_depth(argument: *mut c_void) {
        let marker = 0u8;
        // SAFETY: the caller's local.
        unsafe { *(argument as *mut usize) = &marker as *const u8 as usize };
    }

    #[test]
    fn work_runs_on_the_threads_service_stack_and_returns() {
        let tid = crate::pthread::id() as i32;
        let top = stack_top(tid).unwrap();
        let mut seen = 0usize;
        on_service_stack(record_depth, (&raw mut seen).cast());
        assert!(
            seen < top && seen > top - STACK_SIZE,
            "{seen:#x} vs {top:#x}"
        );
        // And the same stack the next time.
        assert_eq!(stack_top(tid), Some(top));
    }
}
