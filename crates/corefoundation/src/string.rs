use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;

use objc::class::Class;

use crate::base::{CFIndex, CFTypeRef};

/// `flags` bit: the bytes are UTF-16 rather than 8-bit.
const WIDE: u64 = 0x10;
/// `flags` bit: `str` is storage this runtime allocated and must free.
///
/// One of this crate's own. Apple keeps the same information in the same word,
/// with different bits; nothing outside this file reads it either way.
const OWNED: u64 = 0x1000_0000;
/// `flags` bit: the string was created mutable.
///
/// A field rather than a class, for the reason `ArrayContents::mutable` is one:
/// `__NSCFString` has to descend from `NSMutableString` or a guest's
/// `isKindOfClass:` disagrees with the object it is holding.
const MUTABLE: u64 = 0x2000_0000;

/// The four words a `CFString` is.
#[repr(C)]
pub struct CFString {
    isa: *const Class,
    flags: u64,
    /// The characters, NUL-terminated: 8-bit bytes, or UTF-16 code units when
    /// `flags` carries `WIDE`.
    str: *const u8,
    /// Characters, not bytes. For an 8-bit string the two are the same.
    length: i64,
}

/// `kCFStringEncoding*`, the few a guest actually passes.
pub mod encoding {
    pub const MAC_ROMAN: u32 = 0;
    pub const UTF16: u32 = 0x0100;
    pub const UTF16_BE: u32 = 0x1000_0100;
    pub const UTF16_LE: u32 = 0x1400_0100;
    pub const UTF32: u32 = 0x0c00_0100;
    pub const UTF32_BE: u32 = 0x1800_0100;
    pub const UTF32_LE: u32 = 0x1c00_0100;
    pub const ASCII: u32 = 0x0600;
    pub const UTF8: u32 = 0x0800_0100;
    pub const ISO_LATIN_1: u32 = 0x0201;
    /// `kCFStringEncodingWindowsLatin1`, which is what a guest asking for
    /// "the system encoding" most often gets.
    pub const WINDOWS_LATIN1: u32 = 0x0500;
}

/// Whether `encoding` is one where a byte is a character.
///
/// The eight-bit encodings differ above 0x7F, and this runtime does not carry
/// their tables: a string that is pure ASCII reads back identically under all of
/// them, and one that is not is refused rather than mistranslated.
fn is_eight_bit(encoding: u32) -> bool {
    matches!(
        encoding,
        encoding::UTF8
            | encoding::ASCII
            | encoding::ISO_LATIN_1
            | encoding::MAC_ROMAN
            | encoding::WINDOWS_LATIN1
    )
}

fn is_utf16(encoding: u32) -> bool {
    matches!(
        encoding,
        encoding::UTF16 | encoding::UTF16_BE | encoding::UTF16_LE
    )
}

fn is_utf32(encoding: u32) -> bool {
    matches!(
        encoding,
        encoding::UTF32 | encoding::UTF32_BE | encoding::UTF32_LE
    )
}

/// Split `bytes` into `N`-byte units in `encoding`'s order. The order-less
/// encodings honour a leading byte-order mark and drop it, and are host order
/// without one; an explicit order keeps a leading U+FEFF as a character.
fn units<const N: usize>(bytes: &[u8], encoding: u32, big: u32, little: u32) -> Vec<[u8; N]> {
    let mut chunks = bytes
        .chunks_exact(N)
        .map(|c| <[u8; N]>::try_from(c).unwrap());
    let mut big_endian = encoding == big;
    let mut first = None;
    if encoding != big && encoding != little {
        big_endian = cfg!(target_endian = "big");
        if let Some(head) = chunks.next() {
            let mut mark = [0u8; N];
            mark[N - 2] = 0xFE;
            mark[N - 1] = 0xFF;
            let mut reversed = mark;
            reversed.reverse();
            if head == mark {
                big_endian = true;
            } else if head == reversed {
                big_endian = false;
            } else {
                first = Some(head);
            }
        }
    }
    first
        .into_iter()
        .chain(chunks)
        .map(|mut unit| {
            if big_endian == cfg!(target_endian = "little") {
                unit.reverse();
            }
            unit
        })
        .collect()
}

