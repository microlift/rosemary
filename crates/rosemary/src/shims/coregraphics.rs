use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{AbiError, Callbacks, GuestMemory, Result};

use super::host::host_or_refuse;
use crate::shims::bind;

/// `CGRect`, as AAPCS64 returns one.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct CgRect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

impl CgRect {
    /// The thirty-two bytes the guest expects in its hidden buffer, in order.
    fn to_le_bytes(self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, v) in [self.x, self.y, self.width, self.height].iter().enumerate() {
            out[i * 8..(i + 1) * 8].copy_from_slice(&v.to_le_bytes());
        }
        out
    }
}

/// Call a host `CGRect f(u32)` and place the result the guest's way.
///
/// `rdi` is the hidden return buffer and `esi` the first real argument, because SysV
/// AMD64 shifted everything along by one for the MEMORY return.
fn rect_of_display(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let (buffer, display) = (ctx.rdi, ctx.rsi as u32);
    // SAFETY: the symbol is one [`shim_for`] matched, which really is
    // `CGRect (*)(CGDirectDisplayID)`, resolved out of whichever CoreGraphics
    // this process has - Apple's, or this tree's own.
    let f: extern "C" fn(u32) -> CgRect =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u32) -> CgRect>(host) };
    let rect = f(display);
    if buffer == 0 {
        return Err(AbiError::Memory {
            symbol: symbol.to_string(),
            addr: 0,
        });
    }
    mem.write_bytes(buffer, &rect.to_le_bytes(), symbol)?;
    ctx.rax = buffer;
    Ok(())
}

/// Read the `CGRect` a guest passed by value, out of its own stack.
///
/// A MEMORY-class argument is *on the stack* under SysV AMD64, and microlift exits
/// before the `CALL`, so `rsp` is still the pre-call value and the argument area starts
/// at `rsp + 0` - the arrangement [`crate::abi::marshal`]'s stack path already rests
/// on.
pub(crate) fn rect_argument(
    symbol: &str,
    ctx: &RegisterContext,
    mem: &mut dyn GuestMemory,
    offset: u64,
) -> Result<CgRect> {
    let at = ctx.rsp.wrapping_add(offset);
    let mut bytes = [0u8; 32];
    for (i, word) in bytes.chunks_exact_mut(8).enumerate() {
        let value = mem.read_u64(at.wrapping_add(i as u64 * 8), symbol)?;
        word.copy_from_slice(&value.to_le_bytes());
    }
    let field = |i: usize| {
        let mut w = [0u8; 8];
        w.copy_from_slice(&bytes[i * 8..(i + 1) * 8]);
        f64::from_le_bytes(w)
    };
    Ok(CgRect {
        x: field(0),
        y: field(1),
        width: field(2),
        height: field(3),
    })
}

/// `void CGContextClearRect(CGContextRef, CGRect)`
fn context_clear_rect(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let rect = rect_argument(symbol, ctx, mem, 0)?;
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `void (*)(CGContextRef, CGRect)`, resolved out of a CoreGraphics this
    // process has loaded. The context is an opaque handle the guest got from
    // `CGBitmapContextCreate`, so it is the host's own.
    let f: extern "C" fn(u64, CgRect) =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64, CgRect)>(host) };
    f(ctx.rdi, rect);
    Ok(())
}

/// `void CGContextDrawImage(CGContextRef, CGRect, CGImageRef)`
fn context_draw_image(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let rect = rect_argument(symbol, ctx, mem, 0)?;
    // SAFETY: as above, for `void (*)(CGContextRef, CGRect, CGImageRef)`.
    let f: extern "C" fn(u64, CgRect, u64) =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64, CgRect, u64)>(host) };
    f(ctx.rdi, rect, ctx.rsi);
    Ok(())
}

/// The eight `CGFloat CGRectGet...(CGRect)` accessors, which are one shim.
fn rect_accessor(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let rect = rect_argument(symbol, ctx, mem, 0)?;
    // SAFETY: every symbol [`shim_for`] routes here is `CGFloat (*)(CGRect)`,
    // resolved out of a CoreGraphics this process has loaded. Passing the
    // aggregate by value leaves the `v0`–`v3` placement to the compiler, as
    // everywhere else in this module.
    let f: extern "C" fn(CgRect) -> f64 =
        unsafe { core::mem::transmute::<u64, extern "C" fn(CgRect) -> f64>(host) };
    ctx.xmm[0] = [f(rect).to_bits(), 0];
    Ok(())
}

