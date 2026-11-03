// The C spellings are the interface, as they are in `api`.
#![allow(non_snake_case)]

use objc::object::Id;

use crate::api::{self, CFAllocatorRef};
use crate::base::{CFIndex, CFTypeRef};
use crate::callbacks::Policy;
use crate::collections::{self, Number};
use crate::plist::Value;

/// `CFPropertyListMutabilityOptions`.
mod mutability {
    pub const MUTABLE_CONTAINERS: u64 = 1;
    pub const MUTABLE_CONTAINERS_AND_LEAVES: u64 = 2;
}

/// `CFPropertyListFormat`, as `*format` reports it.
mod format {
    pub const XML_V1_0: i64 = 100;
    pub const BINARY_V1_0: i64 = 200;
}

/// `NSCocoaErrorDomain`'s `NSPropertyListReadCorruptError`, which is what Apple's
/// reader reports for every malformed document.
const READ_CORRUPT: i64 = 3840;

/// How deep a document may nest, since the input is whatever a guest was handed.
/// A refusal is an ordinary parse failure where a stack overflow is not.
const MAX_DEPTH: usize = 512;

/// `value` as a retained object, or what could not be represented.
///
/// Rejection over approximation: a UID is an archive's reference and not a
/// value, so it is refused rather than turned into a number.
fn build(value: &Value, mutability: u64, depth: usize) -> Result<CFTypeRef, String> {
    if depth > MAX_DEPTH {
        return Err(format!("the document nests deeper than {MAX_DEPTH} levels"));
    }
    let containers = mutability >= mutability::MUTABLE_CONTAINERS;
    Ok(match value {
        Value::String(text) => {
            let string = crate::string::from_bytes(text.as_bytes(), false);
            if mutability == mutability::MUTABLE_CONTAINERS_AND_LEAVES {
                // SAFETY: a string this function just made and owns.
                let copy = unsafe { api::CFStringCreateMutableCopy(std::ptr::null(), 0, string) };
                unsafe { api::CFRelease(string) };
                copy
            } else {
                string
            }
        }
        Value::Integer(v) => collections::new_number(Number::Integer(*v)),
        Value::Real(v) => collections::new_number(Number::Float(*v)),
        Value::Bool(v) => {
            // The boolean singletons are immortal, but a caller releases what
            // it is given, so the reference is taken here.
            let object = collections::boolean(*v);
            // SAFETY: the process-lifetime singleton.
            unsafe { api::CFRetain(object) }
        }
        Value::Data(bytes) => collections::new_data(bytes.clone()),
        Value::Date(interval) => date(*interval)?,
        Value::Null => {
            return Err("the document holds a null, which is not a property list type".into());
        }
        Value::Array(values) => {
            let elements = values
                .iter()
                .map(|v| build(v, mutability, depth + 1))
                .collect::<Result<Vec<_>, _>>();
            let elements = elements?;
            let array =
                collections::new_array(&elements, Policy::from_type_callbacks(), containers);
            release_each(&elements);
            array
        }
        Value::Dictionary(entries) => {
            let mut pairs = Vec::with_capacity(entries.len());
            let mut failure = None;
            for (key, value) in entries {
                match build(value, mutability, depth + 1) {
                    Ok(value) => {
                        let key = crate::string::from_bytes(key.as_bytes(), false);
                        pairs.push((key, value));
                    }
                    Err(why) => {
                        failure = Some(why);
                        break;
                    }
                }
            }
            if let Some(why) = failure {
                for (key, value) in &pairs {
                    release_each(&[*key, *value]);
                }
                return Err(why);
            }
            let dictionary = collections::new_dictionary(
                &pairs,
                Policy::from_key_callbacks(),
                Policy::from_type_callbacks(),
                containers,
            );
            for (key, value) in &pairs {
                release_each(&[*key, *value]);
            }
            dictionary
        }
        Value::Uid(_) => {
            return Err("the document holds a CF$UID, which is an archive's reference".into());
        }
        Value::Unsupported(marker) => {
            return Err(format!(
                "the document holds a value with marker {marker:#x}"
            ));
        }
    })
}

