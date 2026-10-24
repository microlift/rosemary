use std::ffi::{CStr, c_void};

use crate::class::{Class, Imp};
use crate::object::Id;
use crate::sel::Sel;

/// Install `NSObject` and its metaclass. Idempotent; called from [`crate::bootstrap`].
pub(crate) fn install() {
    let name = c"NSObject";
    let Some(class) = crate::class::allocate_pair(None, name, 0) else {
        return;
    };
    let meta = class.metaclass();

    // Class methods, on the metaclass. `+alloc` and `+new` are the reason the
    // root metaclass's superclass is the root class: `[NSObject class]` has to
    // resolve from either side.
    add(meta, "alloc", alloc as Imp, c"@16@0:8");
    add(
        meta,
        "allocWithZone:",
        alloc_with_zone as Imp,
        c"@24@0:8^v16",
    );
    add(meta, "new", new as Imp, c"@16@0:8");
    add(meta, "class", class_of_class as Imp, c"#16@0:8");
    add(meta, "superclass", superclass as Imp, c"#16@0:8");
    add(
        meta,
        "instancesRespondToSelector:",
        instances_respond as Imp,
        c"B24@0:8:16",
    );
    add(meta, "conformsToProtocol:", conforms as Imp, c"B24@0:8@16");
    add(
        meta,
        "instanceMethodForSelector:",
        instance_method_for as Imp,
        c"^?24@0:8:16",
    );
    // A class is its own copy, which is what lets one be a dictionary key.
    add(meta, "copy", class_of_class as Imp, c"@16@0:8");
    add(
        meta,
        "copyWithZone:",
        class_copy_with_zone as Imp,
        c"@24@0:8^v16",
    );
    add(meta, "initialize", initialize as Imp, c"v16@0:8");
    add(meta, "load", initialize as Imp, c"v16@0:8");

    add(class, "init", init as Imp, c"@16@0:8");
    add(
        class,
        "validateMenuItem:",
        validate_menu_item as Imp,
        c"c24@0:8@16",
    );
    add(class, "dealloc", dealloc as Imp, c"v16@0:8");
    add(class, "retain", retain as Imp, c"@16@0:8");
    add(class, "release", release as Imp, c"v16@0:8");
    add(class, "autorelease", autorelease as Imp, c"@16@0:8");
    add(class, "retainCount", retain_count as Imp, c"Q16@0:8");
    add(class, "self", itself as Imp, c"@16@0:8");
    add(class, "copy", copy as Imp, c"@16@0:8");
    add(class, "mutableCopy", mutable_copy as Imp, c"@16@0:8");
    add(class, "class", class_of as Imp, c"#16@0:8");
    add(class, "superclass", instance_superclass as Imp, c"#16@0:8");
    add(class, "isEqual:", is_equal as Imp, c"B24@0:8@16");
    add(class, "hash", hash as Imp, c"Q16@0:8");
    add(class, "isKindOfClass:", is_kind_of as Imp, c"B24@0:8#16");
    add(
        class,
        "isMemberOfClass:",
        is_member_of as Imp,
        c"B24@0:8#16",
    );
    add(class, "respondsToSelector:", responds as Imp, c"B24@0:8:16");
    add(class, "isProxy", is_proxy as Imp, c"B16@0:8");
    add(class, "zone", zone as Imp, c"^{_NSZone=}16@0:8");
    add(
        class,
        "methodForSelector:",
        method_for as Imp,
        c"^?24@0:8:16",
    );
    add(class, "performSelector:", perform as Imp, c"@24@0:8:16");
    add(
        class,
        "performSelector:withObject:",
        perform_with as Imp,
        c"@32@0:8:16@24",
    );
    add(
        class,
        "performSelector:withObject:withObject:",
        perform_with_with as Imp,
        c"@40@0:8:16@24@32",
    );
    add(
        class,
        "conformsToProtocol:",
        instance_conforms as Imp,
        c"B24@0:8@16",
    );
    add(
        class,
        "doesNotRecognizeSelector:",
        does_not_recognize as Imp,
        c"v24@0:8:16",
    );
    add(
        meta,
        "resolveInstanceMethod:",
        resolve_nothing as Imp,
        c"B24@0:8:16",
    );
    add(
        meta,
        "resolveClassMethod:",
        resolve_nothing as Imp,
        c"B24@0:8:16",
    );
    add(
        class,
        "forwardingTargetForSelector:",
        no_forwarding_target as Imp,
        c"@24@0:8:16",
    );

    crate::class::register_pair(class);
    install_proxy();
}