/// `bool CGRectEqualToRect(CGRect, CGRect)`: both on the guest's stack, one
/// after the other. Compared standardized, as CoreGraphics compares them, so a
/// rectangle with a negative size equals the one it describes.
fn rect_equal(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let standard = |r: CgRect| {
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
        [x, y, width, height]
    };
    let a = standard(rect_argument(symbol, ctx, mem, 0)?);
    let b = standard(rect_argument(symbol, ctx, mem, 32)?);
    ctx.rax = u64::from(a == b);
    Ok(())
}

/// `CGPoint CGEventGetLocation(CGEventRef)`
///
/// Sixteen bytes of two `double`s, which is the case the two ABIs **agree** about -
/// SysV AMD64 returns two SSE eightbytes in `xmm0`/`xmm1`, AAPCS64 returns a two-member
/// homogeneous float aggregate in `v0`/`v1`.
fn event_location(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `CGPoint (*)(CGEventRef)`; the event is one the host made.
    let f: extern "C" fn(u64) -> CgPoint =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64) -> CgPoint>(host) };
    let point = f(ctx.rdi);
    ctx.xmm[0] = [point.x.to_bits(), 0];
    ctx.xmm[1] = [point.y.to_bits(), 0];
    Ok(())
}

/// `CGSize CGDisplayScreenSize(CGDirectDisplayID)`
fn screen_size_of_display(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `CGSize (*)(CGDirectDisplayID)`, resolved out of a CoreGraphics this
    // process has loaded.
    let f: extern "C" fn(u32) -> CgPoint =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u32) -> CgPoint>(host) };
    let size = f(ctx.rdi as u32);
    ctx.xmm[0] = [size.x.to_bits(), 0];
    ctx.xmm[1] = [size.y.to_bits(), 0];
    Ok(())
}

/// `CGEventRef CGEventCreateMouseEvent(CGEventSourceRef source, CGEventType mouseType,
/// CGPoint mouseCursorPosition, CGMouseButton mouseButton)`
fn event_create_mouse(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let point = CgPoint {
        x: f64::from_bits(ctx.xmm[0][0]),
        y: f64::from_bits(ctx.xmm[1][0]),
    };
    // SAFETY: the symbol [`shim_for`] matched, whose declaration is the one
    // above, resolved out of a CoreGraphics this process has loaded. `source` is
    // an opaque handle the guest got from `CGEventSourceCreate` - or null, which
    // is the documented "no source" and what a game that only warps the cursor
    // passes.
    let f: extern "C" fn(u64, u32, CgPoint, u32) -> u64 =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64, u32, CgPoint, u32) -> u64>(host) };
    ctx.rax = f(ctx.rdi, ctx.rsi as u32, point, ctx.rdx as u32);
    Ok(())
}

/// `NSRect NSInsetRect(NSRect aRect, CGFloat dX, CGFloat dY)`
fn inset_rect(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let rect = rect_argument(symbol, ctx, mem, 0)?;
    let (dx, dy) = (f64::from_bits(ctx.xmm[0][0]), f64::from_bits(ctx.xmm[1][0]));
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `NSRect (*)(NSRect, CGFloat, CGFloat)`, resolved out of a Foundation this
    // process has loaded. Declaring the aggregate on both sides leaves the
    // `v0`–`v3` placement to the compiler.
    let f: extern "C" fn(CgRect, f64, f64) -> CgRect =
        unsafe { core::mem::transmute::<u64, extern "C" fn(CgRect, f64, f64) -> CgRect>(host) };
    let out = f(rect, dx, dy);
    let buffer = ctx.rdi;
    if buffer == 0 {
        return Err(AbiError::Memory {
            symbol: symbol.to_string(),
            addr: 0,
        });
    }
    mem.write_bytes(buffer, &out.to_le_bytes(), symbol)?;
    ctx.rax = buffer;
    Ok(())
}

/// `CGAffineTransform`, as both ABIs pass one: forty-eight bytes, which is
/// MEMORY class to a guest and indirect to a host.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct CgTransform {
    pub(crate) a: f64,
    pub(crate) b: f64,
    pub(crate) c: f64,
    pub(crate) d: f64,
    pub(crate) tx: f64,
    pub(crate) ty: f64,
}

impl CgTransform {
    /// The forty-eight bytes the guest expects in its hidden buffer, in order.
    fn to_le_bytes(self) -> [u8; 48] {
        let mut out = [0u8; 48];
        for (i, v) in [self.a, self.b, self.c, self.d, self.tx, self.ty]
            .iter()
            .enumerate()
        {
            out[i * 8..(i + 1) * 8].copy_from_slice(&v.to_le_bytes());
        }
        out
    }
}

