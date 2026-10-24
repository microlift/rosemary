//! `NSValueTransformer`: a registry of named one-value conversions, and the
//! ones Foundation predefines.

use std::collections::BTreeMap;
use std::sync::Mutex;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// The registered transformers by name, each retained.
static REGISTRY: Mutex<BTreeMap<String, usize>> = Mutex::new(BTreeMap::new());

/// The predefined transformers' names, and the classes that implement them.
const PREDEFINED: &[(&str, &std::ffi::CStr)] = &[
    ("NSNegateBoolean", c"_NSNegateBooleanTransformer"),
    ("NSIsNil", c"_NSIsNilTransformer"),
    ("NSIsNotNil", c"_NSIsNotNilTransformer"),
];

fn text(string: Id) -> Option<String> {
    if string.is_null() {
        return None;
    }
    // SAFETY: an NSString, by the prototypes that take one.
    String::from_utf8(unsafe { crate::string::bytes(string) }).ok()
}

fn send(receiver: Id, sel: &str) -> Id {
    // SAFETY: `id (id, SEL)`, which each selector this is used with is.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel))
    }
}

fn instantiate(class: &'static objc::class::Class) -> Id {
    let object = send(objc::class::ptr(class) as Id, "alloc");
    send(object, "init")
}

/// `+setValueTransformer:forName:`. A nil transformer removes the name.
unsafe extern "C" fn set_value_transformer(_class: Id, _sel: Sel, transformer: Id, name: Id) {
    let Some(name) = text(name) else {
        return;
    };
    let old = {
        let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        match transformer.is_null() {
            true => registry.remove(&name),
            false => registry.insert(name, objc::arc::retain(transformer) as usize),
        }
    };
    if let Some(old) = old {
        objc::arc::release(old as Id);
    }
}

/// `+valueTransformerForName:`: a registered one; a predefined one; or, as
/// Apple does, an instance of the `NSValueTransformer` subclass the name
/// names, made and registered on first use.
unsafe extern "C" fn value_transformer_for_name(_class: Id, _sel: Sel, name: Id) -> Id {
    let Some(name) = text(name) else {
        return std::ptr::null_mut();
    };
    if let Some(found) = REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&name)
    {
        return *found as Id;
    }
    let class_name = PREDEFINED
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, class)| (*class).to_owned())
        .or_else(|| std::ffi::CString::new(name.as_str()).ok());
    let base = objc::class::lookup(c"NSValueTransformer").expect("installed below");
    let Some(class) = class_name
        .and_then(|n| objc::class::lookup(&n))
        .filter(|c| c.chain().any(|k| std::ptr::eq(k, base)) && !std::ptr::eq(*c, base))
    else {
        return std::ptr::null_mut();
    };
    let made = instantiate(class);
    if made.is_null() {
        return made;
    }
    let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    // Another thread may have made one first; keep that one.
    let kept = *registry.entry(name).or_insert(made as usize) as Id;
    if kept != made {
        objc::arc::release(made);
    }
    kept
}

