use objc::class::Class;
use objc::object::Id;

use crate::base::CFTypeRef;

/// `CFUUIDBytes`, which a caller may also pass by value.
pub type Bytes = [u8; 16];

#[repr(C)]
struct Body {
    isa: *const Class,
    bytes: Bytes,
}

pub fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        let class = crate::base::register(c"__NSCFUUID", None, size_of::<Body>(), true);
        objc::class::ptr(class) as usize
    });
    // SAFETY: the cell holds a pointer to a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

/// A UUID object holding `bytes`, owned by the caller.
pub fn new(bytes: Bytes) -> CFTypeRef {
    // SAFETY: `Body` begins with `isa` and is the layout `class` was registered
    // with.
    let body: *mut Body = unsafe { crate::base::allocate(class()) };
    if body.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a live instance whose remaining bytes are ours to fill in.
    unsafe { (*body).bytes = bytes };
    body as CFTypeRef
}

/// The bytes of `uuid`, if it is one.
///
/// # Safety
///
/// `uuid` must be null or an object this runtime allocated.
pub unsafe fn bytes_of(uuid: CFTypeRef) -> Option<Bytes> {
    if !unsafe { crate::base::is_kind_of(uuid, class()) } {
        return None;
    }
    // SAFETY: an instance of `class()` is a `Body`.
    Some(unsafe { (*(uuid as *const Body)).bytes })
}

/// Sixteen random bytes with version 4 and the RFC 4122 variant set.
pub fn generate() -> Bytes {
    let mut bytes = [0u8; 16];
    if let Err(e) = getrandom::fill(&mut bytes) {
        objc::fail!("CFUUIDCreate: this host has no source of randomness: {e}");
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes
}

/// Uppercase hexadecimal in 8-4-4-4-12 groups.
pub fn format(bytes: &Bytes) -> String {
    let hex = |range: std::ops::Range<usize>| {
        bytes[range]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<String>()
    };
    format!(
        "{}-{}-{}-{}-{}",
        hex(0..4),
        hex(4..6),
        hex(6..8),
        hex(8..10),
        hex(10..16)
    )
}

pub fn constant(bytes: Bytes) -> CFTypeRef {
    static CONSTANTS: std::sync::Mutex<Vec<(Bytes, usize)>> = std::sync::Mutex::new(Vec::new());
    let mut constants = CONSTANTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, object)) = constants.iter().find(|(b, _)| *b == bytes) {
        return *object as CFTypeRef;
    }
    let object = new(bytes);
    // Retained forever: a constant a guest may release is one it can make dangle.
    objc::arc::make_immortal(object as Id);
    constants.push((bytes, object as usize));
    object
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_uuid_is_version_four() {
        let bytes = generate();
        assert_eq!(bytes[6] & 0xf0, 0x40);
        assert_eq!(bytes[8] & 0xc0, 0x80);
    }

    #[test]
    fn the_text_is_grouped_and_uppercase() {
        let text = format(&[
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ]);
        assert_eq!(text, "01234567-89AB-CDEF-0123-456789ABCDEF");
    }
}
