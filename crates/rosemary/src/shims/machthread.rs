use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{GuestMemory, Result};

use super::{bad_pointer, set_int_return};
use crate::shims::bind;

/// `x86_THREAD_STATE64`, and the number of 32-bit words it occupies.
const X86_THREAD_STATE64: u32 = 4;
const X86_THREAD_STATE64_COUNT: u32 = 42;

/// `x86_FLOAT_STATE64`, likewise.
const X86_FLOAT_STATE64: u32 = 5;
const X86_FLOAT_STATE64_COUNT: u32 = 131;
/// Byte offset of `__fpu_xmm0` in `x86_float_state64_t`; the sixteen registers
/// follow it one after another.
const FPU_XMM0: usize = 168;
/// Byte offset of `__fpu_mxcsr`, which a collector does not read and a crash
/// reporter does.
const FPU_MXCSR: usize = 32;

/// `KERN_SUCCESS` and the one failure this module produces itself.
const KERN_SUCCESS: i32 = 0;
const KERN_INVALID_ARGUMENT: i32 = 4;

/// `x86_thread_state64_t`, in the order the header declares it.
fn thread_state64(ctx: &RegisterContext, rsp: u64) -> [u64; 21] {
    [
        ctx.rax, ctx.rbx, ctx.rcx, ctx.rdx, ctx.rdi, ctx.rsi, ctx.rbp, rsp, ctx.r8, ctx.r9,
        ctx.r10, ctx.r11, ctx.r12, ctx.r13, ctx.r14, ctx.r15, ctx.rip,
        // `rflags`, `cs`, `fs`, `gs`.
        0, 0, 0, 0,
    ]
}

/// `kern_return_t thread_get_state(thread_act_t, thread_state_flavor_t, thread_state_t,
/// mach_msg_type_number_t *)`
fn thread_get_state(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let (port, flavor, out, count_at) = (ctx.rdi as u32, ctx.rsi as u32, ctx.rdx, ctx.rcx);

    let thread = if darwin::libc_backend::using_builtin() {
        darwin::mach::space::thread_of(port)
            .filter(|(pid, _)| *pid == std::process::id() as i32)
            .and_then(|(_, tid)| crate::thread::by_tid(tid))
    } else {
        crate::thread::by_key(port)
    };
    let Some(thread) = thread else {
        if darwin::libc_backend::using_builtin() {
            set_int_return(ctx, KERN_INVALID_ARGUMENT);
            return Ok(());
        }
        return passthrough(symbol, ctx);
    };
    if flavor != X86_THREAD_STATE64 && flavor != X86_FLOAT_STATE64 {
        if darwin::libc_backend::using_builtin() {
            set_int_return(ctx, KERN_INVALID_ARGUMENT);
            return Ok(());
        }
        return passthrough(symbol, ctx);
    }

    // The count is in-out: the caller states its buffer's capacity in 32-bit
    // words and the callee overwrites it with what it wrote.
    let mut capacity = [0u8; 4];
    mem.read(count_at, &mut capacity)
        .map_err(|_| bad_pointer(symbol, count_at))?;
    let capacity = u32::from_le_bytes(capacity);

    let want = match flavor {
        X86_THREAD_STATE64 => X86_THREAD_STATE64_COUNT,
        _ => X86_FLOAT_STATE64_COUNT,
    };
    if capacity < want {
        set_int_return(ctx, KERN_INVALID_ARGUMENT);
        return Ok(());
    }

    let bytes = match (flavor, thread.context()) {
        (X86_THREAD_STATE64, Some(guest)) => {
            let state = thread_state64(guest, guest.rsp);
            let mut buf = Vec::with_capacity(state.len() * 8);
            for word in state {
                buf.extend_from_slice(&word.to_le_bytes());
            }
            buf
        }
        // Registered, and not inside guest code: it has entered before and
        // returned.
        (X86_THREAD_STATE64, None) => vec![0u8; X86_THREAD_STATE64_COUNT as usize * 4],
        (_, guest) => float_state64(guest),
    };

    mem.write(out, &bytes)
        .map_err(|_| bad_pointer(symbol, out))?;
    mem.write(count_at, &want.to_le_bytes())
        .map_err(|_| bad_pointer(symbol, count_at))?;
    set_int_return(ctx, KERN_SUCCESS);
    Ok(())
}

/// `x86_float_state64_t`, with the sixteen `xmm` registers filled in and everything
/// else zero.
fn float_state64(guest: Option<&RegisterContext>) -> Vec<u8> {
    /// The `MXCSR` a process starts with: all exceptions masked, round to
    /// nearest.
    const MXCSR_DEFAULT: u32 = 0x1f80;

    let mut buf = vec![0u8; X86_FLOAT_STATE64_COUNT as usize * 4];
    buf[FPU_MXCSR..FPU_MXCSR + 4].copy_from_slice(&MXCSR_DEFAULT.to_le_bytes());
    if let Some(guest) = guest {
        for (i, xmm) in guest.xmm.iter().enumerate() {
            let at = FPU_XMM0 + i * 16;
            if at + 16 > buf.len() {
                break;
            }
            buf[at..at + 8].copy_from_slice(&xmm[0].to_le_bytes());
            buf[at + 8..at + 16].copy_from_slice(&xmm[1].to_le_bytes());
        }
    }
    buf
}

/// `void *pthread_get_stackaddr_np(pthread_t)` - the *highest* address of the
/// stack, which is where a stack that grows down begins.
fn pthread_get_stackaddr_np(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    match guest_thread_of(ctx.rdi) {
        Some(thread) => ctx.rax = thread.stack().end,
        None => return passthrough(symbol, ctx),
    }
    Ok(())
}

