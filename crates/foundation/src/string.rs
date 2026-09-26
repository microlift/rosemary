use std::ffi::{CStr, c_char};

use corefoundation::api;
use corefoundation::base::CFTypeRef;
use corefoundation::string::encoding;

use crate::geometry::NSRange;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// `NSStringEncoding`, which is *not* `CFStringEncoding`: Foundation numbers its
/// encodings separately and the two are converted at the boundary.
pub mod ns_encoding {
    pub const ASCII: usize = 1;
    pub const NEXTSTEP: usize = 2;
    pub const UTF8: usize = 4;
    pub const ISO_LATIN1: usize = 5;
    pub const UNICODE: usize = 10;
    pub const WINDOWS_CP1252: usize = 12;
    pub const UTF16_BE: usize = 0x9000_0100;
    pub const UTF16_LE: usize = 0x9400_0100;
    pub const UTF32: usize = 0x8c00_0100;
    pub const UTF32_BE: usize = 0x9800_0100;
    pub const UTF32_LE: usize = 0x9c00_0100;
}

/// An `NSStringEncoding` as the CoreFoundation one.
pub fn cf_encoding(ns: usize) -> u32 {
    match ns {
        ns_encoding::ASCII => encoding::ASCII,
        ns_encoding::ISO_LATIN1 => encoding::ISO_LATIN_1,
        ns_encoding::WINDOWS_CP1252 => encoding::WINDOWS_LATIN1,
        ns_encoding::UNICODE => encoding::UTF16,
        ns_encoding::UTF16_LE => encoding::UTF16_LE,
        ns_encoding::UTF16_BE => encoding::UTF16_BE,
        ns_encoding::UTF32 => encoding::UTF32,
        ns_encoding::UTF32_BE => encoding::UTF32_BE,
        ns_encoding::UTF32_LE => encoding::UTF32_LE,
        // UTF-8 for everything else, including `NSNEXTSTEPStringEncoding`: it
        // is the encoding every caller in this tree has actually asked for, and
        // a wrong guess here is a string that reads back empty rather than one
        // that reads back wrong.
        _ => encoding::UTF8,
    }
}

/// The receiver's characters as UTF-8, or empty for something that is not a
/// string.
///
/// # Safety
///
/// `receiver` must be null or an object this runtime allocated.
pub unsafe fn bytes(receiver: Id) -> Vec<u8> {
    unsafe { corefoundation::string::as_string(receiver as CFTypeRef) }
        .map(|s| s.utf8())
        .unwrap_or_default()
}

unsafe extern "C" fn length(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: an `NSString`.
    unsafe { api::CFStringGetLength(receiver as CFTypeRef) as usize }
}

/// The string's own storage when it already is the C string, or else a
/// NUL-terminated copy in an autoreleased buffer - `-UTF8String` never answers
/// NULL for a string, whatever `CFStringGetCStringPtr` may.
///
/// # Safety
///
/// `receiver` is nil or a string object.
unsafe fn c_string_in(receiver: Id, cf: u32) -> *const c_char {
    if receiver.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a string object.
    let direct = unsafe { api::CFStringGetCStringPtr(receiver as CFTypeRef, cf) };
    if !direct.is_null() {
        return direct;
    }
    let text = match cf {
        encoding::UTF8 => Some(unsafe { bytes(receiver) }),
        code if corefoundation::eightbit::is_table(code) => unsafe { units(receiver) }
            .into_iter()
            .map(|unit| corefoundation::eightbit::encode(unit, code))
            .collect(),
        _ => None,
    };
    // A character the encoding has not got makes the conversion fail, as
    // `-cStringUsingEncoding:` answers NULL for.
    let Some(mut text) = text else {
        return std::ptr::null();
    };
    text.push(0);
    let data = objc::arc::autorelease(corefoundation::collections::new_data(text) as Id);
    // SAFETY: a data object this call made, alive until the pool drains.
    unsafe { api::CFDataGetBytePtr(data as CFTypeRef) }.cast()
}

unsafe extern "C" fn utf8_string(receiver: Id, _sel: Sel) -> *const c_char {
    // SAFETY: an `NSString`.
    unsafe { c_string_in(receiver, encoding::UTF8) }
}

unsafe extern "C" fn c_string_using_encoding(receiver: Id, _sel: Sel, ns: usize) -> *const c_char {
    // SAFETY: an `NSString`.
    unsafe { c_string_in(receiver, cf_encoding(ns)) }
}

unsafe extern "C" fn get_c_string(
    receiver: Id,
    _sel: Sel,
    buffer: *mut c_char,
    max: usize,
    ns: usize,
) -> bool {
    // SAFETY: an `NSString` and the caller's buffer.
    unsafe {
        api::CFStringGetCString(receiver as CFTypeRef, buffer, max as isize, cf_encoding(ns)) != 0
    }
}

unsafe extern "C" fn length_of_bytes(receiver: Id, _sel: Sel, _ns: usize) -> usize {
    // SAFETY: an `NSString`. Bytes rather than characters, which for an
    // eight-bit encoding is the same number and for UTF-8 is the one the caller
    // is about to allocate.
    unsafe { bytes(receiver) }.len()
}

unsafe extern "C" fn is_equal_to_string(receiver: Id, _sel: Sel, other: Id) -> bool {
    // SAFETY: two objects; `CFEqual` checks both.
    unsafe { api::CFEqual(receiver as CFTypeRef, other as CFTypeRef) != 0 }
}

/// The UTF-16 code units, which `-length` counts and every index and range a
/// guest passes is in.
///
/// # Safety
///
/// `receiver` is nil or a string object.
unsafe fn units(receiver: Id) -> Vec<u16> {
    unsafe { corefoundation::string::as_string(receiver as CFTypeRef) }
        .map(|s| s.units())
        .unwrap_or_default()
}

/// A new autoreleased string of `units[from..to]`, clamped to its length.
fn unit_slice(units: &[u16], from: usize, to: usize) -> Id {
    let (from, to) = (from.min(units.len()), to.min(units.len()));
    let s = corefoundation::string::from_utf16(&units[from.min(to)..to]);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(s) as Id }
}

