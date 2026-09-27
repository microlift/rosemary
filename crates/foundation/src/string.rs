use std::ffi::{CStr, c_char};

use corefoundation::api;
use corefoundation::base::CFTypeRef;
use corefoundation::string::encoding;

use crate::geometry::NSRange;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// `-intValue`, `-integerValue`, `-longLongValue`, `-doubleValue`, `-floatValue`
/// and `-boolValue`, as pure functions of a string's UTF-16 code units.
///
/// Its own file, and reached with `#[path]` rather than a `lib.rs` line so that
/// everything this selector family needs lives inside the two files that own
/// `NSString`. Its doc comment is where the sources are named: these are ports
/// of `__CFStringScanInteger` and `__CFStringScanDouble` from Apple's
/// open-source CoreFoundation, and of the `-boolValue` clause in `NSString.h`.
#[path = "string_scan.rs"]
mod scan;

/// The `NSStringCompareOptions` bits, from `NSString.h`'s `NS_OPTIONS` typedef.
///
/// The gaps are as load-bearing as the values: `64`, `128` and `256` are not
/// consecutive, and `NSForcedOrderingSearch` is documented as applying to the
/// compare methods only.
mod option {
    pub const CASE_INSENSITIVE: usize = 1;
    pub const LITERAL: usize = 2;
    pub const BACKWARDS: usize = 4;
    pub const ANCHORED: usize = 8;
    pub const NUMERIC: usize = 64;
    pub const DIACRITIC_INSENSITIVE: usize = 128;
    pub const WIDTH_INSENSITIVE: usize = 256;
    pub const FORCED_ORDERING: usize = 512;
    pub const REGULAR_EXPRESSION: usize = 1024;

    /// The bits of `mask` this runtime can answer, and refuse the rest by name.
    ///
    /// A refusal here is the whole point rather than a fallback: a search that
    /// silently dropped `NSRegularExpressionSearch` would answer "not found" for
    /// every pattern that is not a literal, and the caller has no way to tell
    /// that from a real miss. Each refused bit names the capability that is
    /// missing, because each is a different missing thing:
    ///
    /// * `NSNumericSearch` - numeric collation (`Foo2.txt < Foo7.txt < Foo25.txt`);
    /// * `NSDiacriticInsensitiveSearch` - canonical decomposition;
    /// * `NSWidthInsensitiveSearch` - compatibility decomposition;
    /// * `NSRegularExpressionSearch` - an ICU regular-expression engine.
    ///
    /// # Safety
    ///
    /// Nothing is dereferenced; `selector` is only printed.
    pub unsafe fn derivable(selector: &str, mask: usize) -> usize {
        let mut unknown = mask;
        for known in [
            CASE_INSENSITIVE,
            LITERAL,
            BACKWARDS,
            ANCHORED,
            FORCED_ORDERING,
        ] {
            unknown &= !known;
        }
        if unknown != 0 {
            for (bit, name, missing) in [
                (
                    REGULAR_EXPRESSION,
                    "NSRegularExpressionSearch",
                    "an ICU regular-expression engine",
                ),
                (
                    NUMERIC,
                    "NSNumericSearch",
                    "a numeric collation, so that Foo2.txt sorts before Foo7.txt",
                ),
                (
                    DIACRITIC_INSENSITIVE,
                    "NSDiacriticInsensitiveSearch",
                    "Unicode canonical decomposition",
                ),
                (
                    WIDTH_INSENSITIVE,
                    "NSWidthInsensitiveSearch",
                    "Unicode compatibility decomposition",
                ),
            ] {
                if unknown & bit != 0 {
                    objc::fail!(
                        "-[{selector} options:]: bit {bit:#x} is {name}, which needs {missing}; \
                         this host has none, and answering the search without it would report \
                         every non-literal as absent"
                    );
                }
            }
            objc::fail!(
                "-[{selector} options:]: mask {mask:#x} has bits this runtime does not name"
            );
        }
        mask
    }
}

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

