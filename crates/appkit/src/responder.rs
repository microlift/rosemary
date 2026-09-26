use std::sync::Mutex;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

/// `(responder, retained menu)`, both as addresses.
///
/// Addresses rather than `Id`s so the table can be a `static`: a raw pointer is
/// not `Send`, and what is stored here is owned by this module for the life of the
/// process rather than shared with a thread.
///
/// An entry outlives a responder that is deallocated, which is a handful of words
/// for the life of the process: a menu is set on the application and on the few
/// views that have one, and a program that sets one does not then destroy the
/// view a thousand times. The alternative is a `-dealloc` override on three
/// classes to reach one table, which is more moving parts than the leak is worth.
fn menus() -> &'static Mutex<Vec<(usize, usize)>> {
    static MENUS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
    &MENUS
}

unsafe extern "C" fn menu(receiver: Id, _sel: Sel) -> Id {
    let menus = menus().lock().unwrap_or_else(|e| e.into_inner());
    menus
        .iter()
        .find(|(object, _)| *object == receiver as usize)
        .map_or(std::ptr::null_mut(), |(_, menu)| *menu as Id)
}

unsafe extern "C" fn set_menu(receiver: Id, _sel: Sel, value: Id) {
    let mut menus = menus().lock().unwrap_or_else(|e| e.into_inner());
    let kept = objc::arc::retain(value) as usize;
    match menus
        .iter_mut()
        .find(|(object, _)| *object == receiver as usize)
    {
        Some((_, existing)) => {
            objc::arc::release(*existing as Id);
            *existing = kept;
        }
        None => menus.push((receiver as usize, kept)),
    }
}

/// Send an event method on to the next responder: a view's superview, then its
/// window. A window is the end of the chain here, as it has no window of its own.
///
/// # Safety
///
/// `receiver` is null or an object this runtime allocated, and `sel` is a
/// `void (id, SEL, NSEvent *)` method.
pub(crate) unsafe fn forward(receiver: Id, sel: Sel, event: Id) {
    // A next responder set with `-setNextResponder:` comes first: a program
    // puts a controller between a view and its superview that way.
    let set = match crate::view::is_view(receiver) || crate::window::is_window(receiver) {
        // SAFETY: `-nextResponder` is `id (id, SEL)` on views and windows.
        true => unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get("nextResponder"))
        },
        false => std::ptr::null_mut(),
    };
    // SAFETY: null or an object this runtime allocated.
    let next = match set {
        next if !next.is_null() => next,
        _ => match unsafe { crate::view::superview_of(receiver) } {
            next if next.is_null() => unsafe { crate::view::window_of(receiver) },
            next => next,
        },
    };
    if next.is_null() {
        return;
    }
    // SAFETY: the prototype the caller vouches for.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(next, sel, event);
    }
}

/// The responder chain from `receiver` outwards, `receiver` first.
///
/// AppKit's chain, as far as this tree has one: a view's superviews, then the
/// window, then the window's delegate, then the application and *its* delegate.
/// The two delegates are in it because `NSWindow` and `NSApplication` override
/// `-tryToPerform:with:` to try theirs - a program puts its actions on the
/// application delegate and expects them found from anywhere.
///
/// An application is the one receiver that does not start with itself: AppKit
/// looks in the key window first, which is what makes a menu action reach the
/// window the user is typing into rather than the delegate's fallback.
///
/// Bounded, like [`crate::view::window_of`]: a subview cycle a guest built must
/// not hang inside what looks like an action dispatch.
fn chain(receiver: Id) -> Vec<Id> {
    fn push(out: &mut Vec<Id>, object: Id) {
        if !object.is_null() && !out.contains(&object) {
            out.push(object);
        }
    }
    let mut out = Vec::new();
    let application = crate::application::shared();
    let mut current = match receiver == application && !application.is_null() {
        true => crate::window::first_responder_of(crate::window::key_window()),
        false => receiver,
    };
    for _ in 0..64 {
        if current.is_null() {
            break;
        }
        push(&mut out, current);
        // SAFETY: null or an object this runtime allocated.
        let next = unsafe { crate::view::superview_of(current) };
        // SAFETY: as above. Null once `current` is no longer a view, which is
        // where the window ends the view part of the chain.
        current = match next.is_null() {
            true => unsafe { crate::view::window_of(current) },
            false => next,
        };
    }
    // The window the chain arrived at, which is the last responder in it that
    // has a delegate of its own.
    let window = out.iter().rev().copied().find(|object| {
        // SAFETY: an object this runtime allocated.
        unsafe { objc::object::class_of(*object) }.is_some_and(|c| {
            objc::class::lookup(c"NSWindow").is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
        })
    });
    if let Some(window) = window {
        push(&mut out, crate::window::delegate_of(window));
    }
    push(&mut out, application);
    if !application.is_null() {
        // SAFETY: `-delegate` is `id (id, SEL)` on the application.
        let delegate = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(application, objc::sel::get("delegate"))
        };
        push(&mut out, delegate);
    }
    out
}

