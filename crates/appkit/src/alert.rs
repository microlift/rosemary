use std::ffi::c_void;

use objc::object::Id;
use objc::sel::Sel;

use corefoundation::callbacks::Policy;

use crate::define::{class_methods, methods};

#[repr(C)]
pub struct Alert {
    isa: *const c_void,
    message: Id,
    informative: Id,
    /// The `NSButton`s `-addButtonWithTitle:` made, in order: an owned mutable
    /// array, null until the first one is added.
    buttons: Id,
    /// The view a program adds to the dialog to put its own controls in,
    /// retained. Its supported controls are presented by `-runModal`.
    accessory: Id,
    style: usize,
    icon: Id,
    /// The panel `-window` answers, made on first ask. The alert itself is
    /// drawn by `crate::dialog`, so the panel is never shown; it is what a
    /// program configures and what a sheet is attached as.
    window: Id,
    /// The checkbox `-suppressionButton` answers, made on first ask.
    suppression: Id,
    shows_suppression: usize,
}

pub const INSTANCE_SIZE: usize = size_of::<Alert>();

/// `NSAlertFirstButtonReturn`. The rest count up from it.
const FIRST_BUTTON_RETURN: i64 = 1000;

/// The key equivalents AppKit gives an alert's default and Cancel buttons.
const RETURN_KEY: &str = "\r";
const ESCAPE_KEY: &str = "\u{1b}";

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

/// `-addButtonWithTitle:`. The button's tag is the response it answers, and
/// AppKit gives the first Return and one titled Cancel Escape.
unsafe extern "C" fn add_button_with_title(receiver: Id, _sel: Sel, title: Id) -> Id {
    let alert = alert_of(receiver);
    if alert.buttons.is_null() {
        alert.buttons =
            corefoundation::collections::new_array(&[], Policy::from_type_callbacks(), true) as Id;
    }
    let index = button_objects(alert).len();
    let class = objc::class::lookup(c"NSButton").expect("declared in `classes`");
    // SAFETY: `+buttonWithTitle:target:action:` is `id (Class, SEL, id, id,
    // SEL)` and answers an autoreleased button.
    let button = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id, Sel) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            class as *const _ as Id,
            objc::sel::get("buttonWithTitle:target:action:"),
            title,
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    send_word(button, "setTag:", FIRST_BUTTON_RETURN as usize + index);
    // SAFETY: an `NSString` or nil.
    let key = match (index, unsafe { text_of(title) }.as_str()) {
        (0, _) => Some(RETURN_KEY),
        (_, "Cancel") => Some(ESCAPE_KEY),
        _ => None,
    };
    if let Some(key) = key {
        let key = foundation::string::from_str(key);
        send_object(button, "setKeyEquivalent:", key);
        objc::arc::release(key);
    }
    send_object(alert.buttons, "addObject:", button);
    button
}

unsafe extern "C" fn buttons(receiver: Id, _sel: Sel) -> Id {
    let alert = alert_of(receiver);
    if alert.buttons.is_null() {
        alert.buttons =
            corefoundation::collections::new_array(&[], Policy::from_type_callbacks(), true) as Id;
    }
    alert.buttons
}

unsafe extern "C" fn set_icon(receiver: Id, _sel: Sel, icon: Id) {
    set(&mut alert_of(receiver).icon, icon);
}

unsafe extern "C" fn icon(receiver: Id, _sel: Sel) -> Id {
    alert_of(receiver).icon
}

unsafe extern "C" fn window(receiver: Id, _sel: Sel) -> Id {
    let alert = alert_of(receiver);
    if alert.window.is_null() {
        let panel = objc::class::lookup(c"NSPanel").expect("declared in `classes`");
        alert.window = objc::object::create_instance(panel, 0);
    }
    alert.window
}

unsafe extern "C" fn set_shows_suppression_button(receiver: Id, _sel: Sel, shows: u8) {
    alert_of(receiver).shows_suppression = usize::from(shows != 0);
}