/// `+resolveInstanceMethod:` and `+resolveClassMethod:` as `NSObject` has
/// them: nothing added. The lookup skips a class whose resolver is this.
unsafe extern "C" fn resolve_nothing(_receiver: Id, _sel: Sel, _selector: Sel) -> bool {
    false
}

pub(crate) fn default_resolver() -> *const c_void {
    resolve_nothing as *const c_void
}

/// `-forwardingTargetForSelector:` as `NSObject` has it: none.
unsafe extern "C" fn no_forwarding_target(_receiver: Id, _sel: Sel, _selector: Sel) -> Id {
    std::ptr::null_mut()
}

pub(crate) fn default_forwarding_target() -> *const c_void {
    no_forwarding_target as *const c_void
}

unsafe extern "C" fn yes(_receiver: Id, _sel: Sel) -> bool {
    true
}

/// `NSProxy`, Foundation's other root class: an object that stands in for
/// another and forwards what it does not answer itself. Declared here rather
/// than in Foundation because a root class's memory management is the
/// runtime's.
///
/// FIXME (ACCURACY): forwarding reaches a target through
/// `-forwardingTargetForSelector:` only; `-forwardInvocation:` needs
/// `NSInvocation`, which this runtime does not have.
fn install_proxy() {
    let Some(class) = crate::class::allocate_pair(None, c"NSProxy", 0) else {
        return;
    };
    let meta = class.metaclass();
    add(meta, "alloc", alloc as Imp, c"@16@0:8");
    add(
        meta,
        "allocWithZone:",
        alloc_with_zone as Imp,
        c"@24@0:8^v16",
    );
    add(meta, "class", class_of_class as Imp, c"#16@0:8");
    add(meta, "initialize", initialize as Imp, c"v16@0:8");
    add(meta, "load", initialize as Imp, c"v16@0:8");
    add(meta, "respondsToSelector:", responds as Imp, c"B24@0:8:16");
    add(
        meta,
        "resolveInstanceMethod:",
        resolve_nothing as Imp,
        c"B24@0:8:16",
    );
    add(
        meta,
        "resolveClassMethod:",
        resolve_nothing as Imp,
        c"B24@0:8:16",
    );
    add(class, "dealloc", dealloc as Imp, c"v16@0:8");
    add(class, "retain", retain as Imp, c"@16@0:8");
    add(class, "release", release as Imp, c"v16@0:8");
    add(class, "autorelease", autorelease as Imp, c"@16@0:8");
    add(class, "retainCount", retain_count as Imp, c"Q16@0:8");
    add(class, "self", itself as Imp, c"@16@0:8");
    add(class, "class", class_of as Imp, c"#16@0:8");
    add(class, "superclass", instance_superclass as Imp, c"#16@0:8");
    add(class, "isEqual:", is_equal as Imp, c"B24@0:8@16");
    add(class, "hash", hash as Imp, c"Q16@0:8");
    add(class, "isKindOfClass:", is_kind_of as Imp, c"B24@0:8#16");
    add(
        class,
        "isMemberOfClass:",
        is_member_of as Imp,
        c"B24@0:8#16",
    );
    add(class, "respondsToSelector:", responds as Imp, c"B24@0:8:16");
    add(class, "isProxy", yes as Imp, c"B16@0:8");
    add(class, "zone", zone as Imp, c"^{_NSZone=}16@0:8");
    add(
        class,
        "conformsToProtocol:",
        instance_conforms as Imp,
        c"B24@0:8@16",
    );
    add(
        class,
        "forwardingTargetForSelector:",
        no_forwarding_target as Imp,
        c"@24@0:8:16",
    );
    add(
        class,
        "doesNotRecognizeSelector:",
        does_not_recognize as Imp,
        c"v24@0:8:16",
    );
    crate::class::register_pair(class);
}

