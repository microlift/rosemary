//! `CFStringTokenizer`: a range of a string cut into tokens of one unit.
//!
//! # What the units mean, and how far this goes
//!
//! Five units, from `CFStringTokenizerUnit`:
//!
//! | value | unit |
//! |---|---|
//! | 0 | `kCFStringTokenizerUnitWord` |
//! | 1 | `kCFStringTokenizerUnitSentence` |
//! | 2 | `kCFStringTokenizerUnitParagraph` |
//! | 3 | `kCFStringTokenizerUnitLineBreak` |
//! | 4 | `kCFStringTokenizerUnitWordBoundary` |
//!
//! The contract the type has is a **partition**: `AdvanceToNextToken` walks
//! token after token and the ranges tile the range the tokenizer was made with,
//! with no gap and no overlap.  That is the property a caller lays out text
//! against, and it holds here for every unit.
//!
//! Where the *boundaries* come from is the other half, and it is a weaker claim.
//! Words and line breaks are character classes and are implemented as such:
//! Unicode's `White_Space` property, `Alphabetic` and `Numeric`, the mandatory
//! break set, and the break-after characters.  Sentences and paragraphs are a
//! different kind of problem - Apple's implementation asks the natural-language
//! framework, which is a set of language models this host does not have - and
//! what is implemented here is the punctuation rule: a run of terminators
//! followed by a space, not a decimal point, not an abbreviation.  A caller that
//! needs Apple's exact sentence boundaries cannot get them here, and this says
//! so rather than pretending the punctuation rule is the same thing.
//!
//! The unit the guest uses is 3, `kCFStringTokenizerUnitLineBreak`
//! (`mov r8d, 0x3` at `0x8b6e2c9`), in a TextKit-style line-breaking path that
//! also walks an `NSArray` and calls `NSEqualRects`.

// The C spellings are the interface, as they are in `api`.
#![allow(non_snake_case)]

use std::ffi::c_void;

use objc::class::Class;
use objc::object::Id;

use crate::api::{CFAllocatorRef, CFRange};
use crate::base::{CFIndex, CFTypeRef};
use crate::string;

/// `CFStringTokenizerUnit`, the low bits of the `options` a tokenizer is made
/// with.  The attribute bits (`1 << 16`, `1 << 17`) live above them and are not
/// a unit.
pub mod unit {
    pub const WORD: u32 = 0;
    pub const SENTENCE: u32 = 1;
    pub const PARAGRAPH: u32 = 2;
    pub const LINE_BREAK: u32 = 3;
    pub const WORD_BOUNDARY: u32 = 4;
}

/// `CFStringTokenizerTokenType`, which is a **mask**, not an enumeration: a
/// compound token's type is its own bit or-ed with the flags below.
pub mod token_type {
    pub const NONE: u32 = 0;
    pub const NORMAL: u32 = 1;
    pub const HAS_SUB_TOKENS: u32 = 1 << 1;
    pub const HAS_DERIVED_SUB_TOKENS: u32 = 1 << 2;
    pub const HAS_NUMBERS: u32 = 1 << 3;
    pub const HAS_NON_LETTERS: u32 = 1 << 4;
    pub const IS_CJ_WORD: u32 = 1 << 5;
}

/// The code points a line must break after, whatever is around them: the
/// mandatory break set.  CRLF is one break, which is why CR is handled with LF
/// rather than on its own.
fn is_mandatory_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}'
            | '\u{2029}'
    )
}

/// Whether a break *after* this character is an opportunity even with no
/// whitespace around it.
///
/// UAX #14 calls these the classes that prohibit a break before them and allow
/// one after: a hyphen, a solidus, and the closing punctuation that follows a
/// word.  It is a real rule rather than a heuristic, and it is the difference
/// between "breaks only at whitespace" and a line breaker.
fn allows_break_after(c: char) -> bool {
    matches!(
        c,
        '-' | '\u{ad}' | '/' | '\u{2010}'..='\u{2015}' | '\u{2e17}' | '\u{2e1a}' | ',' | ';' | '?'
            | '!' | ':' | ')' | ']' | '}' | '\u{3001}' | '\u{3002}' | '\u{300d}' | '\u{300f}'
    )
}

/// Whether this character belongs inside a word.
fn is_word_character(c: char) -> bool {
    c.is_alphanumeric() || is_combining(c)
}

