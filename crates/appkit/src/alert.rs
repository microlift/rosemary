use std::ffi::c_void;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
pub struct Alert {
    isa: *const c_void,
    message: Id,
    informative: Id,
    /// The button titles, in the order `-addButtonWithTitle:` was sent them.
    /// Owned by the alert, and null until the first one is added.
    buttons: *mut Vec<String>,
    /// The view a program adds to the dialog to put its own controls in,
    /// retained. Its supported controls are presented by `-runModal`.
    accessory: Id,
    style: usize,
}

pub const INSTANCE_SIZE: usize = size_of::<Alert>();

/// `NSAlertFirstButtonReturn`. The rest count up from it.
const FIRST_BUTTON_RETURN: i64 = 1000;

/// `NSAlertDefaultReturn`, `NSAlertAlternateReturn` and `NSAlertOtherReturn`:
/// what the legacy `NSRunAlertPanel` function answers for each of its buttons.
pub const ALERT_DEFAULT_RETURN: i64 = 1;
pub const ALERT_ALTERNATE_RETURN: i64 = 0;
pub const ALERT_OTHER_RETURN: i64 = -1;

/// `NSAlertErrorReturn`, returned when no panel could be displayed.
const ALERT_ERROR_RETURN: i64 = -2;

/// Display the legacy `NSRunAlertPanel` UI after its format has been rendered.
///
/// The title is the alert's message and the rendered format its informative
/// text, as AppKit lays the function out today. A missing default button is
/// OK; a missing alternate or other button is left out.
pub fn run_alert_panel(
    title: &[u8],
    message: &[u8],
    default: Option<&[u8]>,
    alternate: Option<&[u8]>,
    other: Option<&[u8]>,
) -> i64 {
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    let mut buttons = vec![(
        default.map_or_else(|| "OK".to_string(), text),
        ALERT_DEFAULT_RETURN,
    )];
    buttons.extend(alternate.map(|t| (text(t), ALERT_ALTERNATE_RETURN)));
    buttons.extend(other.map(|t| (text(t), ALERT_OTHER_RETURN)));
    let request = crate::dialog::Request {
        title: text(title),
        message: text(title),
        informative: text(message),
        buttons: buttons.iter().map(|(title, _)| title.clone()).collect(),
        cancel: None,
        application_icon: true,
        ..Default::default()
    };
    match crate::dialog::run(&request) {
        Some(chosen) => buttons[chosen].1,
        None => ALERT_ERROR_RETURN,
    }
}

fn alert_of<'a>(receiver: Id) -> &'a mut Alert {
    // SAFETY: an alert of this crate's, which is what every method here is sent
    // to.
    unsafe { &mut *(receiver as *mut Alert) }
}

/// Replace a retained field, in the order that survives being handed itself.
fn set(slot: &mut Id, value: Id) {
    let previous = *slot;
    *slot = objc::arc::retain(value);
    objc::arc::release(previous);
}

unsafe extern "C" fn alert(class: Id, _sel: Sel) -> Id {
    // SAFETY: `class` is `NSAlert`, which this crate registered.
    let Some(class) = (unsafe { objc::class::from_ptr(class as *const objc::class::Class) }) else {
        return std::ptr::null_mut();
    };
    objc::arc::autorelease(objc::object::create_instance(class, 0))
}

/// `+alertWithError:`: the error's description as the message and its
/// recovery suggestion as the informative text.
unsafe extern "C" fn alert_with_error(class: Id, sel: Sel, error: Id) -> Id {
    // SAFETY: the same class.
    let receiver = unsafe { alert(class, sel) };
    if receiver.is_null() || error.is_null() {
        return receiver;
    }
    // SAFETY: both getters are `id (id, SEL)` on an `NSError`.
    let text = |getter: &str| unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(error, objc::sel::get(getter))
    };
    let alert = alert_of(receiver);
    set(&mut alert.message, text("localizedDescription"));
    set(&mut alert.informative, text("localizedRecoverySuggestion"));
    receiver
}