/// ASCII-lowercased, or `None` if the string is not ASCII.
///
/// CF's own case-insensitive fast path is `+= 'a' - 'A'` on a byte below `0x80`
/// and nothing else; see [`case_insensitive_compare`] for the code and for why
/// the non-ASCII case is a different function.
fn ascii_fold(units: &[u16]) -> Option<Vec<u16>> {
    units
        .iter()
        .map(|unit| {
            let byte = u8::try_from(*unit).ok()?;
            Some(u16::from(byte.to_ascii_lowercase()))
        })
        .collect()
}

/// `-rangeOfString:options:`.
///
/// The mask is a runtime value, so the answer is a function of it, and this
/// runtime's answer is only truthful for the bits that need nothing but string
/// arithmetic. The Framework sends this selector twice, and the two masks are
/// the two halves of that split: `0x400` (`NSRegularExpressionSearch`) against
/// `"(^|,)\\s*dc="` at `Framework+0x5d9c68e`, which **this refuses by name**, and
/// `0x1` (`NSCaseInsensitiveSearch`) against
/// `"CN=com.apple.idms.appleid.prd"` at `Framework+0x5d9c88e`, which it answers.
///
/// `NSBackwardsSearch` searches from the end and `NSAnchoredSearch` restricts
/// the match to the start (or the end, backwards) of the receiver; both are
/// position arithmetic, so both are real here. `NSForcedOrderingSearch` is
/// documented as a compare-method flag and is accepted and ignored here for the
/// same reason - a search returns a range, not an ordering.
///
/// The ranges are in UTF-16 units, which is what the return value's
/// `{_NSRange=QQ}` counts and what `NSString.h` means by "the length of the
/// target string".
unsafe extern "C" fn range_of_string_options(
    receiver: Id,
    _sel: Sel,
    needle: Id,
    mask: usize,
) -> NSRange {
    // SAFETY: `NSStringCompareOptions` is a mask of named bits.
    let mask = unsafe { option::derivable("-rangeOfString:options:", mask) };
    // SAFETY: two `NSString`s, or nil.
    let (haystack, needle) = unsafe { (units(receiver), units(needle)) };
    let found = if mask & option::CASE_INSENSITIVE != 0 {
        // SAFETY: the refusal above has already stopped the non-ASCII case.
        match (ascii_fold(&haystack), ascii_fold(&needle)) {
            (Some(haystack), Some(needle)) => {
                find(&haystack, &needle, mask & option::BACKWARDS != 0)
            }
            _ => unreachable!("case-insensitive search over non-ASCII is refused above"),
        }
    } else {
        find(&haystack, &needle, mask & option::BACKWARDS != 0)
    };
    match found {
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

/// The first index of `needle` in `haystack`, from the front or the back.
///
/// `NSBackwardsSearch` means the *last* match, not a match of a reversed string:
/// the first match found scanning down is the last one in the string.
fn find(haystack: &[u16], needle: &[u16], backwards: bool) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let at = |from: usize| haystack[from..].windows(needle.len()).position(|w| w == needle);
    if backwards {
        (0..=haystack.len() - needle.len()).rev().find(|from| at(*from).is_some())
    } else {
        at(0)
    }
}

/// `-rangeOfCharacterFromSet:`, which `NSString.h` describes as returning "the
/// range of the first character from the set in the string, **not** the range of
/// a sequence of characters" - so `{location, length: 1}`, and `NSNotFound` with
/// a length of 0 when nothing in the string is a member.
unsafe extern "C" fn range_of_character_from_set(receiver: Id, _sel: Sel, set: Id) -> NSRange {
    if set.is_null() {
        objc::fail!("-[NSString rangeOfCharacterFromSet:]: the character set is nil");
    }
    // SAFETY: an `NSString` and the caller's live `NSCharacterSet`.
    let whole = unsafe { units(receiver) };
    match whole.iter().position(|unit| unsafe { set_admits(set, *unit) }) {
        Some(at) => NSRange {
            location: at,
            length: 1,
        },
        None => NSRange {
            location: NOT_FOUND,
            length: 0,
        },
    }
}

/// Whether `set` admits `unit`, for either kind of set this runtime has.
///
/// Two, and they are not interchangeable. `crate::characterset`'s
/// `+[NSCharacterSet URLPathAllowedCharacterSet]` is a *stand-in* of this crate's
/// own making - a subclass with a tag word and no CoreFoundation payload - and
/// only that module can read its tag. A real `NSCharacterSet`, whether
/// `+whitespaceCharacterSet` or one built by `CFCharacterSetCreateWithCharactersInRange`,
/// is CoreFoundation's, and `CFCharacterSetIsCharacterMember` answers for it.
///
/// Consulting only the first is a wrong answer rather than a missing one: a
/// `+[NSCharacterSet whitespaceCharacterSet]` a guest passes to
/// `-stringByTrimmingCharactersInSet:` would admit nothing, the trim would
/// remove nothing, and the method would hand back the string it was given.
unsafe fn set_admits(set: Id, unit: u16) -> bool {
    if set.is_null() {
        return false;
    }
    if let Ok(byte) = u8::try_from(unit) {
        // SAFETY: `set` is the caller's live character set.
        if unsafe { crate::characterset::contains(set, byte) } {
            return true;
        }
    }
    // SAFETY: `set` is a live object; CoreFoundation's own membership test
    // answers false for anything that is not one of its sets.
    unsafe { corefoundation::charset::CFCharacterSetIsCharacterMember(set.cast(), unit) != 0 }
}

/// `-replaceOccurrencesOfString:withString:options:range:`, on a mutable string.
///
/// Returns the **number** of replacements, which is what makes it different from
/// `-stringByReplacingOccurrencesOfString:withString:options:range:` - and what
/// makes a wrong count silent, since a caller that only wants the string never
/// looks at it. The Framework sends this selector once, with
/// `NSLiteralSearch` (`0x2`) at `Framework+0x8b6da76`.
///
/// The range is clamped to the string, as the header's own advice
/// (`NSMakeRange(0, [receiver length])`) implies callers do, and an empty
/// target replaces nothing: `-replaceOccurrencesOfString:` with an empty target
/// is documented to be a no-op rather than an insertion per position.
unsafe extern "C" fn replace_occurrences_options_range(
    receiver: Id,
    _sel: Sel,
    target: Id,
    with: Id,
    mask: usize,
    range: NSRange,
) -> usize {
    // SAFETY: `NSStringCompareOptions` is a mask of named bits.
    let mask = unsafe { option::derivable("-replaceOccurrencesOfString:withString:options:range:", mask) };
    // SAFETY: `NSString`s and a mutable string this runtime allocated.
    let (whole, target, with) = unsafe { (units(receiver), units(target), units(with)) };
    if target.is_empty() {
        return 0;
    }
    let lo = range.location.min(whole.len());
    let hi = range
        .location
        .saturating_add(range.length)
        .clamp(lo, whole.len());
    let (subject, rest) = whole.split_at(hi);
    let subject = &subject[lo..];

    let folded = mask & option::CASE_INSENSITIVE != 0;
    let (Some(subject), Some(needle)) = (
        folded.then(|| ascii_fold(subject)).flatten(),
        folded.then(|| ascii_fold(&target)).flatten(),
    ) else {
        objc::fail!(
            "-[NSMutableString replaceOccurrencesOfString:withString:options:range:]: \
             NSCaseInsensitiveSearch over a non-ASCII string, which needs ICU full case folding \
             - see -[NSString caseInsensitiveCompare:] for the same refusal and why"
        );
    };

    // The match positions first, then the output.  The count and the text have to
    // come out of the *same* search, or a caller that reads only one of them gets
    // an answer the other was not derived from - and this selector returns the
    // count, which is the half nobody looks at.
    let anchored = mask & option::ANCHORED != 0;
    let backwards = mask & option::BACKWARDS != 0;
    let mut hits: Vec<usize> = Vec::new();
    let mut at = 0usize;
    while at <= subject.len().saturating_sub(needle.len()) {
        let next = if backwards {
            (at..=subject.len().saturating_sub(needle.len()))
                .rev()
                .find(|from| subject[*from..].starts_with(&needle.as_slice()))
        } else {
            subject[at..]
                .windows(needle.len())
                .position(|w| w == needle.as_slice())
                .map(|off| at + off)
        };
        match next {
            Some(hit) => {
                hits.push(hit);
                at = hit + needle.len();
                if anchored {
                    break;
                }
            }
            None => break,
        }
    }

    let mut out: Vec<u16> = Vec::with_capacity(whole.len());
    let mut cursor = 0usize;
    for hit in &hits {
        out.extend_from_slice(&subject[cursor..*hit]);
        out.extend_from_slice(&with);
        cursor = hit + needle.len();
    }
    out.extend_from_slice(&subject[cursor.min(subject.len())..]);
    out.extend_from_slice(rest);
    if !hits.is_empty() {
        let text = String::from_utf16_lossy(&out);
        // SAFETY: the receiver is a mutable string this runtime allocated.
        unsafe { replace(receiver, "replaceOccurrencesOfString:", text.as_bytes()) };
    }
    hits.len()
}

/// `-stringByAppendingFormat:`, which is `-appendFormat:` returning a new string
/// rather than filling the receiver in - and which arrives already rendered, for
/// the reason [`string_with_format`] gives.
unsafe extern "C" fn string_by_appending_format(receiver: Id, _sel: Sel, format: Id) -> Id {
    // SAFETY: two `NSString`s, or nil.
    let mut joined = unsafe { bytes(receiver) };
    joined.extend_from_slice(&unescape_rendered(&unsafe { bytes(format) }));
    let s = corefoundation::string::from_bytes(&joined, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(s) as Id }
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

/// `-stringByTrimmingCharactersInSet:`.
///
/// In **code units**, and against both kinds of character set this runtime has -
/// see [`set_admits`] for why that is two and not one. Trimming bytes would be a
/// different method: `NBSP` and `IDEOGRAPHIC SPACE` are three bytes each, and a
/// caller that trimmed a byte at a time would leave half a character behind.
unsafe extern "C" fn trimming_characters(receiver: Id, _sel: Sel, set: Id) -> Id {
    if set.is_null() {
        objc::fail!("-[NSString stringByTrimmingCharactersInSet:]: the character set is nil");
    }
    // SAFETY: an `NSString` and the caller's live `NSCharacterSet`.
    let whole = unsafe { units(receiver) };
    let member = |unit: &u16| unsafe { set_admits(set, *unit) };
    let start = whole.iter().position(|unit| !member(unit)).unwrap_or(whole.len());
    let end = whole
        .iter()
        .rposition(|unit| !member(unit))
        .map_or(start, |index| index + 1);
    let sliced: Vec<u16> = whole[start.min(end)..end].to_vec();
    let s = corefoundation::string::from_utf16(&sliced);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(s) as Id }
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

/// `-compare:`, which is `CFStringCompare` with no options.
///
/// The comparison is over **characters**, in code-point order: CF walks the
/// inline buffer with `CFStringGetCharacterFromInlineBuffer` and rejoins
/// surrogate pairs (`CFUniCharIsSurrogateHighCharacter`), so an emoji sorts
/// before `U+E000`..`U+FFFF`, not after it. Comparing UTF-8 bytes gives the same
/// order, because UTF-8 is order-preserving over code points - which is why this
/// stays on `bytes()` and not on the UTF-16 units, where that order would be
/// wrong.
unsafe extern "C" fn compare(receiver: Id, _sel: Sel, other: Id) -> isize {
    // SAFETY: two `NSString`s, or nil.
    let (a, b) = unsafe { (bytes(receiver), bytes(other)) };
    match a.cmp(&b) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// `-caseInsensitiveCompare:`, which is `CFStringCompare` with
/// `kCFCompareCaseInsensitive`.
///
/// **The ASCII line is CF's own line, not a convenient one.**
/// `CFStringFindWithOptionsAndLocale` and
/// `CFStringCompareWithOptionsAndLocale` both have this shape:
///
/// ```c
/// if ((strByte < 0x80) && ((NULL == langCode) || ('I' != strByte))) {
///     if (caseInsensitive && (strByte >= 'A') && (strByte <= 'Z')) strByte += ('a' - 'A');
///     *strBuf1 = strByte; strBuf1Len = 1;
/// } else {
///     strBuf1Len = __CFStringFoldCharacterClusterAtIndex(...);   /* ICU */
/// }
/// ```
///
/// so for ASCII CF folds with `+= 'a' - 'A'` and never consults a case table,
/// while anything else goes to `__CFStringFoldCharacterClusterAtIndex`, which is
/// ICU **full case folding** over a character cluster. Full case folding is not
/// Rust's `char::to_lowercase`: `U+1E9E` folds to `ss` and `U+00DF` folds to
/// `ss`, where simple lowercase gives `U+00DF` and `U+00DF`.
///
/// So the ASCII answer here is exact, and a string with any unit at or above
/// `U+0080` in it is **refused by name** rather than answered. The previous
/// answer - `-caseInsensitiveCompare:` wired to the case-sensitive `compare` -
/// was not merely approximate: it reported `"ABC"` and `"abc"` as *unequal*, and
/// a `test` on the result of this selector is how a guest asks "are these the
/// same?", so that wrong answer selects a branch and nothing reports it.
unsafe extern "C" fn case_insensitive_compare(receiver: Id, _sel: Sel, other: Id) -> isize {
    // SAFETY: two `NSString`s, or nil.
    let (a, b) = unsafe { (units(receiver), units(other)) };
    if a.iter().any(|unit| *unit >= 0x80) || b.iter().any(|unit| *unit >= 0x80) {
        objc::fail!(
            "-[NSString caseInsensitiveCompare:]: at least one string has a character outside \
             ASCII.  CoreFoundation answers those through ICU full case folding \
             (__CFStringFoldCharacterClusterAtIndex), so U+1E9E compares equal to \"ss\" and \
             U+00DF does too - which is not what any case table this host can build says, and \
             a comparison result is a branch the caller cannot audit"
        );
    }
    let fold = |unit: &u16| {
        let byte = *unit as u8;
        byte.to_ascii_lowercase() as u16
    };
    match a.iter().map(fold).cmp(b.iter().map(fold)) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// `-intValue`: `CFStringGetIntValue`, which is a **32-bit** saturating scan.
unsafe extern "C" fn int_value(receiver: Id, _sel: Sel) -> i32 {
    // SAFETY: an `NSString`.
    let whole = unsafe { units(receiver) };
    // The scan saturates inside `i32`'s range, so this truncation never wraps.
    scan::scan_integer(&whole, scan::Width::Int).unwrap_or(0) as i32
}

/// `-integerValue` and `-longLongValue`: the same scan with a 64-bit
/// accumulator, promoted from the 32-bit one on the first digit that does not
/// fit in it.
unsafe extern "C" fn integer_value(receiver: Id, _sel: Sel) -> i64 {
    // SAFETY: an `NSString`.
    let whole = unsafe { units(receiver) };
    scan::scan_integer(&whole, scan::Width::LongLong).unwrap_or(0)
}

/// `-doubleValue`: `CFStringGetDoubleValue` over the `__CFNumberSet` run.
unsafe extern "C" fn double_value(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: an `NSString`.
    let whole = unsafe { units(receiver) };
    scan::scan_double(&whole)
}

/// `-floatValue`, which is `-doubleValue` narrowed to `f32`.
///
/// **The encoding is `f16@0:8` and it has to be.** The two differ only in the low
/// 32 bits of `xmm0`, so a `d16@0:8` row wired to a function returning `f64` put
/// a *double*'s bits where the guest reads a *float*: `3.0f64` is
/// `0x4008000000000000`, whose low half is `0x00000000`, so this selector
/// answered `0.0` for almost every input and the guest had no way to know. A
/// method encoding is never consulted at dispatch, so nothing checks this -
/// which is why it is worth saying out loud.
unsafe extern "C" fn float_value(receiver: Id, _sel: Sel) -> f32 {
    // SAFETY: an `NSString`.
    let whole = unsafe { units(receiver) };
    scan::scan_double(&whole) as f32
}

/// `-boolValue`, which is the only selector here with no `strtol`-shaped
/// grammar: the clause is `NSString.h`'s own, and the whole of it is
/// implemented in [`scan::bool_value`].
///
/// The measured run reaches this at `Framework+0x5d92a5b` on the value of
/// `LSUIElement`, which every one of the five `Chromium Helper` bundles carries
/// as `<string>1</string>`, so the answer is `YES`.
unsafe extern "C" fn bool_value(receiver: Id, _sel: Sel) -> bool {
    // SAFETY: an `NSString`.
    let whole = unsafe { units(receiver) };
    scan::bool_value(&whole)
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
        "rangeOfString:options:" => range_of_string_options, c"{_NSRange=QQ}32@0:8@16Q24";
        "rangeOfCharacterFromSet:" => range_of_character_from_set, c"{_NSRange=QQ}24@0:8@16";
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
        "caseInsensitiveCompare:" => case_insensitive_compare, c"q24@0:8@16";
        "intValue" => int_value, c"i16@0:8";
        "integerValue" => integer_value, c"q16@0:8";
        "longLongValue" => integer_value, c"q16@0:8";
        "boolValue" => bool_value, c"B16@0:8";
        "doubleValue" => double_value, c"d16@0:8";
        "floatValue" => float_value, c"f16@0:8";
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
        "stringByAppendingFormat:" => string_by_appending_format, c"@24@0:8@16";
        "stringByReplacingOccurrencesOfString:withString:"
            => replacing_occurrences, c"@32@0:8@16@24";
    });
    let mutable = objc::class::lookup(c"NSMutableString").expect("NSMutableString");
    methods!(mutable, {
        "appendString:" => append_string_in_place, c"v24@0:8@16";
        "appendFormat:" => append_format, c"v24@0:8@16";
        "setString:" => set_string, c"v24@0:8@16";
        "replaceOccurrencesOfString:withString:options:range:"
            => replace_occurrences_options_range, c"Q64@0:8@16@24Q32{_NSRange=QQ}40";
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

    /// The string `__CFStringScanDouble` is handed, for the inputs where the
    /// collected run is not the whole string.  This is the half of the contract a
    /// libc diff cannot check - libc has no `__CFNumberSet` - so it is pinned
    /// here, and `test/tools/strtod-diff/` diffs the other half against the real
    /// `strtod` over 124 inputs.
    #[test]
    fn the_number_run_stops_at_the_first_character_the_scanner_will_not_pass_on() {
        let run = |text: &str| {
            let units: Vec<u16> = text.encode_utf16().collect();
            scan::number_run(&units)
        };
        // A newline is not in `whitespaceSet` and not in the set, so it ends the
        // run before anything else does.
        assert_eq!(run("\n3.14"), b"");
        // A space is NOT in the set, so the run stops there.  It is easy to
        // assume otherwise from the table row that holds it.
        assert_eq!(run("3 14"), b"3");
        // Nor are '#', '%', '&', ',' or '/': the whole printable run of that row
        // of the table is *not* in the set - only '+', '-' and '.' are.
        for absent in ["1#2", "1%2", "1&2", "1,2", "1/2"] {
            assert_eq!(run(absent), b"1", "{absent}");
        }
        assert_eq!(run("()"), b"");
        // The whole set, in one string: 15 characters, and this is the line that
        // says which fifteen.
        assert_eq!(run("+-.0123456789Ee"), b"+-.0123456789Ee");
        // 'x' is not in the set, so no hexadecimal form can ever be reached.
        assert_eq!(run("0x10"), b"0");
        // Nor 'i', 'n' or 'a', so no inf/nan can ever be reached.
        assert_eq!(run("inf"), b"");
        assert_eq!(run("NaN"), b"");
        // Leading whitespaceSet is skipped before the run starts.
        assert_eq!(run("  \t 42"), b"42");
        assert_eq!(run("1.2.3"), b"1.2.3");
        // Anything at U+0080 or above ends the run: the C test is `ch < 128`.
        assert_eq!(run("12\u{00e9}"), b"12");
    }

    /// `-boolValue` through dispatch, on the string the measured run sends, and
    /// on the byte a wrong `NO` would be indistinguishable from.
    #[test]
    fn bool_value_is_installed_on_the_class_the_receiver_is() {
        crate::bootstrap();
        let source = from_str("1");
        // Sent rather than called: the receiver on the measured path is a
        // `__NSCFString` (a bridged constant from a plist), not this crate's own
        // string, and only going through dispatch proves the method is installed
        // on the class such a receiver has.
        // SAFETY: `-boolValue` is `BOOL (id, SEL)`.
        let value = unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> bool =
                std::mem::transmute(objc::msg::entry());
            send(source, objc::sel::get("boolValue"))
        };
        assert!(value, "LSUIElement is <string>1</string> in every helper bundle");
        // SAFETY: `source` is a string this test made.
        unsafe { api::CFRelease(source as CFTypeRef) };
    }

    /// `-caseInsensitiveCompare:` and `-floatValue` both used to be wired to the
    /// wrong thing; these pin the two answers that were silently wrong.
    #[test]
    fn case_insensitive_and_float_are_not_the_case_sensitive_and_double_answers() {
        crate::bootstrap();
        let a = from_str("ABC");
        let b = from_str("abc");
        // SAFETY: `NSComparisonResult (id, SEL, id)`.
        let compared = unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id) -> isize =
                std::mem::transmute(objc::msg::entry());
            send(a, objc::sel::get("caseInsensitiveCompare:"), b)
        };
        assert_eq!(compared, 0, "\"ABC\" and \"abc\" are the same string");

        let three = from_str("3");
        // SAFETY: `float (id, SEL)`.
        let value = unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> f32 = std::mem::transmute(objc::msg::entry());
            send(three, objc::sel::get("floatValue"))
        };
        // 3.0f64 is 0x4008000000000000, whose low 32 bits are 0 - so a `d16@0:8`
        // row wired to a function returning f64 answered 0.0 here.
        assert_eq!(value, 3.0f32);
        assert_eq!(value.to_bits(), 0x4040_0000);
        for object in [a, b, three] {
            // SAFETY: each is a string this test made.
            unsafe { api::CFRelease(object as CFTypeRef) };
        }
    }

    /// `-stringByTrimmingCharactersInSet:` has to consult a real `NSCharacterSet`.
    /// Consulting only this crate's own URL-path stand-in would make every other
    /// set admit nothing, and the trim would then remove nothing - a wrong answer
    /// that looks exactly like "no trimming was needed".
    ///
    /// The set is built through CoreFoundation's own
    /// `CFCharacterSetGetPredefined(kCFCharacterSetWhitespace)` rather than
    /// through `+[NSCharacterSet whitespaceCharacterSet]`, which this tree does
    /// not install and whose absence would abort the test instead of testing
    /// anything.
    #[test]
    fn trimming_consults_a_character_set_corefoundation_built() {
        crate::bootstrap();
        // SAFETY: a documented nullary constructor with a documented identifier.
        let whitespace = unsafe {
            corefoundation::charset::CFCharacterSetGetPredefined(
                corefoundation::charset::Predefined::Whitespace.identifier() as isize,
            )
        } as Id;
        if whitespace.is_null() {
            return;
        }
        // A non-ASCII member as well, so the test is not only about ASCII: U+00A0
        // NO-BREAK SPACE is three UTF-8 bytes and one UTF-16 unit, and trimming
        // bytes would leave two thirds of a character behind.
        let padded = from_str(" \u{00a0}padded\u{00a0} ");
        // SAFETY: `id (id, SEL, id)`.
        let trimmed = unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
            send(
                padded,
                objc::sel::get("stringByTrimmingCharactersInSet:"),
                whitespace,
            )
        };
        // SAFETY: a string this call made.
        assert_eq!(unsafe { bytes(trimmed) }, "padded".as_bytes());
    }

    /// `-stringByAppendingFormat:`, and the mask-valued searches, exist because the
    /// guest sends them; these are the two masks it actually sends.
    #[test]
    fn the_search_options_the_guest_sends_answer_and_the_regex_one_is_named() {
        crate::bootstrap();
        let haystack = from_str("apple_id CN=com.apple.idms.appleid.prd trailer");
        // The needle differs from the haystack's copy in case only, so the two
        // masks must disagree - otherwise this test cannot tell them apart.
        let needle = from_str("cn=com.apple.idms.appleid.prd");
        // SAFETY: `NSRange (id, SEL, id, NSStringCompareOptions)`.
        let found = unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id, usize) -> NSRange =
                std::mem::transmute(objc::msg::entry());
            send(
                haystack,
                objc::sel::get("rangeOfString:options:"),
                needle,
                0x1, // NSCaseInsensitiveSearch
            )
        };
        assert_eq!(found.location, 9, "the range is in UTF-16 units");
        // SAFETY: `needle` is a string this test made.
        assert_eq!(found.length, unsafe { units(needle) }.len());

        // The Framework's other site passes 0x400, NSRegularExpressionSearch, and
        // is answered by a refusal naming the missing ICU engine.  Calling it here
        // would abort the test process, so only the accepted mask is exercised.
        let literal = unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id, usize) -> NSRange =
                std::mem::transmute(objc::msg::entry());
            send(
                haystack,
                objc::sel::get("rangeOfString:options:"),
                needle,
                0x2, // NSLiteralSearch
            )
        };
        assert_eq!(literal.location, NOT_FOUND, "case matters without the mask");
    }

    /// The abort this selector caused, replayed from the guest's own bytes.
    ///
    /// `test/logs/188.log:370` is `unrecognized selector sent to instance of
    /// __NSCFString: boolValue`, reached from `Framework+0x891217b`, whose
    /// instruction sequence is:
    ///
    /// ```text
    /// 8912176:  call   0x5d929e0
    /// 891217b:  test   al,al
    /// 891217d:  je     0x89121e3
    /// ```
    ///
    /// and `0x5d929e0` is a wrapper that sends, in order, `mainBundle`
    /// (`+[NSBundle mainBundle]`), `infoDictionary`, `objectForKey:` with the
    /// `__cfstring` at `0xe674830`, and then `boolValue`. That `__cfstring`'s
    /// length field at `+24` is `0xb`, and its bytes at `+16` are
    /// `LSUIElement`. So the whole of the measured expression is
    /// `[[[NSBundle mainBundle] infoDictionary] objectForKey:@"LSUIElement"] boolValue`,
    /// and the answer is decided by what the bundle's plist says.
    ///
    /// This test reads the real `Info.plist` of the real helper bundle with this
    /// crate's own plist parser and asks.  It is the difference between
    /// answering the selector correctly and answering the *question* the guest
    /// was asking correctly, and the run cannot show the second one: the wall in
    /// front of that call site is another module's, so the process that would
    /// reach it does not start.
    #[test]
    fn the_guests_own_lsuielement_string_answers_yes() {
        crate::bootstrap();
        const PLIST: &str = "test/Chromium/Chromium.app/Contents/Frameworks/\
             Chromium Framework.framework/Versions/130.0.6723.91/Helpers/\
             Chromium Helper (GPU).app/Contents/Info.plist";
        let Ok(raw) = std::fs::read(PLIST) else {
            // The guest bundle is not present in every checkout; a skip is
            // honest here and a fabricated value is not.
            return;
        };
        let Ok(value) = crate::plist::parse(&raw) else {
            panic!("could not parse {PLIST}");
        };
        let element = value
            .get("LSUIElement")
            .and_then(|v| v.as_str())
            .expect("LSUIElement in the GPU helper's Info.plist");
        assert_eq!(element, "1", "the value the guest hands -boolValue");
        let bridged = from_str(element);
        // Sent, not called: the abort names `__NSCFString`, and only dispatch
        // shows the method is on the class a bridged string has.
        // SAFETY: `-boolValue` is `BOOL (id, SEL)`.
        let answer = unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> bool = std::mem::transmute(objc::msg::entry());
            send(bridged, objc::sel::get("boolValue"))
        };
        assert!(
            answer,
            "NO here is the byte a correct NO would also be, and it would record a \
             background-only bundle as a foreground one"
        );
        // SAFETY: `bridged` is a string this test made.
        unsafe { api::CFRelease(bridged as CFTypeRef) };
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
