use std::ffi::{CStr, CString};

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;
use crate::geometry::{NSPoint, NSRange, NSRect, NSSize};

/// The argument or return shapes an accessor may have here.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Shape {
    Object,
    Bool,
    /// A signed or unsigned integer, with its width in bytes.
    Integer(usize),
    Float,
    Double,
    Point,
    Size,
    Rect,
    Range,
}

impl Shape {
    /// The shape a type encoding names, or `None` for one not handled here.
    pub(crate) fn of(encoding: &[u8]) -> Option<Shape> {
        Some(match encoding.first()? {
            b'@' | b'#' => Shape::Object,
            b'B' => Shape::Bool,
            b'c' | b'C' => Shape::Integer(1),
            b's' | b'S' => Shape::Integer(2),
            b'i' | b'I' => Shape::Integer(4),
            b'q' | b'Q' | b'l' | b'L' => Shape::Integer(8),
            b'f' => Shape::Float,
            b'd' => Shape::Double,
            b'{' => match name_of_struct(encoding) {
                b"CGPoint" | b"NSPoint" | b"_NSPoint" => Shape::Point,
                b"CGSize" | b"NSSize" | b"_NSSize" => Shape::Size,
                b"CGRect" | b"NSRect" | b"_NSRect" => Shape::Rect,
                b"_NSRange" | b"NSRange" => Shape::Range,
                _ => return None,
            },
            _ => return None,
        })
    }
}

/// The tag of `{Name=...}`.
fn name_of_struct(encoding: &[u8]) -> &[u8] {
    let body = &encoding[1..];
    let end = body.iter().position(|b| *b == b'=').unwrap_or(body.len());
    &body[..end]
}

/// How many bytes of `encoding` the type at its head occupies.
///
/// Braces and brackets nest, so this is a depth count rather than a search for
/// the next delimiter.
fn type_length(encoding: &[u8]) -> usize {
    let mut depth = 0usize;
    for (i, b) in encoding.iter().enumerate() {
        match b {
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            // A qualifier or a pointer prefixes the type it applies to.
            b'r' | b'n' | b'N' | b'o' | b'O' | b'R' | b'V' | b'^' if depth == 0 => continue,
            _ if depth == 0 => return i + 1,
            _ => {}
        }
    }
    encoding.len()
}

/// Split a method encoding into its return type and its arguments, dropping the
/// frame offsets that follow each.
///
/// `v48@0:8{CGRect=...}16` becomes `["v", "@", ":", "{CGRect=...}"]`.
pub(crate) fn parts(encoding: &CStr) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut rest = encoding.to_bytes();
    while !rest.is_empty() {
        if rest[0].is_ascii_digit() {
            let end = rest
                .iter()
                .position(|b| !b.is_ascii_digit())
                .unwrap_or(rest.len());
            rest = &rest[end..];
            continue;
        }
        let len = type_length(rest);
        out.push(rest[..len].to_vec());
        rest = &rest[len..];
    }
    out
}

/// `bounds` as `setBounds:`.
pub(crate) fn setter_name(key: &str) -> String {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => format!("set{}{}:", first.to_uppercase(), chars.as_str()),
        None => "set:".to_string(),
    }
}

/// The value `object` holds, boxed as an object.
///
/// # Safety
///
/// `bytes` must be a value of `shape`.
unsafe fn box_value(shape: Shape, bytes: &[u8]) -> Id {
    // SAFETY: `bytes` is at least as wide as `shape`.
    unsafe {
        match shape {
            Shape::Object => *(bytes.as_ptr() as *const Id),
            Shape::Bool => crate::number(i64::from(*bytes.first().unwrap_or(&0) != 0)),
            Shape::Integer(width) => {
                let mut raw = [0u8; 8];
                raw[..width].copy_from_slice(&bytes[..width]);
                let shift = 64 - width * 8;
                crate::number(((i64::from_le_bytes(raw)) << shift) >> shift)
            }
            Shape::Float => crate::real(*(bytes.as_ptr() as *const f32) as f64),
            Shape::Double => crate::real(*(bytes.as_ptr() as *const f64)),
            Shape::Point => crate::geometry::box_point(*(bytes.as_ptr() as *const NSPoint)),
            Shape::Size => crate::geometry::box_size(*(bytes.as_ptr() as *const NSSize)),
            Shape::Rect => crate::geometry::box_rect(*(bytes.as_ptr() as *const NSRect)),
            Shape::Range => crate::geometry::box_range(*(bytes.as_ptr() as *const NSRange)),
        }
    }
}