/// `value` as a retained object, for the preferences store.
///
/// Containers are immutable: what a caller reads back from a domain is a
/// snapshot, and a change goes through a set.
pub fn object_from_value(value: &Value) -> Option<CFTypeRef> {
    build(value, 0, 0).ok()
}

/// The property list value of `object`, or `None` when it holds anything a
/// property list cannot: a dictionary with a non-string key, or an object of
/// some other class. Booleans, numbers, strings, data, dates, arrays and
/// dictionaries are the six.
///
/// # Safety
///
/// `object` is null or an object this runtime allocated or bound.
pub unsafe fn value_of(object: CFTypeRef) -> Option<Value> {
    unsafe { value_at(object, 0) }
}

unsafe fn value_at(object: CFTypeRef, depth: usize) -> Option<Value> {
    if object.is_null() || depth > MAX_DEPTH {
        return None;
    }
    // SAFETY: the caller's guarantee, for each check below.
    unsafe {
        if let Some(string) = crate::string::as_string(object) {
            return Some(Value::String(
                String::from_utf8_lossy(&string.utf8()).into_owned(),
            ));
        }
        if std::ptr::eq(object, collections::boolean(true)) {
            return Some(Value::Bool(true));
        }
        if std::ptr::eq(object, collections::boolean(false)) {
            return Some(Value::Bool(false));
        }
        if let Some((number, kind)) = collections::number_value(object) {
            // A `BOOL` stored through `+numberWithBool:` is a char-typed
            // number here; Apple's stores it as a boolean, and so does this.
            if kind == crate::collections::number_type::CHAR
                && crate::base::is_kind_of(object, collections::boolean_class())
            {
                return Some(Value::Bool(number.as_i64() != 0));
            }
            return Some(match number {
                Number::Integer(v) => Value::Integer(v),
                Number::Float(v) => Value::Real(v),
            });
        }
        if let Some(bytes) = collections::data(object) {
            return Some(Value::Data(bytes.clone()));
        }
        if let Some(array) = collections::array(object) {
            let values = array.values.clone();
            return values
                .iter()
                .map(|v| value_at(*v, depth + 1))
                .collect::<Option<Vec<_>>>()
                .map(Value::Array);
        }
        if let Some(dictionary) = collections::dictionary(object) {
            let entries = dictionary.entries.clone();
            let mut out = std::collections::BTreeMap::new();
            for (key, value) in entries {
                let Value::String(key) = value_at(key, depth + 1)? else {
                    return None;
                };
                out.insert(key, value_at(value, depth + 1)?);
            }
            return Some(Value::Dictionary(out));
        }
        if let Some(class) = objc::class::lookup(c"NSDate")
            && crate::base::is_kind_of(object, class)
        {
            let sel = objc::sel::get("timeIntervalSinceReferenceDate");
            // SAFETY: `-timeIntervalSinceReferenceDate` is `double (id, SEL)`.
            let f: unsafe extern "C" fn(Id, objc::sel::Sel) -> f64 =
                std::mem::transmute(objc::msg::entry());
            return Some(Value::Date(f(object as Id, sel)));
        }
    }
    None
}

fn release_each(objects: &[CFTypeRef]) {
    for object in objects {
        // SAFETY: each is an object this module made and owns a reference to.
        unsafe { api::CFRelease(*object) };
    }
}

/// A retained `CFDate`.
///
/// The date class is Foundation's, registered when it loads, so it is reached
/// through the runtime and not linked: CoreFoundation does not depend on it.
fn date(interval: f64) -> Result<CFTypeRef, String> {
    let Some(class) = objc::class::lookup(c"NSDate") else {
        return Err("the document holds a date, and no date class is registered".into());
    };
    let sel = objc::sel::get("dateWithTimeIntervalSinceReferenceDate:");
    // SAFETY: `+dateWithTimeIntervalSinceReferenceDate:` is
    // `id (Class, SEL, NSTimeInterval)`, installed by Foundation.
    let object = unsafe {
        let f: unsafe extern "C" fn(Id, objc::sel::Sel, f64) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(objc::class::ptr(class) as Id, sel, interval)
    };
    // The constructor autoreleases; the caller of this module owns a reference.
    Ok(objc::arc::retain(object) as CFTypeRef)
}

