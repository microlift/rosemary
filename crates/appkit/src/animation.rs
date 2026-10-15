use std::ffi::c_void;
use std::sync::OnceLock;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// Byte offset of `invoke` in a block literal, after `isa`, `flags` and
/// `reserved`.
const BLOCK_INVOKE_OFFSET: usize = 16;

/// The block's `invoke`, already made callable by the `@?` in the method's
/// encoding.
///
/// # Safety
///
/// `block` must be null or a block literal.
pub(crate) unsafe fn invoke(block: Id) -> Option<*const c_void> {
    if block.is_null() {
        return None;
    }
    let invoke = unsafe { *((block as *const u8).add(BLOCK_INVOKE_OFFSET) as *const usize) };
    (invoke != 0).then_some(invoke as *const c_void)
}

/// The one context, which is every thread's current one: with no frames to
/// animate, there is no per-group state that could differ between them.
fn current() -> Id {
    static CURRENT: OnceLock<usize> = OnceLock::new();
    *CURRENT.get_or_init(|| {
        let class = objc::class::lookup(c"NSAnimationContext").expect("declared");
        let context = objc::object::create_instance(class, 0);
        objc::arc::make_immortal(context);
        context as usize
    }) as Id
}

unsafe extern "C" fn current_context(_class: Id, _sel: Sel) -> Id {
    current()
}

unsafe extern "C" fn run_animation_group(_class: Id, _sel: Sel, changes: Id, completion: Id) {
    // SAFETY: `void (^)(NSAnimationContext *)`, as the selector declares.
    if let Some(f) = unsafe { invoke(changes) } {
        unsafe {
            let f: unsafe extern "C" fn(Id, Id) = std::mem::transmute(f);
            f(changes, current());
        }
    }
    // SAFETY: `void (^)(void)`, as the selector declares.
    if let Some(f) = unsafe { invoke(completion) } {
        unsafe {
            let f: unsafe extern "C" fn(Id) = std::mem::transmute(f);
            f(completion);
        }
    }
}

unsafe extern "C" fn run_animation_group_only(class: Id, sel: Sel, changes: Id) {
    // SAFETY: the same call with no completion handler.
    unsafe { run_animation_group(class, sel, changes, std::ptr::null_mut()) };
}

unsafe extern "C" fn ignored(_receiver: Id, _sel: Sel) {}

unsafe extern "C" fn ignored_double(_receiver: Id, _sel: Sel, _value: f64) {}

unsafe extern "C" fn ignored_object(_receiver: Id, _sel: Sel, _value: Id) {}

unsafe extern "C" fn ignored_flag(_receiver: Id, _sel: Sel, _value: u8) {}

unsafe extern "C" fn zero_duration(_receiver: Id, _sel: Sel) -> f64 {
    0.0
}

unsafe extern "C" fn no(_receiver: Id, _sel: Sel) -> u8 {
    0
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSAnimationContext").expect("declared before its methods");
    class_methods!(class, {
        "currentContext" => current_context, c"@16@0:8";
        "runAnimationGroup:completionHandler:" => run_animation_group, c"v32@0:8@?16@?24";
        "runAnimationGroup:" => run_animation_group_only, c"v24@0:8@?16";
        "beginGrouping" => ignored, c"v16@0:8";
        "endGrouping" => ignored, c"v16@0:8";
    });
    // The group's settings are accepted and have nothing to shape.
    methods!(class, {
        "setDuration:" => ignored_double, c"v24@0:8d16";
        "duration" => zero_duration, c"d16@0:8";
        "setTimingFunction:" => ignored_object, c"v24@0:8@16";
        "setAllowsImplicitAnimation:" => ignored_flag, c"v20@0:8c16";
        "allowsImplicitAnimation" => no, c"c16@0:8";
    });
}