unsafe extern "C" fn character_at_index(receiver: Id, _sel: Sel, index: usize) -> u16 {
    // SAFETY: an `NSString`.
    unsafe { units(receiver) }.get(index).copied().unwrap_or(0)
}

unsafe extern "C" fn append_string(receiver: Id, _sel: Sel, other: Id) -> Id {
    // SAFETY: two `NSString`s, or nil.
    let mut joined = unsafe { bytes(receiver) };
    joined.extend_from_slice(&unsafe { bytes(other) });
    let s = corefoundation::string::from_bytes(&joined, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(s) as Id }
}

/// `-stringByReplacingOccurrencesOfString:withString:`: a literal search, so
/// matching UTF-8 bytes finds exactly the matches UTF-16 units would.
unsafe extern "C" fn replacing_occurrences(receiver: Id, _sel: Sel, target: Id, with: Id) -> Id {
    // SAFETY: `NSString`s, or nil.
    let (haystack, needle, with) = unsafe { (bytes(receiver), bytes(target), bytes(with)) };
    if needle.is_empty() {
        return objc::arc::autorelease(objc::arc::retain(receiver));
    }
    let mut out = Vec::with_capacity(haystack.len());
    let mut rest = haystack.as_slice();
    while let Some(at) = rest.windows(needle.len()).position(|w| w == needle.as_slice()) {
        out.extend_from_slice(&rest[..at]);
        out.extend_from_slice(&with);
        rest = &rest[at + needle.len()..];
    }
    out.extend_from_slice(rest);
    let s = corefoundation::string::from_bytes(&out, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(s) as Id }
}

unsafe extern "C" fn description(receiver: Id, _sel: Sel) -> Id {
    receiver
}

unsafe extern "C" fn copy(receiver: Id, _sel: Sel) -> Id {
    // A copy of an immutable string is the string, which is what Apple's does
    // and why `-copy` on one is free.
    objc::arc::retain(receiver)
}

unsafe extern "C" fn string_with_utf8(_class: Id, _sel: Sel, cstr: *const c_char) -> Id {
    // SAFETY: a NUL-terminated string or null.
    unsafe {
        let s = api::CFStringCreateWithCString(std::ptr::null(), cstr, encoding::UTF8);
        api::CFAutorelease(s) as Id
    }
}

/// `+stringWithCharacters:length:`, the UTF-16 constructor.
///
/// The Objective-C spelling of `CFStringCreateWithCharacters`, and the one a
/// program holding UTF-16 uses: Qt's `QString` *is* UTF-16, so this is how every
/// string it hands to Cocoa is built. The code units are kept as they are rather
/// than going through UTF-8, for the reason that call gives - an unpaired
/// surrogate a caller put there is a code unit it is entitled to read back.
///
/// # Safety
///
/// `chars` is null or covers `count` `unichar`s.
unsafe extern "C" fn string_with_characters(
    _class: Id,
    _sel: Sel,
    chars: *const u16,
    count: usize,
) -> Id {
    let string = unsafe {
        api::CFStringCreateWithCharacters(
            std::ptr::null(),
            chars,
            count as corefoundation::base::CFIndex,
        )
    };
    if string.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a fresh object this call owns.
    unsafe { api::CFAutorelease(string) as Id }
}

unsafe extern "C" fn string_with_cstring_encoding(
    _class: Id,
    _sel: Sel,
    cstr: *const c_char,
    ns: usize,
) -> Id {
    unsafe {
        let s = api::CFStringCreateWithCString(std::ptr::null(), cstr, cf_encoding(ns));
        api::CFAutorelease(s) as Id
    }
}

/// `+stringWithContentsOfURL:encoding:error:`, for the local file URLs this
/// runtime can read.
unsafe extern "C" fn string_with_contents_of_url(
    _class: Id,
    _sel: Sel,
    url: Id,
    ns: usize,
    error: *mut Id,
) -> Id {
    if !error.is_null() {
        // SAFETY: the caller passed a writable `NSError **`.
        unsafe { *error = std::ptr::null_mut() };
    }
    let Some(path) =
        (unsafe { corefoundation::url::as_url(url as CFTypeRef).and_then(|url| url.file_path()) })
    else {
        if !error.is_null() {
            // SAFETY: the caller passed a writable `NSError **`.
            unsafe {
                *error = crate::error::make(
                    "NSCocoaErrorDomain",
                    256,
                    "stringWithContentsOfURL: requires a file URL",
                );
            }
        }
        return std::ptr::null_mut();
    };
    // The guest named it by URL, so it is read at this host's spelling.
    let bytes = match std::fs::read(prefix::ns::host_path(&path)) {
        Ok(bytes) => bytes,
        Err(why) => {
            if !error.is_null() {
                // SAFETY: the caller passed a writable `NSError **`.
                unsafe {
                    *error = crate::error::make(
                        "NSCocoaErrorDomain",
                        256,
                        &format!("could not read {path}: {why}"),
                    );
                }
            }
            return std::ptr::null_mut();
        }
    };
    // SAFETY: the bytes are a live buffer of their own length.
    let string = unsafe {
        api::CFStringCreateWithBytes(
            std::ptr::null(),
            bytes.as_ptr(),
            bytes.len() as isize,
            cf_encoding(ns),
            0,
        ) as Id
    };
    if string.is_null() && !error.is_null() {
        // SAFETY: the caller passed a writable `NSError **`.
        unsafe {
            *error = crate::error::make(
                "NSCocoaErrorDomain",
                261,
                &format!("could not decode {path} with encoding {ns}"),
            );
        }
    }
    if string.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a string this call just created.
    unsafe { api::CFAutorelease(string as CFTypeRef) as Id }
}