/// `-[NSResponder presentError:]`, which AppKit passes up the responder chain
/// to the application to show as an alert. No error here offers a recovery,
/// so the answer is NO.
unsafe extern "C" fn present_error(_receiver: Id, _sel: Sel, error: Id) -> u8 {
    let Some(class) = objc::class::lookup(c"NSAlert") else {
        return 0;
    };
    // SAFETY: the alert class and an `NSError`.
    unsafe {
        let alert = alert_with_error(
            class as *const _ as Id,
            objc::sel::get("alertWithError:"),
            error,
        );
        if !alert.is_null() {
            run_modal(alert, objc::sel::get("runModal"));
        }
    }
    0
}

unsafe extern "C" fn set_message_text(receiver: Id, _sel: Sel, text: Id) {
    set(&mut alert_of(receiver).message, text);
}

unsafe extern "C" fn message_text(receiver: Id, _sel: Sel) -> Id {
    alert_of(receiver).message
}

unsafe extern "C" fn set_informative_text(receiver: Id, _sel: Sel, text: Id) {
    set(&mut alert_of(receiver).informative, text);
}

unsafe extern "C" fn informative_text(receiver: Id, _sel: Sel) -> Id {
    alert_of(receiver).informative
}

/// `-setAccessoryView:`, which is how a program puts its own controls - a
/// checkbox, a pop-up of resolutions - into an alert.
///
unsafe extern "C" fn set_accessory_view(receiver: Id, _sel: Sel, view: Id) {
    set(&mut alert_of(receiver).accessory, view);
}

unsafe extern "C" fn accessory_view(receiver: Id, _sel: Sel) -> Id {
    alert_of(receiver).accessory
}

/// `-layout`; the chrome dialog sizes itself when `-runModal` opens it.
unsafe extern "C" fn layout(_receiver: Id, _sel: Sel) {}

unsafe extern "C" fn set_alert_style(receiver: Id, _sel: Sel, style: usize) {
    alert_of(receiver).style = style;
}

unsafe extern "C" fn alert_style(receiver: Id, _sel: Sel) -> usize {
    alert_of(receiver).style
}

