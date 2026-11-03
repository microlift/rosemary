use core::ffi::{c_int, c_void};

use crate::signal::{Code, Context};

pub const SIGEMT: Option<c_int> = None;
pub const SIGINFO: Option<c_int> = None;

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

const FPE: [Code; 8] = [
    Code::IntegerDivide,
    Code::IntegerOverflow,
    Code::FloatDivide,
    Code::FloatOverflow,
    Code::FloatUnderflow,
    Code::FloatInexact,
    Code::FloatInvalid,
    Code::FloatSubscript,
];

pub fn code(signo: c_int, code: c_int) -> Code {
    match code {
        libc::SI_USER => Code::User,
        libc::SI_TKILL => Code::Thread,
        libc::SI_QUEUE => Code::Queue,
        libc::SI_TIMER => Code::Timer,
        libc::SI_ASYNCIO => Code::AsyncIo,
        libc::SI_MESGQ => Code::MessageQueue,
        libc::SI_KERNEL | libc::SI_SIGIO => Code::Kernel,
        _ => crate::signal::by_signal(signo, code, &ILL, &FPE),
    }
}

#[cfg(target_arch = "x86_64")]
mod machine {
    use super::*;
    use crate::signal::Machine;

    const GPR: [c_int; 16] = [
        libc::REG_RAX,
        libc::REG_RCX,
        libc::REG_RDX,
        libc::REG_RBX,
        libc::REG_RSP,
        libc::REG_RBP,
        libc::REG_RSI,
        libc::REG_RDI,
        libc::REG_R8,
        libc::REG_R9,
        libc::REG_R10,
        libc::REG_R11,
        libc::REG_R12,
        libc::REG_R13,
        libc::REG_R14,
        libc::REG_R15,
    ];

    // The kernel's `struct sigcontext` head; musl hides `fpregs` in
    // `__private`.
    #[repr(C)]
    struct Mcontext {
        gregs: [libc::greg_t; 23],
        fpregs: *mut [u8; 512],
    }

    // Past the registers is the kernel's `_fpx_sw_bytes`, which says whether
    // an `XSAVE` header follows at 512. Its `XSTATE_BV` must claim x87 and
    // SSE, or `XRSTOR` resets them rather than load what was written.
    pub(super) const SW_BYTES: usize = 464;
    const FP_XSTATE_MAGIC1: u32 = 0x4650_5853;

