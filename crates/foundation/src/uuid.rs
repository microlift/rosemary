//! `NSUUID`: sixteen bytes, made at random or parsed from the canonical
//! `8-4-4-4-12` hexadecimal spelling.

use std::ffi::c_void;

use corefoundation::uuid::Bytes;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
struct Uuid {
    isa: *const c_void,
    bytes: Bytes,
}

fn bytes_of<'a>(object: Id) -> &'a mut Bytes {
    // SAFETY: an instance of the class laid out as `Uuid`.
    unsafe { &mut (*(object as *mut Uuid)).bytes }
}

fn is_uuid(object: Id) -> bool {
    !object.is_null()
        && objc::class::lookup(c"NSUUID").is_some_and(|class| {
            // SAFETY: a live object.
            unsafe { corefoundation::base::is_kind_of(object.cast(), class) }
        })
}

/// The bytes of the canonical spelling, or `None` for anything else.
fn parse(text: &str) -> Option<Bytes> {
    let text = text.as_bytes();
    if text.len() != 36 || [8, 13, 18, 23].iter().any(|at| text[*at] != b'-') {
        return None;
    }
    let digits: Vec<u8> = text.iter().copied().filter(|c| *c != b'-').collect();
    let mut out = [0u8; 16];
    for (at, pair) in digits.chunks(2).enumerate() {
        let pair = std::str::from_utf8(pair).ok()?;
        out[at] = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    if !receiver.is_null() {
        *bytes_of(receiver) = corefoundation::uuid::generate();
    }
    receiver
}

unsafe extern "C" fn init_with_string(receiver: Id, _sel: Sel, string: Id) -> Id {
    // SAFETY: an `NSString` or nil.
    let text = String::from_utf8_lossy(&unsafe { crate::string::bytes(string) }).into_owned();
    match parse(&text) {
        Some(bytes) if !receiver.is_null() => {
            *bytes_of(receiver) = bytes;
            receiver
        }
        // Apple's answer for a malformed string.
        _ => {
            objc::arc::release(receiver);
            std::ptr::null_mut()
        }
    }
}

unsafe extern "C" fn init_with_bytes(receiver: Id, _sel: Sel, bytes: *const Bytes) -> Id {
    if !receiver.is_null() && !bytes.is_null() {
        // SAFETY: the caller's sixteen bytes.
        *bytes_of(receiver) = unsafe { *bytes };
    }
    receiver
}

unsafe extern "C" fn get_bytes(receiver: Id, _sel: Sel, out: *mut Bytes) {
    if !out.is_null() {
        // SAFETY: the caller's sixteen writable bytes.
        unsafe { *out = *bytes_of(receiver) };
    }
}

unsafe extern "C" fn uuid_string(receiver: Id, _sel: Sel) -> Id {
    let text = corefoundation::uuid::format(bytes_of(receiver));
    objc::arc::autorelease(crate::string::from_str(&text))
}

unsafe extern "C" fn new_uuid(class: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is `NSUUID` or a subclass.
    let class =
        unsafe { objc::class::from_ptr(class as *const objc::class::Class) }.expect("a class");
    let object = objc::object::create_instance(class, 0);
    // SAFETY: a fresh instance.
    objc::arc::autorelease(unsafe { init(object, objc::sel::get("init")) })
}

unsafe extern "C" fn is_equal(receiver: Id, _sel: Sel, other: Id) -> u8 {
    (is_uuid(other) && bytes_of(receiver) == bytes_of(other)) as u8
}

unsafe extern "C" fn hash(receiver: Id, _sel: Sel) -> usize {
    let bytes = bytes_of(receiver);
    usize::from_le_bytes(bytes[..8].try_into().expect("eight bytes"))
        ^ usize::from_le_bytes(bytes[8..].try_into().expect("eight bytes"))
}

unsafe extern "C" fn compare(receiver: Id, _sel: Sel, other: Id) -> isize {
    if !is_uuid(other) {
        return 1;
    }
    match (*bytes_of(receiver)).cmp(bytes_of(other)) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

unsafe extern "C" fn copy_with_zone(receiver: Id, _sel: Sel, _zone: *const c_void) -> Id {
    objc::arc::retain(receiver)
}

pub(crate) fn install() {
    let class = corefoundation::base::register(c"NSUUID", None, size_of::<Uuid>(), false);
    class_methods!(class, {
        "UUID" => new_uuid, c"@16@0:8";
    });
    methods!(class, {
        "init" => init, c"@16@0:8";
        "initWithUUIDString:" => init_with_string, c"@24@0:8@16";
        "initWithUUIDBytes:" => init_with_bytes, c"@24@0:8r*16";
        "getUUIDBytes:" => get_bytes, c"v24@0:8*16";
        "UUIDString" => uuid_string, c"@16@0:8";
        "description" => uuid_string, c"@16@0:8";
        "isEqual:" => is_equal, c"c24@0:8@16";
        "hash" => hash, c"Q16@0:8";
        "compare:" => compare, c"q24@0:8@16";
        "copyWithZone:" => copy_with_zone, c"@24@0:8^v16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_canonical_spelling_round_trips_and_anything_else_is_refused() {
        let text = "E3B0C442-98FC-1C14-9AFB-F4C8996FB924";
        let bytes = parse(text).expect("canonical");
        assert_eq!(corefoundation::uuid::format(&bytes), text);
        assert_eq!(parse(&text.to_lowercase()), Some(bytes));
        assert_eq!(parse("E3B0C44298FC1C149AFBF4C8996FB924"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("G3B0C442-98FC-1C14-9AFB-F4C8996FB924"), None);
    }
}
