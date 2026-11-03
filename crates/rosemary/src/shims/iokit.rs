use core::ffi::c_void;

use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{Callbacks, GuestMemory, Result};

use super::host::builtin_or_host;
use super::set_int_return;
use crate::shims::bind;

/// `kern_return_t IOServiceAddMatchingNotification( IONotificationPortRef, const
/// io_name_t, CFDictionaryRef, IOServiceMatchingCallback, void *, io_iterator_t *)`
type AddMatchingNotificationFn = unsafe extern "C" fn(
    *mut c_void,
    *const c_void,
    *const c_void,
    *mut c_void,
    *mut c_void,
    *mut u32,
) -> i32;

fn add_matching_notification(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let address = builtin_or_host(symbol)?;
    // SAFETY: the declaration from IOKitLib.h, resolved by name.
    let host = unsafe { core::mem::transmute::<u64, AddMatchingNotificationFn>(address) };

    // The callback is the fourth argument.
    let callback = callbacks.substitute(symbol, ctx.rcx)?;
    let status = unsafe {
        host(
            ctx.rdi as *mut c_void,
            ctx.rsi as *const c_void,
            ctx.rdx as *const c_void,
            callback as *mut c_void,
            ctx.r8 as *mut c_void,
            ctx.r9 as *mut u32,
        )
    };
    set_int_return(ctx, status);
    Ok(())
}

pub type Shim = crate::signal::Shim;

pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        "IOServiceAddMatchingNotification" => {
            Some(bind!(callback "IOServiceAddMatchingNotification" => add_matching_notification))
        }
        _ => None,
    }
}
