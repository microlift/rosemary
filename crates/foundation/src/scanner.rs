//! `NSScanner`: a string, a position in it, and a set of characters to skip
//! before every scan.
//!
//! Every `scan...` method here follows the same shape: skip
//! `charactersToBeSkipped`, try the match, and either commit `scanLocation`
//! past it and answer `true`, or leave `scanLocation` exactly where it was
//! and answer `false` - including the skip, which a failed match never
//! spends. `-isAtEnd` answers the same way a scan attempt would: it skips
//! first, so a string with nothing left but whitespace reads as finished.

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use corefoundation::base::CFTypeRef;

use crate::characterset::contains_char;
use crate::define::{class_methods, methods};

struct Body {
    #[allow(dead_code)]
    isa: *const Class,
    contents: *mut Contents,
}

struct Contents {
    /// Retained. The text is read once, at `-initWithString:`, into `units`;
    /// Apple does not document what a scan sees of a string mutated under
    /// it, and reading a stable snapshot is the simplest answer that is
    /// never wrong about *something*.
    string: Id,
    units: Vec<u16>,
    /// In UTF-16 units, the same count `NSRange` and `-[NSString length]` use.
    location: usize,
    /// Retained, or null meaning "skip nothing" - which is also what
    /// [`contains_char`] answers for a null set, so every skip loop below
    /// needs no separate null check.
    skip_set: Id,
    case_sensitive: bool,
}

impl Drop for Contents {
    fn drop(&mut self) {
        objc::arc::release(self.string);
        if !self.skip_set.is_null() {
            objc::arc::release(self.skip_set);
        }
    }
}

fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let p = *CLASS.get_or_init(|| {
        let class = corefoundation::base::register(c"NSScanner", None, size_of::<Body>(), false);
        unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
            // SAFETY: reached with the last reference gone.
            unsafe {
                let body = receiver as *mut Body;
                if !(*body).contents.is_null() {
                    drop(Box::from_raw((*body).contents));
                    (*body).contents = std::ptr::null_mut();
                }
                objc::arc::finalize(receiver);
            }
        }
        class.replace_method(
            objc::sel::get("dealloc"),
            dealloc as *const std::ffi::c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(p as *const Class).expect("a scanner class") }
}

/// # Safety
///
/// `receiver` must be null or an object this runtime allocated.
unsafe fn contents<'a>(receiver: Id) -> Option<&'a mut Contents> {
    if !unsafe { corefoundation::base::is_kind_of(receiver as CFTypeRef, class()) } {
        return None;
    }
    // SAFETY: an instance of `class()` is a `Body`.
    unsafe {
        let body = receiver as *mut Body;
        (!(*body).contents.is_null()).then(|| &mut *(*body).contents)
    }
}

fn make(string: Id) -> Id {
    // SAFETY: `string` is the caller's, null or live either way.
    let units = unsafe { corefoundation::string::as_string(string as CFTypeRef) }
        .map(|s| s.utf16())
        .unwrap_or_default();
    // SAFETY: `class()`'s instances are `Body`.
    let body: *mut Body = unsafe { corefoundation::base::allocate(class()) };
    if body.is_null() {
        return body as Id;
    }
    let skip_set = crate::characterset::whitespace_and_newline();
    // SAFETY: a live instance whose second word is ours to fill in.
    unsafe {
        (*body).contents = Box::into_raw(Box::new(Contents {
            string: objc::arc::retain(string),
            units,
            location: 0,
            skip_set: objc::arc::retain(skip_set),
            case_sensitive: false,
        }));
    }
    body as Id
}

unsafe extern "C" fn init_with_string(_class: Id, _sel: Sel, string: Id) -> Id {
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(make(string) as CFTypeRef) as Id }
}

unsafe extern "C" fn scanner_with_string(class_object: Id, sel: Sel, string: Id) -> Id {
    unsafe { init_with_string(class_object, sel, string) }
}

