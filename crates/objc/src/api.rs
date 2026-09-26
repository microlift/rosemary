// The C spellings are the interface; renaming them would only hide it.
#![allow(non_snake_case)]

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::sync::{LazyLock, RwLock};

use crate::class::{Class, Imp, Ivar, Method};
use crate::object::Id;
use crate::protocol::{Description, Protocol};
use crate::sel::Sel;

/// A borrowed C string, or `None` for null.
///
/// # Safety
///
/// `p` must be null or a NUL-terminated string that outlives the call.
unsafe fn cstr<'a>(p: *const c_char) -> Option<&'a CStr> {
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) })
}

/// A class pointer as a class.
///
/// # Safety
///
/// `p` must be null or a class pointer this runtime produced.
unsafe fn class(p: *const c_void) -> Option<&'static Class> {
    unsafe { crate::class::from_ptr(p as *const Class) }
}

/// A `Method` handle.
///
/// # Safety
///
/// `p` must be null or a handle this runtime produced.
unsafe fn method(p: *const c_void) -> Option<&'static Method> {
    crate::debug::validate_handle(p, "method", "Objective-C method API");
    (!p.is_null()).then(|| unsafe { &*(p as *const Method) })
}

/// A `Protocol *`.
///
/// # Safety
///
/// `p` must be null or a protocol this runtime produced.
unsafe fn protocol(p: *const c_void) -> Option<&'static Protocol> {
    crate::debug::validate_handle(p, "protocol", "Objective-C protocol API");
    (!p.is_null()).then(|| unsafe { &*(p as *const Protocol) })
}

fn as_ptr<T>(value: Option<&'static T>) -> *mut c_void {
    value.map_or(std::ptr::null_mut(), |v| v as *const T as *mut c_void)
}

pub unsafe extern "C" fn sel_registerName(name: *const c_char) -> Sel {
    match unsafe { cstr(name) } {
        Some(name) => crate::sel::register(name),
        None => std::ptr::null(),
    }
}

pub unsafe extern "C" fn sel_getName(sel: Sel) -> *const c_char {
    match sel.is_null() {
        true => c"<null selector>".as_ptr(),
        false => crate::sel::name(sel),
    }
}

pub unsafe extern "C" fn sel_isEqual(a: Sel, b: Sel) -> bool {
    crate::sel::is_equal(a, b)
}

pub unsafe extern "C" fn objc_getClass(name: *const c_char) -> *mut c_void {
    as_ptr(unsafe { cstr(name) }.and_then(crate::class::lookup))
}

/// The same as [`objc_getClass`] here.
///
/// Apple's two differ in whether the class handler runs, and this runtime has no
/// class handler: nothing here loads a class on demand.
pub unsafe extern "C" fn objc_lookUpClass(name: *const c_char) -> *mut c_void {
    unsafe { objc_getClass(name) }
}

pub unsafe extern "C" fn objc_getRequiredClass(name: *const c_char) -> *mut c_void {
    let found = unsafe { objc_getClass(name) };
    if found.is_null() {
        let name = unsafe { cstr(name) }.map_or("<null>".into(), |n| n.to_string_lossy());
        crate::fail!("objc_getRequiredClass: no class named {name}");
    }
    found
}

pub unsafe extern "C" fn objc_getMetaClass(name: *const c_char) -> *mut c_void {
    as_ptr(
        unsafe { cstr(name) }
            .and_then(crate::class::lookup)
            .map(|c| c.metaclass()),
    )
}

