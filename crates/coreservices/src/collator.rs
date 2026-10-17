use std::cmp::Ordering;
use std::ffi::c_void;

const CASE_INSENSITIVE: u32 = 1 << 3;
const PUNCTUATION_SIGNIFICANT: u32 = 1 << 15;
const DIGITS_AS_NUMBER: u32 = 1 << 17;

const PARAM_ERR: i32 = -50;

struct Collator {
    options: u32,
}

pub unsafe extern "C" fn LocaleRefFromLocaleString(_name: *const i8, locale: *mut usize) -> i32 {
    if locale.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller's out-parameter.
    unsafe { *locale = 1 };
    0
}

pub unsafe extern "C" fn UCCreateCollator(
    _locale: usize,
    _variant: u32,
    options: u32,
    collator: *mut *mut c_void,
) -> i32 {
    if collator.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller's out-parameter; [`UCDisposeCollator`] frees it.
    unsafe { *collator = Box::into_raw(Box::new(Collator { options })).cast() };
    0
}

pub unsafe extern "C" fn UCDisposeCollator(collator: *mut *mut c_void) -> i32 {
    if collator.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: a collator [`UCCreateCollator`] made, or null.
    unsafe {
        let made = std::mem::replace(&mut *collator, std::ptr::null_mut());
        if !made.is_null() {
            drop(Box::from_raw(made as *mut Collator));
        }
    }
    0
}

fn options_of(collator: *mut c_void) -> u32 {
    match collator.is_null() {
        true => 0,
        // SAFETY: a collator [`UCCreateCollator`] made.
        false => unsafe { (*(collator as *const Collator)).options },
    }
}

unsafe fn decode(text: *const u16, length: usize) -> Vec<char> {
    if text.is_null() || length == 0 {
        return Vec::new();
    }
    // SAFETY: the caller's obligation.
    let units = unsafe { std::slice::from_raw_parts(text, length) };
    char::decode_utf16(units.iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Token {
    Number(usize, Vec<char>),
    Char(char),
}

fn tokens(text: &[char], options: u32, fold: bool) -> Vec<Token> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let c = text[at];
        if options & DIGITS_AS_NUMBER != 0 && c.is_ascii_digit() {
            let start = at;
            while at < text.len() && text[at].is_ascii_digit() {
                at += 1;
            }
            let digits: Vec<char> = text[start..at]
                .iter()
                .copied()
                .skip_while(|d| *d == '0')
                .collect();
            out.push(Token::Number(digits.len(), digits));
            continue;
        }
        at += 1;
        if options & PUNCTUATION_SIGNIFICANT == 0 && (c.is_ascii_punctuation() || c.is_whitespace())
        {
            continue;
        }
        out.extend(match fold {
            true => c.to_lowercase().map(Token::Char).collect::<Vec<_>>(),
            false => vec![Token::Char(c)],
        });
    }
    out
}

fn compare(a: &[char], b: &[char], options: u32) -> Ordering {
    let primary = tokens(a, options, true).cmp(&tokens(b, options, true));
    if primary != Ordering::Equal || options & CASE_INSENSITIVE != 0 {
        return primary;
    }
    let case = |text: &[char]| -> Vec<bool> {
        tokens(text, options, false)
            .iter()
            .map(|t| matches!(t, Token::Char(c) if c.is_uppercase()))
            .collect()
    };
    case(a).cmp(&case(b))
}

pub unsafe extern "C" fn UCCompareText(
    collator: *mut c_void,
    text1: *const u16,
    length1: usize,
    text2: *const u16,
    length2: usize,
    equivalent: *mut u8,
    order: *mut i32,
) -> i32 {
    // SAFETY: the caller states each text covers its length.
    let (a, b) = unsafe { (decode(text1, length1), decode(text2, length2)) };
    let ordering = compare(&a, &b, options_of(collator));
    // SAFETY: the caller's out-parameters, each optional.
    unsafe {
        if !equivalent.is_null() {
            *equivalent = u8::from(ordering == Ordering::Equal);
        }
        if !order.is_null() {
            *order = ordering as i32;
        }
    }
    0
}

