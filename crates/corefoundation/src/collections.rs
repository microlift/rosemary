use std::sync::OnceLock;

use hashbrown::HashTable;

use objc::class::Class;

use crate::base::{CFIndex, CFTypeRef};
use crate::boxed::{contents, make};
use crate::callbacks::Policy;

pub type CFDictionaryRef = CFTypeRef;

/// Register a class whose instances hold a `T`; see [`crate::boxed`].
///
/// `superclass` is the abstract Foundation class the guest binds and asks
/// `isKindOfClass:` about; it is what carries the CoreFoundation type.
fn register<T: 'static>(
    name: &'static std::ffi::CStr,
    superclass: &'static Class,
) -> &'static Class {
    crate::base::note_type_class(superclass);
    crate::boxed::register::<T>(name, Some(superclass), false)
}

//

pub struct ArrayContents {
    pub values: Vec<CFTypeRef>,
    pub policy: Policy,
    /// Whether the object was created mutable.
    ///
    /// Not a class distinction, because `__NSCFArray` has to descend from
    /// `NSMutableArray` for a guest's `isKindOfClass:` to work either way. So it
    /// is a field, and a mutation of an immutable array is refused *by name*
    /// rather than quietly allowed - which is what Apple's raise amounts to.
    pub mutable: bool,
}

impl Drop for ArrayContents {
    fn drop(&mut self) {
        for value in self.values.drain(..) {
            self.policy.release(value);
        }
    }
}

pub fn array_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let p = *CLASS.get_or_init(|| {
        objc::class::ptr(register::<ArrayContents>(
            c"__NSCFArray",
            crate::base::mutable_pair(c"NSArray", c"NSMutableArray"),
        )) as usize
    });
    // SAFETY: a leaked `Class` this crate registered.
    unsafe { &*(p as *const Class) }
}

/// Build an array from `values`, retaining each as `policy` says.
pub fn new_array(values: &[CFTypeRef], policy: Policy, mutable: bool) -> CFTypeRef {
    let values: Vec<CFTypeRef> = values.iter().map(|v| policy.retain(*v)).collect();
    make(
        array_class(),
        ArrayContents {
            values,
            policy,
            mutable,
        },
    )
}

/// # Safety
///
/// As [`contents`].
pub unsafe fn array<'a>(object: CFTypeRef) -> Option<&'a mut ArrayContents> {
    unsafe { contents(object, array_class()) }
}

//

/// Where each member of a set or key of a dictionary sits in its storage,
/// found by hash so a lookup asks the equal callback - often an `-isEqual:`
/// send - only of members that hash alike.
///
/// Positions follow the storage's: removal swaps the last entry into the hole,
/// so enumeration order is insertion order until something is removed. Apple
/// promises no order at all.
#[derive(Default)]
struct HashIndex {
    table: HashTable<usize>,
    /// The mixed hash of the entry at each position, so the table can grow
    /// and an entry move without calling back into guest code.
    hashes: Vec<u64>,
}

impl HashIndex {
    /// Spread a `-hash`, which is often a small integer or an aligned pointer,
    /// over all 64 bits; the table takes its bucket from the low bits and its
    /// probe tag from the high ones. This is splitmix64's finalizer.
    fn mix(hash: usize) -> u64 {
        const MULTIPLIER_1: u64 = 0xbf58_476d_1ce4_e5b9;
        const MULTIPLIER_2: u64 = 0x94d0_49bb_1331_11eb;
        let mut h = hash as u64;
        h = (h ^ (h >> 30)).wrapping_mul(MULTIPLIER_1);
        h = (h ^ (h >> 27)).wrapping_mul(MULTIPLIER_2);
        h ^ (h >> 31)
    }

    /// The position whose entry `is_match` accepts, among those of `hash`.
    fn find(&self, hash: u64, mut is_match: impl FnMut(usize) -> bool) -> Option<usize> {
        self.table
            .find(hash, |&at| self.hashes[at] == hash && is_match(at))
            .copied()
    }