unsafe extern "C" fn shows_suppression_button(receiver: Id, _sel: Sel) -> u8 {
    (alert_of(receiver).shows_suppression != 0) as u8
}

/// `-suppressionButton`: a checkbox with AppKit's default title until the
/// program gives it its own.
unsafe extern "C" fn suppression_button(receiver: Id, _sel: Sel) -> Id {
    let alert = alert_of(receiver);
    if alert.suppression.is_null() {
        let class = objc::class::lookup(c"NSButton").expect("declared in `classes`");
        let title = foundation::string::from_str("Do not show this message again");
        // SAFETY: `+checkboxWithTitle:target:action:` is `id (Class, SEL, id,
        // id, SEL)` and answers an autoreleased button.
        let button = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id, Sel) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                class as *const _ as Id,
                objc::sel::get("checkboxWithTitle:target:action:"),
                title,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        objc::arc::release(title);
        alert.suppression = objc::arc::retain(button);
    }
    alert.suppression
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let alert = alert_of(receiver);
    for field in [
        alert.message,
        alert.informative,
        alert.accessory,
        alert.buttons,
        alert.icon,
        alert.window,
        alert.suppression,
    ] {
        objc::arc::release(field);
    }
    objc::arc::finalize(receiver);
}

/// The alert's buttons, in the order they were added.
fn button_objects(alert: &Alert) -> Vec<Id> {
    if alert.buttons.is_null() {
        return Vec::new();
    }
    // SAFETY: the array `-addButtonWithTitle:` made.
    unsafe { corefoundation::collections::array(alert.buttons as _) }
        .map(|array| array.values.iter().map(|v| *v as Id).collect())
        .unwrap_or_default()
}

/// What the dialog shows for `receiver`, and the checkboxes it reports back
/// on, in the order of its accessories.
fn request_for(receiver: Id) -> (crate::dialog::Request, Vec<Id>) {
    // AppKit adds the OK button itself when the program added none.
    if button_objects(alert_of(receiver)).is_empty() {
        let ok = foundation::string::from_str("OK");
        // SAFETY: the alert and a string.
        unsafe { add_button_with_title(receiver, objc::sel::get("addButtonWithTitle:"), ok) };
        objc::arc::release(ok);
    }
    let alert = alert_of(receiver);
    // SAFETY: each is an `NSString` a setter retained, or nil.
    let message = unsafe { text_of(alert.message) };
    let informative = unsafe { text_of(alert.informative) };
    let buttons = button_objects(alert);
    // SAFETY: each is an `NSString` or nil.
    let titles: Vec<String> = buttons
        .iter()
        .map(|b| unsafe { text_of(send_id(*b, "title")) })
        .collect();
    let cancel = buttons.iter().position(|b| {
        // SAFETY: an `NSString` or nil.
        (unsafe { text_of(send_id(*b, "keyEquivalent")) }) == ESCAPE_KEY
    });
    let mut checkboxes = Vec::new();
    collect_accessory_buttons(alert.accessory, &mut checkboxes);
    if alert.shows_suppression != 0 {
        // SAFETY: the alert.
        let suppression =
            unsafe { suppression_button(receiver, objc::sel::get("suppressionButton")) };
        checkboxes.extend(
            crate::controls::accessory_checkbox(suppression)
                .map(|(title, checked)| (suppression, title, checked)),
        );
    }

    eprintln!("rosemary: guest alert: {message}");
    if !informative.is_empty() {
        eprintln!("rosemary: guest alert: {informative}");
    }
    eprintln!("rosemary: guest alert: buttons {}", titles.join(" / "));

    let request = crate::dialog::Request {
        title: message.clone(),
        message,
        informative,
        buttons: titles,
        cancel,
        application_icon: true,
        accessories: checkboxes
            .iter()
            .map(|(_, title, checked)| chrome::alert::Accessory {
                title: title.clone(),
                checked: *checked,
            })
            .collect(),
        ..Default::default()
    };
    (request, checkboxes.into_iter().map(|(b, _, _)| b).collect())
}

