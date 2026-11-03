//! The C half of `NSAccessibility`.
//!
//! No assistive technology connects to a guest here, so posting a
//! notification reaches nobody - which is what it does on a Mac with none
//! running. The functions that answer questions answer them from the element.
#![allow(non_snake_case)]

use foundation::geometry::NSRect;
use objc::object::Id;
use objc::sel::Sel;

fn responds(receiver: Id, selector: Sel) -> bool {
    // SAFETY: a live object or nil.
    unsafe { objc::object::class_of(receiver) }.is_some_and(|c| c.method(selector).is_some())
}

/// `void NSAccessibilityPostNotification(id element, NSString *notification)`.
pub extern "C" fn NSAccessibilityPostNotification(_element: Id, _notification: Id) {}

/// `void NSAccessibilityPostNotificationWithUserInfo(id, NSString *, NSDictionary *)`.
pub extern "C" fn NSAccessibilityPostNotificationWithUserInfo(
    _element: Id,
    _notification: Id,
    _user_info: Id,
) {
}

/// The words of an `AX` name, lowercased: `AXPress` is "press" and
/// `AXRadioButton` "radio button", which is Apple's English description.
fn describe(name: Id) -> Id {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: an NSString, by the callers' prototypes.
    let text = String::from_utf8_lossy(&unsafe { foundation::string::bytes(name) }).into_owned();
    let text = text.strip_prefix("AX").unwrap_or(&text);
    let mut out = String::new();
    for (i, c) in text.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push(' ');
        }
        out.extend(c.to_lowercase());
    }
    objc::arc::autorelease(foundation::string::from_str(&out))
}

/// `NSString *NSAccessibilityActionDescription(NSString *action)`.
pub extern "C" fn NSAccessibilityActionDescription(action: Id) -> Id {
    describe(action)
}

/// `NSString *NSAccessibilityRoleDescription(NSString *role, NSString *subrole)`.
pub extern "C" fn NSAccessibilityRoleDescription(role: Id, subrole: Id) -> Id {
    describe(if subrole.is_null() { role } else { subrole })
}

/// `NSString *NSAccessibilityRoleDescriptionForUIElement(id element)`.
pub extern "C" fn NSAccessibilityRoleDescriptionForUIElement(element: Id) -> Id {
    let selector = objc::sel::get("accessibilityRole");
    if !responds(element, selector) {
        return std::ptr::null_mut();
    }
    // SAFETY: `-accessibilityRole` returns an NSString.
    let role = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(element, selector)
    };
    describe(role)
}

/// `id NSAccessibilityUnignoredAncestor(id element)`: the element itself
/// unless it says it is ignored, then its parent, and so on.
pub extern "C" fn NSAccessibilityUnignoredAncestor(element: Id) -> Id {
    let ignored = objc::sel::get("isAccessibilityElement");
    let parent = objc::sel::get("accessibilityParent");
    let mut at = element;
    for _ in 0..64 {
        if at.is_null() || !responds(at, ignored) {
            return at;
        }
        // SAFETY: `-isAccessibilityElement` returns BOOL.
        let is_element = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> i8 = std::mem::transmute(objc::msg::entry());
            f(at, ignored)
        };
        if is_element != 0 || !responds(at, parent) {
            return at;
        }
        // SAFETY: `-accessibilityParent` returns an object.
        at = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(at, parent)
        };
    }
    at
}

/// `NSRect NSAccessibilityFrameInView(NSView *, NSRect)`: the rectangle in
/// screen coordinates.
pub unsafe extern "C" fn NSAccessibilityFrameInView(view: Id, rect: NSRect) -> NSRect {
    if view.is_null() {
        return rect;
    }
    // SAFETY: NSView's and NSWindow's documented signatures; both return an
    // NSRect, which travels through the `_stret` entry.
    unsafe {
        let convert: unsafe extern "C" fn(Id, Sel, NSRect, Id) -> NSRect =
            std::mem::transmute(objc::msg::entry_stret());
        let in_window = convert(
            view,
            objc::sel::get("convertRect:toView:"),
            rect,
            std::ptr::null_mut(),
        );
        let window: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        let window = window(view, objc::sel::get("window"));
        if window.is_null() {
            return in_window;
        }
        let to_screen: unsafe extern "C" fn(Id, Sel, NSRect) -> NSRect =
            std::mem::transmute(objc::msg::entry_stret());
        to_screen(window, objc::sel::get("convertRectToScreen:"), in_window)
    }
}