unsafe extern "C" fn get_string(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSScanner`.
    unsafe { contents(receiver) }.map_or(std::ptr::null_mut(), |c| c.string)
}

unsafe extern "C" fn get_scan_location(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: an `NSScanner`.
    unsafe { contents(receiver) }.map_or(0, |c| c.location)
}

unsafe extern "C" fn set_scan_location(receiver: Id, _sel: Sel, location: usize) {
    // SAFETY: an `NSScanner`.
    if let Some(c) = unsafe { contents(receiver) } {
        // Clamped rather than refused: a location past the end is a location
        // at the end, as `-isAtEnd` is happy to answer right away.
        c.location = location.min(c.units.len());
    }
}

unsafe extern "C" fn get_characters_to_be_skipped(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSScanner`.
    unsafe { contents(receiver) }.map_or(std::ptr::null_mut(), |c| c.skip_set)
}

unsafe extern "C" fn set_characters_to_be_skipped(receiver: Id, _sel: Sel, set: Id) {
    // SAFETY: an `NSScanner`.
    let Some(c) = (unsafe { contents(receiver) }) else {
        return;
    };
    if !c.skip_set.is_null() {
        objc::arc::release(c.skip_set);
    }
    c.skip_set = if set.is_null() {
        set
    } else {
        objc::arc::retain(set)
    };
}

unsafe extern "C" fn get_case_sensitive(receiver: Id, _sel: Sel) -> bool {
    // SAFETY: an `NSScanner`.
    unsafe { contents(receiver) }.is_some_and(|c| c.case_sensitive)
}

unsafe extern "C" fn set_case_sensitive(receiver: Id, _sel: Sel, value: bool) {
    // SAFETY: an `NSScanner`.
    if let Some(c) = unsafe { contents(receiver) } {
        c.case_sensitive = value;
    }
}

/// The `char` at UTF-16 index `at` in `units`, and how many units it took -
/// two for a surrogate pair, one otherwise. An unpaired surrogate reads as
/// the replacement character and still advances by one, so a malformed
/// string scans to its end rather than stalling on it.
fn char_at(units: &[u16], at: usize) -> Option<(char, usize)> {
    let first = *units.get(at)?;
    match char::decode_utf16(units[at..].iter().copied()).next()? {
        Ok(c) => Some((c, c.len_utf16())),
        Err(_) => {
            let _ = first;
            Some((char::REPLACEMENT_CHARACTER, 1))
        }
    }
}

/// `at`, advanced past every character `charactersToBeSkipped` holds -
/// [`contains_char`] of a null set is always `false`, so a null `skip_set`
/// already means "skip nothing" with no separate case here.
///
/// # Safety
///
/// `skip_set` is null or a live `NSCharacterSet`.
unsafe fn skip(units: &[u16], mut at: usize, skip_set: Id) -> usize {
    while let Some((c, width)) = char_at(units, at) {
        // SAFETY: the caller's obligation.
        if !unsafe { contains_char(skip_set, c) } {
            break;
        }
        at += width;
    }
    at
}

/// `at`, advanced while `predicate` of the character there holds - the one
/// loop [`scan_characters_from_set`] and [`scan_up_to_characters_from_set`]
/// both are.
fn scan_while(units: &[u16], mut at: usize, predicate: impl Fn(char) -> bool) -> usize {
    while let Some((c, width)) = char_at(units, at) {
        if !predicate(c) {
            break;
        }
        at += width;
    }
    at
}

/// Whether `target` matches `units` starting at `at`, honoring
/// `case_sensitive` - simple Unicode case folding through Rust's own
/// `char::to_lowercase`, not a locale-aware one.
///
/// FIXME (ACCURACY): a fold that changes how many UTF-16 units the text
/// takes (German `ß` against `ss`, Turkish dotted İ) compares unequal
/// instead of matching, same as a byte-length mismatch would.
fn matches_at(units: &[u16], at: usize, target: &[u16], case_sensitive: bool) -> bool {
    let Some(candidate) = units.get(at..at + target.len()) else {
        return false;
    };
    if case_sensitive {
        return candidate == target;
    }
    String::from_utf16_lossy(candidate).to_lowercase()
        == String::from_utf16_lossy(target).to_lowercase()
}