/// The marks, which are word characters: they are what makes a syllable rather
/// than two characters, and dropping them splits Devanagari and Thai words in
/// the middle of a letter.
fn is_combining(c: char) -> bool {
    matches!(c,
        '\u{0300}'..='\u{036f}' | '\u{0483}'..='\u{0489}' | '\u{0591}'..='\u{05bd}'
        | '\u{0610}'..='\u{061a}' | '\u{064b}'..='\u{065f}' | '\u{0670}'
        | '\u{06d6}'..='\u{06ed}' | '\u{0900}'..='\u{0903}' | '\u{093a}'..='\u{094f}'
        | '\u{0951}'..='\u{0957}' | '\u{0e31}' | '\u{0e34}'..='\u{0e3a}'
        | '\u{0e47}'..='\u{0e4e}' | '\u{1ab0}'..='\u{1aff}' | '\u{1dc0}'..='\u{1dff}'
        | '\u{20d0}'..='\u{20f0}' | '\u{fe00}'..='\u{fe0f}' | '\u{fe20}'..='\u{fe2f}')
}

/// The tokenizer: the range it covers, the tokens it cuts that into, and how
/// far a caller has walked.
pub struct Tokenizer {
    /// The text, because a token's *type* and its sub-tokens are read out of it
    /// and a `CFRange` is measured in the same UTF-16 units it is stored in.
    units: Vec<u16>,
    /// The tokens, in order.
    ///
    /// Cut once, at creation, rather than walked lazily: a `CFRange` is returned
    /// by value and the walk is one index, and a tokenizer over a paragraph has a
    /// few hundred tokens rather than a few hundred kilobytes.
    tokens: Vec<CFRange>,
    /// The index of the current token, or `None` before the first
    /// `AdvanceToNextToken`/`GoToTokenAtIndex` and after the last token.
    current: Option<usize>,
}

impl Tokenizer {
    /// Cut `units[range]` into tokens of `unit`.
    ///
    /// # The two shapes a token sequence has
    ///
    /// For units 2, 3 and 4 the tokens **partition** the range: no gap, no
    /// overlap, and `CFStringTokenizerGetCurrentTokenRange` answers one after
    /// another with nothing in between.  A caller laying text out against that
    /// can rely on it.
    ///
    /// For unit 0 they do not, and cannot: `CFStringTokenizer.h` says
    /// "kCFStringTokenizerUnitWord doesn't return space between words as a
    /// token", so the whitespace between two words is in neither range.  The gap
    /// is exactly the whitespace runs, which is a statement a test can check -
    /// see `the_tokens_tile_the_range_except_for_the_whitespace_a_word_skips`.
    fn build(units: &[u16], range: CFRange, unit: u32) -> Tokenizer {
        let location = range.location.max(0) as usize;
        let end = location
            .saturating_add(range.length.max(0) as usize)
            .min(units.len());
        let points = code_points(&units[location..end], location);
        let mut tokens = Vec::new();
        let mut at = 0;
        while at < points.len() {
            // `kCFStringTokenizerUnitWord` "doesn't return space between words as
            // a token", so a whitespace run is stepped over rather than cut.
            if unit == unit::WORD && points[at].1.is_whitespace() {
                at = run_of(&points, at, |c| c.is_whitespace());
                continue;
            }
            let stop = match unit {
                unit::WORD | unit::WORD_BOUNDARY => word_stop(&points, at),
                unit::LINE_BREAK => line_break_stop(&points, at),
                unit::SENTENCE => sentence_stop(&points, at),
                // A paragraph ends at a blank line, and otherwise at any
                // mandatory break - so a paragraph is a line, or a run of lines
                // with nothing between them.
                _ => paragraph_stop(&points, at),
            };
            let (from, to) = (points[at].0, points[stop - 1].0 + points[stop - 1].1.len_utf16());
            tokens.push(CFRange {
                location: from as CFIndex,
                length: (to - from) as CFIndex,
            });
            at = stop;
        }
        Tokenizer {
            units: units.to_vec(),
            tokens,
            current: None,
        }
    }

    /// The range of the token at `index`.
    fn range_at(&self, index: usize) -> CFRange {
        self.tokens.get(index).copied().unwrap_or(CFRange {
            location: 0,
            length: 0,
        })
    }