/// The class a dynamically created string has.
pub fn class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let p = *CLASS.get_or_init(|| {
        let base = crate::base::mutable_pair(c"NSString", c"NSMutableString");
        crate::base::note_type_class(base);
        let class =
            crate::base::register(c"__NSCFString", Some(base), size_of::<CFString>(), false);
        install_string_methods(class);
        objc::class::ptr(class) as usize
    });
    // SAFETY: a leaked `Class` this crate registered.
    unsafe { &*(p as *const Class) }
}

/// The class a *constant* string has, and the address
/// `___CFConstantStringClassReference` is bound to.
///
/// A separate class from [`class`] for one reason: its storage is in the
/// image's `__DATA` and must never be freed. `-release` on one does nothing,
/// which is what makes a guest's `CFRelease(@"x")` harmless.
pub fn constant_class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let p = *CLASS.get_or_init(|| {
        // A *subclass*, so that `CFGetTypeID` on a literal answers
        // `CFStringGetTypeID()` and every string method is inherited.
        let class = crate::base::register(
            c"__NSCFConstantString",
            Some(class()),
            size_of::<CFString>(),
            false,
        );
        install_constant_methods(class);
        objc::class::ptr(class) as usize
    });
    // SAFETY: a leaked `Class` this crate registered.
    unsafe { &*(p as *const Class) }
}

/// A string that no number of releases frees, for a global the guest reads.
///
/// Laid out as a compiled literal is, with [`constant_class`] as its class, so
/// `CFRelease` finds a `-dealloc` that does nothing. A global has to be this
/// rather than an ordinary string with an extra retain: Qt's `QCFString` adopts
/// `kCFPreferencesAnyHost` without retaining it and releases it on destruction,
/// once per `QSettings`, which frees an ordinary string after a few dozen.
pub fn immortal(text: &str) -> CFTypeRef {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    let str = Box::leak(bytes.into_boxed_slice()).as_ptr();
    let object = Box::leak(Box::new(CFString {
        isa: objc::class::ptr(constant_class()),
        flags: 0,
        str,
        length: text.len() as i64,
    }));
    std::ptr::from_mut(object) as CFTypeRef
}

/// The methods a string answers: equality by characters, and the length and
/// bytes an `NSString` caller asks for.
///
/// Without `-isEqual:` two strings spelled the same are unequal, which is the
/// first thing toll-free bridging has to get right - a dictionary keyed by
/// string would otherwise never find anything.
fn install_string_methods(class: &'static Class) {
    unsafe extern "C" fn is_equal(
        receiver: objc::object::Id,
        _sel: objc::sel::Sel,
        other: objc::object::Id,
    ) -> bool {
        if receiver == other {
            return true;
        }
        // SAFETY: the receiver is a string; `other` is checked before it is read.
        unsafe {
            match (
                as_string(receiver as CFTypeRef),
                as_string(other as CFTypeRef),
            ) {
                (Some(a), Some(b)) => a.utf8() == b.utf8(),
                _ => false,
            }
        }
    }

    unsafe extern "C" fn hash(receiver: objc::object::Id, _sel: objc::sel::Sel) -> usize {
        // SAFETY: the receiver is a string.
        let Some(s) = (unsafe { as_string(receiver as CFTypeRef) }) else {
            return 0;
        };
        // FNV-1a over the characters, so that two strings spelled the same hash
        // the same however they were built. Which function it is does not
        // matter; that it is a function of the *characters* does.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in s.utf8() {
            h ^= byte as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h as usize
    }

    unsafe extern "C" fn length(receiver: objc::object::Id, _sel: objc::sel::Sel) -> usize {
        // SAFETY: the receiver is a string.
        unsafe { as_string(receiver as CFTypeRef) }.map_or(0, |s| s.length() as usize)
    }

    /// `-UTF8String`, which never answers null for a string that has
    /// characters - unlike `CFStringGetCStringPtr`, whose contract is that the
    /// answer is free or there is none.
    ///
    /// A wide string has no NUL-terminated UTF-8 to point at, so one is made and
    /// autoreleased: the caller is entitled to use it until the pool drains,
    /// which is exactly what Apple's promises.
    unsafe extern "C" fn utf8_string(
        receiver: objc::object::Id,
        _sel: objc::sel::Sel,
    ) -> *const c_char {
        // SAFETY: the receiver is a string.
        let Some(s) = (unsafe { as_string(receiver as CFTypeRef) }) else {
            return std::ptr::null();
        };
        let free = s.c_string(encoding::UTF8);
        if !free.is_null() {
            return free;
        }
        let mut bytes = s.utf8();
        bytes.push(0);
        let data = crate::collections::new_data(bytes);
        // SAFETY: a fresh object, handed to the pool that will free it.
        let data = unsafe { crate::api::CFAutorelease(data) };
        // SAFETY: a `CFData` this call just made.
        unsafe { crate::api::CFDataGetBytePtr(data) as *const c_char }
    }

    class.add_method(
        objc::sel::get("isEqual:"),
        is_equal as *const c_void,
        c"B24@0:8@16",
    );
    class.add_method(objc::sel::get("hash"), hash as *const c_void, c"Q16@0:8");
    class.add_method(
        objc::sel::get("length"),
        length as *const c_void,
        c"Q16@0:8",
    );
    class.add_method(
        objc::sel::get("UTF8String"),
        utf8_string as *const c_void,
        c"*16@0:8",
    );
    install_dealloc(class);
}