/// `-scanString:intoString:`'s search: the first position at or after `at`
/// where `target` matches, or `None` if it never does before the string
/// ends.
fn find(units: &[u16], at: usize, target: &[u16], case_sensitive: bool) -> Option<usize> {
    (at..=units.len()).find(|&pos| matches_at(units, pos, target, case_sensitive))
}

/// The ASCII units from `at`, as bytes - every numeric grammar below is pure
/// ASCII, so the first unit of 0x80 or above ends it exactly where a decoded
/// character would already have failed to match.
fn ascii_run(units: &[u16], at: usize) -> Vec<u8> {
    units[at.min(units.len())..]
        .iter()
        .take_while(|&&u| u < 0x80)
        .map(|&u| u as u8)
        .collect()
}

/// An optional `+`/`-`, then one or more decimal digits - the longest prefix
/// of `bytes` matching that grammar, and whether it was negative. `None` for
/// no digits at all, sign included or not.
fn signed_digits(bytes: &[u8]) -> Option<(usize, bool, &[u8])> {
    let mut at = 0;
    let negative = match bytes.first() {
        Some(b'-') => {
            at += 1;
            true
        }
        Some(b'+') => {
            at += 1;
            false
        }
        _ => false,
    };
    let digits_start = at;
    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
    }
    (at > digits_start).then_some((at, negative, &bytes[digits_start..at]))
}

/// One or more decimal digits, with no sign - what every unsigned scanner
/// accepts.
fn unsigned_digits(bytes: &[u8]) -> Option<(usize, &[u8])> {
    let mut at = 0;
    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
    }
    (at > 0).then_some((at, &bytes[..at]))
}

/// An optional `0x`/`0X`, then one or more hex digits - the radix prefix is
/// accepted but never required, as Apple's own scanner takes it.
fn hex_digits(bytes: &[u8]) -> Option<(usize, &[u8])> {
    let prefixed = bytes.len() >= 2 && bytes[0] == b'0' && matches!(bytes[1], b'x' | b'X');
    let start = if prefixed { 2 } else { 0 };
    let mut at = start;
    while bytes.get(at).is_some_and(u8::is_ascii_hexdigit) {
        at += 1;
    }
    (at > start).then_some((at, &bytes[start..at]))
}

/// The longest prefix of `bytes` that is a C-style floating-point literal -
/// sign, digits, an optional `.` and more digits, and an optional signed
/// exponent - or `0` for none. A dangling `e`/`E` with no exponent digits
/// after it is dropped rather than included.
fn float_token_len(bytes: &[u8]) -> usize {
    let mut at = 0;
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        at += 1;
    }
    let mut has_digits = false;
    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
        has_digits = true;
    }
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
            has_digits = true;
        }
    }
    if !has_digits {
        return 0;
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        let mut past = at + 1;
        if matches!(bytes.get(past), Some(b'+' | b'-')) {
            past += 1;
        }
        let exponent_digits = past;
        while bytes.get(past).is_some_and(u8::is_ascii_digit) {
            past += 1;
        }
        if past > exponent_digits {
            at = past;
        }
    }
    at
}

/// Accumulate `digits` as base-10 (`radix` 10) or base-16 (`radix` 16),
/// saturating at `u64::MAX` rather than wrapping - the overflow behavior
/// Apple documents for every integer scanner here.
fn accumulate(digits: &[u8], radix: u32) -> u64 {
    let mut value = 0u64;
    for &byte in digits {
        let digit = u64::from((byte as char).to_digit(radix).unwrap_or(0));
        value = value.saturating_mul(u64::from(radix)).saturating_add(digit);
    }
    value
}

/// A signed value from `magnitude`/`negative`, saturating at the signed
/// range's own ends instead of wrapping past them.
fn signed_from_magnitude(magnitude: u64, negative: bool) -> i64 {
    if negative {
        magnitude
            .try_into()
            .map(i64::saturating_neg)
            .unwrap_or(i64::MIN)
    } else {
        magnitude.try_into().unwrap_or(i64::MAX)
    }
}

