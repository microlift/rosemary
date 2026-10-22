//! `NSPort`, the abstract class: what `-[NSRunLoop addPort:forMode:]` schedules,
//! and what announces its own invalidation with
//! `NSPortDidBecomeInvalidNotification`.
//!
//! The concrete ports - `NSMachPort`, `NSMessagePort`, `NSSocketPort` - are not
//! here. A program's own subclass is, which is what the abstract class is for.

use std::ffi::c_void;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

/// `NSPortDidBecomeInvalidNotification`.
pub(crate) const DID_BECOME_INVALID: &str = "NSPortDidBecomeInvalidNotification";

#[repr(C)]
struct Port {
    isa: *const c_void,
    /// Zero while valid, so a freshly allocated port is a valid one.
    invalid: usize,
    /// Not retained, as Apple's is not.
    delegate: Id,
}

fn port_of<'a>(receiver: Id) -> &'a mut Port {
    // SAFETY: an `NSPort`, or a subclass laid out after it.
    unsafe { &mut *(receiver as *mut Port) }
}

/// `-invalidate`: "Marks the receiver as invalid and posts an
/// NSPortDidBecomeInvalidNotification to the default notification center."
/// Once: a second call finds it invalid already.
unsafe extern "C" fn invalidate(receiver: Id, _sel: Sel) {
    let port = port_of(receiver);
    if port.invalid != 0 {
        return;
    }
    port.invalid = 1;
    crate::notification::post(DID_BECOME_INVALID, receiver, std::ptr::null_mut());
}

unsafe extern "C" fn is_valid(receiver: Id, _sel: Sel) -> u8 {
    (port_of(receiver).invalid == 0) as u8
}

/// `-scheduleInRunLoop:forMode:` and `-removeFromRunLoop:forMode:`: what a
/// subclass with input to deliver overrides. The abstract port has none.
unsafe extern "C" fn schedule_in_run_loop(_receiver: Id, _sel: Sel, _run_loop: Id, _mode: Id) {}

unsafe extern "C" fn remove_from_run_loop(_receiver: Id, _sel: Sel, _run_loop: Id, _mode: Id) {}

unsafe extern "C" fn set_delegate(receiver: Id, _sel: Sel, delegate: Id) {
    port_of(receiver).delegate = delegate;
}

unsafe extern "C" fn delegate(receiver: Id, _sel: Sel) -> Id {
    port_of(receiver).delegate
}

/// `-reservedSpaceLength`: no header room, which is the abstract class's answer.
unsafe extern "C" fn reserved_space_length(_receiver: Id, _sel: Sel) -> usize {
    0
}

pub(crate) fn install() {
    let class = corefoundation::base::register(c"NSPort", None, size_of::<Port>(), false);
    methods!(class, {
        "invalidate" => invalidate, c"v16@0:8";
        "isValid" => is_valid, c"c16@0:8";
        "scheduleInRunLoop:forMode:" => schedule_in_run_loop, c"v32@0:8@16@24";
        "removeFromRunLoop:forMode:" => remove_from_run_loop, c"v32@0:8@16@24";
        "setDelegate:" => set_delegate, c"v24@0:8@16";
        "delegate" => delegate, c"@16@0:8";
        "reservedSpaceLength" => reserved_space_length, c"Q16@0:8";
    });
}