/// `-dealloc` that gives back the bytes before the object.
///
/// Without it the four words are freed and the storage they point at is not,
/// which is a leak per string and so a leak per frame in anything that builds
/// one.
fn install_dealloc(class: &'static Class) {
    unsafe extern "C" fn dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
        // SAFETY: `-dealloc` is only reached with the last reference gone, and
        // the receiver is a `__NSCFString`.
        unsafe {
            release_storage(receiver as *mut CFString);
            objc::arc::finalize(receiver);
        }
    }
    class.replace_method(
        objc::sel::get("dealloc"),
        dealloc as *const c_void,
        c"v16@0:8",
    );
}

/// `-retain`, `-release` and `-dealloc` that do nothing, for a string that lives
/// in an image rather than on the heap.
fn install_constant_methods(class: &'static Class) {
    unsafe extern "C" fn retain(
        receiver: objc::object::Id,
        _sel: objc::sel::Sel,
    ) -> objc::object::Id {
        receiver
    }
    unsafe extern "C" fn release(_receiver: objc::object::Id, _sel: objc::sel::Sel) {}
    unsafe extern "C" fn retain_count(_receiver: objc::object::Id, _sel: objc::sel::Sel) -> usize {
        // What Apple reports for an object that cannot be deallocated.
        usize::MAX
    }
    class.replace_method(
        objc::sel::get("retain"),
        retain as *const c_void,
        c"@16@0:8",
    );
    class.replace_method(
        objc::sel::get("release"),
        release as *const c_void,
        c"v16@0:8",
    );
    class.replace_method(
        objc::sel::get("autorelease"),
        retain as *const c_void,
        c"@16@0:8",
    );
    class.replace_method(
        objc::sel::get("retainCount"),
        retain_count as *const c_void,
        c"Q16@0:8",
    );
    class.replace_method(
        objc::sel::get("dealloc"),
        release as *const c_void,
        c"v16@0:8",
    );
}

/// `CFStringGetTypeID`.
///
/// The *abstract* class's id, not the concrete one's: `CFGetTypeID` of any
/// string has to answer this, and a `__NSCFConstantString` is not a
/// `__NSCFString`'s equal but is an `NSString`'s.
pub fn type_id_of_strings() -> crate::base::CFTypeID {
    crate::base::type_id_of(class())
}