    /// Record an entry just pushed onto the end of the storage.
    fn push(&mut self, hash: u64) {
        let at = self.hashes.len();
        self.hashes.push(hash);
        let hashes = &self.hashes;
        self.table.insert_unique(hash, at, |&i| hashes[i]);
    }

    /// Forget the entry at `at`, the storage having done `swap_remove(at)`.
    fn swap_remove(&mut self, at: usize) {
        let last = self.hashes.len() - 1;
        match self.table.find_entry(self.hashes[at], |&i| i == at) {
            Ok(entry) => drop(entry.remove()),
            Err(_) => unreachable!("every position is indexed"),
        }
        if at != last {
            let moved = self.table.find_mut(self.hashes[last], |&i| i == last);
            *moved.expect("every position is indexed") = at;
        }
        self.hashes.swap_remove(at);
    }

    fn clear(&mut self) {
        self.table.clear();
        self.hashes.clear();
    }
}

pub struct SetContents {
    values: Vec<CFTypeRef>,
    index: HashIndex,
    pub policy: Policy,
}

impl Drop for SetContents {
    fn drop(&mut self) {
        for value in self.values.drain(..) {
            self.policy.release(value);
        }
    }
}

impl SetContents {
    pub fn new(policy: Policy) -> SetContents {
        SetContents {
            values: Vec::new(),
            index: HashIndex::default(),
            policy,
        }
    }

    pub fn values(&self) -> &[CFTypeRef] {
        &self.values
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    fn find(&self, value: CFTypeRef, hash: u64) -> Option<usize> {
        let policy = self.policy;
        self.index
            .find(hash, |at| policy.equal(self.values[at], value))
    }

    /// The member equal to `value`.
    pub fn member(&self, value: CFTypeRef) -> Option<CFTypeRef> {
        let hash = HashIndex::mix(self.policy.hash(value));
        self.find(value, hash).map(|at| self.values[at])
    }

    pub fn contains(&self, value: CFTypeRef) -> bool {
        self.member(value).is_some()
    }

    /// Add `value`, retained, unless an equal member is already present.
    /// Whether it was added.
    pub fn insert(&mut self, value: CFTypeRef) -> bool {
        let hash = HashIndex::mix(self.policy.hash(value));
        if self.find(value, hash).is_some() {
            return false;
        }
        let value = self.policy.retain(value);
        self.values.push(value);
        self.index.push(hash);
        true
    }

    /// Take out the member equal to `value`. The caller owns the reference
    /// the set held.
    pub fn remove(&mut self, value: CFTypeRef) -> Option<CFTypeRef> {
        let hash = HashIndex::mix(self.policy.hash(value));
        let at = self.find(value, hash)?;
        self.index.swap_remove(at);
        Some(self.values.swap_remove(at))
    }

    /// Take out every member. The caller owns the references the set held.
    pub fn take_all(&mut self) -> Vec<CFTypeRef> {
        self.index.clear();
        std::mem::take(&mut self.values)
    }
}

pub fn set_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let p = *CLASS.get_or_init(|| {
        objc::class::ptr(register::<SetContents>(
            c"__NSCFSet",
            crate::base::mutable_pair(c"NSSet", c"NSMutableSet"),
        )) as usize
    });
    // SAFETY: a leaked `Class` this crate registered.
    unsafe { &*(p as *const Class) }
}

/// A set of `values`, keeping the first of any that are equal.
pub fn new_set(values: &[CFTypeRef], policy: Policy) -> CFTypeRef {
    let mut contents = SetContents::new(policy);
    for value in values {
        contents.insert(*value);
    }
    make(set_class(), contents)
}

/// # Safety
///
/// As [`contents`].
pub unsafe fn set<'a>(object: CFTypeRef) -> Option<&'a mut SetContents> {
    unsafe { contents(object, set_class()) }
}

//

pub struct DictionaryContents {
    entries: Vec<(CFTypeRef, CFTypeRef)>,
    index: HashIndex,
    pub keys: Policy,
    pub values: Policy,
    /// As [`ArrayContents::mutable`].
    pub mutable: bool,
}

