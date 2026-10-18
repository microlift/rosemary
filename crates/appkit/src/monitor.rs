use std::sync::Mutex;

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::class_methods;

struct Monitor {
    token: Id,
    mask: u64,
    /// A heap copy this table owns until `+removeMonitor:`, as Apple's is.
    block: usize,
}

// SAFETY: the block is a copy this table owns and the token is an object of
// this crate's, neither of which is thread-affine.
unsafe impl Send for Monitor {}

static MONITORS: Mutex<Vec<Monitor>> = Mutex::new(Vec::new());

/// The class of the token handed back, which a guest may only hand back again.
fn token_class() -> &'static Class {
    corefoundation::base::abstract_class(c"__NSEventMonitor", None)
}

/// Run every monitor whose mask takes this event, and say whether it survived.
///
/// A handler answers with an event to let it through and nil to swallow it. The
/// answer is read for that decision alone and not put in the event's place: a
/// handler that returns a *different* event is returning one this runtime has no
/// mapping for, and dropping the one in hand would lose the keystroke.
pub(crate) fn allows(event: Id) -> bool {
    let kind = crate::nsevent::kind_of(event);
    let taking: Vec<usize> = MONITORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|m| kind < 64 && m.mask & (1 << kind) != 0)
        // Copied under the lock: a `+removeMonitor:` from inside another
        // handler would otherwise free a block this is about to call.
        .map(|m| objc::block::copy(m.block as Id) as usize)
        .collect();
    let mut allowed = true;
    for block in taking {
        // SAFETY: a block literal of `NSEvent *(^)(NSEvent *)`, whose `invoke`
        // the `@?` in the method encoding has already made callable.
        allowed &= !unsafe { call(block as Id, event) }.is_null();
        objc::block::release(block as Id);
    }
    allowed
}

/// Call an `id (^)(id)`.
///
/// # Safety
///
/// `block` must be null or a block literal of that prototype.
unsafe fn call(block: Id, argument: Id) -> Id {
    // SAFETY: the caller's obligation.
    let Some(invoke) = (unsafe { objc::block::invoke(block) }) else {
        return argument;
    };
    unsafe {
        let f: unsafe extern "C" fn(Id, Id) -> Id = std::mem::transmute(invoke);
        f(block, argument)
    }
}

unsafe extern "C" fn add_local_monitor(_class: Id, _sel: Sel, mask: u64, handler: Id) -> Id {
    let block = objc::block::copy(handler) as usize;
    if block == 0 {
        // Apple's answers a token for a nil handler, and removing it does
        // nothing. Nothing to call later either, so nothing is recorded.
        return std::ptr::null_mut();
    }
    let token = objc::object::create_instance(token_class(), 0);
    MONITORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Monitor { token, mask, block });
    if crate::event::tracing() {
        eprintln!("rosemary: appkit: +addLocalMonitorForEventsMatchingMask: {mask:#x}");
    }
    token
}

unsafe extern "C" fn remove_monitor(_class: Id, _sel: Sel, token: Id) {
    let mut monitors = MONITORS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(at) = monitors.iter().position(|m| m.token == token) else {
        return;
    };
    let gone = monitors.remove(at);
    drop(monitors);
    objc::block::release(gone.block as Id);
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSEvent").expect("declared before its methods");
    class_methods!(class, {
        "addLocalMonitorForEventsMatchingMask:handler:" => add_local_monitor, c"@32@0:8Q16@?24";
        "removeMonitor:" => remove_monitor, c"v24@0:8@16";
    });
}
