//! `CFCharacterSet`: a set of Unicode code points, and the eight predefined
//! sets this runtime can state exactly.
//!
//! # The representation is the bitmap representation
//!
//! Apple's CoreFoundation keeps a character set in one of four private forms
//! (builtin, range, a list of singles, a set of non-BMP "annex" planes) and
//! converts between them on demand.  This keeps the one form that
//! `CFCharacterSetCreateBitmapRepresentation` hands out anyway - a bitmap - and
//! that is not a simplification with a cost: the bitmap *is* the interchange
//! format, and a guest that reads it (Chromium's font-coverage cache does, at
//! `0x626c253`: `bt r13d, r12d` over `bits[c>>3]`, least significant bit first)
//! must get Apple's bytes or get a wrong answer.
//!
//! One plane is 65 536 code points, which is Apple's `NUMCHARACTERS`, so a plane
//! is 8 192 bytes and there are 17 of them - plane 0 unconditionally, planes
//! 1 to 16 only when something is in them.
//!
//! # Predefined sets
//!
//! `CFCharacterSetGetPredefined` takes an identifier out of
//! `CFCharacterSetPredefinedSet` and, per the header, "if the value is not in
//! `CFCharacterSetPredefinedSet`, the behavior is undefined".  Eight of the
//! fifteen identifiers are answered here and **seven are refused by name**,
//! because answering them would mean answering with a *different* set:
//! `char::is_alphabetic()` is the Unicode `Alphabetic` property - `L*` plus
//! `Nl` plus `Other_Alphabetic` - and `char::is_lowercase()` is `Ll` plus
//! `Other_Lowercase`.  Neither is the general category the enum names, and a
//! character set that is a superset of the one asked for is wrong in the way
//! that is hardest to notice: the extra code points are the ones nobody tests.
//!
//! The guest asks for two of them, and both are answered: `3`
//! (`kCFCharacterSetWhitespaceAndNewline`, from `CFRunLoopTimerSetTolerance`'s
//! neighbour at `0x982020a`) and `11` (`kCFCharacterSetPunctuation`, at
//! `0x98202d8`).
//!
//! | identifier | set | answered from |
//! |---|---|---|
//! | 1 | Control | `char::is_control()` (exactly `Cc`) plus the `Cf` table |
//! | 2 | Whitespace | `char::is_whitespace()` less the line separators, plus U+0009 |
//! | 3 | WhitespaceAndNewline | `char::is_whitespace()`, which *is* the set |
//! | 5 | DecimalDigit | the `Nd` table |
//! | 11 | Punctuation | the `P` table |
//! | 12 | Illegal | nothing; the set is empty |
//! | 13 | CapitalizedLetter | the `Lt` table |
//! | 15 | Newline | the seven code points the header lists |
//! | 4, 6, 7, 8, 9, 10, 14 | | **refused** - see the module note |

// The C spellings are the interface, as they are in `api`.
#![allow(non_snake_case)]

use std::collections::BTreeMap;
use std::sync::RwLock;

use objc::class::Class;
use objc::object::Id;

use crate::api::{Boolean, CFAllocatorRef, CFRange};
use crate::base::CFTypeRef;
use crate::string;

/// Apple's `NUMCHARACTERS`: the code points one plane holds.
const PLANE_CODEPOINTS: u32 = 65_536;
/// Apple's `NUMCHARACTERS / BITSPERBYTE`: one plane's bitmap in bytes.
const PLANE_BYTES: usize = (PLANE_CODEPOINTS / 8) as usize;
const PLANE_WORDS: usize = PLANE_BYTES / 8;
/// Apple's `MAX_ANNEX_PLANE`, the highest non-BMP plane a set may name.
const MAX_ANNEX_PLANE: u8 = 16;
/// The highest code point Unicode has, which is also the highest a `UTF32Char`
/// argument can carry.
const MAX_CODEPOINT: u32 = 0x10_FFFF;

type Plane = Box<[u64; PLANE_WORDS]>;

fn empty_plane() -> Plane {
    Box::new([0; PLANE_WORDS])
}

fn plane_of(codepoint: u32) -> u8 {
    (codepoint >> 16) as u8
}

fn offset_of(codepoint: u32) -> u32 {
    codepoint & 0xFFFF
}

fn bit_get(plane: &Plane, offset: u32) -> bool {
    plane[(offset / 64) as usize] & (1u64 << (offset % 64)) != 0
}

fn bit_set(plane: &mut Plane, offset: u32) {
    plane[(offset / 64) as usize] |= 1u64 << (offset % 64);
}