fn add(class: &'static Class, sel: &str, imp: Imp, types: &CStr) {
    class.add_method(crate::sel::get(sel), imp, types);
}

/// The class a message was sent to, whichever side of the pair it arrived on.
///
/// # Safety
///
/// `receiver` must be a class pointer.
unsafe fn as_class(receiver: Id) -> &'static Class {
    match unsafe { crate::class::from_ptr(receiver as *const Class) } {
        Some(class) => class,
        None => crate::fail!("a class method was sent to nil"),
    }
}

/// Send `receiver` a message taking one word and returning an object.
///
/// Resolved rather than called directly, which is the whole point: `+alloc` has
/// to reach a subclass's `+allocWithZone:`, and a category that replaces one of
/// these has to be reached too.
///
/// # Safety
///
/// `receiver` must be a live object, and the method must be
/// `id (*)(id, SEL, void *)`.
unsafe fn send1(receiver: Id, sel: Sel, argument: *mut c_void) -> Id {
    let Some(method) = (unsafe { crate::object::class_of(receiver) }).and_then(|c| c.method(sel))
    else {
        return std::ptr::null_mut();
    };
    let f: unsafe extern "C" fn(Id, Sel, *mut c_void) -> Id =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { f(receiver, sel, argument) }
}

/// `+alloc` is `[self allocWithZone:nil]`, and not a shortcut to the allocator.
///
/// A class that overrides `+allocWithZone:` - which is how a C++ ivar
/// constructor gets to run, see `rosemary`'s `objc::cxxivars` - must be the one
/// that allocates.
unsafe extern "C" fn alloc(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: `+alloc`'s receiver is the class, and `+allocWithZone:` takes one
    // pointer and returns an object.
    unsafe {
        send1(
            receiver,
            crate::sel::get("allocWithZone:"),
            std::ptr::null_mut(),
        )
    }
}

unsafe extern "C" fn alloc_with_zone(receiver: Id, _sel: Sel, _zone: *mut c_void) -> Id {
    // SAFETY: as `alloc`; the zone has meant nothing for twenty years.
    crate::object::create_instance(unsafe { as_class(receiver) }, 0)
}

/// `+new` is `[[self alloc] init]`, both of them resolved.
unsafe extern "C" fn new(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a class.
    let object = unsafe { send1(receiver, crate::sel::get("alloc"), std::ptr::null_mut()) };
    if object.is_null() {
        return object;
    }
    // SAFETY: a freshly allocated object, and `-init` is `id (*)(id, SEL)`.
    unsafe { send1(object, crate::sel::get("init"), std::ptr::null_mut()) }
}

unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    receiver
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    crate::arc::finalize(receiver);
}

unsafe extern "C" fn retain(receiver: Id, _sel: Sel) -> Id {
    crate::arc::retain(receiver)
}

unsafe extern "C" fn release(receiver: Id, _sel: Sel) {
    crate::arc::release(receiver);
}

unsafe extern "C" fn autorelease(receiver: Id, _sel: Sel) -> Id {
    crate::arc::autorelease(receiver)
}

unsafe extern "C" fn retain_count(receiver: Id, _sel: Sel) -> usize {
    crate::arc::retain_count(receiver)
}

unsafe extern "C" fn itself(receiver: Id, _sel: Sel) -> Id {
    receiver
}

