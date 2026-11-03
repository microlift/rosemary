//! Properties kept as associations of the object, for classes whose state is
//! a handful of values with no behaviour of their own.
//!
//! Each property is keyed by its name: a getter `isFoo` or `foo` and a setter
//! `setFoo:` find the same entry. The associations go when the object does.

use std::ffi::c_void;

use objc::object::Id;
use objc::sel::Sel;

/// The key a getter or setter selector names.
fn key(sel: Sel) -> *const c_void {
    // SAFETY: a registered selector is a NUL-terminated interned name.
    let name = unsafe { std::ffi::CStr::from_ptr(objc::sel::name(sel)) }.to_string_lossy();
    let bare = if let Some(rest) = name.strip_prefix("set").and_then(|n| n.strip_suffix(':')) {
        rest
    } else if let Some(rest) = name
        .strip_prefix("is")
        .filter(|rest| rest.starts_with(|c: char| c.is_ascii_uppercase()))
    {
        rest
    } else {
        &name
    };
    let mut chars = bare.chars();
    let normal: String = chars
        .next()
        .map(|c| c.to_ascii_lowercase())
        .into_iter()
        .chain(chars)
        .collect();
    objc::sel::get(&normal) as *const c_void
}

pub unsafe extern "C" fn get_object(receiver: Id, sel: Sel) -> Id {
    objc::arc::associated(receiver, key(sel))
}

/// A strong property.
pub unsafe extern "C" fn set_retained(receiver: Id, sel: Sel, value: Id) {
    objc::arc::set_associated_with_policy(receiver, key(sel), value, 1);
}

/// A `copy` property.
pub unsafe extern "C" fn set_copied(receiver: Id, sel: Sel, value: Id) {
    objc::arc::set_associated_with_policy(receiver, key(sel), value, 3);
}

/// A weak or `assign` property: the owner's business to keep it alive.
pub unsafe extern "C" fn set_assigned(receiver: Id, sel: Sel, value: Id) {
    objc::arc::set_associated_with_policy(receiver, key(sel), value, 0);
}

fn word(receiver: Id, sel: Sel) -> usize {
    objc::arc::associated(receiver, key(sel)) as usize
}

pub unsafe extern "C" fn get_integer(receiver: Id, sel: Sel) -> usize {
    word(receiver, sel)
}

pub unsafe extern "C" fn set_integer(receiver: Id, sel: Sel, value: usize) {
    objc::arc::set_associated_word(receiver, key(sel), value);
}

pub unsafe extern "C" fn get_bool(receiver: Id, sel: Sel) -> u8 {
    (word(receiver, sel) != 0) as u8
}

pub unsafe extern "C" fn set_bool(receiver: Id, sel: Sel, value: u8) {
    objc::arc::set_associated_word(receiver, key(sel), (value != 0) as usize);
}

pub unsafe extern "C" fn get_double(receiver: Id, sel: Sel) -> f64 {
    f64::from_bits(word(receiver, sel) as u64)
}

pub unsafe extern "C" fn set_double(receiver: Id, sel: Sel, value: f64) {
    objc::arc::set_associated_word(receiver, key(sel), value.to_bits() as usize);
}

/// The value of property `name` of `receiver`, for Rust callers.
pub fn object(receiver: Id, name: &str) -> Id {
    objc::arc::associated(receiver, objc::sel::get(name) as *const c_void)
}

pub fn integer(receiver: Id, name: &str) -> usize {
    objc::arc::associated(receiver, objc::sel::get(name) as *const c_void) as usize
}

pub fn set_integer_of(receiver: Id, name: &str, value: usize) {
    objc::arc::set_associated_word(receiver, objc::sel::get(name) as *const c_void, value);
}

/// Install the accessors of a list of properties.
///
/// `object retained "textField", "setTextField:";` and so on; the kinds are
/// `retained`, `copied`, `assigned`, `integer`, `bool` and `double`.
macro_rules! properties {
    ($class:expr, { $( $kind:ident $getter:literal, $setter:literal );* $(;)? }) => {{
        let class: &'static objc::class::Class = $class;
        $( $crate::props::properties!(@one class, $kind, $getter, $setter); )*
    }};
    (@one $class:ident, retained, $getter:literal, $setter:literal) => {
        $crate::define::methods!($class, {
            $getter => $crate::props::get_object, c"@16@0:8";
            $setter => $crate::props::set_retained, c"v24@0:8@16";
        })
    };
    (@one $class:ident, copied, $getter:literal, $setter:literal) => {
        $crate::define::methods!($class, {
            $getter => $crate::props::get_object, c"@16@0:8";
            $setter => $crate::props::set_copied, c"v24@0:8@16";
        })
    };
    (@one $class:ident, assigned, $getter:literal, $setter:literal) => {
        $crate::define::methods!($class, {
            $getter => $crate::props::get_object, c"@16@0:8";
            $setter => $crate::props::set_assigned, c"v24@0:8@16";
        })
    };
    (@one $class:ident, integer, $getter:literal, $setter:literal) => {
        $crate::define::methods!($class, {
            $getter => $crate::props::get_integer, c"q16@0:8";
            $setter => $crate::props::set_integer, c"v24@0:8q16";
        })
    };
    (@one $class:ident, bool, $getter:literal, $setter:literal) => {
        $crate::define::methods!($class, {
            $getter => $crate::props::get_bool, c"c16@0:8";
            $setter => $crate::props::set_bool, c"v20@0:8c16";
        })
    };
    (@one $class:ident, double, $getter:literal, $setter:literal) => {
        $crate::define::methods!($class, {
            $getter => $crate::props::get_double, c"d16@0:8";
            $setter => $crate::props::set_double, c"v24@0:8d16";
        })
    };
}

pub(crate) use properties;
