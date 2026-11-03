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

pub fn shim_for(name: &str) -> Option<super::Shim> {
    (crate::frameworks::using_builtin() && name == "CGLFlushDrawable")
        .then_some(bind!("CGLFlushDrawable" => flush_drawable))
}