/// `size_t pthread_get_stacksize_np(pthread_t)`
fn pthread_get_stacksize_np(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    match guest_thread_of(ctx.rdi) {
        Some(thread) => {
            let range = thread.stack();
            ctx.rax = range.end - range.start;
        }
        None => return passthrough(symbol, ctx),
    }
    Ok(())
}

/// The guest thread a `pthread_t` names, if it is one.
fn guest_thread_of(pthread: u64) -> Option<&'static crate::thread::GuestThread> {
    if pthread == 0 {
        return None;
    }
    // SAFETY: `pthread` is a `pthread_t` the guest holds - from `pthread_self` or
    // `pthread_create` - mapped back to the host's, so the value is one libpthread
    // issued. `pthread_mach_thread_np` reads the port name out of it and sends
    // nothing.
    let port = unsafe { libc::pthread_mach_thread_np(darwin::pthread::thread_of(pthread)) };
    crate::thread::by_key(port)
}

/// The host's own implementation of each name this module claims, resolved **before**
/// any of them can be called.
static HOST: std::sync::OnceLock<[Option<u64>; 3]> = std::sync::OnceLock::new();

/// Resolve this module's host entry points. Called once, before the guest runs.
pub fn warm() {
    HOST.get_or_init(|| {
        [
            crate::host::symbol("thread_get_state"),
            crate::host::symbol("pthread_get_stackaddr_np"),
            crate::host::symbol("pthread_get_stacksize_np"),
        ]
    });
}

/// Forward to the host's own implementation, unshimmed.
///
/// Reached for every thread this runtime has not run guest code on, and for
/// every flavour but the two above - a guest crash reporter asking for
/// `THREAD_IDENTIFIER_INFO` is asking something the kernel can answer.
fn passthrough(symbol: &str, ctx: &mut RegisterContext) -> Result<()> {
    let index = match symbol {
        "thread_get_state" => 0,
        "pthread_get_stackaddr_np" => 1,
        _ => 2,
    };
    let host = HOST
        .get()
        .and_then(|h| h[index])
        .ok_or(crate::abi::AbiError::NoShim {
            symbol: symbol.to_string(),
        })?;
    // SAFETY: each of the three symbols this module claims takes at most four
    // integer arguments and returns one, and the host's own declaration is
    // narrower than this prototype in the argument registers only - the same
    // over-wide-prototype argument [`crate::abi::marshal`] rests on.
    let f: extern "C" fn(u64, u64, u64, u64) -> u64 =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64, u64, u64, u64) -> u64>(host) };
    ctx.rax = f(ctx.rdi, ctx.rsi, ctx.rdx, ctx.rcx);
    Ok(())
}

pub type Shim = crate::shims::Shim;

/// Whether `name` (underscore already stripped) is one of this module's.
pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        "thread_get_state" => Some(bind!("thread_get_state" => thread_get_state)),
        "pthread_get_stackaddr_np" => {
            Some(bind!("pthread_get_stackaddr_np" => pthread_get_stackaddr_np))
        }
        "pthread_get_stacksize_np" => {
            Some(bind!("pthread_get_stacksize_np" => pthread_get_stacksize_np))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_state_puts_each_register_where_the_header_does() {
        let mut ctx = RegisterContext::new();
        // Distinguishable values, so a transposition is visible rather than
        // plausible.
        ctx.rax = 0x11;
        ctx.rbx = 0x22;
        ctx.rcx = 0x33;
        ctx.rdx = 0x44;
        ctx.rdi = 0x55;
        ctx.rsi = 0x66;
        ctx.rbp = 0x77;
        ctx.rsp = 0x8888;
        ctx.r15 = 0xff;
        ctx.rip = 0xabcd;

        // A stack pointer distinct from `ctx.rsp`, so a version that ignored the
        // argument and read the context would be caught.
        let state = thread_state64(&ctx, 0x1000);
        assert_eq!(state[0], 0x11, "rax");
        assert_eq!(state[1], 0x22, "rbx");
        assert_eq!(state[2], 0x33, "rcx");
        assert_eq!(state[3], 0x44, "rdx");
        assert_eq!(state[4], 0x55, "rdi");
        assert_eq!(state[5], 0x66, "rsi");
        assert_eq!(state[6], 0x77, "rbp");
        assert_eq!(state[7], 0x1000, "rsp is the watermark");
        assert_eq!(state[15], 0xff, "r15");
        assert_eq!(state[16], 0xabcd, "rip");
        assert_eq!(state.len() * 8, X86_THREAD_STATE64_COUNT as usize * 4);
    }

    #[test]
    fn a_float_state_is_the_right_size_and_holds_the_xmm_registers() {
        let mut ctx = RegisterContext::new();
        ctx.xmm[0] = [0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210];
        ctx.xmm[15] = [0xdead, 0xbeef];

        let buf = float_state64(Some(&ctx));
        assert_eq!(buf.len(), X86_FLOAT_STATE64_COUNT as usize * 4);
        assert_eq!(
            u64::from_le_bytes(buf[FPU_XMM0..FPU_XMM0 + 8].try_into().unwrap()),
            0x0123_4567_89ab_cdef
        );
        assert_eq!(
            u64::from_le_bytes(buf[FPU_XMM0 + 8..FPU_XMM0 + 16].try_into().unwrap()),
            0xfedc_ba98_7654_3210
        );
        let last = FPU_XMM0 + 15 * 16;
        assert_eq!(
            u64::from_le_bytes(buf[last..last + 8].try_into().unwrap()),
            0xdead
        );
        // The last `xmm` register must end inside the structure, which is the
        // check that catches an offset copied from the wrong architecture.
        assert!(last + 16 <= buf.len());
    }
}
