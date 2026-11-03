use core::ffi::{c_int, c_void};

use crate::sys;

pub const SIGEMT: Option<c_int> = sys::signal::SIGEMT;
pub const SIGINFO: Option<c_int> = sys::signal::SIGINFO;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Code {
    /// `kill` and friends.
    User,
    /// `tgkill`/`pthread_kill` where the host tells them apart from `kill`.
    Thread,
    Queue,
    Timer,
    AsyncIo,
    MessageQueue,
    /// The kernel, for a reason it does not name.
    Kernel,

    IllegalOpcode,
    IllegalOperand,
    IllegalAddressing,
    IllegalTrap,
    PrivilegedOpcode,
    PrivilegedRegister,
    Coprocessor,
    BadStack,

    IntegerDivide,
    IntegerOverflow,
    FloatDivide,
    FloatOverflow,
    FloatUnderflow,
    FloatInexact,
    FloatInvalid,
    FloatSubscript,

    Unmapped,
    Protection,

    Misaligned,
    NoSuchAddress,
    ObjectError,

    Breakpoint,
    Trace,

    Exited,
    Killed,
    Dumped,
    Trapped,
    Stopped,
    Continued,

    PollIn,
    PollOut,
    PollMessage,
    PollError,
    PollPriority,
    PollHangup,

    /// A code this crate has no name for.
    #[default]
    Unknown,
}

/// A `siginfo_t`, with only the union members its code selects filled in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// The host's signal number.
    pub signo: c_int,
    /// The host's `errno` value.
    pub errno: c_int,
    pub code: Code,
    pub pid: i32,
    pub uid: u32,
    /// An exit status, or a host signal number for a child that was signalled.
    pub status: i32,
    pub addr: usize,
    pub value: usize,
}

/// Read the kernel's `siginfo_t`.
///
/// # Safety
///
/// `raw` is a live `siginfo_t`, as the kernel passes one to a handler.
pub unsafe fn info(raw: *const libc::siginfo_t) -> Info {
    // SAFETY: the caller's guarantee.
    let raw = unsafe { &*raw };
    let code = sys::signal::code(raw.si_signo, raw.si_code);
    let mut info = Info {
        signo: raw.si_signo,
        errno: raw.si_errno,
        code,
        ..Info::default()
    };
    // The union overlaps on Linux, so reading a member the code did not select
    // reports another member's bits as, say, a fault address.
    // SAFETY: each accessor reads within the structure; which ones are
    // meaningful is decided by the code.
    unsafe {
        use Code::*;
        match code {
            User | Thread => {
                info.pid = raw.si_pid();
                info.uid = raw.si_uid();
            }
            Queue | MessageQueue => {
                info.pid = raw.si_pid();
                info.uid = raw.si_uid();
                info.value = raw.si_value().sival_ptr as usize;
            }
            Timer | AsyncIo => info.value = raw.si_value().sival_ptr as usize,
            Exited | Killed | Dumped | Trapped | Stopped | Continued => {
                info.pid = raw.si_pid();
                info.uid = raw.si_uid();
                info.status = raw.si_status();
            }
            Kernel => {}
            _ if is_fault(raw.si_signo) => info.addr = raw.si_addr() as usize,
            _ => {}
        }
    }
    info
}

fn is_fault(signo: c_int) -> bool {
    matches!(
        signo,
        libc::SIGILL | libc::SIGFPE | libc::SIGSEGV | libc::SIGBUS | libc::SIGTRAP
    )
}

/// The per-signal codes, which every host numbers alike except for `SIGILL`
/// and `SIGFPE`.
pub(crate) fn by_signal(signo: c_int, code: c_int, ill: &[Code; 8], fpe: &[Code; 8]) -> Code {
    use Code::*;
    let pick = |table: &[Code]| {
        usize::try_from(code - 1)
            .ok()
            .and_then(|i| table.get(i).copied())
            .unwrap_or(Unknown)
    };
    match signo {
        libc::SIGILL => pick(ill),
        libc::SIGFPE => pick(fpe),
        libc::SIGSEGV => pick(&[Unmapped, Protection]),
        libc::SIGBUS => pick(&[Misaligned, NoSuchAddress, ObjectError]),
        libc::SIGTRAP => pick(&[Breakpoint, Trace]),
        libc::SIGCHLD => pick(&[Exited, Killed, Dumped, Trapped, Stopped, Continued]),
        libc::SIGIO => pick(&[
            PollIn,
            PollOut,
            PollMessage,
            PollError,
            PollPriority,
            PollHangup,
        ]),
        _ => Unknown,
    }
}

/// The interrupted thread's registers, in an order of this crate's choosing.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Machine {
    /// rax, rcx, rdx, rbx, rsp, rbp, rsi, rdi, r8-r15: the encoding's order.
    pub gpr: [u64; 16],
    pub rip: u64,
    pub rflags: u64,
    /// Never written back: no host lets a handler change them meaningfully.
    pub cs: u16,
    pub fs: u16,
    pub gs: u16,
    pub trapno: u32,
    pub err: u64,
    pub fault_address: u64,
    /// The `FXSAVE` image, where the host saved one.
    pub fxsave: Option<[u8; 512]>,
}

#[cfg(target_arch = "x86_64")]
impl Machine {
    pub fn pc(&self) -> u64 {
        self.rip
    }
}