    pub unsafe fn context(raw: *const c_void) -> Option<Context> {
        // SAFETY: the caller's live `ucontext_t`.
        let uc = unsafe { &*raw.cast::<libc::ucontext_t>() };
        let reg = |i: c_int| uc.uc_mcontext.gregs[i as usize] as u64;
        // cs, gs, fs, ss, a 16-bit selector each.
        let segments = reg(libc::REG_CSGSFS);
        // SAFETY: `Mcontext` is a prefix of `mcontext_t`.
        let fpregs = unsafe { (*(&raw const uc.uc_mcontext).cast::<Mcontext>()).fpregs };
        let fxsave = match fpregs.is_null() {
            true => None,
            // SAFETY: the kernel's `FXSAVE` image, 512 bytes.
            false => Some(unsafe { fpregs.read_unaligned() }),
        };
        Some(Context {
            machine: Machine {
                gpr: GPR.map(reg),
                rip: reg(libc::REG_RIP),
                rflags: reg(libc::REG_EFL),
                cs: segments as u16,
                gs: (segments >> 16) as u16,
                fs: (segments >> 32) as u16,
                trapno: reg(libc::REG_TRAPNO) as u32,
                err: reg(libc::REG_ERR),
                fault_address: reg(libc::REG_CR2),
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
        for (value, i) in machine.gpr.iter().zip(GPR) {
            uc.uc_mcontext.gregs[i as usize] = *value as i64;
        }
        uc.uc_mcontext.gregs[libc::REG_RIP as usize] = machine.rip as i64;
        uc.uc_mcontext.gregs[libc::REG_EFL as usize] = machine.rflags as i64;
        // SAFETY: as in `context`.
        let fpregs = unsafe { (*(&raw const uc.uc_mcontext).cast::<Mcontext>()).fpregs };
        if let Some(fxsave) = machine.fxsave
            && !fpregs.is_null()
        {
            let frame = fpregs.cast::<u8>();
            // SAFETY: as in `context`; with the magic, the header follows.
            unsafe {
                core::ptr::copy_nonoverlapping(fxsave.as_ptr(), frame, SW_BYTES);
                if frame.add(SW_BYTES).cast::<u32>().read_unaligned() == FP_XSTATE_MAGIC1 {
                    *frame.add(512) |= 0b11;
                }
            }
        }
        uc.uc_sigmask = context.mask;
    }
}

#[cfg(target_arch = "aarch64")]
mod machine {
    use super::*;
    use crate::signal::{Fpsimd, Machine};

    // The tail of `mcontext_t` is a list of tagged records, `struct
    // _aarch64_ctx` in the kernel's `sigcontext.h`, ended by a zero magic.
    // The kernel aligns it to 16; `libc` does not.
    const RESERVED: usize =
        (core::mem::offset_of!(libc::mcontext_t, pstate) + 8).next_multiple_of(16);
    const RESERVED_SIZE: usize = 4096;
    const FPSIMD_MAGIC: u32 = 0x4650_8001;
    const ESR_MAGIC: u32 = 0x4553_5201;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Head {
        magic: u32,
        size: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct FpsimdRecord {
        head: Head,
        fpsr: u32,
        fpcr: u32,
        vregs: [u128; 32],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct EsrRecord {
        head: Head,
        esr: u64,
    }

    /// The record tagged `magic`, if the kernel wrote one.
    unsafe fn record(mc: *const libc::mcontext_t, magic: u32) -> Option<*mut u8> {
        let base = mc.cast::<u8>().cast_mut().wrapping_add(RESERVED);
        let mut at = 0;
        while at + size_of::<Head>() <= RESERVED_SIZE {
            // SAFETY: within the reserved area, checked above.
            let head = unsafe { base.add(at).cast::<Head>().read_unaligned() };
            if head.magic == magic {
                return Some(base.wrapping_add(at));
            }
            if head.magic == 0 || head.size == 0 {
                return None;
            }
            at += head.size as usize;
        }
        None
    }

    pub unsafe fn context(raw: *const c_void) -> Option<Context> {
        // SAFETY: the caller's live `ucontext_t`.
        let uc = unsafe { &*raw.cast::<libc::ucontext_t>() };
        let mc = &uc.uc_mcontext;
        // SAFETY: records the kernel wrote, each read at its own size.
        let (fpsimd, esr) = unsafe {
            (
                record(mc, FPSIMD_MAGIC).map(|at| {
                    let r = at.cast::<FpsimdRecord>().read_unaligned();
                    Fpsimd {
                        v: r.vregs,
                        fpsr: r.fpsr,
                        fpcr: r.fpcr,
                    }
                }),
                record(mc, ESR_MAGIC).map_or(0, |at| at.cast::<EsrRecord>().read_unaligned().esr),
            )
        };
        Some(Context {
            machine: Machine {
                x: mc.regs,
                sp: mc.sp,
                pc: mc.pc,
                pstate: mc.pstate,
                fault_address: mc.fault_address,
                esr,
                fpsimd,
            },
            mask: uc.uc_sigmask,
            stack: uc.uc_stack,
        })
    }

    pub unsafe fn set_context(raw: *mut c_void, context: &Context) {
        // SAFETY: the caller's live `ucontext_t`.
        let uc = unsafe { &mut *raw.cast::<libc::ucontext_t>() };
        let machine = &context.machine;
        uc.uc_mcontext.regs = machine.x;
        uc.uc_mcontext.sp = machine.sp;
        uc.uc_mcontext.pc = machine.pc;
        uc.uc_mcontext.pstate = machine.pstate;
        // FIXME (ACCURACY): with live SVE state the kernel restores the vector
        // registers from the SVE record and ignores these.
        if let Some(fpsimd) = machine.fpsimd
            && let Some(at) = unsafe { record(&uc.uc_mcontext, FPSIMD_MAGIC) }
        {
            // SAFETY: the record found above, written at its own size.
            unsafe {
                let mut r = at.cast::<FpsimdRecord>().read_unaligned();
                r.vregs = fpsimd.v;
                r.fpsr = fpsimd.fpsr;
                r.fpcr = fpsimd.fpcr;
                at.cast::<FpsimdRecord>().write_unaligned(r);
            }
        }
        uc.uc_sigmask = context.mask;
    }

    const _: () = assert!(RESERVED == 288);
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

/// Linux's realtime signals, counted down from the top, where glibc's own
/// two at the bottom are not.
pub fn private_signal(index: u8) -> Option<c_int> {
    let signo = libc::SIGRTMAX() - 2 - c_int::from(index);
    (signo > libc::SIGRTMIN()).then_some(signo)
}

pub fn send_to_thread(tid: i32, signo: c_int) -> std::io::Result<()> {
    // SAFETY: plain integers; `tgkill` names this process so a recycled tid
    // in another one is never signalled.
    let rc = unsafe { libc::syscall(libc::SYS_tgkill, libc::getpid(), tid, signo) };
    match rc {
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

/// `getdents64` on `/proc/self/task`, straight into a buffer on the stack.
pub fn for_each_thread(each: &mut dyn FnMut(i32)) -> bool {
    // SAFETY: a NUL-terminated literal.
    let fd = unsafe {
        libc::open(
            c"/proc/self/task".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return false;
    }
    /// `struct linux_dirent64`'s fixed head: inode, offset, record length,
    /// type, then the NUL-terminated name.
    const NAME: usize = 19;
    let mut buffer = [0u8; 2048];
    let ok = loop {
        // SAFETY: a writable buffer of the stated length.
        let n =
            unsafe { libc::syscall(libc::SYS_getdents64, fd, buffer.as_mut_ptr(), buffer.len()) };
        if n <= 0 {
            break n == 0;
        }
        let mut at = 0;
        while at + NAME <= n as usize {
            let length = u16::from_ne_bytes([buffer[at + 16], buffer[at + 17]]) as usize;
            if length == 0 {
                break;
            }
            let name = &buffer[at + NAME..(at + length).min(n as usize)];
            let mut tid: i32 = 0;
            let mut digits = 0;
            for &byte in name.iter().take_while(|byte| **byte != 0) {
                if !byte.is_ascii_digit() {
                    digits = 0;
                    break;
                }
                tid = tid.wrapping_mul(10).wrapping_add(i32::from(byte - b'0'));
                digits += 1;
            }
            if digits > 0 {
                each(tid);
            }
            at += length;
        }
    };
    // SAFETY: the descriptor opened above.
    unsafe { libc::close(fd) };
    ok
}

#[cfg(test)]
mod listing {
    #[test]
    fn lists_this_thread_and_a_second_one() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (done, wait) = std::sync::mpsc::channel::<()>();
        let other = std::thread::spawn(move || {
            tx.send(rustix::thread::gettid().as_raw_nonzero().get())
                .unwrap();
            wait.recv().unwrap();
        });
        let other_tid = rx.recv().unwrap();
        // A pass may skip a live thread while others exit (the kernel resumes
        // the listing by position), so list again as `suspend_task` does.
        let mut seen = Vec::new();
        for _ in 0..8 {
            assert!(super::for_each_thread(&mut |tid| seen.push(tid)));
            if seen.contains(&other_tid) {
                break;
            }
        }
        done.send(()).unwrap();
        other.join().unwrap();
        assert!(seen.contains(&rustix::thread::gettid().as_raw_nonzero().get()));
        assert!(seen.contains(&other_tid));
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::machine::SW_BYTES;
    use super::*;

    const LOW: [u8; 16] = [0x5a; 16];

    /// Writes `LOW` into `xmm15`'s image, with the software-reserved tail
    /// zeroed as a guest's `x86_FLOAT_STATE64` leaves it.
    extern "C" fn handler(_: c_int, _: *mut libc::siginfo_t, raw: *mut c_void) {
        // SAFETY: the kernel's context for this handler.
        let mut context = unsafe { context(raw) }.expect("a context");
        let image = context.machine.fxsave.as_mut().expect("an FXSAVE image");
        image[400..416].copy_from_slice(&LOW);
        image[SW_BYTES..].fill(0);
        // SAFETY: as above, before the handler returns.
        unsafe { set_context(raw, &context) };
    }

    #[test]
    fn a_written_xmm_register_is_restored_and_its_ymm_half_kept() {
        if !std::arch::is_x86_feature_detected!("avx") {
            return;
        }
        let signo = libc::SIGRTMAX();
        // SAFETY: a handler of this module's own, for a signal nothing else uses.
        unsafe {
            let mut action: libc::sigaction = core::mem::zeroed();
            action.sa_sigaction = handler as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO;
            assert_eq!(libc::sigaction(signo, &action, core::ptr::null_mut()), 0);
        }
        let before = [0xa5u8; 32];
        let mut after = [0u8; 32];
        // SAFETY: `ymm15` is loaded, the signal taken and `ymm15` stored in one
        // block, so nothing between can move it; both buffers are 32 bytes.
        unsafe {
            core::arch::asm!(
                "vmovdqu ymm15, [{before}]",
                "syscall",
                "vmovdqu [{after}], ymm15",
                "vzeroupper",
                before = in(reg) before.as_ptr(),
                after = in(reg) after.as_mut_ptr(),
                inlateout("rax") libc::SYS_tgkill => _,
                in("rdi") libc::getpid(),
                in("rsi") libc::gettid(),
                in("rdx") signo,
                out("rcx") _,
                out("r11") _,
                out("xmm15") _,
            );
        }
        assert_eq!(after[..16], LOW, "the handler's xmm15 was not restored");
        assert_eq!(after[16..], before[16..], "the ymm15 upper half was lost");
    }
}