/// `string` as a `CFString`, or `None` if it is not one.
///
/// # Safety
///
/// `string` must be null or an object this runtime allocated.
pub unsafe fn as_string<'a>(string: CFTypeRef) -> Option<&'a CFString> {
    let is_string = unsafe {
        crate::base::is_kind_of(string, class())
            || crate::base::is_kind_of(string, constant_class())
    };
    // SAFETY: a `__NSCFString` or `__NSCFConstantString` is a `CFString`.
    is_string.then(|| unsafe { &*(string as *const CFString) })
}

impl CFString {
    /// The characters, as UTF-8 bytes.
    ///
    /// A copy for a wide string and a borrow for an 8-bit one, which is why the
    /// return is owned: the two cases have to look the same to a caller.
    pub fn utf8(&self) -> Vec<u8> {
        if self.str.is_null() || self.length <= 0 {
            return Vec::new();
        }
        let count = self.length as usize;
        if self.flags & WIDE == 0 {
            // SAFETY: `str` points at `length` bytes, by the layout above.
            return unsafe { std::slice::from_raw_parts(self.str, count) }.to_vec();
        }
        // SAFETY: `str` points at `length` UTF-16 code units.
        let units = unsafe { std::slice::from_raw_parts(self.str as *const u16, count) };
        String::from_utf16_lossy(units).into_bytes()
    }

    /// The characters, as UTF-16 code units.
    ///
    /// The counterpart of [`CFString::utf8`], and the representation a wide
    /// string keeps: a copy of one has to be wide too, or its length stops
    /// being its character count.
    pub fn utf16(&self) -> Vec<u16> {
        if self.str.is_null() || self.length <= 0 {
            return Vec::new();
        }
        let count = self.length as usize;
        if self.is_wide() {
            // SAFETY: `str` points at `length` UTF-16 code units.
            return unsafe { std::slice::from_raw_parts(self.str as *const u16, count) }.to_vec();
        }
        String::from_utf8_lossy(&self.utf8())
            .encode_utf16()
            .collect()
    }

    /// The code units `CFStringGetLength` counts.
    ///
    /// For a wide string these are the characters. For an 8-bit one they are
    /// its *bytes*, widened one for one, because that is what its `length`
    /// counts - see [`from_encoded`], where a UTF-8 string keeps its bytes and
    /// a length in bytes with them. Indexing has to agree with the length a
    /// caller sized its buffer from, whatever the two together say about a
    /// non-ASCII character.
    pub fn units(&self) -> Vec<u16> {
        match self.is_wide() {
            true => self.utf16(),
            false => self.utf8().into_iter().map(u16::from).collect(),
        }
    }

    /// The characters as `UniChar`s, if they already are some.
    ///
    /// `CFStringGetCharactersPtr`'s contract, which is `c_string`'s in the other
    /// direction: null unless the answer costs nothing.
    pub fn characters_ptr(&self) -> *const u16 {
        match self.is_wide() && !self.str.is_null() {
            true => self.str as *const u16,
            false => std::ptr::null(),
        }
    }

    /// Whether the characters are code units rather than bytes.
    pub fn is_wide(&self) -> bool {
        self.flags & WIDE != 0
    }

    /// Characters, which is what `CFStringGetLength` reports.
    pub fn length(&self) -> CFIndex {
        self.length.max(0) as CFIndex
    }

    /// The bytes as a NUL-terminated C string, if they already are one.
    ///
    /// `CFStringGetCStringPtr`'s contract: null unless the answer is free. A
    /// wide string is never free, and neither is one whose storage this runtime
    /// did not NUL-terminate.
    pub fn c_string(&self, encoding: u32) -> *const c_char {
        match self.flags & WIDE == 0 && is_eight_bit(encoding) && !self.str.is_null() {
            true => self.str as *const c_char,
            false => std::ptr::null(),
        }
    }
}

/// Replace a mutable string's characters with code units, as [`set_bytes`] does
/// with bytes.
///
/// # Safety
///
/// As [`set_bytes`].
pub unsafe fn set_utf16(string: CFTypeRef, units: &[u16]) -> bool {
    if !unsafe { is_mutable(string) } {
        return false;
    }
    let object = string as *mut CFString;
    // SAFETY: a mutable `__NSCFString`, whose storage this crate allocated.
    unsafe {
        release_storage(object);
        let mut owned = units.to_vec();
        owned.push(0);
        (*object).str = Box::into_raw(owned.into_boxed_slice()) as *const u8;
        (*object).length = units.len() as i64;
        (*object).flags |= OWNED | WIDE;
    }
    true
}