/// `-[NSResponder tryToPerform:with:]`, which `NSView` inherits rather than
/// declares - and so do `NSWindow` and `NSApplication`, each of which overrides
/// it only to widen the chain [`chain`] already walks.
///
/// The first responder in the chain that implements `action` is sent it, with
/// `object` as the argument, and the answer is YES. Nothing in the chain
/// implementing it is NO and *nothing sent* - which is the whole point of the
/// method: a caller uses it to ask whether an action can be performed at all.
unsafe extern "C" fn try_to_perform(receiver: Id, _sel: Sel, action: Sel, object: Id) -> u8 {
    if action.is_null() {
        return 0;
    }
    for target in chain(receiver) {
        // SAFETY: a live responder from the chain.
        let Some(method) =
            (unsafe { objc::object::class_of(target) }).and_then(|class| class.method(action))
        else {
            continue;
        };
        // SAFETY: an action method is `void (id, SEL, id)` - the shape every
        // `-tryToPerform:with:` caller's selector has.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(method.implementation());
            f(target, action, object);
        }
        return 1;
    }
    0
}

/// The standard editing command a key means, or `None` for a key that types
/// something rather than commanding something.
///
/// Apple's list is the key bindings dictionary, which a user can edit; this is
/// the part of it every Mac has and every text client implements. Matched on the
/// virtual key rather than on `-characters` because a command key's character is
/// a private-use code point that only exists to be turned back into this.
fn command_for(key_code: u16, modifiers: u64) -> Option<&'static str> {
    use crate::keymap::vk;
    let shift = modifiers & crate::keymap::flags::SHIFT != 0;
    Some(match key_code {
        vk::RETURN | vk::KEYPAD_ENTER => "insertNewline:",
        vk::TAB if shift => "insertBacktab:",
        vk::TAB => "insertTab:",
        vk::DELETE => "deleteBackward:",
        vk::FORWARD_DELETE => "deleteForward:",
        vk::ESCAPE => "cancelOperation:",
        vk::LEFT_ARROW if shift => "moveLeftAndModifySelection:",
        vk::RIGHT_ARROW if shift => "moveRightAndModifySelection:",
        vk::UP_ARROW if shift => "moveUpAndModifySelection:",
        vk::DOWN_ARROW if shift => "moveDownAndModifySelection:",
        vk::LEFT_ARROW => "moveLeft:",
        vk::RIGHT_ARROW => "moveRight:",
        vk::UP_ARROW => "moveUp:",
        vk::DOWN_ARROW => "moveDown:",
        vk::HOME => "moveToBeginningOfDocument:",
        vk::END => "moveToEndOfDocument:",
        vk::PAGE_UP => "pageUp:",
        vk::PAGE_DOWN => "pageDown:",
        _ => return None,
    })
}