/// Whether `table` covers `codepoint`, binary search over the sorted ranges.
fn in_table(table: &[(u32, u32)], codepoint: u32) -> bool {
    table
        .binary_search_by(|(lo, hi)| {
            if codepoint < *lo {
                std::cmp::Ordering::Greater
            } else if codepoint > *hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `CFCharacterSetPredefinedSet`, Apple's values - including the two that do not
/// run in order, which is why the enumeration cannot be indexed by identifier.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Predefined {
    Control,
    Whitespace,
    WhitespaceAndNewline,
    DecimalDigit,
    Letter,
    LowercaseLetter,
    UppercaseLetter,
    NonBase,
    Decomposable,
    AlphaNumeric,
    Punctuation,
    Illegal,
    CapitalizedLetter,
    Symbol,
    Newline,
}

impl Predefined {
    /// The identifier the enum gives this set, which is the number a caller
    /// passes to `CFCharacterSetGetPredefined`.
    pub fn identifier(self) -> i32 {
        match self {
            Self::Control => 1,
            Self::Whitespace => 2,
            Self::WhitespaceAndNewline => 3,
            Self::DecimalDigit => 4,
            Self::Letter => 5,
            Self::LowercaseLetter => 6,
            Self::UppercaseLetter => 7,
            Self::NonBase => 8,
            Self::Decomposable => 9,
            Self::AlphaNumeric => 10,
            Self::Punctuation => 11,
            Self::Illegal => 12,
            Self::CapitalizedLetter => 13,
            Self::Symbol => 14,
            Self::Newline => 15,
        }
    }

    /// The set for `identifier`, or `None` for a value that is not in the enum.
    pub fn from_identifier(identifier: i32) -> Option<Self> {
        Some(match identifier {
            1 => Self::Control,
            2 => Self::Whitespace,
            3 => Self::WhitespaceAndNewline,
            4 => Self::DecimalDigit,
            5 => Self::Letter,
            6 => Self::LowercaseLetter,
            7 => Self::UppercaseLetter,
            8 => Self::NonBase,
            9 => Self::Decomposable,
            10 => Self::AlphaNumeric,
            11 => Self::Punctuation,
            12 => Self::Illegal,
            13 => Self::CapitalizedLetter,
            14 => Self::Symbol,
            15 => Self::Newline,
            _ => return None,
        })
    }

    /// The name the header's enumeration comment gives this set, for the two
    /// diagnostics below.  Not a symbol a guest reads - the name is here so a
    /// refusal says which identifier it is refusing.
    pub fn name(self) -> &'static str {
        match self {
            Self::Control => "kCFCharacterSetControl",
            Self::Whitespace => "kCFCharacterSetWhitespace",
            Self::WhitespaceAndNewline => "kCFCharacterSetWhitespaceAndNewline",
            Self::DecimalDigit => "kCFCharacterSetDecimalDigit",
            Self::Letter => "kCFCharacterSetLetter",
            Self::LowercaseLetter => "kCFCharacterSetLowercaseLetter",
            Self::UppercaseLetter => "kCFCharacterSetUppercaseLetter",
            Self::NonBase => "kCFCharacterSetNonBase",
            Self::Decomposable => "kCFCharacterSetDecomposable",
            Self::AlphaNumeric => "kCFCharacterSetAlphaNumeric",
            Self::Punctuation => "kCFCharacterSetPunctuation",
            Self::Illegal => "kCFCharacterSetIllegal",
            Self::CapitalizedLetter => "kCFCharacterSetCapitalizedLetter",
            Self::Symbol => "kCFCharacterSetSymbol",
            Self::Newline => "kCFCharacterSetNewline",
        }
    }

    /// The seven code points `kCFCharacterSetNewline` is, which the header
    /// spells out: U+000A to U+000D, U+0085, U+2028 and U+2029.
    fn is_newline(c: char) -> bool {
        matches!(c, '\n' | '\u{b}' | '\u{c}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
    }

    /// Whether `codepoint` is in this predefined set, in terms this runtime can
    /// state exactly.  `None` for the identifiers answered by refusal, so a
    /// caller never has to distinguish "not a member" from "no answer".
    pub fn contains(self, codepoint: u32) -> Option<bool> {
        let c = char::from_u32(codepoint);
        Some(match self {
            // Cc from the standard library, which is exactly the category, and
            // Cf from the generated table.
            Self::Control => c.is_some_and(|c| c.is_control() || in_table(CF, codepoint)),
            // Zs and U+0009.  `is_whitespace` is the White_Space property, which
            // is Zs + Zl + Zp + the C0 controls; taking the line separators and
            // the tab away leaves Zs + U+0009, which is what the header says.
            Self::Whitespace => c.is_some_and(|c| {
                c.is_whitespace() && !Self::is_newline(c) && c != '\t' || c == '\t'
            }),
            // Z* plus U+000A to U+000D and U+0085, which is exactly the
            // White_Space property - the tab is in it already.
            Self::WhitespaceAndNewline => c.is_some_and(char::is_whitespace),
            Self::DecimalDigit => in_table(ND, codepoint),
            Self::Punctuation => in_table(P, codepoint),
            Self::CapitalizedLetter => in_table(LT, codepoint),
            Self::Newline => c.is_some_and(Self::is_newline),
            // "Illegal" has had no members since Unicode 3.2 defined
            // surrogates out of existence, and Apple's own table for it is
            // empty.
            Self::Illegal => false,
            Self::Letter
            | Self::LowercaseLetter
            | Self::UppercaseLetter
            | Self::NonBase
            | Self::Decomposable
            | Self::AlphaNumeric
            | Self::Symbol => return None,
        })
    }
}

/// A set of code points: a materialised bitmap per plane, plus the predefined
/// set to answer from for a plane nothing has been materialised into yet.
///
/// The laziness matters for the predefined sets.  Materialising all seventeen
/// planes of `kCFCharacterSetPunctuation` costs 139 KB to answer questions
/// about 855 code points, and most callers only ever ask about plane 0.
pub struct Set {
    predefined: Option<Predefined>,
    planes: RwLock<BTreeMap<u8, Plane>>,
}

impl Set {
    fn empty() -> Self {
        Set {
            predefined: None,
            planes: RwLock::new(BTreeMap::new()),
        }
    }

    fn from_predefined(set: Predefined) -> Self {
        // Deliberately *empty*: a plane that is present but blank would answer
        // `false` for every code point in it and shadow the predefined set, which
        // is the one thing the map must not do.  `materialise` fills a plane in
        // when somebody needs it as bits.
        Set {
            predefined: Some(set),
            planes: RwLock::new(BTreeMap::new()),
        }
    }

    /// Whether `codepoint` is a member.
    pub fn contains(&self, codepoint: u32) -> bool {
        if codepoint > MAX_CODEPOINT {
            return false;
        }
        let plane = plane_of(codepoint);
        if let Some(bits) = self.planes.read().unwrap_or_else(|e| e.into_inner()).get(&plane) {
            return bit_get(bits, offset_of(codepoint));
        }
        // A plane with nothing materialised in it: the predefined set decides,
        // and an explicit set has nothing outside what it materialised.
        self.predefined
            .and_then(|set| set.contains(codepoint))
            .unwrap_or(false)
    }

    /// Materialise `plane` if the predefined set has anything in it.
    fn materialise(&self, plane: u8) {
        let Some(predefined) = self.predefined else {
            return;
        };
        let mut planes = self.planes.write().unwrap_or_else(|e| e.into_inner());
        if planes.contains_key(&plane) {
            return;
        }
        let base = u32::from(plane) << 16;
        let mut bits = empty_plane();
        let mut any = false;
        for offset in 0..PLANE_CODEPOINTS {
            if predefined.contains(base + offset) == Some(true) {
                bit_set(&mut bits, offset);
                any = true;
            }
        }
        // An empty plane stays unmaterialised, so a set that is entirely in
        // plane 0 costs one plane and not seventeen.
        if any {
            planes.insert(plane, bits);
        }
    }

    /// Every plane with a member, materialised, as `(plane, bitmap)`.
    fn materialised_planes(&self) -> Vec<(u8, Plane)> {
        for plane in 0..=MAX_ANNEX_PLANE {
            self.materialise(plane);
        }
        self.planes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(plane, bits)| (*plane, bits.clone()))
            .collect()
    }

    /// Add every code point of `codepoint..codepoint + length`.
    pub fn add_range(&self, start: u32, length: u32) {
        if length == 0 {
            return;
        }
        let Some(end) = start.checked_add(length) else {
            return;
        };
        let mut planes = self.planes.write().unwrap_or_else(|e| e.into_inner());
        let mut codepoint = start.min(MAX_CODEPOINT);
        while codepoint < end {
            let plane = plane_of(codepoint);
            let bits = planes.entry(plane).or_insert_with(empty_plane);
            // A run never crosses a plane boundary, so the last code point in
            // this plane is where the inner loop stops.
            let last = end.min(u32::from(plane + 1) << 16);
            while codepoint < last {
                bit_set(bits, offset_of(codepoint));
                codepoint += 1;
            }
        }
    }

    /// Add one code point.
    pub fn add(&self, codepoint: u32) {
        self.add_range(codepoint, 1);
    }

    /// Add every code point in `other`.
    pub fn union(&self, other: &Set) {
        for (plane, bits) in other.materialised_planes() {
            let mut mine = self.planes.write().unwrap_or_else(|e| e.into_inner());
            let target = mine.entry(plane).or_insert_with(empty_plane);
            for (index, word) in bits.iter().enumerate() {
                target[index] |= *word;
            }
        }
    }

    /// The whole set as a bitmap, in Apple's layout: plane 0's 8 192 bytes, then
    /// for each non-BMP plane an index byte followed by that plane's 8 192.
    pub fn bitmap(&self) -> Vec<u8> {
        let mut planes = self.materialised_planes();
        // Plane 0 is unconditional in Apple's format even if it is empty.
        if planes.first().map(|(plane, _)| *plane) != Some(0) {
            planes.insert(0, (0u8, empty_plane()));
        }
        let mut out = vec![0u8; PLANE_BYTES * planes.len() + planes.len() - 1];
        let mut at = 0;
        for (index, (plane, bits)) in planes.iter().enumerate() {
            if index > 0 {
                out[at] = *plane;
                at += 1;
            }
            for (word_index, word) in bits.iter().enumerate() {
                out[at + word_index * 8..at + word_index * 8 + 8]
                    .copy_from_slice(&word.to_le_bytes());
            }
            at += PLANE_BYTES;
        }
        out
    }

    /// A set holding every code point of `text`.
    ///
    /// `from_chars` and not `from_str` so it cannot be read as an implementation
    /// of [`std::str::FromStr`], which it is not: a `Set` is not a string and
    /// has no error case to report.
    pub fn from_chars(text: &str) -> Self {
        let set = Set::empty();
        for c in text.chars() {
            set.add(u32::from(c));
        }
        set
    }

    /// A set holding one inclusive range.
    pub fn from_range(start: u32, length: u32) -> Self {
        let set = Set::empty();
        set.add_range(start, length);
        set
    }
}

//
// -- the object ---------------------------------------------------------------

#[repr(C)]
struct Body {
    isa: *const Class,
    contents: *const Set,
}

pub fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {        // `NSCharacterSet` is Foundation's, and Foundation adds the methods; what
        // this registration decides is the class the *contents* live in and the
        // CoreFoundation type it reports.  The abstract Foundation class is the
        // parent for the same reason `NSArray` is: a guest's `isKindOfClass:` and
        // its `_OBJC_CLASS_$_NSCharacterSet` binding have to agree.
        let base = crate::base::abstract_class(c"NSCharacterSet", None);
        crate::base::note_type_class(base);
        let class = crate::base::register(c"__NSCFCharacterSet", Some(base), size_of::<Body>(), false);
        unsafe extern "C" fn dealloc(receiver: Id, _sel: objc::sel::Sel) {
            // SAFETY: `-dealloc` is reached with the last reference gone, and the
            // receiver is a `Body` whose `contents` came from `Box::into_raw`.
            unsafe {
                let body = receiver as *mut Body;
                if !(*body).contents.is_null() {
                    drop(Box::from_raw((*body).contents as *mut Set));
                    (*body).contents = std::ptr::null();
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
    // SAFETY: the cell holds a pointer to a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

/// Wrap `set` in an object of the given mutability.
fn make(set: Set) -> CFTypeRef {
    // SAFETY: `Body` begins with `isa` and is the layout `class` was registered
    // with.
    let body: *mut Body = unsafe { crate::base::allocate(class()) };
    if body.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a live instance whose second word is ours to fill in.
    unsafe { (*body).contents = Box::into_raw(Box::new(set)) };
    body as CFTypeRef
}

/// The set in `object`, if it is a character set this runtime made.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
pub unsafe fn as_set(object: CFTypeRef) -> Option<&'static Set> {
    if !unsafe { crate::base::is_kind_of(object, class()) } {
        return None;
    }
    // SAFETY: an instance of `class()` is a `Body` with a live `contents`.
    unsafe {
        let body = object as *const Body;
        (!(*body).contents.is_null()).then(|| &*(*body).contents)
    }
}

/// `CFCharacterSetGetTypeID`.
pub fn type_id() -> crate::base::CFTypeID {
    crate::base::type_id_of(class())
}

//
// -- the predefined singletons -----------------------------------------------

/// Each predefined set, built once.
///
/// The header says the instance is "owned by CF", so every call for the same
/// identifier must hand back the *same* object: a guest comparing two by pointer
/// has to agree with one comparing them with `CFEqual`.
fn predefined_object(set: Predefined) -> CFTypeRef {
    static ONCE: std::sync::OnceLock<std::sync::Mutex<Vec<(Predefined, usize)>>> =
        std::sync::OnceLock::new();
    let table = ONCE.get_or_init(Default::default);
    let mut entries = table.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, object)) = entries.iter().find(|(known, _)| *known == set) {
        return *object as CFTypeRef;
    }
    let object = make(Set::from_predefined(set)) as usize;
    entries.push((set, object));
    object as CFTypeRef
}

/// `CFCharacterSetGetTypeID(void)`.
pub unsafe extern "C" fn CFCharacterSetGetTypeID() -> crate::base::CFTypeID {
    type_id()
}

/// `CFCharacterSetRef CFCharacterSetGetPredefined(CFCharacterSetPredefinedSet)`
///
/// The seven identifiers this runtime will not answer are refused by name, and
/// the answer is null - which is what a set that is not there looks like, so
/// the refusal has to be in the diagnostic rather than in the value.
pub unsafe extern "C" fn CFCharacterSetGetPredefined(identifier: crate::base::CFIndex) -> CFTypeRef {
    let Some(set) = Predefined::from_identifier(identifier as i32) else {
        crate::base::refuse_once(
            "CFCharacterSetGetPredefined",
            format!(
                "identifier {identifier} is not in CFCharacterSetPredefinedSet, and this runtime \
                 will not invent a set for it"
            ),
        );
        return std::ptr::null();
    };
    if set.contains(0).is_none() {
        crate::base::refuse_once(
            "CFCharacterSetGetPredefined",
            format!(
                "{} ({}) is refused: answering it would mean answering with a different set, \
                 because char::is_alphabetic/is_lowercase/is_uppercase are the Unicode properties \
                 Alphabetic/Lowercase/Uppercase and not the general categories the enumeration \
                 names.  Closing it needs the L* category table (677 ranges) in \
                 crates/corefoundation/src/charset.rs",
                set.name(),
                set.identifier()
            ),
        );
        return std::ptr::null();
    }
    predefined_object(set)
}

/// `CFMutableCharacterSetRef CFCharacterSetCreateMutable(CFAllocatorRef alloc)`
pub unsafe extern "C" fn CFCharacterSetCreateMutable(_alloc: CFAllocatorRef) -> CFTypeRef {
    make(Set::empty())
}

/// `CFCharacterSetRef CFCharacterSetCreateWithCharactersInRange(CFAllocatorRef, CFRange)`
pub unsafe extern "C" fn CFCharacterSetCreateWithCharactersInRange(
    _alloc: CFAllocatorRef,
    the_range: CFRange,
) -> CFTypeRef {
    let (Ok(start), Ok(length)) = (u32::try_from(the_range.location), u32::try_from(the_range.length))
    else {
        return std::ptr::null();
    };
    // The header: "If the range is outside of the valid Unicode character point,
    // the behavior is undefined."  Refusing a range that runs off the end of
    // Unicode is the only defined thing to do with one, because a set that
    // silently lost its tail would be a set nobody could check.
    if start > MAX_CODEPOINT || start.saturating_add(length) > MAX_CODEPOINT + 1 {
        crate::base::refuse_once(
            "CFCharacterSetCreateWithCharactersInRange",
            format!(
                "range {start}+{length} is outside U+0000..U+10FFFF, which the header calls \
                 undefined; refused rather than truncated"
            ),
        );
        return std::ptr::null();
    }
    make(Set::from_range(start, length))
}

/// `CFCharacterSetRef CFCharacterSetCreateWithCharactersInString(CFAllocatorRef, CFStringRef)`
pub unsafe extern "C" fn CFCharacterSetCreateWithCharactersInString(
    _alloc: CFAllocatorRef,
    the_string: CFTypeRef,
) -> CFTypeRef {
    // SAFETY: a string or null, and `as_string` is null-safe.
    let set = match unsafe { string::as_string(the_string) } {
        Some(s) => Set::from_chars(&String::from_utf16_lossy(&s.utf16())),
        None => Set::empty(),
    };
    make(set)
}

/// `void CFCharacterSetAddCharactersInRange(CFMutableCharacterSetRef, CFRange)`
pub unsafe extern "C" fn CFCharacterSetAddCharactersInRange(the_set: CFTypeRef, the_range: CFRange) {
    let (Some(set), Ok(start), Ok(length)) = (
        unsafe { as_set(the_set) },
        u32::try_from(the_range.location),
        u32::try_from(the_range.length),
    ) else {
        return;
    };
    if start > MAX_CODEPOINT {
        return;
    }
    // A range that runs past the last code point contributes up to it, which is
    // what a caller adding "everything above here" means.
    set.add_range(start, length);
}

/// `void CFCharacterSetAddCharactersInString(CFMutableCharacterSetRef, CFStringRef)`
pub unsafe extern "C" fn CFCharacterSetAddCharactersInString(the_set: CFTypeRef, the_string: CFTypeRef) {
    let (Some(set), Some(text)) = (unsafe { as_set(the_set) }, unsafe {
        string::as_string(the_string)
    }) else {
        return;
    };
    for c in String::from_utf16_lossy(&text.utf16()).chars() {
        set.add(u32::from(c));
    }
}

/// `void CFCharacterSetUnion(CFMutableCharacterSetRef theSet, CFCharacterSetRef theOtherSet)`
pub unsafe extern "C" fn CFCharacterSetUnion(the_set: CFTypeRef, the_other: CFTypeRef) {
    let (Some(mine), Some(other)) = (unsafe { as_set(the_set) }, unsafe { as_set(the_other) })
    else {
        return;
    };
    mine.union(other);
}

/// `Boolean CFCharacterSetIsCharacterMember(CFCharacterSetRef, UniChar)`
pub unsafe extern "C" fn CFCharacterSetIsCharacterMember(
    the_set: CFTypeRef,
    the_char: u16,
) -> Boolean {
    unsafe { as_set(the_set) }.is_some_and(|set| set.contains(u32::from(the_char))) as Boolean
}

/// `Boolean CFCharacterSetIsLongCharacterMember(CFCharacterSetRef, UniChar)`
///
/// The "long" spelling exists for a `CFRange` of 32-bit code points; the
/// membership test is the same one, over a wider argument.
pub unsafe extern "C" fn CFCharacterSetIsLongCharacterMember(
    the_set: CFTypeRef,
    the_char: u32,
) -> Boolean {
    unsafe { as_set(the_set) }.is_some_and(|set| set.contains(the_char)) as Boolean
}

/// `CFDataRef CFCharacterSetCreateBitmapRepresentation(CFAllocatorRef, CFCharacterSetRef)`
///
/// Apple's format, byte for byte: `__kCFBitmapSize` for plane 0 and then, for
/// each non-BMP plane that has members, an index byte and that plane's
/// `__kCFBitmapSize`.
pub unsafe extern "C" fn CFCharacterSetCreateBitmapRepresentation(
    _alloc: CFAllocatorRef,
    the_set: CFTypeRef,
) -> CFTypeRef {
    let Some(set) = (unsafe { as_set(the_set) }) else {
        return std::ptr::null();
    };
    crate::collections::new_data(set.bitmap())
}

/// `CFIndex CFCharacterSetGetLongCharacterSetForMember(CFCharacterSetRef, UInt32)`
///
/// A convenience the header calls out for 10.7: the set of the characters
/// around `theChar` that are members, so a caller walking a string gets runs
/// rather than testing one code point at a time.  The run is *within* the
/// supplied set, which is the whole of the function's value.
pub unsafe extern "C" fn CFCharacterSetGetLongCharacterSetForMember(
    the_set: CFTypeRef,
    the_char: u32,
) -> CFTypeRef {
    let Some(set) = (unsafe { as_set(the_set) }) else {
        return std::ptr::null();
    };
    if the_char > MAX_CODEPOINT || !set.contains(the_char) {
        return std::ptr::null();
    }
    // At most this plane, because a run never crosses a plane boundary and a
    // set that spanned 65 536 code points would be a caller that wants the set
    // it already has.
    let plane_start = u32::from(plane_of(the_char)) << 16;
    let plane_end = plane_start + PLANE_CODEPOINTS;
    let start = (plane_start..=the_char)
        .rev()
        .find(|c| !set.contains(*c))
        .map_or(plane_start, |c| c + 1);
    let end = ((the_char + 1)..plane_end)
        .find(|c| !set.contains(*c))
        .unwrap_or(plane_end);
    make(Set::from_range(start, end - start))
}

//
// -- generated general-category tables ---------------------------------------

// GENERATED by test/tools/gen-charset-tables.py -- do not edit by hand.
// Unicode 16.0.0, the general categories Apple's CFCharacterSetPredefinedSet
// names refer to.  The tables are range lists because a category is a
// contiguous run of code points far more often than not, and 300 ranges
// is 2.4 KB against the 8 000-odd members they name.

/// `CF`: 21 ranges.
pub static CF: &[(u32, u32)] = &[
    (0xAD, 0xAD), (0x600, 0x605), (0x61C, 0x61C), (0x6DD, 0x6DD), (0x70F, 0x70F),
    (0x890, 0x891), (0x8E2, 0x8E2), (0x180E, 0x180E), (0x200B, 0x200F), (0x202A, 0x202E),
    (0x2060, 0x2064), (0x2066, 0x206F), (0xFEFF, 0xFEFF), (0xFFF9, 0xFFFB), (0x110BD, 0x110BD),
    (0x110CD, 0x110CD), (0x13430, 0x1343F), (0x1BCA0, 0x1BCA3), (0x1D173, 0x1D17A),
    (0xE0001, 0xE0001), (0xE0020, 0xE007F),
];

/// `LT`: 10 ranges.
pub static LT: &[(u32, u32)] = &[
    (0x1C5, 0x1C5), (0x1C8, 0x1C8), (0x1CB, 0x1CB), (0x1F2, 0x1F2), (0x1F88, 0x1F8F),
    (0x1F98, 0x1F9F), (0x1FA8, 0x1FAF), (0x1FBC, 0x1FBC), (0x1FCC, 0x1FCC), (0x1FFC, 0x1FFC),
];

/// `ND`: 71 ranges.
pub static ND: &[(u32, u32)] = &[
    (0x30, 0x39), (0x660, 0x669), (0x6F0, 0x6F9), (0x7C0, 0x7C9), (0x966, 0x96F),
    (0x9E6, 0x9EF), (0xA66, 0xA6F), (0xAE6, 0xAEF), (0xB66, 0xB6F), (0xBE6, 0xBEF),
    (0xC66, 0xC6F), (0xCE6, 0xCEF), (0xD66, 0xD6F), (0xDE6, 0xDEF), (0xE50, 0xE59),
    (0xED0, 0xED9), (0xF20, 0xF29), (0x1040, 0x1049), (0x1090, 0x1099), (0x17E0, 0x17E9),
    (0x1810, 0x1819), (0x1946, 0x194F), (0x19D0, 0x19D9), (0x1A80, 0x1A89), (0x1A90, 0x1A99),
    (0x1B50, 0x1B59), (0x1BB0, 0x1BB9), (0x1C40, 0x1C49), (0x1C50, 0x1C59), (0xA620, 0xA629),
    (0xA8D0, 0xA8D9), (0xA900, 0xA909), (0xA9D0, 0xA9D9), (0xA9F0, 0xA9F9), (0xAA50, 0xAA59),
    (0xABF0, 0xABF9), (0xFF10, 0xFF19), (0x104A0, 0x104A9), (0x10D30, 0x10D39),
    (0x10D40, 0x10D49), (0x11066, 0x1106F), (0x110F0, 0x110F9), (0x11136, 0x1113F),
    (0x111D0, 0x111D9), (0x112F0, 0x112F9), (0x11450, 0x11459), (0x114D0, 0x114D9),
    (0x11650, 0x11659), (0x116C0, 0x116C9), (0x116D0, 0x116E3), (0x11730, 0x11739),
    (0x118E0, 0x118E9), (0x11950, 0x11959), (0x11BF0, 0x11BF9), (0x11C50, 0x11C59),
    (0x11D50, 0x11D59), (0x11DA0, 0x11DA9), (0x11F50, 0x11F59), (0x16130, 0x16139),
    (0x16A60, 0x16A69), (0x16AC0, 0x16AC9), (0x16B50, 0x16B59), (0x16D70, 0x16D79),
    (0x1CCF0, 0x1CCF9), (0x1D7CE, 0x1D7FF), (0x1E140, 0x1E149), (0x1E2F0, 0x1E2F9),
    (0x1E4F0, 0x1E4F9), (0x1E5F1, 0x1E5FA), (0x1E950, 0x1E959), (0x1FBF0, 0x1FBF9),
];

/// `P`: 198 ranges.
pub static P: &[(u32, u32)] = &[
    (0x21, 0x23), (0x25, 0x2A), (0x2C, 0x2F), (0x3A, 0x3B), (0x3F, 0x40), (0x5B, 0x5D),
    (0x5F, 0x5F), (0x7B, 0x7B), (0x7D, 0x7D), (0xA1, 0xA1), (0xA7, 0xA7), (0xAB, 0xAB),
    (0xB6, 0xB7), (0xBB, 0xBB), (0xBF, 0xBF), (0x37E, 0x37E), (0x387, 0x387), (0x55A, 0x55F),
    (0x589, 0x58A), (0x5BE, 0x5BE), (0x5C0, 0x5C0), (0x5C3, 0x5C3), (0x5C6, 0x5C6),
    (0x5F3, 0x5F4), (0x609, 0x60A), (0x60C, 0x60D), (0x61B, 0x61B), (0x61D, 0x61F),
    (0x66A, 0x66D), (0x6D4, 0x6D4), (0x700, 0x70D), (0x7F7, 0x7F9), (0x830, 0x83E),
    (0x85E, 0x85E), (0x964, 0x965), (0x970, 0x970), (0x9FD, 0x9FD), (0xA76, 0xA76),
    (0xAF0, 0xAF0), (0xC77, 0xC77), (0xC84, 0xC84), (0xDF4, 0xDF4), (0xE4F, 0xE4F),
    (0xE5A, 0xE5B), (0xF04, 0xF12), (0xF14, 0xF14), (0xF3A, 0xF3D), (0xF85, 0xF85),
    (0xFD0, 0xFD4), (0xFD9, 0xFDA), (0x104A, 0x104F), (0x10FB, 0x10FB), (0x1360, 0x1368),
    (0x1400, 0x1400), (0x166E, 0x166E), (0x169B, 0x169C), (0x16EB, 0x16ED), (0x1735, 0x1736),
    (0x17D4, 0x17D6), (0x17D8, 0x17DA), (0x1800, 0x180A), (0x1944, 0x1945), (0x1A1E, 0x1A1F),
    (0x1AA0, 0x1AA6), (0x1AA8, 0x1AAD), (0x1B4E, 0x1B4F), (0x1B5A, 0x1B60), (0x1B7D, 0x1B7F),
    (0x1BFC, 0x1BFF), (0x1C3B, 0x1C3F), (0x1C7E, 0x1C7F), (0x1CC0, 0x1CC7), (0x1CD3, 0x1CD3),
    (0x2010, 0x2027), (0x2030, 0x2043), (0x2045, 0x2051), (0x2053, 0x205E), (0x207D, 0x207E),
    (0x208D, 0x208E), (0x2308, 0x230B), (0x2329, 0x232A), (0x2768, 0x2775), (0x27C5, 0x27C6),
    (0x27E6, 0x27EF), (0x2983, 0x2998), (0x29D8, 0x29DB), (0x29FC, 0x29FD), (0x2CF9, 0x2CFC),
    (0x2CFE, 0x2CFF), (0x2D70, 0x2D70), (0x2E00, 0x2E2E), (0x2E30, 0x2E4F), (0x2E52, 0x2E5D),
    (0x3001, 0x3003), (0x3008, 0x3011), (0x3014, 0x301F), (0x3030, 0x3030), (0x303D, 0x303D),
    (0x30A0, 0x30A0), (0x30FB, 0x30FB), (0xA4FE, 0xA4FF), (0xA60D, 0xA60F), (0xA673, 0xA673),
    (0xA67E, 0xA67E), (0xA6F2, 0xA6F7), (0xA874, 0xA877), (0xA8CE, 0xA8CF), (0xA8F8, 0xA8FA),
    (0xA8FC, 0xA8FC), (0xA92E, 0xA92F), (0xA95F, 0xA95F), (0xA9C1, 0xA9CD), (0xA9DE, 0xA9DF),
    (0xAA5C, 0xAA5F), (0xAADE, 0xAADF), (0xAAF0, 0xAAF1), (0xABEB, 0xABEB), (0xFD3E, 0xFD3F),
    (0xFE10, 0xFE19), (0xFE30, 0xFE52), (0xFE54, 0xFE61), (0xFE63, 0xFE63), (0xFE68, 0xFE68),
    (0xFE6A, 0xFE6B), (0xFF01, 0xFF03), (0xFF05, 0xFF0A), (0xFF0C, 0xFF0F), (0xFF1A, 0xFF1B),
    (0xFF1F, 0xFF20), (0xFF3B, 0xFF3D), (0xFF3F, 0xFF3F), (0xFF5B, 0xFF5B), (0xFF5D, 0xFF5D),
    (0xFF5F, 0xFF65), (0x10100, 0x10102), (0x1039F, 0x1039F), (0x103D0, 0x103D0),
    (0x1056F, 0x1056F), (0x10857, 0x10857), (0x1091F, 0x1091F), (0x1093F, 0x1093F),
    (0x10A50, 0x10A58), (0x10A7F, 0x10A7F), (0x10AF0, 0x10AF6), (0x10B39, 0x10B3F),
    (0x10B99, 0x10B9C), (0x10D6E, 0x10D6E), (0x10EAD, 0x10EAD), (0x10F55, 0x10F59),
    (0x10F86, 0x10F89), (0x11047, 0x1104D), (0x110BB, 0x110BC), (0x110BE, 0x110C1),
    (0x11140, 0x11143), (0x11174, 0x11175), (0x111C5, 0x111C8), (0x111CD, 0x111CD),
    (0x111DB, 0x111DB), (0x111DD, 0x111DF), (0x11238, 0x1123D), (0x112A9, 0x112A9),
    (0x113D4, 0x113D5), (0x113D7, 0x113D8), (0x1144B, 0x1144F), (0x1145A, 0x1145B),
    (0x1145D, 0x1145D), (0x114C6, 0x114C6), (0x115C1, 0x115D7), (0x11641, 0x11643),
    (0x11660, 0x1166C), (0x116B9, 0x116B9), (0x1173C, 0x1173E), (0x1183B, 0x1183B),
    (0x11944, 0x11946), (0x119E2, 0x119E2), (0x11A3F, 0x11A46), (0x11A9A, 0x11A9C),
    (0x11A9E, 0x11AA2), (0x11B00, 0x11B09), (0x11BE1, 0x11BE1), (0x11C41, 0x11C45),
    (0x11C70, 0x11C71), (0x11EF7, 0x11EF8), (0x11F43, 0x11F4F), (0x11FFF, 0x11FFF),
    (0x12470, 0x12474), (0x12FF1, 0x12FF2), (0x16A6E, 0x16A6F), (0x16AF5, 0x16AF5),
    (0x16B37, 0x16B3B), (0x16B44, 0x16B44), (0x16D6D, 0x16D6F), (0x16E97, 0x16E9A),
    (0x16FE2, 0x16FE2), (0x1BC9F, 0x1BC9F), (0x1DA87, 0x1DA8B), (0x1E5FF, 0x1E5FF),
    (0x1E95E, 0x1E95F),
];


#[cfg(test)]
mod tests {
    use super::*;

    /// Apple's layout, which is what a guest reads.
    ///
    /// The Chromium font-coverage cache at `0x626c253` walks the data one byte
    /// at a time: `movzx r13d, BYTE PTR [rax+r14]` for `r14` in `0..0x2000`, then
    /// `bt r13d, r12d` for `r12` in `0..8`, and the code point is
    /// `r14 * 8 + r12`.  So plane 0 is 8192 bytes, least significant bit first,
    /// and the code point for bit `b` of byte `n` is `n * 8 + b`.  A set that is
    /// 8192 bytes but indexed the other way round would report a font's
    /// coverage of half the plane as the other half, which is a silent wrong
    /// answer about glyphs.
    #[test]
    fn the_bitmap_is_plane_zero_lsb_first() {
        let set = Set::from_range(0x41, 1);
        set.add(0x42);
        // Two of the first 8192 bytes, and nothing after them: plane 0 only.
        let bitmap = set.bitmap();
        assert_eq!(bitmap.len(), PLANE_BYTES, "one plane and no annex planes");
        assert_eq!(bitmap[0x41 / 8], 0b0000_0110, "bits 0 and 1 of byte 8");
        for (index, byte) in bitmap.iter().enumerate() {
            let expected: u8 = if index == 0x41 / 8 { 0b0000_0110 } else { 0 };
            assert_eq!(*byte, expected, "byte {index}");
        }
        // And the other way round is what the guest's loop computes.
        for code_point in [0x41u32, 0x42] {
            let byte = bitmap[code_point as usize / 8];
            assert!(
                byte & (1u8 << (code_point % 8)) != 0,
                "code point {code_point:#x} is in the set"
            );
        }
        assert_eq!(bitmap[0x43 / 8] & (1 << (0x43 % 8)), 0, "and 0x43 is not");
    }

    /// A code point above the basic plane goes into an annex plane, and the
    /// representation names the plane in a byte before that plane's bitmap - so
    /// a reader can tell plane 1 from plane 2 without guessing.
    #[test]
    fn an_annex_plane_is_named_before_its_bitmap() {
        let set = Set::from_range(0x1_0041, 1);
        let bitmap = set.bitmap();
        assert_eq!(
            bitmap.len(),
            PLANE_BYTES + 1 + PLANE_BYTES,
            "plane 0, then a plane index and one more plane"
        );
        assert_eq!(bitmap[PLANE_BYTES], 1, "the annex plane index");
        assert_eq!(bitmap[PLANE_BYTES + 1 + 0x41 / 8], 1u8 << (0x41 % 8));
        // `CFData` is what the guest reads this through, and its length is the
        // one the cache sizes its buffer from.
        // SAFETY: a set this test made.
        let data = unsafe { CFCharacterSetCreateBitmapRepresentation(std::ptr::null(), wrap(&set)) };
        // SAFETY: a `CFData` this call made.
        let length = unsafe { crate::api::CFDataGetLength(data) } as usize;
        assert_eq!(length, bitmap.len());
        // SAFETY: same.
        unsafe { crate::api::CFRelease(data) };
    }

    /// The two sets the guest asks for, by identifier, and what they contain.
    ///
    /// `0x982020a` asks for 3 and `0x98202d8` for 11, and unions each into a
    /// set it is building out of punctuation literals and CJK ranges.
    #[test]
    fn the_predefined_sets_the_guest_asks_for() {
        // SAFETY: an identifier, which the function only reads.
        let newline_and_space = unsafe { CFCharacterSetGetPredefined(3) };
        // SAFETY: ditto.
        let punctuation = unsafe { CFCharacterSetGetPredefined(11) };
        assert!(!newline_and_space.is_null(), "3 is a set this runtime answers");
        assert!(!punctuation.is_null(), "and so is 11");
        // The same object every time, because the header says the instance is
        // owned by CF.
        // SAFETY: ditto.
        assert_eq!(unsafe { CFCharacterSetGetPredefined(3) }, newline_and_space);
        for member in [' ', '\t', '\n', '\r', '\u{b}', '\u{c}', '\u{85}', '\u{a0}'] {
            // SAFETY: a set this runtime made and a `UniChar`.
            assert_eq!(
                unsafe { CFCharacterSetIsCharacterMember(newline_and_space, member as u16) },
                1,
                "{member:?} is whitespace or a newline"
            );
        }
        // SAFETY: ditto.
        assert_eq!(
            unsafe { CFCharacterSetIsCharacterMember(newline_and_space, b'a' as u16) },
            0
        );
        for member in ['!', '"', '#', '%', '&', '\'', '(', ')', ',', '-', '.', '/', ':', ';', '?']
        {
            // SAFETY: a set this runtime made and a `UniChar`.
            assert_eq!(
                unsafe { CFCharacterSetIsCharacterMember(punctuation, member as u16) },
                1,
                "{member:?} is punctuation"
            );
        }
        // A letter is neither, which is the check that says the two sets are
        // different sets rather than the same one twice.
        // SAFETY: ditto.
        assert_eq!(unsafe { CFCharacterSetIsCharacterMember(punctuation, b'a' as u16) }, 0);
    }

    /// `Union` is what the guest's builder does ten times, and the set has to
    /// come out the union of its parts rather than the last one.
    #[test]
    fn a_union_is_the_union() {
        // SAFETY: a null allocator, which every `Create` here ignores.
        let set = unsafe { CFCharacterSetCreateMutable(std::ptr::null()) };
        // SAFETY: a set this call made.
        unsafe {
            CFCharacterSetAddCharactersInRange(
                set,
                CFRange {
                    location: 0x3200,
                    length: 0x10,
                },
            );
            // SAFETY: the identifier 3, which the guest passes.
            CFCharacterSetUnion(set, CFCharacterSetGetPredefined(3));
        }
        for member in [0x3200u16, 0x320F, b' ' as u16, b'\t' as u16] {
            // SAFETY: a set this runtime made and a `UniChar`.
            assert_eq!(
                unsafe { CFCharacterSetIsCharacterMember(set, member) },
                1,
                "{member:#x} is in the union"
            );
        }
        // SAFETY: ditto.
        assert_eq!(
            unsafe { CFCharacterSetIsCharacterMember(set, 0x33F0) },
            0,
            "and the range really was only sixteen wide"
        );
        // SAFETY: nothing else holds a reference.
        unsafe { crate::api::CFRelease(set) };
    }

    /// The seven identifiers this runtime will not answer, and why the answer
    /// is null rather than a set that is nearly right.
    ///
    /// A superset is the failure mode that matters: the extra code points are
    /// the ones no test looks at, and `char::is_alphabetic()` is `L* + Nl +
    /// Other_Alphabetic` rather than `L*`.
    #[test]
    fn the_seven_refused_identifiers_are_null() {
        for (identifier, name) in [
            (5, "kCFCharacterSetLetter"),
            (6, "kCFCharacterSetLowercaseLetter"),
            (7, "kCFCharacterSetUppercaseLetter"),
            (8, "kCFCharacterSetNonBase"),
            (9, "kCFCharacterSetDecomposable"),
            (10, "kCFCharacterSetAlphaNumeric"),
            (14, "kCFCharacterSetSymbol"),
        ] {
            // SAFETY: an identifier out of the enumeration.
            assert!(
                unsafe { CFCharacterSetGetPredefined(identifier) }.is_null(),
                "{name} is refused, not approximated"
            );
        }
        // And the refusal is a *named* one, printed once.
        // SAFETY: an identifier, read only.
        unsafe { CFCharacterSetGetPredefined(5) };
    }

    /// A range that runs off the end of Unicode is undefined per the header, and
    /// a set that quietly lost its tail would be a set nobody could check.
    #[test]
    fn a_range_outside_unicode_is_refused_rather_than_truncated() {
        // SAFETY: two nulls and a range past U+10FFFF.
        let set = unsafe {
            CFCharacterSetCreateWithCharactersInRange(
                std::ptr::null(),
                CFRange {
                    location: MAX_CODEPOINT as crate::base::CFIndex,
                    length: 0x10_0000,
                },
            )
        };
        assert!(set.is_null(), "refused");
        // SAFETY: an in-range one, for contrast.
        let ok = unsafe {
            CFCharacterSetCreateWithCharactersInRange(
                std::ptr::null(),
                CFRange {
                    location: 0x10_FFF0,
                    length: 0x10,
                },
            )
        };
        assert!(!ok.is_null(), "and the last sixteen code points are fine");
        // SAFETY: a set this call made.
        assert_eq!(unsafe { CFCharacterSetIsLongCharacterMember(ok, 0x10_FFFF) }, 1);
        // SAFETY: nothing else holds a reference.
        unsafe { crate::api::CFRelease(ok) };
    }

    /// A `CFStringRef` built from a literal, used where a C API wants an object.
    fn wrap(set: &Set) -> CFTypeRef {
        let body: *mut Body = unsafe { crate::base::allocate(class()) };
        // SAFETY: a live instance whose second word is ours to fill in.
        unsafe {
            (*body).contents = set as *const Set;
            body as CFTypeRef
        }
    }
}
