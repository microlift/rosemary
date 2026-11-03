use microlift::arch::x86_64::context::RegisterContext;

use super::bad_pointer;
use crate::abi::{GuestMemory, Result};
use crate::shims::bind;

/// `int proc_pidpath(int pid, void *buffer, uint32_t buffersize)`.
///
/// The host implementation would identify the arm64 mirror executable.
pub fn proc_pidpath(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let path = if darwin::libc_backend::using_builtin() {
        darwin::process::describe(ctx.rdi as i32).map(|process| process.path)
    } else {
        crate::dyld::executable_path()
    };
    let Some(path) = path else {
        darwin::errno::set(libc::ESRCH);
        crate::shims::set_int_return(ctx, 0);
        return Ok(());
    };
    let bytes = path.as_bytes();
    let capacity = ctx.rdx as usize;
    if ctx.rsi == 0 || capacity <= bytes.len() {
        darwin::errno::set(libc::EINVAL);
        crate::shims::set_int_return(ctx, 0);
        return Ok(());
    }
    mem.write(ctx.rsi, bytes)
        .map_err(|_| bad_pointer(symbol, ctx.rsi))?;
    mem.write(ctx.rsi + bytes.len() as u64, &[0])
        .map_err(|_| bad_pointer(symbol, ctx.rsi + bytes.len() as u64))?;
    crate::shims::set_int_return(ctx, bytes.len() as i32);
    Ok(())
}

// `pthread_get_stackaddr_np` and `pthread_get_stacksize_np` live in
// [`super::machthread`], which has the `pthread_t` -> thread lookup.

/// The guest has no separately protected JIT mapping to toggle.
pub fn pthread_jit_write_protect_np(
    _symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    crate::shims::set_int_return(ctx, 0);
    Ok(())
}

/// `execl`, `execlp` and `execle`: the argument list gathered into the vector
/// the `v` forms take, and handed to `darwin::spawn`'s, which is what knows a
/// Mach-O has to be run through this runtime.
///
/// Variadic, so the host's cannot be called with the guest's registers; and
/// the host's would not rewrite a Mach-O anyway.
pub fn execl_family(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let path = ctx.rdi as usize as *const std::ffi::c_char;
    let mut argv: Vec<*const std::ffi::c_char> = Vec::new();
    let envp = {
        let mut args = super::printf::VarArgs::new(symbol, ctx, &*mem, 1);
        loop {
            let arg = args.next_int()?;
            argv.push(arg as usize as *const std::ffi::c_char);
            if arg == 0 {
                break;
            }
        }
        // `execle` carries the environment after the terminating null.
        match symbol {
            "execle" => Some(args.next_int()? as usize as *const *const std::ffi::c_char),
            _ => None,
        }
    };
    // SAFETY: a null-terminated vector of the guest's own strings, which live
    // in this address space, and the guest's path and environment.
    let rc = unsafe {
        match (symbol, envp) {
            ("execlp", _) => darwin::spawn::execvp(path, argv.as_ptr()),
            (_, Some(envp)) => darwin::spawn::execve(path, argv.as_ptr(), envp),
            _ => darwin::spawn::execv(path, argv.as_ptr()),
        }
    };
    crate::shims::set_int_return(ctx, rc);
    Ok(())
}

pub type Shim = crate::shims::Shim;

pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        "proc_pidpath" => Some(bind!("proc_pidpath" => proc_pidpath)),
        "pthread_jit_write_protect_np" => {
            Some(bind!("pthread_jit_write_protect_np" => pthread_jit_write_protect_np))
        }
        "execl" => Some(bind!("execl" => execl_family)),
        "execlp" => Some(bind!("execlp" => execl_family)),
        "execle" => Some(bind!("execle" => execl_family)),
        _ => None,
    }
}
