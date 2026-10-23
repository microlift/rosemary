// The C spellings are the interface; renaming them would only hide it.
#![allow(non_snake_case)]

use std::ffi::{CStr, c_char, c_void};

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
/// `rosemary`'s dispatch caches this value and re-probes it to notice a swizzle,
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
/// Never a wrapper: `rosemary` compares this against its own thunk table to tell
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

/// Swap two methods' implementations, as one step: a send racing the swap sees
/// one implementation or the other, never the same one twice.
pub unsafe extern "C" fn method_exchangeImplementations(a: *const c_void, b: *const c_void) {
    let (Some(a), Some(b)) = (unsafe { method(a) }, unsafe { method(b) }) else {
        return;
    };
    let first = a.implementation();
    let second = b.set_implementation(first);
    a.set_implementation(second);
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

/// `Ivar object_getInstanceVariable(id, const char *name, void **outValue)`:
/// the ivar by name, and its value as a pointer-sized word.
pub unsafe extern "C" fn object_getInstanceVariable(
    object: Id,
    name: *const c_char,
    out: *mut *mut c_void,
) -> *const c_void {
    let Some(class) = (unsafe { crate::object::class_of(object) }) else {
        return std::ptr::null();
    };
    let ivar = unsafe { class_getInstanceVariable(crate::class::ptr(class).cast(), name) };
    if !ivar.is_null() && !out.is_null() {
        let offset = unsafe { ivar_getOffset(ivar) };
        // SAFETY: the ivar is inside the object, and the caller asked for its
        // first word.
        unsafe { *out = *((object as *const u8).offset(offset) as *const *mut c_void) };
    }
    ivar
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
    // SAFETY: the caller is about to free the object itself.
    unsafe { crate::object::destruct_instance(object) };
    object
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

/// The return-value handoff: a callee that ends in `objc_autoreleaseReturnValue`
/// and a caller that begins with `objc_retainAutoreleasedReturnValue` skip the
/// autorelease and the retain between them.
///
/// Not an optimisation a guest can do without. Code that returns objects on a
/// thread with no autorelease pool - Unity's render thread does, every frame -
/// is correct on a Mac only because the handoff keeps those objects out of the
/// pool; without it they pile up in a pool that is never drained.
///
/// The callee recognises a caller that will claim the object from the
/// instruction at its return address, as Apple's runtime does, and leaves the
/// object for it in a thread-local slot. The slot names the object, so a claim
/// of anything else takes the ordinary path.
mod handoff {
    use std::cell::Cell;

    use crate::object::Id;

    thread_local! {
        static PENDING: Cell<usize> = const { Cell::new(0) };
    }

    /// Whether the caller returned to at `ra` claims the object at once.
    ///
    /// # Safety
    ///
    /// `ra` must be a return address: code that is mapped and readable.
    unsafe fn caller_claims(ra: usize) -> bool {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: every address read is code the caller is about to run, or a
        // pointer slot that code reads.
        unsafe {
            let read32 = |at: usize| (at as *const i32).read_unaligned() as isize;
            let code = ra as *const u8;
            // mov rdi, rax
            if std::slice::from_raw_parts(code, 3) != [0x48, 0x89, 0xc7] {
                return false;
            }
            let slot = match *code.add(3) {
                // call stub; the stub is `jmp [rip + slot]`.
                0xe8 => {
                    let stub = (ra + 8).wrapping_add_signed(read32(ra + 4));
                    if (stub as *const [u8; 2]).read_unaligned() != [0xff, 0x25] {
                        return false;
                    }
                    (stub + 6).wrapping_add_signed(read32(stub + 2))
                }
                // call [rip + slot]
                0xff if *code.add(4) == 0x15 => (ra + 9).wrapping_add_signed(read32(ra + 5)),
                _ => return false,
            };
            let target = (slot as *const usize).read_unaligned();
            target == super::objc_retainAutoreleasedReturnValue as *const () as usize
                || target == super::objc_unsafeClaimAutoreleasedReturnValue as *const () as usize
        }
        #[cfg(target_arch = "aarch64")]
        // SAFETY: the instruction the caller returns to.
        unsafe {
            // mov x29, x29: the marker a claiming caller puts after its call.
            (ra as *const u32).read_unaligned() == 0xaa1d03fd
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            let _ = ra;
            false
        }
    }

    /// Hand `object`, which carries a reference the caller is owed, to a
    /// claiming caller, or autorelease it.
    pub(super) unsafe extern "C" fn give(object: Id, ra: usize) -> Id {
        // SAFETY: `ra` came from the trampoline, which read the return address.
        if !object.is_null() && unsafe { caller_claims(ra) } {
            let stale = PENDING.replace(object as usize);
            if stale != 0 {
                // A handoff nobody claimed; its reference was owed to a pool.
                crate::arc::autorelease(stale as Id);
            }
            return object;
        }
        crate::arc::autorelease(object)
    }

    /// Whether `object` was handed off on this thread, taking it if so.
    pub(super) fn claim(object: Id) -> bool {
        if object.is_null() || PENDING.get() != object as usize {
            return false;
        }
        PENDING.set(0);
        true
    }

    pub(super) unsafe extern "C" fn retain_and_give(object: Id, ra: usize) -> Id {
        unsafe { give(crate::arc::retain(object), ra) }
    }
}

/// Enters `$target(object, return address)`.
macro_rules! with_return_address {
    ($name:ident, $target:path) => {
        #[cfg(target_arch = "x86_64")]
        #[unsafe(naked)]
        pub unsafe extern "C" fn $name(object: Id) -> Id {
            core::arch::naked_asm!("mov rsi, [rsp]", "jmp {target}", target = sym $target)
        }
        #[cfg(target_arch = "aarch64")]
        #[unsafe(naked)]
        pub unsafe extern "C" fn $name(object: Id) -> Id {
            core::arch::naked_asm!("mov x1, x30", "b {target}", target = sym $target)
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        pub unsafe extern "C" fn $name(object: Id) -> Id {
            unsafe { $target(object, 0) }
        }
    };
}

with_return_address!(objc_autoreleaseReturnValue, handoff::give);
with_return_address!(objc_retainAutoreleaseReturnValue, handoff::retain_and_give);

pub unsafe extern "C" fn objc_retainAutoreleasedReturnValue(object: Id) -> Id {
    if handoff::claim(object) {
        return object;
    }
    crate::arc::retain(object)
}

pub unsafe extern "C" fn objc_unsafeClaimAutoreleasedReturnValue(object: Id) -> Id {
    crate::debug::validate(object, "objc_unsafeClaimAutoreleasedReturnValue");
    if handoff::claim(object) {
        // The handoff carried a reference this caller does not keep.
        crate::arc::release(object);
    }
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
    policy: usize,
) {
    crate::arc::set_associated_with_policy(object, key, value, policy);
}

pub unsafe extern "C" fn objc_removeAssociatedObjects(object: Id) {
    crate::arc::remove_associated(object);
}

pub unsafe extern "C" fn objc_getAssociatedObject(object: Id, key: *const c_void) -> Id {
    crate::arc::associated(object, key)
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    /// A caller's code after its call: `mov rdi, rax`, then a call through
    /// `slot` (`call [rip + slot]`), or through a stub (`call stub`, where the
    /// stub is `jmp [rip + slot]`). Answers the code and its return address.
    fn caller(through_stub: bool, claimer: usize) -> (Box<[u8; 64]>, usize) {
        let mut code = Box::new([0xccu8; 64]);
        let base = code.as_ptr() as usize;
        let slot = 48;
        code[slot..slot + 8].copy_from_slice(&claimer.to_le_bytes());
        code[..3].copy_from_slice(&[0x48, 0x89, 0xc7]);
        let rel = |from: usize, to: usize| ((to as isize - from as isize) as i32).to_le_bytes();
        if through_stub {
            let stub = 32;
            code[3] = 0xe8;
            code[4..8].copy_from_slice(&rel(base + 8, base + stub));
            code[stub..stub + 2].copy_from_slice(&[0xff, 0x25]);
            code[stub + 2..stub + 6].copy_from_slice(&rel(base + stub + 6, base + slot));
        } else {
            code[3..5].copy_from_slice(&[0xff, 0x15]);
            code[5..9].copy_from_slice(&rel(base + 9, base + slot));
        }
        (code, base)
    }

    fn object() -> Id {
        crate::bootstrap();
        let class = crate::class::lookup(c"NSObject").expect("NSObject");
        crate::object::create_instance(class, 0)
    }

    /// The count `object` is left with once a pool around `body` drains.
    fn after_pool(object: Id, body: impl FnOnce()) -> usize {
        let pool = crate::arc::pool_push();
        body();
        crate::arc::pool_pop(pool);
        crate::arc::retain_count(object)
    }

    #[test]
    fn a_claimed_return_value_never_reaches_the_pool() {
        let claimer = objc_retainAutoreleasedReturnValue as *const () as usize;
        for through_stub in [false, true] {
            let (_code, ra) = caller(through_stub, claimer);
            let object = object();
            // The callee's +1 is handed over and the claim keeps it, with
            // nothing left in the pool to release it again.
            let count = after_pool(object, || unsafe {
                let returned = handoff::give(object, ra);
                assert_eq!(objc_retainAutoreleasedReturnValue(returned), object);
                assert_eq!(crate::arc::retain_count(object), 1, "{through_stub}");
            });
            assert_eq!(count, 1, "through_stub={through_stub}");
            crate::arc::release(object);
        }
    }

    #[test]
    fn an_unsafe_claim_gives_the_handed_reference_back() {
        let claimer = objc_unsafeClaimAutoreleasedReturnValue as *const () as usize;
        let (_code, ra) = caller(false, claimer);
        let object = object();
        crate::arc::retain(object);
        let count = after_pool(object, || unsafe {
            let returned = handoff::give(object, ra);
            objc_unsafeClaimAutoreleasedReturnValue(returned);
            assert_eq!(crate::arc::retain_count(object), 1);
        });
        assert_eq!(count, 1);
        crate::arc::release(object);
    }

    #[test]
    fn a_caller_that_does_not_claim_gets_an_autoreleased_value() {
        let (_code, ra) = caller(false, objc_retain as *const () as usize);
        let object = object();
        crate::arc::retain(object);
        // Autoreleased, so the pool takes the extra reference back.
        let count = after_pool(object, || unsafe {
            handoff::give(object, ra);
        });
        assert_eq!(count, 1);
        // A later claim of the same object is an ordinary retain.
        unsafe { objc_retainAutoreleasedReturnValue(object) };
        assert_eq!(crate::arc::retain_count(object), 2);
        crate::arc::release(object);
        crate::arc::release(object);
    }

    /// A retaining association keeps its value alive, and the value is let
    /// go of when the owner is destroyed - so a later object at the owner's
    /// address does not inherit it.
    #[test]
    fn a_retained_association_lives_and_dies_with_its_owner() {
        static KEY: u8 = 0;
        let key = &KEY as *const u8 as *const c_void;
        let (owner, value) = (object(), object());
        // SAFETY: two live objects; 0x301 is OBJC_ASSOCIATION_RETAIN.
        unsafe { objc_setAssociatedObject(owner, key, value, 0x301) };
        assert_eq!(crate::arc::retain_count(value), 2);
        assert_eq!(unsafe { objc_getAssociatedObject(owner, key) }, value);
        // SAFETY: the owner is intact and about to be freed.
        unsafe { crate::object::destruct_instance(owner) };
        assert_eq!(crate::arc::retain_count(value), 1);
        assert!(unsafe { objc_getAssociatedObject(owner, key) }.is_null());
        crate::arc::release(value);
    }

    static FORWARD_TARGET: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    unsafe extern "C" fn answer(_receiver: Id, _sel: crate::sel::Sel) -> usize {
        42
    }

    unsafe extern "C" fn target_for(
        _receiver: Id,
        _sel: crate::sel::Sel,
        _wanted: crate::sel::Sel,
    ) -> Id {
        FORWARD_TARGET.load(std::sync::atomic::Ordering::Acquire) as Id
    }

    /// A message the receiver does not answer goes to the object its
    /// `-forwardingTargetForSelector:` names, and arrives with that object as
    /// its receiver.
    #[test]
    fn a_forwarding_target_receives_the_message() {
        crate::bootstrap();
        let root = crate::class::lookup(c"NSObject").unwrap();
        let answering = crate::class::allocate_pair(Some(root), c"ForwardTestTarget", 0).unwrap();
        answering.add_method(
            crate::sel::get("forwardTestAnswer"),
            answer as crate::class::Imp,
            c"Q16@0:8",
        );
        crate::class::register_pair(answering);
        let proxy = crate::class::allocate_pair(Some(root), c"ForwardTestProxy", 0).unwrap();
        proxy.add_method(
            crate::sel::get("forwardingTargetForSelector:"),
            target_for as crate::class::Imp,
            c"@24@0:8:16",
        );
        crate::class::register_pair(proxy);
        let target = crate::object::create_instance(answering, 0);
        FORWARD_TARGET.store(target as usize, std::sync::atomic::Ordering::Release);
        let sender = crate::object::create_instance(proxy, 0);
        // SAFETY: `NSUInteger (id, SEL)`, sent through the trampoline.
        let got = unsafe {
            let f: unsafe extern "C" fn(Id, crate::sel::Sel) -> usize =
                std::mem::transmute(crate::msg::entry());
            f(sender, crate::sel::get("forwardTestAnswer"))
        };
        assert_eq!(got, 42);
        crate::arc::release(sender);
        crate::arc::release(target);
    }
}
