// The C spellings are the interface.
#![allow(non_snake_case)]

//! AppKit's client-side accessibility entry points, which Chromium calls
//! directly.
//!
//! # What the binary says about the shapes
//!
//! Every call site sets **two** registers before
//! `NSAccessibilityPostNotification` and **three** before
//! `NSAccessibilityPostNotificationWithUserInfo`, and the notification is never
//! the first of them. At `0x809b068`:
//!
//! ```text
//! 809b062  mov  rdi, rbx                ; the accessibility element
//! 809b065  mov  rsi, r14                ; the NSNotification
//! 809b068  call _NSAccessibilityPostNotification
//! ```
//!
//! and at `0x803f7ef`, where the notification is one of the imported globals -
//! `_NSAccessibilityAnnouncementRequestedNotification`, read through its `__got`
//! slot at `0xdd51510`:
//!
//! ```text
//! 803f7df  mov  rax, [rip+…]            # 0xdd51510
//! 803f7e6  mov  rsi, [rax]              ; the NSNotification
//! 803f7e9  mov  rdi, r15                ; the accessibility element
//! 803f7ec  mov  rdx, r13                ; the userInfo dictionary
//! 803f7ef  call _NSAccessibilityPostNotificationWithUserInfo
//! ```
//!
//! So a first parameter exists and the notification is the second. The element
//! in every case is the result of a virtual call at vtable slot `0x58` - Blink's
//! own `AXObject` bridged to an `NSAccessibilityElement`, whose class import
//! (`_OBJC_CLASS_$_NSAccessibilityElement`) this runtime does not answer.
//!
//! # Why posting is refused rather than performed
//!
//! These two are how a program tells the accessibility *system* that something
//! changed. There is no such system here: no accessibility client, no server, no
//! observers, and no `NSAccessibilityElement` class for the argument to be. The
//! truthful answer is that nothing was posted, and the guest reads no result -
//! both are `void` with no out-parameter - so a bound function that says so once
//! is the whole of what can be correct. Leaving the import unbound instead would
//! be a jump through a zero slot, which is worse than a refusal by every measure
//! this project keeps.
//!
//! # Why the two descriptions are refused rather than answered
//!
//! `NSAccessibilityRoleDescription` and `NSAccessibilityActionDescription` look
//! up two tables of Apple's own prose - "button", "increment", and so on. Those
//! strings live in AppKit and **nowhere in the guest**: a byte search of the
//! 244 MB Framework finds no such table, because the guest asks AppKit for them
//! at run time. There is no way to derive them and no way to check an answer, so
//! they are refused by name and answer nil.
//!
//! Nil is a value the guest already handles on exactly this path. At
//! `0x8056c00`, the sibling `NSAccessibilityActionDescription`:
//!
//! ```text
//! 8056bfa  call objc_msgSend             ; -[self instanceActive]
//! 8056c00  test al, al
//! 8056c02  je   0x8056c1a               ; not active
//! 8056c04  mov  rdi, [rbp-0x10]          ; active: ask AppKit
//! 8056c08  call _NSAccessibilityActionDescription
//! 8056c1a  xor  ebx, ebx                ; otherwise: nil
//! ```
//!
//! so the result is an accessibility attribute value, and absent is one it takes.
//!
//! # Why the two walks are implemented
//!
//! `NSAccessibilityUnignoredAncestor` and `NSAccessibilityUnignoredChildren` walk
//! the *guest's own* object graph, and every message they need is named in the
//! image's `__TEXT,__objc_methname` table:
//!
//! ```text
//! accessibilityParent        accessibilityIsIgnored
//! accessibilityChildren
//! ```
//!
//! and `accessibilityParent` and `accessibilityChildren` are in the guest's own
//! `__objc_selrefs` (`0xe6e8dc8` and `0xe6e8dc0`) - the guest sends both itself:
//! `accessibilityParent` through `objc_msgSendSuper2` at `0x3a87cb4`, and
//! `accessibilityChildren` at `0x8044842`. So the walk is real work over real
//! objects and not a fabrication, and every send is guarded on
//! `respondsToSelector:` so an element that cannot answer refuses **by name**
//! rather than aborting.
//!
//! The return shapes are read off the callers rather than assumed. The
//! `UnignoredChildren` answer goes straight into `-[NSMutableArray
//! addObjectsFromArray:]` (`0x8044842`, with `addObjectsFromArray:` loaded into
//! `[rbp-0x50]`), so it is an array and nothing else. Both walks are autoreleased
//! at the call site (`0x804465d`, `0x804488c`, `0x3a87cc9`, `0x8057482`), so
//! that is what they return.

