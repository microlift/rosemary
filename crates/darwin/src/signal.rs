use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use starnix::signal::{Code, Info, Machine};

use crate::translation::DarwinTranslation;

/// A saved `sa_mask` that a fault handler can stash without allocating.
///
/// Bytes rather than a `sigset_t` because the latter has no `const` empty value
/// (it is 128 bytes under glibc); all-zero is the empty set on every host.
#[derive(Clone, Copy)]
pub struct Mask([u8; size_of::<libc::sigset_t>()]);

impl Mask {
    pub const fn empty() -> Mask {
        Mask([0; size_of::<libc::sigset_t>()])
    }

    pub fn of(action: &libc::sigaction) -> Mask {
        // SAFETY: a `sigset_t` is a plain bit set of exactly this size.
        Mask(unsafe { core::mem::transmute_copy(&action.sa_mask) })
    }

    pub fn apply(self, action: &mut libc::sigaction) {
        // SAFETY: as in `of`.
        action.sa_mask = unsafe { core::mem::transmute_copy(&self.0) };
    }
}

/// A [`Mask`] in a `static`, written once during start-up and read from
/// handlers afterwards.
pub struct StaticMask(core::cell::UnsafeCell<Mask>);

// SAFETY: written only by `set`, whose contract excludes concurrent access.
unsafe impl Sync for StaticMask {}

impl StaticMask {
    pub const fn empty() -> StaticMask {
        StaticMask(core::cell::UnsafeCell::new(Mask::empty()))
    }

    /// # Safety
    ///
    /// Called before any handler that reads this can run, and not concurrently
    /// with another `set`.
    pub unsafe fn set(&self, mask: Mask) {
        // SAFETY: the caller's guarantee.
        unsafe { *self.0.get() = mask };
    }

    pub fn get(&self) -> Mask {
        // SAFETY: always a valid `Mask`, and only `set` writes it.
        unsafe { *self.0.get() }
    }
}

/// Darwin's signal numbers, from `<sys/signal.h>`.
pub mod signum {
    pub const SIGHUP: i32 = 1;
    pub const SIGINT: i32 = 2;
    pub const SIGQUIT: i32 = 3;
    pub const SIGILL: i32 = 4;
    pub const SIGTRAP: i32 = 5;
    pub const SIGABRT: i32 = 6;
    pub const SIGEMT: i32 = 7;
    pub const SIGFPE: i32 = 8;
    pub const SIGKILL: i32 = 9;
    pub const SIGBUS: i32 = 10;
    pub const SIGSEGV: i32 = 11;
    pub const SIGSYS: i32 = 12;
    pub const SIGPIPE: i32 = 13;
    pub const SIGALRM: i32 = 14;
    pub const SIGTERM: i32 = 15;
    pub const SIGURG: i32 = 16;
    pub const SIGSTOP: i32 = 17;
    pub const SIGTSTP: i32 = 18;
    pub const SIGCONT: i32 = 19;
    pub const SIGCHLD: i32 = 20;
    pub const SIGTTIN: i32 = 21;
    pub const SIGTTOU: i32 = 22;
    pub const SIGIO: i32 = 23;
    pub const SIGXCPU: i32 = 24;
    pub const SIGXFSZ: i32 = 25;
    pub const SIGVTALRM: i32 = 26;
    pub const SIGPROF: i32 = 27;
    pub const SIGWINCH: i32 = 28;
    pub const SIGINFO: i32 = 29;
    pub const SIGUSR1: i32 = 30;
    pub const SIGUSR2: i32 = 31;
}

/// The host's number for each Darwin signal, indexed by Darwin's less one.
const HOST_SIGNALS: [Option<c_int>; 31] = [
    Some(libc::SIGHUP),
    Some(libc::SIGINT),
    Some(libc::SIGQUIT),
    Some(libc::SIGILL),
    Some(libc::SIGTRAP),
    Some(libc::SIGABRT),
    starnix::signal::SIGEMT,
    Some(libc::SIGFPE),
    Some(libc::SIGKILL),
    Some(libc::SIGBUS),
    Some(libc::SIGSEGV),
    Some(libc::SIGSYS),
    Some(libc::SIGPIPE),
    Some(libc::SIGALRM),
    Some(libc::SIGTERM),
    Some(libc::SIGURG),
    Some(libc::SIGSTOP),
    Some(libc::SIGTSTP),
    Some(libc::SIGCONT),
    Some(libc::SIGCHLD),
    Some(libc::SIGTTIN),
    Some(libc::SIGTTOU),
    Some(libc::SIGIO),
    Some(libc::SIGXCPU),
    Some(libc::SIGXFSZ),
    Some(libc::SIGVTALRM),
    Some(libc::SIGPROF),
    Some(libc::SIGWINCH),
    starnix::signal::SIGINFO,
    Some(libc::SIGUSR1),
    Some(libc::SIGUSR2),
];

/// Darwin's signal number as this host's, or `None` for one the host has not
/// got. Never a neighbour: a handler installed for one signal must not run
/// for another.
pub fn signal_to_host(darwin: i32) -> Option<c_int> {
    let index = usize::try_from(darwin).ok()?.checked_sub(1)?;
    HOST_SIGNALS.get(index).copied().flatten()
}

pub fn signal_from_host(host: c_int) -> Option<i32> {
    (1..=31).find(|d| signal_to_host(*d) == Some(host))
}

/// A Darwin signal this host cannot raise (`SIGEMT`, `SIGINFO` on Linux).
///
/// A handler for one is kept and reported back, and never runs: a program that
/// installs one for every signal it forwards (`sudo`) must not fail to start.
pub fn unraisable(darwin: i32) -> bool {
    (1..=31).contains(&darwin) && signal_to_host(darwin).is_none()
}

const SA_SIGINFO: u32 = 0x0040;

/// Darwin's `SA_*` bits against the host's.
const FLAGS: [(u32, c_int); 7] = [
    (0x0001, libc::SA_ONSTACK),
    (0x0002, libc::SA_RESTART),
    (0x0004, libc::SA_RESETHAND),
    (0x0008, libc::SA_NOCLDSTOP),
    (0x0010, libc::SA_NODEFER),
    (0x0020, libc::SA_NOCLDWAIT),
    (SA_SIGINFO, libc::SA_SIGINFO),
];

/// Darwin's `sa_flags` as this host's. A bit with no counterpart is dropped.
pub fn flags_to_host(darwin: u32) -> c_int {
    FLAGS
        .iter()
        .filter(|(d, _)| darwin & d != 0)
        .fold(0, |out, (_, h)| out | h)
}

pub fn flags_from_host(host: c_int) -> u32 {
    FLAGS
        .iter()
        .filter(|(_, h)| host & h != 0)
        .fold(0, |out, (d, _)| out | d)
}

fn darwin_bit(darwin: i32) -> u32 {
    1 << (darwin - 1)
}

/// Rewrite the bits of `host` that a Darwin mask can name, keeping the rest
/// (real-time signals, for one) as they were.
fn mask_into_host(darwin: u32, host: &mut libc::sigset_t) {
    for signum in 1..=31 {
        let Some(host_signum) = signal_to_host(signum) else {
            continue;
        };
        // SAFETY: a live set and a signal this host has.
        unsafe {
            match darwin & darwin_bit(signum) != 0 {
                true => libc::sigaddset(host, host_signum),
                false => libc::sigdelset(host, host_signum),
            }
        };
    }
}

/// A Darwin `sigset_t`, one bit per Darwin signal, as this host's.
pub fn mask_to_host(darwin: u32) -> libc::sigset_t {
    // SAFETY: emptied before use.
    let mut host: libc::sigset_t = unsafe { core::mem::zeroed() };
    // SAFETY: a live local.
    unsafe { libc::sigemptyset(&mut host) };
    mask_into_host(darwin, &mut host);
    host
}

pub fn mask_from_host(host: &libc::sigset_t) -> u32 {
    (1..=31)
        .filter(|signum| {
            // SAFETY: a live set and a signal this host has.
            signal_to_host(*signum).is_some_and(|h| unsafe { libc::sigismember(host, h) } == 1)
        })
        .fold(0, |out, signum| out | darwin_bit(signum))
}