/// The `NSAccessibility` protocol's properties, kept for whoever reads them
/// back. Nothing else reads them: no assistive technology is attached.
macro_rules! object_properties {
    ($( $getter:literal, $setter:literal, $policy:expr );* $(;)?) => {
        fn install_properties(class: &'static objc::class::Class) {
            $(
                {
                    static KEY: u8 = 0;
                    unsafe extern "C" fn get(receiver: Id, _sel: Sel) -> Id {
                        objc::arc::associated(receiver, &KEY as *const u8 as *const std::ffi::c_void)
                    }
                    unsafe extern "C" fn set(receiver: Id, _sel: Sel, value: Id) {
                        objc::arc::set_associated_with_policy(
                            receiver,
                            &KEY as *const u8 as *const std::ffi::c_void,
                            value,
                            $policy,
                        );
                    }
                    crate::define::methods!(class, {
                        $getter => get, c"@16@0:8";
                        $setter => set, c"v24@0:8@16";
                    });
                }
            )*
        }
    };
}

/// `OBJC_ASSOCIATION_RETAIN_NONATOMIC`, `_COPY_NONATOMIC` and `_ASSIGN`.
const RETAIN: usize = 1;
const COPY: usize = 3;
const ASSIGN: usize = 0;

object_properties! {
    "accessibilityRole", "setAccessibilityRole:", COPY;
    "accessibilitySubrole", "setAccessibilitySubrole:", COPY;
    "accessibilityRoleDescription", "setAccessibilityRoleDescription:", COPY;
    "accessibilityLabel", "setAccessibilityLabel:", COPY;
    "accessibilityTitle", "setAccessibilityTitle:", COPY;
    "accessibilityHelp", "setAccessibilityHelp:", COPY;
    "accessibilityIdentifier", "setAccessibilityIdentifier:", COPY;
    "accessibilityValueDescription", "setAccessibilityValueDescription:", COPY;
    "accessibilityPlaceholderValue", "setAccessibilityPlaceholderValue:", COPY;
    "accessibilityValue", "setAccessibilityValue:", RETAIN;
    "accessibilityChildren", "setAccessibilityChildren:", COPY;
    "accessibilitySelectedChildren", "setAccessibilitySelectedChildren:", COPY;
    "accessibilityTitleUIElement", "setAccessibilityTitleUIElement:", ASSIGN;
    "accessibilityParent", "setAccessibilityParent:", ASSIGN;
}

/// `-isAccessibilityElement` and its setter. Absent means the default, which
/// the receiver's class decides; this keeps only an explicit answer.
static ELEMENT: u8 = 0;

unsafe extern "C" fn is_accessibility_element(receiver: Id, _sel: Sel) -> u8 {
    let value = objc::arc::associated(receiver, &ELEMENT as *const u8 as *const std::ffi::c_void);
    // Stored as 1 for NO and 2 for YES, which no object is.
    (value as usize == 2) as u8
}

unsafe extern "C" fn set_accessibility_element(receiver: Id, _sel: Sel, element: u8) {
    objc::arc::set_associated(
        receiver,
        &ELEMENT as *const u8 as *const std::ffi::c_void,
        (if element != 0 { 2usize } else { 1 }) as Id,
    );
}

unsafe extern "C" fn accessibility_enabled(_receiver: Id, _sel: Sel) -> u8 {
    1
}

unsafe extern "C" fn set_ignored_flag(_receiver: Id, _sel: Sel, _flag: u8) {}

pub(crate) fn install() {
    for name in [c"NSView", c"NSWindow", c"NSCell", c"NSApplication"] {
        let Some(class) = objc::class::lookup(name) else {
            continue;
        };
        install_properties(class);
        crate::define::methods!(class, {
            "isAccessibilityElement" => is_accessibility_element, c"c16@0:8";
            "setAccessibilityElement:" => set_accessibility_element, c"v20@0:8c16";
            "isAccessibilityEnabled" => accessibility_enabled, c"c16@0:8";
            "setAccessibilityEnabled:" => set_ignored_flag, c"v20@0:8c16";
        });
    }
}