/// Read the `CGAffineTransform` a guest passed by value, out of its own stack.
///
/// A MEMORY-class argument is on the stack under SysV AMD64, and microlift
/// exits before the `CALL`, so `rsp` is still the pre-call value and the
/// argument area starts at `rsp + 0` - the arrangement [`rect_argument`] rests
/// on too.
fn transform_argument(
    symbol: &str,
    ctx: &RegisterContext,
    mem: &mut dyn GuestMemory,
    offset: u64,
) -> Result<CgTransform> {
    let at = ctx.rsp.wrapping_add(offset);
    let mut bytes = [0u8; 48];
    for (i, word) in bytes.chunks_exact_mut(8).enumerate() {
        let value = mem.read_u64(at.wrapping_add(i as u64 * 8), symbol)?;
        word.copy_from_slice(&value.to_le_bytes());
    }
    let field = |i: usize| {
        let mut w = [0u8; 8];
        w.copy_from_slice(&bytes[i * 8..(i + 1) * 8]);
        f64::from_le_bytes(w)
    };
    Ok(CgTransform {
        a: field(0),
        b: field(1),
        c: field(2),
        d: field(3),
        tx: field(4),
        ty: field(5),
    })
}

/// Place a `CGAffineTransform` return the guest's way: through the hidden
/// buffer `rdi` names, with `rax` answering it.
fn return_transform(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    buffer: u64,
    value: CgTransform,
) -> Result<()> {
    if buffer == 0 {
        return Err(AbiError::Memory {
            symbol: symbol.to_string(),
            addr: 0,
        });
    }
    mem.write_bytes(buffer, &value.to_le_bytes(), symbol)?;
    ctx.rax = buffer;
    Ok(())
}

/// The host prototypes these shims call, one alias per shape.
type Transform6 = extern "C" fn(f64, f64, f64, f64, f64, f64) -> CgTransform;
type Transform2 = extern "C" fn(f64, f64) -> CgTransform;
type Transform1 = extern "C" fn(CgTransform) -> CgTransform;
type TransformPair = extern "C" fn(CgTransform, CgTransform) -> CgTransform;
type TransformScaled = extern "C" fn(CgTransform, f64, f64) -> CgTransform;
type TransformIdentity = extern "C" fn(CgTransform) -> u8;
type RectApply = extern "C" fn(CgRect, CgTransform) -> CgRect;

/// `CGAffineTransform CGAffineTransformMake(CGFloat a, CGFloat b, CGFloat c,
/// CGFloat d, CGFloat tx, CGFloat ty)`
///
/// Six `double`s in `xmm0`-`xmm5`; the hidden return buffer is `rdi`, which
/// the SSE bank does not shift.
fn transform_make(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let buffer = ctx.rdi;
    let arg = |i: usize| f64::from_bits(ctx.xmm[i][0]);
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `CGAffineTransform (*)(CGFloat, CGFloat, CGFloat, CGFloat, CGFloat, CGFloat)`.
    let f: Transform6 = unsafe { core::mem::transmute::<u64, Transform6>(host) };
    let out = f(arg(0), arg(1), arg(2), arg(3), arg(4), arg(5));
    return_transform(symbol, ctx, mem, buffer, out)
}

/// `CGAffineTransform CGAffineTransformMakeScale(CGFloat sx, CGFloat sy)`
fn transform_make_scale(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let buffer = ctx.rdi;
    let (sx, sy) = (f64::from_bits(ctx.xmm[0][0]), f64::from_bits(ctx.xmm[1][0]));
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `CGAffineTransform (*)(CGFloat, CGFloat)`.
    let f: Transform2 = unsafe { core::mem::transmute::<u64, Transform2>(host) };
    let out = f(sx, sy);
    return_transform(symbol, ctx, mem, buffer, out)
}

/// `CGAffineTransform CGAffineTransformMakeTranslation(CGFloat tx, CGFloat ty)`
fn transform_make_translation(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let buffer = ctx.rdi;
    let (tx, ty) = (f64::from_bits(ctx.xmm[0][0]), f64::from_bits(ctx.xmm[1][0]));
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `CGAffineTransform (*)(CGFloat, CGFloat)`.
    let f: Transform2 = unsafe { core::mem::transmute::<u64, Transform2>(host) };
    let out = f(tx, ty);
    return_transform(symbol, ctx, mem, buffer, out)
}