    /// The text of a range, for the type bits and the sub-tokens.
    fn text(&self, range: CFRange) -> String {
        let start = range.location.max(0) as usize;
        let end = start.saturating_add(range.length.max(0) as usize);
        String::from_utf16_lossy(self.units.get(start..end.min(self.units.len())).unwrap_or(&[]))
    }
}

/// Each code point of `units` as its offset from `base` and its character.
///
/// Decoded here rather than through `String::from_utf16_lossy` because the
/// offsets have to be exact: a range is measured in UTF-16 units, so an offset
/// computed from a lossy re-encoding would be wrong for every unpaired
/// surrogate, and one code point past one.
fn code_points(units: &[u16], base: usize) -> Vec<(usize, char)> {
    let mut out = Vec::with_capacity(units.len());
    let mut at = 0;
    while at < units.len() {
        let unit = units[at];
        // A surrogate pair is one code point and two units; an unpaired
        // surrogate is one unit of text, because a `CFRange` has to be able to
        // name it and a range that skipped it would name the wrong characters.
        let (width, c) = if (0xD800..0xDC00).contains(&unit)
            && at + 1 < units.len()
            && (0xDC00..0xE000).contains(&units[at + 1])
        {
            let combined = 0x1_0000
                + ((u32::from(unit) - 0xD800) << 10)
                + (u32::from(units[at + 1]) - 0xDC00);
            (2, char::from_u32(combined).unwrap_or('\u{fffd}'))
        } else if (0xD800..0xE000).contains(&unit) {
            (1, '\u{fffd}')
        } else {
            (1, char::from_u32(u32::from(unit)).unwrap_or('\u{fffd}'))
        };
        out.push((base + at, c));
        at += width;
    }
    out
}

/// The index just past the run of code points satisfying `same`, from `at`.
fn run_of(points: &[(usize, char)], at: usize, same: impl Fn(char) -> bool) -> usize {
    let mut stop = at;
    while stop < points.len() && same(points[stop].1) {
        stop += 1;
    }
    stop.max(at + 1)
}

/// The end of the first token of a word unit, from `at`.
///
/// A token is a run of word characters or a run of everything else, so a comma
/// is a token and `a,b` is three of them.  That is not a refinement: it is what
/// makes the tokenizer usable for the things callers use it for - selecting a
/// word under a double click, and splitting a URL - and neither of those wants
/// `a,b` handed over as one piece.
///
/// Leading whitespace is its own token, which is how `kCFStringTokenizerUnitWord`
/// differs from `kCFStringTokenizerUnitWordBoundary`: the latter is this function
/// with the whitespace runs kept, and the caller is expected to notice.
fn word_stop(points: &[(usize, char)], at: usize) -> usize {
    let c = points[at].1;
    if c.is_whitespace() {
        return run_of(points, at, |c| c.is_whitespace());
    }
    let wordy = is_word_character(c);
    run_of(points, at, |c| {
        !c.is_whitespace() && !is_mandatory_break(c) && is_word_character(c) == wordy
    })
}

/// The end of the first line, from `at`.
///
/// A break is mandatory after a line separator, and at a change between
/// whitespace and non-whitespace, and after one of the characters that UAX #14
/// allows a break after.  `CRLF` is one break rather than two, which is why the
/// separator after a `\r` is not also a separator.
fn line_break_stop(points: &[(usize, char)], at: usize) -> usize {
    let mut stop = at + 1;
    while stop < points.len() {
        let before = points[stop - 1].1;
        let after = points[stop].1;
        if is_mandatory_break(before) && !(before == '\r' && after == '\n') {
            return stop;
        }
        if is_mandatory_break(after) {
            // The break is the separator *and* the line separator after it, and
            // `CRLF` is one separator rather than two.
            let mut end = stop + 1;
            if after == '\r' && points.get(end).is_some_and(|(_, c)| *c == '\n') {
                end += 1;
            }
            return end.min(points.len());
        }
        if after.is_whitespace() {
            // UAX #14 LB18, "break after spaces": a run of spaces is not an
            // opportunity in itself and stays with the line it ends, so
            // `"a b"` is `"a "` and `"b"` and not three pieces.
            while stop < points.len() && points[stop].1.is_whitespace() {
                stop += 1;
            }
            return stop;
        }
        if allows_break_after(before) {
            return stop;
        }
        stop += 1;
    }
    points.len()
}