const SS_ONSTACK: i32 = 0x0001;
const SS_DISABLE: i32 = 0x0004;

/// Darwin's `stack_t`. Linux puts `ss_flags` before `ss_size`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stack {
    sp: u64,
    size: u64,
    flags: i32,
}

impl DarwinTranslation<libc::stack_t> for Stack {
    fn to_host(&self) -> libc::stack_t {
        let mut flags = 0;
        if self.flags & SS_ONSTACK != 0 {
            flags |= libc::SS_ONSTACK;
        }
        if self.flags & SS_DISABLE != 0 {
            flags |= libc::SS_DISABLE;
        }
        libc::stack_t {
            ss_sp: self.sp as *mut c_void,
            ss_size: self.size as usize,
            ss_flags: flags,
        }
    }

    fn from_host(host: &libc::stack_t) -> Stack {
        let mut flags = 0;
        if host.ss_flags & libc::SS_ONSTACK != 0 {
            flags |= SS_ONSTACK;
        }
        if host.ss_flags & libc::SS_DISABLE != 0 {
            flags |= SS_DISABLE;
        }
        Stack {
            sp: host.ss_sp as u64,
            size: host.ss_size as u64,
            flags,
        }
    }
}

/// Signals whose host disposition belongs to the runtime; a bit per Darwin
/// signal. See [`retain`].
static RETAINED: AtomicU32 = AtomicU32::new(0);

/// What the guest asked for on each retained or unraisable signal, so it reads
/// its own installation back - a crash reporter that finds `SIG_DFL` there
/// concludes it failed to start.
static PRETENDED: [Pretended; 32] = [const { Pretended::new() }; 32];

struct Pretended {
    handler: AtomicU64,
    mask: AtomicU32,
    flags: AtomicU32,
}

impl Pretended {
    const fn new() -> Pretended {
        Pretended {
            handler: AtomicU64::new(0),
            mask: AtomicU32::new(0),
            flags: AtomicU32::new(0),
        }
    }

    fn get(&self) -> Disposition {
        Disposition {
            handler: self.handler.load(Ordering::Relaxed),
            mask: self.mask.load(Ordering::Relaxed),
            flags: self.flags.load(Ordering::Relaxed),
        }
    }

    fn set(&self, value: Disposition) {
        self.handler.store(value.handler, Ordering::Relaxed);
        self.mask.store(value.mask, Ordering::Relaxed);
        self.flags.store(value.flags, Ordering::Relaxed);
    }
}

/// Keep the host's disposition for `darwin_signum` whatever the guest asks for.
///
/// For the fault signals while the runtime's own reporter is wanted: a guest
/// `SIGSEGV` handler otherwise takes every diagnostic with it. Not the default,
/// because a program that recovers from its own faults would hang instead.
pub fn retain(darwin_signum: i32) {
    if (1..=31).contains(&darwin_signum) {
        RETAINED.fetch_or(darwin_bit(darwin_signum), Ordering::Relaxed);
    }
}

pub fn retained(darwin_signum: i32) -> bool {
    (1..=31).contains(&darwin_signum)
        && RETAINED.load(Ordering::Relaxed) & darwin_bit(darwin_signum) != 0
}

/// Whether the alternate signal stack belongs to the runtime too.
///
/// Every `SA_ONSTACK` handler shares it, so a guest that installs a small one
/// (Crashpad installs `MINSIGSTKSZ`) overflows the runtime's fault reporter.
static RETAIN_ALTSTACK: AtomicBool = AtomicBool::new(false);

// Alternate stacks are per-thread kernel state, so what the guest asked for is
// per-thread too.
thread_local! {
    static PRETENDED_ALTSTACK: core::cell::Cell<(usize, usize, i32)> =
        const { core::cell::Cell::new((0, 0, libc::SS_DISABLE)) };
}

pub fn retain_altstack() {
    RETAIN_ALTSTACK.store(true, Ordering::Relaxed);
}

pub fn altstack_retained() -> bool {
    RETAIN_ALTSTACK.load(Ordering::Relaxed)
}

pub fn pretended_altstack() -> libc::stack_t {
    PRETENDED_ALTSTACK.with(|held| {
        let (sp, size, flags) = held.get();
        libc::stack_t {
            ss_sp: sp as *mut c_void,
            ss_size: size,
            ss_flags: flags,
        }
    })
}

pub fn set_pretended_altstack(stack: &libc::stack_t) {
    PRETENDED_ALTSTACK.with(|held| held.set((stack.ss_sp as usize, stack.ss_size, stack.ss_flags)));
}

/// Darwin's `struct sigaction`, the same sixteen bytes on both architectures.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Disposition {
    pub handler: u64,
    pub mask: u32,
    pub flags: u32,
}

/// `SIG_DFL`, `SIG_IGN` and `SIG_ERR`, which are values rather than addresses.
fn is_sentinel(handler: u64) -> bool {
    handler == 0 || handler == 1 || handler as i64 == -1
}

/// Darwin's `siginfo_t`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Siginfo {
    pub(crate) signo: i32,
    errno: i32,
    pub(crate) code: i32,
    pub(crate) pid: i32,
    uid: u32,
    pub(crate) status: i32,
    addr: u64,
    value: u64,
    band: i64,
    _pad: [u64; 7],
}

impl Siginfo {
    pub(crate) fn new(darwin: i32, info: &Info) -> Siginfo {
        let status = match info.code {
            Code::Killed | Code::Dumped | Code::Stopped | Code::Trapped => {
                signal_from_host(info.status).unwrap_or(info.status)
            }
            _ => info.status,
        };
        Siginfo {
            signo: darwin,
            errno: crate::errno::from_host(info.errno),
            code: code_to_darwin(info.code),
            pid: info.pid,
            uid: info.uid,
            status,
            addr: info.addr as u64,
            value: info.value as u64,
            band: 0,
            _pad: [0; 7],
        }
    }
}

/// Darwin's `si_code`. `SIGILL` and `SIGFPE` are numbered differently from
/// every other host.
fn code_to_darwin(code: Code) -> i32 {
    use Code::*;
    match code {
        User | Thread => 0x10001,
        Queue => 0x10002,
        Timer => 0x10003,
        AsyncIo => 0x10004,
        MessageQueue => 0x10005,
        Kernel | Unknown => 0,
        IllegalOpcode => 1,
        IllegalTrap => 2,
        PrivilegedOpcode => 3,
        IllegalOperand => 4,
        IllegalAddressing => 5,
        PrivilegedRegister => 6,
        Coprocessor => 7,
        BadStack => 8,
        FloatDivide => 1,
        FloatOverflow => 2,
        FloatUnderflow => 3,
        FloatInexact => 4,
        FloatInvalid => 5,
        FloatSubscript => 6,
        IntegerDivide => 7,
        IntegerOverflow => 8,
        Unmapped => 1,
        Protection => 2,
        Misaligned => 1,
        NoSuchAddress => 2,
        ObjectError => 3,
        Breakpoint => 1,
        Trace => 2,
        Exited => 1,
        Killed => 2,
        Dumped => 3,
        Trapped => 4,
        Stopped => 5,
        Continued => 6,
        PollIn => 1,
        PollOut => 2,
        PollMessage => 3,
        PollError => 4,
        PollPriority => 5,
        PollHangup => 6,
    }
}

#[repr(C)]
struct Ucontext {
    onstack: i32,
    sigmask: u32,
    stack: Stack,
    link: u64,
    mcsize: u64,
    mcontext: *mut Mcontext,
}

/// Darwin's `mcontext64`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Mcontext {
    es: ExceptionState,
    ss: ThreadState,
    fs: FloatState,
}

#[cfg(target_arch = "x86_64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct ExceptionState {
    trapno: u16,
    cpu: u16,
    err: u32,
    faultvaddr: u64,
}

#[cfg(target_arch = "x86_64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct ThreadState {
    rax: u64,
    rbx: u64,
    rcx: u64,
    rdx: u64,
    rdi: u64,
    rsi: u64,
    rbp: u64,
    rsp: u64,
    r: [u64; 8],
    rip: u64,
    rflags: u64,
    cs: u64,
    fs: u64,
    gs: u64,
}