/// `CGAffineTransform CGAffineTransformConcat(CGAffineTransform t1,
/// CGAffineTransform t2)`
///
/// Both arguments are MEMORY class, so they are on the guest's stack one after
/// the other, forty-eight bytes each.
fn transform_concat(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let buffer = ctx.rdi;
    let first = transform_argument(symbol, ctx, mem, 0)?;
    let second = transform_argument(symbol, ctx, mem, 48)?;
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `CGAffineTransform (*)(CGAffineTransform, CGAffineTransform)`.
    let f: TransformPair = unsafe { core::mem::transmute::<u64, TransformPair>(host) };
    let out = f(first, second);
    return_transform(symbol, ctx, mem, buffer, out)
}

/// `CGAffineTransform CGAffineTransformScale(CGAffineTransform t, CGFloat sx,
/// CGFloat sy)`
fn transform_scale(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let buffer = ctx.rdi;
    let transform = transform_argument(symbol, ctx, mem, 0)?;
    let (sx, sy) = (f64::from_bits(ctx.xmm[0][0]), f64::from_bits(ctx.xmm[1][0]));
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `CGAffineTransform (*)(CGAffineTransform, CGFloat, CGFloat)`.
    let f: TransformScaled = unsafe { core::mem::transmute::<u64, TransformScaled>(host) };
    let out = f(transform, sx, sy);
    return_transform(symbol, ctx, mem, buffer, out)
}

/// `CGAffineTransform CGAffineTransformTranslate(CGAffineTransform t,
/// CGFloat tx, CGFloat ty)`
fn transform_translate(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let buffer = ctx.rdi;
    let transform = transform_argument(symbol, ctx, mem, 0)?;
    let (tx, ty) = (f64::from_bits(ctx.xmm[0][0]), f64::from_bits(ctx.xmm[1][0]));
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `CGAffineTransform (*)(CGAffineTransform, CGFloat, CGFloat)`.
    let f: TransformScaled = unsafe { core::mem::transmute::<u64, TransformScaled>(host) };
    let out = f(transform, tx, ty);
    return_transform(symbol, ctx, mem, buffer, out)
}

/// `CGAffineTransform CGAffineTransformInvert(CGAffineTransform t)`
fn transform_invert(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let buffer = ctx.rdi;
    let transform = transform_argument(symbol, ctx, mem, 0)?;
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `CGAffineTransform (*)(CGAffineTransform)`.
    let f: Transform1 = unsafe { core::mem::transmute::<u64, Transform1>(host) };
    let out = f(transform);
    return_transform(symbol, ctx, mem, buffer, out)
}

/// `bool CGAffineTransformIsIdentity(CGAffineTransform t)`
fn transform_is_identity(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let transform = transform_argument(symbol, ctx, mem, 0)?;
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `bool (*)(CGAffineTransform)`.
    let f: TransformIdentity = unsafe { core::mem::transmute::<u64, TransformIdentity>(host) };
    // A `bool` is a byte, so only the low byte of `rax` carries it.
    ctx.rax = u64::from(f(transform));
    Ok(())
}

/// `CGRect CGRectApplyAffineTransform(CGRect rect, CGAffineTransform t)`
///
/// Both arguments are MEMORY class: the rectangle at `rsp + 0`, the transform
/// at `rsp + 32`.
fn rect_apply_transform(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let buffer = ctx.rdi;
    let rect = rect_argument(symbol, ctx, mem, 0)?;
    let transform = transform_argument(symbol, ctx, mem, 32)?;
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `CGRect (*)(CGRect, CGAffineTransform)`.
    let f: RectApply = unsafe { core::mem::transmute::<u64, RectApply>(host) };
    let out = f(rect, transform);
    if buffer == 0 {
        return Err(AbiError::Memory {
            symbol: symbol.to_string(),
            addr: 0,
        });
    }
    mem.write_bytes(buffer, &out.to_le_bytes(), symbol)?;
    ctx.rax = buffer;
    Ok(())
}

/// `CGPathRef CGPathCreateWithRect(CGRect rect, const CGAffineTransform *m)`
///
/// The rectangle is MEMORY class to a guest, so it is on the guest's stack and
/// the transform is the first integer register. The path is a pointer, so it
/// comes back in `rax` with no hidden buffer.
fn path_create_with_rect(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let rect = rect_argument(symbol, ctx, mem, 0)?;
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `CGPathRef (*)(CGRect, const CGAffineTransform *)`.
    let f: extern "C" fn(CgRect, u64) -> u64 =
        unsafe { core::mem::transmute::<u64, extern "C" fn(CgRect, u64) -> u64>(host) };
    ctx.rax = f(rect, ctx.rdi);
    Ok(())
}

