//! `NSValueTransformer`, after Apple's `NSValueTransformer.h` and its
//! documentation: an abstract class a program subclasses, and a registry of
//! named instances.
//!
//! The base class transforms nothing - `-transformedValue:` answers its
//! argument - and allows the reverse, which is `-transformedValue:` again.
//! `+valueTransformerForName:` answers a registered instance, or else makes
//! and registers one of the class of that name, if it is a transformer.
//!
//! Of the named transformers Foundation registers itself, the three over
//! values are here: `NSNegateBoolean`, `NSIsNil` and `NSIsNotNil`. The three
//! that unarchive data are not, and asking for one by name answers nil.

use std::sync::Mutex;

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// Each name, and the address of the transformer registered under it,
/// retained - in the order they were registered, which is the order
/// `+valueTransformerNames` lists them in.
static REGISTRY: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());

fn registry() -> std::sync::MutexGuard<'static, Vec<(String, usize)>> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner())
}

/// The string names Foundation exports for its own transformers.
pub const KEYS: &[(&str, &str)] = &[
    ("_NSNegateBooleanTransformerName", "NSNegateBoolean"),
    ("_NSIsNilTransformerName", "NSIsNil"),
    ("_NSIsNotNilTransformerName", "NSIsNotNil"),
    ("_NSUnarchiveFromDataTransformerName", "NSUnarchiveFromData"),
    (
        "_NSKeyedUnarchiveFromDataTransformerName",
        "NSKeyedUnarchiveFromData",
    ),
    (
        "_NSSecureUnarchiveFromDataTransformerName",
        "NSSecureUnarchiveFromData",
    ),
];

fn base_class() -> &'static Class {
    corefoundation::base::abstract_class(c"NSValueTransformer", None)
}

fn send_id(receiver: Id, sel: &str) -> Id {
    // SAFETY: `objc_msgSend`, as `id (id, SEL)`.
    unsafe {
        let send: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        send(receiver, objc::sel::get(sel))
    }
}

fn send_bool(receiver: Id, sel: &str) -> bool {
    // SAFETY: `objc_msgSend`, as `BOOL (id, SEL)`.
    unsafe {
        let send: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
        send(receiver, objc::sel::get(sel)) != 0
    }
}

fn text(name: Id) -> Option<String> {
    // SAFETY: an `NSString`, or nil.
    let bytes = unsafe { crate::string::bytes(name) };
    String::from_utf8(bytes).ok().filter(|s| !s.is_empty())
}

/// `+setValueTransformer:forName:`. A nil transformer removes the name.
unsafe extern "C" fn set_value_transformer(_class: Id, _sel: Sel, transformer: Id, name: Id) {
    let Some(name) = text(name) else {
        return;
    };
    let previous = {
        let mut registry = registry();
        let found = registry.iter().position(|(n, _)| *n == name);
        match (found, transformer.is_null()) {
            (Some(i), true) => Some(registry.remove(i).1),
            (Some(i), false) => Some(std::mem::replace(
                &mut registry[i].1,
                objc::arc::retain(transformer) as usize,
            )),
            (None, true) => None,
            (None, false) => {
                registry.push((name, objc::arc::retain(transformer) as usize));
                None
            }
        }
    };
    if let Some(previous) = previous {
        objc::arc::release(previous as Id);
    }
}

/// `+valueTransformerForName:`.
unsafe extern "C" fn value_transformer_for_name(_class: Id, _sel: Sel, name: Id) -> Id {
    let Some(name) = text(name) else {
        return std::ptr::null_mut();
    };
    if let Some((_, found)) = registry().iter().find(|(n, _)| *n == name) {
        return *found as Id;
    }
    // Not registered: a transformer class of that name is made, registered
    // under it and answered, as the documentation describes.
    let Some(class) = std::ffi::CString::new(name.as_str())
        .ok()
        .and_then(|n| objc::class::lookup(&n))
        .filter(|c| c.chain().any(|x| std::ptr::eq(x, base_class())))
    else {
        return std::ptr::null_mut();
    };
    let made = send_id(send_id(objc::class::ptr(class) as Id, "alloc"), "init");
    if made.is_null() {
        return made;
    }
    let name_object = crate::string::from_str(&name);
    // SAFETY: a transformer and a string this call made.
    unsafe { set_value_transformer(std::ptr::null_mut(), _sel, made, name_object) };
    objc::arc::release(name_object);
    objc::arc::release(made);
    registry()
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(std::ptr::null_mut(), |(_, t)| *t as Id)
}