unsafe extern "C" fn is_at_end(receiver: Id, _sel: Sel) -> bool {
    // SAFETY: an `NSScanner`.
    let Some(c) = (unsafe { contents(receiver) }) else {
        return true;
    };
    // SAFETY: `c.skip_set` is null or live.
    (unsafe { skip(&c.units, c.location, c.skip_set) }) >= c.units.len()
}

unsafe extern "C" fn scan_string_into_string(
    receiver: Id,
    _sel: Sel,
    target: Id,
    out: *mut Id,
) -> bool {
    // SAFETY: an `NSScanner`.
    let Some(c) = (unsafe { contents(receiver) }) else {
        return false;
    };
    // SAFETY: `target` is the caller's, null or live either way.
    let target_units = unsafe { corefoundation::string::as_string(target as CFTypeRef) }
        .map(|s| s.utf16())
        .unwrap_or_default();
    // SAFETY: `c.skip_set` is null or live.
    let start = unsafe { skip(&c.units, c.location, c.skip_set) };
    if !matches_at(&c.units, start, &target_units, c.case_sensitive) {
        return false;
    }
    let end = start + target_units.len();
    if !out.is_null() {
        let scanned = corefoundation::string::from_utf16(&c.units[start..end]);
        // SAFETY: the caller's out-parameter.
        unsafe { *out = corefoundation::api::CFAutorelease(scanned) as Id };
    }
    c.location = end;
    true
}

unsafe extern "C" fn scan_up_to_string_into_string(
    receiver: Id,
    _sel: Sel,
    target: Id,
    out: *mut Id,
) -> bool {
    // SAFETY: an `NSScanner`.
    let Some(c) = (unsafe { contents(receiver) }) else {
        return false;
    };
    // SAFETY: `target` is the caller's.
    let target_units = unsafe { corefoundation::string::as_string(target as CFTypeRef) }
        .map(|s| s.utf16())
        .unwrap_or_default();
    // SAFETY: `c.skip_set` is null or live.
    let start = unsafe { skip(&c.units, c.location, c.skip_set) };
    // Not found anywhere: scan to the end of the string, same as Apple
    // documents. Found right here: nothing was scanned, so this fails.
    let end = find(&c.units, start, &target_units, c.case_sensitive).unwrap_or(c.units.len());
    if end == start {
        return false;
    }
    if !out.is_null() {
        let scanned = corefoundation::string::from_utf16(&c.units[start..end]);
        // SAFETY: the caller's out-parameter.
        unsafe { *out = corefoundation::api::CFAutorelease(scanned) as Id };
    }
    c.location = end;
    true
}

unsafe extern "C" fn scan_characters_from_set_into_string(
    receiver: Id,
    _sel: Sel,
    set: Id,
    out: *mut Id,
) -> bool {
    // SAFETY: an `NSScanner`.
    let Some(c) = (unsafe { contents(receiver) }) else {
        return false;
    };
    // SAFETY: `c.skip_set` is null or live.
    let start = unsafe { skip(&c.units, c.location, c.skip_set) };
    // SAFETY: `set` is the caller's, null or live either way.
    let end = scan_while(&c.units, start, |ch| unsafe { contains_char(set, ch) });
    if end == start {
        return false;
    }
    if !out.is_null() {
        let scanned = corefoundation::string::from_utf16(&c.units[start..end]);
        // SAFETY: the caller's out-parameter.
        unsafe { *out = corefoundation::api::CFAutorelease(scanned) as Id };
    }
    c.location = end;
    true
}