/// `value` unboxed into `shape`'s bytes, or `None` if it is not that shape.
///
/// # Safety
///
/// `value` must be null or an object this runtime allocated.
unsafe fn unbox_value(shape: Shape, value: Id) -> Option<Vec<u8>> {
    unsafe {
        Some(match shape {
            Shape::Object => (value as usize).to_le_bytes().to_vec(),
            Shape::Bool => vec![u8::from(crate::number_of(value)? != 0)],
            Shape::Integer(width) => crate::number_of(value)?.to_le_bytes()[..width].to_vec(),
            Shape::Float => (crate::real_of(value)? as f32).to_le_bytes().to_vec(),
            Shape::Double => crate::real_of(value)?.to_le_bytes().to_vec(),
            Shape::Point | Shape::Size | Shape::Rect | Shape::Range => {
                crate::geometry::unbox_bytes(value)?
            }
        })
    }
}

/// Call `imp` as a setter taking one argument of `shape`.
///
/// # Safety
///
/// `imp` must be the implementation of a method whose one argument is `shape`,
/// and `bytes` must be a value of it.
unsafe fn call_setter(
    imp: *const std::ffi::c_void,
    receiver: Id,
    sel: Sel,
    shape: Shape,
    bytes: &[u8],
) {
    // Each arm states the prototype the encoding said the method has, which is
    // the only way the argument lands where the callee looks for it.
    macro_rules! call {
        ($ty:ty) => {{
            let f: unsafe extern "C" fn(Id, Sel, $ty) = unsafe { std::mem::transmute(imp) };
            // SAFETY: `bytes` is a value of this type, as just checked.
            let value = unsafe { *(bytes.as_ptr() as *const $ty) };
            unsafe { f(receiver, sel, value) }
        }};
    }
    match shape {
        Shape::Object => call!(Id),
        Shape::Bool => call!(bool),
        Shape::Integer(1) => call!(i8),
        Shape::Integer(2) => call!(i16),
        Shape::Integer(4) => call!(i32),
        Shape::Integer(_) => call!(i64),
        Shape::Float => call!(f32),
        Shape::Double => call!(f64),
        Shape::Point => call!(NSPoint),
        Shape::Size => call!(NSSize),
        Shape::Rect => call!(NSRect),
        Shape::Range => call!(NSRange),
    }
}

/// Call `imp` as a getter returning `shape`, and box what comes back.
///
/// # Safety
///
/// As [`call_setter`], for the return type.
unsafe fn call_getter(imp: *const std::ffi::c_void, receiver: Id, sel: Sel, shape: Shape) -> Id {
    macro_rules! call {
        ($ty:ty) => {{
            let f: unsafe extern "C" fn(Id, Sel) -> $ty = unsafe { std::mem::transmute(imp) };
            let value = unsafe { f(receiver, sel) };
            // SAFETY: `value` is a live value of this type.
            unsafe {
                box_value(
                    shape,
                    std::slice::from_raw_parts(
                        &value as *const $ty as *const u8,
                        std::mem::size_of::<$ty>(),
                    ),
                )
            }
        }};
    }
    match shape {
        Shape::Object => call!(Id),
        Shape::Bool => call!(bool),
        Shape::Integer(1) => call!(i8),
        Shape::Integer(2) => call!(i16),
        Shape::Integer(4) => call!(i32),
        Shape::Integer(_) => call!(i64),
        Shape::Float => call!(f32),
        Shape::Double => call!(f64),
        Shape::Point => call!(NSPoint),
        Shape::Size => call!(NSSize),
        Shape::Rect => call!(NSRect),
        Shape::Range => call!(NSRange),
    }
}

