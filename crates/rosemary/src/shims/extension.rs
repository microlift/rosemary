use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{GuestMemory, Result};
use crate::shims::bind;
use crate::shims::printf::VarArgs;

/// One of this module's implementations, in the shape [`crate::shims`] wants.
pub type Shim = crate::shims::Shim;

/// Whether `name` (underscore already stripped) is one of this module's.
pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        "rosemary_host_call_trampoline" => {
            Some(bind!("rosemary_host_call_trampoline" => host_call_trampoline))
        }
        _ => None,
    }
}

/// `size_t rosemary_host_call_trampoline(void *addr, size_t argcount, ...)`
///
/// The variadic tail is the guest's, so it is read here rather than by the
/// native backend's `va_list` walk.
fn host_call_trampoline(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let addr = ctx.rdi as usize;
    let argcount = ctx.rsi;
    let mut tail = VarArgs::new(symbol, ctx, mem, 2);
    let args = (0..argcount)
        .map(|_| tail.next_int().map(|v| v as usize))
        .collect::<Result<Vec<_>>>()?;
    // SAFETY: the extension API hands the guest host function pointers and
    // makes it responsible for calling them with a matching signature.
    let ret = unsafe { darwin::rosemary::call::call_host(addr, &args) };
    ctx.rax = ret as u64;
    Ok(())
}