#[cfg(target_arch = "x86_64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct FloatState {
    reserved: [i32; 2],
    fxsave: [u8; 512],
    reserved1: i32,
}

#[cfg(target_arch = "x86_64")]
impl Mcontext {
    fn from_machine(m: &Machine) -> Mcontext {
        let g = &m.gpr;
        Mcontext {
            es: ExceptionState {
                trapno: m.trapno as u16,
                cpu: 0,
                err: m.err as u32,
                faultvaddr: m.fault_address,
            },
            ss: ThreadState {
                rax: g[0],
                rcx: g[1],
                rdx: g[2],
                rbx: g[3],
                rsp: g[4],
                rbp: g[5],
                rsi: g[6],
                rdi: g[7],
                r: g[8..].try_into().unwrap(),
                rip: m.rip,
                rflags: m.rflags,
                cs: m.cs.into(),
                fs: m.fs.into(),
                gs: m.gs.into(),
            },
            fs: FloatState {
                reserved: [0; 2],
                fxsave: m.fxsave.unwrap_or([0; 512]),
                reserved1: 0,
            },
        }
    }

    /// What a handler may change, back into `m`.
    fn apply(&self, m: &mut Machine) {
        let ss = &self.ss;
        m.gpr[..8].copy_from_slice(&[
            ss.rax, ss.rcx, ss.rdx, ss.rbx, ss.rsp, ss.rbp, ss.rsi, ss.rdi,
        ]);
        m.gpr[8..].copy_from_slice(&ss.r);
        m.rip = ss.rip;
        m.rflags = ss.rflags;
        if let Some(fxsave) = m.fxsave.as_mut() {
            *fxsave = self.fs.fxsave;
        }
    }

    #[cfg(test)]
    fn pc(&mut self) -> &mut u64 {
        &mut self.ss.rip
    }

    #[cfg(test)]
    fn result(&mut self) -> &mut u64 {
        &mut self.ss.rax
    }
}

#[cfg(target_arch = "aarch64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct ExceptionState {
    far: u64,
    esr: u32,
    exception: u32,
}

#[cfg(target_arch = "aarch64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct ThreadState {
    x: [u64; 29],
    fp: u64,
    lr: u64,
    sp: u64,
    pc: u64,
    cpsr: u32,
    pad: u32,
}

#[cfg(target_arch = "aarch64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct FloatState {
    v: [u128; 32],
    fpsr: u32,
    fpcr: u32,
}

#[cfg(target_arch = "aarch64")]
impl Mcontext {
    fn from_machine(m: &Machine) -> Mcontext {
        let fp = m.fpsimd.unwrap_or(starnix::signal::Fpsimd {
            v: [0; 32],
            fpsr: 0,
            fpcr: 0,
        });
        Mcontext {
            es: ExceptionState {
                far: m.fault_address,
                esr: m.esr as u32,
                exception: 0,
            },
            ss: ThreadState {
                x: m.x[..29].try_into().unwrap(),
                fp: m.x[29],
                lr: m.x[30],
                sp: m.sp,
                pc: m.pc,
                cpsr: m.pstate as u32,
                pad: 0,
            },
            fs: FloatState {
                v: fp.v,
                fpsr: fp.fpsr,
                fpcr: fp.fpcr,
            },
        }
    }

    /// What a handler may change, back into `m`.
    fn apply(&self, m: &mut Machine) {
        let ss = &self.ss;
        m.x[..29].copy_from_slice(&ss.x);
        m.x[29] = ss.fp;
        m.x[30] = ss.lr;
        m.sp = ss.sp;
        m.pc = ss.pc;
        m.pstate = (m.pstate & !0xffff_ffff) | u64::from(ss.cpsr);
        if let Some(fp) = m.fpsimd.as_mut() {
            fp.v = self.fs.v;
            fp.fpsr = self.fs.fpsr;
            fp.fpcr = self.fs.fpcr;
        }
    }

    #[cfg(test)]
    fn pc(&mut self) -> &mut u64 {
        &mut self.ss.pc
    }

    #[cfg(test)]
    fn result(&mut self) -> &mut u64 {
        &mut self.ss.x[0]
    }
}

const _: () = {
    assert!(size_of::<Disposition>() == 16);
    assert!(size_of::<Stack>() == 24);
    assert!(size_of::<Siginfo>() == 104);
    assert!(size_of::<Ucontext>() == 56);
    #[cfg(target_arch = "x86_64")]
    assert!(size_of::<Mcontext>() == 712);
    #[cfg(target_arch = "aarch64")]
    assert!(size_of::<Mcontext>() == 816);
};

/// The guest's handler for each Darwin signal, entered through [`deliver`].
/// Written before the host disposition changes, so a signal arriving in
/// between finds the handler it is about to be sent to.
static GUEST_HANDLERS: [AtomicU64; 32] = [const { AtomicU64::new(0) }; 32];
static GUEST_SIGINFO: [AtomicBool; 32] = [const { AtomicBool::new(false) }; 32];

/// Signals that reached [`deliver`], per Darwin signal: what `EVFILT_SIGNAL`
/// reports, which counts attempts to deliver whatever the disposition is.
static DELIVERED: [AtomicU64; 32] = [const { AtomicU64::new(0) }; 32];

/// How many `EVFILT_SIGNAL` registrations watch each Darwin signal. While one
/// does, the host handler is [`deliver`] even for `SIG_DFL` and `SIG_IGN`,
/// because a signal the host ignores is otherwise invisible.
static OBSERVERS: [AtomicU32; 32] = [const { AtomicU32::new(0) }; 32];

/// Serializes [`observe`], which changes host dispositions in two steps.
static OBSERVE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// How many times `darwin` has been delivered to this process while observed.
pub fn delivered(darwin: i32) -> u64 {
    DELIVERED[darwin as usize & 31].load(Ordering::Acquire)
}

fn observed(slot: usize) -> bool {
    OBSERVERS[slot].load(Ordering::Acquire) != 0
}

/// Signals whose default action is to do nothing (`SIGIO` is Darwin's, which
/// differs from Linux's).
fn default_ignored(darwin: i32) -> bool {
    use signum::*;
    matches!(
        darwin,
        SIGCHLD | SIGURG | SIGWINCH | SIGCONT | SIGIO | SIGINFO
    )
}

/// Start (`on`) or stop watching `darwin` for `EVFILT_SIGNAL`, answering with
/// the host `errno` on failure.
pub fn observe(darwin: i32, on: bool) -> Result<(), c_int> {
    let Some(host_signum) = signal_to_host(darwin) else {
        return Err(libc::EINVAL);
    };
    if retained(darwin) {
        return Err(libc::EINVAL);
    }
    let slot = darwin as usize;
    let _guard = OBSERVE.lock().unwrap_or_else(|e| e.into_inner());
    if !on {
        let previous = OBSERVERS[slot].fetch_sub(1, Ordering::AcqRel);
        if previous != 1 {
            return Ok(());
        }
        // Back to what the guest asked for, if that was not a handler.
        let handler = GUEST_HANDLERS[slot].load(Ordering::Acquire);
        // SAFETY: a zeroed `struct sigaction` is valid; only read.
        let mut old: libc::sigaction = unsafe { core::mem::zeroed() };
        // SAFETY: a live structure to fill in.
        let queried = unsafe { libc::sigaction(host_signum, core::ptr::null(), &mut old) };
        if queried == 0 && delivers(old.sa_sigaction) && is_sentinel(handler) {
            let restored = if handler == 1 {
                libc::SIG_IGN
            } else {
                libc::SIG_DFL
            };
            // SAFETY: a disposition value rather than a function.
            unsafe { libc::signal(host_signum, restored) };
        }
        return Ok(());
    }
    if OBSERVERS[slot].load(Ordering::Acquire) == 0 {
        // SAFETY: as above.
        let mut old: libc::sigaction = unsafe { core::mem::zeroed() };
        // SAFETY: a live structure to fill in.
        if unsafe { libc::sigaction(host_signum, core::ptr::null(), &mut old) } != 0 {
            return Err(crate::errno::get());
        }
        if !delivers(old.sa_sigaction) {
            // A host handler of the runtime's own cannot be wrapped, and
            // pretending to watch it would never fire.
            let handler = match old.sa_sigaction {
                libc::SIG_DFL => 0,
                libc::SIG_IGN => 1,
                _ => return Err(libc::EINVAL),
            };
            GUEST_SIGINFO[slot].store(false, Ordering::Release);
            GUEST_HANDLERS[slot].store(handler, Ordering::Release);
            // SAFETY: as above.
            let mut act: libc::sigaction = unsafe { core::mem::zeroed() };
            act.sa_sigaction = deliver as *const () as libc::sighandler_t;
            act.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
            // SAFETY: two live structures.
            if unsafe { libc::sigaction(host_signum, &act, core::ptr::null_mut()) } != 0 {
                return Err(crate::errno::get());
            }
        }
    }
    OBSERVERS[slot].fetch_add(1, Ordering::AcqRel);
    Ok(())
}