/// The ivar `key` names: `_key` as Apple looks for it first, then `key`.
fn ivar_for(class: &'static Class, key: &str) -> Option<&'static objc::class::Ivar> {
    for spelling in [format!("_{key}"), key.to_string()] {
        if let Ok(name) = CString::new(spelling)
            && let Some(ivar) = class.ivar(&name)
        {
            return Some(ivar);
        }
    }
    None
}

unsafe extern "C" fn set_value_for_key(receiver: Id, _sel: Sel, value: Id, key: Id) {
    // SAFETY: the key is an `NSString`.
    let key = unsafe { crate::string::bytes(key) };
    let Ok(key) = std::str::from_utf8(&key) else {
        return;
    };
    // SAFETY: a live object.
    let Some(class) = (unsafe { objc::object::class_of(receiver) }) else {
        return;
    };

    // The setter first, which is what makes this reach a guest's own code.
    let sel = objc::sel::get(&setter_name(key));
    if let Some(method) = class.method(sel) {
        let parts = parts(
            // SAFETY: a method's type encoding is a NUL-terminated string the
            // runtime owns.
            unsafe { CStr::from_ptr(method.types()) },
        );
        let Some(shape) = parts.get(3).and_then(|p| Shape::of(p)) else {
            objc::fail!(
                "-[{} setValue:forKey:@\"{key}\"]: this runtime does not know how to pass a {}",
                class.name_str(),
                parts
                    .get(3)
                    .map_or(String::new(), |p| String::from_utf8_lossy(p).into_owned())
            );
        };
        if value.is_null() && shape != Shape::Object {
            return send_key(receiver, "setNilValueForKey:", key);
        }
        // SAFETY: the value is an object this runtime allocated, or nil.
        let Some(bytes) = (unsafe { unbox_value(shape, value) }) else {
            objc::fail!(
                "-[{} setValue:forKey:@\"{key}\"]: a {} is not a {shape:?}",
                class.name_str(),
                unsafe { objc::object::class_of(value) }.map_or("nil", |c| c.name_str())
            );
        };
        // SAFETY: the encoding said the method takes one `shape`, and `bytes`
        // is one.
        unsafe { call_setter(method.implementation(), receiver, sel, shape, &bytes) };
        return;
    }

    // Then the ivar, which is what Apple falls back to.
    let Some(ivar) = ivar_for(class, key) else {
        objc::fail!(
            "-[{} setValue:forKey:@\"{key}\"]: no setter and no ivar of that name",
            class.name_str()
        );
    };
    let Some(shape) = Shape::of(ivar.types().to_bytes()) else {
        objc::fail!(
            "-[{} setValue:forKey:@\"{key}\"]: this runtime does not know the ivar's type {}",
            class.name_str(),
            ivar.types().to_string_lossy()
        );
    };
    if value.is_null() && shape != Shape::Object {
        return send_key(receiver, "setNilValueForKey:", key);
    }
    // SAFETY: a live object and a value of the ivar's own type.
    let Some(bytes) = (unsafe { unbox_value(shape, value) }) else {
        objc::fail!(
            "-[{} setValue:forKey:@\"{key}\"]: the value is not a {shape:?}",
            class.name_str()
        );
    };
    // A setter announces its own change once KVO has wrapped it; a store
    // straight into the ivar has to be announced here.
    let announce = crate::kvo::is_watched(receiver) && automatically_notifies(class, key);
    if announce {
        send_key(receiver, "willChangeValueForKey:", key);
    }
    // SAFETY: the ivar is inside the allocation and is `bytes.len()` wide.
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            (receiver as *mut u8).offset(ivar.offset()),
            bytes.len().min(ivar.size()),
        )
    };
    if announce {
        send_key(receiver, "didChangeValueForKey:", key);
    }
}

/// Send a `void (id, SEL, NSString *)` message naming `key`.
fn send_key(receiver: Id, selector: &str, key: &str) {
    let key = crate::string::from_str(key);
    // SAFETY: the selectors this is used with are `void (id, SEL, NSString *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector), key);
        corefoundation::api::CFRelease(key as corefoundation::base::CFTypeRef);
    }
}

