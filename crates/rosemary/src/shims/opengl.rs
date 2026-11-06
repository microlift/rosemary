use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{GuestMemory, Result};
use crate::shims::bind;

/// Count CGL presentation without passing compatibility handles to the native
/// overlay. Register and memory handling stays in the runtime bridge.
fn flush_drawable(
    _symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    crate::stats::record_frame();
    // SAFETY: CGLFlushDrawable takes one opaque context and returns CGLError.
    let error = unsafe { appkit::opengl::CGLFlushDrawable(ctx.rdi as *mut std::ffi::c_void) };
    crate::shims::set_int_return(ctx, error);
    Ok(())
}

/// `void gluLookAt(GLdouble eyeX, ..., GLdouble upZ)`: nine doubles, so SysV puts
/// the first eight in `xmm0`-`xmm7` and `upZ` in the first stack slot.
fn look_at(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let host = crate::shims::host::builtin_or_host(symbol)?;
    let d = |i: usize| f64::from_bits(ctx.xmm[i][0]);
    let at = ctx.rsp.wrapping_add(crate::abi::marshal::stack_base());
    let up_z = f64::from_bits(mem.read_u64(at, symbol)?);
    type LookAt = extern "C" fn(f64, f64, f64, f64, f64, f64, f64, f64, f64);
    // SAFETY: the symbol [`shim_for`] matched, which is `gluLookAt`, resolved out
    // of this tree's own GLU or the host's, both with this prototype.
    let f: LookAt = unsafe { core::mem::transmute::<u64, LookAt>(host) };
    f(d(0), d(1), d(2), d(3), d(4), d(5), d(6), d(7), up_z);
    Ok(())
}

pub fn shim_for(name: &str) -> Option<super::Shim> {
    match name {
        "CGLFlushDrawable" if crate::frameworks::using_builtin() => {
            Some(bind!("CGLFlushDrawable" => flush_drawable))
        }
        "gluLookAt" => Some(bind!("gluLookAt" => look_at)),
        _ => None,
    }
}
