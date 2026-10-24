// The C spellings are the interface; renaming them would only hide it.
#![allow(non_snake_case)]

use std::ffi::c_void;
use std::mem::size_of;

use crate::object::Id;
use crate::sel::Sel;

/// Send `receiver` a nullary message returning an object.
///
/// # Safety
///
/// `receiver` must be null or a live object.
unsafe fn send0(receiver: Id, sel: Sel) -> Id {
    let Some(class) = (unsafe { crate::object::class_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    crate::class::ensure_initialized(class);
    let Some(imp) = class.method(sel) else {
        return std::ptr::null_mut();
    };
    // SAFETY: a nullary message returning an object is `id (*)(id, SEL)`.
    let f: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(imp.implementation()) };
    unsafe { f(receiver, sel) }
}

/// Send `receiver` a one-argument message returning a `BOOL`.
///
/// # Safety
///
/// As [`send0`], and the method must take one word and return a `BOOL`.
unsafe fn send1_bool(receiver: Id, sel: Sel, argument: usize) -> bool {
    let Some(class) = (unsafe { crate::object::class_of(receiver) }) else {
        return false;
    };
    crate::class::ensure_initialized(class);
    let Some(imp) = class.method(sel) else {
        return false;
    };
    let f: unsafe extern "C" fn(Id, Sel, usize) -> bool =
        unsafe { std::mem::transmute(imp.implementation()) };
    unsafe { f(receiver, sel, argument) }
}

pub unsafe extern "C" fn objc_alloc(cls: Id) -> Id {
    // SAFETY: a class is an object.
    unsafe { send0(cls, crate::sel::get("alloc")) }
}

pub unsafe extern "C" fn objc_allocWithZone(cls: Id) -> Id {
    unsafe { send0(cls, crate::sel::get("allocWithZone:")) }
}

pub unsafe extern "C" fn objc_alloc_init(cls: Id) -> Id {
    let object = unsafe { objc_alloc(cls) };
    // SAFETY: a freshly allocated object.
    unsafe { send0(object, crate::sel::get("init")) }
}

pub unsafe extern "C" fn objc_opt_new(cls: Id) -> Id {
    // SAFETY: a class is an object.
    unsafe { send0(cls, crate::sel::get("new")) }
}

pub unsafe extern "C" fn objc_opt_self(object: Id) -> Id {
    crate::debug::validate(object, "objc_opt_self");
    object
}

pub unsafe extern "C" fn objc_opt_class(object: Id) -> *mut c_void {
    if object.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a live object.
    let Some(class) = (unsafe { crate::object::class_of(object) }) else {
        return std::ptr::null_mut();
    };
    // A class answers `-class` with itself, not with its metaclass, so the
    // metaclass hop `object_getClass` would take has to be undone here.
    match class.is_meta() {
        true => object,
        false => crate::class::ptr(class) as *mut c_void,
    }
}

pub unsafe extern "C" fn objc_opt_isKindOfClass(object: Id, cls: *const c_void) -> bool {
    // SAFETY: a live object, and a class pointer.
    unsafe { send1_bool(object, crate::sel::get("isKindOfClass:"), cls as usize) }
}

pub unsafe extern "C" fn objc_opt_respondsToSelector(object: Id, sel: Sel) -> bool {
    // SAFETY: a live object, and a selector.
    unsafe { send1_bool(object, crate::sel::get("respondsToSelector:"), sel as usize) }
}

/// `@property` accessors the compiler folds, for a property whose ivar it names
/// by byte offset.
///
/// The atomic spellings are not atomic here. Making them so needs a per-property
/// lock, and nothing in this tree has yet produced a program that races a
/// property; when one does, the fix belongs here and not at the call site.
pub unsafe extern "C" fn objc_getProperty(
    receiver: Id,
    _sel: Sel,
    offset: isize,
    _atomic: bool,
) -> Id {
    if receiver.is_null() {
        return std::ptr::null_mut();
    }
    crate::debug::validate_range(receiver, offset, size_of::<Id>(), "objc_getProperty");
    // SAFETY: the compiler computed `offset` against this object's own class.
    let value = unsafe { *((receiver as *const u8).offset(offset) as *const Id) };
    crate::arc::autorelease(crate::arc::retain(value))
}

pub unsafe extern "C" fn objc_setProperty(
    receiver: Id,
    _sel: Sel,
    offset: isize,
    value: Id,
    _atomic: bool,
    _copy: i8,
) {
    // SAFETY: as `objc_getProperty`.
    unsafe { store_property(receiver, offset, value) }
}

/// The specialized spellings, whose arguments are in a different order from
/// [`objc_setProperty`]'s: no `atomic` or `copy`, because the name says both.
pub unsafe extern "C" fn objc_setProperty_atomic(
    receiver: Id,
    _sel: Sel,
    value: Id,
    offset: isize,
) {
    unsafe { store_property(receiver, offset, value) }
}

pub unsafe extern "C" fn objc_setProperty_nonatomic(
    receiver: Id,
    _sel: Sel,
    value: Id,
    offset: isize,
) {
    unsafe { store_property(receiver, offset, value) }
}

pub unsafe extern "C" fn objc_setProperty_atomic_copy(
    receiver: Id,
    _sel: Sel,
    value: Id,
    offset: isize,
) {
    unsafe { store_property(receiver, offset, copy_of(value)) }
}

pub unsafe extern "C" fn objc_setProperty_nonatomic_copy(
    receiver: Id,
    _sel: Sel,
    value: Id,
    offset: isize,
) {
    unsafe { store_property(receiver, offset, copy_of(value)) }
}

/// The getter and setter of a struct-typed property. Not atomic, for the
/// reason the object accessors above are not; `has_strong` only matters to a
/// garbage collector.
pub unsafe extern "C" fn objc_copyStruct(
    dest: *mut c_void,
    src: *const c_void,
    size: isize,
    _atomic: bool,
    _has_strong: bool,
) {
    if size <= 0 {
        return;
    }
    // SAFETY: the compiler passes an ivar and a local of `size` bytes each.
    unsafe { std::ptr::copy(src as *const u8, dest as *mut u8, size as usize) };
}

/// # Safety
///
/// `offset` must be an ivar offset of `receiver`'s own class.
unsafe fn store_property(receiver: Id, offset: isize, value: Id) {
    if receiver.is_null() {
        return;
    }
    crate::debug::validate_range(receiver, offset, size_of::<Id>(), "objc_setProperty");
    let slot = unsafe { (receiver as *mut u8).offset(offset) as *mut Id };
    unsafe { crate::api::objc_storeStrong(slot, value) };
}

/// `-copy`, or the object itself where the class does not implement it.
fn copy_of(value: Id) -> Id {
    // SAFETY: a live object or null.
    let copied = unsafe { send0(value, crate::sel::get("copy")) };
    match copied.is_null() {
        true => value,
        false => copied,
    }
}

/// `objc_retainBlock` is `_Block_copy` under another name.
///
/// A stack block has to be *copied*, not retained: retaining without copying
/// would hand back a pointer into a frame that is about to go away. See
/// [`crate::block`], which is this tree's libclosure.
pub unsafe extern "C" fn objc_retainBlock(block: Id) -> Id {
    crate::block::copy(block)
}

/// Mutating a collection while enumerating it. A named abort, as on Apple's.
pub unsafe extern "C" fn objc_enumerationMutation(object: Id) {
    // SAFETY: a live object.
    let class = unsafe { crate::object::class_of(object) };
    crate::fail!(
        "collection was mutated while being enumerated: {}",
        class.map_or("nil", |c| c.name_str())
    );
}

/// The `std::terminate` an uncaught Objective-C exception reaches.
pub unsafe extern "C" fn objc_terminate() -> ! {
    crate::fail!("objc_terminate");
}
