use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
struct Body {
    isa: *const objc::class::Class,
    kind: usize,
}

const URL_PATH: usize = 1;
const WHITESPACE: usize = 2;
const NEWLINE: usize = 3;
const WHITESPACE_AND_NEWLINE: usize = 4;
const DECIMAL_DIGITS: usize = 5;
const LETTERS: usize = 6;
const ALPHANUMERICS: usize = 7;
const PUNCTUATION: usize = 8;
/// A set made from characters or a range, whose members are in [`custom`].
const CUSTOM: usize = 9;
const CONTROL: usize = 10;
const LOWERCASE: usize = 11;
const UPPERCASE: usize = 12;
const SYMBOLS: usize = 13;
const URL_QUERY: usize = 14;
const URL_HOST: usize = 15;
const URL_USER: usize = 16;
const KINDS: usize = 17;

/// The members of each `CUSTOM` set: inclusive code point ranges, and whether
/// the set is everything *but* them.
#[derive(Clone, Default)]
struct Members {
    ranges: Vec<(u32, u32)>,
    inverted: bool,
}

fn custom() -> &'static Mutex<HashMap<usize, Members>> {
    static CUSTOM_SETS: OnceLock<Mutex<HashMap<usize, Members>>> = OnceLock::new();
    CUSTOM_SETS.get_or_init(Default::default)
}

/// One of Foundation's standard sets, made once for the process as Apple's are.
fn standard(kind: usize) -> Id {
    static SETS: std::sync::Mutex<[usize; KINDS]> = std::sync::Mutex::new([0; KINDS]);
    let mut sets = SETS.lock().unwrap_or_else(|e| e.into_inner());
    if sets[kind] == 0 {
        let class = objc::class::lookup(c"NSCharacterSet").expect("installed NSCharacterSet");
        // SAFETY: `Body` begins with `isa` and is the class's registered size.
        let object: *mut Body = unsafe { corefoundation::base::allocate(class) };
        // SAFETY: a newly allocated instance whose payload belongs to us.
        unsafe { (*object).kind = kind };
        objc::arc::retain(object as Id);
        sets[kind] = object as usize;
    }
    sets[kind] as Id
}

pub(crate) unsafe extern "C" fn url_path_allowed_character_set(_class: Id, _sel: Sel) -> Id {
    standard(URL_PATH)
}

unsafe extern "C" fn url_query_allowed_character_set(_class: Id, _sel: Sel) -> Id {
    standard(URL_QUERY)
}

unsafe extern "C" fn url_host_allowed_character_set(_class: Id, _sel: Sel) -> Id {
    standard(URL_HOST)
}

unsafe extern "C" fn url_user_allowed_character_set(_class: Id, _sel: Sel) -> Id {
    standard(URL_USER)
}

unsafe extern "C" fn whitespace_character_set(_class: Id, _sel: Sel) -> Id {
    standard(WHITESPACE)
}

unsafe extern "C" fn newline_character_set(_class: Id, _sel: Sel) -> Id {
    standard(NEWLINE)
}

unsafe extern "C" fn whitespace_and_newline_character_set(_class: Id, _sel: Sel) -> Id {
    standard(WHITESPACE_AND_NEWLINE)
}

/// Whether a byte of a UTF-8 string is a member of `set`.
///
/// FIXME (ACCURACY): the standard whitespace and newline sets hold only their
/// ASCII members. The Unicode spaces and line separators Apple's sets also
/// hold (U+00A0, U+0085, U+2028, ...) are never matched. Matching bytes is
/// safe for ASCII members, since no byte of a multibyte character is below
/// 0x80; a set with non-ASCII members wants [`contains_char`].
pub(crate) unsafe fn contains(set: Id, byte: u8) -> bool {
    // SAFETY: the caller's obligation.
    byte < 0x80 && unsafe { contains_char(set, char::from(byte)) }
}