/// A retained `CFError`, or null when no error class is registered.
fn error_object(description: &str) -> CFTypeRef {
    let Some(class) = objc::class::lookup(c"NSError") else {
        return std::ptr::null();
    };
    let domain = crate::string::from_bytes(b"NSCocoaErrorDomain", false);
    let message = crate::string::from_bytes(description.as_bytes(), false);
    let key = crate::string::from_bytes(b"NSDebugDescription", false);
    let info = collections::new_dictionary(
        &[(key, message)],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        false,
    );
    let sel = objc::sel::get("errorWithDomain:code:userInfo:");
    // SAFETY: `+errorWithDomain:code:userInfo:` is `id (Class, SEL, id,
    // NSInteger, id)`, installed by Foundation.
    let object = unsafe {
        let f: unsafe extern "C" fn(Id, objc::sel::Sel, CFTypeRef, i64, CFTypeRef) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            objc::class::ptr(class) as Id,
            sel,
            domain,
            READ_CORRUPT,
            info,
        )
    };
    let object = objc::arc::retain(object) as CFTypeRef;
    release_each(&[domain, key, message, info]);
    object
}

/// `CFPropertyListRef CFPropertyListCreateFromXMLData(CFAllocatorRef, CFDataRef,
/// CFOptionFlags, CFStringRef *errorString)`
///
/// The deprecated spelling, which reports an error as a string rather than an
/// error object; despite the name it reads every format.
///
/// # Safety
///
/// As [`CFPropertyListCreateWithData`].
pub unsafe extern "C" fn CFPropertyListCreateFromXMLData(
    allocator: CFAllocatorRef,
    data: CFTypeRef,
    options: u64,
    error_string: *mut CFTypeRef,
) -> CFTypeRef {
    if !error_string.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { *error_string = std::ptr::null() };
    }
    // SAFETY: the caller's arguments, passed through.
    let value = unsafe {
        CFPropertyListCreateWithData(
            allocator,
            data,
            options,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if value.is_null() && !error_string.is_null() {
        // SAFETY: as above.
        unsafe {
            *error_string = crate::string::from_bytes(b"the data is not a property list", false)
        };
    }
    value
}

/// `CFPropertyListRef CFPropertyListCreateWithData(CFAllocatorRef, CFDataRef,
/// CFOptionFlags options, CFPropertyListFormat *format, CFErrorRef *error)`
///
/// XML and binary lists are read. `*format` reports which, and a document in
/// neither - the OpenStep spelling included - is an error rather than a guess.
/// `kCFPropertyListMutableContainersAndLeaves` makes strings mutable too; data
/// objects are one class here, so they are the same either way.
///
/// # Safety
///
/// `data` is null or an object this runtime allocated; `format` and `error` are
/// null or writable for one value of their type.
pub unsafe extern "C" fn CFPropertyListCreateWithData(
    _allocator: CFAllocatorRef,
    data: CFTypeRef,
    options: u64,
    format: *mut CFIndex,
    error: *mut CFTypeRef,
) -> CFTypeRef {
    if !error.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *error = std::ptr::null() };
    }
    let fail = |why: &str| {
        if !error.is_null() {
            // SAFETY: as above.
            unsafe { *error = error_object(why) };
        }
        std::ptr::null()
    };
    // SAFETY: null or an object this runtime allocated; type-checked.
    let Some(bytes) = (unsafe { collections::data(data) }) else {
        return fail("the argument is not data");
    };
    let binary = bytes.starts_with(b"bplist00");
    let value = match crate::plist::parse(bytes) {
        Ok(value) => value,
        Err(why) => return fail(&why.to_string()),
    };
    match build(&value, options, 0) {
        Ok(object) => {
            if !format.is_null() {
                // SAFETY: the caller states it is writable.
                unsafe {
                    *format = if binary {
                        format::BINARY_V1_0
                    } else {
                        format::XML_V1_0
                    } as CFIndex
                };
            }
            object
        }
        Err(why) => fail(&why),
    }
}