unsafe extern "C" fn scan_up_to_characters_from_set_into_string(
    receiver: Id,
    _sel: Sel,
    set: Id,
    out: *mut Id,
) -> bool {
    // SAFETY: an `NSScanner`.
    let Some(c) = (unsafe { contents(receiver) }) else {
        return false;
    };
    // SAFETY: `c.skip_set` is null or live.
    let start = unsafe { skip(&c.units, c.location, c.skip_set) };
    // SAFETY: `set` is the caller's, null or live either way.
    let end = scan_while(&c.units, start, |ch| unsafe { !contains_char(set, ch) });
    if end == start {
        return false;
    }
    if !out.is_null() {
        let scanned = corefoundation::string::from_utf16(&c.units[start..end]);
        // SAFETY: the caller's out-parameter.
        unsafe { *out = corefoundation::api::CFAutorelease(scanned) as Id };
    }
    c.location = end;
    true
}

/// The shared body of every signed-decimal scanner: skip, find the longest
/// `+`/`-`-and-digits run, and commit on success.
///
/// # Safety
///
/// As every other method here: `receiver` is an `NSScanner`.
unsafe fn scan_signed(receiver: Id) -> Option<i64> {
    let c = unsafe { contents(receiver) }?;
    // SAFETY: `c.skip_set` is null or live.
    let start = unsafe { skip(&c.units, c.location, c.skip_set) };
    let ascii = ascii_run(&c.units, start);
    let (len, negative, digits) = signed_digits(&ascii)?;
    c.location = start + len;
    Some(signed_from_magnitude(accumulate(digits, 10), negative))
}

/// As [`scan_signed`], for the unsigned and hex scanners, which share the
/// same shape once the digit grammar differs.
unsafe fn scan_unsigned(receiver: Id, radix: u32, hex: bool) -> Option<u64> {
    let c = unsafe { contents(receiver) }?;
    // SAFETY: `c.skip_set` is null or live.
    let start = unsafe { skip(&c.units, c.location, c.skip_set) };
    let ascii = ascii_run(&c.units, start);
    let (len, digits) = if hex {
        hex_digits(&ascii)?
    } else {
        unsigned_digits(&ascii)?
    };
    c.location = start + len;
    Some(accumulate(digits, radix))
}

unsafe fn scan_fp(receiver: Id) -> Option<f64> {
    let c = unsafe { contents(receiver) }?;
    // SAFETY: `c.skip_set` is null or live.
    let start = unsafe { skip(&c.units, c.location, c.skip_set) };
    let ascii = ascii_run(&c.units, start);
    let len = float_token_len(&ascii);
    if len == 0 {
        return None;
    }
    let text = std::str::from_utf8(&ascii[..len]).ok()?;
    let value: f64 = text.parse().ok()?;
    c.location = start + len;
    Some(value)
}

unsafe extern "C" fn scan_int(receiver: Id, _sel: Sel, out: *mut i32) -> bool {
    // SAFETY: an `NSScanner`.
    let Some(value) = (unsafe { scan_signed(receiver) }) else {
        return false;
    };
    if !out.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *out = value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32 };
    }
    true
}

unsafe extern "C" fn scan_integer(receiver: Id, _sel: Sel, out: *mut i64) -> bool {
    // SAFETY: an `NSScanner`. `NSInteger` is 64-bit on every host this tree
    // runs a guest on.
    let Some(value) = (unsafe { scan_signed(receiver) }) else {
        return false;
    };
    if !out.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *out = value };
    }
    true
}

unsafe extern "C" fn scan_long_long(receiver: Id, _sel: Sel, out: *mut i64) -> bool {
    // SAFETY: as `scan_integer`.
    unsafe { scan_integer(receiver, _sel, out) }
}

unsafe extern "C" fn scan_unsigned_long_long(receiver: Id, _sel: Sel, out: *mut u64) -> bool {
    // SAFETY: an `NSScanner`.
    let Some(value) = (unsafe { scan_unsigned(receiver, 10, false) }) else {
        return false;
    };
    if !out.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *out = value };
    }
    true
}

unsafe extern "C" fn scan_double(receiver: Id, _sel: Sel, out: *mut f64) -> bool {
    // SAFETY: an `NSScanner`.
    let Some(value) = (unsafe { scan_fp(receiver) }) else {
        return false;
    };
    if !out.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *out = value };
    }
    true
}