/// Build a string from 8-bit bytes this runtime will own, or from UTF-16 bytes
/// in this machine's order.
///
/// The storage is NUL-terminated even though the length is carried separately,
/// so that `CFStringGetCStringPtr` can answer without copying - which is the one
/// thing a caller of it is entitled to expect.
pub fn from_bytes(bytes: &[u8], wide: bool) -> CFTypeRef {
    // 8-bit storage counts bytes as characters, which is only true of ASCII:
    // other UTF-8 is kept as UTF-16, so its length and indices are the ones a
    // Mac reports, and a caller sizing a buffer from them does not cut a
    // character in half.
    if !wide && !bytes.is_ascii() {
        if let Ok(text) = std::str::from_utf8(bytes) {
            return from_utf16(&text.encode_utf16().collect::<Vec<u16>>());
        }
    }
    if wide {
        // An odd trailing byte is not a code unit and there is nothing to pair
        // it with, so it is dropped rather than paired with whatever follows.
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return from_utf16(&units);
    }
    // SAFETY: `CFString` begins with `isa` and is the layout `class` was
    // registered with.
    let object: *mut CFString = unsafe { crate::base::allocate(class()) };
    if object.is_null() {
        return std::ptr::null();
    }
    let mut owned = bytes.to_vec();
    owned.push(0);
    let str = Box::into_raw(owned.into_boxed_slice()) as *const u8;
    // SAFETY: `object` is a live instance of `class()`, so its four words are
    // ours to fill in. The `isa` is already set.
    unsafe {
        (*object).flags = OWNED;
        (*object).str = str;
        (*object).length = bytes.len() as i64;
    }
    object as CFTypeRef
}

/// Build a wide string from code units this runtime will own.
///
/// The storage is allocated as `u16` rather than as bytes because every reader
/// of a wide string takes `str` as a `*const u16` - a byte allocation is only
/// aligned for one of those by luck, and [`release_storage`] has to hand back
/// what was allocated. Terminated with a NUL unit for the same reason the 8-bit
/// case is: a guest that walks the characters expects the end to be there.
pub fn from_utf16(units: &[u16]) -> CFTypeRef {
    // SAFETY: `CFString` begins with `isa` and is the layout `class` was
    // registered with.
    let object: *mut CFString = unsafe { crate::base::allocate(class()) };
    if object.is_null() {
        return std::ptr::null();
    }
    let mut owned = units.to_vec();
    owned.push(0);
    let str = Box::into_raw(owned.into_boxed_slice()) as *const u8;
    unsafe {
        (*object).flags = OWNED | WIDE;
        (*object).str = str;
        (*object).length = units.len() as i64;
    }
    object as CFTypeRef
}

/// Copy a string's characters into a new mutable one, keeping the
/// representation.
///
/// A mutable copy exists to be appended to, and an append that changed how the
/// characters are stored would change what `CFStringGetLength` counts: an 8-bit
/// string's length is its bytes, a wide one's is its characters.
pub fn new_mutable_copy(s: &CFString) -> CFTypeRef {
    let object = match s.is_wide() {
        true => from_utf16(&s.utf16()),
        false => from_bytes(&s.utf8(), false),
    };
    if !object.is_null() {
        // SAFETY: a `__NSCFString` this call just made.
        unsafe { (*(object as *mut CFString)).flags |= MUTABLE };
    }
    object
}

/// Build an empty mutable string.
pub fn new_mutable() -> CFTypeRef {
    let object = from_bytes(&[], false);
    if !object.is_null() {
        // SAFETY: a `__NSCFString` this call just made.
        unsafe { (*(object as *mut CFString)).flags |= MUTABLE };
    }
    object
}

/// Whether `string` may be changed.
///
/// # Safety
///
/// As [`as_string`].
pub unsafe fn is_mutable(string: CFTypeRef) -> bool {
    unsafe { as_string(string) }.is_some_and(|s| s.flags & MUTABLE != 0)
}