/// Record the dialog's answer on the alert's controls; the chosen button's
/// tag, which is the alert's response.
fn answer(receiver: Id, checkboxes: &[Id], index: usize, states: &[bool]) -> i64 {
    for (button, checked) in checkboxes.iter().zip(states) {
        crate::controls::set_accessory_checkbox(*button, *checked);
    }
    let buttons = button_objects(alert_of(receiver));
    let Some(chosen) = buttons.get(index) else {
        return FIRST_BUTTON_RETURN + index as i64;
    };
    // SAFETY: `-tag` is `NSInteger (id, SEL)`.
    let tag = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> i64 = std::mem::transmute(objc::msg::entry());
        f(*chosen, objc::sel::get("tag"))
    };
    // SAFETY: an `NSString` or nil.
    let title = unsafe { text_of(send_id(*chosen, "title")) };
    eprintln!("rosemary: guest alert: answered {tag} ({title})");
    tag
}

/// With no dialog to show, the second button is the one that declines; with
/// fewer than two there is nothing to decline with.
fn unshown_answer(request: &crate::dialog::Request) -> (usize, Vec<bool>) {
    (
        usize::from(request.buttons.len() >= 2),
        request.accessories.iter().map(|a| a.checked).collect(),
    )
}

unsafe extern "C" fn run_modal(receiver: Id, _sel: Sel) -> i64 {
    let (request, checkboxes) = request_for(receiver);
    let (index, states) = crate::dialog::run_with_accessories(&request)
        .map(|(index, _, states)| (index, states))
        .unwrap_or_else(|| unshown_answer(&request));
    answer(receiver, &checkboxes, index, &states)
}

/// `-beginSheetModalForWindow:completionHandler:`: the dialog goes up and this
/// returns; the handler hears the response when it is answered, or when the
/// program ends the sheet itself with `-[NSWindow endSheet:returnCode:]`.
unsafe extern "C" fn begin_sheet_modal(receiver: Id, sel: Sel, parent: Id, handler: Id) {
    let handler = match handler.is_null() {
        true => None,
        false => Some(objc::block::copy(handler) as usize),
    };
    let complete = move |code: i64| {
        let Some(handler) = handler.map(|h| h as Id) else {
            return;
        };
        // SAFETY: `void (^)(NSModalResponse)`, as the selector declares.
        if let Some(invoke) = unsafe { objc::block::invoke(handler) } {
            unsafe {
                let f: unsafe extern "C" fn(Id, i64) = std::mem::transmute(invoke);
                f(handler, code);
            }
        }
        objc::block::release(handler);
    };
    let (request, checkboxes) = request_for(receiver);
    // SAFETY: the alert.
    let panel = unsafe { window(receiver, sel) };
    // Kept alive until the sheet ends, as AppKit keeps a presented alert.
    let alert = objc::arc::retain(receiver);
    let id = std::rc::Rc::new(std::cell::Cell::new(None));
    let shown = crate::dialog::show(&request, move |index, states| {
        let code = answer(alert, &checkboxes, index, &states);
        crate::sheet::end(panel, code);
    });
    let Some(shown) = shown else {
        let (index, states) = unshown_answer(&request);
        // The dialog never opened, so its closure, and the reference it holds,
        // went with it.
        let code = answer(receiver, &[], index, &states);
        complete(code);
        objc::arc::release(alert);
        return;
    };
    id.set(Some(shown));
    let ended = std::rc::Rc::clone(&id);
    crate::sheet::attach(
        parent,
        panel,
        Box::new(move |code| {
            if let Some(id) = ended.take() {
                crate::dialog::dismiss(id);
            }
            complete(code);
            objc::arc::release(alert);
        }),
    );
}