fn kind_of(set: Id) -> Option<usize> {
    let class = objc::class::lookup(c"NSCharacterSet")?;
    // SAFETY: the caller supplies a live object or nil.
    if !unsafe { corefoundation::base::is_kind_of(set.cast(), class) } {
        return None;
    }
    // SAFETY: instances of the class have `Body`'s layout.
    Some(unsafe { (*(set as *const Body)).kind })
}

/// Whether `c` is a member of `set`.
///
/// # Safety
///
/// `set` is null or an object this runtime allocated.
pub(crate) unsafe fn contains_char(set: Id, c: char) -> bool {
    let Some(kind) = kind_of(set) else {
        return false;
    };
    let ascii = u8::try_from(c).ok().filter(u8::is_ascii);
    match kind {
        URL_PATH => {
            ascii.is_some_and(|b| b.is_ascii_alphanumeric() || b"-._~/!$&'()*+,;=:@".contains(&b))
        }
        // RFC 3986's sets, as Foundation draws them.
        URL_QUERY => {
            ascii.is_some_and(|b| b.is_ascii_alphanumeric() || b"-._~/?!$&'()*+,;=:@".contains(&b))
        }
        URL_HOST => {
            ascii.is_some_and(|b| b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:[]".contains(&b))
        }
        URL_USER => {
            ascii.is_some_and(|b| b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=".contains(&b))
        }
        WHITESPACE => matches!(ascii, Some(b' ' | b'\t')),
        NEWLINE => ascii.is_some_and(|b| (0x0a..=0x0d).contains(&b)),
        WHITESPACE_AND_NEWLINE => {
            ascii.is_some_and(|b| b == b' ' || b == b'\t' || (0x0a..=0x0d).contains(&b))
        }
        DECIMAL_DIGITS => darwin::icu::u_charDigitValue(c as i32) >= 0,
        LETTERS => c.is_alphabetic(),
        ALPHANUMERICS => c.is_alphanumeric(),
        // FIXME (ACCURACY): ASCII's members of the Unicode `P*` categories
        // only; no table of the rest is kept.
        PUNCTUATION => ascii.is_some_and(|b| b"!\"#%&'()*,-./:;?@[\\]_{}".contains(&b)),
        CONTROL => c.is_control(),
        LOWERCASE => c.is_lowercase(),
        UPPERCASE => c.is_uppercase(),
        // FIXME (ACCURACY): ASCII's members of the Unicode `S*` categories
        // only, as for punctuation.
        SYMBOLS => ascii.is_some_and(|b| b"$+<=>^`|~".contains(&b)),
        CUSTOM => {
            let custom = custom().lock().unwrap_or_else(|e| e.into_inner());
            let Some(members) = custom.get(&(set as usize)) else {
                return false;
            };
            let code = c as u32;
            members
                .ranges
                .iter()
                .any(|(low, high)| (*low..=*high).contains(&code))
                != members.inverted
        }
        _ => false,
    }
}

/// A new set of `members`, autoreleased.
fn make_custom(members: Members) -> Id {
    let class = objc::class::lookup(c"NSCharacterSet").expect("installed NSCharacterSet");
    // SAFETY: `Body` begins with `isa` and is the class's registered size.
    let object: *mut Body = unsafe { corefoundation::base::allocate(class) };
    // SAFETY: a newly allocated instance whose payload belongs to us.
    unsafe { (*object).kind = CUSTOM };
    custom()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(object as usize, members);
    objc::arc::autorelease(object as Id)
}

unsafe extern "C" fn with_characters_in_string(_class: Id, _sel: Sel, text: Id) -> Id {
    // SAFETY: a string, or nil.
    let text = String::from_utf8_lossy(&unsafe { crate::string::bytes(text) }).into_owned();
    let ranges = text.chars().map(|c| (c as u32, c as u32)).collect();
    make_custom(Members {
        ranges,
        inverted: false,
    })
}

/// `+characterSetWithRange:`, over UTF-16 code units as `NSRange` counts them.
unsafe extern "C" fn with_range(_class: Id, _sel: Sel, range: crate::geometry::NSRange) -> Id {
    let ranges = match range.length {
        0 => Vec::new(),
        n => vec![(range.location as u32, (range.location + n - 1) as u32)],
    };
    make_custom(Members {
        ranges,
        inverted: false,
    })
}

/// `-invertedSet`: a set whose members are everything this one's are not.
unsafe extern "C" fn inverted_set(receiver: Id, _sel: Sel) -> Id {
    match kind_of(receiver) {
        Some(CUSTOM) => {
            let members = custom()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&(receiver as usize))
                .cloned()
                .unwrap_or_default();
            make_custom(Members {
                inverted: !members.inverted,
                ..members
            })
        }
        // A standard set's complement is recorded by membership test: every
        // code point whose answer differs. Built by asking the set itself, for
        // the Basic Multilingual Plane and the planes past it in one range.
        Some(_) => {
            let mut ranges = Vec::new();
            let mut start: Option<u32> = None;
            for code in 0..=0xFFFFu32 {
                let member = char::from_u32(code)
                    // SAFETY: the receiver, a live set.
                    .is_some_and(|c| unsafe { contains_char(receiver, c) });
                match (member, start) {
                    (true, None) => start = Some(code),
                    (false, Some(from)) => {
                        ranges.push((from, code - 1));
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(from) = start {
                ranges.push((from, 0xFFFF));
            }
            make_custom(Members {
                ranges,
                inverted: true,
            })
        }
        None => std::ptr::null_mut(),
    }
}

/// `-characterIsMember:`, of one UTF-16 code unit.
unsafe extern "C" fn character_is_member(receiver: Id, _sel: Sel, unit: u16) -> u8 {
    // SAFETY: the receiver, a live set.
    u8::from(char::from_u32(u32::from(unit)).is_some_and(|c| unsafe { contains_char(receiver, c) }))
}

unsafe extern "C" fn decimal_digit_character_set(_class: Id, _sel: Sel) -> Id {
    standard(DECIMAL_DIGITS)
}

unsafe extern "C" fn letter_character_set(_class: Id, _sel: Sel) -> Id {
    standard(LETTERS)
}

unsafe extern "C" fn alphanumeric_character_set(_class: Id, _sel: Sel) -> Id {
    standard(ALPHANUMERICS)
}

unsafe extern "C" fn punctuation_character_set(_class: Id, _sel: Sel) -> Id {
    standard(PUNCTUATION)
}

pub(crate) fn install() {
    let class = corefoundation::base::register(c"NSCharacterSet", None, size_of::<Body>(), false);
    class_methods!(class, {
        "URLPathAllowedCharacterSet" => url_path_allowed_character_set, c"@16@0:8";
        "URLQueryAllowedCharacterSet" => url_query_allowed_character_set, c"@16@0:8";
        // The fragment set is the query set.
        "URLFragmentAllowedCharacterSet" => url_query_allowed_character_set, c"@16@0:8";
        "URLHostAllowedCharacterSet" => url_host_allowed_character_set, c"@16@0:8";
        // The password set is the user set.
        "URLUserAllowedCharacterSet" => url_user_allowed_character_set, c"@16@0:8";
        "URLPasswordAllowedCharacterSet" => url_user_allowed_character_set, c"@16@0:8";
        "whitespaceCharacterSet" => whitespace_character_set, c"@16@0:8";
        "newlineCharacterSet" => newline_character_set, c"@16@0:8";
        "whitespaceAndNewlineCharacterSet" => whitespace_and_newline_character_set, c"@16@0:8";
        "decimalDigitCharacterSet" => decimal_digit_character_set, c"@16@0:8";
        "letterCharacterSet" => letter_character_set, c"@16@0:8";
        "alphanumericCharacterSet" => alphanumeric_character_set, c"@16@0:8";
        "punctuationCharacterSet" => punctuation_character_set, c"@16@0:8";
        "characterSetWithCharactersInString:" => with_characters_in_string, c"@24@0:8@16";
        "characterSetWithRange:" => with_range, c"@32@0:8{_NSRange=QQ}16";
    });
    methods!(class, {
        "invertedSet" => inverted_set, c"@16@0:8";
        "characterIsMember:" => character_is_member, c"c20@0:8S16";
    });
}

/// `CFCharacterSetRef CFCharacterSetGetPredefined(CFCharacterSetPredefinedSet)`.
/// A set with no counterpart here is an empty one.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CFCharacterSetGetPredefined(set: isize) -> Id {
    let kind = match set {
        1 => CONTROL,
        2 => WHITESPACE,
        3 => WHITESPACE_AND_NEWLINE,
        4 => DECIMAL_DIGITS,
        5 => LETTERS,
        6 => LOWERCASE,
        7 => UPPERCASE,
        10 => ALPHANUMERICS,
        11 => PUNCTUATION,
        14 => SYMBOLS,
        15 => NEWLINE,
        _ => CUSTOM,
    };
    standard(kind)
}

/// `CFMutableCharacterSetRef CFCharacterSetCreateMutable(CFAllocatorRef)`: an
/// empty set the caller owns.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CFCharacterSetCreateMutable(_allocator: *const std::ffi::c_void) -> Id {
    let set = make_custom(Members::default());
    objc::arc::retain(set);
    set
}

/// `void CFCharacterSetAddCharactersInRange(CFMutableCharacterSetRef, CFRange)`,
/// over UTF-32 code points.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CFCharacterSetAddCharactersInRange(
    set: Id,
    location: isize,
    length: isize,
) {
    if kind_of(set) != Some(CUSTOM) || location < 0 || length <= 0 {
        return;
    }
    let mut custom = custom().lock().unwrap_or_else(|e| e.into_inner());
    let members = custom.entry(set as usize).or_default();
    let range = (location as u32, (location + length - 1) as u32);
    if members.inverted {
        // Adding to a complement removes from what it excludes.
        members.ranges = members
            .ranges
            .iter()
            .flat_map(|&(low, high)| {
                let mut parts = Vec::new();
                if low < range.0 {
                    parts.push((low, high.min(range.0 - 1)));
                }
                if high > range.1 {
                    parts.push((low.max(range.1 + 1), high));
                }
                parts
            })
            .collect();
    } else {
        members.ranges.push(range);
    }
}

/// `Boolean CFCharacterSetIsLongCharacterMember(CFCharacterSetRef, UTF32Char)`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CFCharacterSetIsLongCharacterMember(set: Id, code: u32) -> u8 {
    char::from_u32(code)
        .is_some_and(|c| unsafe { contains_char(set, c) })
        .into()
}