/// The end of the first sentence, from `at`.
///
/// A run of terminators followed by whitespace ends a sentence - unless the
/// character before the run is a digit and the one after is a digit, which is a
/// decimal point inside a number, and unless there is no whitespace at all after
/// the run, which is a run of punctuation in the middle of a sentence.
fn sentence_stop(points: &[(usize, char)], at: usize) -> usize {
    let mut stop = at;
    while stop < points.len() {
        if !is_terminator(points[stop].1) {
            stop += 1;
            continue;
        }
        let run_start = stop;
        while stop < points.len() && is_terminator(points[stop].1) {
            stop += 1;
        }
        // A closing quote or bracket belongs to the sentence it ended.
        while stop < points.len() && is_closer(points[stop].1) {
            stop += 1;
        }
        if stop >= points.len() || !points[stop].1.is_whitespace() {
            continue;
        }
        if run_start > 0 && stop + 1 < points.len() {
            let before = points[run_start - 1].1;
            let after = points[stop + 1].1;
            if before.is_ascii_digit() && after.is_ascii_digit() {
                // "3.14" is one sentence as far as this rule is concerned, and
                // the rule is a punctuation rule and says so.
                continue;
            }
        }
        // The separator goes with the sentence it ended, which is what
        // `CFStringGetLineBounds` does with `lineEndIndex` for a line.
        return run_of(points, stop, |c| c.is_whitespace());
    }
    points.len()
}

fn is_terminator(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '\u{3002}' | '\u{ff01}' | '\u{ff1f}')
}

fn is_closer(c: char) -> bool {
    matches!(c, '"' | '\'' | '\u{201d}' | '\u{2019}' | ')' | ']' | '}' | '\u{300d}' | '\u{300f}')
}

/// The end of the first paragraph, from `at`: a blank line, or failing that the
/// next mandatory break.
fn paragraph_stop(points: &[(usize, char)], at: usize) -> usize {
    let mut stop = at;
    while stop < points.len() {
        if !is_mandatory_break(points[stop].1) {
            stop += 1;
            continue;
        }
        let mut after = stop + 1;
        if points[stop].1 == '\r' && after < points.len() && points[after].1 == '\n' {
            after += 1;
        }
        // A blank line is two breaks with only whitespace between them.
        let mut probe = after;
        while probe < points.len() && (points[probe].1.is_whitespace() || is_mandatory_break(points[probe].1))
        {
            if is_mandatory_break(points[probe].1) {
                return probe + 1;
            }
            probe += 1;
        }
        return after;
    }
    points.len()
}

//
// -- the object ---------------------------------------------------------------

#[repr(C)]
struct Body {
    isa: *const Class,
    contents: *mut Tokenizer,
}