/// `-[NSResponder interpretKeyEvents:]`: hand key events to the text system.
///
/// Two things happen, and the first is the one a guest is really asking for. A
/// view calls this from its `-keyDown:` to say *this key was text, not a game
/// control*, and that is what decides where the host's resolved text commit goes
/// - see [`crate::text::accepts_host_text`]. A view that never calls it keeps its
/// keys.
///
/// The second is the editing commands, sent on as `-doCommandBySelector:`. What
/// is deliberately *not* sent is the printable text of the event: this host's
/// text arrives separately and layout-resolved from SDL, and inserting
/// `-characters` here as well would type every printable key twice - the same
/// reason [`crate::text`]'s own `-keyDown:` does not.
unsafe extern "C" fn interpret_key_events(receiver: Id, _sel: Sel, events: Id) {
    crate::text::accept_key_interpretation(receiver);
    // SAFETY: null or an `NSArray`, which is what the method takes.
    let Some(array) =
        (unsafe { corefoundation::collections::array(events as *const std::ffi::c_void) })
    else {
        return;
    };
    let command_sel = objc::sel::get("doCommandBySelector:");
    // SAFETY: a live responder this method was sent to.
    let Some(method) =
        (unsafe { objc::object::class_of(receiver) }).and_then(|class| class.method(command_sel))
    else {
        return;
    };
    for event in array.values.iter().copied() {
        let event = event as Id;
        if event.is_null() {
            continue;
        }
        // SAFETY: `-type`, `-keyCode` and `-modifierFlags` on an event, each
        // with the prototype its selector declares. A member of this array that
        // is not an event reads as zero rather than as another object's fields.
        let (kind, key_code, modifiers) = unsafe {
            let integer: unsafe extern "C" fn(Id, Sel) -> u64 =
                std::mem::transmute(objc::msg::entry());
            let short: unsafe extern "C" fn(Id, Sel) -> u16 =
                std::mem::transmute(objc::msg::entry());
            (
                integer(event, objc::sel::get("type")),
                short(event, objc::sel::get("keyCode")),
                integer(event, objc::sel::get("modifierFlags")),
            )
        };
        // A key *up* commands nothing: it would repeat every command the
        // matching key down already performed.
        if kind != crate::nsevent::kind::KEY_DOWN {
            continue;
        }
        let Some(command) = command_for(key_code, modifiers) else {
            continue;
        };
        // SAFETY: `-doCommandBySelector:` is `void (id, SEL, SEL)`, and the
        // implementation is the one the receiver's class answers with.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Sel) =
                std::mem::transmute(method.implementation());
            f(receiver, command_sel, objc::sel::get(command));
        }
    }
}

/// Where an action goes: `target` when there is one and it implements the
/// action, otherwise the first object in the application's responder chain
/// that does. Nil when nothing takes it.
pub(crate) fn target_for_action(action: Sel, target: Id) -> Id {
    let implements = |object: Id| {
        // SAFETY: null or a live object this runtime allocated.
        unsafe { objc::object::class_of(object) }.is_some_and(|c| c.method(action).is_some())
    };
    if action.is_null() {
        return std::ptr::null_mut();
    }
    if !target.is_null() {
        return match implements(target) {
            true => target,
            false => std::ptr::null_mut(),
        };
    }
    let application = crate::application::shared();
    if application.is_null() {
        return std::ptr::null_mut();
    }
    chain(application)
        .into_iter()
        .find(|object| implements(*object))
        .unwrap_or(std::ptr::null_mut())
}

/// `-[NSApplication sendAction:to:from:]`.
unsafe extern "C" fn send_action(_receiver: Id, _sel: Sel, action: Sel, to: Id, from: Id) -> u8 {
    let target = target_for_action(action, to);
    if target.is_null() {
        return 0;
    }
    // SAFETY: an action method is `void (id, SEL, id)`; sent rather than
    // called so a guest's forwarding runs.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(target, action, from);
    }
    1
}

/// `-[NSApplication targetForAction:to:from:]`.
unsafe extern "C" fn target_for_action_to_from(
    _receiver: Id,
    _sel: Sel,
    action: Sel,
    to: Id,
    _from: Id,
) -> Id {
    target_for_action(action, to)
}

/// `-[NSApplication targetForAction:]`.
unsafe extern "C" fn target_for_action_only(_receiver: Id, _sel: Sel, action: Sel) -> Id {
    target_for_action(action, std::ptr::null_mut())
}

/// `-performKeyEquivalent:` on a view: offered to each subview in turn, and
/// taken by none unless one of them - a guest's override - takes it.
unsafe extern "C" fn view_perform_key_equivalent(receiver: Id, sel: Sel, event: Id) -> u8 {
    // SAFETY: a view this method was installed on.
    for child in unsafe { crate::view::children(receiver) } {
        // SAFETY: `-performKeyEquivalent:` is `BOOL (id, SEL, NSEvent *)`.
        let taken = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 =
                std::mem::transmute(objc::msg::entry());
            f(child, sel, event)
        };
        if taken != 0 {
            return 1;
        }
    }
    0
}

