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

/// `BOOL NSEqualSizes(NSSize, NSSize)`: computed here rather than forwarded,
/// since it is pure arithmetic on the two structs and needs no host AppKit at
/// all - unlike [`mouse_in_rect`] and [`point_in_rect`], which still do.
fn equal_sizes(_symbol: &str, ctx: &mut RegisterContext, _mem: &mut dyn GuestMemory) -> Result<()> {
    let a = (f64::from_bits(ctx.xmm[0][0]), f64::from_bits(ctx.xmm[1][0]));
    let b = (f64::from_bits(ctx.xmm[2][0]), f64::from_bits(ctx.xmm[3][0]));
    ctx.rax = u64::from(a == b);
    Ok(())
}

/// `aRect`, with a non-negative size and the origin moved to match - as
/// `super::coregraphics::rect_equal`'s `standard` closure does.
fn standardized(r: CgRect) -> CgRect {
    let (x, width) = if r.width < 0.0 {
        (r.x + r.width, -r.width)
    } else {
        (r.x, r.width)
    };
    let (y, height) = if r.height < 0.0 {
        (r.y + r.height, -r.height)
    } else {
        (r.y, r.height)
    };
    CgRect {
        x,
        y,
        width,
        height,
    }
}

/// `BOOL NSIntersectsRect(NSRect, NSRect)`: both on the guest's stack, one
/// after the other, as [`super::coregraphics::rect_equal`]'s are. Self-contained,
/// for the reason [`equal_sizes`] is.
fn intersects_rect(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let a = standardized(rect_argument(symbol, ctx, mem, 0)?);
    let b = standardized(rect_argument(symbol, ctx, mem, 32)?);
    let empty = |r: &CgRect| r.width <= 0.0 || r.height <= 0.0;
    let overlap = !empty(&a)
        && !empty(&b)
        && a.x < b.x + b.width
        && b.x < a.x + a.width
        && a.y < b.y + b.height
        && b.y < a.y + a.height;
    ctx.rax = u64::from(overlap);
    Ok(())
}

/// `BOOL NSContainsRect(NSRect aRect, NSRect bRect)`: whether `bRect` lies
/// entirely within `aRect`. Self-contained, for the reason [`equal_sizes`] is.
fn contains_rect(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let a = standardized(rect_argument(symbol, ctx, mem, 0)?);
    let b = standardized(rect_argument(symbol, ctx, mem, 32)?);
    let contains = b.width > 0.0
        && b.height > 0.0
        && a.x <= b.x
        && a.y <= b.y
        && b.x + b.width <= a.x + a.width
        && b.y + b.height <= a.y + a.height;
    ctx.rax = u64::from(contains);
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
        "NSEqualSizes" => Some(bind!("NSEqualSizes" => equal_sizes)),
        "NSIntersectsRect" => Some(bind!("NSIntersectsRect" => intersects_rect)),
        "NSContainsRect" => Some(bind!("NSContainsRect" => contains_rect)),
        _ => None,
    }
}
