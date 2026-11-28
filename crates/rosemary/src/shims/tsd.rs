//! The guest's `%gs` base, which Darwin lets a thread point anywhere and Wine
//! points at its TEB while Windows code runs.

use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{GuestMemory, Result};
use crate::shims::bind;

/// `void _thread_set_tsd_base(void *)`, the wrapper around the
/// `thread_fast_set_cthread_self64` trap.
///
/// The guest's `%gs` is its context's, so the change is made there rather
/// than to the host thread's register, which only host code would read.
fn thread_set_tsd_base(
    _symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    set_cthread_self(ctx, ctx.rdi);
    Ok(())
}

/// `thread_fast_set_cthread_self64`: set the guest thread's `%gs` base.
///
/// FIXME (ACCURACY): host code that enters guest code again starts it with
/// the Darwin array in `%gs`, where on Darwin the base keeps whatever was last
/// set.
pub fn set_cthread_self(ctx: &mut RegisterContext, base: u64) {
    ctx.gs_base = base;
}

pub type Shim = crate::shims::Shim;

pub fn shim_for(name: &str) -> Option<Shim> {
    (name == "_thread_set_tsd_base").then_some(bind!("_thread_set_tsd_base" => thread_set_tsd_base))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_the_guest_gs_base() {
        let mut ctx = RegisterContext::default();
        set_cthread_self(&mut ctx, 0x7000_0000);
        assert_eq!(ctx.gs_base, 0x7000_0000);
    }
}