unsafe extern "C" fn scan_float(receiver: Id, _sel: Sel, out: *mut f32) -> bool {
    // SAFETY: an `NSScanner`. Overflowing `f32` answers infinity, the same
    // saturation `scanDouble:` would show a caller comparing against it.
    let Some(value) = (unsafe { scan_fp(receiver) }) else {
        return false;
    };
    if !out.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *out = value as f32 };
    }
    true
}

unsafe extern "C" fn scan_hex_int(receiver: Id, _sel: Sel, out: *mut u32) -> bool {
    // SAFETY: an `NSScanner`.
    let Some(value) = (unsafe { scan_unsigned(receiver, 16, true) }) else {
        return false;
    };
    if !out.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *out = value.min(u64::from(u32::MAX)) as u32 };
    }
    true
}

unsafe extern "C" fn scan_hex_long_long(receiver: Id, _sel: Sel, out: *mut u64) -> bool {
    // SAFETY: an `NSScanner`.
    let Some(value) = (unsafe { scan_unsigned(receiver, 16, true) }) else {
        return false;
    };
    if !out.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *out = value };
    }
    true
}

pub(crate) fn install() {
    let class = class();
    class_methods!(class, {
        "scannerWithString:" => scanner_with_string, c"@24@0:8@16";
    });
    methods!(class, {
        "initWithString:" => init_with_string, c"@24@0:8@16";
        "string" => get_string, c"@16@0:8";
        "scanLocation" => get_scan_location, c"Q16@0:8";
        "setScanLocation:" => set_scan_location, c"v24@0:8Q16";
        "charactersToBeSkipped" => get_characters_to_be_skipped, c"@16@0:8";
        "setCharactersToBeSkipped:" => set_characters_to_be_skipped, c"v24@0:8@16";
        "caseSensitive" => get_case_sensitive, c"B16@0:8";
        "setCaseSensitive:" => set_case_sensitive, c"v24@0:8B16";
        "isAtEnd" => is_at_end, c"B16@0:8";
        "scanString:intoString:" => scan_string_into_string, c"B32@0:8@16^@24";
        "scanUpToString:intoString:" => scan_up_to_string_into_string, c"B32@0:8@16^@24";
        "scanCharactersFromSet:intoString:" => scan_characters_from_set_into_string,
            c"B32@0:8@16^@24";
        "scanUpToCharactersFromSet:intoString:" => scan_up_to_characters_from_set_into_string,
            c"B32@0:8@16^@24";
        "scanInt:" => scan_int, c"B24@0:8^i16";
        "scanInteger:" => scan_integer, c"B24@0:8^q16";
        "scanLongLong:" => scan_long_long, c"B24@0:8^q16";
        "scanUnsignedLongLong:" => scan_unsigned_long_long, c"B24@0:8^Q16";
        "scanDouble:" => scan_double, c"B24@0:8^d16";
        "scanFloat:" => scan_float, c"B24@0:8^f16";
        "scanHexInt:" => scan_hex_int, c"B24@0:8^I16";
        "scanHexLongLong:" => scan_hex_long_long, c"B24@0:8^Q16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanner(text: &str) -> Id {
        let string = crate::string::from_str(text);
        // SAFETY: a fresh string.
        let s = unsafe { init_with_string(std::ptr::null_mut(), objc::sel::get("x"), string) };
        objc::arc::release(string);
        s
    }

    fn sel() -> Sel {
        objc::sel::get("x")
    }

    #[test]
    fn string_and_location_round_trip() {
        crate::bootstrap();
        let s = scanner("hello");
        // SAFETY: a live scanner.
        unsafe {
            assert!(!is_at_end(s, sel()));
            set_scan_location(s, sel(), 3);
            assert_eq!(get_scan_location(s, sel()), 3);
            // Clamped, not refused.
            set_scan_location(s, sel(), 999);
            assert_eq!(get_scan_location(s, sel()), 5);
            assert!(is_at_end(s, sel()));
        }
    }

    #[test]
    fn is_at_end_skips_trailing_whitespace() {
        crate::bootstrap();
        let s = scanner("x   ");
        // SAFETY: a live scanner.
        unsafe {
            set_scan_location(s, sel(), 1);
            assert!(is_at_end(s, sel()), "only skippable characters remain");
        }
    }

    #[test]
    fn scan_string_is_case_insensitive_by_default_and_skips_leading_space() {
        crate::bootstrap();
        let s = scanner("  FOO bar");
        let target = crate::string::from_str("foo");
        let mut out: Id = std::ptr::null_mut();
        // SAFETY: live objects throughout.
        unsafe {
            assert!(scan_string_into_string(s, sel(), target, &mut out));
            assert_eq!(crate::string::bytes(out), b"FOO");
            assert_eq!(get_scan_location(s, sel()), 5);
        }
        objc::arc::release(target);
    }

    #[test]
    fn scan_string_leaves_the_location_on_a_failed_match() {
        crate::bootstrap();
        let s = scanner("bar");
        let target = crate::string::from_str("foo");
        // SAFETY: live objects.
        unsafe {
            assert!(!scan_string_into_string(
                s,
                sel(),
                target,
                std::ptr::null_mut()
            ));
            assert_eq!(get_scan_location(s, sel()), 0);
        }
        objc::arc::release(target);
    }

    #[test]
    fn case_sensitive_scan_string_rejects_a_different_case() {
        crate::bootstrap();
        let s = scanner("FOO");
        // SAFETY: a live scanner.
        unsafe { set_case_sensitive(s, sel(), true) };
        let target = crate::string::from_str("foo");
        // SAFETY: live objects.
        unsafe {
            assert!(!scan_string_into_string(
                s,
                sel(),
                target,
                std::ptr::null_mut()
            ));
        }
        objc::arc::release(target);
    }

    #[test]
    fn scan_up_to_string_fails_right_at_the_stop_string() {
        crate::bootstrap();
        let s = scanner("stop here");
        let target = crate::string::from_str("stop");
        // SAFETY: live objects.
        unsafe {
            assert!(!scan_up_to_string_into_string(
                s,
                sel(),
                target,
                std::ptr::null_mut()
            ));
        }
        objc::arc::release(target);
    }

    #[test]
    fn scan_up_to_string_consumes_the_rest_when_never_found() {
        crate::bootstrap();
        let s = scanner("no stop in here");
        let target = crate::string::from_str("xyz");
        let mut out: Id = std::ptr::null_mut();
        // SAFETY: live objects.
        unsafe {
            assert!(scan_up_to_string_into_string(s, sel(), target, &mut out));
            assert_eq!(crate::string::bytes(out), b"no stop in here");
            assert!(is_at_end(s, sel()));
        }
        objc::arc::release(target);
    }

    /// `0`-`9`, built the same way `CTFontCopyCharacterSet` does - a plain,
    /// retained custom set, with no dependency on which standard sets this
    /// crate happens to expose outside [`crate::characterset`].
    fn digit_set() -> Id {
        crate::characterset::create_with_ranges(vec![(u32::from(b'0'), u32::from(b'9'))])
    }

    #[test]
    fn scan_characters_from_set_reads_a_maximal_run() {
        crate::bootstrap();
        let s = scanner("abc123");
        let digits = digit_set();
        let mut out: Id = std::ptr::null_mut();
        // SAFETY: live objects.
        unsafe {
            // `abc` is not a digit, so the scan must skip past it first via
            // `scanUpToCharactersFromSet:`, which this call stands in for by
            // moving the location directly - the method under test here is
            // only the maximal-run read itself.
            set_scan_location(s, sel(), 3);
            assert!(scan_characters_from_set_into_string(
                s,
                sel(),
                digits,
                &mut out
            ));
            assert_eq!(crate::string::bytes(out), b"123");
            assert!(is_at_end(s, sel()));
        }
        objc::arc::release(digits);
    }

    #[test]
    fn scan_up_to_characters_from_set_stops_before_the_first_member() {
        crate::bootstrap();
        let s = scanner("abc123");
        let digits = digit_set();
        let mut out: Id = std::ptr::null_mut();
        // SAFETY: live objects.
        unsafe {
            assert!(scan_up_to_characters_from_set_into_string(
                s,
                sel(),
                digits,
                &mut out
            ));
            assert_eq!(crate::string::bytes(out), b"abc");
            assert_eq!(get_scan_location(s, sel()), 3);
        }
        objc::arc::release(digits);
    }

    #[test]
    fn scan_int_reads_a_signed_decimal() {
        crate::bootstrap();
        let s = scanner("  -42rest");
        let mut out = 0i32;
        // SAFETY: a live scanner and a writable `out`.
        unsafe {
            assert!(scan_int(s, sel(), &mut out));
            assert_eq!(out, -42);
            assert_eq!(get_scan_location(s, sel()), 5);
        }
    }

    #[test]
    fn scan_integer_saturates_rather_than_overflowing() {
        crate::bootstrap();
        let s = scanner("99999999999999999999");
        let mut out = 0i64;
        // SAFETY: a live scanner and a writable `out`.
        unsafe {
            assert!(scan_integer(s, sel(), &mut out));
            assert_eq!(out, i64::MAX);
        }
    }

    #[test]
    fn scan_unsigned_long_long_refuses_a_sign() {
        crate::bootstrap();
        let s = scanner("-5");
        let mut out = 0u64;
        // SAFETY: a live scanner and a writable `out`.
        unsafe {
            assert!(!scan_unsigned_long_long(s, sel(), &mut out));
            assert_eq!(get_scan_location(s, sel()), 0);
        }
    }

    #[test]
    fn scan_double_reads_an_exponent() {
        crate::bootstrap();
        let s = scanner("1.5e2 ");
        let mut out = 0.0f64;
        // SAFETY: a live scanner and a writable `out`.
        unsafe {
            assert!(scan_double(s, sel(), &mut out));
            assert_eq!(out, 150.0);
        }
    }

    #[test]
    fn scan_hex_int_takes_an_optional_prefix() {
        crate::bootstrap();
        let with_prefix = scanner("0x1F");
        let without_prefix = scanner("1F");
        let mut a = 0u32;
        let mut b = 0u32;
        // SAFETY: live scanners and writable outputs.
        unsafe {
            assert!(scan_hex_int(with_prefix, sel(), &mut a));
            assert!(scan_hex_int(without_prefix, sel(), &mut b));
        }
        assert_eq!((a, b), (31, 31));
    }

    #[test]
    fn scan_hex_long_long_saturates_at_the_unsigned_maximum() {
        crate::bootstrap();
        let s = scanner("0xFFFFFFFFFFFFFFFFFF");
        let mut out = 0u64;
        // SAFETY: a live scanner and a writable `out`.
        unsafe {
            assert!(scan_hex_long_long(s, sel(), &mut out));
            assert_eq!(out, u64::MAX);
        }
    }

    #[test]
    fn a_null_out_pointer_still_advances_the_location() {
        crate::bootstrap();
        let s = scanner("42");
        // SAFETY: a live scanner; null is a documented, accepted out-pointer.
        unsafe {
            assert!(scan_int(s, sel(), std::ptr::null_mut()));
            assert_eq!(get_scan_location(s, sel()), 2);
        }
    }

    #[test]
    fn setting_characters_to_be_skipped_to_nil_skips_nothing() {
        crate::bootstrap();
        let s = scanner("   x");
        // SAFETY: a live scanner; nil is documented as "skip none".
        unsafe {
            set_characters_to_be_skipped(s, sel(), std::ptr::null_mut());
            assert!(get_characters_to_be_skipped(s, sel()).is_null());
            let target = crate::string::from_str("x");
            assert!(!scan_string_into_string(
                s,
                sel(),
                target,
                std::ptr::null_mut()
            ));
            objc::arc::release(target);
        }
    }
}