/// `Boolean CFCharacterSetIsCharacterMember(CFCharacterSetRef, UniChar)`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CFCharacterSetIsCharacterMember(set: Id, unit: u16) -> u8 {
    char::from_u32(u32::from(unit))
        .is_some_and(|c| unsafe { contains_char(set, c) })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_url_path_set_is_a_stable_object() {
        crate::bootstrap();
        // SAFETY: a nullary class method returning an object.
        let first = unsafe {
            url_path_allowed_character_set(
                objc::class::ptr(objc::class::lookup(c"NSCharacterSet").unwrap()) as Id,
                objc::sel::get("URLPathAllowedCharacterSet"),
            )
        };
        let second = unsafe {
            url_path_allowed_character_set(
                objc::class::ptr(objc::class::lookup(c"NSCharacterSet").unwrap()) as Id,
                objc::sel::get("URLPathAllowedCharacterSet"),
            )
        };
        assert!(!first.is_null());
        assert_eq!(first, second);
    }

    #[test]
    fn newline_and_whitespace_sets_hold_their_ascii_members() {
        crate::bootstrap();
        let (newline, whitespace) = (standard(NEWLINE), standard(WHITESPACE));
        // SAFETY: standard sets.
        unsafe {
            assert!(contains(newline, b'\n') && contains(newline, b'\r'));
            assert!(!contains(newline, b' '));
            assert!(contains(whitespace, b' ') && contains(whitespace, b'\t'));
            assert!(!contains(whitespace, b'\n'));
        }
    }
}