impl Drop for DictionaryContents {
    fn drop(&mut self) {
        for (key, value) in self.entries.drain(..) {
            self.keys.release(key);
            self.values.release(value);
        }
    }
}

impl DictionaryContents {
    pub fn new(keys: Policy, values: Policy, mutable: bool) -> DictionaryContents {
        DictionaryContents {
            entries: Vec::new(),
            index: HashIndex::default(),
            keys,
            values,
            mutable,
        }
    }

    pub fn entries(&self) -> &[(CFTypeRef, CFTypeRef)] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn find(&self, key: CFTypeRef, hash: u64) -> Option<usize> {
        let keys = self.keys;
        self.index
            .find(hash, |at| keys.equal(self.entries[at].0, key))
    }

    /// Where the key equal to `key` sits in [`entries`](Self::entries).
    pub fn position(&self, key: CFTypeRef) -> Option<usize> {
        self.find(key, HashIndex::mix(self.keys.hash(key)))
    }

    /// The value of the key equal to `key`.
    pub fn get(&self, key: CFTypeRef) -> Option<CFTypeRef> {
        self.position(key).map(|at| self.entries[at].1)
    }

    /// Insert or replace, retaining both as the policies say. A replaced value
    /// keeps the key object already present.
    pub fn set(&mut self, key: CFTypeRef, value: CFTypeRef) {
        let hash = HashIndex::mix(self.keys.hash(key));
        let value = self.values.retain(value);
        match self.find(key, hash) {
            Some(at) => {
                let old = std::mem::replace(&mut self.entries[at].1, value);
                self.values.release(old);
            }
            None => {
                let key = self.keys.retain(key);
                self.entries.push((key, value));
                self.index.push(hash);
            }
        }
    }

    /// Take out the pair whose key is equal to `key`. The caller owns the
    /// references the dictionary held.
    pub fn remove(&mut self, key: CFTypeRef) -> Option<(CFTypeRef, CFTypeRef)> {
        let at = self.position(key)?;
        self.index.swap_remove(at);
        Some(self.entries.swap_remove(at))
    }

    /// Remove and release the pair whose key is equal to `key`. The pair is out
    /// of the dictionary before a release can run guest code.
    pub fn discard(&mut self, key: CFTypeRef) {
        if let Some((key, value)) = self.remove(key) {
            self.keys.release(key);
            self.values.release(value);
        }
    }

    /// Take out every pair. The caller owns the references the dictionary held.
    pub fn take_all(&mut self) -> Vec<(CFTypeRef, CFTypeRef)> {
        self.index.clear();
        std::mem::take(&mut self.entries)
    }
}

pub fn dictionary_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let p = *CLASS.get_or_init(|| {
        objc::class::ptr(register::<DictionaryContents>(
            c"__NSCFDictionary",
            crate::base::mutable_pair(c"NSDictionary", c"NSMutableDictionary"),
        )) as usize
    });
    // SAFETY: a leaked `Class` this crate registered.
    unsafe { &*(p as *const Class) }
}

pub fn new_dictionary(
    pairs: &[(CFTypeRef, CFTypeRef)],
    keys: Policy,
    values: Policy,
    mutable: bool,
) -> CFTypeRef {
    let mut contents = DictionaryContents::new(keys, values, mutable);
    for (key, value) in pairs {
        contents.set(*key, *value);
    }
    make(dictionary_class(), contents)
}

/// # Safety
///
/// As [`contents`].
pub unsafe fn dictionary<'a>(object: CFTypeRef) -> Option<&'a mut DictionaryContents> {
    unsafe { contents(object, dictionary_class()) }
}

//

/// `CFNumberType`, which says how to read the sixteen bytes a number carries.
///
/// The values are Apple's and are part of the interface: a guest passes one in
/// and reads one back out.
pub mod number_type {
    use crate::base::CFIndex;