/// `CGPathRef CGPathCreateWithRoundedRect(CGRect rect, CGFloat cornerWidth,
/// CGFloat cornerHeight, const CGAffineTransform *m)`
fn path_create_with_rounded_rect(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let rect = rect_argument(symbol, ctx, mem, 0)?;
    let (width, height) = (f64::from_bits(ctx.xmm[0][0]), f64::from_bits(ctx.xmm[1][0]));
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `CGPathRef (*)(CGRect, CGFloat, CGFloat, const CGAffineTransform *)`.
    let f: extern "C" fn(CgRect, f64, f64, u64) -> u64 =
        unsafe { core::mem::transmute::<u64, extern "C" fn(CgRect, f64, f64, u64) -> u64>(host) };
    ctx.rax = f(rect, width, height, ctx.rdi);
    Ok(())
}

/// `void CGPathAddRect(CGMutablePathRef path, const CGAffineTransform *m,
/// CGRect rect)`
fn path_add_rect(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let rect = rect_argument(symbol, ctx, mem, 0)?;
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `void (*)(CGMutablePathRef, const CGAffineTransform *, CGRect)`.
    let f: extern "C" fn(u64, u64, CgRect) =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64, u64, CgRect)>(host) };
    f(ctx.rdi, ctx.rsi, rect);
    Ok(())
}

/// `void CGPathAddRoundedRect(CGMutablePathRef path, const CGAffineTransform
/// *m, CGRect rect, CGFloat cornerWidth, CGFloat cornerHeight)`
fn path_add_rounded_rect(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let rect = rect_argument(symbol, ctx, mem, 0)?;
    let (width, height) = (f64::from_bits(ctx.xmm[0][0]), f64::from_bits(ctx.xmm[1][0]));
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `void (*)(CGMutablePathRef, const CGAffineTransform *, CGRect, CGFloat, CGFloat)`.
    let f: extern "C" fn(u64, u64, CgRect, f64, f64) =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64, u64, CgRect, f64, f64)>(host) };
    f(ctx.rdi, ctx.rsi, rect, width, height);
    Ok(())
}

/// `CGRect CGPathGetBoundingBox(CGPathRef path)`
///
/// A MEMORY-class return: the guest passes the buffer in `rdi` and the path
/// moves to `rsi`.
fn path_bounding_box(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let (buffer, path) = (ctx.rdi, ctx.rsi);
    if buffer == 0 {
        return Err(AbiError::Memory {
            symbol: symbol.to_string(),
            addr: 0,
        });
    }
    // SAFETY: the symbol [`shim_for`] matched, which is `CGRect (*)(CGPathRef)`.
    let f: extern "C" fn(u64) -> CgRect =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64) -> CgRect>(host) };
    let rect = f(path);
    mem.write_bytes(buffer, &rect.to_le_bytes(), symbol)?;
    ctx.rax = buffer;
    Ok(())
}

/// `CGPoint CGPathGetCurrentPoint(CGPathRef path)`
///
/// Sixteen bytes of two `double`s, which the two ABIs agree about: `xmm0` and
/// `xmm1` to a guest, `v0` and `v1` to a host.
fn path_current_point(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    // SAFETY: the symbol [`shim_for`] matched, which is `CGPoint (*)(CGPathRef)`.
    let f: extern "C" fn(u64) -> CgPoint =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64) -> CgPoint>(host) };
    let point = f(ctx.rdi);
    ctx.xmm[0] = [point.x.to_bits(), 0];
    ctx.xmm[1] = [point.y.to_bits(), 0];
    Ok(())
}

/// `bool CGPathContainsPoint(CGPathRef path, const CGAffineTransform *m,
/// CGPoint point, bool eoFill)`
///
/// The point takes the two SSE registers, so the flag takes the next *integer*
/// register - `rdx`, after the path and the transform.
fn path_contains_point(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let point = CgPoint {
        x: f64::from_bits(ctx.xmm[0][0]),
        y: f64::from_bits(ctx.xmm[1][0]),
    };
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `bool (*)(CGPathRef, const CGAffineTransform *, CGPoint, bool)`.
    let f: extern "C" fn(u64, u64, CgPoint, u8) -> u8 =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64, u64, CgPoint, u8) -> u8>(host) };
    ctx.rax = u64::from(f(ctx.rdi, ctx.rsi, point, ctx.rdx as u8));
    Ok(())
}

