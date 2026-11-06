//! `NSInputManager`, deprecated since 10.6 in favour of `NSTextInputContext`
//! (`crate::input_context`) and not otherwise used here: just enough that a
//! program which still asks for it, rather than crashing on the import, gets
//! an object that never has marked text and never claims an event.

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

static SHARED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// `+currentInputManager`: one process-lifetime instance, since nothing here
/// ever switches input managers.
unsafe extern "C" fn current_input_manager(class: Id, _sel: Sel) -> Id {
    *SHARED.get_or_init(|| {
        // SAFETY: the receiver of a class method is a class this runtime knows.
        let class = unsafe { objc::class::from_ptr(class as *const objc::class::Class) }
            .expect("NSInputManager is declared");
        let manager = objc::object::create_instance(class, 0);
        objc::arc::make_immortal(manager);
        manager as usize
    }) as Id
}

/// `-wantsToHandleMouseEvents`: nothing here routes mouse events through an
/// input manager instead of the normal responder chain.
unsafe extern "C" fn wants_to_handle_mouse_events(_receiver: Id, _sel: Sel) -> u8 {
    0
}

/// `-handleMouseEvent:`: never called, since [`wants_to_handle_mouse_events`]
/// says no.
unsafe extern "C" fn handle_mouse_event(_receiver: Id, _sel: Sel, _event: Id) {}

/// `-handleEvent:`: not handled - `crate::input_context` is where composition
/// actually happens.
unsafe extern "C" fn handle_event(_receiver: Id, _sel: Sel, _event: Id) -> u8 {
    0
}

/// `-markedTextAttribute`: no marked text is ever produced through this class.
unsafe extern "C" fn marked_text_attribute(_receiver: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

unsafe extern "C" fn marked_text_abandoned(_receiver: Id, _sel: Sel, _client: Id) {}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSInputManager").expect("declared before its methods");
    class_methods!(class, {
        "currentInputManager" => current_input_manager, c"@16@0:8";
    });
    methods!(class, {
        "wantsToHandleMouseEvents" => wants_to_handle_mouse_events, c"B16@0:8";
        "handleMouseEvent:" => handle_mouse_event, c"v24@0:8@16";
        "handleEvent:" => handle_event, c"B24@0:8@16";
        "markedTextAttribute" => marked_text_attribute, c"@16@0:8";
        // Nothing is ever being composed here, so there is nothing to abandon.
        "markedTextAbandoned:" => marked_text_abandoned, c"v24@0:8@16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_input_manager_is_one_instance_reused() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSInputManager").expect("declared");
        let class_id = objc::class::ptr(class) as Id;
        let sel = objc::sel::get("currentInputManager");
        let first = unsafe { current_input_manager(class_id, sel) };
        let second = unsafe { current_input_manager(class_id, sel) };
        assert!(!first.is_null());
        assert_eq!(first, second);
    }

    #[test]
    fn it_never_claims_an_event() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSInputManager").expect("declared");
        let manager = objc::object::create_instance(class, 0);
        assert_eq!(
            unsafe {
                handle_event(
                    manager,
                    objc::sel::get("handleEvent:"),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        objc::arc::release(manager);
    }
}