use std::sync::OnceLock;

use corefoundation::base::CFTypeRef;
use objc::object::Id;
use objc::sel::Sel;

/// A diagnostic said once, so a refusal is legible rather than silent.
fn say_once(what: &str) {
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_ok() {
        eprintln!("rosetta3: {what}");
    }
}

/// The best short description of an object, for a diagnostic.
///
/// Sent rather than read, and only when the object answers `-description`: this
/// runs inside a refusal, and a refusal must not be the thing that aborts.
fn describe(object: Id) -> String {
    if object.is_null() {
        return "nil".to_string();
    }
    let sel: Sel = objc::sel::get("description");
    // SAFETY: the receiver is a guest object; both sends are guarded on it, and
    // `-description` is `id (*)(id, SEL)`.
    unsafe {
        if !objc::shorthand::objc_opt_respondsToSelector(object, sel) {
            return "an object".to_string();
        }
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        let text = f(object, sel);
        if text.is_null() {
            return "an object".to_string();
        }
        // SAFETY: `text` is whatever `-description` answered, which by that
        // method's contract is a string.
        match corefoundation::string::as_string(text as CFTypeRef) {
            Some(string) => String::from_utf8_lossy(&string.utf8()).into_owned(),
            None => "an object".to_string(),
        }
    }
}

// ------------------------------------------------------------------ posting

/// `void NSAccessibilityPostNotification(id element, NSNotification *notification)`
///
/// The notification is the **second** parameter - see the module note. Neither
/// argument is read: this runtime posts nothing, because there is nothing here to
/// post to.
pub extern "C" fn NSAccessibilityPostNotification(_element: Id, notification: Id) {
    say_once(&format!(
        "NSAccessibilityPostNotification: not posted - this runtime has no \
         accessibility client, server or element class; the notification ({}) was dropped",
        describe(notification)
    ));
}

/// `void NSAccessibilityPostNotificationWithUserInfo(id element, NSNotification *notification, NSDictionary *userInfo)`
///
/// As [`NSAccessibilityPostNotification`], with the userInfo dictionary third.
pub extern "C" fn NSAccessibilityPostNotificationWithUserInfo(
    _element: Id,
    notification: Id,
    user_info: Id,
) {
    say_once(&format!(
        "NSAccessibilityPostNotificationWithUserInfo: not posted - this runtime has no \
         accessibility client, server or element class; the notification ({}) and its \
         userInfo ({}) were dropped",
        describe(notification),
        describe(user_info)
    ));
}

// -------------------------------------------------------------- descriptions

/// `NSString *NSAccessibilityRoleDescription(NSString *role)`
///
/// Refused: the table is Apple's prose and is in AppKit, not in the guest. nil,
/// which is a value the guest's accessibility attributes already take.
pub extern "C" fn NSAccessibilityRoleDescription(role: Id) -> Id {
    say_once(&format!(
        "NSAccessibilityRoleDescription: refusing - this runtime has no table of Apple's \
         accessibility role descriptions, and the guest carries none to check one against; \
         asked for {}, answering nil",
        describe(role)
    ));
    std::ptr::null_mut()
}

/// `NSString *NSAccessibilityActionDescription(NSString *action)`
///
/// Refused, as [`NSAccessibilityRoleDescription`] is, and for the same reason.
pub extern "C" fn NSAccessibilityActionDescription(action: Id) -> Id {
    say_once(&format!(
        "NSAccessibilityActionDescription: refusing - this runtime has no table of Apple's \
         accessibility action descriptions; asked for {}, answering nil",
        describe(action)
    ));
    std::ptr::null_mut()
}

// -------------------------------------------------------------------- walks

/// Whether `element` answers `name`, without sending it.
fn answers(element: Id, name: &str) -> bool {
    // SAFETY: `respondsToSelector:` is `BOOL (*)(id, SEL)` and the receiver is
    // the guest's object, which is alive for the length of the call.
    unsafe { objc::shorthand::objc_opt_respondsToSelector(element, objc::sel::get(name)) }
}