fn automatically_notifies(class: &'static Class, key: &str) -> bool {
    let key = crate::string::from_str(key);
    // SAFETY: `BOOL (Class, SEL, NSString *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 = std::mem::transmute(objc::msg::entry());
        let answer = f(
            objc::class::ptr(class) as Id,
            objc::sel::get("automaticallyNotifiesObserversForKey:"),
            key,
        );
        corefoundation::api::CFRelease(key as corefoundation::base::CFTypeRef);
        answer != 0
    }
}

/// Send `-valueForKey:` with `key`, so an override answers.
fn send_value_for_key(receiver: Id, key: &[u8]) -> Id {
    let key = crate::string::from_str(&String::from_utf8_lossy(key));
    // SAFETY: `id (id, SEL, NSString *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
        let value = f(receiver, objc::sel::get("valueForKey:"), key);
        corefoundation::api::CFRelease(key as corefoundation::base::CFTypeRef);
        value
    }
}

/// `-valueForKeyPath:`: `-valueForKey:` for each component in turn.
unsafe extern "C" fn value_for_key_path(receiver: Id, _sel: Sel, path: Id) -> Id {
    // SAFETY: the path is an `NSString`.
    let path = unsafe { crate::string::bytes(path) };
    path.split(|b| *b == b'.')
        .fold(receiver, |object, key| match object.is_null() {
            true => object,
            false => send_value_for_key(object, key),
        })
}

/// `-setValue:forKeyPath:`: `-setValue:forKey:` on what the path's head leads to.
unsafe extern "C" fn set_value_for_key_path(receiver: Id, _sel: Sel, value: Id, path: Id) {
    // SAFETY: the path is an `NSString`.
    let path = unsafe { crate::string::bytes(path) };
    let (head, last) = match path.iter().rposition(|b| *b == b'.') {
        Some(dot) => (&path[..dot], &path[dot + 1..]),
        None => (&path[..0], &path[..]),
    };
    let target = match head.is_empty() {
        true => receiver,
        false => head
            .split(|b| *b == b'.')
            .fold(receiver, |object, key| match object.is_null() {
                true => object,
                false => send_value_for_key(object, key),
            }),
    };
    if target.is_null() {
        return;
    }
    let key = crate::string::from_str(&String::from_utf8_lossy(last));
    // SAFETY: `void (id, SEL, id, NSString *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) = std::mem::transmute(objc::msg::entry());
        f(target, objc::sel::get("setValue:forKey:"), value, key);
        corefoundation::api::CFRelease(key as corefoundation::base::CFTypeRef);
    }
}

unsafe extern "C" fn value_for_key(receiver: Id, _sel: Sel, key: Id) -> Id {
    // SAFETY: the key is an `NSString`.
    let key = unsafe { crate::string::bytes(key) };
    let Ok(key) = std::str::from_utf8(&key) else {
        return std::ptr::null_mut();
    };
    // SAFETY: a live object.
    let Some(class) = (unsafe { objc::object::class_of(receiver) }) else {
        return std::ptr::null_mut();
    };

    // The getter spellings Apple looks for, in its order.
    let mut chars = key.chars();
    let capitalized = match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    };
    for name in [
        key.to_string(),
        format!("get{capitalized}"),
        format!("is{capitalized}"),
        format!("_{key}"),
    ] {
        let sel = objc::sel::get(&name);
        let Some(method) = class.method(sel) else {
            continue;
        };
        // SAFETY: a NUL-terminated encoding the runtime owns.
        let parts = parts(unsafe { CStr::from_ptr(method.types()) });
        let Some(shape) = parts.first().and_then(|p| Shape::of(p)) else {
            objc::fail!(
                "-[{} valueForKey:@\"{key}\"]: this runtime does not know how to return a {}",
                class.name_str(),
                parts
                    .first()
                    .map_or(String::new(), |p| String::from_utf8_lossy(p).into_owned())
            );
        };
        // SAFETY: the encoding said the method returns a `shape`.
        return unsafe { call_getter(method.implementation(), receiver, sel, shape) };
    }

    let Some(ivar) = ivar_for(class, key) else {
        objc::fail!(
            "-[{} valueForKey:@\"{key}\"]: no getter and no ivar of that name",
            class.name_str()
        );
    };
    let Some(shape) = Shape::of(ivar.types().to_bytes()) else {
        objc::fail!(
            "-[{} valueForKey:@\"{key}\"]: this runtime does not know the ivar's type {}",
            class.name_str(),
            ivar.types().to_string_lossy()
        );
    };
    // SAFETY: the ivar is inside the allocation and holds a value of its type.
    unsafe {
        box_value(
            shape,
            std::slice::from_raw_parts((receiver as *const u8).offset(ivar.offset()), ivar.size()),
        )
    }
}