    pub const SINT8: CFIndex = 1;
    pub const SINT16: CFIndex = 2;
    pub const SINT32: CFIndex = 3;
    pub const SINT64: CFIndex = 4;
    pub const FLOAT32: CFIndex = 5;
    pub const FLOAT64: CFIndex = 6;
    pub const CHAR: CFIndex = 7;
    pub const SHORT: CFIndex = 8;
    pub const INT: CFIndex = 9;
    pub const LONG: CFIndex = 10;
    pub const LONG_LONG: CFIndex = 11;
    pub const FLOAT: CFIndex = 12;
    pub const DOUBLE: CFIndex = 13;
    pub const CFINDEX: CFIndex = 14;
    pub const NSINTEGER: CFIndex = 15;
    pub const CGFLOAT: CFIndex = 16;
}

/// A number, held as whichever of the two things it is.
///
/// Not one `f64`: a `SInt64` past 2^53 does not survive the trip, and a guest
/// that stores a HID element cookie or a vendor ID would get a neighbouring
/// value back with nothing to say so.
#[derive(Clone, Copy)]
pub enum Number {
    Integer(i64),
    Float(f64),
}

impl Number {
    pub fn is_float(&self) -> bool {
        matches!(self, Number::Float(_))
    }

    pub fn as_i64(&self) -> i64 {
        match self {
            Number::Integer(v) => *v,
            Number::Float(v) => *v as i64,
        }
    }

    pub fn as_f64(&self) -> f64 {
        match self {
            Number::Integer(v) => *v as f64,
            Number::Float(v) => *v,
        }
    }
}

/// A number and the `CFNumberType` it answers `CFNumberGetType` with.
///
/// The type is the canonical one for what it was created from - `kCFNumberIntType`
/// is `kCFNumberSInt32Type` - because that is all Apple's keeps, and a guest
/// switching on it sees the six storage types and never the C names.
struct NumberBody {
    value: Number,
    kind: CFIndex,
}

/// The canonical `CFNumberType` for a type a number can be created with.
pub fn canonical_number_type(kind: CFIndex) -> Option<CFIndex> {
    use number_type as t;
    Some(match number_shape(kind)? {
        (1, false) => t::SINT8,
        (2, false) => t::SINT16,
        (4, false) => t::SINT32,
        (4, true) => t::FLOAT32,
        (_, false) => t::SINT64,
        (_, true) => t::FLOAT64,
    })
}

/// How wide `type` is, and whether it is floating point.
pub fn number_shape(kind: CFIndex) -> Option<(usize, bool)> {
    use number_type as t;
    Some(match kind {
        t::SINT8 | t::CHAR => (1, false),
        t::SINT16 | t::SHORT => (2, false),
        t::SINT32 | t::INT => (4, false),
        t::SINT64 | t::LONG | t::LONG_LONG | t::CFINDEX | t::NSINTEGER => (8, false),
        t::FLOAT32 | t::FLOAT => (4, true),
        t::FLOAT64 | t::DOUBLE | t::CGFLOAT => (8, true),
        _ => return None,
    })
}

pub fn number_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let p = *CLASS.get_or_init(|| {
        objc::class::ptr(register::<NumberBody>(
            c"__NSCFNumber",
            crate::base::abstract_class(c"NSNumber", None),
        )) as usize
    });
    // SAFETY: a leaked `Class` this crate registered.
    unsafe { &*(p as *const Class) }
}

/// A number of the widest type of its kind: `SInt64` or `Float64`.
pub fn new_number(value: Number) -> CFTypeRef {
    let kind = match value {
        Number::Integer(_) => number_type::SINT64,
        Number::Float(_) => number_type::FLOAT64,
    };
    new_number_of_type(value, kind)
}

/// A number that reports `kind`, canonicalised; an unknown `kind` reports the
/// widest type of the value's kind.
pub fn new_number_of_type(value: Number, kind: CFIndex) -> CFTypeRef {
    let kind = canonical_number_type(kind).unwrap_or(match value {
        Number::Integer(_) => number_type::SINT64,
        Number::Float(_) => number_type::FLOAT64,
    });
    make(number_class(), NumberBody { value, kind })
}

