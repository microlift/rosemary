use core::ffi::{c_int, c_void};

use crate::signal::{Code, Context};

pub const SIGEMT: Option<c_int> = Some(libc::SIGEMT);
pub const SIGINFO: Option<c_int> = Some(libc::SIGINFO);
/// FreeBSD reports `kill` as `SI_USER`, so code 0 has no sender.
pub const KERNEL_CODE_HAS_SENDER: bool = false;

// `<sys/signal.h>`; the `libc` crate does not carry these for FreeBSD.
const SI_NOINFO: c_int = 0;
const SI_USER: c_int = 0x10001;
const SI_QUEUE: c_int = 0x10002;
const SI_TIMER: c_int = 0x10003;
const SI_ASYNCIO: c_int = 0x10004;
const SI_MESGQ: c_int = 0x10005;
const SI_KERNEL: c_int = 0x10006;
const SI_LWP: c_int = 0x10007;

const ILL: [Code; 8] = [
    Code::IllegalOpcode,
    Code::IllegalOperand,
    Code::IllegalAddressing,
    Code::IllegalTrap,
    Code::PrivilegedOpcode,
    Code::PrivilegedRegister,
    Code::Coprocessor,
    Code::BadStack,
];

// The integer pair is the other way round from Linux.
const FPE: [Code; 8] = [
    Code::IntegerOverflow,
    Code::IntegerDivide,
    Code::FloatDivide,
    Code::FloatOverflow,
    Code::FloatUnderflow,
    Code::FloatInexact,
    Code::FloatInvalid,
    Code::FloatSubscript,
];

pub fn code(signo: c_int, code: c_int) -> Code {
    match code {
        SI_USER => Code::User,
        SI_LWP => Code::Thread,
        SI_QUEUE => Code::Queue,
        SI_TIMER => Code::Timer,
        SI_ASYNCIO => Code::AsyncIo,
        SI_MESGQ => Code::MessageQueue,
        SI_KERNEL | SI_NOINFO => Code::Kernel,
        _ => crate::signal::by_signal(signo, code, &ILL, &FPE),
    }
}

#[cfg(target_arch = "x86_64")]
mod machine {
    use super::*;
    use crate::signal::Machine;

    /// `mc_fpformat` when `mc_fpstate` begins with an `FXSAVE` image.
    const MC_FPFMT_XMM: libc::c_long = 0x10002;

    fn gpr(mc: &mut libc::mcontext_t) -> [&mut i64; 16] {
        [
            &mut mc.mc_rax,
            &mut mc.mc_rcx,
            &mut mc.mc_rdx,
            &mut mc.mc_rbx,
            &mut mc.mc_rsp,
            &mut mc.mc_rbp,
            &mut mc.mc_rsi,
            &mut mc.mc_rdi,
            &mut mc.mc_r8,
            &mut mc.mc_r9,
            &mut mc.mc_r10,
            &mut mc.mc_r11,
            &mut mc.mc_r12,
            &mut mc.mc_r13,
            &mut mc.mc_r14,
            &mut mc.mc_r15,
        ]
    }

    pub unsafe fn context(raw: *const c_void) -> Option<Context> {
        // SAFETY: the caller's live `ucontext_t`; copied so `gpr` can borrow it.
        let uc = unsafe { raw.cast::<libc::ucontext_t>().read() };
        let mut mc = uc.uc_mcontext;
        let fxsave = (mc.mc_fpformat == MC_FPFMT_XMM).then(|| {
            // SAFETY: the first 512 bytes of `mc_fpstate` are the image.
            unsafe {
                (&raw const mc.mc_fpstate)
                    .cast::<[u8; 512]>()
                    .read_unaligned()
            }
        });
        Some(Context {
            machine: Machine {
                gpr: gpr(&mut mc).map(|r| *r as u64),
                rip: mc.mc_rip as u64,
                rflags: mc.mc_rflags as u64,
                cs: mc.mc_cs as u16,
                fs: mc.mc_fs,
                gs: mc.mc_gs,
                trapno: mc.mc_trapno,
                err: mc.mc_err as u64,
                fault_address: mc.mc_addr as u64,
                fxsave,
            },
            mask: uc.uc_sigmask,
            stack: uc.uc_stack,
        })
    }

    pub unsafe fn set_context(raw: *mut c_void, context: &Context) {
        // SAFETY: the caller's live `ucontext_t`.
        let uc = unsafe { &mut *raw.cast::<libc::ucontext_t>() };
        let machine = &context.machine;
        for (slot, value) in gpr(&mut uc.uc_mcontext).into_iter().zip(machine.gpr) {
            *slot = value as i64;
        }
        uc.uc_mcontext.mc_rip = machine.rip as i64;
        uc.uc_mcontext.mc_rflags = machine.rflags as i64;
        if let Some(fxsave) = machine.fxsave
            && uc.uc_mcontext.mc_fpformat == MC_FPFMT_XMM
        {
            // SAFETY: as in `context`.
            unsafe {
                (&raw mut uc.uc_mcontext.mc_fpstate)
                    .cast::<[u8; 512]>()
                    .write_unaligned(fxsave)
            };
        }
        uc.uc_sigmask = context.mask;
    }
}

#[cfg(target_arch = "aarch64")]
mod machine {
    use super::*;
    use crate::signal::{Fpsimd, Machine};

    // `<machine/ucontext.h>`. Declared here because the `libc` crate gives
    // `fp_q` one register rather than 32, which misplaces everything after it.
    #[repr(C)]
    struct Gpregs {
        x: [u64; 30],
        lr: u64,
        sp: u64,
        elr: u64,
        spsr: u32,
        _pad: i32,
    }