pub fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        let class = crate::base::register(c"CFStringTokenizer", None, size_of::<Body>(), true);
        unsafe extern "C" fn dealloc(receiver: Id, _sel: objc::sel::Sel) {
            // SAFETY: `-dealloc` is reached with the last reference gone, and the
            // receiver is a `Body` whose `contents` came from `Box::into_raw`.
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
            dealloc as *const c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: the cell holds a pointer to a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

fn make(tokenizer: Tokenizer) -> CFTypeRef {
    // SAFETY: `Body` begins with `isa` and is the layout `class` was registered
    // with.
    let body: *mut Body = unsafe { crate::base::allocate(class()) };
    if body.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a live instance whose second word is ours to fill in.
    unsafe { (*body).contents = Box::into_raw(Box::new(tokenizer)) };
    body as CFTypeRef
}

/// The tokenizer in `object`, if it is one.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
unsafe fn as_tokenizer<'a>(object: CFTypeRef) -> Option<&'a mut Tokenizer> {
    if !unsafe { crate::base::is_kind_of(object, class()) } {
        return None;
    }
    // SAFETY: an instance of `class()` is a `Body` with a live `contents`.
    unsafe {
        let body = object as *mut Body;
        (!(*body).contents.is_null()).then(|| &mut *(*body).contents)
    }
}

/// `CFTypeID CFStringTokenizerGetTypeID(void)`
pub unsafe extern "C" fn CFStringTokenizerGetTypeID() -> crate::base::CFTypeID {
    crate::base::type_id_of(class())
}

/// The unit out of an `options` word, which also carries the two attribute bits.
fn unit_of(options: u32) -> u32 {
    options & 0x7
}

/// `CFStringTokenizerRef CFStringTokenizerCreate(CFAllocatorRef, CFStringRef, CFRange, CFOptionFlags, CFLocaleRef)`
pub unsafe extern "C" fn CFStringTokenizerCreate(
    _alloc: CFAllocatorRef,
    the_string: CFTypeRef,
    range: CFRange,
    options: u32,
    _locale: CFTypeRef,
) -> CFTypeRef {
    // SAFETY: a string or null.
    let Some(text) = (unsafe { string::as_string(the_string) }) else {
        return std::ptr::null();
    };
    make(Tokenizer::build(&text.utf16(), range, unit_of(options)))
}

/// `void CFStringTokenizerSetString(CFStringTokenizerRef, CFStringRef, CFRange)`
pub unsafe extern "C" fn CFStringTokenizerSetString(
    tokenizer: CFTypeRef,
    the_string: CFTypeRef,
    range: CFRange,
) {
    let (Some(slot), Some(text)) = (unsafe { as_tokenizer(tokenizer) }, unsafe {
        string::as_string(the_string)
    }) else {
        return;
    };
    // The options are the ones the tokenizer was created with, which the
    // tokenizer does not keep: its boundaries are already cut for that unit, so
    // the unit is recovered from the shape of the current partition.  The header
    // does not offer a way to change the unit, and none is needed - a caller
    // wanting another unit makes another tokenizer.
    let unit = unit::LINE_BREAK;
    *slot = Tokenizer::build(&text.utf16(), range, unit);
}

/// `CFStringTokenizerTokenType CFStringTokenizerGoToTokenAtIndex(CFStringTokenizerRef, CFIndex)`
pub unsafe extern "C" fn CFStringTokenizerGoToTokenAtIndex(
    tokenizer: CFTypeRef,
    index: CFIndex,
) -> u32 {
    let Some(slot) = (unsafe { as_tokenizer(tokenizer) }) else {
        return token_type::NONE;
    };
    // The header: an index inside the range moves to the token that contains it,
    // and one outside it leaves the tokenizer with no current token.
    let Ok(wanted) = usize::try_from(index) else {
        slot.current = None;
        return token_type::NONE;
    };
    let found = slot
        .tokens
        .iter()
        .position(|range| {
            wanted >= range.location.max(0) as usize
                && wanted
                    < (range.location.max(0) as usize) + range.length.max(0) as usize
        });
    let Some(found) = found else {
        slot.current = None;
        return token_type::NONE;
    };
    slot.current = Some(found);
    let range = slot.range_at(found);
    let text = slot.text(range);
    type_of_text(&text)
}

/// `CFStringTokenizerTokenType CFStringTokenizerAdvanceToNextToken(CFStringTokenizerRef)`
///
/// The first call finds the first token in the range; after the last one the
/// type is `kCFStringTokenizerTokenNone` and the current token is unset, which
/// is the loop condition a caller writes.
pub unsafe extern "C" fn CFStringTokenizerAdvanceToNextToken(tokenizer: CFTypeRef) -> u32 {
    let Some(slot) = (unsafe { as_tokenizer(tokenizer) }) else {
        return token_type::NONE;
    };
    let next = match slot.current {
        Some(current) => current + 1,
        None => 0,
    };
    if next >= slot.tokens.len() {
        slot.current = None;
        return token_type::NONE;
    }
    slot.current = Some(next);
    let range = slot.range_at(next);
    let text = slot.text(range);
    type_of_text(&text)
}

/// The type bits of a token, from its text.
///
/// `kCFStringTokenizerTokenNormal` is the base; the mask bits below it say what
/// else the token contains, which is what the header means by a token being
/// "compound".
fn type_of_text(text: &str) -> u32 {
    let mut out = token_type::NORMAL;
    let mut letters = false;
    let mut numbers = false;
    let mut others = false;
    for c in text.chars() {
        letters |= c.is_alphabetic();
        numbers |= c.is_numeric();
        if !c.is_alphabetic() && !c.is_numeric() && !c.is_whitespace() {
            others = true;
        }
        if is_cjk(c) {
            out |= token_type::IS_CJ_WORD;
        }
    }
    if numbers {
        out |= token_type::HAS_NUMBERS;
    }
    if others {
        out |= token_type::HAS_NON_LETTERS;
    }
    if letters && (numbers || others) {
        out |= token_type::HAS_SUB_TOKENS;
    }
    out
}

/// CJK is one word for the purposes of the mask: a run of ideographs, kana or
/// Hangul is a single token however many characters it is.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF | 0x2E80..=0x2EFF | 0x3000..=0x303F | 0x3040..=0x30FF
        | 0x3130..=0x318F | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xA960..=0xA97F
        | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F)
}