/// `__NSCFBoolean`, which is a subclass of `NSNumber` here as it is on Darwin.
///
/// A separate class rather than a number holding zero or one, because
/// `kCFBooleanTrue` is an *object a guest compares against by identity* - and
/// because `CFGetTypeID` of one has to answer `CFBooleanGetTypeID()` rather than
/// the number type, which a guest switching on the type of a dictionary value
/// depends on.
pub fn boolean_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let p = *CLASS.get_or_init(|| {
        let class = register::<NumberBody>(
            c"__NSCFBoolean",
            crate::base::abstract_class(c"NSNumber", None),
        );
        crate::base::note_type_class(class);
        objc::class::ptr(class) as usize
    });
    // SAFETY: a leaked `Class` this crate registered.
    unsafe { &*(p as *const Class) }
}

/// `kCFBooleanTrue` and `kCFBooleanFalse`, one object each for the process.
///
/// Retained forever: a guest that released one could make every later read of
/// the global dangle.
pub fn boolean(value: bool) -> CFTypeRef {
    static TRUE: OnceLock<usize> = OnceLock::new();
    static FALSE: OnceLock<usize> = OnceLock::new();
    let cell = match value {
        true => &TRUE,
        false => &FALSE,
    };
    *cell.get_or_init(|| {
        // `kCFNumberCharType`, which is what Apple's answers for a boolean: it
        // is the one type outside the canonical six, because `-objCType` of one
        // is `c`.
        let body = NumberBody {
            value: Number::Integer(value as i64),
            kind: number_type::CHAR,
        };
        let object = make(boolean_class(), body);
        objc::arc::make_immortal(object as objc::object::Id);
        object as usize
    }) as CFTypeRef
}

/// The CoreFoundation storage of a number or a boolean.
///
/// # Safety
///
/// As [`contents`].
unsafe fn number_body<'a>(object: CFTypeRef) -> Option<&'a mut NumberBody> {
    unsafe {
        contents::<NumberBody>(object, number_class())
            .or_else(|| contents::<NumberBody>(object, boolean_class()))
    }
}

/// The compiler's constant number layouts, as `Foundation` registers their
/// classes. Read here because a literal `@5` reaches CoreFoundation functions
/// as often as a number made at run time does, and the layout is clang's.
#[repr(C)]
struct ConstantInteger {
    isa: *const Class,
    encoding: *const std::ffi::c_char,
    value: i64,
}

#[repr(C)]
struct ConstantDouble {
    isa: *const Class,
    value: f64,
}

#[repr(C)]
struct ConstantFloat {
    isa: *const Class,
    value: f32,
}

/// The canonical type of an integer `-objCType`: an unsigned type is stored in
/// the next wider signed one, as `+numberWithUnsignedInt:` is.
fn integer_encoding_type(encoding: u8) -> CFIndex {
    use number_type as t;
    match encoding {
        b'c' | b'B' => t::SINT8,
        b's' | b'C' => t::SINT16,
        b'i' | b'S' => t::SINT32,
        _ => t::SINT64,
    }
}

/// A constant number the compiler laid out, with its canonical type.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated or bound.
unsafe fn constant_number(object: CFTypeRef) -> Option<(Number, CFIndex)> {
    let class = unsafe { crate::base::class_of(object) }?;
    let is = |name: &std::ffi::CStr| {
        objc::class::lookup(name).is_some_and(|k| class.chain().any(|c| std::ptr::eq(c, k)))
    };
    // SAFETY: an instance of each class has that class's clang layout.
    unsafe {
        if is(c"NSConstantIntegerNumber") {
            let n = &*(object as *const ConstantInteger);
            let encoding = match n.encoding.is_null() {
                true => b'q',
                false => *n.encoding as u8,
            };
            return Some((Number::Integer(n.value), integer_encoding_type(encoding)));
        }
        if is(c"NSConstantDoubleNumber") {
            let n = &*(object as *const ConstantDouble);
            return Some((Number::Float(n.value), number_type::FLOAT64));
        }
        if is(c"NSConstantFloatNumber") {
            let n = &*(object as *const ConstantFloat);
            return Some((Number::Float(n.value as f64), number_type::FLOAT32));
        }
    }
    None
}

