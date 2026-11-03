//! CF objects whose contents are a boxed Rust value.
//!
//! Shared so that every such type gives its contents back the same way: one
//! `-dealloc`, written once, rather than a copy per framework crate.

use std::ffi::c_void;

use objc::class::Class;
use objc::object::Id;

use crate::base::CFTypeRef;

/// The object, and a pointer to the Rust value holding its contents.
#[repr(C)]
struct Body<T> {
    isa: *const Class,
    contents: *mut T,
}

/// Register a class whose instances hold a `T`, with a `-dealloc` that gives
/// the contents back. `is_type` is [`crate::base::register`]'s.
pub fn register<T: 'static>(
    name: &'static std::ffi::CStr,
    superclass: Option<&'static Class>,
    is_type: bool,
) -> &'static Class {
    let class = crate::base::register(name, superclass, size_of::<Body<T>>(), is_type);
    unsafe extern "C" fn dealloc<T>(receiver: Id, _sel: objc::sel::Sel) {
        // SAFETY: `-dealloc` is reached with the last reference gone, and the
        // receiver is a `Body<T>` whose `contents` came from `Box::into_raw`.
        unsafe {
            let body = receiver as *mut Body<T>;
            if !(*body).contents.is_null() {
                drop(Box::from_raw((*body).contents));
                (*body).contents = std::ptr::null_mut();
            }
            objc::arc::finalize(receiver);
        }
    }
    class.replace_method(
        objc::sel::get("dealloc"),
        dealloc::<T> as *const c_void,
        c"v16@0:8",
    );
    class
}

/// Allocate an instance of `class` holding `contents`, or null.
pub fn make<T>(class: &'static Class, contents: T) -> CFTypeRef {
    // SAFETY: `Body<T>` begins with `isa` and is the layout `class` was
    // registered with.
    let body: *mut Body<T> = unsafe { crate::base::allocate(class) };
    if body.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a live instance whose second word is ours to fill in.
    unsafe { (*body).contents = Box::into_raw(Box::new(contents)) };
    body as CFTypeRef
}

/// The contents of `object`, if it is an instance of `class`.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated, and `class` one
/// [`register`] made for `T`.
pub unsafe fn contents<'a, T>(object: CFTypeRef, class: &'static Class) -> Option<&'a mut T> {
    if !unsafe { crate::base::is_kind_of(object, class) } {
        return None;
    }
    // SAFETY: an instance of `class` is a `Body<T>` with a live `contents`.
    unsafe {
        let body = object as *mut Body<T>;
        (!(*body).contents.is_null()).then(|| &mut *(*body).contents)
    }
}