/// `void CGPathApplyWithBlock(CGPathRef path, CGPathApplierBlock block)`
///
/// The block's invoke pointer is a guest address until it is substituted, and
/// the host would call it as host code. `substitute_block` writes the thunk
/// into the block first, which is what every other block-taking shim does.
fn path_apply_with_block(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    let (path, block) = (ctx.rdi, ctx.rsi);
    crate::shims::block::substitute_block(symbol, block, mem, callbacks)?;
    // SAFETY: the symbol [`callback_shim_for`] matched, which is
    // `void (*)(CGPathRef, CGPathApplierBlock)`.
    let f: extern "C" fn(u64, u64) =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64, u64)>(host) };
    f(path, block);
    Ok(())
}

/// One of this module's implementations that needs [`Callbacks`].
pub type CallbackShim = crate::signal::Shim;

/// Whether `name` (underscore already stripped) is one of this module's
/// callback-taking shims.
pub fn callback_shim_for(name: &str) -> Option<CallbackShim> {
    match name {
        "CGPathApplyWithBlock" => {
            Some(bind!(callback "CGPathApplyWithBlock" => path_apply_with_block))
        },
        _ => None,
    }
}

/// `CGPoint`, as both ABIs return one: two floating-point registers.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct CgPoint {
    pub(crate) x: f64,
    pub(crate) y: f64,
}

/// `CVTime`, as both ABIs return one: two integer registers.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CvTime {
    time_value: i64,
    time_scale: i32,
    flags: i32,
}

/// `CVTime CVDisplayLinkGetNominalOutputVideoRefreshPeriod(CVDisplayLinkRef)`
fn display_link_nominal_period(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = host_or_refuse(symbol)?;
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `CVTime (*)(CVDisplayLinkRef)`, resolved out of a CoreVideo this process
    // has loaded. The argument is an opaque handle the guest got from
    // `CVDisplayLinkCreateWithActiveCGDisplays`, so it is the host's own.
    let f: extern "C" fn(u64) -> CvTime =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64) -> CvTime>(host) };
    let time = f(ctx.rdi);
    ctx.rax = time.time_value as u64;
    // The second eightbyte is the two `int32`s packed low-then-high, which is
    // what "one eightbyte" means for a pair of them.
    ctx.rdx = u64::from(time.time_scale as u32) | (u64::from(time.flags as u32) << 32);
    Ok(())
}

/// One of this module's implementations, in the shape [`crate::shims`] wants.
pub type Shim = crate::shims::Shim;