/// A number's value and its `CFNumberType`: a CoreFoundation number, a
/// boolean, or a constant the compiler laid out.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated or bound.
pub unsafe fn number_value(object: CFTypeRef) -> Option<(Number, CFIndex)> {
    match unsafe { number_body(object) } {
        Some(body) => Some((body.value, body.kind)),
        None => unsafe { constant_number(object) },
    }
}

/// A number's value; see [`number_value`].
///
/// # Safety
///
/// As [`number_value`].
pub unsafe fn number(object: CFTypeRef) -> Option<Number> {
    unsafe { number_value(object) }.map(|(value, _)| value)
}

/// The `CFNumberType` a number reports; see [`number_value`].
///
/// # Safety
///
/// As [`number_value`].
pub unsafe fn number_type(object: CFTypeRef) -> Option<CFIndex> {
    unsafe { number_value(object) }.map(|(_, kind)| kind)
}

pub fn data_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let p = *CLASS.get_or_init(|| {
        objc::class::ptr(register::<Vec<u8>>(
            c"__NSCFData",
            crate::base::mutable_pair(c"NSData", c"NSMutableData"),
        )) as usize
    });
    // SAFETY: a leaked `Class` this crate registered.
    unsafe { &*(p as *const Class) }
}

pub fn new_data(bytes: Vec<u8>) -> CFTypeRef {
    make(data_class(), bytes)
}

/// # Safety
///
/// As [`contents`].
pub unsafe fn data<'a>(object: CFTypeRef) -> Option<&'a mut Vec<u8>> {
    unsafe { contents(object, data_class()) }
}

/// A `CFIndex` clamped into a slice index, or `None` for out of range.
pub fn at(index: CFIndex, len: usize) -> Option<usize> {
    (index >= 0 && (index as usize) < len).then_some(index as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plain integers posing as pointers, which `Policy::NONE` never reads.
    fn token(n: usize) -> CFTypeRef {
        n as CFTypeRef
    }

    /// Every value hashes alike, so every lookup walks one bucket.
    unsafe extern "C" fn same_hash(_value: CFTypeRef) -> usize {
        0
    }

    unsafe extern "C" fn same_parity(a: CFTypeRef, b: CFTypeRef) -> u8 {
        (a as usize % 2 == b as usize % 2) as u8
    }

    #[test]
    fn dictionary_finds_every_key_left_after_removals() {
        const KEYS: usize = 1000;
        let mut d = DictionaryContents::new(Policy::NONE, Policy::NONE, true);
        for n in 1..=KEYS {
            d.set(token(n), token(n * 10));
        }
        for n in (1..=KEYS).step_by(3) {
            assert_eq!(d.remove(token(n)), Some((token(n), token(n * 10))));
        }
        for n in 1..=KEYS {
            let expected = (n % 3 != 1).then(|| token(n * 10));
            assert_eq!(d.get(token(n)), expected, "key {n}");
        }
        assert_eq!(d.len(), KEYS - KEYS.div_ceil(3));
        assert!(
            d.entries()
                .iter()
                .all(|(k, v)| *v == token(*k as usize * 10))
        );
    }

    #[test]
    fn dictionary_replacing_a_value_keeps_one_entry() {
        let mut d = DictionaryContents::new(Policy::NONE, Policy::NONE, true);
        d.set(token(1), token(10));
        d.set(token(1), token(11));
        assert_eq!(d.entries(), &[(token(1), token(11))]);
    }

    #[test]
    fn set_asks_the_equal_callback_of_members_that_hash_alike() {
        let policy = Policy {
            equal: Some(same_parity),
            hash: Some(same_hash),
            ..Policy::NONE
        };
        let mut set = SetContents::new(policy);
        assert!(set.insert(token(2)));
        assert!(set.insert(token(3)));
        assert!(!set.insert(token(4)));
        assert_eq!(set.member(token(6)), Some(token(2)));
        assert_eq!(set.remove(token(8)), Some(token(2)));
        assert_eq!(set.member(token(5)), Some(token(3)));
        assert_eq!(set.member(token(2)), None);
        assert_eq!(set.values(), &[token(3)]);
    }
}