/// `-class` is the class; `+class` is the class itself, not the metaclass.
unsafe extern "C" fn class_of(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an instance.
    unsafe { crate::object::class_of(receiver) }
        .map_or(std::ptr::null_mut(), |c| crate::class::ptr(c) as Id)
}

unsafe extern "C" fn class_of_class(receiver: Id, _sel: Sel) -> Id {
    receiver
}

unsafe extern "C" fn superclass(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a class.
    unsafe { as_class(receiver) }
        .superclass()
        .map_or(std::ptr::null_mut(), |c| crate::class::ptr(c) as Id)
}

unsafe extern "C" fn instance_superclass(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an instance.
    unsafe { crate::object::class_of(receiver) }
        .and_then(|c| c.superclass())
        .map_or(std::ptr::null_mut(), |c| crate::class::ptr(c) as Id)
}

unsafe extern "C" fn is_equal(receiver: Id, _sel: Sel, other: Id) -> bool {
    receiver == other
}

unsafe extern "C" fn hash(receiver: Id, _sel: Sel) -> usize {
    receiver as usize
}

unsafe extern "C" fn is_proxy(_receiver: Id, _sel: Sel) -> bool {
    false
}

unsafe extern "C" fn zone(_receiver: Id, _sel: Sel) -> *mut c_void {
    std::ptr::null_mut()
}

unsafe extern "C" fn is_kind_of(receiver: Id, _sel: Sel, class: Id) -> bool {
    // SAFETY: an instance and a class.
    let Some(mine) = (unsafe { crate::object::class_of(receiver) }) else {
        return false;
    };
    // SAFETY: a class pointer this runtime produced, or null.
    let Some(theirs) = (unsafe { crate::class::from_ptr(class as *const Class) }) else {
        return false;
    };
    mine.chain().any(|c| std::ptr::eq(c, theirs))
}

/// `[self class] == class`, as Apple has it: sent, so an object that answers
/// `-class` with a class other than its `isa` (a KVO-notifying object, a
/// proxy) is a member of the class it answers.
unsafe extern "C" fn is_member_of(receiver: Id, _sel: Sel, class: Id) -> bool {
    // SAFETY: `Class (id, SEL)`.
    let mine = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(crate::msg::entry());
        f(receiver, crate::sel::get("class"))
    };
    !mine.is_null() && mine == class
}

unsafe extern "C" fn responds(receiver: Id, _sel: Sel, selector: Sel) -> bool {
    // SAFETY: an instance.
    unsafe { crate::object::class_of(receiver) }.is_some_and(|c| c.method(selector).is_some())
}

unsafe extern "C" fn instances_respond(receiver: Id, _sel: Sel, selector: Sel) -> bool {
    // SAFETY: a class.
    unsafe { as_class(receiver) }.method(selector).is_some()
}

/// The `IMP` for `selector` on `receiver`'s class, or the stub that reports the
/// message rather than the nil a caller would jump to. See [`crate::msg::forward`].
///
/// # Safety
///
/// `receiver` must be a live object.
unsafe fn imp_for(receiver: Id, selector: Sel) -> Imp {
    let imp = unsafe { crate::object::class_of(receiver) }.map_or(std::ptr::null(), |c| {
        crate::class::method_implementation(c, selector)
    });
    match imp.is_null() {
        true => crate::msg::forward(),
        false => imp,
    }
}

/// `-methodForSelector:`, which a class receiver answers about its *class*
/// methods: `class_of` a class is its metaclass, which is where those live.
unsafe extern "C" fn method_for(receiver: Id, _sel: Sel, selector: Sel) -> Imp {
    // SAFETY: an instance.
    unsafe { imp_for(receiver, selector) }
}

unsafe extern "C" fn instance_method_for(receiver: Id, _sel: Sel, selector: Sel) -> Imp {
    // SAFETY: a class.
    let imp = crate::class::method_implementation(unsafe { as_class(receiver) }, selector);
    match imp.is_null() {
        true => crate::msg::forward(),
        false => imp,
    }
}