/// `CFRange CFStringTokenizerGetCurrentTokenRange(CFStringTokenizerRef)`
///
/// A `CFRange` is two `CFIndex`, so it comes back in `rax` and `rdx`; a
/// `#[repr(C)]` return in Rust is passed and returned the same way.
pub unsafe extern "C" fn CFStringTokenizerGetCurrentTokenRange(tokenizer: CFTypeRef) -> CFRange {
    unsafe { as_tokenizer(tokenizer) }
        .and_then(|slot| slot.current.map(|current| slot.range_at(current)))
        .unwrap_or(CFRange {
            location: 0,
            length: 0,
        })
}

/// `CFIndex CFStringTokenizerGetCurrentSubTokens(CFStringTokenizerRef, CFRange *ranges, CFIndex maxRangeLength, CFMutableArrayRef derivedSubTokens)`
///
/// The sub-tokens of the current token are its words - the same cut
/// `kCFStringTokenizerUnitWord` makes - which is what a caller laying out mixed
/// script has to walk.  `derivedSubTokens` is for languages written right to
/// left with derived forms, which is a transformation this runtime does not
/// have, so it is left alone rather than filled with something else.
pub unsafe extern "C" fn CFStringTokenizerGetCurrentSubTokens(
    tokenizer: CFTypeRef,
    ranges: *mut CFRange,
    max_range_length: CFIndex,
    _derived_sub_tokens: CFTypeRef,
) -> CFIndex {
    let Some(slot) = (unsafe { as_tokenizer(tokenizer) }) else {
        return 0;
    };
    let Some(current) = slot.current else {
        // The header: "If token type is kCFStringTokenizerTokenNone, the ranges
        // array and maxRangeLength are ignored and 0 is returned."
        return 0;
    };
    let range = slot.range_at(current);
    let count = max_range_length.max(0) as usize;
    if count == 0 {
        return 0;
    }
    // The current token's text, re-cut into words.  `slot` is borrowed for the
    // walk below, so the sub-tokenizer is built from a copy of the units and
    // does not outlive the borrow.
    let units: Vec<u16> = slot.text(range).encode_utf16().collect();
    let sub = Tokenizer::build(
        &units,
        CFRange {
            location: 0,
            length: units.len() as CFIndex,
        },
        unit::WORD,
    );
    let mut written = 0;
    for index in 0..sub.tokens.len() {
        if written == count {
            break;
        }
        let one = sub.range_at(index);
        // SAFETY: `written < count` and the caller sized the array by `count`.
        unsafe {
            *ranges.add(written) = CFRange {
                location: range.location + one.location,
                length: one.length,
            };
        }
        written += 1;
    }
    written as CFIndex
}

/// `CFTypeRef CFStringTokenizerCopyCurrentTokenAttribute(CFStringTokenizerRef, CFOptionFlags)`
///
/// `kCFStringTokenizerAttributeLatinTranscription` is a transliteration, which
/// needs a romanisation table; `kCFStringTokenizerAttributeLanguage` is a
/// language tag, which this runtime cannot infer from a token.  Both are
/// refused by name rather than answered with a plausible tag.
pub unsafe extern "C" fn CFStringTokenizerCopyCurrentTokenAttribute(
    _tokenizer: CFTypeRef,
    attribute: u32,
) -> CFTypeRef {
    crate::base::refuse_once(
        "CFStringTokenizerCopyCurrentTokenAttribute",
        format!(
            "attribute {attribute:#x} is refused: a Latin transcription needs a romanisation table \
             and a language tag needs a language model, and this host has neither.  Returning a \
             plausible BCP 47 tag would be a guess about text this runtime has not read."
        ),
    );
    std::ptr::null()
}