/// `-accessibilityIsIgnored`, or `None` if the element cannot answer it.
fn is_ignored(element: Id) -> Option<bool> {
    if !answers(element, "accessibilityIsIgnored") {
        return None;
    }
    let sel = objc::sel::get("accessibilityIsIgnored");
    // SAFETY: guarded on the receiver answering the selector just above, and
    // `-accessibilityIsIgnored` is `BOOL (*)(id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> bool = std::mem::transmute(objc::msg::entry());
        Some(f(element, sel))
    }
}

/// `-accessibilityParent`, or `None` if the element cannot answer it.
fn parent_of(element: Id) -> Option<Id> {
    if !answers(element, "accessibilityParent") {
        return None;
    }
    let sel = objc::sel::get("accessibilityParent");
    // SAFETY: as [`is_ignored`], and `-accessibilityParent` is `id (*)(id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        Some(f(element, sel))
    }
}

/// `-accessibilityChildren`, or `None` if the element cannot answer it.
///
/// Read with `-count` and `-objectAtIndex:` rather than through this crate's own
/// `collections::array`, because the array is the *guest's* and may be a
/// subclass whose storage is not this runtime's to interpret.
fn children_of(element: Id) -> Option<Vec<Id>> {
    if !answers(element, "accessibilityChildren") {
        return None;
    }
    let children_sel = objc::sel::get("accessibilityChildren");
    // SAFETY: guarded on the receiver answering the selector; the message is
    // `id (*)(id, SEL)`.
    let children = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(element, children_sel)
    };
    if children.is_null() {
        return Some(Vec::new());
    }
    if !answers(children, "count") || !answers(children, "objectAtIndex:") {
        return Some(Vec::new());
    }
    let count_sel = objc::sel::get("count");
    let at_sel = objc::sel::get("objectAtIndex:");
    // SAFETY: both sends are guarded above. The count is what the array itself
    // reports, so the loop cannot run past its end.
    unsafe {
        let count_fn: unsafe extern "C" fn(Id, Sel) -> usize =
            std::mem::transmute(objc::msg::entry());
        let at_fn: unsafe extern "C" fn(Id, Sel, usize) -> Id =
            std::mem::transmute(objc::msg::entry());
        let count = count_fn(children, count_sel);
        let mut out = Vec::with_capacity(count.min(4096));
        for index in 0..count {
            out.push(at_fn(children, at_sel, index));
        }
        Some(out)
    }
}

