use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{GuestMemory, Result};
use crate::shims::bind;
use crate::shims::coregraphics::Shim;

/// The selectors a present goes through, uniqued once.
struct Selectors {
    /// The three spellings that carry a command buffer *and* a drawable.
    present_drawable: [u64; 3],
    /// The three that a drawable answers on its own, with the guest's command
    /// buffer already committed. See [`super::metal::present_standalone`].
    present_self: [u64; 3],
    /// Not a present at all, and watched anyway: it is where the guest's command
    /// queue is learned, which is what the previous line needs.
    commit: u64,
    flush_buffer: u64,
}

fn selectors() -> &'static Selectors {
    static SELECTORS: std::sync::OnceLock<Selectors> = std::sync::OnceLock::new();
    SELECTORS.get_or_init(|| {
        let sel = |name| super::objc::sel(name) as u64;
        Selectors {
            present_drawable: [
                sel("presentDrawable:"),
                sel("presentDrawable:atTime:"),
                sel("presentDrawable:afterMinimumDuration:"),
            ],
            present_self: [
                sel("present"),
                sel("presentAtTime:"),
                sel("presentAfterMinimumDuration:"),
            ],
            commit: sel("commit"),
            flush_buffer: sel("flushBuffer"),
        }
    })
}

/// Selectors reported the first time the guest sends one, under `ROSEMARY_GUI_DEBUG`.
const WATCHED: &[&str] = &[
    "presentDrawable:",
    "presentDrawable:atTime:",
    "presentDrawable:afterMinimumDuration:",
    "present",
    "presentAtTime:",
    "presentAfterMinimumDuration:",
    "commit",
    "commandQueue",
    "nextDrawable",
    "currentDrawable",
    "flushBuffer",
    "CGLContextObj",
];

fn watched() -> &'static [(u64, &'static str, std::sync::atomic::AtomicBool)] {
    static TABLE: std::sync::OnceLock<Vec<(u64, &'static str, std::sync::atomic::AtomicBool)>> =
        std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        WATCHED
            .iter()
            .map(|&name| {
                (
                    super::objc::sel(name) as u64,
                    name,
                    std::sync::atomic::AtomicBool::new(false),
                )
            })
            .collect()
    })
}

/// Report the first time the guest sends each watched selector.
fn observe(sel: u64) {
    for (candidate, name, said) in watched() {
        if *candidate == sel && !said.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!("rosemary: gui  the guest sends -[... {name}]");
        }
    }
}

/// Called for every message the guest sends, before it is dispatched.
pub fn before_send(sel: u64, receiver: u64, argument: u64) {
    let selectors = selectors();
    // Counted before the `armed` gate below, and deliberately: the frame rate is
    // what the runtime is judged on, so reading it must not require turning on the
    // overlay - which costs frames of its own and so cannot be the only way to see
    // how many there were. Seven pointer compares on a path that has already done
    // a class lookup and is about to do a method search.
    if sel == selectors.flush_buffer
        || selectors.present_drawable.contains(&sel)
        || selectors.present_self.contains(&sel)
    {
        crate::stats::record_frame();
    }
    if !super::armed() {
        return;
    }
    if super::debugging() {
        observe(sel);
    }
    if selectors.present_drawable.contains(&sel) {
        super::guarded(|| {
            // SAFETY: this is the `presentDrawable:` the guest is sending, on
            // the thread sending it, so the receiver is its command buffer and
            // the argument is the drawable - which is exactly what
            // [`super::metal::present`] requires.
            unsafe { super::metal::present(receiver as *mut _, argument as *mut _) };
        });
        return;
    }
    if selectors.present_self.contains(&sel) {
        super::guarded(|| {
            // SAFETY: a live receiver on the thread messaging it, which is all
            // `present_standalone` asks - it checks for itself whether the
            // receiver is really a drawable, because `present` is a common
            // enough name that some other class may have one.
            unsafe { super::metal::present_standalone(receiver as *mut _) };
        });
        return;
    }
    if sel == selectors.commit {
        super::guarded(|| {
            // SAFETY: as above; `note_queue` checks whether the receiver is
            // really a command buffer before messaging it further.
            unsafe { super::metal::note_queue(receiver as *mut _) };
        });
        return;
    }
    if sel == selectors.flush_buffer {
        // The context's identity is its `CGLContextObj`, which is also what the
        // `CGLFlushDrawable` path has - so a program that uses both spellings
        // for one context shares one set of overlay resources rather than
        // building two.
        //
        // SAFETY: `-CGLContextObj` is `NSOpenGLContext`'s, takes nothing and
        // returns an opaque pointer. The receiver responds to `flushBuffer`,
        // which is that class's method.
        let context = unsafe {
            super::objc::send0::<*mut std::ffi::c_void>(
                receiver as *mut _,
                super::objc::sel("CGLContextObj"),
            )
        };
        super::guarded(|| super::gl::present(context as usize));
    }
}

/// `CGLError CGLFlushDrawable(CGLContextObj ctx)`
fn flush_drawable(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    let context = ctx.rdi;
    // As in [`before_send`]: the frame is counted whether or not the overlay is
    // drawing on it.
    crate::stats::record_frame();
    if !crate::frameworks::using_builtin() && super::armed() {
        super::guarded(|| super::gl::present(context as usize));
    }
    let host = crate::shims::host::host_or_refuse(symbol)?;
    // SAFETY: the symbol [`shim_for`] matched, whose declaration in
    // `<OpenGL/OpenGL.h>` is `CGLError (*)(CGLContextObj)`. The selected
    // framework provider supplies the matching implementation and handle.
    let f: extern "C" fn(u64) -> i32 =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64) -> i32>(host) };
    crate::shims::set_int_return(ctx, f(context));
    Ok(())
}

/// Whether `name` (underscore already stripped) is one of this module's.
pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        // Installed even with the overlay off, because it is also where a frame
        // is counted on the CGL path. The shim calls straight through to the
        // selected framework's `CGLFlushDrawable`, so the only cost of taking it
        // when nothing is drawing is one indirection per presented frame.
        "CGLFlushDrawable" => Some(bind!("CGLFlushDrawable" => flush_drawable)),
        _ => None,
    }
}