type Handler1 = extern "C" fn(c_int);
type Handler3 = extern "C" fn(c_int, *mut Siginfo, *mut Ucontext);

/// A host fault the runtime resolves itself, asked before any guest handler sees
/// the signal: a store to a page the code cache write-protected. Zero when unset.
static FAULT_FILTER: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Have [`deliver`] ask `filter` about every `SIGSEGV` and `SIGBUS` first, and
/// return without delivering when it answers true. `filter` must be
/// async-signal-safe.
pub fn set_fault_filter(filter: fn(c_int, *mut libc::siginfo_t) -> bool) {
    FAULT_FILTER.store(filter as usize, Ordering::Release);
}

fn filtered(host: c_int, info: *mut libc::siginfo_t) -> bool {
    if host != libc::SIGSEGV && host != libc::SIGBUS {
        return false;
    }
    type Filter = fn(c_int, *mut libc::siginfo_t) -> bool;
    match FAULT_FILTER.load(Ordering::Acquire) {
        0 => false,
        f => {
            // SAFETY: only ever stored from a `Filter` by `set_fault_filter`.
            let filter: Filter = unsafe { core::mem::transmute::<usize, Filter>(f) };
            filter(host, info)
        }
    }
}

/// What the kernel enters for every guest handler: the signal number, and for
/// an `SA_SIGINFO` handler both structures, rebuilt in Darwin's numbering and
/// layout. Async-signal-safe: no allocation, no locks.
extern "C" fn deliver(host: c_int, info: *mut libc::siginfo_t, context: *mut c_void) {
    if filtered(host, info) {
        return;
    }
    let Some(darwin) = signal_from_host(host) else {
        return;
    };
    let slot = darwin as usize;
    DELIVERED[slot].fetch_add(1, Ordering::AcqRel);
    crate::kqueue::signalled();
    let handler = GUEST_HANDLERS[slot].load(Ordering::Acquire);
    if is_sentinel(handler) {
        // Only here because a kqueue watches it: the default action still
        // applies, and is replayed now that the watcher has been told.
        if handler == 0 && observed(slot) && !default_ignored(darwin) {
            // SAFETY: both are async-signal-safe.
            unsafe {
                libc::signal(host, libc::SIG_DFL);
                libc::raise(host);
            }
        }
        return;
    }
    if GUEST_SIGINFO[slot].load(Ordering::Acquire) {
        // SAFETY: installed as a three-argument handler; the pointers are the
        // kernel's own.
        unsafe {
            deliver_siginfo(
                core::mem::transmute::<usize, Handler3>(handler as usize),
                darwin,
                info,
                context,
            )
        }
    } else {
        // SAFETY: installed as a one-argument handler.
        let handler: Handler1 = unsafe { core::mem::transmute(handler as usize) };
        handler(darwin);
    }
}

/// # Safety
///
/// `info` and `raw` are the kernel's arguments to the running handler.
unsafe fn deliver_siginfo(
    handler: Handler3,
    darwin: i32,
    info: *const libc::siginfo_t,
    raw: *mut c_void,
) {
    let info = match info.is_null() {
        true => Info::default(),
        // SAFETY: the caller's guarantee.
        false => unsafe { starnix::signal::info(info) },
    };
    let mut siginfo = Siginfo::new(darwin, &info);
    // SAFETY: the caller's guarantee.
    let Some(mut host) = (unsafe { starnix::signal::context(raw) }) else {
        handler(darwin, &mut siginfo, core::ptr::null_mut());
        return;
    };

    let mut mcontext = Mcontext::from_machine(&host.machine);
    let mask = mask_from_host(&host.mask);
    let mut ucontext = Ucontext {
        onstack: (host.stack.ss_flags & libc::SS_ONSTACK != 0) as i32,
        sigmask: mask,
        stack: Stack::from_host(&host.stack),
        link: 0,
        mcsize: size_of::<Mcontext>() as u64,
        mcontext: &raw mut mcontext,
    };
    handler(darwin, &mut siginfo, &mut ucontext);

    // Only a changed context is written back: the round trip is lossy (upper
    // `pstate` bits, segment selectors) and an unchanged one must not be.
    let before = host.machine;
    mcontext.apply(&mut host.machine);
    let remask = ucontext.sigmask != mask;
    if remask {
        mask_into_host(ucontext.sigmask, &mut host.mask);
    }
    if remask || host.machine != before {
        // SAFETY: the caller's guarantee; the handler has not returned.
        unsafe { starnix::signal::set_context(raw, &host) };
    }
}

fn delivers(host_handler: libc::sighandler_t) -> bool {
    host_handler == deliver as *const () as libc::sighandler_t
}

/// A handler the host installed, as the guest sees it: the default.
///
/// It is the runtime's or a host library's, takes Linux's `siginfo_t` and
/// context, and a Darwin process would not have had one. A guest that chains
/// to it - HotSpot does, for a fault it does not own - would pass it Darwin's.
fn guest_view(host_handler: libc::sighandler_t) -> u64 {
    match is_sentinel(host_handler as u64) {
        true => host_handler as u64,
        false => libc::SIG_DFL as u64,
    }
}

/// Install `handler` for `slot` behind [`deliver`], returning what to give
/// the host.
fn route(slot: usize, handler: u64, siginfo: bool) -> libc::sighandler_t {
    if is_sentinel(handler) {
        if !observed(slot) {
            return handler as libc::sighandler_t;
        }
        // A watched signal stays routed through `deliver`.
        GUEST_SIGINFO[slot].store(false, Ordering::Release);
        GUEST_HANDLERS[slot].store(handler, Ordering::Release);
        return deliver as *const () as libc::sighandler_t;
    }
    GUEST_SIGINFO[slot].store(siginfo, Ordering::Release);
    GUEST_HANDLERS[slot].store(handler, Ordering::Release);
    deliver as *const () as libc::sighandler_t
}

/// `sigaction` with Darwin's signal number, mask and flags.
///
/// # Safety
///
/// `act.handler`, when not a sentinel, is a function this process can call as
/// a signal handler (a trampoline, not the guest's own address, when emulated).
pub unsafe fn sigaction(
    darwin_signum: i32,
    act: Option<Disposition>,
    old: Option<&mut Disposition>,
) -> c_int {
    if unraisable(darwin_signum) || retained(darwin_signum) {
        let pretended = &PRETENDED[darwin_signum as usize];
        let previous = pretended.get();
        if let Some(act) = act {
            pretended.set(act);
        }
        if let Some(old) = old {
            *old = previous;
        }
        return 0;
    }
    let Some(host_signum) = signal_to_host(darwin_signum) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };

    let slot = darwin_signum as usize;
    let previous_handler = GUEST_HANDLERS[slot].load(Ordering::Acquire);
    let previous_siginfo = GUEST_SIGINFO[slot].load(Ordering::Acquire);

    let host_act = act.map(|act| {
        // SAFETY: zeroed is a valid `struct sigaction` on every host.
        let mut host: libc::sigaction = unsafe { core::mem::zeroed() };
        host.sa_sigaction = route(slot, act.handler, act.flags & SA_SIGINFO != 0);
        host.sa_flags = flags_to_host(act.flags);
        if delivers(host.sa_sigaction) {
            host.sa_flags |= libc::SA_SIGINFO;
        }
        host.sa_mask = mask_to_host(act.mask);
        host
    });
    // SAFETY: as above.
    let mut host_old: libc::sigaction = unsafe { core::mem::zeroed() };

    // SAFETY: two live host structures, or null.
    let rc = unsafe {
        libc::sigaction(
            host_signum,
            host_act
                .as_ref()
                .map_or(core::ptr::null(), |a| a as *const _),
            match old {
                Some(_) => &mut host_old,
                None => core::ptr::null_mut(),
            },
        )
    };

    if rc == 0
        && let Some(old) = old
    {
        let through_us = delivers(host_old.sa_sigaction);
        *old = match through_us {
            true => {
                let mut flags = flags_from_host(host_old.sa_flags);
                if !previous_siginfo {
                    flags &= !SA_SIGINFO;
                }
                Disposition {
                    handler: previous_handler,
                    mask: mask_from_host(&host_old.sa_mask),
                    flags,
                }
            }
            false if is_sentinel(host_old.sa_sigaction as u64) => Disposition {
                handler: host_old.sa_sigaction as u64,
                mask: mask_from_host(&host_old.sa_mask),
                flags: flags_from_host(host_old.sa_flags),
            },
            false => Disposition {
                handler: guest_view(host_old.sa_sigaction),
                ..Disposition::default()
            },
        };
    }
    rc
}