/// An autoreleased array, which is the container the `UnignoredChildren`
/// caller's `-addObjectsFromArray:` requires.
fn array_of(elements: &[Id]) -> Id {
    let values: Vec<CFTypeRef> = elements.iter().map(|e| *e as CFTypeRef).collect();
    let array = corefoundation::collections::new_array(
        &values,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh array this call owns; the guest autoreleases the answer
    // itself, so the +1 here is the one the pool takes.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

/// A bound on the walk, so a cycle in a guest's `-accessibilityParent` chain is a
/// refusal with a name rather than a hang.
const MAX_HOPS: usize = 256;

/// `id NSAccessibilityUnignoredAncestor(id element)`
///
/// The nearest strict ancestor of `element` that accessibility does not ignore,
/// and nil when there is none - which is what AppKit answers for a top-level
/// element, and what the guest's own `std::string` store is written against.
///
/// autoreleased, because the call site retains and autoreleases the answer itself
/// (`0x8044645` then `0x804465d`).
pub extern "C" fn NSAccessibilityUnignoredAncestor(element: Id) -> Id {
    if element.is_null() {
        return std::ptr::null_mut();
    }
    let mut node = element;
    for _ in 0..MAX_HOPS {
        // SAFETY: `node` is non-null, and `parent_of` guards its own send.
        let Some(parent) = parent_of(node) else {
            say_once(
                "NSAccessibilityUnignoredAncestor: refusing - the element answers neither \
                 -accessibilityParent nor -accessibilityIsIgnored, so this runtime cannot \
                 tell which ancestor is unignored; answering nil",
            );
            return std::ptr::null_mut();
        };
        if parent.is_null() {
            // The top of the guest's chain. No unignored ancestor is a real
            // answer, not a refusal.
            return std::ptr::null_mut();
        }
        if is_ignored(parent) == Some(false) {
            return parent;
        }
        node = parent;
    }
    say_once(
        "NSAccessibilityUnignoredAncestor: refusing - the -accessibilityParent chain did \
         not end within 256 hops, so this runtime will not keep walking it",
    );
    std::ptr::null_mut()
}

/// `id NSAccessibilityUnignoredChildren(id element)`
///
/// The children of `element` that accessibility does not ignore, as an
/// autoreleased array.
///
/// A child whose `-accessibilityIsIgnored` cannot be asked is treated as *not*
/// ignored, which is the direction that keeps a child in the answer: the
/// alternative drops a subtree on the strength of a method nobody implements.
pub extern "C" fn NSAccessibilityUnignoredChildren(element: Id) -> Id {
    if element.is_null() {
        return array_of(&[]);
    }
    // SAFETY: non-null, and `children_of` guards its own sends.
    let Some(children) = children_of(element) else {
        say_once(
            "NSAccessibilityUnignoredChildren: refusing - the element does not answer \
             -accessibilityChildren, so this runtime cannot enumerate its children; \
             answering an empty array",
        );
        return array_of(&[]);
    };
    let kept: Vec<Id> = children
        .into_iter()
        .filter(|child| !child.is_null() && is_ignored(*child) != Some(true))
        .collect();
    array_of(&kept)
}

exports::exports! {
    functions {
        "NSAccessibilityPostNotification" => NSAccessibilityPostNotification,
        "NSAccessibilityPostNotificationWithUserInfo" => NSAccessibilityPostNotificationWithUserInfo,
        "NSAccessibilityRoleDescription" => NSAccessibilityRoleDescription,
        "NSAccessibilityActionDescription" => NSAccessibilityActionDescription,
        "NSAccessibilityUnignoredAncestor" => NSAccessibilityUnignoredAncestor,
        "NSAccessibilityUnignoredChildren" => NSAccessibilityUnignoredChildren,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test element: the runtime's own `isa` slot, then the two fields the
    /// accessors read.
    ///
    /// The `isa` is not decoration. A message send reads the first word of an
    /// object to find its class, so a struct that started with the parent pointer
    /// would replace the class of every element with a pointer to another object -
    /// and the test would pass for the wrong reason on the one-hop cases while
    /// faulting on the walk. The layout here is the same `#[repr(C)] { isa, … }`
    /// every other class in this tree uses.
    #[repr(C)]
    struct Element {
        /// Never written by this module: `+new` installs it, and it is the
        /// object's class. It is here so that the fields below land at the
        /// offsets the accessors read.
        isa: *const std::ffi::c_void,
        parent: Id,
        children: Id,
        ignored: bool,
    }

    unsafe extern "C" fn parent_of_element(receiver: Id, _sel: Sel) -> Id {
        // SAFETY: only installed on a class registered with this module's
        // `Element`-sized storage.
        unsafe { (*(receiver as *const Element)).parent }
    }

    unsafe extern "C" fn children_of_element(receiver: Id, _sel: Sel) -> Id {
        // SAFETY: as [`parent_of_element`].
        unsafe { (*(receiver as *const Element)).children }
    }

    unsafe extern "C" fn is_ignored_element(receiver: Id, _sel: Sel) -> bool {
        // SAFETY: as [`parent_of_element`].
        unsafe { (*(receiver as *const Element)).ignored }
    }

    /// A fresh element of a class of its own, with the three accessors installed.
    fn element(
        name: &'static std::ffi::CStr,
        parent: Id,
        ignored: bool,
        children: &[Id],
    ) -> Id {
        crate::bootstrap();
        let class =
            corefoundation::base::register(name, None, std::mem::size_of::<Element>(), true);
        class.replace_method(
            objc::sel::get("accessibilityIsIgnored"),
            is_ignored_element as *const _,
            c"c16@0:8",
        );
        class.replace_method(
            objc::sel::get("accessibilityParent"),
            parent_of_element as *const _,
            c"@16@0:8",
        );
        class.replace_method(
            objc::sel::get("accessibilityChildren"),
            children_of_element as *const _,
            c"@16@0:8",
        );
        let array: Id = if children.is_empty() {
            std::ptr::null_mut()
        } else {
            let values: Vec<CFTypeRef> = children.iter().map(|c| *c as CFTypeRef).collect();
            corefoundation::collections::new_array(
                &values,
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            ) as Id
        };
        // SAFETY: a class object this runtime registered, and a nullary message
        // returning `id`.
        unsafe {
            let new: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(objc::msg::entry());
            let object = new(objc::class::ptr(class) as Id, objc::sel::get("new"));
            assert!(!object.is_null(), "{name:?} allocated");
            // SAFETY: fresh storage of the size the class was registered with.
            // Only the three fields are written: the `isa` `+new` just installed
            // is the object's class, and overwriting it would make every later
            // send on this element read the parent pointer as a class.
            let fields = object as *mut Element;
            (*fields).parent = parent;
            (*fields).children = array;
            (*fields).ignored = ignored;
            object
        }
    }

    /// A leaf: not ignored, with nothing above it and no children of its own.
    /// The case that must answer nil rather than walk.
    fn leaf(name: &'static std::ffi::CStr) -> Id {
        element(name, std::ptr::null_mut(), false, &[])
    }

    #[test]
    fn a_top_level_element_has_no_unignored_ancestor() {
        crate::bootstrap();
        let element = leaf(c"R3AXLeaf");
        assert!(
            NSAccessibilityUnignoredAncestor(element).is_null(),
            "nothing is above a top-level element, and nil is what the guest's own \
             std::string store is written against"
        );
    }

    /// The walk itself: an ignored parent is stepped over, and the first ancestor
    /// that is not ignored is what comes back.
    ///
    /// The three-deep chain is the shape the guest's tree has - Blink's AX tree
    /// is several levels deep before it reaches anything AppKit would call an
    /// element - and a one-level test would pass on an implementation that
    /// returned the immediate parent whatever it was.
    #[test]
    fn an_ignored_ancestor_is_walked_past_to_the_first_that_is_not() {
        crate::bootstrap();
        let top = leaf(c"R3AXTop");
        let middle = element(c"R3AXMiddle", top, true, &[]);
        let ignored = element(c"R3AXIgnored", middle, true, &[]);
        let bottom = element(c"R3AXBottom", ignored, false, &[]);

        assert_eq!(
            NSAccessibilityUnignoredAncestor(bottom),
            top,
            "the first ancestor that is not ignored, however many ignored ones are \
             between it and the element"
        );
        // Two ignored hops answer the same thing, which a one-hop implementation
        // would get wrong: it would have answered `middle` and `ignored`.
        assert_eq!(NSAccessibilityUnignoredAncestor(ignored), top);
        assert_eq!(
            NSAccessibilityUnignoredAncestor(middle),
            top,
            "one ignored hop, still the same answer"
        );
        // And the top itself has nothing above it, so the walk ends.
        assert!(NSAccessibilityUnignoredAncestor(top).is_null());
    }

    /// A parent that does not ignore itself and has no parent of its own is the
    /// answer, one hop out.
    #[test]
    fn the_nearest_unignored_parent_is_answered_directly() {
        crate::bootstrap();
        let parent = leaf(c"R3AXDirectParent");
        let child = element(c"R3AXDirectChild", parent, false, &[]);
        assert_eq!(NSAccessibilityUnignoredAncestor(child), parent);
    }

    /// The `UnignoredChildren` answer is an **array**, because the caller's next
    /// instruction is `-[NSMutableArray addObjectsFromArray:]` (`0x8044842`).
    /// An element with nothing above it answers an empty one rather than nil,
    /// because `addObjectsFromArray:` on nil is a message to nil.
    #[test]
    fn the_children_answer_is_an_array_even_when_there_is_nothing_in_it() {
        crate::bootstrap();
        let element = leaf(c"R3AXNoChildren");
        let children = NSAccessibilityUnignoredChildren(element);
        assert!(!children.is_null(), "nil would be a message to nil in the caller");
        // SAFETY: an array this runtime made.
        assert_eq!(
            unsafe { corefoundation::api::CFArrayGetCount(children as CFTypeRef) },
            0
        );
    }

    /// The filter: an ignored child is left out and the rest are kept, in the
    /// order they came.
    ///
    /// Without this the walk would be "return `-accessibilityChildren`", which is
    /// the one implementation that passes an empty-children test and is wrong
    /// here - a Blink AX node's children include the ones AppKit ignores, and
    /// handing those to `-addObjectsFromArray:` would put an inaccessible element
    /// into the accessibility tree.
    #[test]
    fn an_ignored_child_is_left_out_of_the_answer() {
        crate::bootstrap();
        let kept = leaf(c"R3AXKeptChild");
        let dropped = element(c"R3AXDroppedChild", std::ptr::null_mut(), true, &[]);
        let also_kept = leaf(c"R3AXSecondKept");
        let parent = element(c"R3AXParentOfThree", std::ptr::null_mut(), false, &[
            kept,
            dropped,
            also_kept,
        ]);

        let children = NSAccessibilityUnignoredChildren(parent);
        assert!(!children.is_null());
        // SAFETY: an array this runtime made, of objects this test made.
        unsafe {
            assert_eq!(corefoundation::api::CFArrayGetCount(children as CFTypeRef), 2);
            assert_eq!(
                corefoundation::api::CFArrayGetValueAtIndex(children as CFTypeRef, 0) as Id,
                kept,
                "the order the children came in is kept"
            );
            assert_eq!(
                corefoundation::api::CFArrayGetValueAtIndex(children as CFTypeRef, 1) as Id,
                also_kept
            );
        }
    }

    /// An element that answers neither accessor is refused by name and answers
    /// nil, which is the shape the guest's `UnignoredAncestor` result flows into.
    ///
    /// This is the case that matters most here: the element the real guest passes
    /// is an `NSAccessibilityElement` whose class import this runtime does not
    /// answer, so the *unbound* class becomes an empty one with no methods. A
    /// refusal that dereferenced it would fault; this one names itself.
    #[test]
    fn an_element_with_no_accessibility_methods_is_refused_rather_than_faulted() {
        crate::bootstrap();
        let class = corefoundation::base::register(c"R3AXBare", None, 8, true);
        // SAFETY: a class object this runtime registered, and a nullary message
        // returning `id`.
        let element = unsafe {
            let new: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(objc::msg::entry());
            new(objc::class::ptr(class) as Id, objc::sel::get("new"))
        };
        assert!(!element.is_null());
        assert!(NSAccessibilityUnignoredAncestor(element).is_null());
        let children = NSAccessibilityUnignoredChildren(element);
        assert!(!children.is_null(), "an empty array, not nil");
    }

    #[test]
    fn nil_in_is_answered_without_a_message() {
        assert!(NSAccessibilityUnignoredAncestor(std::ptr::null_mut()).is_null());
        assert!(!NSAccessibilityUnignoredChildren(std::ptr::null_mut()).is_null());
    }

    /// The two description tables are refused, and answer nil.
    ///
    /// A test that only checked the functions were callable would not notice a
    /// table invented here - which is the whole hazard, since an invented
    /// "button" reads correctly in a log and is wrong in an assistive technology.
    #[test]
    fn the_two_description_tables_are_refused_rather_than_invented() {
        crate::bootstrap();
        let role = foundation::string::from_str("AXButton");
        assert!(NSAccessibilityRoleDescription(role).is_null());
        assert!(NSAccessibilityActionDescription(role).is_null());
        objc::arc::release(role);
    }

    /// The two posting functions read no result and write none, so all that can
    /// be checked is that they are callable with the shape the binary uses: the
    /// notification in the **second** register.
    #[test]
    fn posting_takes_the_notification_second_and_reads_nothing_back() {
        crate::bootstrap();
        let element = leaf(c"R3AXPoster");
        let notification = foundation::string::from_str("NSAccessibilityTest");
        // SAFETY: the two functions are `void` and read no result; the
        // notification is the second, which is the shape `0x809b068` uses.
        unsafe {
            let post: extern "C" fn(Id, Id) = NSAccessibilityPostNotification;
            post(element, notification);
            let post_with: extern "C" fn(Id, Id, Id) = NSAccessibilityPostNotificationWithUserInfo;
            post_with(element, notification, std::ptr::null_mut());
        }
        assert!(!notification.is_null());
    }
}
