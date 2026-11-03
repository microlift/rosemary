use std::sync::OnceLock;

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

pub struct SetContents {
    pub values: Vec<CFTypeRef>,
    pub policy: Policy,
}

impl Drop for SetContents {
    fn drop(&mut self) {
        for value in self.values.drain(..) {
            self.policy.release(value);
        }
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

pub fn new_set(values: &[CFTypeRef], policy: Policy) -> CFTypeRef {
    let mut unique = Vec::with_capacity(values.len());
    for value in values {
        if !unique.iter().any(|v| policy.equal(*v, *value)) {
            unique.push(policy.retain(*value));
        }
    }
    make(
        set_class(),
        SetContents {
            values: unique,
            policy,
        },
    )
}

/// # Safety
///
/// As [`contents`].
pub unsafe fn set<'a>(object: CFTypeRef) -> Option<&'a mut SetContents> {
    unsafe { contents(object, set_class()) }
}

//

pub struct DictionaryContents {
    pub entries: Vec<(CFTypeRef, CFTypeRef)>,
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
    pub fn position(&self, key: CFTypeRef) -> Option<usize> {
        self.entries
            .iter()
            .position(|(k, _)| self.keys.equal(*k, key))
    }

    /// Insert or replace, retaining both as the policies say.
    pub fn set(&mut self, key: CFTypeRef, value: CFTypeRef) {
        let value = self.values.retain(value);
        match self.position(key) {
            Some(at) => {
                self.values.release(self.entries[at].1);
                self.entries[at].1 = value;
            }
            None => self.entries.push((self.keys.retain(key), value)),
        }
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
    let mut contents = DictionaryContents {
        entries: Vec::with_capacity(pairs.len()),
        keys,
        values,
        mutable,
    };
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
    pub const SINT8: i32 = 1;
    pub const SINT16: i32 = 2;
    pub const SINT32: i32 = 3;
    pub const SINT64: i32 = 4;
    pub const FLOAT32: i32 = 5;
    pub const FLOAT64: i32 = 6;
    pub const CHAR: i32 = 7;
    pub const SHORT: i32 = 8;
    pub const INT: i32 = 9;
    pub const LONG: i32 = 10;
    pub const LONG_LONG: i32 = 11;
    pub const FLOAT: i32 = 12;
    pub const DOUBLE: i32 = 13;
    pub const CFINDEX: i32 = 14;
    pub const NSINTEGER: i32 = 15;
    pub const CGFLOAT: i32 = 16;
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
    kind: i32,
}

/// The canonical `CFNumberType` for a type a number can be created with.
pub fn canonical_number_type(kind: i32) -> Option<i32> {
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
pub fn number_shape(kind: i32) -> Option<(usize, bool)> {
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
pub fn new_number_of_type(value: Number, kind: i32) -> CFTypeRef {
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
fn integer_encoding_type(encoding: u8) -> i32 {
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
unsafe fn constant_number(object: CFTypeRef) -> Option<(Number, i32)> {
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
pub unsafe fn number_value(object: CFTypeRef) -> Option<(Number, i32)> {
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
pub unsafe fn number_type(object: CFTypeRef) -> Option<i32> {
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