/// `signal` with Darwin's signal number. `SIG_ERR` for one this host has not got.
///
/// # Safety
///
/// As [`sigaction`].
pub unsafe fn signal(darwin_signum: i32, handler: u64) -> u64 {
    if unraisable(darwin_signum) || retained(darwin_signum) {
        let pretended = &PRETENDED[darwin_signum as usize];
        let previous = pretended.get();
        pretended.set(Disposition {
            handler,
            ..previous
        });
        return previous.handler;
    }
    let Some(host_signum) = signal_to_host(darwin_signum) else {
        crate::errno::set(libc::EINVAL);
        return u64::MAX;
    };
    let slot = darwin_signum as usize;
    let previous = GUEST_HANDLERS[slot].load(Ordering::Acquire);
    let installed = route(slot, handler, false);
    // SAFETY: the caller's obligation, or [`deliver`].
    let old = unsafe { libc::signal(host_signum, installed) };
    match delivers(old) {
        true => previous,
        false => guest_view(old),
    }
}

/// The faulting address `info` reports.
///
/// # Safety
///
/// `info` is a live `siginfo_t`, as the kernel passes one to a handler.
pub unsafe fn fault_address(info: &libc::siginfo_t) -> u64 {
    // SAFETY: the caller's guarantee.
    unsafe { starnix::signal::info(info) }.addr as u64
}

/// The program counter the kernel interrupted, or `None` where it cannot be
/// read. Never faults on a null pointer: a fault reporter that faults again
/// destroys the evidence it was called to preserve.
///
/// # Safety
///
/// `context` is null or the third argument of an `SA_SIGINFO` handler.
pub unsafe fn interrupted_pc(context: *mut c_void) -> Option<u64> {
    // SAFETY: the caller's guarantee.
    unsafe { starnix::signal::context(context) }.map(|c| c.machine.pc())
}

/// The entry points a guest binds to.
pub mod entry {
    use std::ffi::{CStr, c_char, c_int, c_void};

    use super::*;

    /// A signal this host has not got: `EINVAL` rather than a neighbour.
    fn no_such_signal() -> c_int {
        crate::errno::set(libc::EINVAL);
        -1
    }

    /// Darwin's `SIG_BLOCK` is 1, which is Linux's `SIG_UNBLOCK`.
    fn how_to_host(darwin: c_int) -> Option<c_int> {
        Some(match darwin {
            1 => libc::SIG_BLOCK,
            2 => libc::SIG_UNBLOCK,
            3 => libc::SIG_SETMASK,
            _ => return None,
        })
    }

    /// # Safety
    ///
    /// `handler` is a sentinel or a function this process can call as a signal
    /// handler.
    pub unsafe extern "C" fn signal_(signum: c_int, handler: u64) -> u64 {
        // SAFETY: the caller's obligation.
        unsafe { super::signal(signum, handler) }
    }

    /// # Safety
    ///
    /// `act` and `old` are Darwin `struct sigaction`s, or null.
    pub unsafe extern "C" fn sigaction_(
        signum: c_int,
        act: *const Disposition,
        old: *mut Disposition,
    ) -> c_int {
        // SAFETY: the caller's structure, if it passed one.
        let wanted = (!act.is_null()).then(|| unsafe { act.read_unaligned() });
        let mut previous = Disposition::default();
        // SAFETY: under the native backend the guest's handler is host code.
        let rc = unsafe { super::sigaction(signum, wanted, Some(&mut previous)) };
        if rc == 0 && !old.is_null() {
            // SAFETY: the caller's structure.
            unsafe { old.write_unaligned(previous) };
        }
        rc
    }

    /// # Safety
    ///
    /// `set` and `oset` are Darwin `sigset_t`s, or null.
    pub unsafe extern "C" fn sigprocmask_(how: c_int, set: *const u32, oset: *mut u32) -> c_int {
        let wanted = match set.is_null() {
            true => None,
            false => match how_to_host(how) {
                // SAFETY: the caller's word.
                Some(how) => Some((how, mask_to_host(unsafe { set.read_unaligned() }))),
                None => return no_such_signal(),
            },
        };
        // SAFETY: written before it is read.
        let mut previous: libc::sigset_t = unsafe { core::mem::zeroed() };
        // SAFETY: live host sets, or null. With no set, `how` is ignored.
        let rc = unsafe {
            libc::sigprocmask(
                wanted.as_ref().map_or(libc::SIG_SETMASK, |(how, _)| *how),
                wanted
                    .as_ref()
                    .map_or(core::ptr::null(), |(_, set)| set as *const _),
                &mut previous,
            )
        };
        if rc == 0 && !oset.is_null() {
            // SAFETY: the caller's word.
            unsafe { oset.write_unaligned(mask_from_host(&previous)) };
        }
        rc
    }

    /// Returns its error rather than setting `errno`, so the number must be
    /// Darwin's.
    ///
    /// # Safety
    ///
    /// As [`sigprocmask_`].
    pub unsafe extern "C" fn pthread_sigmask_(
        how: c_int,
        set: *const u32,
        oset: *mut u32,
    ) -> c_int {
        // SAFETY: the same obligation.
        match unsafe { sigprocmask_(how, set, oset) } {
            0 => 0,
            _ => crate::errno::from_host(crate::errno::get()),
        }
    }

    /// Darwin's `MINSIGSTKSZ`. Accepting Linux's 2 KiB would let a later
    /// handler overflow a stack Darwin would have refused.
    const MINSIGSTKSZ: u64 = 32 * 1024;

    fn read_stack(stack: Stack) -> Result<libc::stack_t, c_int> {
        if !matches!(stack.flags, 0 | SS_DISABLE) {
            return Err(libc::EINVAL);
        }
        if stack.flags == 0 && stack.size < MINSIGSTKSZ {
            return Err(libc::ENOMEM);
        }
        Ok(stack.to_host())
    }

    /// # Safety
    ///
    /// `ss` and `oss` are Darwin `stack_t`s, or null.
    pub unsafe extern "C" fn sigaltstack_(ss: *const c_void, oss: *mut c_void) -> c_int {
        let wanted = match ss.is_null() {
            true => None,
            // SAFETY: the caller's structure.
            false => match read_stack(unsafe { ss.cast::<Stack>().read_unaligned() }) {
                Ok(stack) => Some(stack),
                Err(error) => {
                    crate::errno::set(error);
                    return -1;
                }
            },
        };
        let (rc, previous) = match altstack_retained() {
            true => {
                let previous = pretended_altstack();
                if let Some(wanted) = &wanted {
                    set_pretended_altstack(wanted);
                }
                (0, previous)
            }
            false => {
                // SAFETY: written before it is read.
                let mut previous: libc::stack_t = unsafe { core::mem::zeroed() };
                // SAFETY: live host structures, or null.
                let rc = unsafe {
                    libc::sigaltstack(
                        wanted.as_ref().map_or(core::ptr::null(), |s| s as *const _),
                        &mut previous,
                    )
                };
                (rc, previous)
            }
        };
        if rc == 0 && !oss.is_null() {
            // SAFETY: the caller's structure.
            unsafe {
                oss.cast::<Stack>()
                    .write_unaligned(Stack::from_host(&previous))
            };
        }
        rc
    }

