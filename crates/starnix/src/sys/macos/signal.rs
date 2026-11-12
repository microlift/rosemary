use core::ffi::{c_int, c_void};

use crate::signal::{Code, Context};

pub const SIGEMT: Option<c_int> = Some(libc::SIGEMT);
pub const SIGINFO: Option<c_int> = Some(libc::SIGINFO);
/// XNU reports `kill` with code 0 and still names the sender.
pub const KERNEL_CODE_HAS_SENDER: bool = true;

// `<sys/signal.h>`; the `libc` crate does not carry these for Apple targets.
const SI_USER: c_int = 0x10001;
const SI_QUEUE: c_int = 0x10002;
const SI_TIMER: c_int = 0x10003;
const SI_ASYNCIO: c_int = 0x10004;
const SI_MESGQ: c_int = 0x10005;

// Darwin's order, which is neither POSIX's listing nor Linux's.
const ILL: [Code; 8] = [
    Code::IllegalOpcode,
    Code::IllegalTrap,
    Code::PrivilegedOpcode,
    Code::IllegalOperand,
    Code::IllegalAddressing,
    Code::PrivilegedRegister,
    Code::Coprocessor,
    Code::BadStack,
];

const FPE: [Code; 8] = [
    Code::FloatDivide,
    Code::FloatOverflow,
    Code::FloatUnderflow,
    Code::FloatInexact,
    Code::FloatInvalid,
    Code::FloatSubscript,
    Code::IntegerDivide,
    Code::IntegerOverflow,
];

pub fn code(signo: c_int, code: c_int) -> Code {
    match code {
        SI_USER => Code::User,
        SI_QUEUE => Code::Queue,
        SI_TIMER => Code::Timer,
        SI_ASYNCIO => Code::AsyncIo,
        SI_MESGQ => Code::MessageQueue,
        0 => Code::Kernel,
        _ => crate::signal::by_signal(signo, code, &ILL, &FPE),
    }
}

/// `<sys/_types/_ucontext.h>`. The machine context is out of line, and the
/// `libc` crate declares neither.
#[repr(C)]
struct Ucontext {
    _onstack: c_int,
    sigmask: libc::sigset_t,
    stack: libc::stack_t,
    _link: *mut Ucontext,
    _mcsize: usize,
    mcontext: *mut Mcontext,
}

#[repr(C)]
struct Mcontext {
    es: ExceptionState,
    ss: ThreadState,
    fs: FloatState,
}

#[cfg(target_arch = "x86_64")]
mod machine {
    use super::*;
    use crate::signal::Machine;

    #[repr(C)]
    pub(super) struct ExceptionState {
        trapno: u16,
        _cpu: u16,
        err: u32,
        faultvaddr: u64,
    }

    #[repr(C)]
    pub(super) struct ThreadState {
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

    /// The `FXSAVE` image between two reserved words. An AVX state, which
    /// the kernel may pass instead, begins with the same bytes.
    #[repr(C)]
    pub(super) struct FloatState {
        _reserved: [c_int; 2],
        fxsave: [u8; 512],
        _reserved1: c_int,
    }

    pub(super) fn load(mc: &Mcontext) -> Machine {
        let ss = &mc.ss;
        let mut gpr = [
            ss.rax, ss.rcx, ss.rdx, ss.rbx, ss.rsp, ss.rbp, ss.rsi, ss.rdi, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        gpr[8..].copy_from_slice(&ss.r);
        Machine {
            gpr,
            rip: ss.rip,
            rflags: ss.rflags,
            cs: ss.cs as u16,
            fs: ss.fs as u16,
            gs: ss.gs as u16,
            trapno: mc.es.trapno.into(),
            err: mc.es.err.into(),
            fault_address: mc.es.faultvaddr,
            fxsave: Some(mc.fs.fxsave),
        }
    }

    pub(super) fn store(mc: &mut Mcontext, machine: &Machine) {
        let ss = &mut mc.ss;
        let g = &machine.gpr;
        [
            ss.rax, ss.rcx, ss.rdx, ss.rbx, ss.rsp, ss.rbp, ss.rsi, ss.rdi,
        ] = [g[0], g[1], g[2], g[3], g[4], g[5], g[6], g[7]];
        ss.r.copy_from_slice(&g[8..]);
        ss.rip = machine.rip;
        ss.rflags = machine.rflags;
        if let Some(fxsave) = machine.fxsave {
            mc.fs.fxsave = fxsave;
        }
    }

    const _: () = assert!(core::mem::offset_of!(Mcontext, fs) == 184);
}

#[cfg(target_arch = "aarch64")]
mod machine {
    use super::*;
    use crate::signal::{Fpsimd, Machine};