pub unsafe extern "C" fn objc_allocateClassPair(
    superclass: *const c_void,
    name: *const c_char,
    extra: usize,
) -> *mut c_void {
    let Some(name) = (unsafe { cstr(name) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: as above. Null is a root class, which is what `nil` means here.
    let superclass = unsafe { class(superclass) };
    as_ptr(crate::class::allocate_pair(superclass, name, extra))
}

pub unsafe extern "C" fn objc_registerClassPair(cls: *const c_void) {
    if let Some(cls) = unsafe { class(cls) } {
        crate::class::register_pair(cls);
    }
}

pub unsafe extern "C" fn objc_disposeClassPair(cls: *const c_void) {
    if let Some(cls) = unsafe { class(cls) } {
        crate::class::dispose_pair(cls);
    }
}

pub unsafe extern "C" fn class_getName(cls: *const c_void) -> *const c_char {
    unsafe { class(cls) }.map_or(c"nil".as_ptr(), |c| c.name())
}

pub unsafe extern "C" fn class_getSuperclass(cls: *const c_void) -> *mut c_void {
    as_ptr(unsafe { class(cls) }.and_then(|c| c.superclass()))
}

pub unsafe extern "C" fn class_isMetaClass(cls: *const c_void) -> bool {
    unsafe { class(cls) }.is_some_and(|c| c.is_meta())
}

pub unsafe extern "C" fn class_getInstanceSize(cls: *const c_void) -> usize {
    unsafe { class(cls) }.map_or(0, |c| c.instance_size())
}

pub unsafe extern "C" fn class_addMethod(
    cls: *const c_void,
    name: Sel,
    imp: Imp,
    types: *const c_char,
) -> bool {
    let (Some(cls), Some(types)) = (unsafe { class(cls) }, unsafe { cstr(types) }) else {
        return false;
    };
    cls.add_method(name, imp, types)
}

pub unsafe extern "C" fn class_replaceMethod(
    cls: *const c_void,
    name: Sel,
    imp: Imp,
    types: *const c_char,
) -> Imp {
    let (Some(cls), Some(types)) = (unsafe { class(cls) }, unsafe { cstr(types) }) else {
        return std::ptr::null();
    };
    cls.replace_method(name, imp, types)
}

pub unsafe extern "C" fn class_addIvar(
    cls: *const c_void,
    name: *const c_char,
    size: usize,
    alignment: u8,
    types: *const c_char,
) -> bool {
    let (Some(cls), Some(name), Some(types)) =
        (unsafe { class(cls) }, unsafe { cstr(name) }, unsafe {
            cstr(types)
        })
    else {
        return false;
    };
    cls.add_ivar(name, size, alignment, types)
}

pub unsafe extern "C" fn class_getInstanceVariable(
    cls: *const c_void,
    name: *const c_char,
) -> *const c_void {
    let (Some(cls), Some(name)) = (unsafe { class(cls) }, unsafe { cstr(name) }) else {
        return std::ptr::null();
    };
    cls.ivar(name)
        .map_or(std::ptr::null(), |i| i as *const Ivar as *const c_void)
}

pub unsafe extern "C" fn class_getInstanceMethod(cls: *const c_void, name: Sel) -> *const c_void {
    unsafe { class(cls) }
        .and_then(|c| c.method(name))
        .map_or(std::ptr::null(), |m| m as *const Method as *const c_void)
}

pub unsafe extern "C" fn class_getClassMethod(cls: *const c_void, name: Sel) -> *const c_void {
    unsafe { class(cls) }
        .and_then(|c| c.metaclass().method(name))
        .map_or(std::ptr::null(), |m| m as *const Method as *const c_void)
}

/// The resolved `IMP` itself, never a forwarding trampoline.
///
/// `rosetta3`'s dispatch caches this value and re-probes it to notice a swizzle,
/// so it must change exactly when the resolution does. See the crate doc.
pub unsafe extern "C" fn class_getMethodImplementation(cls: *const c_void, name: Sel) -> Imp {
    unsafe { class(cls) }.map_or(std::ptr::null(), |c| {
        crate::class::method_implementation(c, name)
    })
}

pub unsafe extern "C" fn class_respondsToSelector(cls: *const c_void, name: Sel) -> bool {
    unsafe { class(cls) }.is_some_and(|c| c.method(name).is_some())
}

pub unsafe extern "C" fn class_addProtocol(cls: *const c_void, proto: *const c_void) -> bool {
    let (Some(cls), Some(proto)) = (unsafe { class(cls) }, unsafe { protocol(proto) }) else {
        return false;
    };
    cls.add_protocol(proto)
}

pub unsafe extern "C" fn class_conformsToProtocol(
    cls: *const c_void,
    proto: *const c_void,
) -> bool {
    let (Some(cls), Some(proto)) = (unsafe { class(cls) }, unsafe { protocol(proto) }) else {
        return false;
    };
    cls.conforms_to(proto)
}

pub unsafe extern "C" fn class_createInstance(cls: *const c_void, extra: usize) -> Id {
    unsafe { class(cls) }.map_or(std::ptr::null_mut(), |c| {
        crate::object::create_instance(c, extra)
    })
}

/// The `IMP` exactly as it was installed.
///
/// Never a wrapper: `rosetta3` compares this against its own thunk table to tell
/// a guest method from a host one. See the crate doc.
pub unsafe extern "C" fn method_getImplementation(m: *const c_void) -> Imp {
    unsafe { method(m) }.map_or(std::ptr::null(), |m| m.implementation())
}

pub unsafe extern "C" fn method_getTypeEncoding(m: *const c_void) -> *const c_char {
    unsafe { method(m) }.map_or(std::ptr::null(), |m| m.types())
}

pub unsafe extern "C" fn method_getName(m: *const c_void) -> Sel {
    unsafe { method(m) }.map_or(std::ptr::null(), |m| m.sel)
}

pub unsafe extern "C" fn method_setImplementation(m: *const c_void, imp: Imp) -> Imp {
    unsafe { method(m) }.map_or(std::ptr::null(), |m| m.set_implementation(imp))
}

/// `void method_exchangeImplementations(Method m1, Method m2)`: the two methods'
/// implementations trade places.
///
/// Both arguments are usually the *same* selector found on two classes, because
/// the usual reason to swizzle is to intercept a superclass's implementation
/// without replacing it - the original stays reachable, and a caller that saved
/// the `IMP` first has kept it. The C prototype does not require it and neither
/// does this.
///
/// Two [`Method::set_implementation`] calls rather than one operation, so the pair
/// is not atomic and a thread exchanging overlapping pairs can catch a method
/// holding an implementation neither side asked for. Apple has the same window:
/// its entry point swaps two words under the runtime lock, which excludes a
/// concurrent runtime call but not a second swizzle from inside a swizzled method.
/// Closing it here would mean a lock on the dispatch path, for a window Apple's
/// own runtime leaves open.
///
/// The invalidation is the one [`method_setImplementation`] already has, and it is
/// worth being exact about which one: a cached dispatch plan re-probes
/// [`crate::class::method_implementation`] and compares, and a swap changes that
/// answer for *both* classes. Each `set_implementation` also bumps
/// [`crate::class::generation`], so a swizzle through this name is
/// indistinguishable from one through that one to anything counting them.
pub unsafe extern "C" fn method_exchangeImplementations(a: *const c_void, b: *const c_void) {
    let (Some(a), Some(b)) = (unsafe { method(a) }, unsafe { method(b) }) else {
        return;
    };
    // Both read before either write, so the same handle passed twice leaves the
    // method holding what it held rather than swapping it with itself.
    let (first, second) = (a.implementation(), b.implementation());
    a.set_implementation(second);
    b.set_implementation(first);
}

pub unsafe extern "C" fn ivar_getOffset(ivar: *const c_void) -> isize {
    crate::debug::validate_handle(ivar, "ivar", "ivar_getOffset");
    // SAFETY: an `Ivar` this runtime produced, or null.
    match ivar.is_null() {
        true => 0,
        false => unsafe { &*(ivar as *const Ivar) }.offset(),
    }
}

pub unsafe extern "C" fn ivar_getName(ivar: *const c_void) -> *const c_char {
    crate::debug::validate_handle(ivar, "ivar", "ivar_getName");
    match ivar.is_null() {
        true => std::ptr::null(),
        false => unsafe { &*(ivar as *const Ivar) }.name(),
    }
}

pub unsafe extern "C" fn object_getClass(object: Id) -> *mut c_void {
    as_ptr(unsafe { crate::object::class_of(object) })
}

pub unsafe extern "C" fn object_setClass(object: Id, cls: *const c_void) -> *mut c_void {
    let Some(cls) = (unsafe { class(cls) }) else {
        return std::ptr::null_mut();
    };
    as_ptr(unsafe { crate::object::set_class(object, cls) })
}

pub unsafe extern "C" fn object_getClassName(object: Id) -> *const c_char {
    unsafe { crate::object::class_of(object) }.map_or(c"nil".as_ptr(), |c| c.name())
}

pub unsafe extern "C" fn object_getIndexedIvars(object: Id) -> *mut c_void {
    unsafe { crate::object::indexed_ivars(object) }
}

pub unsafe extern "C" fn object_getIvar(object: Id, ivar: *const c_void) -> Id {
    crate::debug::validate_handle(ivar, "ivar", "object_getIvar");
    if crate::debug::enabled()
        && !object.is_null()
        && !ivar.is_null()
        // SAFETY: the object and ivar handles were validated above.
        && !unsafe { crate::object::class_of(object) }
            .is_some_and(|class| class.contains_ivar(ivar as *const Ivar))
    {
        crate::fail!("objc debug: object_getIvar was given an ivar from another class");
    }
    let Some(name) = (unsafe { cstr(ivar_getName(ivar)) }) else {
        return std::ptr::null_mut();
    };
    unsafe { crate::object::get_ivar(object, name) }
}

pub unsafe extern "C" fn object_setIvar(object: Id, ivar: *const c_void, value: Id) {
    crate::debug::validate_handle(ivar, "ivar", "object_setIvar");
    crate::debug::validate(value, "object_setIvar value");
    if crate::debug::enabled()
        && !object.is_null()
        && !ivar.is_null()
        // SAFETY: the object and ivar handles were validated above.
        && !unsafe { crate::object::class_of(object) }
            .is_some_and(|class| class.contains_ivar(ivar as *const Ivar))
    {
        crate::fail!("objc debug: object_setIvar was given an ivar from another class");
    }
    let Some(name) = (unsafe { cstr(ivar_getName(ivar)) }) else {
        return;
    };
    unsafe { crate::object::set_ivar(object, name, value) }
}

pub unsafe extern "C" fn objc_constructInstance(cls: *const c_void, bytes: *mut c_void) -> Id {
    let Some(cls) = (unsafe { class(cls) }) else {
        return std::ptr::null_mut();
    };
    if bytes.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller says `bytes` is at least `instance_size` bytes.
    unsafe { *(bytes as *mut *mut Class) = crate::class::ptr(cls) };
    bytes
}

pub unsafe extern "C" fn objc_destructInstance(object: Id) -> Id {
    crate::debug::validate(object, "objc_destructInstance");
    object
}

/// `id object_dispose(id obj)`: destroy the receiver, and hand it back.
///
/// **A different entry point from [`objc_destructInstance`]**, which this runtime
/// answers by returning its argument: that one unwinds C++ ivars and stops there,
/// while this one destroys the object. A guest that confuses them gets nothing
/// from this side of the boundary to tell it, so the two are kept apart here
/// rather than merged into one answer.
///
/// `-dispose` when the class answers it and `-dealloc` when it does not. That is
/// Apple's rule, and the reason the rule exists is in the names: `dispose` is the
/// older spelling of `dealloc` and the two are interchangeable to a caller, so a
/// class may implement either and a caller that sent the wrong one would get a
/// selector nothing answers.
///
/// Resolved through the class chain rather than its own method list, because that
/// is what a message send does and what `-respondsToSelector:` reports. A guest
/// asking whether the class has a `-dispose` and a guest asking `object_dispose`
/// to destroy the object must not get different answers.
///
/// The rule cannot re-enter, and `NSObject` is why: it declares no `-dispose`, and
/// its `-dealloc` is [`crate::arc::finalize`], which frees and does not call back.
/// So a class implementing either selector runs it exactly once, and a class
/// implementing neither - a root class that is not `NSObject` - reaches the
/// unrecognised-selector report rather than a second traversal.
pub unsafe extern "C" fn object_dispose(object: Id) -> Id {
    crate::debug::validate(object, "object_dispose");
    // Messaging nil is the language, and a class is a legal argument as well - it
    // is an object - whose class is its metaclass. Neither is special-cased below;
    // what the receiver's class answers is the whole of the decision, which is
    // Apple's rule and the reason this entry point is `object_getClass` plus a send.
    if object.is_null() {
        return object;
    }
    // SAFETY: a live object - which includes a class object, whose first word is a
    // metaclass pointer - or null, which returned above.
    let Some(class) = (unsafe { crate::object::class_of(object) }) else {
        return object;
    };
    let dispose = crate::sel::get("dispose");
    let sel = match class.method(dispose) {
        Some(_) => dispose,
        None => crate::sel::get("dealloc"),
    };
    // Nothing answers the selector that was chosen: land on the same `IMP`
    // `-methodForSelector:` reports for an absent one, which names the class and
    // the selector rather than being a jump to zero. See [`crate::msg::forward`].
    let imp = crate::class::method_implementation(class, sel);
    // SAFETY: `f` answers `sel`, whose prototype begins with the receiver and the
    // selector, and both of those selectors are `void (*)(id, SEL)` in every
    // Objective-C there has ever been.
    let f: unsafe extern "C" fn(Id, Sel) = match imp.is_null() {
        true => unsafe { std::mem::transmute(crate::msg::forward()) },
        false => unsafe { std::mem::transmute(imp) },
    };
    unsafe { f(object, sel) };
    // The receiver comes back, and the caller must not use it: the object is gone.
    object
}

/// The preprocessor installed for each exception, by the exception's address.
///
/// A table of its own rather than [`crate::arc`]'s association machinery, and the
/// *value* is why: a preprocessor is a function pointer, and
/// [`crate::arc::set_associated`] validates its value as an object - which under
/// `--objc-debug` means reading a function's first machine word as an `isa` and
/// refusing the call.
///
/// Addresses, not pointers, and never reclaimed - which is the same trade
/// [`crate::arc`]'s own association table makes here and for the same reason: a
/// `*const c_void` is not `Sync`, and a table of them would need an assertion this
/// crate would then have to justify. What accumulates is one word per exception
/// raised. An entry outliving its exception is harmless; an address *reused* by a
/// later one would not be, and dropping an entry belongs with the deallocation
/// path in [`crate::arc`] rather than here.
static PREPROCESSORS: LazyLock<RwLock<HashMap<usize, usize>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// `id objc_setExceptionPreprocessor(id exception, objc_exception_preprocessor fn)`.
///
/// Installs `preprocessor` for `exception` and returns the one that was there -
/// null the first time, which is what a guest chains onto.
///
/// **Nothing in this runtime ever calls a preprocessor.** The only code that does
/// is the exception machinery, and `rosetta3` refuses that by name rather than
/// pretending to have it: `objc_exception_throw` is in its `REFUSED` table, so a
/// guest that throws is told so by name instead of reaching a preprocessor that
/// would not run. So the store and the hand-back are the whole of *this* entry
/// point's contract, and the reason is worth saying rather than leaving a reader
/// to assume the call is somewhere and merely not reached yet.
///
/// The value crosses as an address, deliberately, and is stored and returned
/// unchanged. That is what makes the chaining work: a guest that gets its own
/// previous preprocessor back and calls it later is calling its own code. Casting
/// it to something callable would make that a lie - on the emulated backend the
/// guest's code is not something the host may call directly, and the thunking that
/// would make it callable is what the refusal above makes unnecessary.
pub unsafe extern "C" fn objc_setExceptionPreprocessor(exception: Id, preprocessor: Imp) -> Id {
    crate::debug::validate(exception, "objc_setExceptionPreprocessor");
    if exception.is_null() {
        return std::ptr::null_mut();
    }
    let mut table = PREPROCESSORS.write().unwrap_or_else(|e| e.into_inner());
    // A null preprocessor clears the slot, the way setting an association to nil
    // does - and still hands back what was there.
    let previous = match preprocessor.is_null() {
        true => table.remove(&(exception as usize)),
        false => table.insert(exception as usize, preprocessor as usize),
    };
    previous.map_or(std::ptr::null_mut(), |was| was as *mut c_void)
}

/// `objc_property_attribute_t`: a name and a value, both C strings.
#[repr(C)]
pub struct PropertyAttribute {
    pub name: *const c_char,
    pub value: *const c_char,
}

/// A property handle.
///
/// # Safety
///
/// `p` must be null or a handle this runtime produced.
unsafe fn property(p: *const c_void) -> Option<&'static crate::property::Property> {
    crate::debug::validate_handle(p, "property", "Objective-C property API");
    (!p.is_null()).then(|| unsafe { &*(p as *const crate::property::Property) })
}

/// A block the caller frees with `free`, holding `values`.
///
/// The whole family of `class_copy...List` works this way, and a Rust
/// allocation handed across it would be freed by an allocator that never made
/// it. Neither would this process's own `malloc`: see [`crate::allocator`],
/// which is why the block comes from the *guest's* allocator and not from
/// `libc`.
unsafe fn copied_list<T>(values: Vec<T>, out_count: *mut u32) -> *mut T {
    // SAFETY: a nullable out-parameter, as every `copy...List` has.
    if !out_count.is_null() {
        unsafe { *out_count = values.len() as u32 };
    }
    if values.is_empty() {
        return std::ptr::null_mut();
    }
    // SAFETY: a positive size, and the result is checked before it is written.
    let block = unsafe { crate::allocator::guest_malloc(size_of::<T>() * values.len()) } as *mut T;
    if block.is_null() {
        return block;
    }
    for (i, value) in values.into_iter().enumerate() {
        // SAFETY: `i` is inside the allocation, which is uninitialized.
        unsafe { block.add(i).write(value) };
    }
    block
}

pub unsafe extern "C" fn class_copyPropertyList(
    cls: *const c_void,
    out_count: *mut u32,
) -> *mut *const c_void {
    let properties = unsafe { class(cls) }
        .map(|c| c.properties())
        .unwrap_or_default();
    let values: Vec<*const c_void> = properties
        .into_iter()
        .map(|p| p as *const crate::property::Property as *const c_void)
        .collect();
    // SAFETY: `out_count` is null or writable, as the C prototype requires.
    unsafe { copied_list(values, out_count) }
}

pub unsafe extern "C" fn class_getProperty(
    cls: *const c_void,
    name: *const c_char,
) -> *const c_void {
    let (Some(cls), Some(name)) = (unsafe { class(cls) }, unsafe { cstr(name) }) else {
        return std::ptr::null();
    };
    cls.property(name)
        .map_or(std::ptr::null(), |p| p as *const _ as *const c_void)
}

/// `class_addProperty`, which takes the attributes apart rather than as the
/// string the compiler emits - so they are put back together here.
pub unsafe extern "C" fn class_addProperty(
    cls: *const c_void,
    name: *const c_char,
    attributes: *const PropertyAttribute,
    count: u32,
) -> bool {
    let (Some(cls), Some(name)) = (unsafe { class(cls) }, unsafe { cstr(name) }) else {
        return false;
    };
    if cls.property(name).is_some() {
        return false;
    }
    // SAFETY: as above; `attributes` is null or `count` entries long.
    unsafe { cls.set_property(name, &attribute_string(attributes, count)) };
    true
}

pub unsafe extern "C" fn class_replaceProperty(
    cls: *const c_void,
    name: *const c_char,
    attributes: *const PropertyAttribute,
    count: u32,
) {
    let (Some(cls), Some(name)) = (unsafe { class(cls) }, unsafe { cstr(name) }) else {
        return;
    };
    unsafe { cls.set_property(name, &attribute_string(attributes, count)) };
}

/// The attribute string an `objc_property_attribute_t` array spells.
///
/// # Safety
///
/// `attributes` must be null or `count` entries of NUL-terminated strings.
unsafe fn attribute_string(attributes: *const PropertyAttribute, count: u32) -> std::ffi::CString {
    let mut fields: Vec<Vec<u8>> = Vec::new();
    for i in 0..count as usize {
        if attributes.is_null() {
            break;
        }
        let entry = unsafe { &*attributes.add(i) };
        let (Some(name), value) = (unsafe { cstr(entry.name) }, unsafe { cstr(entry.value) })
        else {
            continue;
        };
        let mut field = name.to_bytes().to_vec();
        field.extend_from_slice(value.map_or(b"", |v| v.to_bytes()));
        fields.push(field);
    }
    std::ffi::CString::new(fields.join(&b',')).unwrap_or_default()
}

pub unsafe extern "C" fn property_getName(p: *const c_void) -> *const c_char {
    unsafe { property(p) }.map_or(c"".as_ptr(), |p| p.name())
}

pub unsafe extern "C" fn property_getAttributes(p: *const c_void) -> *const c_char {
    unsafe { property(p) }.map_or(std::ptr::null(), |p| p.attributes())
}

/// `property_copyAttributeList`.
///
/// The strings the entries point at are the property's own and outlive the call;
/// only the array is the caller's to free, which is what Apple documents and
/// what a caller that frees the whole thing with one `free` depends on.
pub unsafe extern "C" fn property_copyAttributeList(
    p: *const c_void,
    out_count: *mut u32,
) -> *mut PropertyAttribute {
    let Some(p) = (unsafe { property(p) }) else {
        // SAFETY: null or writable.
        if !out_count.is_null() {
            unsafe { *out_count = 0 };
        }
        return std::ptr::null_mut();
    };
    // Leaked with the property, because the caller frees the array and not what
    // it points at.
    let values: Vec<PropertyAttribute> = p
        .attribute_list()
        .into_iter()
        .map(|(name, value)| PropertyAttribute {
            name: Box::leak(Box::new(name)).as_ptr(),
            value: Box::leak(Box::new(value)).as_ptr(),
        })
        .collect();
    // SAFETY: `out_count` is null or writable.
    unsafe { copied_list(values, out_count) }
}

/// `objc_loadClassref`, which resolves a class reference slot.
///
/// Apple's low bit marks a *stub* class, whose initializer has to be called
/// before the slot means anything. This runtime has no stub classes - a class is
/// a class from the moment it is allocated - so the slot already holds the
/// answer and the bit is never set.
pub unsafe extern "C" fn objc_loadClassref(slot: *mut *mut c_void) -> *mut c_void {
    match slot.is_null() {
        true => std::ptr::null_mut(),
        // SAFETY: the caller's obligation: a writable class reference slot.
        false => unsafe { *slot },
    }
}

pub unsafe extern "C" fn objc_getProtocol(name: *const c_char) -> *mut c_void {
    as_ptr(unsafe { cstr(name) }.and_then(crate::protocol::lookup))
}

pub unsafe extern "C" fn objc_allocateProtocol(name: *const c_char) -> *mut c_void {
    as_ptr(unsafe { cstr(name) }.and_then(crate::protocol::allocate))
}

pub unsafe extern "C" fn objc_registerProtocol(proto: *const c_void) {
    if let Some(proto) = unsafe { protocol(proto) } {
        crate::protocol::register(proto);
    }
}

pub unsafe extern "C" fn protocol_getName(proto: *const c_void) -> *const c_char {
    unsafe { protocol(proto) }.map_or(c"nil".as_ptr(), |p| p.name())
}

pub unsafe extern "C" fn protocol_addProtocol(proto: *const c_void, addition: *const c_void) {
    if let (Some(proto), Some(addition)) =
        (unsafe { protocol(proto) }, unsafe { protocol(addition) })
    {
        proto.add_protocol(addition);
    }
}

pub unsafe extern "C" fn protocol_addMethodDescription(
    proto: *const c_void,
    name: Sel,
    types: *const c_char,
    is_required: bool,
    is_instance: bool,
) {
    if let (Some(proto), Some(types)) = (unsafe { protocol(proto) }, unsafe { cstr(types) }) {
        proto.add_method(name, types, is_required, is_instance);
    }
}

/// Returned by value: two words, a selector and a type encoding.
pub unsafe extern "C" fn protocol_getMethodDescription(
    proto: *const c_void,
    name: Sel,
    is_required: bool,
    is_instance: bool,
) -> Description {
    unsafe { protocol(proto) }.map_or(
        Description {
            sel: std::ptr::null(),
            types: std::ptr::null(),
        },
        |p| p.method(name, is_required, is_instance),
    )
}

pub unsafe extern "C" fn objc_retain(object: Id) -> Id {
    crate::arc::retain(object)
}

pub unsafe extern "C" fn objc_release(object: Id) {
    crate::arc::release(object);
}

pub unsafe extern "C" fn objc_autorelease(object: Id) -> Id {
    crate::arc::autorelease(object)
}

pub unsafe extern "C" fn objc_autoreleasePoolPush() -> *mut c_void {
    crate::arc::pool_push()
}

pub unsafe extern "C" fn objc_autoreleasePoolPop(token: *mut c_void) {
    crate::arc::pool_pop(token);
}

/// The return-value optimisations, none of which this runtime elides.
///
/// Eliding them needs the caller and the callee to agree through a marker
/// instruction after the call site, which is a bargain between two compilations
/// this runtime is not part of. Doing the plain thing is always correct.
pub unsafe extern "C" fn objc_autoreleaseReturnValue(object: Id) -> Id {
    crate::arc::autorelease(object)
}

pub unsafe extern "C" fn objc_retainAutoreleaseReturnValue(object: Id) -> Id {
    crate::arc::autorelease(crate::arc::retain(object))
}

pub unsafe extern "C" fn objc_retainAutoreleasedReturnValue(object: Id) -> Id {
    crate::arc::retain(object)
}

pub unsafe extern "C" fn objc_unsafeClaimAutoreleasedReturnValue(object: Id) -> Id {
    crate::debug::validate(object, "objc_unsafeClaimAutoreleasedReturnValue");
    object
}

pub unsafe extern "C" fn objc_retainAutorelease(object: Id) -> Id {
    crate::arc::autorelease(crate::arc::retain(object))
}

pub unsafe extern "C" fn objc_storeStrong(slot: *mut Id, value: Id) {
    if slot.is_null() {
        return;
    }
    // SAFETY: the caller owns `slot`.
    let previous = unsafe { *slot };
    if previous == value {
        return;
    }
    crate::arc::retain(value);
    unsafe { *slot = value };
    crate::arc::release(previous);
}

pub unsafe extern "C" fn objc_initWeak(slot: *mut Id, value: Id) -> Id {
    if slot.is_null() {
        return std::ptr::null_mut();
    }
    crate::arc::init_weak(slot, value)
}

pub unsafe extern "C" fn objc_storeWeak(slot: *mut Id, value: Id) -> Id {
    match slot.is_null() {
        true => std::ptr::null_mut(),
        false => crate::arc::store_weak(slot, value),
    }
}

pub unsafe extern "C" fn objc_loadWeakRetained(slot: *mut Id) -> Id {
    match slot.is_null() {
        true => std::ptr::null_mut(),
        false => crate::arc::load_weak_retained(slot),
    }
}

pub unsafe extern "C" fn objc_loadWeak(slot: *mut Id) -> Id {
    // SAFETY: the caller owns `slot`.
    match slot.is_null() {
        true => std::ptr::null_mut(),
        false => crate::arc::autorelease(unsafe { objc_loadWeakRetained(slot) }),
    }
}

pub unsafe extern "C" fn objc_destroyWeak(slot: *mut Id) {
    if !slot.is_null() {
        crate::arc::destroy_weak(slot);
    }
}

pub unsafe extern "C" fn objc_copyWeak(to: *mut Id, from: *mut Id) {
    if to.is_null() || from.is_null() {
        return;
    }
    crate::debug::weak_load(from, "objc_copyWeak");
    // SAFETY: the caller owns both slots and `to` is uninitialized.
    crate::arc::init_weak(to, unsafe { *from });
}

pub unsafe extern "C" fn objc_moveWeak(to: *mut Id, from: *mut Id) {
    // SAFETY: the caller owns both slots.
    unsafe { objc_copyWeak(to, from) };
    unsafe { objc_destroyWeak(from) };
}

pub unsafe extern "C" fn objc_sync_enter(object: Id) -> i32 {
    crate::arc::sync_enter(object);
    0
}

pub unsafe extern "C" fn objc_sync_exit(object: Id) -> i32 {
    crate::arc::sync_exit(object);
    0
}

pub unsafe extern "C" fn objc_setAssociatedObject(
    object: Id,
    key: *const c_void,
    value: Id,
    _policy: usize,
) {
    crate::arc::set_associated(object, key, value);
}

pub unsafe extern "C" fn objc_getAssociatedObject(object: Id, key: *const c_void) -> Id {
    crate::arc::associated(object, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registered subclass of `NSObject` declaring `methods`.
    ///
    /// Each is `(selector, IMP, type encoding)`, the three things
    /// `class_addMethod` takes, so a test never has to invent an encoding
    /// half way down.
    ///
    /// The name is the caller's, as everywhere else in this crate's tests: class
    /// names are global and these share a process.
    fn class_with(name: &CStr, methods: &[(&str, Imp, &CStr)]) -> &'static Class {
        crate::bootstrap();
        let nsobject = crate::class::lookup(c"NSObject").expect("NSObject is this runtime's own");
        let class = crate::class::allocate_pair(Some(nsobject), name, 0).expect("a fresh name");
        for (selector, imp, types) in methods {
            assert!(class.add_method(crate::sel::get(selector), *imp, types));
        }
        crate::class::register_pair(class);
        class
    }

    /// A registered subclass of `base`, which is where an inherited `-dispose`
    /// comes from.
    fn subclass_of(base: &'static Class, name: &CStr) -> &'static Class {
        crate::bootstrap();
        let class = crate::class::allocate_pair(Some(base), name, 0).expect("a fresh name");
        crate::class::register_pair(class);
        class
    }

    /// A fresh instance of `class`.
    fn instance(class: &'static Class) -> Id {
        let object = crate::object::create_instance(class, 0);
        assert!(!object.is_null(), "a registered class allocates");
        object
    }

    /// `-answer` on a fresh instance of `class`, as a call site observes it.
    ///
    /// Through [`crate::msg::entry`] rather than by calling the `IMP` this test
    /// happens to hold, so what is asserted is what a guest's `objc_msgSend`
    /// reaches.
    fn answer(class: &'static Class) -> u64 {
        let object = instance(class);
        // SAFETY: `-answer` is `uint64_t (*)(id, SEL)`, and `object` is live.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> u64 =
                std::mem::transmute(crate::msg::entry());
            send(object, crate::sel::get("answer"))
        }
    }

    /// The `-answer` `Method` of `class`, as the C API hands it out.
    fn answer_method(class: &'static Class) -> *const c_void {
        // SAFETY: `class` is a class this runtime registered and `answer` a
        // selector it declares. Spelled `crate::class` because this module's own
        // `class` is the handle-decoding helper of the same name.
        let found = unsafe {
            class_getInstanceMethod(
                crate::class::ptr(class) as *const c_void,
                crate::sel::get("answer"),
            )
        };
        assert!(!found.is_null(), "the class declares -answer");
        found
    }

    /// The two bodies an exchange trades between.
    ///
    /// Distinct values, and they must be: identical bodies are the same machine
    /// code, the linker folds them together at `--release`, and then both casts
    /// yield the *same* address - the exchange becomes unobservable and the test
    /// fails with nothing wrong with the code it is about.
    unsafe extern "C" fn one(_receiver: Id, _sel: Sel) -> u64 {
        std::hint::black_box(1u64)
    }

    unsafe extern "C" fn two(_receiver: Id, _sel: Sel) -> u64 {
        std::hint::black_box(2u64)
    }

    /// The whole of the contract, as a guest sees it: after the exchange, calling
    /// each selector reaches the *other* implementation.
    #[test]
    fn an_exchange_makes_each_selector_reach_the_other_implementation() {
        let first = class_with(c"Rosetta3ExchangeA", &[("answer", one as Imp, c"Q16@0:8")]);
        let second = class_with(c"Rosetta3ExchangeB", &[("answer", two as Imp, c"Q16@0:8")]);
        assert_eq!(answer(first), 1, "before the exchange");
        assert_eq!(answer(second), 2, "before the exchange");

        let (a, b) = (answer_method(first), answer_method(second));
        // SAFETY: two `Method` handles this runtime produced, one from each class.
        unsafe { method_exchangeImplementations(a, b) };

        assert_eq!(
            answer(first),
            2,
            "the first class's -answer still runs the first class's body"
        );
        assert_eq!(
            answer(second),
            1,
            "the second class's -answer still runs the second class's body"
        );

        // And it is an exchange rather than a copy: doing it twice is the identity.
        // SAFETY: as above.
        unsafe { method_exchangeImplementations(a, b) };
        assert_eq!(answer(first), 1);
        assert_eq!(answer(second), 2);
    }

    /// The same thing from the other side: what the two `Method` objects hold.
    ///
    /// Asserted apart from the send because this is the value a caller saves
    /// *before* swizzling in order to call the original afterwards, and the value
    /// `rosetta3`'s dispatch plan re-probes.
    #[test]
    fn an_exchange_swaps_the_implementations_the_methods_report() {
        let first = class_with(c"Rosetta3ExchangeC", &[("answer", one as Imp, c"Q16@0:8")]);
        let second = class_with(c"Rosetta3ExchangeD", &[("answer", two as Imp, c"Q16@0:8")]);
        let (a, b) = (answer_method(first), answer_method(second));
        // SAFETY: two `Method` handles this runtime produced.
        unsafe {
            assert_eq!(method_getImplementation(a), one as Imp);
            assert_eq!(method_getImplementation(b), two as Imp);
            method_exchangeImplementations(a, b);
            assert_eq!(method_getImplementation(a), two as Imp);
            assert_eq!(method_getImplementation(b), one as Imp);
        }
    }

    /// An exchange moves the probe a cached dispatch plan is checked against, and it
    /// moves it for **both** classes - which is the part a pair of independent
    /// writes could plausibly get wrong.
    #[test]
    fn an_exchange_moves_the_dispatch_probe_for_both_classes() {
        let first = class_with(c"Rosetta3ExchangeE", &[("answer", one as Imp, c"Q16@0:8")]);
        let second = class_with(c"Rosetta3ExchangeF", &[("answer", two as Imp, c"Q16@0:8")]);
        let sel = crate::sel::get("answer");
        let before = (
            crate::class::method_implementation(first, sel),
            crate::class::method_implementation(second, sel),
        );
        // SAFETY: two `Method` handles this runtime produced.
        unsafe { method_exchangeImplementations(answer_method(first), answer_method(second)) };
        let after = (
            crate::class::method_implementation(first, sel),
            crate::class::method_implementation(second, sel),
        );
        assert_eq!(after, (before.1, before.0), "the two probes traded");
        assert_ne!(
            after.0, before.0,
            "a plan keyed on the old probe stays valid"
        );
    }

    /// One test's pair of counting methods.
    ///
    /// A macro because every call site needs its own `static`s: these tests share a
    /// process and run at once, so a shared counter would report another test's
    /// sends as its own.
    ///
    /// Both methods are always generated and each test installs one of them, so the
    /// other half is dead here - allowed rather than parameterised, because a macro
    /// taking a list of selectors to synthesize would be more machinery than the
    /// four tests it would serve.
    macro_rules! counted {
        ($name:ident) => {
            mod $name {
                #![allow(dead_code)]

                use super::*;
                use std::sync::atomic::{AtomicU64, Ordering};

                pub static DISPOSE: AtomicU64 = AtomicU64::new(0);
                pub static DEALLOC: AtomicU64 = AtomicU64::new(0);

                pub unsafe extern "C" fn dispose(_receiver: Id, _sel: Sel) {
                    DISPOSE.fetch_add(1, Ordering::Relaxed);
                }

                pub unsafe extern "C" fn dealloc(_receiver: Id, _sel: Sel) {
                    DEALLOC.fetch_add(1, Ordering::Relaxed);
                }

                /// `(-dispose, -dealloc)` sends this pair has seen. Asserted as a
                /// pair rather than one at a time, because "the entry point picked
                /// `dispose`" and "the entry point also ran `dealloc`" are one fact.
                pub fn saw() -> (u64, u64) {
                    (
                        DISPOSE.load(Ordering::Relaxed),
                        DEALLOC.load(Ordering::Relaxed),
                    )
                }
            }
        };
    }

    counted!(both);
    counted!(dealloc_only);
    counted!(inherited);
    counted!(inherited_dealloc);

    /// A `-dealloc` that is a real one: it counts, and then hands on to
    /// `NSObject`'s own, so the memory is actually given back.
    ///
    /// Hand-written rather than from [`counted`] because it is the only method here
    /// that frees, and the counting has to happen *before* the chain continues -
    /// afterwards the receiver is memory the allocator owns again.
    ///
    /// The hand-on is a *looked-up* `NSObject` implementation rather than a send of
    /// `dealloc` to the receiver, which would find this method again and recurse
    /// forever. A guest writes `objc_msgSendSuper`; the answer it reaches is the
    /// same one, and this is the shape of it a host can hold.
    mod chained {
        use super::*;
        use std::sync::atomic::{AtomicU64, Ordering};

        pub static DEALLOC: AtomicU64 = AtomicU64::new(0);

        pub unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
            DEALLOC.fetch_add(1, Ordering::Relaxed);
            let nsobject =
                crate::class::lookup(c"NSObject").expect("NSObject is this runtime's own");
            let imp = crate::class::method_implementation(nsobject, crate::sel::get("dealloc"));
            // SAFETY: `-dealloc` is `void (*)(id, SEL)` in every Objective-C there
            // has ever been, and `NSObject`'s is the one a chain ends in.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(imp);
                f(receiver, crate::sel::get("dealloc"))
            }
        }
    }

    /// `-dispose` is the preferred spelling when the class has one, and `-dealloc`
    /// is not run as well - a second destruction of the same object.
    #[test]
    fn object_dispose_prefers_dispose_when_the_class_has_it() {
        let class = class_with(
            c"Rosetta3DisposeA",
            &[
                ("dispose", both::dispose as Imp, c"v16@0:8"),
                ("dealloc", both::dealloc as Imp, c"v16@0:8"),
            ],
        );
        let object = instance(class);
        // SAFETY: a live object, which is what the entry point takes.
        let returned = unsafe { object_dispose(object) };
        assert_eq!(returned, object, "the receiver comes back");
        assert_eq!(
            both::saw(),
            (1, 0),
            "one -dispose and no -dealloc, or one of each"
        );
    }

    /// And `-dealloc` is the answer when there is no `-dispose`.
    #[test]
    fn object_dispose_answers_dealloc_when_there_is_no_dispose() {
        let class = class_with(
            c"Rosetta3DisposeB",
            &[("dealloc", dealloc_only::dealloc as Imp, c"v16@0:8")],
        );
        let object = instance(class);
        // SAFETY: a live object, which is what the entry point takes.
        let returned = unsafe { object_dispose(object) };
        assert_eq!(returned, object, "the receiver comes back");
        assert_eq!(
            dealloc_only::saw(),
            (0, 1),
            "a class with no -dispose was sent one, or its -dealloc twice"
        );
    }

    /// A `-dispose` inherited from a superclass counts, because the lookup is the
    /// same walk a message send does - and because a guest that asked
    /// `-respondsToSelector:` has to get the same answer this gives.
    #[test]
    fn object_dispose_finds_a_dispose_a_superclass_declares() {
        let base = class_with(
            c"Rosetta3DisposeC",
            &[("dispose", inherited::dispose as Imp, c"v16@0:8")],
        );
        let sub = subclass_of(base, c"Rosetta3DisposeCSub");
        let object = instance(sub);
        // SAFETY: a live object, which is what the entry point takes.
        assert_eq!(unsafe { object_dispose(object) }, object);
        assert_eq!(
            inherited::saw(),
            (1, 0),
            "the subclass's inherited -dispose did not run, or its -dealloc did too"
        );
    }

    /// And the same walk finds an inherited `-dealloc`, which is the case that
    /// matters for termination: a class inheriting it has no `-dispose` anywhere
    /// in its chain, so the entry point has to reach through to it.
    #[test]
    fn object_dispose_finds_a_dealloc_a_superclass_declares() {
        let base = class_with(
            c"Rosetta3DisposeC2",
            &[("dealloc", inherited_dealloc::dealloc as Imp, c"v16@0:8")],
        );
        let sub = subclass_of(base, c"Rosetta3DisposeC2Sub");
        let object = instance(sub);
        // SAFETY: a live object, which is what the entry point takes.
        assert_eq!(unsafe { object_dispose(object) }, object);
        assert_eq!(
            inherited_dealloc::saw(),
            (0, 1),
            "the subclass's inherited -dealloc did not run, or something else did"
        );
    }

    /// A class whose `-dealloc` is the real thing - it frees - runs it exactly
    /// once, and the two-selector rule terminates there.
    ///
    /// Only the counter is read, never the receiver: the entry point hands back a
    /// pointer to memory the allocator owns again, which is the contract a guest
    /// obeys by not touching what it is given. This is the test that says the rule
    /// cannot re-enter, and the one that would hang rather than fail if it did.
    #[test]
    fn object_dispose_reaches_a_real_dealloc_and_stops_there() {
        let class = class_with(
            c"Rosetta3DisposeD",
            &[("dealloc", chained::dealloc as Imp, c"v16@0:8")],
        );
        let object = instance(class);
        // SAFETY: a live object, which is what the entry point takes. The call
        // frees `object`, and nothing below reads it.
        unsafe {
            object_dispose(object);
        }
        assert_eq!(
            chained::DEALLOC.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the -dealloc chain ran something other than once"
        );
    }

    /// Messaging nil is the language, and this entry point inherits it.
    #[test]
    fn object_dispose_of_nil_is_nil() {
        // SAFETY: null is what the C prototype allows, and what the check is for.
        assert!(unsafe { object_dispose(std::ptr::null_mut()) }.is_null());
    }

    /// Two preprocessors, distinct for the reason the two `answer` bodies are.
    unsafe extern "C" fn first_preprocessor(_exception: Id) -> Id {
        std::hint::black_box(1u64) as Id
    }

    unsafe extern "C" fn second_preprocessor(_exception: Id) -> Id {
        std::hint::black_box(2u64) as Id
    }

    /// The hand-back, which is the whole of this entry point's contract: the first
    /// ask gets null, and every ask after it gets the function the previous ask
    /// installed, so a guest can chain onto it.
    #[test]
    fn an_exception_preprocessor_hands_back_the_one_it_replaced() {
        let exception = instance(class_with(c"Rosetta3PreprocessorA", &[]));
        // SAFETY: a live object, and two addresses this test owns.
        unsafe {
            assert!(
                objc_setExceptionPreprocessor(exception, first_preprocessor as Imp).is_null(),
                "the first ask has nothing to chain onto"
            );
            assert_eq!(
                objc_setExceptionPreprocessor(exception, second_preprocessor as Imp),
                first_preprocessor as Id,
                "the second ask did not get the first preprocessor back"
            );
            assert_eq!(
                objc_setExceptionPreprocessor(exception, first_preprocessor as Imp),
                second_preprocessor as Id,
                "the third ask did not get the second back"
            );
        }
    }

    /// The slot belongs to the exception, not to the runtime: a different exception
    /// has none of its own, which is what makes the return usable as a chain.
    #[test]
    fn the_preprocessor_slot_is_per_exception() {
        let class = class_with(c"Rosetta3PreprocessorB", &[]);
        let (first, second) = (instance(class), instance(class));
        // SAFETY: two live instances of one class, and an address this test owns.
        unsafe {
            assert!(
                objc_setExceptionPreprocessor(first, first_preprocessor as Imp).is_null(),
                "the first exception has no preprocessor of its own"
            );
            assert!(
                objc_setExceptionPreprocessor(second, first_preprocessor as Imp).is_null(),
                "installing on one exception installed it on the other"
            );
            assert_eq!(
                objc_setExceptionPreprocessor(first, second_preprocessor as Imp),
                first_preprocessor as Id
            );
        }
    }
}