/// Send a getter that answers an object.
fn send_id(receiver: Id, selector: &str) -> Id {
    // SAFETY: every selector this is used with is `id (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector))
    }
}

fn send_object(receiver: Id, selector: &str, argument: Id) {
    // SAFETY: every selector this is used with is `void (id, SEL, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector), argument);
    }
}

fn send_word(receiver: Id, selector: &str, value: usize) {
    // SAFETY: every selector this is used with is `void (id, SEL, NSInteger)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, usize) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector), value);
    }
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
        "buttons" => buttons, c"@16@0:8";
        "setIcon:" => set_icon, c"v24@0:8@16";
        "icon" => icon, c"@16@0:8";
        "window" => window, c"@16@0:8";
        "setShowsSuppressionButton:" => set_shows_suppression_button, c"v20@0:8c16";
        "showsSuppressionButton" => shows_suppression_button, c"c16@0:8";
        "suppressionButton" => suppression_button, c"@16@0:8";
        "beginSheetModalForWindow:completionHandler:" => begin_sheet_modal, c"v32@0:8@16@?24";
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

    fn new_alert() -> Id {
        crate::bootstrap();
        objc::object::create_instance(objc::class::lookup(c"NSAlert").unwrap(), 0)
    }

    #[test]
    fn added_buttons_are_tagged_with_their_response() {
        let alert = new_alert();
        let add = objc::sel::get("addButtonWithTitle:");
        let (yes, no) = (
            foundation::string::from_str("Yes"),
            foundation::string::from_str("Cancel"),
        );
        // SAFETY: an alert and two strings.
        let buttons = unsafe {
            [
                add_button_with_title(alert, add, yes),
                add_button_with_title(alert, add, no),
            ]
        };
        // SAFETY: `-tag` is `NSInteger (id, SEL)`.
        let tag = |button: Id| unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> i64 = std::mem::transmute(objc::msg::entry());
            f(button, objc::sel::get("tag"))
        };
        assert_eq!(tag(buttons[0]), FIRST_BUTTON_RETURN);
        assert_eq!(tag(buttons[1]), FIRST_BUTTON_RETURN + 1);
        assert_eq!(button_objects(alert_of(alert)), buttons);
        let (request, _) = request_for(alert);
        assert_eq!(request.buttons, ["Yes", "Cancel"]);
        assert_eq!(request.cancel, Some(1));
        objc::arc::release(alert);
        objc::arc::release(yes);
        objc::arc::release(no);
    }

    #[test]
    fn an_alert_with_no_buttons_gets_ok() {
        let alert = new_alert();
        let (request, _) = request_for(alert);
        assert_eq!(request.buttons, ["OK"]);
        objc::arc::release(alert);
    }

    #[test]
    fn the_suppression_checkbox_is_offered_only_when_shown() {
        let alert = new_alert();
        let (request, checkboxes) = request_for(alert);
        assert!(request.accessories.is_empty() && checkboxes.is_empty());
        // SAFETY: the alert.
        let suppression = unsafe {
            set_shows_suppression_button(alert, objc::sel::get("setShowsSuppressionButton:"), 1);
            suppression_button(alert, objc::sel::get("suppressionButton"))
        };
        let (request, checkboxes) = request_for(alert);
        assert_eq!(checkboxes, [suppression]);
        assert_eq!(
            request.accessories[0].title,
            "Do not show this message again"
        );
        answer(alert, &checkboxes, 0, &[true]);
        assert_eq!(
            crate::controls::accessory_checkbox(suppression).map(|(_, on)| on),
            Some(true)
        );
        objc::arc::release(alert);
    }

    #[test]
    fn the_window_is_made_once() {
        let alert = new_alert();
        // SAFETY: the alert.
        let first = unsafe { window(alert, objc::sel::get("window")) };
        assert!(!first.is_null());
        assert_eq!(unsafe { window(alert, objc::sel::get("window")) }, first);
        objc::arc::release(alert);
    }
}
