use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// The process's one context, made on first use.
///
/// Apple makes one per client view; one shared object is indistinguishable to a
/// caller, because [`client`] asks which view is focused rather than
/// remembering one.
fn shared() -> Id {
    static CONTEXT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CONTEXT.get_or_init(|| {
        let class =
            objc::class::lookup(c"NSTextInputContext").expect("declared before its methods");
        objc::object::create_instance(class, 0) as usize
    }) as Id
}

/// The key window's first responder, when it is a text input client.
fn focused_client() -> Id {
    let responder = crate::window::first_responder_of(crate::window::key_window());
    match crate::text::is_input_client(responder) {
        true => responder,
        false => std::ptr::null_mut(),
    }
}

/// `+[NSTextInputContext currentInputContext]`.
///
/// Nil when nothing focused takes text, as Apple's is: a program asks this to
/// decide whether key events are text, and a context with no client would say
/// they are.
unsafe extern "C" fn current_input_context(_class: Id, _sel: Sel) -> Id {
    match focused_client().is_null() {
        true => std::ptr::null_mut(),
        false => shared(),
    }
}

/// `-client`: the view text is going to.
unsafe extern "C" fn client(_receiver: Id, _sel: Sel) -> Id {
    focused_client()
}

/// `-discardMarkedText`, `-invalidateCharacterCoordinates`, `-activate` and
/// `-deactivate`: there is no composition session to end, and the text input
/// area follows the first responder from [`crate::window`] already.
unsafe extern "C" fn ignored(_receiver: Id, _sel: Sel) {}

/// `-[NSView inputContext]`: the context, for a view that takes text.
unsafe extern "C" fn view_input_context(receiver: Id, _sel: Sel) -> Id {
    match crate::text::is_input_client(receiver) {
        true => shared(),
        false => std::ptr::null_mut(),
    }
}

/// `-handleEvent:`: a key event goes to the client's `-interpretKeyEvents:`,
/// which is where a composition would start; anything else is not consumed.
unsafe extern "C" fn handle_event(_receiver: Id, _sel: Sel, event: Id) -> u8 {
    let client = focused_client();
    // SAFETY: `NSEventType (id, SEL)`.
    let kind = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());
        f(event, objc::sel::get("type"))
    };
    // `NSEventTypeKeyDown`.
    if client.is_null() || kind != 10 {
        return 0;
    }
    let class = objc::class::lookup(c"NSArray").expect("NSArray");
    // SAFETY: `id (Class, SEL, id)` and `void (id, SEL, NSArray *)`.
    unsafe {
        let with: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
        let events = with(
            objc::class::ptr(class) as Id,
            objc::sel::get("arrayWithObject:"),
            event,
        );
        let interpret: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        interpret(client, objc::sel::get("interpretKeyEvents:"), events);
    }
    1
}

/// `-selectedKeyboardInputSource`: the one layout this host reports.
unsafe extern "C" fn selected_keyboard_input_source(_receiver: Id, _sel: Sel) -> Id {
    foundation::notification::name_object(crate::textinput::SOURCE_ID) as Id
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSTextInputContext").expect("declared before its methods");
    class_methods!(class, {
        "currentInputContext" => current_input_context, c"@16@0:8";
    });
    methods!(class, {
        "client" => client, c"@16@0:8";
        "discardMarkedText" => ignored, c"v16@0:8";
        "invalidateCharacterCoordinates" => ignored, c"v16@0:8";
        "activate" => ignored, c"v16@0:8";
        "deactivate" => ignored, c"v16@0:8";
        "selectedKeyboardInputSource" => selected_keyboard_input_source, c"@16@0:8";
        "handleEvent:" => handle_event, c"c24@0:8@16";
    });
    let view = objc::class::lookup(c"NSView").expect("declared before its methods");
    methods!(view, {
        "inputContext" => view_input_context, c"@16@0:8";
    });
}