/// Replace a mutable string's characters, freeing what was there.
///
/// `false` for a string that was not created mutable, which is the caller's to
/// report by name - the two spellings of that mistake, `-appendString:` on a
/// literal and on an immutable copy, are both worth naming.
///
/// # Safety
///
/// As [`as_string`].
pub unsafe fn set_bytes(string: CFTypeRef, bytes: &[u8]) -> bool {
    if !unsafe { is_mutable(string) } {
        return false;
    }
    // Non-ASCII text is kept as code units, for the reason `from_bytes` does.
    if !bytes.is_ascii()
        && let Ok(text) = std::str::from_utf8(bytes)
    {
        return unsafe { set_utf16(string, &text.encode_utf16().collect::<Vec<u16>>()) };
    }
    let object = string as *mut CFString;
    // SAFETY: a mutable `__NSCFString`, whose storage this crate allocated.
    unsafe {
        release_storage(object);
        let mut owned = bytes.to_vec();
        owned.push(0);
        (*object).str = Box::into_raw(owned.into_boxed_slice()) as *const u8;
        (*object).length = bytes.len() as i64;
        // The new storage is 8-bit whatever the old was, and a stale `WIDE`
        // would have `release_storage` free it as code units.
        (*object).flags = (*object).flags & !WIDE | OWNED;
    }
    true
}

/// Build a string from bytes in `encoding`, or null for an encoding this
/// runtime does not carry a table for.
pub fn from_encoded(bytes: &[u8], encoding: u32) -> CFTypeRef {
    if is_eight_bit(encoding) {
        // UTF-8 is its own storage; the one-byte encodings go through their
        // tables, and ASCII refuses anything above 0x7F.
        if encoding == encoding::UTF8 || bytes.is_ascii() {
            return from_bytes(bytes, false);
        }
        return match crate::eightbit::decode_all(bytes, encoding) {
            Some(units) => from_utf16(&units),
            None => std::ptr::null(),
        };
    }
    if is_utf16(encoding) {
        let units: Vec<u16> = units::<2>(bytes, encoding, encoding::UTF16_BE, encoding::UTF16_LE)
            .into_iter()
            .map(u16::from_ne_bytes)
            .collect();
        return from_utf16(&units);
    }
    if is_utf32(encoding) {
        let mut wide = Vec::with_capacity(bytes.len() / 4);
        for unit in units::<4>(bytes, encoding, encoding::UTF32_BE, encoding::UTF32_LE) {
            let Some(ch) = char::from_u32(u32::from_ne_bytes(unit)) else {
                return std::ptr::null();
            };
            wide.extend_from_slice(ch.encode_utf16(&mut [0; 2]));
        }
        return from_utf16(&wide);
    }
    std::ptr::null()
}

/// Free the storage of a string this runtime allocated.
///
/// # Safety
///
/// `object` must be a `__NSCFString` with no remaining references.
pub unsafe fn release_storage(object: *mut CFString) {
    let flags = unsafe { (*object).flags };
    if flags & OWNED == 0 {
        return;
    }
    // SAFETY: `str` came from `Box::into_raw` in `from_bytes` or `from_utf16`,
    // and is freed as what was allocated there - bytes or code units, one more
    // than the length records, for the terminator.
    unsafe {
        let count = (*object).length as usize + 1;
        match flags & WIDE == 0 {
            true => drop(Box::from_raw(std::slice::from_raw_parts_mut(
                (*object).str as *mut u8,
                count,
            ))),
            false => drop(Box::from_raw(std::slice::from_raw_parts_mut(
                (*object).str as *mut u16,
                count,
            ))),
        }
        (*object).str = std::ptr::null();
        (*object).flags &= !OWNED;
    }
}

/// The bytes of a NUL-terminated C string, or `None` for null.
///
/// # Safety
///
/// `p` must be null or NUL-terminated.
pub unsafe fn cstr_bytes<'a>(p: *const c_char) -> Option<&'a [u8]> {
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_bytes())
}