    #[repr(C)]
    struct Fpregs {
        q: [u128; 32],
        sr: u32,
        cr: u32,
        _flags: i32,
        _pad: i32,
    }

    #[repr(C)]
    struct Mcontext {
        gpregs: Gpregs,
        fpregs: Fpregs,
        flags: i32,
        _pad: i32,
        _spare: [u64; 8],
    }

    #[repr(C)]
    struct Ucontext {
        sigmask: libc::sigset_t,
        mcontext: Mcontext,
        _link: *mut c_void,
        stack: libc::stack_t,
    }

    const MC_FP_VALID: i32 = 0x1;

    pub unsafe fn context(raw: *const c_void) -> Option<Context> {
        // SAFETY: the caller's live `ucontext_t`.
        let uc = unsafe { &*raw.cast::<Ucontext>() };
        let gp = &uc.mcontext.gpregs;
        let fp = &uc.mcontext.fpregs;
        let mut x = [0; 31];
        x[..30].copy_from_slice(&gp.x);
        x[30] = gp.lr;
        Some(Context {
            machine: Machine {
                x,
                sp: gp.sp,
                pc: gp.elr,
                pstate: gp.spsr.into(),
                fault_address: 0,
                esr: 0,
                fpsimd: (uc.mcontext.flags & MC_FP_VALID != 0).then_some(Fpsimd {
                    v: fp.q,
                    fpsr: fp.sr,
                    fpcr: fp.cr,
                }),
            },
            mask: uc.sigmask,
            stack: uc.stack,
        })
    }

    pub unsafe fn set_context(raw: *mut c_void, context: &Context) {
        // SAFETY: the caller's live `ucontext_t`.
        let uc = unsafe { &mut *raw.cast::<Ucontext>() };
        let machine = &context.machine;
        let gp = &mut uc.mcontext.gpregs;
        gp.x.copy_from_slice(&machine.x[..30]);
        gp.lr = machine.x[30];
        gp.sp = machine.sp;
        gp.elr = machine.pc;
        gp.spsr = machine.pstate as u32;
        if let Some(fpsimd) = machine.fpsimd {
            let fp = &mut uc.mcontext.fpregs;
            fp.q = fpsimd.v;
            fp.sr = fpsimd.fpsr;
            fp.cr = fpsimd.fpcr;
            uc.mcontext.flags |= MC_FP_VALID;
        }
        uc.sigmask = context.mask;
    }

    const _: () = assert!(core::mem::offset_of!(Ucontext, mcontext) == 16);
    const _: () = assert!(core::mem::offset_of!(Mcontext, fpregs) == 272);
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod machine {
    use super::*;

    pub unsafe fn context(_: *const c_void) -> Option<Context> {
        None
    }

    pub unsafe fn set_context(_: *mut c_void, _: &Context) {}
}

pub use machine::{context, set_context};

/// `SIGRTMAX` in `<sys/signal.h>`, which the `libc` crate does not carry.
const SIGRTMAX: c_int = 126;

/// The realtime signals, counted down from the top.
pub fn private_signal(index: u8) -> Option<c_int> {
    Some(SIGRTMAX - 2 - c_int::from(index))
}

pub fn send_to_thread(tid: i32, signo: c_int) -> std::io::Result<()> {
    // SAFETY: plain integers; `thr_kill` only reaches this process's threads.
    match unsafe { libc::thr_kill(libc::c_long::from(tid), signo) } {
        0 => Ok(()),
        _ => Err(std::io::Error::last_os_error()),
    }
}

pub fn queue(pid: i32, signo: c_int, value: usize) -> std::io::Result<()> {
    let value = libc::sigval {
        sival_ptr: value as *mut c_void,
    };
    // SAFETY: plain values.
    match unsafe { libc::sigqueue(pid, signo, value) } {
        0 => Ok(()),
        _ => Err(std::io::Error::last_os_error()),
    }
}

/// `KERN_PROC_PID | KERN_PROC_INC_THREAD`, one `kinfo_proc` per thread, into
/// pages mapped for the purpose: the list may be longer than a stack holds.
pub fn for_each_thread(each: &mut dyn FnMut(i32)) -> bool {
    let mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID | libc::KERN_PROC_INC_THREAD,
        crate::process::me(),
    ];
    let mut size = 0usize;
    // SAFETY: a size query with a valid MIB.
    if unsafe {
        libc::sysctl(
            mib.as_ptr(),
            4,
            core::ptr::null_mut(),
            &mut size,
            core::ptr::null(),
            0,
        )
    } != 0
    {
        return false;
    }
    // Room for threads started between the two calls.
    let capacity = size * 2 + size_of::<libc::kinfo_proc>() * 16;
    // SAFETY: a fresh anonymous mapping, unmapped below.
    let pages = unsafe {
        libc::mmap(
            core::ptr::null_mut(),
            capacity,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    if pages == libc::MAP_FAILED {
        return false;
    }
    let mut size = capacity;
    // SAFETY: the mapping holds `capacity` bytes.
    let ok = unsafe { libc::sysctl(mib.as_ptr(), 4, pages, &mut size, core::ptr::null(), 0) } == 0;
    if ok {
        let count = size / size_of::<libc::kinfo_proc>();
        for i in 0..count {
            // SAFETY: within the `size` bytes the kernel filled.
            let entry = unsafe { &*pages.cast::<libc::kinfo_proc>().add(i) };
            each(entry.ki_tid as i32);
        }
    }
    // SAFETY: the mapping made above.
    unsafe { libc::munmap(pages, capacity) };
    ok
}
