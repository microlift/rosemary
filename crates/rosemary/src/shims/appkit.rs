use microlift::arch::x86_64::context::RegisterContext;

use super::coregraphics::{CgPoint, CgRect, rect_argument};
use super::host::host_or_refuse;
use crate::abi::{GuestMemory, Result};
use crate::shims::bind;

/// `BOOL NSMouseInRect(NSPoint aPoint, NSRect aRect, BOOL isFlipped)`
///
/// The guest's registers are the whole reason this is not a table entry.
fn mouse_in_rect(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let point = CgPoint {
        x: f64::from_bits(ctx.xmm[0][0]),
        y: f64::from_bits(ctx.xmm[1][0]),
    };
    let rect = rect_argument(symbol, ctx, mem, 0)?;
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `BOOL (*)(NSPoint, NSRect, BOOL)`, resolved out of an AppKit this process
    // has loaded - the guest's own window brought it in.
    let f: extern "C" fn(CgPoint, CgRect, u8) -> u8 =
        unsafe { core::mem::transmute::<u64, extern "C" fn(CgPoint, CgRect, u8) -> u8>(host) };
    // A `BOOL` is a signed char, so only the low byte of `rdi` carries it and
    // only the low byte of `rax` may be written back - the rest of `rax` is
    // whatever the guest's caller will ignore, and zeroing it is the honest
    // answer for a byte-wide return.
    ctx.rax = u64::from(f(point, rect, ctx.rdi as u8));
    Ok(())
}

/// `BOOL NSPointInRect(NSPoint aPoint, NSRect aRect)`
fn point_in_rect(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let point = CgPoint {
        x: f64::from_bits(ctx.xmm[0][0]),
        y: f64::from_bits(ctx.xmm[1][0]),
    };
    let rect = rect_argument(symbol, ctx, mem, 0)?;
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `BOOL (*)(NSPoint, NSRect)`, resolved out of an AppKit this process has
    // loaded - the guest's own window brought it in.
    let f: extern "C" fn(CgPoint, CgRect) -> u8 =
        unsafe { core::mem::transmute::<u64, extern "C" fn(CgPoint, CgRect) -> u8>(host) };
    ctx.rax = u64::from(f(point, rect));
    Ok(())
}

/// `void NSRectFill(NSRect aRect)`
fn rect_fill(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let rect = rect_argument(symbol, ctx, mem, 0)?;
    // SAFETY: as above, for `void (*)(NSRect)`.
    let f: extern "C" fn(CgRect) =
        unsafe { core::mem::transmute::<u64, extern "C" fn(CgRect)>(host) };
    f(rect);
    Ok(())
}

/// Whether `name` (underscore already stripped) is one of this module's.
pub fn shim_for(name: &str) -> Option<super::coregraphics::Shim> {
    match name {
        "NSMouseInRect" => Some(bind!("NSMouseInRect" => mouse_in_rect)),
        "NSPointInRect" => Some(bind!("NSPointInRect" => point_in_rect)),
        "NSRectFill" => Some(bind!("NSRectFill" => rect_fill)),
        _ => None,
    }
}