    #[repr(C)]
    pub(super) struct ExceptionState {
        far: u64,
        esr: u32,
        _exception: u32,
    }

    #[repr(C)]
    pub(super) struct ThreadState {
        x: [u64; 29],
        fp: u64,
        lr: u64,
        sp: u64,
        pc: u64,
        cpsr: u32,
        _pad: u32,
    }

    #[repr(C)]
    pub(super) struct FloatState {
        v: [u128; 32],
        fpsr: u32,
        fpcr: u32,
    }

    pub(super) fn load(mc: &Mcontext) -> Machine {
        let ss = &mc.ss;
        let mut x = [0; 31];
        x[..29].copy_from_slice(&ss.x);
        x[29] = ss.fp;
        x[30] = ss.lr;
        Machine {
            x,
            sp: ss.sp,
            pc: ss.pc,
            pstate: ss.cpsr.into(),
            fault_address: mc.es.far,
            esr: mc.es.esr.into(),
            fpsimd: Some(Fpsimd {
                v: mc.fs.v,
                fpsr: mc.fs.fpsr,
                fpcr: mc.fs.fpcr,
            }),
        }
    }

    pub(super) fn store(mc: &mut Mcontext, machine: &Machine) {
        let ss = &mut mc.ss;
        ss.x.copy_from_slice(&machine.x[..29]);
        ss.fp = machine.x[29];
        ss.lr = machine.x[30];
        ss.sp = machine.sp;
        ss.pc = machine.pc;
        ss.cpsr = machine.pstate as u32;
        if let Some(fpsimd) = machine.fpsimd {
            mc.fs.v = fpsimd.v;
            mc.fs.fpsr = fpsimd.fpsr;
            mc.fs.fpcr = fpsimd.fpcr;
        }
    }

    const _: () = assert!(core::mem::offset_of!(Mcontext, fs) == 288);
}

use machine::{ExceptionState, FloatState, ThreadState};

pub unsafe fn context(raw: *const c_void) -> Option<Context> {
    // SAFETY: the caller's live `ucontext_t`, whose machine context the
    // kernel keeps live for as long as the handler runs.
    unsafe {
        let uc = &*raw.cast::<Ucontext>();
        let mc = uc.mcontext.as_ref()?;
        Some(Context {
            machine: machine::load(mc),
            mask: uc.sigmask,
            stack: uc.stack,
        })
    }
}

pub unsafe fn set_context(raw: *mut c_void, context: &Context) {
    // SAFETY: as in `context`.
    unsafe {
        let uc = &mut *raw.cast::<Ucontext>();
        if let Some(mc) = uc.mcontext.as_mut() {
            machine::store(mc, &context.machine);
        }
        uc.sigmask = context.mask;
    }
}

/// None: a macOS host has the real Mach calls this would stand in for.
pub fn private_signal(_index: u8) -> Option<c_int> {
    None
}

pub fn send_to_thread(_tid: i32, _signo: c_int) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

pub fn queue(pid: i32, signo: c_int, _value: usize) -> std::io::Result<()> {
    // SAFETY: plain values.
    match unsafe { libc::kill(pid, signo) } {
        0 => Ok(()),
        _ => Err(std::io::Error::last_os_error()),
    }
}

pub fn for_each_thread(_each: &mut dyn FnMut(i32)) -> bool {
    false
}