/// `-addButtonWithTitle:`, which answers the `NSButton` AppKit made.
///
/// nil here: there is no button object, because there is no dialog to put one
/// in. A caller that keeps the result and configures it is configuring nothing,
/// which is the same amount of nothing the undisplayed dialog shows.
unsafe extern "C" fn add_button_with_title(receiver: Id, _sel: Sel, title: Id) -> Id {
    let alert = alert_of(receiver);
    if alert.buttons.is_null() {
        alert.buttons = Box::into_raw(Box::new(Vec::new()));
    }
    // SAFETY: the title is an `NSString` or nil, and `buttons` was just made.
    unsafe { (*alert.buttons).push(text_of(title)) };
    std::ptr::null_mut()
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let alert = alert_of(receiver);
    objc::arc::release(alert.message);
    objc::arc::release(alert.informative);
    objc::arc::release(alert.accessory);
    if !alert.buttons.is_null() {
        // SAFETY: `-dealloc` is reached with the last reference gone, and the
        // pointer came from `Box::into_raw`.
        drop(unsafe { Box::from_raw(alert.buttons) });
        alert.buttons = std::ptr::null_mut();
    }
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn run_modal(receiver: Id, _sel: Sel) -> i64 {
    let alert = alert_of(receiver);
    // SAFETY: each is an `NSString` a setter retained, or nil.
    let message = unsafe { text_of(alert.message) };
    let informative = unsafe { text_of(alert.informative) };
    // SAFETY: the vector this crate built, or null.
    let buttons: &[String] = match alert.buttons.is_null() {
        true => &[],
        false => unsafe { &*alert.buttons },
    };
    let mut accessory_buttons = Vec::new();
    collect_accessory_buttons(alert.accessory, &mut accessory_buttons);

    eprintln!("rosemary: guest alert: {message}");
    if !informative.is_empty() {
        eprintln!("rosemary: guest alert: {informative}");
    }
    if !buttons.is_empty() {
        eprintln!("rosemary: guest alert: buttons {}", buttons.join(" / "));
    }

    let request = crate::dialog::Request {
        title: message.clone(),
        message: message.clone(),
        informative: informative.clone(),
        buttons: match buttons.is_empty() {
            true => vec!["OK".to_string()],
            false => buttons.to_vec(),
        },
        cancel: None,
        application_icon: true,
        accessories: accessory_buttons
            .iter()
            .map(|(_, title, checked)| chrome::alert::Accessory {
                title: title.clone(),
                checked: *checked,
            })
            .collect(),
        ..Default::default()
    };
    // With no dialog to show, the second button is the one that declines;
    // with fewer than two there is nothing to decline with.
    let (index, _, states) = crate::dialog::run_with_accessories(&request).unwrap_or_else(|| {
        (
            if buttons.len() >= 2 { 1 } else { 0 },
            false,
            accessory_buttons
                .iter()
                .map(|(_, _, checked)| *checked)
                .collect(),
        )
    });
    for ((button, _, _), checked) in accessory_buttons.iter().zip(states) {
        crate::controls::set_accessory_checkbox(*button, checked);
    }
    let answer = FIRST_BUTTON_RETURN + index as i64;
    let chosen = buttons.get(index).map_or("<none>", |t| t.as_str());
    eprintln!("rosemary: guest alert: answered {answer} ({chosen})");
    answer
}

fn collect_accessory_buttons(view: Id, result: &mut Vec<(Id, String, bool)>) {
    if !crate::view::is_view(view) || crate::view::is_hidden_or_has_hidden_ancestor(view) {
        return;
    }
    if let Some((title, checked)) = crate::controls::accessory_checkbox(view) {
        result.push((view, title, checked));
    }
    for child in crate::view::subviews_of(view) {
        collect_accessory_buttons(child, result);
    }
}

/// # Safety
///
/// `string` must be null or a string object.
unsafe fn text_of(string: Id) -> String {
    if string.is_null() {
        return String::new();
    }
    String::from_utf8_lossy(&unsafe { foundation::string::bytes(string) }).into_owned()
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSAlert").expect("declared in `classes`");
    class_methods!(class, {
        "alert" => alert, c"@16@0:8";
        "alertWithError:" => alert_with_error, c"@24@0:8@16";
    });
    let responder = objc::class::lookup(c"NSResponder").expect("declared in `classes`");
    methods!(responder, {
        "presentError:" => present_error, c"c24@0:8@16";
    });
    methods!(class, {
        "setMessageText:" => set_message_text, c"v24@0:8@16";
        "messageText" => message_text, c"@16@0:8";
        "setInformativeText:" => set_informative_text, c"v24@0:8@16";
        "informativeText" => informative_text, c"@16@0:8";
        "setAlertStyle:" => set_alert_style, c"v24@0:8Q16";
        "alertStyle" => alert_style, c"Q16@0:8";
        "addButtonWithTitle:" => add_button_with_title, c"@24@0:8@16";
        "setAccessoryView:" => set_accessory_view, c"v24@0:8@16";
        "accessoryView" => accessory_view, c"@16@0:8";
        "layout" => layout, c"v16@0:8";
        "runModal" => run_modal, c"q16@0:8";
        "dealloc" => dealloc, c"v16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessory_view_collects_nested_checkboxes() {
        crate::bootstrap();
        let root = objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0);
        let button = objc::object::create_instance(objc::class::lookup(c"NSButton").unwrap(), 0);
        let title = foundation::string::from_str("Remember choice");
        // SAFETY: the selectors have object and integer arguments respectively.
        unsafe {
            let object: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            let integer: unsafe extern "C" fn(Id, Sel, usize) =
                std::mem::transmute(objc::msg::entry());
            object(button, objc::sel::get("setTitle:"), title);
            integer(button, objc::sel::get("setButtonType:"), 3);
            object(root, objc::sel::get("addSubview:"), button);
        }
        let mut found = Vec::new();
        collect_accessory_buttons(root, &mut found);
        assert_eq!(found, vec![(button, "Remember choice".into(), false)]);
        objc::arc::release(root);
        objc::arc::release(button);
        objc::arc::release(title);
    }
}
