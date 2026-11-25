use std::ffi::c_void;

use crate::abi::Result;

/// `CGLError CGLFlushDrawable(CGLContextObj ctx)`, counting the frame and
/// drawing the overlay over it.
unsafe extern "C" fn flush_drawable(context: *mut c_void) -> i32 {
    crate::stats::record_frame();
    #[cfg(target_os = "macos")]
    crate::gui::hook::before_flush(context);
    // SAFETY: the caller's own context, passed through as `CGLFlushDrawable`
    // takes it.
    unsafe { appkit::opengl::CGLFlushDrawable(context) }
}

fn target() -> Result<u64> {
    Ok(appkit::opengl::CGLFlushDrawable as *const () as u64)
}

pub fn function_for(name: &str) -> Option<super::Function> {
    (crate::frameworks::using_builtin() && name == "CGLFlushDrawable")
        .then(|| super::Function::new(name, flush_drawable as *const () as u64, target))
        .flatten()
}
