//! `NSRegularExpression` and `NSTextCheckingResult`, over the `fancy-regex`
//! crate `darwin::regex` already depends on for POSIX `regex_t` - its native
//! pattern syntax is Perl/PCRE-style, which is close enough to ICU's that this
//! crate uses it directly rather than translating, the way `darwin::regex`
//! translates *from* POSIX syntax to reach the same engine.
//!
//! FIXME (ACCURACY): `fancy-regex` does not implement every ICU construct -
//! Unicode property escapes (`\p{...}`) and POSIX character classes
//! (`[[:alpha:]]`) among them. A pattern using one fails to compile here
//! rather than matching wrongly, which is the failure mode this tree prefers.
//! `NSMatchingOptions` (as opposed to `NSRegularExpressionOptions`, which this
//! does honor) is accepted and ignored: `NSMatchingReportProgress`,
//! `...ReportCompletion` and `...WithTransparentBounds` are run-loop and
//! bounds concerns this synchronous implementation has no use for, and
//! `NSMatchingAnchored`/`...WithoutAnchoringBounds` are not applied.

use corefoundation::base::CFTypeRef;
use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};
use crate::geometry::NSRange;

/// `NSNotFound`.
const NOT_FOUND: i64 = i64::MAX;

mod options {
    pub const CASE_INSENSITIVE: u64 = 1 << 0;
    pub const ALLOW_COMMENTS_AND_WHITESPACE: u64 = 1 << 1;
    pub const IGNORE_METACHARACTERS: u64 = 1 << 2;
    pub const DOT_MATCHES_LINE_SEPARATORS: u64 = 1 << 3;
    pub const ANCHORS_MATCH_LINES: u64 = 1 << 4;
}

/// A string's UTF-16 code units, each mapped back to the UTF-8 byte its
/// character starts at - `fancy_regex` works in bytes, `NSRange` in UTF-16
/// units, and every match this module answers has to cross that boundary.
struct Index {
    text: String,
    /// `byte_at[i]` is the byte offset of UTF-16 unit `i`; one further entry,
    /// `text.len()`, closes the string off. A character outside the BMP
    /// repeats its one byte offset across both halves of its surrogate pair,
    /// which is what makes [`Index::utf16_of_byte`] find the pair's first half
    /// for either one.
    byte_at: Vec<usize>,
}

impl Index {
    fn new(text: String) -> Self {
        let mut byte_at = Vec::with_capacity(text.len() + 1);
        let mut byte = 0usize;
        for c in text.chars() {
            for _ in 0..c.len_utf16() {
                byte_at.push(byte);
            }
            byte += c.len_utf8();
        }
        byte_at.push(byte);
        Index { text, byte_at }
    }

    fn utf16_len(&self) -> i64 {
        (self.byte_at.len() - 1) as i64
    }

    fn byte_of(&self, utf16: i64) -> Option<usize> {
        self.byte_at.get(usize::try_from(utf16).ok()?).copied()
    }

    /// The UTF-16 index of the character starting at `byte` - always an exact
    /// boundary, since every match or capture `fancy_regex` reports starts and
    /// ends on one.
    fn utf16_of_byte(&self, byte: usize) -> i64 {
        self.byte_at.partition_point(|&b| b < byte) as i64
    }

    fn range_of(&self, start: usize, end: usize) -> NSRange {
        let location = self.utf16_of_byte(start);
        let length = self.utf16_of_byte(end) - location;
        NSRange {
            location: location as usize,
            length: length as usize,
        }
    }
}

//
// NSTextCheckingResult
//

struct ResultBody {
    #[allow(dead_code)]
    isa: *const Class,
    contents: *mut Vec<NSRange>,
}