/// The `-performSelector:` family, resolved and called rather than routed
/// through `objc_msgSend`: the guest's sends are intercepted before they reach
/// it, so a send made from here has to find the implementation itself.
unsafe extern "C" fn perform(receiver: Id, _sel: Sel, selector: Sel) -> Id {
    // SAFETY: an instance, and `imp_for` answers an implementation of
    // `selector`, whose prototype begins with the receiver and the selector.
    let f: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(imp_for(receiver, selector)) };
    unsafe { f(receiver, selector) }
}

unsafe extern "C" fn perform_with(receiver: Id, _sel: Sel, selector: Sel, object: Id) -> Id {
    // SAFETY: as `perform`, for a method taking one object.
    let f: unsafe extern "C" fn(Id, Sel, Id) -> Id =
        unsafe { std::mem::transmute(imp_for(receiver, selector)) };
    unsafe { f(receiver, selector, object) }
}

unsafe extern "C" fn perform_with_with(
    receiver: Id,
    _sel: Sel,
    selector: Sel,
    first: Id,
    second: Id,
) -> Id {
    // SAFETY: as `perform`, for a method taking two objects.
    let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id =
        unsafe { std::mem::transmute(imp_for(receiver, selector)) };
    unsafe { f(receiver, selector, first, second) }
}

unsafe extern "C" fn conforms(receiver: Id, _sel: Sel, protocol: Id) -> bool {
    // SAFETY: a class and a protocol.
    let class = unsafe { as_class(receiver) };
    protocol_of(protocol).is_some_and(|p| class.conforms_to(p))
}

unsafe extern "C" fn instance_conforms(receiver: Id, _sel: Sel, protocol: Id) -> bool {
    // SAFETY: an instance.
    let Some(class) = (unsafe { crate::object::class_of(receiver) }) else {
        return false;
    };
    protocol_of(protocol).is_some_and(|p| class.conforms_to(p))
}

fn protocol_of(protocol: Id) -> Option<&'static crate::protocol::Protocol> {
    // SAFETY: a `Protocol *` this runtime produced, or null.
    (!protocol.is_null()).then(|| unsafe { &*(protocol as *const crate::protocol::Protocol) })
}

unsafe extern "C" fn does_not_recognize(receiver: Id, _sel: Sel, selector: Sel) {
    // SAFETY: an instance.
    let class = unsafe { crate::object::class_of(receiver) };
    let name = class.map_or("nil", |c| c.name_str());
    let selector = match selector.is_null() {
        true => std::borrow::Cow::Borrowed("<null selector>"),
        // SAFETY: a registered selector is its own NUL-terminated name.
        false => unsafe { CStr::from_ptr(selector) }.to_string_lossy(),
    };
    crate::fail!("unrecognized selector sent to instance of {name}: {selector}");
}

/// `-copy` is `[self copyWithZone:nil]`, sent rather than looked up here.
///
/// `NSObject` does not adopt `NSCopying`: a class that has no `-copyWithZone:`
/// reaches the runtime's unrecognised-selector failure, which is Apple's
/// exception, and names the selector that is missing rather than `-copy`.
unsafe extern "C" fn copy(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: `-copyWithZone:` is `id (id, SEL, NSZone *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, *mut c_void) -> Id =
            std::mem::transmute(crate::msg::entry());
        f(
            receiver,
            crate::sel::get("copyWithZone:"),
            std::ptr::null_mut(),
        )
    }
}

/// `-mutableCopy` is `[self mutableCopyWithZone:nil]`, as [`copy`].
unsafe extern "C" fn mutable_copy(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: `-mutableCopyWithZone:` is `id (id, SEL, NSZone *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, *mut c_void) -> Id =
            std::mem::transmute(crate::msg::entry());
        f(
            receiver,
            crate::sel::get("mutableCopyWithZone:"),
            std::ptr::null_mut(),
        )
    }
}