/// `+stringWithFormat:`, which arrives already rendered.
///
/// A guest's variadic message never reaches an `IMP`: `objc::dispatch` reads the
/// format and the guest's variadic tail out of its registers, renders the two
/// together, doubles every `%` in the result so it cannot be read as a format
/// again, and re-sends the counted message with that string standing in for the
/// original. So the only work left here is undoing that doubling.
///
/// A lone `%` is therefore something upstream did not render. It is passed
/// through as itself - a visible oddity in the output - and said once, rather
/// than dropped.
fn unescape_rendered(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes.iter().copied().peekable();
    while let Some(b) = rest.next() {
        if b != b'%' {
            out.push(b);
            continue;
        }
        match rest.peek() {
            Some(b'%') => {
                rest.next();
                out.push(b'%');
            }
            _ => {
                unrendered_conversion();
                out.push(b'%');
            }
        }
    }
    out
}

fn unrendered_conversion() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SAID: AtomicBool = AtomicBool::new(false);
    if !SAID.swap(true, Ordering::Relaxed) {
        eprintln!(
            "rosetta3: foundation: a format reached +[NSString stringWithFormat:] with a \
             conversion still in it, which means it was not rendered on the way"
        );
    }
}

unsafe extern "C" fn string_with_format(_class: Id, _sel: Sel, format: Id) -> Id {
    // SAFETY: the format is an `NSString` or nil.
    let rendered = unescape_rendered(&unsafe { bytes(format) });
    let s = corefoundation::string::from_bytes(&rendered, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(s) as Id }
}

unsafe extern "C" fn init_with_format(receiver: Id, sel: Sel, format: Id) -> Id {
    // SAFETY: the allocated shell is released and the built string returned,
    // which is what every `-init...` in this crate does.
    unsafe {
        objc::arc::release(receiver);
        let rendered = string_with_format(std::ptr::null_mut(), sel, format);
        objc::arc::retain(rendered)
    }
}

unsafe extern "C" fn empty_string(_class: Id, _sel: Sel) -> Id {
    // SAFETY: a literal.
    unsafe {
        let s = api::CFStringCreateWithCString(std::ptr::null(), c"".as_ptr(), encoding::UTF8);
        api::CFAutorelease(s) as Id
    }
}

/// `-initWithBytes:length:encoding:` and its `-initWithCString:` relatives.
///
/// The awkward shape Objective-C allocation has: `+alloc` already made an
/// object, and `-init...` may hand back a *different* one. Here it always does -
/// the allocated shell is released and a properly built string returned - which
/// is allowed, expected, and the only way to reuse the CoreFoundation
/// constructor.
unsafe extern "C" fn init_with_bytes(
    receiver: Id,
    _sel: Sel,
    bytes: *const u8,
    length: usize,
    ns: usize,
) -> Id {
    // SAFETY: the caller says `bytes` covers `length` bytes.
    unsafe {
        objc::arc::release(receiver);
        api::CFStringCreateWithBytes(std::ptr::null(), bytes, length as isize, cf_encoding(ns), 0)
            as Id
    }
}

unsafe extern "C" fn init_with_utf8(receiver: Id, _sel: Sel, cstr: *const c_char) -> Id {
    // SAFETY: a NUL-terminated string or null.
    unsafe {
        objc::arc::release(receiver);
        api::CFStringCreateWithCString(std::ptr::null(), cstr, encoding::UTF8) as Id
    }
}

unsafe extern "C" fn init_with_cstring_encoding(
    receiver: Id,
    _sel: Sel,
    cstr: *const c_char,
    ns: usize,
) -> Id {
    unsafe {
        objc::arc::release(receiver);
        api::CFStringCreateWithCString(std::ptr::null(), cstr, cf_encoding(ns)) as Id
    }
}

/// `-dataUsingEncoding:`, which is how a string reaches anything that takes
/// bytes - `NSJSONSerialization` and `-[NSData writeToFile:]` both.
///
/// Answers nil for a string this encoding cannot spell, which is Apple's
/// contract and matters: a lossy conversion here is a message body with a
/// character silently replaced, where nil is a caller's own error path.
unsafe extern "C" fn data_using_encoding(receiver: Id, _sel: Sel, ns: usize) -> Id {
    // SAFETY: an `NSString`.
    let utf8 = unsafe { bytes(receiver) };
    let encoded = match ns {
        ns_encoding::UTF8 => Some(utf8),
        ns_encoding::ASCII => utf8.iter().all(|b| *b < 0x80).then_some(utf8),
        ns_encoding::ISO_LATIN1 | ns_encoding::WINDOWS_CP1252 => String::from_utf8(utf8)
            .ok()
            .and_then(|text| text.chars().map(|c| u8::try_from(c as u32).ok()).collect()),
        ns_encoding::UNICODE | ns_encoding::UTF16_LE | ns_encoding::UTF16_BE => {
            String::from_utf8(utf8).ok().map(|text| {
                let big_endian = ns == ns_encoding::UTF16_BE;
                text.encode_utf16()
                    .flat_map(|unit| match big_endian {
                        true => unit.to_be_bytes(),
                        false => unit.to_le_bytes(),
                    })
                    .collect()
            })
        }
        // Refused by number rather than folded into UTF-8 the way `cf_encoding`
        // folds it: the answer here is bytes the caller writes out or sends, so
        // an encoding nobody translated would leave the wrong ones on the wire
        // under the right name.
        _ => {
            eprintln!(
                "rosetta3: foundation: -[NSString dataUsingEncoding:]: encoding {ns} is not one \
                 this runtime encodes"
            );
            None
        }
    };
    match encoded {
        Some(bytes) => {
            let data = corefoundation::collections::new_data(bytes);
            // SAFETY: a fresh object this function owns a reference to.
            unsafe { api::CFAutorelease(data) as Id }
        }
        None => std::ptr::null_mut(),
    }
}

/// `-dataUsingEncoding:allowLossyConversion:`.
///
/// The flag is ignored, and that is the honest reading of it: nothing above
/// converts lossily, so a caller passing `YES` gets the same nil a caller
/// passing `NO` gets rather than a string with characters dropped.
unsafe extern "C" fn data_using_encoding_lossy(
    receiver: Id,
    sel: Sel,
    ns: usize,
    _lossy: bool,
) -> Id {
    unsafe { data_using_encoding(receiver, sel, ns) }
}