    pub extern "C" fn raise_(signum: c_int) -> c_int {
        match signal_to_host(signum) {
            // SAFETY: a signal this host has.
            Some(host) => unsafe { libc::raise(host) },
            None => no_such_signal(),
        }
    }

    /// Signal zero is the existence probe, not a number to translate.
    pub extern "C" fn kill_(pid: c_int, signum: c_int) -> c_int {
        let host = match signum {
            0 => 0,
            _ => match signal_to_host(signum) {
                Some(host) => host,
                None => return no_such_signal(),
            },
        };
        // Broadcasts and process groups are the host's; one process may be
        // launchd, which the guest calls 1.
        let pid = match pid > 0 {
            true => crate::process::host_pid(pid),
            false => pid,
        };
        // SAFETY: a plain system call.
        unsafe { libc::kill(pid, host) }
    }

    pub extern "C" fn killpg_(pgrp: c_int, signum: c_int) -> c_int {
        let host = match signum {
            0 => 0,
            _ => match signal_to_host(signum) {
                Some(host) => host,
                None => return no_such_signal(),
            },
        };
        // SAFETY: a plain system call.
        unsafe { libc::killpg(pgrp, host) }
    }

    // A Darwin `sigset_t` is one word numbered by Darwin's signals, so these
    // are pure bit arithmetic. Glibc's would write 128 bytes into it.

    fn bit(signum: c_int) -> Option<u32> {
        (1..=31).contains(&signum).then(|| darwin_bit(signum))
    }

    /// # Safety
    ///
    /// `set` is a writable Darwin `sigset_t`, or null.
    pub unsafe extern "C" fn sigemptyset_(set: *mut u32) -> c_int {
        if set.is_null() {
            return -1;
        }
        // SAFETY: the caller's word.
        unsafe { set.write_unaligned(0) };
        0
    }

    /// # Safety
    ///
    /// As [`sigemptyset_`].
    pub unsafe extern "C" fn sigfillset_(set: *mut u32) -> c_int {
        if set.is_null() {
            return -1;
        }
        // SAFETY: the caller's word.
        unsafe { set.write_unaligned(!0) };
        0
    }

    /// # Safety
    ///
    /// As [`sigemptyset_`].
    pub unsafe extern "C" fn sigaddset_(set: *mut u32, signum: c_int) -> c_int {
        let Some(bit) = bit(signum) else {
            return no_such_signal();
        };
        if set.is_null() {
            return -1;
        }
        // SAFETY: the caller's word.
        unsafe { set.write_unaligned(set.read_unaligned() | bit) };
        0
    }

    /// # Safety
    ///
    /// As [`sigemptyset_`].
    pub unsafe extern "C" fn sigdelset_(set: *mut u32, signum: c_int) -> c_int {
        let Some(bit) = bit(signum) else {
            return no_such_signal();
        };
        if set.is_null() {
            return -1;
        }
        // SAFETY: the caller's word.
        unsafe { set.write_unaligned(set.read_unaligned() & !bit) };
        0
    }

    /// # Safety
    ///
    /// `set` is a readable Darwin `sigset_t`, or null.
    pub unsafe extern "C" fn sigismember_(set: *const u32, signum: c_int) -> c_int {
        let Some(bit) = bit(signum) else {
            return no_such_signal();
        };
        if set.is_null() {
            return -1;
        }
        // SAFETY: the caller's word.
        (unsafe { set.read_unaligned() } & bit != 0) as c_int
    }

    /// # Safety
    ///
    /// As [`sigemptyset_`].
    pub unsafe extern "C" fn sigpending_(set: *mut u32) -> c_int {
        if set.is_null() {
            crate::errno::set(libc::EFAULT);
            return -1;
        }
        // SAFETY: written before it is read.
        let mut host: libc::sigset_t = unsafe { core::mem::zeroed() };
        // SAFETY: a live host set.
        let rc = unsafe { libc::sigpending(&mut host) };
        if rc == 0 {
            // SAFETY: the caller's word.
            unsafe { set.write_unaligned(mask_from_host(&host)) };
        }
        rc
    }

    /// # Safety
    ///
    /// `mask` is a readable Darwin `sigset_t`, or null.
    pub unsafe extern "C" fn sigsuspend_(mask: *const u32) -> c_int {
        if mask.is_null() {
            crate::errno::set(libc::EFAULT);
            return -1;
        }
        // SAFETY: the caller's word.
        let host = mask_to_host(unsafe { mask.read_unaligned() });
        // SAFETY: a live host set.
        unsafe { libc::sigsuspend(&host) }
    }

    /// The argument is Darwin's number, so it is translated before the host's
    /// table is asked.
    pub extern "C" fn strsignal_(signum: c_int) -> *const c_char {
        match signal_to_host(signum) {
            // SAFETY: storage the caller neither owns nor frees.
            Some(host) => unsafe { libc::strsignal(host) },
            None => match signum {
                signum::SIGEMT => c"EMT trap",
                signum::SIGINFO => c"Information request",
                _ => c"Unknown signal",
            }
            .as_ptr(),
        }
    }

    /// # Safety
    ///
    /// `prefix` is a NUL-terminated string, or null.
    pub unsafe extern "C" fn psignal_(signum: c_int, prefix: *const c_char) {
        // SAFETY: NUL-terminated storage of this module's or the host's.
        let described = unsafe { CStr::from_ptr(strsignal_(signum)) }.to_string_lossy();
        match prefix.is_null() {
            true => eprintln!("{described}"),
            false => {
                // SAFETY: the caller's string.
                let prefix = unsafe { CStr::from_ptr(prefix) }.to_string_lossy();
                eprintln!("{prefix}: {described}");
            }
        }
    }

    /// Darwin's `sys_signame`, indexed by Darwin's numbering.
    #[repr(transparent)]
    pub struct SignalNames([*const c_char; 32]);

    // SAFETY: pointers to string literals, never written.
    unsafe impl Sync for SignalNames {}

    pub static SYS_SIGNAME: SignalNames = SignalNames([
        c"Signal 0".as_ptr(),
        c"hup".as_ptr(),
        c"int".as_ptr(),
        c"quit".as_ptr(),
        c"ill".as_ptr(),
        c"trap".as_ptr(),
        c"abrt".as_ptr(),
        c"emt".as_ptr(),
        c"fpe".as_ptr(),
        c"kill".as_ptr(),
        c"bus".as_ptr(),
        c"segv".as_ptr(),
        c"sys".as_ptr(),
        c"pipe".as_ptr(),
        c"alrm".as_ptr(),
        c"term".as_ptr(),
        c"urg".as_ptr(),
        c"stop".as_ptr(),
        c"tstp".as_ptr(),
        c"cont".as_ptr(),
        c"chld".as_ptr(),
        c"ttin".as_ptr(),
        c"ttou".as_ptr(),
        c"io".as_ptr(),
        c"xcpu".as_ptr(),
        c"xfsz".as_ptr(),
        c"vtalrm".as_ptr(),
        c"prof".as_ptr(),
        c"winch".as_ptr(),
        c"info".as_ptr(),
        c"usr1".as_ptr(),
        c"usr2".as_ptr(),
    ]);