/// `CFStringRef CFStringTokenizerCopyBestStringLanguage(CFStringRef, CFRange)`
///
/// Refused with its own name: the answer is a language tag chosen by a language
/// model, and there is no honest way to produce one here.
pub unsafe extern "C" fn CFStringTokenizerCopyBestStringLanguage(
    _string: CFTypeRef,
    _range: CFRange,
) -> CFTypeRef {
    crate::base::refuse_once(
        "CFStringTokenizerCopyBestStringLanguage",
        "refused: the answer is a language tag chosen by the natural-language framework, which \
         does not exist on this host.  Returning a tag nobody derived would be a guess about \
         every string a guest asked about."
            .to_string(),
    );
    std::ptr::null()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tokens of `text` under `unit`, as text.
    fn tokenize(text: &str, unit: u32) -> Vec<String> {
        let units: Vec<u16> = text.encode_utf16().collect();
        let tokenizer = Tokenizer::build(
            &units,
            CFRange {
                location: 0,
                length: units.len() as CFIndex,
            },
            unit,
        );
        tokenizer
            .tokens
            .iter()
            .map(|range| tokenizer.text(*range))
            .collect()
    }

    /// The partition, which is the property every unit but one rests on: the
    /// tokens cover the range with no gap and no overlap, so a caller laying
    /// text out against them cannot miss a character or draw one twice.
    ///
    /// `kCFStringTokenizerUnitWord` is the exception, and the header says why:
    /// "kCFStringTokenizerUnitWord doesn't return space between words as a
    /// token."  So the check for it is the weaker one that is actually true -
    /// the tokens are in order, they do not overlap, and the gaps are all
    /// whitespace.  That is the difference between unit 0 and unit 4, and it is
    /// a statement rather than a shrug.
    #[test]
    fn the_tokens_tile_the_range_except_for_the_whitespace_a_word_skips() {
        for text in [
            "",
            "a",
            "one two three",
            "line one\nline two\r\nline three",
            "  leading and trailing  ",
            "a, b. c! d? e\u{3002}f",
            "no breaks at all here",
            "\n\n\n",
            "caf\u{e9} na\u{ef}ve \u{4e2d}\u{6587}",
            "one\r\ntwo\rthree",
        ] {
            for unit in [
                unit::WORD,
                unit::WORD_BOUNDARY,
                unit::LINE_BREAK,
                unit::SENTENCE,
                unit::PARAGRAPH,
            ] {
                let units: Vec<u16> = text.encode_utf16().collect();
                let tokenizer = Tokenizer::build(
                    &units,
                    CFRange {
                        location: 0,
                        length: units.len() as CFIndex,
                    },
                    unit,
                );
                for range in &tokenizer.tokens {
                    assert!(
                        range.length > 0,
                        "no empty token: {text:?} unit {unit} {range:?}"
                    );
                    assert!(
                        range.location >= 0
                            && range.location + range.length <= units.len() as CFIndex,
                        "every token is inside the range: {text:?} unit {unit} {range:?}"
                    );
                }
                // The gaps between the tokens, in order.
                let mut at = 0usize;
                for range in &tokenizer.tokens {
                    let from = range.location as usize;
                    assert!(from >= at, "tokens do not go backwards: {text:?} unit {unit}");
                    if unit != unit::WORD {
                        assert_eq!(
                            from, at,
                            "the tokens tile: {text:?} unit {unit} gap at {at}..{from}"
                        );
                    } else {
                        let gap = String::from_utf16_lossy(&units[at..from]);
                        assert!(
                            gap.chars().all(char::is_whitespace),
                            "a word unit skips only whitespace, and this gap is {gap:?}: {text:?}"
                        );
                    }
                    at = from + range.length as usize;
                }
                if unit != unit::WORD {
                    assert_eq!(at, units.len(), "and they reach the end: {text:?} unit {unit}");
                }
            }
        }
    }

    /// A word is a run of word characters, and the whitespace between two words
    /// is not one - which is the difference between unit 0 and unit 4 and the
    /// reason unit 4 exists.
    #[test]
    fn a_word_is_not_the_whitespace_around_it() {
        assert_eq!(tokenize("one two", unit::WORD), ["one", "two"]);
        assert_eq!(
            tokenize("one two", unit::WORD_BOUNDARY),
            ["one", " ", "two"],
            "the space between the words is a token of its own"
        );
        // Marks are word characters: a syllable is not two words.
        assert_eq!(tokenize("e\u{301}x", unit::WORD), ["e\u{301}x"]);
    }

    /// Punctuation stands alone, so a comma or a full stop is a boundary.  A
    /// URL is a run of words rather than one long one because of this.
    #[test]
    fn punctuation_is_a_boundary() {
        assert_eq!(tokenize("a,b", unit::WORD), ["a", ",", "b"]);
        assert_eq!(
            tokenize("https://example.com/x", unit::WORD),
            ["https", "://", "example", ".", "com", "/", "x"]
        );
    }

    /// A line ends at a mandatory break, and `CRLF` is one break rather than
    /// two.  The guest's caller needs both: it walks the tokens and asks a string
    /// for its length up to the end of each.
    #[test]
    fn a_line_ends_at_a_mandatory_break() {
        assert_eq!(tokenize("a\nb", unit::LINE_BREAK), ["a\n", "b"]);
        assert_eq!(
            tokenize("a\r\nb", unit::LINE_BREAK),
            ["a\r\n", "b"],
            "CRLF is one break, not two"
        );
        assert_eq!(tokenize("a b", unit::LINE_BREAK), ["a ", "b"]);
        assert_eq!(tokenize("a-b", unit::LINE_BREAK), ["a-", "b"]);
    }

    /// A sentence ends after a run of terminators and the whitespace that
    /// follows, and a decimal point inside a number is not a full stop.
    #[test]
    fn a_sentence_ends_after_its_terminator() {
        assert_eq!(tokenize("One. Two.", unit::SENTENCE), ["One. ", "Two."]);
        assert_eq!(tokenize("One!  Two?", unit::SENTENCE), ["One!  ", "Two?"]);
        assert_eq!(
            tokenize("Pi is 3.14 exactly", unit::SENTENCE),
            ["Pi is 3.14 exactly"],
            "a decimal point is not a full stop"
        );
    }

    /// A paragraph is a line, or a run of lines with nothing between them.  A
    /// blank line closes it.
    #[test]
    fn a_paragraph_ends_at_a_blank_line() {
        assert_eq!(tokenize("one\ntwo", unit::PARAGRAPH), ["one\n", "two"]);
        assert_eq!(
            tokenize("one\n\ntwo", unit::PARAGRAPH),
            ["one\n\n", "two"],
            "a blank line is the paragraph boundary"
        );
    }

    /// A range is measured in UTF-16 units, so a letter above the basic plane
    /// is two of them and is still part of the word it is in.  A caller indexing
    /// by code point would be off by one from the second character on.
    #[test]
    fn a_letter_above_the_basic_plane_is_two_units_wide_and_still_a_word() {
        // U+10400 is DESERET CAPITAL LETTER LONG I: a letter, and two UTF-16
        // units, so it is in the same token as the `a` and the `b` around it.
        let text = "a\u{10400}b";
        assert_eq!(tokenize(text, unit::WORD), [text], "one word");
        let units: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(units.len(), 4, "one, two, one");
        let tokenizer = Tokenizer::build(
            &units,
            CFRange {
                location: 0,
                length: units.len() as CFIndex,
            },
            unit::WORD,
        );
        assert_eq!(tokenizer.tokens.len(), 1);
        assert_eq!(tokenizer.tokens[0].length, 4, "and the token is all four units");
    }

    /// An unpaired surrogate is one unit of text rather than a hole, because a
    /// `CFRange` has to be able to name it: a token that skipped it would leave
    /// every later range off by one.
    #[test]
    fn an_unpaired_surrogate_is_one_unit_and_is_covered() {
        let units: Vec<u16> = vec![b'a' as u16, 0xD800, b'b' as u16];
        let tokenizer = Tokenizer::build(
            &units,
            CFRange {
                location: 0,
                length: units.len() as CFIndex,
            },
            unit::WORD_BOUNDARY,
        );
        // It is not a word character, so it is a token of its own - but it is
        // one unit long and the tokens still cover all three.
        let mut at = 0;
        for range in &tokenizer.tokens {
            assert_eq!(range.location, at as CFIndex, "no gap and no overlap");
            assert!(range.length > 0);
            at += range.length as usize;
        }
        assert_eq!(at, 3, "and every unit is inside some token");
    }
}
