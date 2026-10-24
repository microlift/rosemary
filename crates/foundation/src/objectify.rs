use corefoundation::base::CFTypeRef;
use objc::object::Id;

use crate::plist::Value;

/// `value` as an object, autoreleased.
///
/// `what` names the caller for a diagnostic - a plist is read from disk on a
/// guest's say-so, so a type this cannot represent has to say which file and
/// which key it came from rather than only that one existed.
pub fn object(value: &Value, what: &str) -> Id {
    let object = retained(value, what);
    // SAFETY: a fresh object this function owns a reference to.
    unsafe { corefoundation::api::CFAutorelease(object) as Id }
}

/// As [`object`], keeping the reference for a caller that is about to put it in
/// a collection.
fn retained(value: &Value, what: &str) -> CFTypeRef {
    match value {
        Value::String(text) => corefoundation::string::from_bytes(text.as_bytes(), false),
        Value::Integer(v) => corefoundation::collections::new_number(
            corefoundation::collections::Number::Integer(*v),
        ),
        // A boolean is an `NSNumber` here, as it is on Darwin: `kCFBooleanTrue`
        // is a distinct object there, and this runtime has no such singleton, so
        // `-boolValue` is what tells the two apart either way.
        Value::Bool(b) => corefoundation::collections::new_number(
            corefoundation::collections::Number::Integer(*b as i64),
        ),
        Value::Real(v) => {
            corefoundation::collections::new_number(corefoundation::collections::Number::Float(*v))
        }
        Value::Data(bytes) => corefoundation::collections::new_data(bytes.clone()),
        Value::Null => {
            let class = objc::class::lookup(c"NSNull").expect("Foundation registered NSNull");
            let sel = objc::sel::get("null");
            // SAFETY: `+null` is `id (Class, SEL)`, installed by `crate::value`,
            // and hands back a singleton this runtime keeps forever.
            let null = unsafe {
                let f: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
                    std::mem::transmute(objc::msg::entry());
                f(objc::class::ptr(class) as Id, sel)
            };
            objc::arc::retain(null) as CFTypeRef
        }
        Value::Array(values) => {
            let elements: Vec<CFTypeRef> = values.iter().map(|v| retained(v, what)).collect();
            let array = corefoundation::collections::new_array(
                &elements,
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            );
            release_each(&elements);
            array
        }
        Value::Dictionary(entries) => {
            let pairs: Vec<(CFTypeRef, CFTypeRef)> = entries
                .iter()
                .map(|(key, value)| {
                    (
                        corefoundation::string::from_bytes(key.as_bytes(), false),
                        retained(value, what),
                    )
                })
                .collect();
            let dictionary = corefoundation::collections::new_dictionary(
                &pairs,
                corefoundation::callbacks::Policy::from_key_callbacks(),
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            );
            for (key, value) in &pairs {
                release_each(&[*key, *value]);
            }
            dictionary
        }
        Value::Date(interval) => {
            let class = objc::class::lookup(c"NSDate").expect("Foundation registered NSDate");
            // SAFETY: `+dateWithTimeIntervalSinceReferenceDate:` is
            // `id (Class, SEL, NSTimeInterval)`, installed by `crate::timer`.
            let date = unsafe {
                let f: unsafe extern "C" fn(Id, objc::sel::Sel, f64) -> Id =
                    std::mem::transmute(objc::msg::entry());
                f(
                    objc::class::ptr(class) as Id,
                    objc::sel::get("dateWithTimeIntervalSinceReferenceDate:"),
                    *interval,
                )
            };
            objc::arc::retain(date) as CFTypeRef
        }
        Value::Uid(_) => objc::fail!(
            "{what}: this property list holds a CF$UID, which is an archive's reference and not a \
             value - see foundation::unarchiver"
        ),
        Value::Unsupported(marker) => objc::fail!(
            "{what}: this property list holds a value with marker {marker:#x}, which \
             foundation::plist does not decode"
        ),
    }
}

/// Drop this function's own reference to each element, now that the collection
/// holding them has taken one.
fn release_each(objects: &[CFTypeRef]) {
    for object in objects {
        // SAFETY: each is an object this module made and owns a reference to.
        unsafe { corefoundation::api::CFRelease(*object) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A nested plist converts whole: the dictionary answers for its keys, and
    /// the array inside it for its elements.
    #[test]
    fn a_nested_plist_becomes_a_dictionary_of_objects() {
        crate::bootstrap();
        let mut map = BTreeMap::new();
        map.insert("name".to_string(), Value::String("DELTARUNE".into()));
        map.insert("count".to_string(), Value::Integer(3));
        map.insert(
            "list".to_string(),
            Value::Array(vec![Value::String("a".into()), Value::Real(2.5)]),
        );
        let dictionary = object(&Value::Dictionary(map), "a test");
        assert!(!dictionary.is_null());

        // Read back the way a guest reads one.
        // SAFETY: `-objectForKey:` is `id (id, SEL, id)`.
        let value_for = |key: &str| unsafe {
            let f: unsafe extern "C" fn(Id, objc::sel::Sel, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                dictionary,
                objc::sel::get("objectForKey:"),
                crate::string::from_str(key),
            )
        };
        // SAFETY: the value is a string.
        assert_eq!(
            unsafe { crate::string::bytes(value_for("name")) },
            b"DELTARUNE"
        );
        // SAFETY: the value is a number.
        assert_eq!(unsafe { crate::number_of(value_for("count")) }, Some(3));

        let list = value_for("list");
        // SAFETY: an array this conversion made.
        let array =
            unsafe { corefoundation::collections::array(list as CFTypeRef) }.expect("an array");
        assert_eq!(array.values.len(), 2);
    }

    /// The elements outlive the call: the collection holds the only reference,
    /// and this module's own is dropped rather than leaked.
    #[test]
    fn the_collection_owns_what_it_holds() {
        crate::bootstrap();
        let list = Value::Array(vec![Value::String("kept".into())]);
        let array = object(&list, "a test");
        // SAFETY: an array this conversion made.
        let contents =
            unsafe { corefoundation::collections::array(array as CFTypeRef) }.expect("an array");
        let element = contents.values[0] as Id;
        assert_eq!(
            objc::arc::retain_count(element),
            1,
            "the array holds one reference"
        );
        // SAFETY: the element is a live string the array still holds.
        assert_eq!(unsafe { crate::string::bytes(element) }, b"kept");
    }
}