#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Machine {
    /// x0-x30; x29 is the frame pointer and x30 the link register.
    pub x: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    pub pstate: u64,
    pub fault_address: u64,
    /// `ESR_EL1`, or zero where the host does not report it.
    pub esr: u64,
    pub fpsimd: Option<Fpsimd>,
}

#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fpsimd {
    pub v: [u128; 32],
    pub fpsr: u32,
    pub fpcr: u32,
}

#[cfg(target_arch = "aarch64")]
impl Machine {
    pub fn pc(&self) -> u64 {
        self.pc
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Machine {
    _private: (),
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
impl Machine {
    pub fn pc(&self) -> u64 {
        0
    }
}

/// A handler's third argument, read out of the host's `ucontext_t`.
#[derive(Clone, Copy)]
pub struct Context {
    pub machine: Machine,
    /// The mask restored when the handler returns.
    pub mask: libc::sigset_t,
    /// Read only: writing it back would move the thread's alternate stack on
    /// Linux and nowhere else.
    pub stack: libc::stack_t,
}

/// Read the context the kernel passed, or `None` for a null pointer or a host
/// this crate cannot read one on.
///
/// # Safety
///
/// `raw` is null or the third argument of an `SA_SIGINFO` handler.
pub unsafe fn context(raw: *const c_void) -> Option<Context> {
    if raw.is_null() {
        return None;
    }
    // SAFETY: the caller's guarantee.
    unsafe { sys::signal::context(raw) }
}

/// Write `context` into the one the kernel restores from when the handler
/// returns. Registers, floating-point state and mask; nothing else.
///
/// # Safety
///
/// As [`context`], and the handler that received `raw` has not yet returned.
pub unsafe fn set_context(raw: *mut c_void, context: &Context) {
    if raw.is_null() {
        return;
    }
    // SAFETY: the caller's guarantee.
    unsafe { sys::signal::set_context(raw, context) }
}

/// The `index`th host signal set aside for the runtime: one no guest can
/// name, so no guest handler or mask ever touches it. `None` where the host
/// has none to spare.
pub fn private_signal(index: u8) -> Option<c_int> {
    sys::signal::private_signal(index)
}

/// Send `signo` to thread `tid` of this process, and only to it.
pub fn send_to_thread(tid: i32, signo: c_int) -> std::io::Result<()> {
    sys::signal::send_to_thread(tid, signo)
}

/// Queue `signo` for process `pid` with `value` attached, which its handler
/// reads back as [`Info::value`] under [`Code::Queue`]. A host without
/// `sigqueue` sends a plain signal.
pub fn queue(pid: i32, signo: c_int, value: usize) -> std::io::Result<()> {
    sys::signal::queue(pid, signo, value)
}

/// Call `each` with every thread id of this process, async-signal-safely: no
/// allocation and no locks, so a signal handler may walk its own threads.
/// A thread may be missed while others exit; list again to catch it.
/// False where the host cannot list them that way.
pub fn for_each_thread(each: &mut dyn FnMut(i32)) -> bool {
    sys::signal::for_each_thread(each)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static SEEN: Mutex<Option<(Info, Option<u64>)>> = Mutex::new(None);

    extern "C" fn record(_: c_int, raw_info: *mut libc::siginfo_t, raw: *mut c_void) {
        // SAFETY: the kernel's own arguments.
        let seen = unsafe { (info(raw_info), context(raw).map(|c| c.machine.pc())) };
        if let Ok(mut slot) = SEEN.try_lock() {
            *slot = Some(seen);
        }
    }

    #[test]
    fn a_signal_sent_to_self_names_its_sender_and_has_a_pc() {
        // SAFETY: a handler of this test's own on a signal nothing else uses,
        // restored afterwards.
        unsafe {
            let mut act: libc::sigaction = core::mem::zeroed();
            act.sa_sigaction = record as *const () as libc::sighandler_t;
            act.sa_flags = libc::SA_SIGINFO;
            let mut old: libc::sigaction = core::mem::zeroed();
            libc::sigaction(libc::SIGUSR2, &act, &mut old);
            libc::raise(libc::SIGUSR2);
            libc::sigaction(libc::SIGUSR2, &old, core::ptr::null_mut());
        }
        let (info, pc) = SEEN.lock().unwrap().take().expect("the handler ran");
        assert_eq!(info.signo, libc::SIGUSR2);
        assert!(
            matches!(info.code, Code::User | Code::Thread),
            "{:?}",
            info.code
        );
        assert_eq!(info.pid, crate::process::me());
        assert_eq!(info.addr, 0);
        if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
            assert!(pc.is_some_and(|pc| pc != 0));
        }
    }

    #[test]
    fn the_shared_codes_are_numbered_alike_everywhere() {
        let none = [Code::Unknown; 8];
        assert_eq!(by_signal(libc::SIGSEGV, 2, &none, &none), Code::Protection);
        assert_eq!(by_signal(libc::SIGCHLD, 2, &none, &none), Code::Killed);
        assert_eq!(by_signal(libc::SIGBUS, 9, &none, &none), Code::Unknown);
        assert_eq!(by_signal(libc::SIGBUS, 0, &none, &none), Code::Unknown);
    }
}