/// `-initWithData:encoding:`, the other direction.
unsafe extern "C" fn init_with_data(receiver: Id, _sel: Sel, data: Id, ns: usize) -> Id {
    objc::arc::release(receiver);
    // Type-checked: a length read out of another class's field is the first
    // thing this would allocate.
    // SAFETY: a live object.
    let Some(bytes) = (unsafe { corefoundation::collections::data(data as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `bytes` is a live buffer of its own length.
    unsafe {
        api::CFStringCreateWithBytes(
            std::ptr::null(),
            bytes.as_ptr(),
            bytes.len() as isize,
            cf_encoding(ns),
            0,
        ) as Id
    }
}

/// A Rust string as an `NSString` the caller owns, for this crate's own use.
pub fn from_str(text: &str) -> Id {
    corefoundation::string::from_bytes(text.as_bytes(), false) as Id
}

/// A `&CStr` as an `NSString` the caller owns.
pub fn from_cstr(text: &CStr) -> Id {
    corefoundation::string::from_bytes(text.to_bytes(), false) as Id
}

/// Replace a mutable string's characters, or say by name that it is not one.
///
/// # Safety
///
/// `receiver` must be null or an object this runtime allocated.
unsafe fn replace(receiver: Id, selector: &str, bytes: &[u8]) {
    if unsafe { corefoundation::string::set_bytes(receiver as CFTypeRef, bytes) } {
        return;
    }
    let class = unsafe { objc::object::class_of(receiver) };
    objc::fail!(
        "-[{} {selector}]: the string was not created mutable",
        class.map_or("nil", |c| c.name_str())
    );
}

unsafe extern "C" fn append_string_in_place(receiver: Id, _sel: Sel, other: Id) {
    // SAFETY: two `NSString`s, or nil.
    let mut joined = unsafe { bytes(receiver) };
    joined.extend_from_slice(&unsafe { bytes(other) });
    unsafe { replace(receiver, "appendString:", &joined) };
}

/// `-appendFormat:`, which arrives already rendered - see [`string_with_format`].
unsafe extern "C" fn append_format(receiver: Id, _sel: Sel, format: Id) {
    // SAFETY: two `NSString`s, or nil.
    let mut joined = unsafe { bytes(receiver) };
    joined.extend_from_slice(&unescape_rendered(&unsafe { bytes(format) }));
    unsafe { replace(receiver, "appendFormat:", &joined) };
}

unsafe extern "C" fn set_string(receiver: Id, _sel: Sel, other: Id) {
    // SAFETY: two `NSString`s, or nil.
    let bytes = unsafe { bytes(other) };
    unsafe { replace(receiver, "setString:", &bytes) };
}

unsafe extern "C" fn mutable_string(_class: Id, _sel: Sel) -> Id {
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(corefoundation::string::new_mutable()) as Id }
}

unsafe extern "C" fn mutable_string_with_capacity(_class: Id, _sel: Sel, _capacity: usize) -> Id {
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(corefoundation::string::new_mutable()) as Id }
}

unsafe extern "C" fn init_mutable(receiver: Id, _sel: Sel) -> Id {
    // The allocated shell is released and a mutable string returned, which is
    // what every `-init...` in this crate does.
    objc::arc::release(receiver);
    corefoundation::string::new_mutable() as Id
}

/// `-mutableCopy`, which is a mutable string with the same characters.
unsafe extern "C" fn mutable_copy(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSString`.
    let bytes = unsafe { bytes(receiver) };
    let copy = corefoundation::string::new_mutable();
    // SAFETY: a mutable string this call just made.
    unsafe { corefoundation::string::set_bytes(copy, &bytes) };
    copy as Id
}

/// `NSNotFound`, which is `NSIntegerMax`.
const NOT_FOUND: usize = usize::MAX >> 1;

unsafe extern "C" fn range_of_string(receiver: Id, _sel: Sel, needle: Id) -> NSRange {
    // SAFETY: two `NSString`s, or nil.
    let (haystack, needle) = unsafe { (units(receiver), units(needle)) };
    if needle.is_empty() {
        return NSRange {
            location: NOT_FOUND,
            length: 0,
        };
    }
    match haystack
        .windows(needle.len())
        .position(|w| w == needle.as_slice())
    {
        Some(at) => NSRange {
            location: at,
            length: needle.len(),
        },
        None => NSRange {
            location: NOT_FOUND,
            length: 0,
        },
    }
}

/// `-containsString:`, which is `-rangeOfString:` reaching a match.
///
/// An empty needle is not contained: `-rangeOfString:` answers `NSNotFound` for
/// one, and Apple's `-containsString:` is documented as that comparison.
unsafe extern "C" fn contains_string(receiver: Id, sel: Sel, needle: Id) -> bool {
    // SAFETY: `-rangeOfString:` is `NSRange (id, SEL, id)`.
    unsafe { range_of_string(receiver, sel, needle) }.location != NOT_FOUND
}

unsafe extern "C" fn has_prefix(receiver: Id, _sel: Sel, prefix: Id) -> bool {
    // SAFETY: two `NSString`s, or nil.
    let (whole, prefix) = unsafe { (bytes(receiver), bytes(prefix)) };
    whole.starts_with(&prefix)
}

unsafe extern "C" fn has_suffix(receiver: Id, _sel: Sel, suffix: Id) -> bool {
    // SAFETY: two `NSString`s, or nil.
    let (whole, suffix) = unsafe { (bytes(receiver), bytes(suffix)) };
    whole.ends_with(&suffix)
}

/// A new autoreleased string from a slice of this one, clamped to its length.
fn slice(bytes: &[u8], from: usize, to: usize) -> Id {
    let (from, to) = (from.min(bytes.len()), to.min(bytes.len()));
    let s = corefoundation::string::from_bytes(&bytes[from.min(to)..to], false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(s) as Id }
}

unsafe extern "C" fn removing_percent_encoding(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is an NSString.
    let original = unsafe { bytes(receiver) };
    let mut decoded = Vec::with_capacity(original.len());
    let mut index = 0;
    while index < original.len() {
        if original[index] == b'%' {
            let hex = |byte: u8| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            };
            let value = original
                .get(index + 1)
                .copied()
                .and_then(hex)
                .zip(original.get(index + 2).copied().and_then(hex));
            let Some((high, low)) = value else {
                return std::ptr::null_mut();
            };
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(original[index]);
            index += 1;
        }
    }
    if std::str::from_utf8(&decoded).is_err() {
        return std::ptr::null_mut();
    }
    slice(&decoded, 0, decoded.len())
}

/// Percent-encode UTF-8 for an `URLPathAllowedCharacterSet`.
unsafe extern "C" fn adding_percent_encoding(receiver: Id, _sel: Sel, allowed: Id) -> Id {
    if allowed.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the receiver is an NSString.
    let original = unsafe { bytes(receiver) };
    let mut encoded = Vec::with_capacity(original.len());
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in original {
        // SAFETY: the caller supplied an `NSCharacterSet`.
        let allowed = unsafe { crate::characterset::contains(allowed, byte) };
        if allowed {
            encoded.push(byte);
        } else {
            encoded.extend_from_slice(&[
                b'%',
                HEX[(byte >> 4) as usize],
                HEX[(byte & 15) as usize],
            ]);
        }
    }
    slice(&encoded, 0, encoded.len())
}

unsafe extern "C" fn trimming_characters(receiver: Id, _sel: Sel, set: Id) -> Id {
    if set.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the receiver is an NSString.
    let original = unsafe { bytes(receiver) };
    let start = original
        .iter()
        .position(|byte| {
            // SAFETY: `set` is the caller's live NSCharacterSet.
            !unsafe { crate::characterset::contains(set, *byte) }
        })
        .unwrap_or(original.len());
    let end = original
        .iter()
        .rposition(|byte| !unsafe { crate::characterset::contains(set, *byte) })
        .map_or(start, |index| index + 1);
    slice(&original, start, end)
}

unsafe extern "C" fn substring_from_index(receiver: Id, _sel: Sel, index: usize) -> Id {
    // SAFETY: an `NSString`.
    let whole = unsafe { units(receiver) };
    unit_slice(&whole, index, whole.len())
}

unsafe extern "C" fn substring_to_index(receiver: Id, _sel: Sel, index: usize) -> Id {
    // SAFETY: an `NSString`.
    let whole = unsafe { units(receiver) };
    unit_slice(&whole, 0, index)
}

unsafe extern "C" fn substring_with_range(receiver: Id, _sel: Sel, range: NSRange) -> Id {
    // SAFETY: an `NSString`.
    let whole = unsafe { units(receiver) };
    unit_slice(
        &whole,
        range.location,
        range.location.saturating_add(range.length),
    )
}

/// `-getCharacters:range:`, which fills `buffer` with the UTF-16 of a range.
///
/// Real UTF-16 rather than the bytes `-characterAtIndex:` hands back, because
/// this is the call a caller uses to read a *whole string* out in one go - Qt's
/// `QString::fromCFString` does exactly that - and a string with anything outside
/// ASCII in it would arrive as its UTF-8 bytes reinterpreted as characters.
///
/// A range outside the string writes nothing, for the reason
/// `corefoundation::api::CFStringGetCharacters` gives: the caller sized the
/// buffer from the range it asked for.
///
/// # Safety
///
/// `buffer` is writable for `range.length` `unichar`s.
unsafe extern "C" fn get_characters_range(
    receiver: Id,
    _sel: Sel,
    buffer: *mut u16,
    range: NSRange,
) {
    // SAFETY: the receiver is an `NSString`, and the caller states the buffer
    // holds the range's characters.
    unsafe {
        api::CFStringGetCharacters(
            receiver as CFTypeRef,
            range.location as corefoundation::base::CFIndex,
            range.length as corefoundation::base::CFIndex,
            buffer,
        )
    }
}

/// `-getCharacters:`, which is the whole string rather than a range.
///
/// # Safety
///
/// `buffer` is writable for `-length` `unichar`s.
unsafe extern "C" fn get_characters(receiver: Id, sel: Sel, buffer: *mut u16) {
    // SAFETY: an `NSString`.
    let length = unsafe { length(receiver, sel) };
    // SAFETY: the caller states the buffer holds the whole string.
    unsafe {
        get_characters_range(
            receiver,
            sel,
            buffer,
            NSRange {
                location: 0,
                length,
            },
        )
    }
}

unsafe extern "C" fn lowercase(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSString`.
    let mut whole = unsafe { bytes(receiver) };
    whole.make_ascii_lowercase();
    let s = corefoundation::string::from_bytes(&whole, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(s) as Id }
}

unsafe extern "C" fn uppercase(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSString`.
    let mut whole = unsafe { bytes(receiver) };
    whole.make_ascii_uppercase();
    let s = corefoundation::string::from_bytes(&whole, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(s) as Id }
}

unsafe extern "C" fn compare(receiver: Id, _sel: Sel, other: Id) -> isize {
    // SAFETY: two `NSString`s, or nil.
    let (a, b) = unsafe { (bytes(receiver), bytes(other)) };
    match a.cmp(&b) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

unsafe extern "C" fn int_value(receiver: Id, _sel: Sel) -> i32 {
    // SAFETY: an `NSString`.
    parse_leading(&unsafe { bytes(receiver) }).map_or(0, |v| v as i32)
}

unsafe extern "C" fn integer_value(receiver: Id, _sel: Sel) -> i64 {
    // SAFETY: an `NSString`.
    parse_leading(&unsafe { bytes(receiver) }).unwrap_or(0)
}

unsafe extern "C" fn double_value(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: an `NSString`.
    let text = unsafe { bytes(receiver) };
    let text = String::from_utf8_lossy(&text);
    let end = text
        .char_indices()
        .position(|(_, c)| !(c.is_ascii_digit() || "+-.eE".contains(c)))
        .unwrap_or(text.len());
    text[..end].parse().unwrap_or(0.0)
}

/// The leading integer, as `-intValue` reads one: skip spaces, take a sign and
/// digits, and answer zero for anything else. Never an error - Foundation's
/// does not have one.
fn parse_leading(bytes: &[u8]) -> Option<i64> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start();
    let end = text
        .char_indices()
        .position(|(i, c)| !(c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+'))))
        .unwrap_or(text.len());
    text[..end].parse().ok()
}

unsafe extern "C" fn last_path_component(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSString`.
    let whole = unsafe { bytes(receiver) };
    let at = whole.iter().rposition(|b| *b == b'/').map_or(0, |i| i + 1);
    slice(&whole, at, whole.len())
}

unsafe extern "C" fn delete_last_path_component(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSString`.
    let whole = unsafe { bytes(receiver) };
    let at = whole.iter().rposition(|b| *b == b'/').unwrap_or(0);
    slice(&whole, 0, at)
}

unsafe extern "C" fn path_extension(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSString`.
    let whole = unsafe { bytes(receiver) };
    let leaf = whole.iter().rposition(|b| *b == b'/').map_or(0, |i| i + 1);
    match whole[leaf..].iter().rposition(|b| *b == b'.') {
        Some(dot) if dot > 0 => slice(&whole, leaf + dot + 1, whole.len()),
        _ => slice(&whole, 0, 0),
    }
}

unsafe extern "C" fn append_path_component(receiver: Id, _sel: Sel, component: Id) -> Id {
    // SAFETY: two `NSString`s, or nil.
    let (mut whole, component) = unsafe { (bytes(receiver), bytes(component)) };
    if !whole.is_empty() && whole.last() != Some(&b'/') {
        whole.push(b'/');
    }
    whole.extend_from_slice(&component);
    let s = corefoundation::string::from_bytes(&whole, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(s) as Id }
}

/// `-componentsSeparatedByString:`.
///
/// An empty separator would split into as many empty pieces as there are bytes,
/// which is not what Apple does - it raises - so the whole string comes back as
/// one component instead, which is the answer a caller can use.
unsafe extern "C" fn components_separated_by_string(receiver: Id, _sel: Sel, separator: Id) -> Id {
    // SAFETY: two `NSString`s, or nil.
    let (whole, separator) = unsafe { (bytes(receiver), bytes(separator)) };
    let mut pieces: Vec<corefoundation::base::CFTypeRef> = Vec::new();
    let mut push = |piece: &[u8]| pieces.push(corefoundation::string::from_bytes(piece, false));
    if separator.is_empty() {
        push(&whole);
    } else {
        let mut at = 0;
        while let Some(found) = whole[at..]
            .windows(separator.len())
            .position(|w| w == separator.as_slice())
        {
            push(&whole[at..at + found]);
            at += found + separator.len();
        }
        push(&whole[at..]);
    }
    let array = corefoundation::collections::new_array(
        &pieces,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // Each piece was retained by the array, so this call's own reference goes.
    for piece in pieces {
        // SAFETY: a string this function made.
        unsafe { api::CFRelease(piece) };
    }
    // SAFETY: a fresh array.
    unsafe { api::CFAutorelease(array) as Id }
}

/// `-stringByDeletingPathExtension`: everything up to the last dot in the last
/// component.
///
/// The dot has to be *in the last component* and not at its start: `a.b/c` keeps
/// its dot, and `/etc/.profile` is a name rather than an extension. Both are the
/// cases a naive search for the last dot gets wrong.
unsafe extern "C" fn delete_path_extension(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSString`, or nil.
    let whole = unsafe { bytes(receiver) };
    let leaf = whole.iter().rposition(|b| *b == b'/').map_or(0, |i| i + 1);
    match whole[leaf..].iter().rposition(|b| *b == b'.') {
        Some(dot) if dot > 0 => slice(&whole, 0, leaf + dot),
        _ => slice(&whole, 0, whole.len()),
    }
}

unsafe extern "C" fn append_path_extension(receiver: Id, _sel: Sel, extension: Id) -> Id {
    // SAFETY: two `NSString`s, or nil.
    let (mut whole, extension) = unsafe { (bytes(receiver), bytes(extension)) };
    if !extension.is_empty() {
        whole.push(b'.');
        whole.extend_from_slice(&extension);
    }
    let s = corefoundation::string::from_bytes(&whole, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(s) as Id }
}

/// `-stringByExpandingTildeInPath`.
///
/// `~` alone and `~/...` are the home directory; `~user` is *someone else's*,
/// which needs the password database and is left alone rather than expanded to
/// this user's - a path that silently became a different account's is worse than
/// one that stayed as it was written.
unsafe extern "C" fn expand_tilde(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSString`, or nil.
    let whole = unsafe { bytes(receiver) };
    let expanded = match whole.first() {
        Some(b'~') if matches!(whole.get(1), None | Some(b'/')) => {
            let mut home = crate::home_directory().into_bytes();
            home.extend_from_slice(&whole[1..]);
            home
        }
        _ => whole,
    };
    let s = corefoundation::string::from_bytes(&expanded, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(s) as Id }
}

/// `-stringByStandardizingPath`: the tilde expanded, `//` collapsed, `.` and
/// `..` resolved lexically.
///
/// Lexically and not through the filesystem, which is what Apple documents for a
/// path that is not absolute or that has symlinks in it. Resolving `..` by
/// asking the filesystem would follow a symlink and give a different answer than
/// the guest's own string arithmetic.
unsafe extern "C" fn standardize_path(receiver: Id, sel: Sel) -> Id {
    // SAFETY: `-stringByExpandingTildeInPath` is `id (id, SEL)`.
    let expanded = unsafe { expand_tilde(receiver, sel) };
    // SAFETY: a string this call just made.
    let whole = unsafe { bytes(expanded) };
    let absolute = whole.first() == Some(&b'/');

    let mut parts: Vec<&[u8]> = Vec::new();
    for part in whole.split(|b| *b == b'/') {
        match part {
            b"" | b"." => {}
            b".." if parts.last().is_some_and(|p| *p != b"..") => {
                parts.pop();
            }
            // A leading `..` on an absolute path is the root's parent, which is
            // the root.
            b".." if absolute => {}
            other => parts.push(other),
        }
    }
    let mut out: Vec<u8> = Vec::with_capacity(whole.len());
    if absolute {
        out.push(b'/');
    }
    out.extend_from_slice(&parts.join(&b'/'));
    if out.is_empty() {
        out.push(b'.');
    }
    let s = corefoundation::string::from_bytes(&out, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(s) as Id }
}

/// `-fileSystemRepresentation`, which is `-UTF8String` on a filesystem whose
/// paths are bytes.
unsafe extern "C" fn file_system_representation(receiver: Id, sel: Sel) -> *const c_char {
    // SAFETY: `-UTF8String` is `const char * (id, SEL)`.
    unsafe { utf8_string(receiver, sel) }
}

unsafe extern "C" fn is_absolute_path(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: an `NSString`, or nil.
    (unsafe { bytes(receiver) }.first() == Some(&b'/')) as u8
}

/// `+stringWithString:` and `-initWithString:`: a copy of another string.
///
/// A copy rather than the same object, because the argument may be mutable and
/// a caller taking a snapshot of one expects the snapshot to stay put.
unsafe extern "C" fn string_with_string(_class: Id, _sel: Sel, other: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let text = unsafe { bytes(other) };
    let s = corefoundation::string::from_bytes(&text, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(s) as Id }
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSString").expect("CoreFoundation registered NSString");
    methods!(class, {
        "length" => length, c"Q16@0:8";
        "UTF8String" => utf8_string, c"*16@0:8";
        "cStringUsingEncoding:" => c_string_using_encoding, c"*24@0:8Q16";
        "getCString:maxLength:encoding:" => get_c_string, c"B40@0:8*16Q24Q32";
        "lengthOfBytesUsingEncoding:" => length_of_bytes, c"Q24@0:8Q16";
        "dataUsingEncoding:" => data_using_encoding, c"@24@0:8Q16";
        "dataUsingEncoding:allowLossyConversion:" => data_using_encoding_lossy, c"@32@0:8Q16B24";
        "maximumLengthOfBytesUsingEncoding:" => length_of_bytes, c"Q24@0:8Q16";
        "isEqualToString:" => is_equal_to_string, c"B24@0:8@16";
        "characterAtIndex:" => character_at_index, c"S24@0:8Q16";
        "getCharacters:range:" => get_characters_range, c"v40@0:8^S16{_NSRange=QQ}24";
        "getCharacters:" => get_characters, c"v24@0:8^S16";
        "description" => description, c"@16@0:8";
        "copy" => copy, c"@16@0:8";
        "copyWithZone:" => copy, c"@24@0:8^v16";
        "mutableCopy" => mutable_copy, c"@16@0:8";
        "rangeOfString:" => range_of_string, c"{_NSRange=QQ}24@0:8@16";
        "containsString:" => contains_string, c"B24@0:8@16";
        "hasPrefix:" => has_prefix, c"B24@0:8@16";
        "hasSuffix:" => has_suffix, c"B24@0:8@16";
        "substringFromIndex:" => substring_from_index, c"@24@0:8Q16";
        "substringToIndex:" => substring_to_index, c"@24@0:8Q16";
        "substringWithRange:" => substring_with_range, c"@32@0:8{_NSRange=QQ}16";
        "componentsSeparatedByString:" => components_separated_by_string, c"@24@0:8@16";
        "lowercaseString" => lowercase, c"@16@0:8";
        "uppercaseString" => uppercase, c"@16@0:8";
        "compare:" => compare, c"q24@0:8@16";
        "caseInsensitiveCompare:" => compare, c"q24@0:8@16";
        "intValue" => int_value, c"i16@0:8";
        "integerValue" => integer_value, c"q16@0:8";
        "longLongValue" => integer_value, c"q16@0:8";
        "doubleValue" => double_value, c"d16@0:8";
        "floatValue" => double_value, c"d16@0:8";
        "lastPathComponent" => last_path_component, c"@16@0:8";
        "stringByDeletingLastPathComponent" => delete_last_path_component, c"@16@0:8";
        "pathExtension" => path_extension, c"@16@0:8";
        "stringByAppendingPathComponent:" => append_path_component, c"@24@0:8@16";
        "stringByDeletingPathExtension" => delete_path_extension, c"@16@0:8";
        "stringByAppendingPathExtension:" => append_path_extension, c"@24@0:8@16";
        "isAbsolutePath" => is_absolute_path, c"B16@0:8";
        "stringByExpandingTildeInPath" => expand_tilde, c"@16@0:8";
        "stringByStandardizingPath" => standardize_path, c"@16@0:8";
        "stringByResolvingSymlinksInPath" => standardize_path, c"@16@0:8";
        "fileSystemRepresentation" => file_system_representation, c"*16@0:8";
        "mutableCopyWithZone:" => mutable_copy, c"@24@0:8^v16";
        "initWithBytes:length:encoding:" => init_with_bytes, c"@40@0:8^v16Q24Q32";
        "initWithData:encoding:" => init_with_data, c"@32@0:8@16Q24";
        "initWithUTF8String:" => init_with_utf8, c"@24@0:8*16";
        "initWithCString:encoding:" => init_with_cstring_encoding, c"@32@0:8*16Q24";
        "initWithFormat:" => init_with_format, c"@24@0:8@16";
        "stringByRemovingPercentEncoding" => removing_percent_encoding, c"@16@0:8";
        "stringByAddingPercentEncodingWithAllowedCharacters:" => adding_percent_encoding, c"@24@0:8@16";
        "stringByTrimmingCharactersInSet:" => trimming_characters, c"@24@0:8@16";
        "stringByAppendingString:" => append_string, c"@24@0:8@16";
        "stringByReplacingOccurrencesOfString:withString:"
            => replacing_occurrences, c"@32@0:8@16@24";
    });
    let mutable = objc::class::lookup(c"NSMutableString").expect("NSMutableString");
    methods!(mutable, {
        "appendString:" => append_string_in_place, c"v24@0:8@16";
        "appendFormat:" => append_format, c"v24@0:8@16";
        "setString:" => set_string, c"v24@0:8@16";
        "init" => init_mutable, c"@16@0:8"
    });
    class_methods!(mutable, {
        "string" => mutable_string, c"@16@0:8";
        "stringWithCapacity:" => mutable_string_with_capacity, c"@24@0:8Q16";
        "new" => mutable_string, c"@16@0:8"
    });

    class_methods!(class, {
        "stringWithUTF8String:" => string_with_utf8, c"@24@0:8*16";
        "stringWithCString:encoding:" => string_with_cstring_encoding, c"@32@0:8*16Q24";
        "stringWithContentsOfURL:encoding:error:" => string_with_contents_of_url,
        c"@40@0:8@16Q24^@32";
        "string" => empty_string, c"@16@0:8";
        "stringWithFormat:" => string_with_format, c"@24@0:8@16";
        "localizedStringWithFormat:" => string_with_format, c"@24@0:8@16";
        "stringWithString:" => string_with_string, c"@24@0:8@16";
        "stringWithCharacters:length:" => string_with_characters, c"@32@0:8^S16Q24";
    });
    methods!(class, {
        "initWithString:" => string_with_string, c"@24@0:8@16";
        // `-initWithCharacters:length:` on a freshly allocated `NSString` answers
        // a new object rather than filling the receiver in. That is what every
        // other initializer here does, and what the immutable storage allows:
        // `corefoundation::string` lays a string out at creation.
        "initWithCharacters:length:" => string_with_characters, c"@32@0:8^S16Q24";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding_handles_utf8_and_rejects_invalid_sequences() {
        crate::bootstrap();
        for (input, expected) in [
            ("", Some("")),
            ("plain+text", Some("plain+text")),
            ("caf%C3%A9/%f0%9f%98%80", Some("café/😀")),
            ("é%20text%2F%25%2520", Some("é text/%%20")),
            ("%00", Some("\0")),
            ("%", None),
            ("%2", None),
            ("%GG", None),
            ("%+1", None),
            ("%FF", None),
            ("%C3", None),
        ] {
            let source = from_str(input);
            // SAFETY: NSString's registered selector returns an object.
            unsafe {
                let send: unsafe extern "C" fn(Id, Sel) -> Id =
                    std::mem::transmute(objc::msg::entry());
                let result = send(source, objc::sel::get("stringByRemovingPercentEncoding"));
                match expected {
                    Some(expected) => {
                        assert!(!result.is_null(), "{input}");
                        assert_eq!(bytes(result), expected.as_bytes(), "{input}");
                    }
                    None => assert!(result.is_null(), "{input}"),
                }
                api::CFRelease(source as CFTypeRef);
            }
        }
    }

    /// Splitting and joining are each other's inverse, which is the property a
    /// path or an argument list is taken apart and put back together on.
    #[test]
    fn separating_and_joining_round_trip() {
        crate::bootstrap();
        let text = from_str("alpha::beta::gamma");
        let separator = from_str("::");
        // SAFETY: `-componentsSeparatedByString:` is `id (id, SEL, id)`.
        let parts = unsafe {
            components_separated_by_string(
                text,
                objc::sel::get("componentsSeparatedByString:"),
                separator,
            )
        };
        // SAFETY: an array this call made.
        let array =
            unsafe { corefoundation::collections::array(parts as corefoundation::base::CFTypeRef) }
                .expect("an array");
        assert_eq!(array.values.len(), 3);

        // Sent rather than called: `-componentsJoinedByString:` is installed by
        // `crate::collections`, and going through dispatch is what says the two
        // halves meet on the same class.
        // SAFETY: it is `id (id, SEL, id)`.
        let joined = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                parts,
                objc::sel::get("componentsJoinedByString:"),
                separator,
            )
        };
        // SAFETY: a string.
        assert_eq!(unsafe { bytes(joined) }, b"alpha::beta::gamma");
    }

    /// The path methods on the two cases a search for the last dot gets wrong.
    #[test]
    fn a_path_extension_is_in_the_last_component_and_is_not_the_whole_name() {
        crate::bootstrap();
        let deleted = |path: &str| {
            let s = from_str(path);
            // SAFETY: `-stringByDeletingPathExtension` is `id (id, SEL)`.
            let out = unsafe {
                delete_path_extension(s, objc::sel::get("stringByDeletingPathExtension"))
            };
            // SAFETY: a string this call made.
            String::from_utf8(unsafe { bytes(out) }).expect("utf8")
        };
        assert_eq!(deleted("/a/b/game.ios"), "/a/b/game");
        // A dot in a *directory* is not this path's extension.
        assert_eq!(deleted("/a.b/c"), "/a.b/c");
        // And a leading dot is a name, not an extension.
        assert_eq!(deleted("/etc/.profile"), "/etc/.profile");
        assert_eq!(deleted("plain"), "plain");
    }

    /// A separator that is not there gives one component, and an empty one gives
    /// the whole string rather than a component per byte.
    #[test]
    fn a_separator_that_is_absent_or_empty_gives_one_component() {
        crate::bootstrap();
        let text = from_str("nothing to split");
        for separator in ["|", ""] {
            // SAFETY: `-componentsSeparatedByString:` is `id (id, SEL, id)`.
            let parts = unsafe {
                components_separated_by_string(
                    text,
                    objc::sel::get("componentsSeparatedByString:"),
                    from_str(separator),
                )
            };
            // SAFETY: an array this call made.
            let array = unsafe {
                corefoundation::collections::array(parts as corefoundation::base::CFTypeRef)
            }
            .expect("an array");
            assert_eq!(array.values.len(), 1, "separator {separator:?}");
        }
    }

    #[test]
    fn url_path_percent_encoding_preserves_path_punctuation() {
        crate::bootstrap();
        let source = from_str("/login/a b?next=x");
        // SAFETY: a nullary class method returning its process-lifetime set.
        let allowed = unsafe {
            crate::characterset::url_path_allowed_character_set(
                std::ptr::null_mut(),
                objc::sel::get("URLPathAllowedCharacterSet"),
            )
        };
        // SAFETY: a string receiver and a live character-set object.
        let encoded = unsafe {
            adding_percent_encoding(
                source,
                objc::sel::get("stringByAddingPercentEncodingWithAllowedCharacters:"),
                allowed,
            )
        };
        // SAFETY: an NSString this call created.
        assert_eq!(unsafe { bytes(encoded) }, b"/login/a%20b%3Fnext=x");
    }
}