/// `-[NSWindow performKeyEquivalent:]`: the window's views are asked.
unsafe extern "C" fn window_perform_key_equivalent(receiver: Id, sel: Sel, event: Id) -> u8 {
    let content = crate::window::content_view_of(receiver);
    if content.is_null() {
        return 0;
    }
    // SAFETY: `-performKeyEquivalent:` is `BOOL (id, SEL, NSEvent *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 = std::mem::transmute(objc::msg::entry());
        f(content, sel, event)
    }
}

/// An `NSResponder` event method: a responder that does not handle the event
/// passes it up. These methods also matter when a guest override calls `super`;
/// they are real methods on AppKit's base class rather than absent selectors.
unsafe extern "C" fn forward_event(receiver: Id, sel: Sel, event: Id) {
    // SAFETY: installed as `v24@0:8@16`.
    unsafe { forward(receiver, sel, event) }
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSResponder").expect("declared before its methods");
    methods!(class, {
        "menu" => menu, c"@16@0:8";
        "setMenu:" => set_menu, c"v24@0:8@16";
        "mouseDown:" => forward_event, c"v24@0:8@16";
        "mouseUp:" => forward_event, c"v24@0:8@16";
        "rightMouseDown:" => forward_event, c"v24@0:8@16";
        "rightMouseUp:" => forward_event, c"v24@0:8@16";
        "otherMouseDown:" => forward_event, c"v24@0:8@16";
        "otherMouseUp:" => forward_event, c"v24@0:8@16";
        "mouseMoved:" => forward_event, c"v24@0:8@16";
        "mouseDragged:" => forward_event, c"v24@0:8@16";
        "rightMouseDragged:" => forward_event, c"v24@0:8@16";
        "otherMouseDragged:" => forward_event, c"v24@0:8@16";
        "scrollWheel:" => forward_event, c"v24@0:8@16";
        "keyDown:" => forward_event, c"v24@0:8@16";
        "keyUp:" => forward_event, c"v24@0:8@16";
        "flagsChanged:" => forward_event, c"v24@0:8@16";
        "mouseEntered:" => forward_event, c"v24@0:8@16";
        "mouseExited:" => forward_event, c"v24@0:8@16";
        "interpretKeyEvents:" => interpret_key_events, c"v24@0:8@16";
        "tryToPerform:with:" => try_to_perform, c"c32@0:8:16@24";
    });
    let application = objc::class::lookup(c"NSApplication").expect("declared before its methods");
    methods!(application, {
        "sendAction:to:from:" => send_action, c"c40@0:8:16@24@32";
        "targetForAction:to:from:" => target_for_action_to_from, c"@40@0:8:16@24@32";
        "targetForAction:" => target_for_action_only, c"@24@0:8:16";
    });
    let view = objc::class::lookup(c"NSView").expect("declared before its methods");
    methods!(view, {
        "performKeyEquivalent:" => view_perform_key_equivalent, c"c24@0:8@16";
    });
    let window = objc::class::lookup(c"NSWindow").expect("declared before its methods");
    methods!(window, {
        "performKeyEquivalent:" => window_perform_key_equivalent, c"c24@0:8@16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn responder(class: &std::ffi::CStr) -> Id {
        crate::bootstrap();
        objc::object::create_instance(objc::class::lookup(class).expect("a class"), 0)
    }

    /// A menu set on one responder is not a menu set on another, and replacing one
    /// replaces it rather than adding a second entry.
    #[test]
    fn each_responder_keeps_its_own_menu() {
        let one = responder(c"NSView");
        let two = responder(c"NSView");
        let first = responder(c"NSMenu");
        let second = responder(c"NSMenu");
        // SAFETY: `-menu` and `-setMenu:` on responders of this crate's.
        unsafe {
            let get = objc::sel::get("menu");
            let set = objc::sel::get("setMenu:");
            assert!(menu(one, get).is_null(), "nothing set yet");
            set_menu(one, set, first);
            assert_eq!(menu(one, get), first);
            assert!(menu(two, get).is_null(), "a menu is not shared");
            set_menu(one, set, second);
            assert_eq!(menu(one, get), second, "replaced, not appended");
        }
    }

    /// `-mouseUp:` climbs to the first ancestor that overrides it.
    #[test]
    fn mouse_up_reaches_an_overriding_superview() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEEN: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn record(receiver: Id, _sel: Sel, _event: Id) {
            SEEN.store(receiver as usize, Ordering::Release);
        }
        crate::bootstrap();
        let sel = objc::sel::get("mouseUp:");
        let view = objc::class::lookup(c"NSView").expect("NSView");
        let class =
            objc::class::allocate_pair(Some(view), c"MouseUpRecorder", 0).expect("a subclass");
        class.add_method(sel, record as objc::class::Imp, c"v24@0:8@16");
        objc::class::register_pair(class);
        let parent = objc::object::create_instance(class, 0);
        let child = responder(c"NSView");
        // SAFETY: `-addSubview:` is `void (id, SEL, id)` on views of this crate's.
        unsafe {
            let add: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            add(parent, objc::sel::get("addSubview:"), child);
            forward_event(child, sel, std::ptr::null_mut());
        }
        assert_eq!(SEEN.load(Ordering::Acquire), parent as usize);
    }

    /// Cocoa declares every pointer method on `NSResponder`, so a guest
    /// override may call `super` without reaching message forwarding.
    #[test]
    fn pointer_event_methods_are_inherited_from_the_responder() {
        crate::bootstrap();
        let responder = objc::class::lookup(c"NSResponder").expect("NSResponder");
        let view = objc::class::lookup(c"NSView").expect("NSView");
        for name in [
            "mouseDown:",
            "mouseUp:",
            "rightMouseDown:",
            "rightMouseUp:",
            "otherMouseDown:",
            "otherMouseUp:",
            "mouseMoved:",
            "mouseDragged:",
            "rightMouseDragged:",
            "otherMouseDragged:",
            "scrollWheel:",
        ] {
            let sel = objc::sel::get(name);
            let owned = responder.method(sel).expect("declared on NSResponder");
            let inherited = view.method(sel).expect("inherited by NSView");
            assert_eq!(
                inherited.implementation(),
                owned.implementation(),
                "-{name}"
            );
        }
    }

    /// The command keys, as the selectors a text client is expected to answer.
    #[test]
    fn a_command_key_is_a_standard_selector() {
        use crate::keymap::{flags, vk};
        assert_eq!(command_for(vk::RETURN, 0), Some("insertNewline:"));
        assert_eq!(command_for(vk::TAB, 0), Some("insertTab:"));
        assert_eq!(command_for(vk::TAB, flags::SHIFT), Some("insertBacktab:"));
        assert_eq!(command_for(vk::DELETE, 0), Some("deleteBackward:"));
        assert_eq!(command_for(vk::LEFT_ARROW, 0), Some("moveLeft:"));
        assert_eq!(
            command_for(vk::LEFT_ARROW, flags::SHIFT),
            Some("moveLeftAndModifySelection:")
        );
        // A key that types something commands nothing: inserting it is the
        // separate, layout-resolved text commit.
        assert_eq!(command_for(vk::ANSI_A, 0), None);
        assert_eq!(command_for(vk::ANSI_A, flags::SHIFT), None);
    }

    /// One array of events in, one `-doCommandBySelector:` per command key out,
    /// and nothing for the key that types.
    #[test]
    fn interpreting_key_events_sends_the_commands() {
        use std::sync::Mutex;
        static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
        unsafe extern "C" fn record(_receiver: Id, _sel: Sel, command: Sel) {
            // SAFETY: a selector's name is a NUL-terminated string the runtime
            // keeps for the process.
            let name = unsafe { std::ffi::CStr::from_ptr(objc::sel::name(command)) };
            SEEN.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(name.to_string_lossy().into_owned());
        }
        crate::bootstrap();
        let view = objc::class::lookup(c"NSView").expect("NSView");
        let class =
            objc::class::allocate_pair(Some(view), c"CommandRecorder", 0).expect("a subclass");
        class.add_method(
            objc::sel::get("doCommandBySelector:"),
            record as objc::class::Imp,
            c"v24@0:8:16",
        );
        objc::class::register_pair(class);
        let receiver = objc::object::create_instance(class, 0);
        let key = |code: u16, kind: u64| {
            crate::nsevent::make(crate::nsevent::Fields {
                kind,
                key_code: code,
                ..crate::nsevent::Fields::default()
            }) as *const std::ffi::c_void
        };
        let events = [
            key(crate::keymap::vk::RETURN, crate::nsevent::kind::KEY_DOWN),
            key(crate::keymap::vk::ANSI_A, crate::nsevent::kind::KEY_DOWN),
            key(crate::keymap::vk::DELETE, crate::nsevent::kind::KEY_DOWN),
            // A key up repeats no command.
            key(crate::keymap::vk::RETURN, crate::nsevent::kind::KEY_UP),
        ];
        let array = corefoundation::collections::new_array(
            &events,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        );
        // SAFETY: `-interpretKeyEvents:` is `void (id, SEL, id)`.
        unsafe {
            interpret_key_events(receiver, objc::sel::get("interpretKeyEvents:"), array as Id)
        };
        assert_eq!(
            *SEEN.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["insertNewline:".to_string(), "deleteBackward:".to_string()]
        );
        assert!(
            crate::text::accepts_host_text(receiver),
            "a view that interpreted its keys is taking text"
        );
    }

    /// An action nobody implements is NO, and nothing is sent - which is what
    /// a caller uses the method to find out.
    #[test]
    fn an_unhandled_action_is_refused() {
        let view = responder(c"NSView");
        // SAFETY: `-tryToPerform:with:` is `BOOL (id, SEL, SEL, id)`.
        let answered = unsafe {
            try_to_perform(
                view,
                objc::sel::get("tryToPerform:with:"),
                objc::sel::get("r3NoSuchAction:"),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(answered, 0);
    }

    /// The action climbs to the first responder in the chain that implements
    /// it, and is sent there with the object the caller passed.
    #[test]
    fn an_action_reaches_the_first_responder_that_implements_it() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static TARGET: AtomicUsize = AtomicUsize::new(0);
        static ARGUMENT: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn perform(receiver: Id, _sel: Sel, object: Id) {
            TARGET.store(receiver as usize, Ordering::Release);
            ARGUMENT.store(object as usize, Ordering::Release);
        }
        crate::bootstrap();
        let action = objc::sel::get("r3Perform:");
        let view = objc::class::lookup(c"NSView").expect("NSView");
        let class = objc::class::allocate_pair(Some(view), c"ActionTarget", 0).expect("a subclass");
        class.add_method(action, perform as objc::class::Imp, c"v24@0:8@16");
        objc::class::register_pair(class);
        let parent = objc::object::create_instance(class, 0);
        let child = responder(c"NSView");
        let sender = responder(c"NSView");
        // SAFETY: `-addSubview:` is `void (id, SEL, id)` on views of this crate's.
        unsafe {
            let add: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            add(parent, objc::sel::get("addSubview:"), child);
        }
        // SAFETY: `-tryToPerform:with:` is `BOOL (id, SEL, SEL, id)`.
        let answered =
            unsafe { try_to_perform(child, objc::sel::get("tryToPerform:with:"), action, sender) };
        assert_eq!(answered, 1, "the superview implements it");
        assert_eq!(TARGET.load(Ordering::Acquire), parent as usize);
        assert_eq!(ARGUMENT.load(Ordering::Acquire), sender as usize);
    }

    /// `-tryToPerform:with:` is `NSResponder`'s, not `NSView`'s: Apple declares
    /// it on the base class, and every responder here reaches the same one.
    #[test]
    fn try_to_perform_is_inherited_from_the_responder() {
        crate::bootstrap();
        let sel = objc::sel::get("tryToPerform:with:");
        let responder = objc::class::lookup(c"NSResponder").expect("NSResponder");
        let owned = responder.method(sel).expect("declared on NSResponder");
        for name in [c"NSView", c"NSWindow", c"NSApplication", c"NSScroller"] {
            let class = objc::class::lookup(name).expect("a responder subclass");
            let found = class.method(sel).expect("inherited");
            assert_eq!(
                found.implementation(),
                owned.implementation(),
                "{name:?} does not reach NSResponder's"
            );
        }
    }

    /// Every responder subclass inherits it, which is the point of putting it on
    /// `NSResponder`: Qt sets a menu on `NSApp` and on its views, and the two are
    /// unrelated layouts.
    #[test]
    fn the_application_and_the_window_inherit_it() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSResponder").expect("NSResponder");
        let sel = objc::sel::get("setMenu:");
        for name in [c"NSApplication", c"NSWindow", c"NSView"] {
            let subclass = objc::class::lookup(name).expect("a responder subclass");
            assert!(
                subclass.chain().any(|c| std::ptr::eq(c, class)),
                "{name:?} is not a responder"
            );
            assert!(
                subclass.method(sel).is_some(),
                "{name:?} does not inherit -setMenu:"
            );
        }
    }
}
