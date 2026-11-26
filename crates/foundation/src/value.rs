use std::ffi::{CStr, c_char, c_void};

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
struct PointerValue {
    isa: *const c_void,
    pointer: *const c_void,
    /// `^v` or `@`: the two are different values even at the same address.
    encoding: &'static CStr,
}

const POINTER: &CStr = c"^v";
const NONRETAINED_OBJECT: &CStr = c"@";

fn pointer_value_class() -> &'static objc::class::Class {
    let base = objc::class::lookup(c"NSValue").expect("Foundation registered NSValue");
    corefoundation::base::register(
        c"NSConcretePointerValue",
        Some(base),
        std::mem::size_of::<PointerValue>(),
        false,
    )
}

fn new_pointer_value(pointer: *const c_void, encoding: &'static CStr) -> Id {
    // SAFETY: the body begins with `isa` and matches the registered size.
    let value = unsafe { corefoundation::base::allocate::<PointerValue>(pointer_value_class()) };
    // SAFETY: a fresh instance whose payload is ours to initialize.
    unsafe {
        (*value).pointer = pointer;
        (*value).encoding = encoding;
    }
    objc::arc::autorelease(value as Id)
}

unsafe extern "C" fn value_with_pointer(_class: Id, _sel: Sel, pointer: *const c_void) -> Id {
    new_pointer_value(pointer, POINTER)
}

/// `+valueWithNonretainedObject:`, which holds the object without a reference:
/// a key that names an object without keeping it alive.
unsafe extern "C" fn value_with_nonretained_object(_class: Id, _sel: Sel, object: Id) -> Id {
    new_pointer_value(object as *const c_void, NONRETAINED_OBJECT)
}

/// # Safety
///
/// `receiver` is an `NSConcretePointerValue`.
unsafe fn pointer_of(receiver: Id) -> &'static PointerValue {
    unsafe { &*(receiver as *const PointerValue) }
}

unsafe extern "C" fn pointer_value(receiver: Id, _sel: Sel) -> *const c_void {
    // SAFETY: installed only on `NSConcretePointerValue`.
    unsafe { pointer_of(receiver) }.pointer
}

unsafe extern "C" fn pointer_obj_c_type(receiver: Id, _sel: Sel) -> *const c_char {
    // SAFETY: installed only on `NSConcretePointerValue`.
    unsafe { pointer_of(receiver) }.encoding.as_ptr()
}

unsafe extern "C" fn pointer_is_equal(receiver: Id, _sel: Sel, other: Id) -> bool {
    if receiver == other {
        return true;
    }
    // SAFETY: `other` is nil or a live object.
    if other.is_null()
        || !unsafe { corefoundation::base::is_kind_of(other as _, pointer_value_class()) }
    {
        return false;
    }
    // SAFETY: both are pointer values.
    let (a, b) = unsafe { (pointer_of(receiver), pointer_of(other)) };
    a.pointer == b.pointer && a.encoding == b.encoding
}

unsafe extern "C" fn pointer_hash(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: installed only on `NSConcretePointerValue`.
    unsafe { pointer_of(receiver) }.pointer as usize
}

/// Immutable, so a copy is the same object.
unsafe extern "C" fn pointer_copy_with_zone(receiver: Id, _sel: Sel, _zone: *const c_void) -> Id {
    objc::arc::retain(receiver)
}

unsafe extern "C" fn pointer_dealloc(receiver: Id, _sel: Sel) {
    // `pointer` is borrowed. In particular, do not inherit `NSValue`'s
    // destructor, which interprets its second word as an owned `Boxed`.
    objc::arc::finalize(receiver);
}

/// The one `NSNull`, which `+null` must return every time.
static NULL: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

unsafe extern "C" fn null(class: Id, _sel: Sel) -> Id {
    *NULL.get_or_init(|| {
        // SAFETY: `class` is `NSNull`, which this crate registered.
        let object = unsafe {
            objc::object::create_instance(
                objc::class::from_ptr(class as *const objc::class::Class).expect("a class"),
                0,
            )
        };
        // Retained forever: a singleton that could be deallocated is one a guest
        // can make dangle by releasing what it never owned.
        objc::arc::make_immortal(object);
        object as usize
    }) as Id
}

/// The `NSNull` singleton, for this crate's own converters.
///
/// Sent rather than called, so a guest that replaced `+null` is the one
/// answering - `NSNull` is a class a guest may categorise.
pub fn null_object() -> Id {
    let class = objc::class::lookup(c"NSNull").expect("Foundation registered NSNull");
    // SAFETY: `+null` is `id (Class, SEL)`, installed below.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(objc::class::ptr(class) as Id, objc::sel::get("null"))
    }
}

pub(crate) fn install() {
    let value_class = objc::class::lookup(c"NSValue").expect("geometry registered NSValue");
    class_methods!(value_class, {
        "valueWithPointer:" => value_with_pointer, c"@24@0:8^v16";
        "valueWithNonretainedObject:" => value_with_nonretained_object, c"@24@0:8@16";
    });
    methods!(pointer_value_class(), {
        "pointerValue" => pointer_value, c"^v16@0:8";
        "nonretainedObjectValue" => pointer_value, c"@16@0:8";
        "objCType" => pointer_obj_c_type, c"r*16@0:8";
        "isEqual:" => pointer_is_equal, c"B24@0:8@16";
        "isEqualToValue:" => pointer_is_equal, c"B24@0:8@16";
        "hash" => pointer_hash, c"Q16@0:8";
        "copyWithZone:" => pointer_copy_with_zone, c"@24@0:8^v16";
        "dealloc" => pointer_dealloc, c"v16@0:8";
    });

    // A CoreFoundation type, so `CFGetTypeID(kCFNull)` is `CFNullGetTypeID()`.
    let null_class =
        corefoundation::base::register(c"NSNull", None, std::mem::size_of::<usize>(), true);
    class_methods!(null_class, {
        "null" => null, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pointer_value_does_not_free_the_borrowed_pointer() {
        crate::bootstrap();
        let pool = objc::arc::pool_push();
        let pointer = 0x10usize as *const c_void;
        // SAFETY: the class method accepts an opaque pointer and returns a live
        // autoreleased value.
        let value = unsafe {
            value_with_pointer(
                std::ptr::null_mut(),
                objc::sel::get("valueWithPointer:"),
                pointer,
            )
        };
        // SAFETY: `value` is the pointer value made immediately above.
        assert_eq!(
            unsafe { pointer_value(value, objc::sel::get("pointerValue")) },
            pointer
        );
        // This reaches `-dealloc`; treating `pointer` as an owned box would
        // attempt to free address 0x10.
        objc::arc::pool_pop(pool);
    }
}