/// `-setNilValueForKey:`, which Apple makes an exception: nil is no scalar.
unsafe extern "C" fn set_nil_value_for_key(receiver: Id, _sel: Sel, key: Id) {
    // SAFETY: a live object, and the key is an `NSString`.
    let (class, key) = unsafe { (objc::object::class_of(receiver), crate::string::bytes(key)) };
    objc::fail!(
        "-[{} setNilValueForKey:]: could not set nil as the value for the key {}",
        class.map_or("nil", |c| c.name_str()),
        String::from_utf8_lossy(&key)
    );
}

/// Whether setting nil for `key` on an object of `class` would reach
/// `-setNilValueForKey:` - a scalar key - and the class has not overridden
/// that to make it mean something.
pub fn rejects_nil(class: &'static Class, key: &str) -> bool {
    let shape = match class.method(objc::sel::get(&setter_name(key))) {
        // SAFETY: a method's type encoding is a NUL-terminated string the
        // runtime owns.
        Some(method) => parts(unsafe { CStr::from_ptr(method.types()) })
            .get(3)
            .and_then(|p| Shape::of(p)),
        None => ivar_for(class, key).and_then(|i| Shape::of(i.types().to_bytes())),
    };
    if shape.is_none_or(|s| s == Shape::Object) {
        return false;
    }
    let nsobject = objc::class::lookup(c"NSObject").expect("NSObject");
    let handler = objc::sel::get("setNilValueForKey:");
    class.method(handler).map(|m| m.implementation())
        == nsobject.method(handler).map(|m| m.implementation())
}

/// Whether `-setValue:forKey:` on an object of `class` would reach anything.
pub fn accepts(class: &'static Class, key: &str) -> bool {
    if let Some(method) = class.method(objc::sel::get(&setter_name(key))) {
        // A setter whose argument this runtime cannot pass is a setter that
        // would abort, so it does not count as accepting the key.
        // SAFETY: a method's type encoding is a NUL-terminated string the
        // runtime owns.
        let parts = parts(unsafe { CStr::from_ptr(method.types()) });
        return parts.get(3).and_then(|p| Shape::of(p)).is_some();
    }
    ivar_for(class, key).is_some_and(|i| Shape::of(i.types().to_bytes()).is_some())
}

pub(crate) fn install() {
    let nsobject = objc::class::lookup(c"NSObject").expect("the runtime's own NSObject");
    methods!(nsobject, {
        "setValue:forKey:" => set_value_for_key, c"v32@0:8@16@24";
        "setNilValueForKey:" => set_nil_value_for_key, c"v24@0:8@16";
        "valueForKey:" => value_for_key, c"@24@0:8@16";
        "setValue:forKeyPath:" => set_value_for_key_path, c"v32@0:8@16@24";
        "valueForKeyPath:" => value_for_key_path, c"@24@0:8@16"
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_method_encoding_splits_into_its_types() {
        let split = parts(c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16");
        let split: Vec<String> = split
            .iter()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect();
        assert_eq!(
            split,
            vec!["v", "@", ":", "{CGRect={CGPoint=dd}{CGSize=dd}}"]
        );
        assert_eq!(Shape::of(split[3].as_bytes()), Some(Shape::Rect));
        assert_eq!(Shape::of(b"d"), Some(Shape::Double));
        assert_eq!(Shape::of(b"@"), Some(Shape::Object));
        // A shape this runtime cannot pass is `None` rather than a guess.
        assert_eq!(Shape::of(b"{Whatever=ii}"), None);
    }

    #[test]
    fn a_setter_name_is_the_key_capitalized() {
        assert_eq!(setter_name("bounds"), "setBounds:");
        assert_eq!(setter_name("x"), "setX:");
        assert_eq!(setter_name(""), "set:");
    }
}
