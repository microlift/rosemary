use std::ffi::CStr;

use objc::class::Class;
use objc::object::Id;

#[repr(C)]
pub(crate) struct Object<T> {
    isa: *const Class,
    body: T,
}

/// Declare a Metal class whose instances carry a `T`.
pub(crate) fn register<T>(name: &CStr) -> &'static Class {
    corefoundation::base::register(name, None, size_of::<Object<T>>(), false)
}

/// Allocate one instance of a declared class and install its body.
pub(crate) fn make<T>(name: &CStr, body: T) -> Id {
    let class = objc::class::lookup(name).expect("Metal is bootstrapped");
    // SAFETY: `Object<T>` is the layout `register::<T>` declared for this class.
    let object = unsafe { corefoundation::base::allocate::<Object<T>>(class) };
    if object.is_null() {
        return std::ptr::null_mut();
    }
    // `allocate` initializes only the Objective-C header, so the body is
    // uninitialized memory: write through it without dropping first.
    // SAFETY: a fresh instance whose body is this call's to establish.
    unsafe { std::ptr::addr_of_mut!((*object).body).write(body) };
    object as Id
}

/// Install the body of an instance the Objective-C runtime allocated, which
/// is how a class a guest makes with `+alloc` and `-init` gets one.
///
/// # Safety
///
/// `receiver` must be an instance of a class declared for this `T` whose body
/// has not been established, or has been dropped since.
pub(crate) unsafe fn init<T>(receiver: Id, body: T) {
    // SAFETY: the caller guarantees the layout; the old body is not dropped,
    // because there is none.
    unsafe { std::ptr::addr_of_mut!((*(receiver as *mut Object<T>)).body).write(body) };
}

/// The body of an object of a class declared with `register::<T>`.
///
/// # Safety
///
/// `receiver` must be an instance of a class declared for this `T`. Every
/// caller is a method implementation registered on exactly that class.
pub(crate) unsafe fn body<'a, T>(receiver: Id) -> &'a mut T {
    // SAFETY: the caller guarantees the receiver's layout.
    unsafe { &mut (*(receiver as *mut Object<T>)).body }
}

/// Read a live body's fields without claiming exclusive access.
///
/// # Safety
/// `receiver` has this layout; concurrent mutation must use interior mutability.
pub(crate) unsafe fn body_ref<'a, T>(receiver: Id) -> &'a T {
    unsafe { &(*(receiver as *const Object<T>)).body }
}

/// Whether `object` is an instance of `name` or one of its subclasses.
pub(crate) fn is_kind(object: Id, name: &CStr) -> bool {
    let Some(class) = objc::class::lookup(name) else {
        return false;
    };
    // SAFETY: null, or an object this runtime allocated.
    unsafe { objc::object::class_of(object) }.is_some_and(|actual| {
        actual
            .chain()
            .any(|candidate| std::ptr::eq(candidate, class))
    })
}

macro_rules! methods {
    ($class:expr, { $( $sel:literal => $imp:expr, $types:literal );* $(;)? }) => {{
        let class: &'static objc::class::Class = $class;
        $(
            class.replace_method(
                objc::sel::get($sel),
                $imp as *const std::ffi::c_void,
                $types,
            );
        )*
    }};
}

pub(crate) use methods;