/// `+valueTransformerNames`.
unsafe extern "C" fn value_transformer_names(_class: Id, _sel: Sel) -> Id {
    let names: Vec<Id> = REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .keys()
        .map(|name| crate::string::from_str(name))
        .collect();
    let refs: Vec<corefoundation::base::CFTypeRef> = names.iter().map(|n| *n as _).collect();
    let array = corefoundation::collections::new_array(
        &refs,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for name in names {
        objc::arc::release(name);
    }
    objc::arc::autorelease(array as Id)
}

/// `+transformedValueClass`: any object.
unsafe extern "C" fn transformed_value_class(_class: Id, _sel: Sel) -> Id {
    objc::class::ptr(objc::class::lookup(c"NSObject").expect("NSObject")) as Id
}

unsafe extern "C" fn allows_reverse(_class: Id, _sel: Sel) -> u8 {
    1
}

unsafe extern "C" fn disallows_reverse(_class: Id, _sel: Sel) -> u8 {
    0
}

/// `-transformedValue:`, which the base class answers with the value itself.
unsafe extern "C" fn identity(_receiver: Id, _sel: Sel, value: Id) -> Id {
    value
}

/// `-reverseTransformedValue:`: the forward transformation, when the class
/// allows going back, as Apple's default does.
unsafe extern "C" fn reverse_transformed_value(receiver: Id, _sel: Sel, value: Id) -> Id {
    // SAFETY: a live object.
    let Some(class) = (unsafe { objc::object::class_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `+allowsReverseTransformation` is `BOOL (id, SEL)`.
    let allowed = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
        f(
            objc::class::ptr(class) as Id,
            objc::sel::get("allowsReverseTransformation"),
        )
    };
    if allowed == 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: `-transformedValue:` is `id (id, SEL, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("transformedValue:"), value)
    }
}

fn boolean(value: bool) -> Id {
    corefoundation::collections::boolean(value) as Id
}

fn truth(value: Id) -> bool {
    if value.is_null() {
        return false;
    }
    // SAFETY: `-boolValue`, which NSNumber and NSString answer.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
        f(value, objc::sel::get("boolValue")) != 0
    }
}

unsafe extern "C" fn negate(_receiver: Id, _sel: Sel, value: Id) -> Id {
    boolean(!truth(value))
}

unsafe extern "C" fn is_nil(_receiver: Id, _sel: Sel, value: Id) -> Id {
    boolean(value.is_null())
}

unsafe extern "C" fn is_not_nil(_receiver: Id, _sel: Sel, value: Id) -> Id {
    boolean(!value.is_null())
}

pub(crate) fn install() {
    let class = corefoundation::base::abstract_class(c"NSValueTransformer", None);
    class_methods!(class, {
        "setValueTransformer:forName:" => set_value_transformer, c"v32@0:8@16@24";
        "valueTransformerForName:" => value_transformer_for_name, c"@24@0:8@16";
        "valueTransformerNames" => value_transformer_names, c"@16@0:8";
        "transformedValueClass" => transformed_value_class, c"#16@0:8";
        "allowsReverseTransformation" => allows_reverse, c"c16@0:8";
    });
    methods!(class, {
        "transformedValue:" => identity, c"@24@0:8@16";
        "reverseTransformedValue:" => reverse_transformed_value, c"@24@0:8@16";
    });
    for (_, name) in PREDEFINED {
        let predefined = corefoundation::base::abstract_class(name, Some(class));
        class_methods!(predefined, {
            "allowsReverseTransformation" => allows_reverse, c"c16@0:8";
        });
    }
    let lookup = |name: &std::ffi::CStr| objc::class::lookup(name).expect("made above");
    methods!(lookup(c"_NSNegateBooleanTransformer"), {
        "transformedValue:" => negate, c"@24@0:8@16";
        "reverseTransformedValue:" => negate, c"@24@0:8@16";
    });
    for (name, imp) in [
        (
            c"_NSIsNilTransformer",
            is_nil as unsafe extern "C" fn(Id, Sel, Id) -> Id,
        ),
        (c"_NSIsNotNilTransformer", is_not_nil),
    ] {
        let class = lookup(name);
        methods!(class, {
            "transformedValue:" => imp, c"@24@0:8@16";
        });
        class_methods!(class, {
            "allowsReverseTransformation" => disallows_reverse, c"c16@0:8";
        });
    }

    // `NSURLProtocol`: the class a program subclasses to answer its own URL
    // schemes. Registration is recorded for `+registeredClasses`.
    //
    // FIXME (ACCURACY): no URL loading here consults a registered protocol.
    let protocol = corefoundation::base::abstract_class(c"NSURLProtocol", None);
    class_methods!(protocol, {
        "registerClass:" => register_protocol, c"c24@0:8#16";
        "unregisterClass:" => unregister_protocol, c"v24@0:8#16";
        "canInitWithRequest:" => disallows_request, c"c24@0:8@16";
        "canonicalRequestForRequest:" => identity, c"@24@0:8@16";
    });

    // `NSScriptCommand`: AppleScript's commands, which a program subclasses
    // to handle its own. Nothing here sends Apple events to a guest, so none
    // is ever made by this runtime; the class exists so subclasses load.
    corefoundation::base::abstract_class(c"NSScriptCommand", None);
}

/// Protocol classes registered with `+registerClass:`, most recent first.
static PROTOCOLS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

unsafe extern "C" fn register_protocol(_class: Id, _sel: Sel, protocol: Id) -> u8 {
    if protocol.is_null() {
        return 0;
    }
    PROTOCOLS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(0, protocol as usize);
    1
}

unsafe extern "C" fn unregister_protocol(_class: Id, _sel: Sel, protocol: Id) {
    PROTOCOLS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|p| *p != protocol as usize);
}

unsafe extern "C" fn disallows_request(_class: Id, _sel: Sel, _request: Id) -> u8 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class_object(name: &std::ffi::CStr) -> Id {
        objc::class::ptr(objc::class::lookup(name).unwrap()) as Id
    }

    #[test]
    fn negate_boolean_is_predefined_and_reverses() {
        crate::bootstrap();
        let name = crate::string::from_str("NSNegateBoolean");
        // SAFETY: direct calls with the installed prototypes.
        unsafe {
            let transformer = value_transformer_for_name(
                class_object(c"NSValueTransformer"),
                std::ptr::null_mut(),
                name,
            );
            assert!(!transformer.is_null());
            let yes = boolean(true);
            let f: unsafe extern "C" fn(Id, Sel, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            assert!(!truth(f(
                transformer,
                objc::sel::get("transformedValue:"),
                yes
            )));
            assert!(!truth(f(
                transformer,
                objc::sel::get("reverseTransformedValue:"),
                yes
            )));
        }
        objc::arc::release(name);
    }

    #[test]
    fn an_unknown_name_has_no_transformer() {
        crate::bootstrap();
        let name = crate::string::from_str("NoSuchTransformer");
        // SAFETY: as above.
        let found = unsafe {
            value_transformer_for_name(
                class_object(c"NSValueTransformer"),
                std::ptr::null_mut(),
                name,
            )
        };
        assert!(found.is_null());
        objc::arc::release(name);
    }
}