unsafe extern "C" fn class_copy_with_zone(receiver: Id, _sel: Sel, _zone: *mut c_void) -> Id {
    receiver
}

/// `+initialize` and `+load`, which `NSObject` answers by doing nothing.
unsafe extern "C" fn initialize(_receiver: Id, _sel: Sel) {}

/// `NSObject`'s default menu-item validation leaves an item enabled.
unsafe extern "C" fn validate_menu_item(_receiver: Id, _sel: Sel, _item: Id) -> u8 {
    1
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;

    use super::*;
    use crate::sel;

    unsafe extern "C" fn answer(_receiver: Id, _sel: Sel) -> Id {
        42 as Id
    }

    /// An instance of a fresh subclass of `NSObject` implementing `-answer`.
    ///
    /// The name is the caller's because class names are global and the tests
    /// share a process.
    fn instance(name: &CStr) -> Id {
        crate::bootstrap();
        let nsobject = crate::class::lookup(c"NSObject").expect("NSObject");
        let class = crate::class::allocate_pair(Some(nsobject), name, 0).expect("a fresh name");
        assert!(class.add_method(sel::get("answer"), answer as Imp, c"@16@0:8"));
        crate::class::register_pair(class);
        crate::object::create_instance(class, 0)
    }

    /// Send a message taking one word and returning one, as a call site does.
    fn send(receiver: Id, name: &str, argument: *const c_void) -> *const c_void {
        // SAFETY: an instance made above.
        let class = unsafe { crate::object::class_of(receiver) }.expect("an instance");
        let sel = sel::get(name);
        let imp = crate::class::method_implementation(class, sel);
        assert!(!imp.is_null(), "NSObject answers -{name}");
        // SAFETY: every method reached here takes one word and returns one.
        let f: unsafe extern "C" fn(Id, Sel, *const c_void) -> *const c_void =
            unsafe { std::mem::transmute(imp) };
        unsafe { f(receiver, sel, argument) }
    }

    thread_local! {
        static DESTRUCTED: std::cell::RefCell<Vec<&'static str>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    unsafe extern "C" fn destruct_base(_receiver: Id, _sel: Sel) {
        DESTRUCTED.with(|d| d.borrow_mut().push("base"));
    }

    unsafe extern "C" fn destruct_derived(_receiver: Id, _sel: Sel) {
        DESTRUCTED.with(|d| d.borrow_mut().push("derived"));
    }

    /// ARC releases strong ivars in `.cxx_destruct`; each class's own runs
    /// once, nearest first, and a class without one is skipped.
    #[test]
    fn releasing_the_last_reference_runs_each_cxx_destruct_nearest_first() {
        crate::bootstrap();
        let nsobject = crate::class::lookup(c"NSObject").expect("NSObject");
        let destruct = sel::get(".cxx_destruct");
        let base = crate::class::allocate_pair(Some(nsobject), c"RosemaryDestructBase", 0)
            .expect("a fresh name");
        assert!(base.add_method(destruct, destruct_base as Imp, c"v16@0:8"));
        crate::class::register_pair(base);
        let middle = crate::class::allocate_pair(Some(base), c"RosemaryDestructMiddle", 0)
            .expect("a fresh name");
        crate::class::register_pair(middle);
        let derived = crate::class::allocate_pair(Some(middle), c"RosemaryDestructDerived", 0)
            .expect("a fresh name");
        assert!(derived.add_method(destruct, destruct_derived as Imp, c"v16@0:8"));
        crate::class::register_pair(derived);

        crate::arc::release(crate::object::create_instance(derived, 0));
        assert_eq!(DESTRUCTED.with(|d| d.borrow().clone()), ["derived", "base"]);
    }

    #[test]
    fn method_for_selector_answers_the_installed_imp() {
        let object = instance(c"RosemaryMethodForA");
        let found = send(
            object,
            "methodForSelector:",
            sel::get("answer") as *const c_void,
        );
        assert_eq!(found, answer as *const c_void);
    }

    /// The nil this used to answer is a jump to zero at the call site, which
    /// reports neither the class nor the selector.
    #[test]
    fn method_for_selector_never_answers_nil() {
        let object = instance(c"RosemaryMethodForB");
        let found = send(
            object,
            "methodForSelector:",
            sel::get("absent") as *const c_void,
        );
        assert_eq!(found, crate::msg::forward());
    }

    #[test]
    fn perform_selector_reaches_the_method() {
        let object = instance(c"RosemaryMethodForC");
        let result = send(
            object,
            "performSelector:",
            sel::get("answer") as *const c_void,
        );
        assert_eq!(result as usize, 42);
    }

    unsafe extern "C" fn copy_with_zone(_receiver: Id, _sel: Sel, _zone: *mut c_void) -> Id {
        7 as Id
    }

    /// `-copy` is the class's `-copyWithZone:`, found by dispatch rather than
    /// answered by `NSObject`.
    #[test]
    fn copy_sends_copy_with_zone() {
        let object = instance(c"RosemaryCopyA");
        // SAFETY: an instance made above.
        let class = unsafe { crate::object::class_of(object) }.expect("an instance");
        assert!(class.add_method(
            sel::get("copyWithZone:"),
            copy_with_zone as Imp,
            c"@24@0:8^v16"
        ));
        assert_eq!(send(object, "copy", std::ptr::null()) as usize, 7);
    }

    /// A class copies to itself.
    #[test]
    fn a_class_is_its_own_copy() {
        crate::bootstrap();
        let class = crate::class::ptr(crate::class::lookup(c"NSObject").expect("NSObject")) as Id;
        // SAFETY: `+copy` is `id (Class, SEL)`.
        assert_eq!(unsafe { copy_class(class) }, class);
    }

    /// # Safety
    ///
    /// `class` is a class pointer.
    unsafe fn copy_class(class: Id) -> Id {
        let meta = unsafe { crate::object::class_of(class) }.expect("a metaclass");
        let imp = crate::class::method_implementation(meta, sel::get("copy"));
        // SAFETY: `+copy` is `id (Class, SEL)`.
        let f: unsafe extern "C" fn(Id, Sel) -> Id = unsafe { std::mem::transmute(imp) };
        unsafe { f(class, sel::get("copy")) }
    }

    thread_local! {
        static INITIALIZED: std::cell::RefCell<Vec<String>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    unsafe extern "C" fn record_initialize(receiver: Id, _sel: Sel) {
        // SAFETY: the receiver is a class.
        let name = unsafe { crate::class::from_ptr(receiver as *const Class) }
            .expect("a class")
            .name_str()
            .to_string();
        INITIALIZED.with(|i| i.borrow_mut().push(name));
    }

    /// `+initialize` runs once per class before its first message, superclass
    /// first; a subclass without its own runs the inherited one as itself.
    #[test]
    fn initialize_is_sent_once_per_class_superclass_first() {
        crate::bootstrap();
        let nsobject = crate::class::lookup(c"NSObject").expect("NSObject");
        let base = crate::class::allocate_pair(Some(nsobject), c"RosemaryInitBase", 0)
            .expect("a fresh name");
        base.metaclass()
            .add_method(sel::get("initialize"), record_initialize as Imp, c"v16@0:8");
        crate::class::register_pair(base);
        let derived = crate::class::allocate_pair(Some(base), c"RosemaryInitDerived", 0)
            .expect("a fresh name");
        crate::class::register_pair(derived);

        crate::class::ensure_initialized(derived);
        crate::class::ensure_initialized(derived.metaclass());
        crate::class::ensure_initialized(base);
        assert_eq!(
            INITIALIZED.with(|i| i.borrow().clone()),
            ["RosemaryInitBase", "RosemaryInitDerived"]
        );
    }
}
