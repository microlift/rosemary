use std::ffi::{CStr, c_void};

use crate::class::{Class, ptr};

/// An object pointer.
pub type Id = *mut c_void;

/// Allocate and zero an instance of `class`, with `extra` bytes after the ivars.
///
/// Null if the class was never registered: an unregistered class has no fixed
/// size, and an instance of one would be a wrong program with no diagnostic.
#[track_caller]
pub fn create_instance(class: &'static Class, extra: usize) -> Id {
    if !class.is_registered() {
        return std::ptr::null_mut();
    }
    let Some(size) = class.instance_size().checked_add(extra) else {
        return std::ptr::null_mut();
    };
    let Some(allocation_size) = crate::debug::allocation_size(size) else {
        return std::ptr::null_mut();
    };
    // SAFETY: a non-zero size. Zeroed by hand because the registry holds
    // `malloc` and `free` and not `calloc` - and zeroed it must be, which is
    // what `class_createInstance` promises and what every `-init` that skips a
    // field relies on.
    let memory = unsafe { crate::allocator::guest_malloc(allocation_size) };
    if memory.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `size` bytes, freshly ours.
    unsafe { std::ptr::write_bytes(memory as *mut u8, 0, size) };
    // SAFETY: `memory` is at least one word and freshly ours.
    unsafe { *(memory as *mut *mut Class) = ptr(class) };
    // SAFETY: `memory` was allocated with `allocation_size`, including the
    // debug tail when enabled.
    unsafe { crate::debug::allocated(memory, class, size) };
    crate::census::allocated(class);
    memory
}

/// Run every `.cxx_destruct` of `object`'s classes, nearest first, as Apple's
/// `objc_destructInstance` does. ARC compiles the release of a class's strong
/// ivars into that method, so skipping it leaks whatever they hold.
///
/// # Safety
///
/// `object` must be null or an intact instance at the end of its `-dealloc`
/// chain.
pub unsafe fn destruct_instance(object: Id) {
    // SAFETY: the caller's obligation.
    let Some(class) = (unsafe { class_of(object) }) else {
        return;
    };
    let destruct = crate::sel::get(".cxx_destruct");
    // Apple's order: the ivars first, then the associations.
    let _associations = RemoveAssociations(object);
    for class in class.chain() {
        // Only a class's own: an inherited one runs once, for its own class.
        let Some(method) = class.own_method(destruct) else {
            continue;
        };
        // SAFETY: `.cxx_destruct` is `void (*)(id, SEL)`, compiled for this
        // class's ivars, which `object` has.
        unsafe {
            let f: unsafe extern "C" fn(Id, crate::sel::Sel) =
                std::mem::transmute(method.implementation());
            f(object, destruct);
        }
    }
}

/// Drops `object`'s associations when it goes out of scope, so they go even
/// if a `.cxx_destruct` unwinds.
struct RemoveAssociations(Id);

impl Drop for RemoveAssociations {
    fn drop(&mut self) {
        crate::arc::remove_associated(self.0);
    }
}

/// Free an instance [`create_instance`] returned.
///
/// # Safety
///
/// `object` must be null or an instance no one holds a reference to.
pub unsafe fn dispose_instance(object: Id) {
    if object.is_null() {
        return;
    }
    // SAFETY: the caller's obligation; the object is still intact.
    unsafe { destruct_instance(object) };
    // SAFETY: the object is still readable, which is what names its class.
    unsafe { crate::census::freed(object) };
    if crate::debug::finalized(object) {
        return;
    }
    // SAFETY: the caller's obligation, and the block came from
    // [`create_instance`], which takes it from the same allocator.
    unsafe { crate::allocator::guest_free(object) }
}

/// The class of `object`.
///
/// A class is an object whose `isa` is its metaclass, so this is the same read
/// either way - which is why `object_getClass` on a class gives the metaclass.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated, or a structure
/// whose first word is one of its class pointers.
pub unsafe fn class_of(object: Id) -> Option<&'static Class> {
    if object.is_null() {
        return None;
    }
    crate::debug::validate(object, "object_getClass");
    // SAFETY: the caller's obligation; the first word is the `isa`.
    let isa = unsafe { *(object as *const *const Class) };
    // SAFETY: an `isa` is a leaked `Class` or null.
    unsafe { crate::class::from_ptr(isa) }
}

/// Replace `object`'s class, returning the one it had.
///
/// # Safety
///
/// As [`class_of`], and the new class must have the same instance layout.
pub unsafe fn set_class(object: Id, class: &'static Class) -> Option<&'static Class> {
    if object.is_null() {
        return None;
    }
    let previous = unsafe { class_of(object) };
    crate::debug::set_class(object, class);
    // SAFETY: as above; the first word is the `isa`.
    unsafe { *(object as *mut *mut Class) = ptr(class) };
    previous
}

/// The bytes after the instance's own ivars, which `class_createInstance`'s
/// `extra` reserved.
///
/// # Safety
///
/// As [`class_of`].
pub unsafe fn indexed_ivars(object: Id) -> *mut c_void {
    let Some(class) = (unsafe { class_of(object) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the allocation is at least `instance_size` bytes.
    unsafe { (object as *mut u8).add(class.instance_size()) as *mut c_void }
}

/// Read the object-typed ivar `name`.
///
/// # Safety
///
/// As [`class_of`], and `name` must be an object-typed ivar of its class.
pub unsafe fn get_ivar(object: Id, name: &CStr) -> Id {
    let Some(offset) = (unsafe { ivar_offset(object, name) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the ivar is inside the allocation, and object-typed.
    unsafe { *((object as *mut u8).add(offset) as *const Id) }
}

/// Write the object-typed ivar `name`. Assigns; it does not retain.
///
/// # Safety
///
/// As [`get_ivar`].
pub unsafe fn set_ivar(object: Id, name: &CStr, value: Id) {
    let Some(offset) = (unsafe { ivar_offset(object, name) }) else {
        return;
    };
    unsafe { *((object as *mut u8).add(offset) as *mut Id) = value };
}

/// Where `name` sits in `object`.
///
/// # Safety
///
/// As [`class_of`].
unsafe fn ivar_offset(object: Id, name: &CStr) -> Option<usize> {
    let class = unsafe { class_of(object) }?;
    Some(class.ivar(name)?.offset() as usize)
}