/// `+valueTransformerNames`: every registered name.
unsafe extern "C" fn value_transformer_names(_class: Id, _sel: Sel) -> Id {
    let names: Vec<Id> = registry()
        .iter()
        .map(|(n, _)| crate::string::from_str(n))
        .collect();
    let as_cf: Vec<_> = names
        .iter()
        .map(|&n| n as corefoundation::base::CFTypeRef)
        .collect();
    let array = corefoundation::collections::new_array(
        &as_cf,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for name in names {
        objc::arc::release(name);
    }
    // SAFETY: an array this call made.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

/// `+transformedValueClass`: `NSObject`, for the base class.
unsafe extern "C" fn transformed_value_class(_class: Id, _sel: Sel) -> Id {
    objc::class::lookup(c"NSObject").map_or(std::ptr::null_mut(), |c| objc::class::ptr(c) as Id)
}

unsafe extern "C" fn yes(_class: Id, _sel: Sel) -> u8 {
    1
}

unsafe extern "C" fn no(_class: Id, _sel: Sel) -> u8 {
    0
}

/// `-transformedValue:`: the value itself, for the base class.
unsafe extern "C" fn transformed_value(_receiver: Id, _sel: Sel, value: Id) -> Id {
    value
}

/// `-reverseTransformedValue:`: `-transformedValue:`, if the class allows a
/// reverse at all; the documentation's exception if not.
unsafe extern "C" fn reverse_transformed_value(receiver: Id, _sel: Sel, value: Id) -> Id {
    // SAFETY: an object, whose class this asks.
    let class = unsafe { objc::object::class_of(receiver) }
        .map_or(std::ptr::null_mut(), |c| objc::class::ptr(c) as Id);
    if !send_bool(class, "allowsReverseTransformation") {
        objc::fail!(
            "NSInvalidArgumentException: -reverseTransformedValue: sent to a transformer whose \
             class does not allow reverse transformation"
        );
    }
    // SAFETY: `objc_msgSend`, as `id (id, SEL, id)`.
    unsafe {
        let send: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
        send(receiver, objc::sel::get("transformedValue:"), value)
    }
}

/// `-[NSNegateBoolean transformedValue:]`: the opposite of `-boolValue`.
unsafe extern "C" fn negate_boolean(_receiver: Id, _sel: Sel, value: Id) -> Id {
    let truth = !value.is_null() && send_bool(value, "boolValue");
    corefoundation::collections::boolean(!truth) as Id
}

/// `-[NSIsNil transformedValue:]`
unsafe extern "C" fn is_nil(_receiver: Id, _sel: Sel, value: Id) -> Id {
    corefoundation::collections::boolean(value.is_null()) as Id
}

/// `-[NSIsNotNil transformedValue:]`
unsafe extern "C" fn is_not_nil(_receiver: Id, _sel: Sel, value: Id) -> Id {
    corefoundation::collections::boolean(!value.is_null()) as Id
}

/// One of Foundation's own transformers: a private subclass whose
/// `-transformedValue:` is `transform`, registered under `name`.
fn builtin(
    class_name: &std::ffi::CStr,
    name: &str,
    transform: unsafe extern "C" fn(Id, Sel, Id) -> Id,
    reversible: bool,
) {
    let class = corefoundation::base::abstract_class(class_name, Some(base_class()));
    methods!(class, {
        "transformedValue:" => transform, c"@24@0:8@16";
    });
    let allows: unsafe extern "C" fn(Id, Sel) -> u8 = if reversible { yes } else { no };
    class_methods!(class, {
        "allowsReverseTransformation" => allows, c"B16@0:8";
    });
    let instance = send_id(send_id(objc::class::ptr(class) as Id, "alloc"), "init");
    let name_object = crate::string::from_str(name);
    // SAFETY: a transformer and a string this call made.
    unsafe {
        set_value_transformer(
            std::ptr::null_mut(),
            objc::sel::get("setValueTransformer:forName:"),
            instance,
            name_object,
        )
    };
    objc::arc::release(name_object);
    objc::arc::release(instance);
}

pub(crate) fn install() {
    let class = base_class();
    class_methods!(class, {
        "setValueTransformer:forName:" => set_value_transformer, c"v32@0:8@16@24";
        "valueTransformerForName:" => value_transformer_for_name, c"@24@0:8@16";
        "valueTransformerNames" => value_transformer_names, c"@16@0:8";
        "transformedValueClass" => transformed_value_class, c"#16@0:8";
        "allowsReverseTransformation" => yes, c"B16@0:8";
    });
    methods!(class, {
        "transformedValue:" => transformed_value, c"@24@0:8@16";
        "reverseTransformedValue:" => reverse_transformed_value, c"@24@0:8@16";
    });
    // The negation is its own inverse; whether something was nil cannot be
    // undone.
    builtin(
        c"_NSNegateBooleanTransformer",
        "NSNegateBoolean",
        negate_boolean,
        true,
    );
    builtin(c"_NSIsNilTransformer", "NSIsNil", is_nil, false);
    builtin(c"_NSIsNotNilTransformer", "NSIsNotNil", is_not_nil, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class_id(class: &'static Class) -> Id {
        objc::class::ptr(class) as Id
    }

    #[test]
    fn the_builtin_transformers_are_registered_by_name() {
        crate::bootstrap();
        let base = class_id(base_class());
        let sel = objc::sel::get("valueTransformerForName:");
        let name = crate::string::from_str("NSNegateBoolean");
        // SAFETY: the class methods' own prototypes.
        unsafe {
            let negate = value_transformer_for_name(base, sel, name);
            assert!(!negate.is_null());
            let yes = corefoundation::collections::boolean(true) as Id;
            let no = corefoundation::collections::boolean(false) as Id;
            let send: unsafe extern "C" fn(Id, Sel, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            assert_eq!(send(negate, objc::sel::get("transformedValue:"), yes), no);
            assert_eq!(
                send(negate, objc::sel::get("reverseTransformedValue:"), no),
                yes
            );

            let is_nil = value_transformer_for_name(base, sel, crate::string::from_str("NSIsNil"));
            assert_eq!(
                send(
                    is_nil,
                    objc::sel::get("transformedValue:"),
                    std::ptr::null_mut()
                ),
                yes
            );
            let missing = value_transformer_for_name(
                base,
                sel,
                crate::string::from_str("NSUnarchiveFromData"),
            );
            assert!(missing.is_null());
        }
    }

    /// A subclass a program declares is found by its class name, made once,
    /// and then answered from the registry.
    #[test]
    fn a_transformer_class_is_found_and_registered_by_its_name() {
        crate::bootstrap();
        let base = base_class();
        let subclass = objc::class::allocate_pair(Some(base), c"RosemaryTestTransformer", 0)
            .expect("a fresh name");
        objc::class::register_pair(subclass);
        let sel = objc::sel::get("valueTransformerForName:");
        let name = crate::string::from_str("RosemaryTestTransformer");
        // SAFETY: the class method's own prototype.
        unsafe {
            let first = value_transformer_for_name(class_id(base), sel, name);
            assert!(!first.is_null());
            assert!(objc::object::class_of(first).is_some_and(|c| std::ptr::eq(c, subclass)));
            let second = value_transformer_for_name(class_id(base), sel, name);
            assert_eq!(first, second);
            let names =
                value_transformer_names(class_id(base), objc::sel::get("valueTransformerNames"));
            assert!(!names.is_null());
            // An unrelated class is not a transformer.
            let not_one = value_transformer_for_name(
                class_id(base),
                sel,
                crate::string::from_str("NSObject"),
            );
            assert!(not_one.is_null());
        }
    }
}