fn result_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let p = *CLASS.get_or_init(|| {
        let class = corefoundation::base::register(
            c"NSTextCheckingResult",
            None,
            std::mem::size_of::<ResultBody>(),
            false,
        );
        unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
            // SAFETY: reached with the last reference gone.
            unsafe {
                let body = receiver as *mut ResultBody;
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
    unsafe { objc::class::from_ptr(p as *const Class).expect("a text checking result class") }
}

/// A fresh, autoreleased `NSTextCheckingResult`. `ranges[0]` is the whole
/// match; every later one is a capture group, `{NSNotFound, 0}` where the
/// group did not participate.
fn make_result(ranges: Vec<NSRange>) -> Id {
    // SAFETY: `result_class()`'s instances are `ResultBody`.
    let body: *mut ResultBody = unsafe { corefoundation::base::allocate(result_class()) };
    if body.is_null() {
        return body as Id;
    }
    // SAFETY: a live instance whose second word is ours to fill in.
    unsafe { (*body).contents = Box::into_raw(Box::new(ranges)) };
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(body as CFTypeRef) as Id }
}

/// # Safety
///
/// `receiver` must be null or an object this runtime allocated.
unsafe fn result_ranges<'a>(receiver: Id) -> Option<&'a Vec<NSRange>> {
    if !unsafe { corefoundation::base::is_kind_of(receiver as CFTypeRef, result_class()) } {
        return None;
    }
    // SAFETY: an instance of `result_class()` is a `ResultBody`.
    unsafe {
        let body = receiver as *mut ResultBody;
        (!(*body).contents.is_null()).then(|| &*(*body).contents)
    }
}

unsafe extern "C" fn result_range(receiver: Id, _sel: Sel) -> NSRange {
    // SAFETY: an `NSTextCheckingResult`.
    unsafe { result_ranges(receiver) }
        .and_then(|r| r.first())
        .copied()
        .unwrap_or(NSRange {
            location: NOT_FOUND as usize,
            length: 0,
        })
}

unsafe extern "C" fn result_range_at_index(receiver: Id, _sel: Sel, index: usize) -> NSRange {
    // SAFETY: an `NSTextCheckingResult`.
    unsafe { result_ranges(receiver) }
        .and_then(|r| r.get(index))
        .copied()
        .unwrap_or(NSRange {
            location: NOT_FOUND as usize,
            length: 0,
        })
}

unsafe extern "C" fn result_number_of_ranges(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: an `NSTextCheckingResult`.
    unsafe { result_ranges(receiver) }.map_or(0, |r| r.len())
}

fn install_result() {
    let class = result_class();
    methods!(class, {
        "range" => result_range, c"{_NSRange=QQ}16@0:8";
        "rangeAtIndex:" => result_range_at_index, c"{_NSRange=QQ}24@0:8Q16";
        "numberOfRanges" => result_number_of_ranges, c"Q16@0:8";
    });
}

//
// NSRegularExpression
//

struct Body {
    #[allow(dead_code)]
    isa: *const Class,
    contents: *mut Contents,
}

struct Contents {
    /// Retained.
    pattern: Id,
    options: u64,
    regex: fancy_regex::Regex,
}

impl Drop for Contents {
    fn drop(&mut self) {
        objc::arc::release(self.pattern);
    }
}

fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let p = *CLASS.get_or_init(|| {
        let class = corefoundation::base::register(
            c"NSRegularExpression",
            None,
            std::mem::size_of::<Body>(),
            false,
        );
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
    unsafe { objc::class::from_ptr(p as *const Class).expect("a regular expression class") }
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

/// Every non-word, non-underscore character in `pattern` backslash-escaped,
/// for `NSRegularExpressionOptions.IgnoreMetacharacters`: the pattern matches
/// its own literal text.
fn escape(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() * 2);
    for c in pattern.chars() {
        if !c.is_alphanumeric() && c != '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn compile(pattern: &str, flags: u64) -> Result<fancy_regex::Regex, String> {
    let literal = flags & options::IGNORE_METACHARACTERS != 0;
    let owned = if literal {
        escape(pattern)
    } else {
        pattern.to_string()
    };
    fancy_regex::RegexBuilder::new(&owned)
        .case_insensitive(flags & options::CASE_INSENSITIVE != 0)
        .ignore_whitespace(flags & options::ALLOW_COMMENTS_AND_WHITESPACE != 0)
        .dot_matches_new_line(flags & options::DOT_MATCHES_LINE_SEPARATORS != 0)
        .multi_line(flags & options::ANCHORS_MATCH_LINES != 0)
        .build()
        .map_err(|e| e.to_string())
}

fn make(pattern: Id, flags: u64) -> Result<Id, String> {
    // SAFETY: an `NSString`, or nil.
    let text = unsafe { crate::string::bytes(pattern) };
    let text = String::from_utf8(text).map_err(|e| e.to_string())?;
    let regex = compile(&text, flags)?;
    // SAFETY: `class()`'s instances are `Body`.
    let body: *mut Body = unsafe { corefoundation::base::allocate(class()) };
    if body.is_null() {
        return Err("allocation failed".to_string());
    }
    // SAFETY: a live instance whose second word is ours to fill in.
    unsafe {
        (*body).contents = Box::into_raw(Box::new(Contents {
            pattern: objc::arc::retain(pattern),
            options: flags,
            regex,
        }));
    }
    Ok(body as Id)
}

fn fail(error: *mut Id, message: &str) {
    if error.is_null() {
        return;
    }
    let object = crate::error::make("NSCocoaErrorDomain", 2048, message);
    // SAFETY: the guest handed us the address of its own `NSError *`.
    unsafe { *error = object };
}

unsafe extern "C" fn init_with_pattern_options_error(
    _receiver: Id,
    _sel: Sel,
    pattern: Id,
    options: u64,
    error: *mut Id,
) -> Id {
    if !error.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { *error = std::ptr::null_mut() };
    }
    match make(pattern, options) {
        Ok(object) => object,
        Err(why) => {
            fail(error, &why);
            std::ptr::null_mut()
        }
    }
}

unsafe extern "C" fn regular_expression_with_pattern_options_error(
    class_object: Id,
    sel: Sel,
    pattern: Id,
    options: u64,
    error: *mut Id,
) -> Id {
    // SAFETY: the same three arguments, forwarded.
    let object =
        unsafe { init_with_pattern_options_error(class_object, sel, pattern, options, error) };
    if object.is_null() {
        return object;
    }
    // SAFETY: a fresh object this function owns a reference to.
    unsafe { corefoundation::api::CFAutorelease(object as CFTypeRef) as Id }
}

unsafe extern "C" fn get_pattern(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSRegularExpression`.
    unsafe { contents(receiver) }.map_or(std::ptr::null_mut(), |c| c.pattern)
}

unsafe extern "C" fn get_options(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: an `NSRegularExpression`.
    unsafe { contents(receiver) }.map_or(0, |c| c.options)
}

/// The `Index` and the byte bounds of `range`, or `None` for a `range` past
/// the end of `string` - the whole family of matching methods refuses on one
/// the same way, by answering nothing found.
///
/// # Safety
///
/// `string` must be null or an `NSString`.
unsafe fn indexed(string: Id, range: NSRange) -> Option<(Index, usize, usize)> {
    // SAFETY: the caller's obligation.
    let bytes = unsafe { crate::string::bytes(string) };
    let text = String::from_utf8(bytes).ok()?;
    let index = Index::new(text);
    let start = i64::try_from(range.location).ok()?;
    let end = start.checked_add(i64::try_from(range.length).ok()?)?;
    if end > index.utf16_len() {
        return None;
    }
    let byte_start = index.byte_of(start)?;
    let byte_end = index.byte_of(end)?;
    Some((index, byte_start, byte_end))
}

/// Every match within `[byte_start, byte_end)` of `index.text`, as
/// `(whole_match_bytes, capture_bytes)` pairs - `capture_bytes[i]` is `None`
/// for a group that did not participate.
fn find_all(
    regex: &fancy_regex::Regex,
    index: &Index,
    byte_start: usize,
    byte_end: usize,
) -> Vec<((usize, usize), Vec<Option<(usize, usize)>>)> {
    let Some(haystack) = index.text.get(byte_start..byte_end) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for found in regex.captures_iter(haystack) {
        let Ok(captures) = found else { break };
        let Some(whole) = captures.get(0) else {
            continue;
        };
        let groups: Vec<Option<(usize, usize)>> = (1..captures.len())
            .map(|i| {
                captures
                    .get(i)
                    .map(|m| (byte_start + m.start(), byte_start + m.end()))
            })
            .collect();
        out.push((
            (byte_start + whole.start(), byte_start + whole.end()),
            groups,
        ));
    }
    out
}

unsafe extern "C" fn number_of_matches(
    receiver: Id,
    _sel: Sel,
    string: Id,
    _options: u64,
    range: NSRange,
) -> usize {
    let Some(contents) = (unsafe { contents(receiver) }) else {
        return 0;
    };
    let Some((index, start, end)) = (unsafe { indexed(string, range) }) else {
        return 0;
    };
    find_all(&contents.regex, &index, start, end).len()
}

/// A single match's `NSTextCheckingResult`, or nil for none - the body of both
/// `-firstMatchInString:...` and one element of `-matchesInString:...`.
fn result_for(index: &Index, whole: (usize, usize), groups: &[Option<(usize, usize)>]) -> Id {
    let mut ranges = vec![index.range_of(whole.0, whole.1)];
    ranges.extend(groups.iter().map(|g| match g {
        Some((a, b)) => index.range_of(*a, *b),
        None => NSRange {
            location: NOT_FOUND as usize,
            length: 0,
        },
    }));
    make_result(ranges)
}

unsafe extern "C" fn first_match(
    receiver: Id,
    _sel: Sel,
    string: Id,
    _options: u64,
    range: NSRange,
) -> Id {
    let Some(contents) = (unsafe { contents(receiver) }) else {
        return std::ptr::null_mut();
    };
    let Some((index, start, end)) = (unsafe { indexed(string, range) }) else {
        return std::ptr::null_mut();
    };
    let found = find_all(&contents.regex, &index, start, end);
    match found.first() {
        Some((whole, groups)) => result_for(&index, *whole, groups),
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn range_of_first_match(
    receiver: Id,
    sel: Sel,
    string: Id,
    options: u64,
    range: NSRange,
) -> NSRange {
    // SAFETY: the same three arguments.
    let result = unsafe { first_match(receiver, sel, string, options, range) };
    if result.is_null() {
        return NSRange {
            location: NOT_FOUND as usize,
            length: 0,
        };
    }
    // SAFETY: an `NSTextCheckingResult` this call just made.
    unsafe { result_range(result, sel) }
}

unsafe extern "C" fn matches_in_string(
    receiver: Id,
    _sel: Sel,
    string: Id,
    _options: u64,
    range: NSRange,
) -> Id {
    let values: Vec<CFTypeRef> = match unsafe { contents(receiver) }
        .and_then(|contents| unsafe { indexed(string, range) }.map(|i| (contents, i)))
    {
        Some((contents, (index, start, end))) => find_all(&contents.regex, &index, start, end)
            .into_iter()
            .map(|(whole, groups)| result_for(&index, whole, &groups) as CFTypeRef)
            .collect(),
        None => Vec::new(),
    };
    let array = corefoundation::collections::new_array(
        &values,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

/// Substitute `$0`, `$1`, ... in `template` with the matching group's text -
/// `$0` is the whole match, an unparticipating group is the empty string, and
/// `\$` is a literal dollar sign - `NSRegularExpression`'s own template syntax.
fn expand_template(
    template: &str,
    text: &str,
    whole: (usize, usize),
    groups: &[Option<(usize, usize)>],
) -> String {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'$') {
            out.push('$');
            chars.next();
            continue;
        }
        if c != '$' {
            out.push(c);
            continue;
        }
        let mut digits = String::new();
        while let Some(d) = chars.peek() {
            if d.is_ascii_digit() {
                digits.push(*d);
                chars.next();
            } else {
                break;
            }
        }
        let Ok(n) = digits.parse::<usize>() else {
            continue;
        };
        if n == 0 {
            out.push_str(&text[whole.0..whole.1]);
        } else if let Some(Some((a, b))) = groups.get(n - 1) {
            out.push_str(&text[*a..*b]);
        }
    }
    out
}

unsafe extern "C" fn string_by_replacing_matches(
    receiver: Id,
    _sel: Sel,
    string: Id,
    _options: u64,
    range: NSRange,
    template: Id,
) -> Id {
    let Some(contents) = (unsafe { contents(receiver) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: an `NSString`, or nil.
    let Ok(template) = String::from_utf8(unsafe { crate::string::bytes(template) }) else {
        return std::ptr::null_mut();
    };
    let Some((index, start, end)) = (unsafe { indexed(string, range) }) else {
        return std::ptr::null_mut();
    };
    let matches = find_all(&contents.regex, &index, start, end);
    let mut out = String::new();
    out.push_str(&index.text[..start]);
    let mut at = start;
    for (whole, groups) in &matches {
        out.push_str(&index.text[at..whole.0]);
        out.push_str(&expand_template(&template, &index.text, *whole, groups));
        at = whole.1;
    }
    out.push_str(&index.text[at..]);
    // SAFETY: a fresh object.
    unsafe {
        corefoundation::api::CFAutorelease(corefoundation::string::from_bytes(
            out.as_bytes(),
            false,
        )) as Id
    }
}

pub(crate) fn install() {
    install_result();
    let class = class();
    class_methods!(class, {
        "regularExpressionWithPattern:options:error:" => regular_expression_with_pattern_options_error,
            c"@40@0:8@16Q24^@32";
    });
    methods!(class, {
        "initWithPattern:options:error:" => init_with_pattern_options_error, c"@40@0:8@16Q24^@32";
        "pattern" => get_pattern, c"@16@0:8";
        "options" => get_options, c"Q16@0:8";
        "numberOfMatchesInString:options:range:" => number_of_matches,
            c"Q48@0:8@16Q24{_NSRange=QQ}32";
        "firstMatchInString:options:range:" => first_match, c"@48@0:8@16Q24{_NSRange=QQ}32";
        "rangeOfFirstMatchInString:options:range:" => range_of_first_match,
            c"{_NSRange=QQ}48@0:8@16Q24{_NSRange=QQ}32";
        "matchesInString:options:range:" => matches_in_string, c"@48@0:8@16Q24{_NSRange=QQ}32";
        "stringByReplacingMatchesInString:options:range:withTemplate:" => string_by_replacing_matches,
            c"@56@0:8@16Q24{_NSRange=QQ}32@48";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_regex(pattern: &str, options: u64) -> Id {
        let pattern = crate::string::from_str(pattern);
        let mut error: Id = std::ptr::null_mut();
        // SAFETY: a live pattern string.
        let regex = unsafe {
            init_with_pattern_options_error(
                std::ptr::null_mut(),
                objc::sel::get("x"),
                pattern,
                options,
                &mut error,
            )
        };
        objc::arc::release(pattern);
        assert!(!regex.is_null(), "the pattern should have compiled");
        regex
    }

    fn whole_range(location: usize, length: usize) -> NSRange {
        NSRange { location, length }
    }

    #[test]
    fn counts_and_finds_matches_by_utf16_range() {
        crate::bootstrap();
        let regex = make_regex(r"\d+", 0);
        let text = crate::string::from_str("a12 b345");
        let sel = objc::sel::get("x");
        // SAFETY: a live regex and string.
        unsafe {
            assert_eq!(number_of_matches(regex, sel, text, 0, whole_range(0, 8)), 2);
            let range = range_of_first_match(regex, sel, text, 0, whole_range(0, 8));
            assert_eq!(range, whole_range(1, 2), "the first run of digits, \"12\"");
        }
    }

    #[test]
    fn case_insensitive_matches_either_case() {
        crate::bootstrap();
        let regex = make_regex("hello", options::CASE_INSENSITIVE);
        let text = crate::string::from_str("HELLO there");
        let sel = objc::sel::get("x");
        // SAFETY: a live regex and string.
        unsafe {
            assert_eq!(
                number_of_matches(regex, sel, text, 0, whole_range(0, 11)),
                1
            );
        }
    }

    #[test]
    fn capture_groups_are_readable_from_the_checking_result() {
        crate::bootstrap();
        let regex = make_regex(r"(\d+)-(\d+)", 0);
        let text = crate::string::from_str("12-34");
        let sel = objc::sel::get("x");
        // SAFETY: a live regex and string.
        unsafe {
            let result = first_match(regex, sel, text, 0, whole_range(0, 5));
            assert!(!result.is_null());
            assert_eq!(
                result_number_of_ranges(result, sel),
                3,
                "whole match + two groups"
            );
            assert_eq!(result_range_at_index(result, sel, 1), whole_range(0, 2));
            assert_eq!(result_range_at_index(result, sel, 2), whole_range(3, 2));
        }
    }

    #[test]
    fn replacing_matches_substitutes_capture_groups_into_the_template() {
        crate::bootstrap();
        let regex = make_regex(r"(\w+)@(\w+)", 0);
        let text = crate::string::from_str("user@host");
        let template = crate::string::from_str("$2:$1");
        let sel = objc::sel::get("x");
        // SAFETY: live objects.
        unsafe {
            let replaced =
                string_by_replacing_matches(regex, sel, text, 0, whole_range(0, 9), template);
            assert_eq!(crate::string::bytes(replaced), b"host:user");
        }
    }

    #[test]
    fn a_surrogate_pair_does_not_desynchronize_later_offsets() {
        crate::bootstrap();
        // U+1F600 (a four-byte, two-UTF-16-unit emoji) followed by digits.
        let regex = make_regex(r"\d+", 0);
        let text = crate::string::from_str("\u{1F600}42");
        let sel = objc::sel::get("x");
        // SAFETY: live objects.
        unsafe {
            let range = range_of_first_match(regex, sel, text, 0, whole_range(0, 4));
            assert_eq!(range, whole_range(2, 2), "after the two-unit emoji");
        }
    }

    #[test]
    fn an_invalid_pattern_fails_with_an_error_rather_than_a_panic() {
        crate::bootstrap();
        let pattern = crate::string::from_str("(unterminated");
        let mut error: Id = std::ptr::null_mut();
        // SAFETY: a live pattern string.
        let regex = unsafe {
            init_with_pattern_options_error(
                std::ptr::null_mut(),
                objc::sel::get("x"),
                pattern,
                0,
                &mut error,
            )
        };
        assert!(regex.is_null());
        assert!(!error.is_null());
    }
}