pub unsafe extern "C" fn UCGetCollationKey(
    collator: *mut c_void,
    text: *const u16,
    length: usize,
    max: usize,
    actual: *mut usize,
    key: *mut u32,
) -> i32 {
    let options = options_of(collator) & !DIGITS_AS_NUMBER;
    // SAFETY: the caller states the text covers its length.
    let chars = unsafe { decode(text, length) };
    let mut values: Vec<u32> = tokens(&chars, options, true)
        .into_iter()
        .filter_map(|t| match t {
            Token::Char(c) => Some(c as u32 + 1),
            Token::Number(..) => None,
        })
        .collect();
    if options & CASE_INSENSITIVE == 0 {
        values.push(0);
        values.extend(chars.iter().map(|c| u32::from(c.is_uppercase()) + 1));
    }
    if values.len() > max || (key.is_null() && max != 0) {
        return PARAM_ERR;
    }
    // SAFETY: the caller's buffer of `max` values and out-parameter.
    unsafe {
        if !key.is_null() {
            std::ptr::copy_nonoverlapping(values.as_ptr(), key, values.len());
        }
        if !actual.is_null() {
            *actual = values.len();
        }
    }
    0
}

pub unsafe extern "C" fn UCCompareCollationKeys(
    key1: *const u32,
    length1: usize,
    key2: *const u32,
    length2: usize,
    equivalent: *mut u8,
    order: *mut i32,
) -> i32 {
    let slice = |key: *const u32, length: usize| match key.is_null() || length == 0 {
        true => &[][..],
        // SAFETY: the caller states the key covers its length.
        false => unsafe { std::slice::from_raw_parts(key, length) },
    };
    let ordering = slice(key1, length1).cmp(slice(key2, length2));
    // SAFETY: the caller's out-parameters, each optional.
    unsafe {
        if !equivalent.is_null() {
            *equivalent = u8::from(ordering == Ordering::Equal);
        }
        if !order.is_null() {
            *order = ordering as i32;
        }
    }
    0
}

exports::exports! {
    functions {
        "LocaleRefFromLocaleString" => LocaleRefFromLocaleString,
        "UCCreateCollator" => UCCreateCollator,
        "UCDisposeCollator" => UCDisposeCollator,
        "UCCompareText" => UCCompareText,
        "UCGetCollationKey" => UCGetCollationKey,
        "UCCompareCollationKeys" => UCCompareCollationKeys,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(a: &str, b: &str, options: u32) -> i32 {
        let (a, b): (Vec<u16>, Vec<u16>) = (a.encode_utf16().collect(), b.encode_utf16().collect());
        let mut collator = std::ptr::null_mut();
        let (mut equivalent, mut order) = (0u8, 0i32);
        // SAFETY: each pointer covers what it is said to.
        unsafe {
            assert_eq!(UCCreateCollator(1, 0, options, &mut collator), 0);
            UCCompareText(
                collator,
                a.as_ptr(),
                a.len(),
                b.as_ptr(),
                b.len(),
                &mut equivalent,
                &mut order,
            );
            UCDisposeCollator(&mut collator);
        }
        assert!(collator.is_null());
        assert_eq!(equivalent != 0, order == 0);
        order
    }

    #[test]
    fn texts_order_as_a_user_reads_them() {
        assert_eq!(order("apple", "Banana", 0), -1);
        assert_eq!(order("apple", "Apple", CASE_INSENSITIVE), 0);
        assert_eq!(order("apple", "Apple", 0), -1);
        assert_eq!(order("file9", "file10", DIGITS_AS_NUMBER), -1);
        assert_eq!(order("file9", "file10", 0), 1);
        assert_eq!(order("a-b", "ab", 0), 0);
        assert_eq!(order("a-b", "ab", PUNCTUATION_SIGNIFICANT), -1);
    }
}