/// `CFDataRef CFPropertyListCreateData(CFAllocatorRef, CFPropertyListRef,
/// CFPropertyListFormat, CFOptionFlags, CFErrorRef *error)`
///
/// XML and binary are written; the OpenStep format, which Apple can no
/// longer write either, and anything that is not a property list type are
/// errors.
///
/// # Safety
///
/// `list` is null or an object this runtime allocated; `error` is null or
/// writable for one value.
pub unsafe extern "C" fn CFPropertyListCreateData(
    _allocator: CFAllocatorRef,
    list: CFTypeRef,
    format: CFIndex,
    _options: u64,
    error: *mut CFTypeRef,
) -> CFTypeRef {
    if !error.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *error = std::ptr::null() };
    }
    let fail = |why: &str| {
        if !error.is_null() {
            // SAFETY: as above.
            unsafe { *error = error_object(why) };
        }
        std::ptr::null()
    };
    // SAFETY: null or an object this runtime allocated.
    let Some(value) = (unsafe { value_of(list) }) else {
        return fail("the object is not a property list");
    };
    let bytes = match format as i64 {
        format::XML_V1_0 => crate::plist::write_xml(&value),
        format::BINARY_V1_0 => crate::plist::write_binary(&value),
        _ => return fail("this property list format cannot be written"),
    };
    collections::new_data(bytes)
}

exports::exports! {
    functions {
        "CFPropertyListCreateData" => CFPropertyListCreateData,
        "CFPropertyListCreateWithData" => CFPropertyListCreateWithData,
        "CFPropertyListCreateFromXMLData" => CFPropertyListCreateFromXMLData,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>name</key><string>rose</string>
<key>count</key><integer>3</integer>
<key>on</key><true/>
<key>list</key><array><string>a</string><real>1.5</real></array>
</dict></plist>"#;

    #[test]
    fn reads_an_xml_list_and_reports_its_format() {
        let data = collections::new_data(XML.to_vec());
        let mut format: CFIndex = 0;
        let mut error: CFTypeRef = std::ptr::null();
        // SAFETY: an object made above and writable locals.
        let list = unsafe {
            CFPropertyListCreateWithData(std::ptr::null(), data, 0, &mut format, &mut error)
        };
        assert!(!list.is_null() && error.is_null());
        assert_eq!(format, 100);
        // SAFETY: a dictionary this call returned.
        let dictionary = unsafe { collections::dictionary(list) }.expect("a dictionary");
        assert_eq!(dictionary.entries.len(), 4);
        assert!(!dictionary.mutable);
        unsafe { api::CFRelease(list) };
        unsafe { api::CFRelease(data) };
    }

    #[test]
    fn garbage_is_null_and_not_a_panic() {
        let data = collections::new_data(b"not a plist".to_vec());
        let mut error: CFTypeRef = std::ptr::null();
        // SAFETY: as above.
        let list = unsafe {
            CFPropertyListCreateWithData(
                std::ptr::null(),
                data,
                0,
                std::ptr::null_mut(),
                &mut error,
            )
        };
        assert!(list.is_null());
        unsafe { api::CFRelease(data) };
    }

    #[test]
    fn mutable_containers_are_honoured() {
        let data = collections::new_data(XML.to_vec());
        // SAFETY: as above.
        let list = unsafe {
            CFPropertyListCreateWithData(
                std::ptr::null(),
                data,
                1,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        let dictionary = unsafe { collections::dictionary(list) }.expect("a dictionary");
        assert!(dictionary.mutable);
        unsafe { api::CFRelease(list) };
        unsafe { api::CFRelease(data) };
    }
}