/// Whether `name` (underscore already stripped) is one of this module's.
pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        // `CGRect CGDisplayBounds(CGDirectDisplayID)` - where a display sits in
        // the global desktop coordinate space, which SDL asks for every display
        // it enumerates.
        "CGDisplayBounds" => Some(bind!("CGDisplayBounds" => rect_of_display)),
        // `CGSize CGDisplayScreenSize(CGDirectDisplayID)` - sixteen bytes rather
        // than thirty-two, so it is the agreeing case rather than the
        // disagreeing one, and it still cannot be a table entry.
        "CGDisplayScreenSize" => Some(bind!("CGDisplayScreenSize" => screen_size_of_display)),
        // The bitmap-context pair, which is how a `CGImage` becomes texture
        // bytes: a `CGRect` **argument**, passed on the guest's stack.
        "CGContextClearRect" => Some(bind!("CGContextClearRect" => context_clear_rect)),
        "CGContextDrawImage" => Some(bind!("CGContextDrawImage" => context_draw_image)),
        "CGEventGetLocation" => Some(bind!("CGEventGetLocation" => event_location)),
        "NSInsetRect" => Some(bind!("NSInsetRect" => inset_rect)),
        "CGRectEqualToRect" => Some(bind!("CGRectEqualToRect" => rect_equal)),
        "NSEqualRects" => Some(bind!("NSEqualRects" => rect_equal)),
        // A `CGPoint` **argument** in the middle of three scalars; see
        // [`event_create_mouse`] for why that moves `mouseButton` a register.
        "CGEventCreateMouseEvent" => Some(bind!("CGEventCreateMouseEvent" => event_create_mouse)),
        "CVDisplayLinkGetNominalOutputVideoRefreshPeriod" => Some(
            bind!("CVDisplayLinkGetNominalOutputVideoRefreshPeriod" => display_link_nominal_period),
        ),
        // The `CGRect` accessors: a rectangle in, a `CGFloat` out, all eight
        // the same shape.
        "CGRectGetHeight" => Some(bind!("CGRectGetHeight" => rect_accessor)),
        "CGRectGetWidth" => Some(bind!("CGRectGetWidth" => rect_accessor)),
        "CGRectGetMinX" => Some(bind!("CGRectGetMinX" => rect_accessor)),
        "CGRectGetMidX" => Some(bind!("CGRectGetMidX" => rect_accessor)),
        "CGRectGetMaxX" => Some(bind!("CGRectGetMaxX" => rect_accessor)),
        "CGRectGetMinY" => Some(bind!("CGRectGetMinY" => rect_accessor)),
        "CGRectGetMidY" => Some(bind!("CGRectGetMidY" => rect_accessor)),
        "CGRectGetMaxY" => Some(bind!("CGRectGetMaxY" => rect_accessor)),
        // `CGAffineTransform` is forty-eight bytes, so every one of these is
        // MEMORY class on both sides and cannot be a table entry.
        "CGAffineTransformMake" => Some(bind!("CGAffineTransformMake" => transform_make)),
        "CGAffineTransformMakeScale" => {
            Some(bind!("CGAffineTransformMakeScale" => transform_make_scale))
        }
        "CGAffineTransformMakeTranslation" => {
            Some(bind!("CGAffineTransformMakeTranslation" => transform_make_translation))
        }
        "CGAffineTransformConcat" => Some(bind!("CGAffineTransformConcat" => transform_concat)),
        "CGAffineTransformScale" => Some(bind!("CGAffineTransformScale" => transform_scale)),
        "CGAffineTransformTranslate" => {
            Some(bind!("CGAffineTransformTranslate" => transform_translate))
        }
        "CGAffineTransformInvert" => Some(bind!("CGAffineTransformInvert" => transform_invert)),
        "CGAffineTransformIsIdentity" => {
            Some(bind!("CGAffineTransformIsIdentity" => transform_is_identity))
        }
        "CGRectApplyAffineTransform" => {
            Some(bind!("CGRectApplyAffineTransform" => rect_apply_transform))
        }
        // `CGPath`'s aggregate forms. The rectangle is MEMORY class to a guest
        // and the point is two SSE eightbytes; neither can be a table entry.
        "CGPathCreateWithRect" => Some(bind!("CGPathCreateWithRect" => path_create_with_rect)),
        "CGPathCreateWithEllipseInRect" => {
            Some(bind!("CGPathCreateWithEllipseInRect" => path_create_with_rect))
        }
        "CGPathCreateWithRoundedRect" => {
            Some(bind!("CGPathCreateWithRoundedRect" => path_create_with_rounded_rect))
        }
        "CGPathAddRect" => Some(bind!("CGPathAddRect" => path_add_rect)),
        "CGPathAddEllipseInRect" => Some(bind!("CGPathAddEllipseInRect" => path_add_rect)),
        "CGPathAddRoundedRect" => Some(bind!("CGPathAddRoundedRect" => path_add_rounded_rect)),
        "CGPathGetBoundingBox" => Some(bind!("CGPathGetBoundingBox" => path_bounding_box)),
        "CGPathGetPathBoundingBox" => {
            Some(bind!("CGPathGetPathBoundingBox" => path_bounding_box))
        }
        "CGPathGetCurrentPoint" => Some(bind!("CGPathGetCurrentPoint" => path_current_point)),
        "CGPathContainsPoint" => Some(bind!("CGPathContainsPoint" => path_contains_point)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout claim, which is the only thing this module asserts about `CGRect` and
    /// the thing a wrong answer would silently violate.
    #[test]
    fn a_rect_is_four_doubles_in_order() {
        assert_eq!(core::mem::size_of::<CgRect>(), 32);
        let bytes = CgRect {
            x: 1.5,
            y: 2.5,
            width: 3.5,
            height: 4.5,
        }
        .to_le_bytes();
        for (i, expected) in [1.5f64, 2.5, 3.5, 4.5].iter().enumerate() {
            let mut word = [0u8; 8];
            word.copy_from_slice(&bytes[i * 8..(i + 1) * 8]);
            assert_eq!(f64::from_le_bytes(word), *expected);
        }
    }

    /// The layout claim, which is the only thing this module asserts about
    /// `CGAffineTransform` and the thing a wrong answer would silently violate.
    #[test]
    fn a_transform_is_six_doubles_in_order() {
        assert_eq!(core::mem::size_of::<CgTransform>(), 48);
        let bytes = CgTransform {
            a: 1.5,
            b: 2.5,
            c: 3.5,
            d: 4.5,
            tx: 5.5,
            ty: 6.5,
        }
        .to_le_bytes();
        for (i, expected) in [1.5f64, 2.5, 3.5, 4.5, 5.5, 6.5].iter().enumerate() {
            let mut word = [0u8; 8];
            word.copy_from_slice(&bytes[i * 8..(i + 1) * 8]);
            assert_eq!(f64::from_le_bytes(word), *expected);
        }
    }

    #[test]
    fn the_path_aggregates_are_shimmed() {
        for name in [
            "CGPathCreateWithRect",
            "CGPathCreateWithEllipseInRect",
            "CGPathCreateWithRoundedRect",
            "CGPathAddRect",
            "CGPathAddEllipseInRect",
            "CGPathAddRoundedRect",
            "CGPathGetBoundingBox",
            "CGPathGetPathBoundingBox",
            "CGPathGetCurrentPoint",
            "CGPathContainsPoint",
        ] {
            assert!(shim_for(name).is_some(), "{name} has no shim");
        }
    }

    #[test]
    fn the_applier_forms_are_reachable() {
        match crate::abi::table::lookup(0, "CGPathApply") {
            crate::abi::Lookup::Known(abi) => assert!(abi.callbacks.contains(&2)),
            other => panic!("CGPathApply is not described: {other:?}"),
        }
        assert!(callback_shim_for("CGPathApplyWithBlock").is_some());
    }

    /// Against the host, so the claim is about CoreGraphics rather than about this
    /// module's arithmetic.
    #[test]
    fn the_main_displays_bounds_land_in_the_guests_buffer() {
        let Some(_) = crate::host::symbol("CGDisplayBounds") else {
            // A headless builder has no CoreGraphics to ask.
            return;
        };
        let mut buffer = [0u8; 32];
        let at = buffer.as_mut_ptr() as usize as u64;
        let mut ctx = RegisterContext::new();
        ctx.rdi = at;
        // `CGMainDisplayID()` is 0's alias in every case that matters here; a
        // display ID this runtime made up would be refused by CoreGraphics and
        // return a zero rect, which the assertions below would catch.
        let display = host_main_display();
        ctx.rsi = u64::from(display);
        let buffer_region = at..at + 32;
        let mut mem = crate::vm::GuestView::new(vec![buffer_region]);
        rect_of_display("_CGDisplayBounds", &mut ctx, &mut mem).expect("bounds");

        assert_eq!(ctx.rax, at, "rax must return the hidden buffer");
        let read = |i: usize| {
            let mut w = [0u8; 8];
            w.copy_from_slice(&buffer[i * 8..(i + 1) * 8]);
            f64::from_le_bytes(w)
        };
        if display != 0 {
            assert!(read(2) > 0.0 && read(3) > 0.0, "a display has a size");
        }
    }

    /// The packing claim, which is the whole of what this shim does with
    /// `CVTime`'s second eightbyte: `timeScale` low, `flags` high.
    #[test]
    fn a_cvtime_second_eightbyte_is_the_two_int32s_low_then_high() {
        assert_eq!(core::mem::size_of::<CvTime>(), 16);
        let Some(_) = crate::host::symbol("CVDisplayLinkGetNominalOutputVideoRefreshPeriod") else {
            return;
        };
        // A null display link is the case CoreVideo answers without a display.
        let mut ctx = RegisterContext::new();
        ctx.rdi = 0;
        let mut mem = crate::vm::GuestView::new(Vec::new());
        display_link_nominal_period(
            "_CVDisplayLinkGetNominalOutputVideoRefreshPeriod",
            &mut ctx,
            &mut mem,
        )
        .expect("period");
        // `kCVTimeIsIndefinite` is bit 0 of `flags`, which lives in the high
        // half of `rdx`.
        let scale = ctx.rdx as u32;
        let flags = (ctx.rdx >> 32) as u32;
        assert!(
            flags & 1 == 1 || scale != 0,
            "a null link should be indefinite, or report a scale: rdx {:#x}",
            ctx.rdx
        );
    }

    fn host_main_display() -> u32 {
        let Some(f) = crate::host::symbol("CGMainDisplayID") else {
            return 0;
        };
        // SAFETY: `CGMainDisplayID` is `CGDirectDisplayID (*)(void)`.
        let f: extern "C" fn() -> u32 =
            unsafe { core::mem::transmute::<u64, extern "C" fn() -> u32>(f) };
        f()
    }
}