    exports::exports! {
        functions {
            "signal" => signal_,
            "sigaction" => sigaction_,
            "sigprocmask" => sigprocmask_,
            "pthread_sigmask" => pthread_sigmask_,
            "sigaltstack" => sigaltstack_,
            "sigsuspend" => sigsuspend_,
            "sigpending" => sigpending_,
            "raise" => raise_,
            "kill" => kill_,
            "killpg" => killpg_,
            "sigemptyset" => sigemptyset_,
            "sigfillset" => sigfillset_,
            "sigaddset" => sigaddset_,
            "sigdelset" => sigdelset_,
            "sigismember" => sigismember_,
            "strsignal" => strsignal_,
            "psignal" => psignal_,
        }
        constants {
            // Data: the guest indexes the array at this address.
            "sys_signame" => &raw const SYS_SIGNAME as u64,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_sigprocmask_how_values_are_translated_rather_than_forwarded() {
            assert_eq!(how_to_host(1), Some(libc::SIG_BLOCK));
            assert_eq!(how_to_host(2), Some(libc::SIG_UNBLOCK));
            assert_eq!(how_to_host(3), Some(libc::SIG_SETMASK));
            assert_eq!(how_to_host(0), None);
        }

        #[test]
        fn an_alternate_stack_is_reordered_rather_than_passed_through() {
            let darwin = Stack {
                sp: 0x1234_5000,
                size: 64 * 1024,
                flags: SS_DISABLE,
            };
            let host = read_stack(darwin).expect("a valid disabled stack");
            assert_eq!(host.ss_sp as usize, 0x1234_5000);
            assert_eq!(host.ss_size, 64 * 1024);
            assert_eq!(host.ss_flags, libc::SS_DISABLE);
            assert_eq!(Stack::from_host(&host), darwin);
        }

        #[test]
        fn alternate_stack_input_is_validated_to_darwins_rules() {
            let mut darwin = Stack {
                sp: 0x1234_5000,
                size: 2048,
                flags: 0,
            };
            // SAFETY: a complete Darwin `stack_t`.
            assert_eq!(
                unsafe { sigaltstack_((&raw const darwin).cast(), core::ptr::null_mut()) },
                -1
            );
            assert_eq!(crate::errno::get(), libc::ENOMEM);

            darwin.size = MINSIGSTKSZ;
            darwin.flags = 2;
            // SAFETY: as above.
            assert_eq!(
                unsafe { sigaltstack_((&raw const darwin).cast(), core::ptr::null_mut()) },
                -1
            );
            assert_eq!(crate::errno::get(), libc::EINVAL);
            crate::errno::set(0);
        }

        #[test]
        fn the_set_operations_touch_four_bytes_and_number_the_bits_darwins_way() {
            let mut set = [0u32, 0xdead_beef];
            // SAFETY: a live word, and the second element is the guard.
            unsafe {
                assert_eq!(sigfillset_(set.as_mut_ptr()), 0);
                assert_eq!(sigdelset_(set.as_mut_ptr(), signum::SIGUSR1), 0);
                assert_eq!(sigismember_(set.as_ptr(), signum::SIGUSR1), 0);
                assert_eq!(sigismember_(set.as_ptr(), signum::SIGCHLD), 1);
                assert_eq!(sigemptyset_(set.as_mut_ptr()), 0);
                assert_eq!(sigaddset_(set.as_mut_ptr(), signum::SIGUSR1), 0);
            }
            assert_eq!(set[0], 1 << (signum::SIGUSR1 - 1));
            assert_eq!(set[1], 0xdead_beef, "nothing past the caller's word");
        }

        #[test]
        fn signal_32_is_not_accepted_as_a_darwin_signal() {
            let mut set = 0u32;
            // SAFETY: a live word.
            assert_eq!(unsafe { sigaddset_(&mut set, 32) }, -1);
            assert_eq!(crate::errno::get(), libc::EINVAL);
            assert_eq!(set, 0);
            crate::errno::set(0);
        }

        #[test]
        fn signal_zero_probes_existence_rather_than_being_translated() {
            let me = crate::process::host_self();
            assert_eq!(kill_(me, 0), 0);
            assert_eq!(kill_(me, 99), -1, "not a signal");
        }

        #[test]
        fn a_signal_this_host_has_not_got_is_refused_rather_than_aimed_at_a_neighbour() {
            if signal_to_host(signum::SIGINFO).is_some() {
                return;
            }
            assert_eq!(raise_(signum::SIGINFO), -1);
            // SAFETY: this module's own string.
            let named = unsafe { CStr::from_ptr(strsignal_(signum::SIGINFO)) };
            assert_eq!(named.to_str().unwrap(), "Information request");
        }

        #[test]
        fn a_disposition_occupies_darwins_sixteen_bytes_and_no_more() {
            let mut bytes = [Disposition::default(); 2];
            bytes[1].handler = 0xAAAA;
            let held = Disposition {
                handler: SIG_IGN,
                mask: 1 << (signum::SIGINT - 1),
                flags: 0x0002,
            };
            // SAFETY: a signal nothing else uses, restored below.
            unsafe {
                assert_eq!(
                    sigaction_(signum::SIGWINCH, &held, core::ptr::null_mut()),
                    0
                );
                assert_eq!(
                    sigaction_(signum::SIGWINCH, core::ptr::null(), bytes.as_mut_ptr()),
                    0
                );
                signal_(signum::SIGWINCH, 0);
            }
            assert_eq!(bytes[0], held);
            assert_eq!(
                bytes[1].handler, 0xAAAA,
                "nothing past the guest's structure"
            );
        }

        const SIG_IGN: u64 = 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicI32;

    /// Tests that change real dispositions run one at a time.
    static REAL: Mutex<()> = Mutex::new(());

    fn real() -> std::sync::MutexGuard<'static, ()> {
        REAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    extern "C" fn host_handler(_: c_int) {}

    #[test]
    fn a_host_installed_handler_reads_back_as_the_default() {
        let _real = real();
        let host = signal_to_host(signum::SIGURG).unwrap();
        // SAFETY: a handler that does nothing, then the old disposition put back.
        unsafe {
            let mut action: libc::sigaction = core::mem::zeroed();
            action.sa_sigaction = host_handler as *const () as libc::sighandler_t;
            let mut saved: libc::sigaction = core::mem::zeroed();
            libc::sigaction(host, &action, &mut saved);
            let mut back = Disposition::default();
            assert_eq!(sigaction(signum::SIGURG, None, Some(&mut back)), 0);
            libc::sigaction(host, &saved, core::ptr::null_mut());
            assert_eq!(back, Disposition::default());
        }
    }

    #[test]
    fn a_retained_signal_is_accepted_recorded_and_not_applied() {
        let _real = real();
        let host = signal_to_host(signum::SIGUSR2).unwrap();
        let current = || {
            // SAFETY: a query.
            unsafe {
                let mut current: libc::sigaction = core::mem::zeroed();
                libc::sigaction(host, core::ptr::null(), &mut current);
                current.sa_sigaction
            }
        };
        let before = current();

        retain(signum::SIGUSR2);
        let wanted = Disposition {
            handler: 0xdead_beef,
            ..Disposition::default()
        };
        let mut back = Disposition::default();
        // SAFETY: a retained handler is never entered.
        unsafe {
            assert_eq!(sigaction(signum::SIGUSR2, Some(wanted), None), 0);
            assert_eq!(sigaction(signum::SIGUSR2, None, Some(&mut back)), 0);
        }
        RETAINED.fetch_and(!darwin_bit(signum::SIGUSR2), Ordering::Relaxed);

        assert_eq!(current(), before, "the host's disposition was changed");
        assert_eq!(back.handler, wanted.handler);
    }

    #[test]
    fn a_mask_survives_a_static() {
        static SAVED: StaticMask = StaticMask::empty();
        // SAFETY: zeroed is a valid `sigaction`; nothing reads `SAVED`.
        unsafe {
            let mut action: libc::sigaction = core::mem::zeroed();
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaddset(&mut action.sa_mask, libc::SIGUSR2);
            SAVED.set(Mask::of(&action));
        }
        // SAFETY: as above.
        let mut other: libc::sigaction = unsafe { core::mem::zeroed() };
        SAVED.get().apply(&mut other);
        // SAFETY: a live set.
        assert_eq!(
            unsafe { libc::sigismember(&other.sa_mask, libc::SIGUSR2) },
            1
        );
        assert_eq!(
            unsafe { libc::sigismember(&other.sa_mask, libc::SIGUSR1) },
            0
        );
    }

    #[test]
    fn translation_round_trips() {
        for darwin in 1..=31 {
            if let Some(host) = signal_to_host(darwin) {
                assert_eq!(signal_from_host(host), Some(darwin), "signal {darwin}");
            }
        }
        for (darwin, _) in FLAGS {
            assert_eq!(flags_from_host(flags_to_host(darwin)), darwin);
        }
        let darwin = darwin_bit(signum::SIGUSR1) | darwin_bit(signum::SIGCHLD);
        let host = mask_to_host(darwin);
        // SAFETY: a live set.
        assert_eq!(unsafe { libc::sigismember(&host, libc::SIGUSR1) }, 1);
        assert_eq!(mask_from_host(&host), darwin);
    }

    #[test]
    fn a_null_context_has_no_program_counter() {
        // SAFETY: null is accepted.
        assert_eq!(unsafe { interrupted_pc(core::ptr::null_mut()) }, None);
    }

    #[test]
    fn retained_alternate_stack_state_is_per_thread_and_initially_disabled() {
        set_pretended_altstack(&libc::stack_t {
            ss_sp: 0x1234usize as *mut c_void,
            ss_size: 64 * 1024,
            ss_flags: 0,
        });
        let other = std::thread::spawn(|| {
            let initial = pretended_altstack();
            assert!(initial.ss_sp.is_null());
            assert_eq!(initial.ss_flags, libc::SS_DISABLE);
            set_pretended_altstack(&libc::stack_t {
                ss_sp: 0x5678usize as *mut c_void,
                ss_size: 32 * 1024,
                ss_flags: 0,
            });
            pretended_altstack().ss_sp as usize
        })
        .join()
        .unwrap();
        assert_eq!(other, 0x5678);
        assert_eq!(pretended_altstack().ss_sp as usize, 0x1234);
    }

    #[test]
    fn a_disposition_round_trips_through_the_host() {
        extern "C" fn handler(_: c_int) {}
        let _real = real();
        let wanted = Disposition {
            handler: handler as *const () as u64,
            mask: darwin_bit(signum::SIGUSR2) | darwin_bit(signum::SIGINT),
            flags: 0x0002,
        };
        let mut back = Disposition::default();
        // SAFETY: a real handler, and the default put back.
        unsafe {
            assert_eq!(sigaction(signum::SIGUSR1, Some(wanted), None), 0);
            assert_eq!(sigaction(signum::SIGUSR1, None, Some(&mut back)), 0);
            sigaction(signum::SIGUSR1, Some(Disposition::default()), None);
        }
        assert_eq!(back, wanted);
    }

    #[test]
    fn a_signal_this_host_lacks_is_kept_but_never_installed() {
        if !unraisable(signum::SIGINFO) {
            return;
        }
        let handler = Disposition {
            handler: 0x1234_5678,
            ..Disposition::default()
        };
        let mut back = Disposition::default();
        // SAFETY: nothing is installed for an unraisable signal.
        unsafe {
            assert_eq!(sigaction(signum::SIGINFO, Some(handler), None), 0);
            assert_eq!(sigaction(signum::SIGINFO, None, Some(&mut back)), 0);
            assert_eq!(
                sigaction(signum::SIGINFO, Some(Disposition::default()), None),
                0
            );
            assert_eq!(signal(signum::SIGEMT, 0), 0);
            assert_eq!(sigaction(40, None, None), -1);
        }
        assert_eq!(back.handler, handler.handler);
    }

    static HEARD: AtomicI32 = AtomicI32::new(0);

    extern "C" fn record(signum: c_int) {
        HEARD.store(signum, Ordering::SeqCst);
    }

    #[test]
    fn a_guest_handler_is_entered_with_darwins_signal_number() {
        let _real = real();
        let act = Disposition {
            handler: record as *const () as u64,
            ..Disposition::default()
        };
        let mut back = Disposition::default();
        // SAFETY: a handler of this test's own, and the default put back.
        unsafe {
            assert_eq!(sigaction(signum::SIGUSR1, Some(act), None), 0);
            libc::raise(signal_to_host(signum::SIGUSR1).unwrap());
            assert_eq!(sigaction(signum::SIGUSR1, None, Some(&mut back)), 0);
            assert_eq!(signal(signum::SIGUSR1, 0), act.handler);
        }
        assert_eq!(HEARD.load(Ordering::SeqCst), signum::SIGUSR1);
        assert_eq!(back.handler, act.handler);
        assert_eq!(
            back.flags & SA_SIGINFO,
            0,
            "the translator's flag is not reported"
        );
    }

    static SEEN: Mutex<Option<(Siginfo, u64, u64, u32)>> = Mutex::new(None);

    extern "C" fn record_siginfo(signum: c_int, info: *mut Siginfo, context: *mut Ucontext) {
        // SAFETY: the structures `deliver` built.
        let _ = signum;
        let seen = unsafe {
            let context = &*context;
            (
                *info,
                context.mcsize,
                (*context.mcontext).pc_value(),
                context.sigmask,
            )
        };
        if let Ok(mut slot) = SEEN.try_lock() {
            *slot = Some(seen);
        }
    }

    impl Mcontext {
        fn pc_value(&mut self) -> u64 {
            *self.pc()
        }
    }

    #[test]
    fn a_siginfo_handler_is_entered_with_darwins_structures() {
        let _real = real();
        let act = Disposition {
            handler: record_siginfo as *const () as u64,
            mask: 0,
            flags: SA_SIGINFO,
        };
        // SAFETY: a handler of this test's own on a signal whose default is
        // to be ignored, and the default put back.
        unsafe {
            assert_eq!(sigaction(signum::SIGURG, Some(act), None), 0);
            libc::raise(signal_to_host(signum::SIGURG).unwrap());
            sigaction(signum::SIGURG, Some(Disposition::default()), None);
        }
        let (info, mcsize, pc, mask) = SEEN.lock().unwrap().take().expect("the handler ran");
        assert_eq!(info.signo, signum::SIGURG);
        assert_eq!(info.code, 0x10001, "SI_USER");
        assert_eq!(info.pid, crate::process::host_self());
        assert_eq!(mcsize, size_of::<Mcontext>() as u64);
        assert_ne!(pc, 0);
        assert_eq!(
            mask & darwin_bit(signum::SIGURG),
            0,
            "the mask to restore, not the one in force"
        );
    }

    static FAULTED_AT: AtomicU64 = AtomicU64::new(0);

    /// Steps over the faulting load and gives it a result, as a JIT's
    /// guard-page handler would.
    extern "C" fn resume(_: c_int, info: *mut Siginfo, context: *mut Ucontext) {
        // SAFETY: the structures `deliver` built.
        unsafe {
            FAULTED_AT.store(
                (*info).addr | (((*info).code as u64) << 48),
                Ordering::SeqCst,
            );
            let mc = &mut *(*context).mcontext;
            *mc.pc() += FAULTING_LOAD_LENGTH;
            *mc.result() = 0x5afe;
        }
    }

    #[cfg(target_arch = "x86_64")]
    const FAULTING_LOAD_LENGTH: u64 = 3;
    #[cfg(target_arch = "aarch64")]
    const FAULTING_LOAD_LENGTH: u64 = 4;

    fn faulting_load(address: usize) -> u64 {
        let value: u64;
        // SAFETY: faults, and `resume` steps over it.
        #[cfg(target_arch = "x86_64")]
        unsafe {
            core::arch::asm!("mov rax, qword ptr [rcx]", in("rcx") address, out("rax") value);
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!("ldr x0, [x1]", in("x1") address, out("x0") value);
        }
        value
    }

    #[test]
    fn a_handler_that_changes_the_context_resumes_where_it_says() {
        let _real = real();
        let host = signal_to_host(signum::SIGSEGV).unwrap();
        // SAFETY: zeroed is valid; the original disposition is restored below.
        let mut original: libc::sigaction = unsafe { core::mem::zeroed() };
        unsafe { libc::sigaction(host, core::ptr::null(), &mut original) };
        let act = Disposition {
            handler: resume as *const () as u64,
            mask: 0,
            flags: SA_SIGINFO,
        };
        // SAFETY: `resume` handles exactly the fault below.
        let value = unsafe {
            assert_eq!(sigaction(signum::SIGSEGV, Some(act), None), 0);
            let value = faulting_load(0x18);
            libc::sigaction(host, &original, core::ptr::null_mut());
            value
        };
        assert_eq!(value, 0x5afe);
        let seen = FAULTED_AT.load(Ordering::SeqCst);
        assert_eq!(seen & 0xffff_ffff_ffff, 0x18, "si_addr");
        assert_eq!(seen >> 48, 1, "SEGV_MAPERR");
    }
}
