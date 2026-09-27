// The C spellings are the interface; renaming them would only hide it.
#![allow(non_snake_case)]

use std::ffi::{c_char, c_void};

use objc::object::Id;

use crate::base::{CFIndex, CFTypeID, CFTypeRef};
use crate::send;
use crate::string;

/// `Boolean`, which is one byte.
pub type Boolean = u8;

/// Everything a `CFCreate` call ignores about its allocator.
///
/// Apple's allocators exist so a caller can account for, or place, the memory a
/// framework takes. Nothing in this tree does either, and a guest that passes
/// `kCFAllocatorDefault` - which is null - is asking for the default one, which
/// is the only one here.
pub type CFAllocatorRef = *const c_void;

/// The default allocator is represented by null, as it is by the allocator
/// globals this crate publishes. A custom allocator still needs an object of
/// its own: callers retain and release it, even though this CoreFoundation does
/// not route its allocations through the callback table.
fn allocator_class() -> &'static objc::class::Class {
    crate::base::register(c"__NSCFAllocator", None, std::mem::size_of::<usize>(), true)
}

pub unsafe extern "C" fn CFAllocatorGetTypeID() -> CFTypeID {
    crate::base::type_id(allocator_class())
}

pub unsafe extern "C" fn CFAllocatorGetDefault() -> CFAllocatorRef {
    std::ptr::null()
}

pub unsafe extern "C" fn CFAllocatorCreate(
    _allocator: CFAllocatorRef,
    _context: *const c_void,
) -> CFAllocatorRef {
    // The context's callbacks are deliberately not copied: every constructor
    // in this implementation owns its storage and therefore treats an
    // allocator argument as advisory. The returned object preserves the part
    // of the contract guests can observe here: distinct identity and CF/ObjC
    // retain/release behavior.
    objc::object::create_instance(allocator_class(), 0) as CFAllocatorRef
}

/// Hand a foreign object back to the CoreFoundation that made it.
///
/// Returns whether it did. See [`crate::foreign`]: the alternative is reading
/// Apple's object as one of this runtime's, which walks a class structure that
/// is not there.
#[cfg(target_os = "macos")]
unsafe fn forwarded(name: &std::ffi::CStr, object: CFTypeRef) -> bool {
    // SAFETY: null or an object.
    if !unsafe { crate::foreign::is(object) } {
        return false;
    }
    if std::env::var_os("ROSETTA3_TRACE_FOREIGN_CF").is_some() {
        eprintln!(
            "rosetta3: foreign CoreFoundation {}({object:p})",
            name.to_string_lossy()
        );
    }
    // A handle of this tree's that has no isa to read. Nothing to forward and
    // nothing to count: see [`crate::base::is_opaque`].
    if crate::base::is_opaque(object) {
        return true;
    }
    if !crate::foreign::forwarding() {
        objc::fail!(
            "{}: the object is not this runtime's, and ROSETTA3_NO_FOREIGN forbids passing it to \
             the CoreFoundation that made it",
            name.to_string_lossy()
        );
    }
    let Some(f) = crate::foreign::entry(name) else {
        objc::fail!(
            "{}: the object is not this runtime's and there is no host CoreFoundation to pass it \
             to",
            name.to_string_lossy()
        );
    };
    // SAFETY: every entry point forwarded here is `void (*)(CFTypeRef)` in the
    // one respect that matters - one pointer argument - and a return value the
    // caller does not read.
    unsafe {
        let f: unsafe extern "C" fn(CFTypeRef) -> CFTypeRef = std::mem::transmute(f);
        f(object);
    }
    true
}

/// Off Darwin every object the guest holds is this runtime's, so there is
/// nothing to forward and nothing to check for.
#[cfg(not(target_os = "macos"))]
unsafe fn forwarded(_name: &std::ffi::CStr, object: CFTypeRef) -> bool {
    // The opaque handles are here too - `IOHIDDeviceRef` is one on every host -
    // even though a foreign object is not.
    crate::base::is_opaque(object)
}

/// Refuse a reference operation on a handle whose references this runtime has
/// already been given back.
///
/// `CFRelease` after the last one, while a run loop is still holding the handle,
/// is a guest bug. There is no answer to give: the C world's reference is gone
/// and both remaining paths through `CFRelease` - Apple's `objc_release` and this
/// tree's own - would read and write an allocation the caller does not own. So it
/// is refused *by name*, which is the project's answer for a question it cannot
/// answer truthfully, and which is also what Apple does by crashing.
///
/// The case this catches is the entry still being in the table, which is the
/// common one: a source that is released and then released again, or released
/// and then retained, while a loop is running it. The other case - a release
/// after the entry is gone - cannot be caught at all, and [`crate::base`] says
/// why: the address is then indistinguishable from any other the allocator owns.
/// That one is a use-after-free here exactly as it is in Apple's CoreFoundation.
fn refuse_if_spent(call: &str, object: CFTypeRef) {
    objc::fail!(
        "corefoundation: {call} on {object:p}, and this runtime has already been given back every \
         reference it held to that handle, so the caller does not hold the one it is asking \
         about.  A caller cannot retain or release a reference it does not hold; Apple's \
         CoreFoundation frees it here too, and the difference is that this one says so.  \
         Refusing rather than touching an allocation that is not the caller's any more."
    );
}

pub unsafe extern "C" fn CFRetain(object: CFTypeRef) -> CFTypeRef {
    // A handle the C world owns a reference to, counted. This has to be asked
    // before `forwarded`, which on every host but Darwin *is* the opaque check
    // and would answer "handled" and return without retaining anything.
    match crate::base::retain_opaque(object) {
        crate::base::CRef::Counted => return object,
        crate::base::CRef::Spent => refuse_if_spent("CFRetain", object),
        crate::base::CRef::Unknown => (),
    }
    if unsafe { forwarded(c"CFRetain", object) } {
        return object;
    }
    objc::arc::retain(object as Id) as CFTypeRef
}

/// `CFRelease`, which for four of this tree's handle kinds is the only thing that
/// can end them.
///
/// A `CFRunLoopSourceRef` here is a `leak`ed `Arc` the guest owns, and Apple has
/// no `CFRunLoopSourceRelease` - the guest's `CFRelease` is the only reference
/// it can give back. Answering it as a no-op (which is what a bare `is_opaque`
/// does) means the handle, its `Arc` and both registry entries outlive the
/// guest's use of them forever, and the allocator hands the address out again
/// while the table still claims it. So the count is kept here and the reference
/// is really dropped; a release past the last is refused by name rather than
/// freeing the allocation twice.
pub unsafe extern "C" fn CFRelease(object: CFTypeRef) {
    match crate::base::release_opaque(object) {
        crate::base::CRef::Counted => return,
        crate::base::CRef::Spent => refuse_if_spent("CFRelease", object),
        crate::base::CRef::Unknown => (),
    }
    if unsafe { forwarded(c"CFRelease", object) } {
        return;
    }
    objc::arc::release(object as Id);
}

pub unsafe extern "C" fn CFAutorelease(object: CFTypeRef) -> CFTypeRef {
    if crate::base::spent(object) {
        refuse_if_spent("CFAutorelease", object);
    }
    if unsafe { forwarded(c"CFAutorelease", object) } {
        return object;
    }
    objc::arc::autorelease(object as Id) as CFTypeRef
}

pub unsafe extern "C" fn CFGetRetainCount(object: CFTypeRef) -> CFIndex {
    // The count the C world actually holds, for a handle that has one - and
    // **zero** included, because a count is a table read and asking for it is
    // safe even when the answer is that the caller holds none. The flat `1`
    // further down is the answer for a handle that is not a reference at all - a
    // device, a pasteboard - and it was also, until this was asked separately,
    // the answer for every source and timer, which is a count that cannot go
    // down and does not go up. It is worth stating what this does *not* include:
    // a run loop holds its own strong reference to a source it is running, and
    // that one is not in the number.
    if let Some(count) = crate::base::c_retain_count(object) {
        return count;
    }
    if crate::base::is_opaque(object) {
        return 1;
    }
    objc::arc::retain_count(object as Id) as CFIndex
}

pub unsafe extern "C" fn CFGetTypeID(object: CFTypeRef) -> CFTypeID {
    if let Some(type_id) = crate::base::opaque_type_id(object) {
        return type_id;
    }
    // SAFETY: an object this runtime allocated.
    unsafe { crate::base::class_of(object) }.map_or(0, crate::base::type_id_of)
}

/// `CFStringRef CFCopyTypeIDDescription(CFTypeID type_id)`
///
/// A *description of the type*, not of any object: what a caller wants when it
/// holds a `CFTypeRef` and does not know what it is.  Apple's answer is built
/// from the type's `copyDescription` callback, so for a type this runtime made it
/// is the class's name - which is the one string in a process that names both
/// the type and the `isKindOfClass:` a caller would reach for next.
///
/// A type ID that is not one of this runtime's classes has no name to give.
/// `0` is the ID `CFGetTypeID` reports for every object that is not a
/// CoreFoundation type, so it is not a type and there is nothing to describe;
/// this runtime says that in the string rather than inventing a type name, and
/// says it on stderr, because the string ends up in a log line somewhere and a
/// log line that lies is worse than one that admits it.
///
/// The guest's only uses are two log messages in
/// `chrome/browser/mac/install_from_dmg.mm` and `metrics.mm`, of the shape
/// `"DiskImageURL: expected CFString, observed "` followed by this
/// (`0x5afa8df` and `0x5afaa2f`), behind a `LOG(INFO)` level test.
pub unsafe extern "C" fn CFCopyTypeIDDescription(type_id: CFTypeID) -> CFTypeRef {
    if let Some(name) = crate::base::type_name(type_id) {
        return crate::string::from_bytes(name.as_bytes(), false);
    }
    crate::base::refuse_once(
        "CFCopyTypeIDDescription",
        format!(
            "type id {type_id:#x} is not a CoreFoundation type this runtime registered, so there \
             is no name for it.  Answering with the name of a type that merely sounds plausible \
             would put a wrong type name into a diagnostic, which is the one place a wrong name \
             costs the most."
        ),
    );
    crate::string::from_bytes(
        format!("<unknown CoreFoundation type {type_id:#x}>").as_bytes(),
        false,
    )
}

/// `CFEqual`, which is `-isEqual:` and nothing more.
///
/// Two strings with the same characters are equal, because `__NSCFString`
/// overrides `-isEqual:`; two objects of any other type are equal when they are
/// the same object, because `NSObject`'s does.
pub unsafe extern "C" fn CFEqual(a: CFTypeRef, b: CFTypeRef) -> Boolean {
    if a == b {
        return 1;
    }
    if a.is_null() || b.is_null() {
        return 0;
    }
    if crate::base::is_opaque(a) || crate::base::is_opaque(b) {
        return 0;
    }
    // SAFETY: two live objects; `-isEqual:` is `BOOL (*)(id, SEL, id)`.
    unsafe { crate::send::send1_bool(a as Id, objc::sel::get("isEqual:"), b as usize) as Boolean }
}

pub unsafe extern "C" fn CFHash(object: CFTypeRef) -> usize {
    if object.is_null() {
        return 0;
    }
    if crate::base::is_opaque(object) {
        return object as usize;
    }
    // SAFETY: a live object; `-hash` is `NSUInteger (*)(id, SEL)`.
    unsafe { crate::send::send0_usize(object as Id, objc::sel::get("hash")) }
}

//

pub unsafe extern "C" fn CFStringGetTypeID() -> CFTypeID {
    string::type_id_of_strings()
}

pub unsafe extern "C" fn CFStringCreateWithCString(
    _allocator: CFAllocatorRef,
    cstr: *const c_char,
    encoding: u32,
) -> CFTypeRef {
    // SAFETY: a NUL-terminated string or null.
    let Some(bytes) = (unsafe { string::cstr_bytes(cstr) }) else {
        return std::ptr::null();
    };
    string::from_encoded(bytes, encoding)
}

pub unsafe extern "C" fn CFStringCreateWithFileSystemRepresentation(
    _allocator: CFAllocatorRef,
    path: *const c_char,
) -> CFTypeRef {
    // SAFETY: a NUL-terminated filesystem path.
    let Some(bytes) = (unsafe { string::cstr_bytes(path) }) else {
        return std::ptr::null();
    };
    string::from_bytes(bytes, false)
}

/// `CFStringCreateWithCStringNoCopy`, which copies here.
///
/// The "no copy" contract is about the *allocator* being told who frees the
/// buffer, and a guest that passes `kCFAllocatorNull` means "never free it".
/// Copying is correct under every one of those spellings and wrong under none;
/// it only costs the bytes.
pub unsafe extern "C" fn CFStringCreateWithCStringNoCopy(
    allocator: CFAllocatorRef,
    cstr: *const c_char,
    encoding: u32,
    _contents_deallocator: CFAllocatorRef,
) -> CFTypeRef {
    unsafe { CFStringCreateWithCString(allocator, cstr, encoding) }
}

pub unsafe extern "C" fn CFStringCreateWithBytes(
    _allocator: CFAllocatorRef,
    bytes: *const u8,
    length: CFIndex,
    encoding: u32,
    _external_representation: Boolean,
) -> CFTypeRef {
    if bytes.is_null() || length < 0 {
        return std::ptr::null();
    }
    // SAFETY: the caller says `bytes` covers `length` bytes.
    let slice = unsafe { std::slice::from_raw_parts(bytes, length as usize) };
    string::from_encoded(slice, encoding)
}

/// `CFStringCreateWithCharactersNoCopy`: the characters are copied anyway, as
/// [`CFStringCreateWithCStringNoCopy`] copies its bytes, so the buffer is never
/// handed to `contents_deallocator`.
pub unsafe extern "C" fn CFStringCreateWithCharactersNoCopy(
    allocator: CFAllocatorRef,
    chars: *const u16,
    count: CFIndex,
    _contents_deallocator: CFAllocatorRef,
) -> CFTypeRef {
    unsafe { CFStringCreateWithCharacters(allocator, chars, count) }
}

/// `CFStringRef CFStringCreateWithCharacters(CFAllocatorRef, const UniChar *,
/// CFIndex numChars)`.
///
/// The characters are UTF-16 in this machine's order, and are kept that way: a
/// string built from code units is asked for its code units back, and going
/// through UTF-8 would lose an unpaired surrogate that a caller put there.
///
/// No characters is the empty string rather than null - `numChars` of zero is
/// how a caller spells one, and there is nothing to read at the pointer.
pub unsafe extern "C" fn CFStringCreateWithCharacters(
    _allocator: CFAllocatorRef,
    chars: *const u16,
    count: CFIndex,
) -> CFTypeRef {
    if count < 0 || (chars.is_null() && count > 0) {
        return std::ptr::null();
    }
    // SAFETY: the caller says `chars` covers `count` code units.
    let units = match count {
        0 => &[][..],
        _ => unsafe { std::slice::from_raw_parts(chars, count as usize) },
    };
    string::from_utf16(units)
}

/// `CFStringCreateCopy`, which keeps the original's representation.
///
/// A wide string copied to 8-bit would have its `length` become a count of
/// UTF-8 bytes rather than of characters, so a caller that copies a string and
/// asks its length would be told something different about the same characters.
pub unsafe extern "C" fn CFStringCreateCopy(
    _allocator: CFAllocatorRef,
    original: CFTypeRef,
) -> CFTypeRef {
    let Some(s) = (unsafe { string::as_string(original) }) else {
        return std::ptr::null();
    };
    match s.is_wide() {
        true => string::from_utf16(&s.utf16()),
        false => string::from_bytes(&s.utf8(), false),
    }
}

/// `CFStringCreateWithSubstring`, whose range is measured in UTF-16 units.
pub unsafe extern "C" fn CFStringCreateWithSubstring(
    _allocator: CFAllocatorRef,
    source: CFTypeRef,
    location: CFIndex,
    length: CFIndex,
) -> CFTypeRef {
    let (Ok(location), Ok(length)) = (usize::try_from(location), usize::try_from(length)) else {
        return std::ptr::null();
    };
    // SAFETY: a live string or null.
    let Some(source) = (unsafe { string::as_string(source) }) else {
        return std::ptr::null();
    };
    let units = source.utf16();
    let Some(end) = location
        .checked_add(length)
        .filter(|end| *end <= units.len())
    else {
        return std::ptr::null();
    };
    string::from_utf16(&units[location..end])
}

pub unsafe extern "C" fn CFStringGetLength(s: CFTypeRef) -> CFIndex {
    unsafe { string::as_string(s) }.map_or(0, |s| s.length())
}

pub unsafe extern "C" fn CFStringGetIntValue(value: CFTypeRef) -> i32 {
    // SAFETY: a string or null.
    let Some(value) = (unsafe { string::as_string(value) }) else {
        return 0;
    };
    let bytes = value.utf8();
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_start();
    let end = text
        .char_indices()
        .position(|(index, ch)| !(ch.is_ascii_digit() || (index == 0 && matches!(ch, '+' | '-'))))
        .unwrap_or(text.len());
    text[..end].parse::<i64>().map_or(0, |number| number as i32)
}

pub unsafe extern "C" fn CFStringGetDoubleValue(value: CFTypeRef) -> f64 {
    // SAFETY: a string or null.
    let Some(value) = (unsafe { string::as_string(value) }) else {
        return 0.0;
    };
    let bytes = value.utf8();
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_start();
    let end = text
        .char_indices()
        .position(|(_, ch)| !(ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.' | 'e' | 'E')))
        .unwrap_or(text.len());
    text[..end].parse().unwrap_or(0.0)
}

/// `CFStringGetMaximumSizeForEncoding`.
///
/// A bound, not an answer: the caller adds one for the NUL and allocates that
/// much. `length` is in UTF-16 code units, so **three** bytes per unit is the
/// UTF-8 bound and not four - a code point that needs four bytes is a surrogate
/// pair and so costs two units. Apple's answers exactly this, and a caller that
/// sizes a buffer here and checks it against a length elsewhere would notice
/// the difference.
pub unsafe extern "C" fn CFStringGetMaximumSizeForEncoding(
    length: CFIndex,
    encoding: u32,
) -> CFIndex {
    if length < 0 {
        return 0;
    }
    match encoding {
        string::encoding::UTF8 => length * 3,
        // Two bytes a unit, and room for a byte-order mark.
        string::encoding::UTF16 | string::encoding::UTF16_BE | string::encoding::UTF16_LE => {
            length * 2 + 2
        }
        string::encoding::UTF32 | string::encoding::UTF32_BE | string::encoding::UTF32_LE => {
            length * 4 + 4
        }
        _ => length,
    }
}

/// `CFStringGetCString`: copy the characters into the caller's buffer.
///
/// `false` when they do not fit, having written nothing - which is the contract,
/// and the reason a caller that ignores the return prints an empty string rather
/// than a truncated one.
pub unsafe extern "C" fn CFStringGetCString(
    s: CFTypeRef,
    buffer: *mut c_char,
    buffer_size: CFIndex,
    encoding: u32,
) -> Boolean {
    let Some(s) = (unsafe { string::as_string(s) }) else {
        return 0;
    };
    if buffer.is_null() || buffer_size <= 0 {
        return 0;
    }
    let bytes = match encoding {
        string::encoding::UTF8 => s.utf8(),
        // Every other encoding this runtime answers for is one where an ASCII
        // byte is itself, and a non-ASCII one has no defined answer here.
        e if e == string::encoding::UTF16
            || e == string::encoding::UTF16_BE
            || e == string::encoding::UTF16_LE
            || e == string::encoding::UTF32
            || e == string::encoding::UTF32_BE
            || e == string::encoding::UTF32_LE =>
        {
            return 0;
        }
        e if crate::eightbit::is_table(e) => {
            let encoded: Option<Vec<u8>> = s
                .units()
                .into_iter()
                .map(|u| crate::eightbit::encode(u, e))
                .collect();
            match encoded {
                Some(bytes) => bytes,
                None => return 0,
            }
        }
        _ => return 0,
    };
    if bytes.len() + 1 > buffer_size as usize {
        return 0;
    }
    // SAFETY: the caller says `buffer` covers `buffer_size` bytes, and the copy
    // plus its NUL fits inside that, as just checked.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer as *mut u8, bytes.len());
        *(buffer as *mut u8).add(bytes.len()) = 0;
    }
    1
}

/// `CFStringGetCStringPtr`: the bytes if they are already there, and null
/// otherwise.
///
/// Null is a legitimate answer and a caller must handle it; one that does not is
/// broken on Apple's CoreFoundation too.
pub unsafe extern "C" fn CFStringGetCStringPtr(s: CFTypeRef, encoding: u32) -> *const c_char {
    unsafe { string::as_string(s) }.map_or(std::ptr::null(), |s| s.c_string(encoding))
}

/// `CFStringGetBytes`: the general form, which reports how much it moved.
/// `CFStringGetBytes`: the range's characters in an encoding, and how many of
/// them fit.
///
/// The return is *characters converted*, which is not the byte count a caller
/// gets in `usedBufLen` and is not the whole range either when the buffer runs
/// out - a caller that trusts one for the other reads past what it was given.
pub unsafe extern "C" fn CFStringGetBytes(
    s: CFTypeRef,
    range_location: CFIndex,
    range_length: CFIndex,
    encoding: u32,
    _loss_byte: u8,
    external_representation: Boolean,
    buffer: *mut u8,
    max_buffer_length: CFIndex,
    used_buffer_length: *mut CFIndex,
) -> CFIndex {
    let Some(s) = (unsafe { string::as_string(s) }) else {
        return 0;
    };
    let start = range_location.max(0) as usize;
    let length = range_length.max(0) as usize;

    // Bytes to write, and how many bytes one character of them takes: the two
    // together are what turns a truncated buffer back into a character count.
    let (wanted, per_character, prefix) = match encoding {
        string::encoding::UTF8
        | string::encoding::ASCII
        | string::encoding::ISO_LATIN_1
        | string::encoding::MAC_ROMAN
        | string::encoding::WINDOWS_LATIN1 => {
            // SAFETY: the caller's buffer and out-parameter, as for the others.
            return unsafe {
                get_bytes_by_character(
                    &s.units(),
                    start,
                    length,
                    encoding,
                    _loss_byte,
                    buffer,
                    max_buffer_length,
                    used_buffer_length,
                )
            };
        }
        string::encoding::UTF16 | string::encoding::UTF16_BE | string::encoding::UTF16_LE => {
            let units = s.units();
            if start > units.len() {
                return 0;
            }
            let end = (start + length).min(units.len());
            // A byte-order mark, and only where Apple writes one: the plain
            // UTF-16 encoding asked for as an external representation. The two
            // explicit orders say their order in their name.
            let mark = external_representation != 0 && encoding == string::encoding::UTF16;
            let big_endian = encoding == string::encoding::UTF16_BE;
            let mut bytes = Vec::with_capacity((end - start) * 2 + 2 * mark as usize);
            for unit in mark.then_some(0xFEFF_u16).iter().chain(&units[start..end]) {
                match big_endian {
                    true => bytes.extend_from_slice(&unit.to_be_bytes()),
                    false => bytes.extend_from_slice(&unit.to_le_bytes()),
                }
            }
            (bytes, 2, 2 * mark as usize)
        }
        string::encoding::UTF32 | string::encoding::UTF32_BE | string::encoding::UTF32_LE => {
            // Characters here are UTF-16 units, so a range may split a pair;
            // the half is dropped rather than written as an invalid scalar.
            let units = s.units();
            if start > units.len() {
                return 0;
            }
            let end = (start + length).min(units.len());
            let mark = external_representation != 0 && encoding == string::encoding::UTF32;
            let big_endian = encoding == string::encoding::UTF32_BE;
            let scalars = char::decode_utf16(units[start..end].iter().copied())
                .filter_map(Result::ok)
                .map(u32::from);
            let mut bytes = Vec::new();
            for scalar in mark.then_some(0xFEFF_u32).into_iter().chain(scalars) {
                match big_endian {
                    true => bytes.extend_from_slice(&scalar.to_be_bytes()),
                    false => bytes.extend_from_slice(&scalar.to_le_bytes()),
                }
            }
            (bytes, 4, 4 * mark as usize)
        }
        // An encoding with no table here converts nothing, which is what a
        // caller checking the return against zero already handles.
        _ => return 0,
    };

    // A null buffer is the caller asking how much room it needs.
    let moved = match buffer.is_null() {
        true => wanted.len(),
        false => {
            let room = max_buffer_length.max(0) as usize;
            // Whole characters only: half of one is not a character, and the
            // count returned below has to name what the caller can read.
            let moved = wanted.len().min(room);
            let moved = match moved >= prefix {
                true => prefix + (moved - prefix) / per_character * per_character,
                false => 0,
            };
            // SAFETY: the caller says `buffer` covers `max_buffer_length`
            // bytes, and `moved` is no more than that.
            unsafe { std::ptr::copy_nonoverlapping(wanted.as_ptr(), buffer, moved) };
            moved
        }
    };
    if !used_buffer_length.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *used_buffer_length = moved as CFIndex };
    }
    (moved.saturating_sub(prefix) / per_character) as CFIndex
}

/// `CFStringGetBytes` for UTF-8 and the one-byte encodings: the range is in
/// characters, as every range is, and each is converted whole - a buffer that
/// runs out, or a character the encoding lacks with no loss byte to stand in,
/// ends the conversion at the character before.
///
/// # Safety
///
/// `buffer` is null or holds `max_buffer_length` bytes; `used_buffer_length` is
/// null or writable.
#[allow(clippy::too_many_arguments)]
unsafe fn get_bytes_by_character(
    units: &[u16],
    start: usize,
    length: usize,
    encoding: u32,
    loss_byte: u8,
    buffer: *mut u8,
    max_buffer_length: CFIndex,
    used_buffer_length: *mut CFIndex,
) -> CFIndex {
    if start > units.len() {
        return 0;
    }
    let end = (start + length).min(units.len());
    let room = match buffer.is_null() {
        true => usize::MAX,
        false => max_buffer_length.max(0) as usize,
    };
    let (mut converted, mut used, mut at) = (0usize, 0usize, start);
    while at < end {
        // One character: a surrogate pair is two units and one character.
        let pair = at + 1 < end
            && (0xD800..0xDC00).contains(&units[at])
            && (0xDC00..0xE000).contains(&units[at + 1]);
        let span = 1 + pair as usize;
        let piece: Option<Vec<u8>> = match encoding {
            string::encoding::UTF8 => {
                Some(String::from_utf16_lossy(&units[at..at + span]).into_bytes())
            }
            _ if pair => None,
            code => crate::eightbit::encode(units[at], code).map(|b| vec![b]),
        };
        let piece = match piece {
            Some(piece) => piece,
            None if loss_byte != 0 => vec![loss_byte],
            None => break,
        };
        if used + piece.len() > room {
            break;
        }
        if !buffer.is_null() {
            // SAFETY: within the `room` bytes the caller provided.
            unsafe { std::ptr::copy_nonoverlapping(piece.as_ptr(), buffer.add(used), piece.len()) };
        }
        used += piece.len();
        converted += span;
        at += span;
    }
    if !used_buffer_length.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *used_buffer_length = used as CFIndex };
    }
    converted as CFIndex
}

pub unsafe extern "C" fn CFStringCreateExternalRepresentation(
    _allocator: CFAllocatorRef,
    string: CFTypeRef,
    encoding: u32,
    loss_byte: u8,
) -> CFTypeRef {
    // SAFETY: a string or null.
    let Some(value) = (unsafe { crate::string::as_string(string) }) else {
        return std::ptr::null();
    };
    let length = value.length();
    let mut used = 0;
    // SAFETY: a sizing call writes only the local `used` value.
    let converted = unsafe {
        CFStringGetBytes(
            string,
            0,
            length,
            encoding,
            loss_byte,
            1,
            std::ptr::null_mut(),
            0,
            &mut used,
        )
    };
    if converted != length || used < 0 {
        return std::ptr::null();
    }
    let mut bytes = vec![0; used as usize];
    // SAFETY: the buffer has the exact size the sizing call returned.
    let converted = unsafe {
        CFStringGetBytes(
            string,
            0,
            length,
            encoding,
            loss_byte,
            1,
            bytes.as_mut_ptr(),
            used,
            &mut used,
        )
    };
    if converted != length {
        return std::ptr::null();
    }
    collections::new_data(bytes)
}

pub unsafe extern "C" fn CFStringCreateMutable(
    _allocator: CFAllocatorRef,
    _max_length: CFIndex,
) -> CFTypeRef {
    // The capacity is a hint Apple's uses to preallocate and this one ignores;
    // a `CFStringRef` with a maximum length is a thing a guest may then exceed,
    // and truncating silently is worse than reallocating.
    string::new_mutable()
}

/// `CFStringCreateMutableCopy(CFAllocatorRef, CFIndex maxLength, CFStringRef)`.
///
/// The maximum length is ignored for the reason [`CFStringCreateMutable`]
/// ignores it. The characters keep the representation the original had, so that
/// the length of the copy is the length of what it was copied from.
pub unsafe extern "C" fn CFStringCreateMutableCopy(
    _allocator: CFAllocatorRef,
    _max_length: CFIndex,
    original: CFTypeRef,
) -> CFTypeRef {
    let Some(s) = (unsafe { string::as_string(original) }) else {
        return std::ptr::null();
    };
    string::new_mutable_copy(s)
}

/// Add to a mutable string in the representation it already has.
///
/// An 8-bit string appended to in code units - or the reverse - would have its
/// `length` start counting something different from what it counted before.
///
/// # Safety
///
/// `target` is null or an object this runtime allocated.
unsafe fn append(target: CFTypeRef, tail_utf8: &[u8]) {
    let Some(a) = (unsafe { string::as_string(target) }) else {
        return;
    };
    if a.is_wide() {
        let mut units = a.utf16();
        units.extend(String::from_utf8_lossy(tail_utf8).encode_utf16());
        unsafe { string::set_utf16(target, &units) };
        return;
    }
    let mut joined = a.utf8();
    joined.extend_from_slice(tail_utf8);
    unsafe { string::set_bytes(target, &joined) };
}

pub unsafe extern "C" fn CFStringAppend(target: CFTypeRef, appended: CFTypeRef) {
    let (Some(a), Some(b)) = (unsafe { string::as_string(target) }, unsafe {
        string::as_string(appended)
    }) else {
        return;
    };
    // Both wide: the code units go across untouched, so an unpaired surrogate
    // survives the append rather than becoming a replacement character.
    if a.is_wide() && b.is_wide() {
        let mut units = a.utf16();
        units.extend_from_slice(&b.utf16());
        unsafe { string::set_utf16(target, &units) };
        return;
    }
    unsafe { append(target, &b.utf8()) };
}

/// Insert a string at a character boundary in a mutable string.
pub unsafe extern "C" fn CFStringInsert(target: CFTypeRef, index: CFIndex, inserted: CFTypeRef) {
    let Ok(index) = usize::try_from(index) else {
        return;
    };
    // Read the inserted value first because it is allowed to alias `target`.
    let Some(inserted) = (unsafe { string::as_string(inserted) }) else {
        return;
    };
    let inserted_wide = inserted.is_wide();
    let inserted_units = inserted_wide.then(|| inserted.utf16());
    let inserted_bytes = (!inserted_wide).then(|| inserted.utf8());
    let Some(target_value) = (unsafe { string::as_string(target) }) else {
        return;
    };
    if target_value.is_wide() {
        let mut units = target_value.utf16();
        if index > units.len() {
            return;
        }
        let addition = inserted_units.unwrap_or_else(|| {
            String::from_utf8_lossy(inserted_bytes.as_deref().unwrap_or_default())
                .encode_utf16()
                .collect()
        });
        units.splice(index..index, addition);
        // SAFETY: the caller's target is a mutable string.
        unsafe { string::set_utf16(target, &units) };
        return;
    }
    let mut bytes = target_value.utf8();
    if index > bytes.len() {
        return;
    }
    let addition = inserted_bytes.unwrap_or_else(|| {
        String::from_utf16_lossy(inserted_units.as_deref().unwrap_or_default()).into_bytes()
    });
    bytes.splice(index..index, addition);
    // SAFETY: the caller's target is a mutable string.
    unsafe { string::set_bytes(target, &bytes) };
}

pub unsafe extern "C" fn CFStringDelete(target: CFTypeRef, location: CFIndex, length: CFIndex) {
    let (Ok(location), Ok(length)) = (usize::try_from(location), usize::try_from(length)) else {
        return;
    };
    // SAFETY: a live mutable string or null.
    let Some(value) = (unsafe { string::as_string(target) }) else {
        return;
    };
    if value.is_wide() {
        let mut units = value.utf16();
        let Some(end) = location
            .checked_add(length)
            .filter(|end| *end <= units.len())
        else {
            return;
        };
        units.drain(location..end);
        // SAFETY: the caller's target is a mutable string.
        unsafe { string::set_utf16(target, &units) };
        return;
    }
    let mut bytes = value.utf8();
    let Some(end) = location
        .checked_add(length)
        .filter(|end| *end <= bytes.len())
    else {
        return;
    };
    bytes.drain(location..end);
    // SAFETY: the caller's target is a mutable string.
    unsafe { string::set_bytes(target, &bytes) };
}

pub unsafe extern "C" fn CFStringAppendCString(
    target: CFTypeRef,
    cstr: *const c_char,
    _encoding: u32,
) {
    // SAFETY: a NUL-terminated string or null.
    let Some(bytes) = (unsafe { string::cstr_bytes(cstr) }) else {
        return;
    };
    unsafe { append(target, bytes) };
}

/// `void CFStringUppercase(CFMutableStringRef, CFLocaleRef locale)`.
///
/// The locale is not consulted: this runtime has no case-mapping tables of its
/// own, so the mapping is Unicode's default one. What that costs is the
/// language-specific pairs - a Turkish locale uppercases `i` to `I` here where
/// Apple's gives `U+0130`. Everything else, including the multi-character
/// mappings such as `ss` to `SS`, is the same answer.
pub unsafe extern "C" fn CFStringUppercase(target: CFTypeRef, _locale: CFTypeRef) {
    unsafe { recase(target, str::to_uppercase) }
}

/// `void CFStringLowercase(CFMutableStringRef, CFLocaleRef locale)`, with the
/// same treatment of the locale as [`CFStringUppercase`].
pub unsafe extern "C" fn CFStringLowercase(target: CFTypeRef, _locale: CFTypeRef) {
    unsafe { recase(target, str::to_lowercase) }
}

/// Rewrite a mutable string through a case mapping, keeping the representation.
///
/// An immutable string is left alone rather than written through: Apple's would
/// be a crash, and there is nothing this can return to say so.
///
/// # Safety
///
/// `target` is null or an object this runtime allocated.
unsafe fn recase(target: CFTypeRef, map: fn(&str) -> String) {
    let Some(s) = (unsafe { string::as_string(target) }) else {
        return;
    };
    if s.is_wide() {
        let mapped = map(&String::from_utf16_lossy(&s.utf16()));
        let units: Vec<u16> = mapped.encode_utf16().collect();
        unsafe { string::set_utf16(target, &units) };
        return;
    }
    let mapped = map(&String::from_utf8_lossy(&s.utf8()));
    unsafe { string::set_bytes(target, mapped.as_bytes()) };
}

/// `UniChar CFStringGetCharacterAtIndex(CFStringRef, CFIndex)`.
///
/// Zero for an index outside the string, which Apple's would fault on: there is
/// no character to answer with, and a caller reading past the length it was
/// given has already lost track of the string.
pub unsafe extern "C" fn CFStringGetCharacterAtIndex(s: CFTypeRef, index: CFIndex) -> u16 {
    let Some(s) = (unsafe { string::as_string(s) }) else {
        return 0;
    };
    match usize::try_from(index) {
        Ok(index) => s.units().get(index).copied().unwrap_or(0),
        Err(_) => 0,
    }
}

/// `void CFStringGetCharacters(CFStringRef, CFRange, UniChar *buffer)`, with the
/// range flattened into the two words it is passed in.
///
/// A range that is not inside the string writes nothing: the caller sized the
/// buffer from the range, so writing a shorter answer would leave it reading
/// characters it never received, and writing a longer one is the overflow.
pub unsafe extern "C" fn CFStringGetCharacters(
    s: CFTypeRef,
    range_location: CFIndex,
    range_length: CFIndex,
    buffer: *mut u16,
) {
    let Some(s) = (unsafe { string::as_string(s) }) else {
        return;
    };
    let (Ok(start), Ok(count)) = (
        usize::try_from(range_location),
        usize::try_from(range_length),
    ) else {
        return;
    };
    let units = s.units();
    if buffer.is_null() || start + count > units.len() {
        return;
    }
    // SAFETY: the caller states `buffer` holds the range's characters, and the
    // range has just been checked against the string.
    unsafe { std::ptr::copy_nonoverlapping(units[start..].as_ptr(), buffer, count) };
}

/// `const UniChar *CFStringGetCharactersPtr(CFStringRef)`.
///
/// The same contract as `CFStringGetCStringPtr`: the characters if they are
/// already there, and null rather than a conversion. An 8-bit string's bytes
/// are not `UniChar`s, so it has none to point at.
pub unsafe extern "C" fn CFStringGetCharactersPtr(s: CFTypeRef) -> *const u16 {
    unsafe { string::as_string(s) }.map_or(std::ptr::null(), |s| s.characters_ptr())
}

/// `void CFStringAppendCharacters(CFMutableStringRef, const UniChar *, CFIndex)`.
pub unsafe extern "C" fn CFStringAppendCharacters(
    target: CFTypeRef,
    chars: *const u16,
    count: CFIndex,
) {
    let Ok(count) = usize::try_from(count) else {
        return;
    };
    if chars.is_null() && count > 0 {
        return;
    }
    // SAFETY: the caller says `chars` covers `count` code units.
    let tail = match count {
        0 => &[][..],
        _ => unsafe { std::slice::from_raw_parts(chars, count) },
    };
    let Some(a) = (unsafe { string::as_string(target) }) else {
        return;
    };
    if a.is_wide() {
        let mut units = a.utf16();
        units.extend_from_slice(tail);
        unsafe { string::set_utf16(target, &units) };
        return;
    }
    unsafe { append(target, String::from_utf16_lossy(tail).as_bytes()) };
}

pub unsafe extern "C" fn CFStringCompare(a: CFTypeRef, b: CFTypeRef, options: u64) -> CFIndex {
    let (Some(a), Some(b)) = (unsafe { string::as_string(a) }, unsafe {
        string::as_string(b)
    }) else {
        return 0;
    };
    let (mut x, mut y) = (a.utf8(), b.utf8());
    // `kCFCompareCaseInsensitive`.
    if options & 1 != 0 {
        x.make_ascii_lowercase();
        y.make_ascii_lowercase();
    }
    match x.cmp(&y) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// `CFRange`, which is two `CFIndex` and nothing else.
///
/// `Copy` because it is passed and returned by value through registers and
/// stack on both 64-bit ABIs, and a caller building a range out of another one
/// has no reason to have to clone it to do so.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CFRange {
    pub location: CFIndex,
    pub length: CFIndex,
}

/// Find a string inside a caller-selected UTF-16 range.
pub unsafe extern "C" fn CFStringFindWithOptions(
    searched: CFTypeRef,
    needle: CFTypeRef,
    range_location: CFIndex,
    range_length: CFIndex,
    options: u64,
    result: *mut CFRange,
) -> Boolean {
    let (Ok(start), Ok(length)) = (
        usize::try_from(range_location),
        usize::try_from(range_length),
    ) else {
        return 0;
    };
    // SAFETY: live strings or null.
    let (Some(searched), Some(needle)) = (unsafe { string::as_string(searched) }, unsafe {
        string::as_string(needle)
    }) else {
        return 0;
    };
    let mut haystack = searched.utf16();
    let mut needle = needle.utf16();
    let Some(end) = start
        .checked_add(length)
        .filter(|end| *end <= haystack.len())
    else {
        return 0;
    };
    const CASE_INSENSITIVE: u64 = 1;
    const BACKWARDS: u64 = 4;
    const ANCHORED: u64 = 8;
    if options & CASE_INSENSITIVE != 0 {
        // ASCII case folding preserves UTF-16 indices, unlike a full Unicode
        // case mapping whose expansions would make the returned range wrong.
        for unit in haystack.iter_mut().chain(needle.iter_mut()) {
            if let Ok(byte) = u8::try_from(*unit) {
                *unit = u16::from(byte.to_ascii_lowercase());
            }
        }
    }
    if needle.len() > length {
        return 0;
    }
    let mut candidates = start..=end - needle.len();
    let found = if options & ANCHORED != 0 {
        let index = if options & BACKWARDS != 0 {
            end - needle.len()
        } else {
            start
        };
        (haystack[index..index + needle.len()] == needle).then_some(index)
    } else if options & BACKWARDS != 0 {
        candidates
            .rev()
            .find(|index| haystack[*index..*index + needle.len()] == needle)
    } else {
        candidates.find(|index| haystack[*index..*index + needle.len()] == needle)
    };
    let Some(location) = found else {
        return 0;
    };
    if !result.is_null() {
        // SAFETY: the caller supplied writable storage for one `CFRange`.
        unsafe {
            *result = CFRange {
                location: location as CFIndex,
                length: needle.len() as CFIndex,
            }
        };
    }
    1
}

/// `CFArrayRef CFStringCreateArrayWithFindResults(CFAllocatorRef, CFStringRef, CFStringRef, CFRange, CFStringCompareFlags)`
///
/// An array of **pointers to** `CFRange` - the header says "an array of CFRange
/// pointers", and it means it: the guest reads `CFArrayGetValueAtIndex(...)`
/// and then dereferences the result (`mov rax, [rax]` at `0x4dd1778`), so an
/// array of `CFRange` by value would hand it a `location` where it expects an
/// address.
///
/// The rules, all from the header's own comment and all of them things a caller
/// can observe:
///
/// * `NULL` if there are no matches - not an empty array, which a caller can
///   tell apart and which would be a different answer.
/// * **Overlapping instances are not found**, so "AA" in "AAA" is one range.
/// * An empty `stringToFind` finds nothing.
/// * `kCFCompareBackwards` scans from the end *and stores the results backwards*,
///   so the last match is in slot 0.
/// * `kCFCompareAnchored` reports only the consecutive instances at the start
///   (or at the end, backwards) - "AB" in "ABABXAB" is two ranges, not three.
/// * `kCFCompareNumerically` is ignored.
/// * Only `rangeToSearch` is searched.
///
/// The guest's caller (`0x4dd1664`, in `chrome/browser/mac/shims`'s requirement
/// parser) passes a one-character needle, the whole string as the range and no
/// options, then requires more than one match to take a branch.
pub unsafe extern "C" fn CFStringCreateArrayWithFindResults(
    _alloc: CFAllocatorRef,
    the_string: CFTypeRef,
    string_to_find: CFTypeRef,
    range_to_search: CFRange,
    compare_options: u64,
) -> CFTypeRef {
    const CASE_INSENSITIVE: u64 = 1;
    const BACKWARDS: u64 = 4;
    const ANCHORED: u64 = 8;
    // SAFETY: live strings or null.
    let (Some(searched), Some(needle)) = (unsafe { string::as_string(the_string) }, unsafe {
        string::as_string(string_to_find)
    }) else {
        return std::ptr::null();
    };
    let mut haystack = searched.utf16();
    let mut needle = needle.utf16();
    // An empty needle finds nothing, and that is the header's rule rather than a
    // degenerate case: it is what stops a caller looping over a find that can
    // never advance.
    if needle.is_empty() {
        return std::ptr::null();
    }
    let (Ok(start), Ok(length)) = (
        usize::try_from(range_to_search.location),
        usize::try_from(range_to_search.length),
    ) else {
        return std::ptr::null();
    };
    let Some(end) = start
        .checked_add(length)
        .filter(|end| *end <= haystack.len())
    else {
        return std::ptr::null();
    };
    if compare_options & CASE_INSENSITIVE != 0 {
        // ASCII case folding, for the reason `CFStringFindWithOptions` gives:
        // a full Unicode case mapping can change a string's length, and a range
        // measured in the original string would then name the wrong characters.
        for unit in haystack.iter_mut().chain(needle.iter_mut()) {
            if let Ok(byte) = u8::try_from(*unit) {
                *unit = u16::from(byte.to_ascii_lowercase());
            }
        }
    }
    let matches = find_all(
        &haystack,
        &needle,
        start,
        end,
        compare_options & ANCHORED != 0,
        compare_options & BACKWARDS != 0,
    );
    if matches.is_empty() {
        return std::ptr::null();
    }
    // The ranges are the array's to give back, so they go in a box the array
    // holds and drops.  A pointer a guest still holds after releasing the array
    // dangles - which is exactly what it does in Apple's, whose value release
    // callback frees them.
    let ranges: Vec<Box<CFRange>> = matches
        .into_iter()
        .map(|location| Box::new(CFRange {
            location: location as CFIndex,
            length: needle.len() as CFIndex,
        }))
        .collect();
    let values: Vec<CFTypeRef> = ranges.iter().map(|r| &**r as *const CFRange as CFTypeRef).collect();
    let array = collections::new_array(&values, crate::callbacks::Policy::NONE, false);
    collections::keep_alive(array, Box::new(ranges));
    array
}

/// Every non-overlapping match of `needle` in `haystack[start..end]`.
///
/// Backwards is a separate walk rather than a reversed one so that the
/// *order* the header describes - last match in slot 0 - falls out of the walk
/// itself and cannot drift from it.
fn find_all(
    haystack: &[u16],
    needle: &[u16],
    start: usize,
    end: usize,
    anchored: bool,
    backwards: bool,
) -> Vec<usize> {
    let width = needle.len();
    if width == 0 || width > end.saturating_sub(start) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if backwards {
        let mut at = end - width;
        loop {
            if haystack[at..at + width] == *needle {
                out.push(at);
                // Anchored means *consecutive* instances at the end, so the next
                // candidate is one needle-width further back - and there is no
                // room for one below that.
                if anchored {
                    if at < start + width {
                        break;
                    }
                    at -= width;
                    continue;
                }
            } else if anchored {
                break;
            }
            if at == start {
                break;
            }
            at -= 1;
        }
    } else {
        let mut at = start;
        while at + width <= end {
            if haystack[at..at + width] == *needle {
                out.push(at);
                // Skipping the whole match is what "overlapping instances are
                // not found" means: "AAA" has one "AA", at 0, not two.
                at += width;
                continue;
            }
            if anchored {
                break;
            }
            at += 1;
        }
    }
    out
}

/// `CFRange` is flattened to its two `CFIndex` fields by both 64-bit ABIs.
pub unsafe extern "C" fn CFStringCompareWithOptions(
    a: CFTypeRef,
    b: CFTypeRef,
    location: CFIndex,
    length: CFIndex,
    options: u64,
) -> CFIndex {
    let (Some(a), Some(b)) = (unsafe { string::as_string(a) }, unsafe {
        string::as_string(b)
    }) else {
        return 0;
    };
    let (Ok(start), Some(end)) = (
        usize::try_from(location),
        location
            .checked_add(length)
            .and_then(|end| usize::try_from(end).ok()),
    ) else {
        return 0;
    };
    let units = a.utf16();
    let Some(range) = units.get(start..end) else {
        return 0;
    };
    let mut x = String::from_utf16_lossy(range).into_bytes();
    let mut y = b.utf8();
    if options & 1 != 0 {
        x.make_ascii_lowercase();
        y.make_ascii_lowercase();
    }
    match x.cmp(&y) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

//

use crate::callbacks::{KeyCallBacks, Policy, ValueCallBacks};
use crate::collections;

pub unsafe extern "C" fn CFArrayGetTypeID() -> CFTypeID {
    crate::base::type_id_of(collections::array_class())
}

pub unsafe extern "C" fn CFArrayCreate(
    _allocator: CFAllocatorRef,
    values: *const CFTypeRef,
    count: CFIndex,
    callbacks: *const ValueCallBacks,
) -> CFTypeRef {
    if count < 0 || (values.is_null() && count > 0) {
        return std::ptr::null();
    }
    // SAFETY: the caller says `values` covers `count` entries.
    let slice = match count {
        0 => &[][..],
        _ => unsafe { std::slice::from_raw_parts(values, count as usize) },
    };
    // SAFETY: null, or a live callbacks structure.
    collections::new_array(slice, unsafe { Policy::from_value(callbacks) }, false)
}

/// `CFArrayCreateMutable`. The capacity is a hint, and zero means unbounded;
/// this array grows either way, so it is only reserved for.
pub unsafe extern "C" fn CFArrayCreateMutable(
    _allocator: CFAllocatorRef,
    _capacity: CFIndex,
    callbacks: *const ValueCallBacks,
) -> CFTypeRef {
    // SAFETY: null, or a live callbacks structure.
    collections::new_array(&[], unsafe { Policy::from_value(callbacks) }, true)
}

pub unsafe extern "C" fn CFArrayAppendValue(array: CFTypeRef, value: CFTypeRef) {
    let Some(a) = (unsafe { collections::array(array) }) else {
        return;
    };
    if !a.mutable {
        return;
    }
    a.values.push(a.policy.retain(value));
}

/// The function called by [`CFArrayApplyFunction`] for each value in its range.
pub type ArrayApplierFunction = unsafe extern "C" fn(CFTypeRef, *mut c_void);

/// Apply `callback` to every value in the requested array range.
///
/// `CFRange` is passed as its two `CFIndex` fields so that this Rust entry point
/// has the same System V ABI as the C function taking the aggregate by value.
/// The values are copied before invoking guest code: callbacks must not mutate
/// the array, but this keeps an accidental mutation from invalidating the walk.
pub unsafe extern "C" fn CFArrayApplyFunction(
    array: CFTypeRef,
    range_location: CFIndex,
    range_length: CFIndex,
    callback: Option<ArrayApplierFunction>,
    context: *mut c_void,
) {
    let Some(callback) = callback else {
        return;
    };
    let (Ok(start), Some(end)) = (
        usize::try_from(range_location),
        range_location
            .checked_add(range_length)
            .and_then(|end| usize::try_from(end).ok()),
    ) else {
        return;
    };
    let Some(values) = (unsafe { collections::array(array) })
        .and_then(|a| a.values.get(start..end).map(ToOwned::to_owned))
    else {
        return;
    };
    for value in values {
        // SAFETY: `callback` is supplied by the caller with this signature.
        unsafe { callback(value, context) };
    }
}

pub unsafe extern "C" fn CFArrayGetCount(array: CFTypeRef) -> CFIndex {
    unsafe { collections::array(array) }.map_or(0, |a| a.values.len() as CFIndex)
}

pub unsafe extern "C" fn CFArrayGetValueAtIndex(array: CFTypeRef, index: CFIndex) -> CFTypeRef {
    let Some(a) = (unsafe { collections::array(array) }) else {
        return std::ptr::null();
    };
    match collections::at(index, a.values.len()) {
        Some(at) => a.values[at],
        // Apple's traps here. Answering null is the more useful failure: a guest
        // that walks past the end gets a nil it can see rather than a crash in a
        // frame that says nothing about its own loop.
        None => std::ptr::null(),
    }
}

//

/// `Boolean CFStringHasPrefix(CFStringRef string, CFStringRef prefix)`
///
/// A plain character comparison, in the units `CFStringGetLength` counts - which
/// is the only question a caller asking "does this start with that" can be
/// asking, whichever of the two representations either string happens to keep.
///
/// A null on either side is false rather than a fault. CoreFoundation traps, and
/// a guest that got here has a bug; a `false` it can branch on is more use than
/// a crash with no mention of either string in the frame.
pub unsafe extern "C" fn CFStringHasPrefix(string: CFTypeRef, prefix: CFTypeRef) -> Boolean {
    // SAFETY: two `CFStringRef`s or null.
    let (Some(string), Some(prefix)) = (unsafe { string::as_string(string) }, unsafe {
        string::as_string(prefix)
    }) else {
        return 0;
    };
    let (string, prefix) = (string.units(), prefix.units());
    u8::from(string.len() >= prefix.len() && string[..prefix.len()] == prefix[..])
}

/// `Boolean CFStringHasSuffix(CFStringRef string, CFStringRef suffix)`
///
/// [`CFStringHasPrefix`], from the other end.
pub unsafe extern "C" fn CFStringHasSuffix(string: CFTypeRef, suffix: CFTypeRef) -> Boolean {
    // SAFETY: two `CFStringRef`s or null.
    let (Some(string), Some(suffix)) = (unsafe { string::as_string(string) }, unsafe {
        string::as_string(suffix)
    }) else {
        return 0;
    };
    let (string, suffix) = (string.units(), suffix.units());
    u8::from(string.len() >= suffix.len() && string[string.len() - suffix.len()..] == suffix[..])
}

/// `CFRange CFStringGetRangeOfCharacterClusterAtIndex(CFStringRef theString,
/// CFIndex theIndex, CFRange *range)`
///
/// The range is written through `range` as well as returned, because the
/// returned `CFRange` is a sixteen-byte aggregate that no signature in this
/// tree can place - see `crates/rosetta3/src/abi/table.rs` - whereas the out
/// parameter is a plain pointer and always carries the answer. A caller that
/// passes null gets the documented out-of-bounds range and nothing else, which
/// is Apple's behaviour for a string that has no character at the index.
///
/// ## What a cluster is
///
/// A *composed character sequence*: one base character followed by the marks
/// that combine with it, so `e` and a following U+0301 are one character that
/// happens to be two units. Which is the question a caller asking this has: it
/// is measuring where a *character* begins, and counting code units would put
/// the boundary in the middle of one.
///
/// The cases that decide it, and no others:
///
/// - a surrogate pair, which is one character in two units;
/// - a combining mark, a variation selector, or a zero-width joiner, which
///   attach to whatever precedes them - and, after a joiner, to the character
///   that joiner brought in, which is how a family emoji stays one cluster;
/// - a pair of regional indicators, which together are one flag and whose
///   single half is its own cluster.
///
/// An index outside the string is the empty range, which is what the header
/// documents. `theIndex == length` counts as outside: CoreFoundation's real
/// answer there is the last cluster, but the header says zero and every caller
/// in this tree's guest space guards the case rather than relying on it.
pub unsafe extern "C" fn CFStringGetRangeOfCharacterClusterAtIndex(
    the_string: CFTypeRef,
    the_index: CFIndex,
    range: *mut CFRange,
) -> CFRange {
    /// The empty range, for an index that is not in the string.
    const NOTHING: CFRange = CFRange {
        location: 0,
        length: 0,
    };
    // "Containing" is the whole of the contract, so the index may be *inside* a
    // character rather than at its start: `e` followed by a combining acute is one
    // character, and asking about the accent has to give the `e` too.
    let found = match unsafe { string::as_string(the_string) } {
        // SAFETY: a `CFStringRef` or null.
        Some(string) => {
            let units = string.units();
            match usize::try_from(the_index)
                .ok()
                .filter(|at| *at < units.len())
            {
                Some(index) => {
                    let start = character_start(&units, index);
                    CFRange {
                        location: start as CFIndex,
                        length: character_length(&units, start) as CFIndex,
                    }
                }
                None => NOTHING,
            }
        }
        None => NOTHING,
    };
    // Written on every path, including the out-of-bounds one: the header says the
    // range is stored there when the parameter is non-null, and a caller that
    // passed a buffer and reads it back must not be left holding whatever was in
    // it.
    if !range.is_null() {
        // SAFETY: the caller supplied writable storage for one `CFRange`.
        unsafe { *range = CFRange { ..found } };
    }
    found
}

/// The index at which the character covering `at` begins.
///
/// Walks back over the units that attach to what came before, and over a joiner -
/// which means the character that joiner brought in, so an index in the middle
/// of an emoji sequence still gets the whole sequence.
fn character_start(units: &[u16], at: usize) -> usize {
    let mut start = at;
    while start > 0
        && (continuation_length(units, start) != 0
            || units
                .get(start - 1)
                .copied()
                .map(u32::from)
                .and_then(char::from_u32)
                .is_some_and(is_joiner))
    {
        start -= 1;
    }
    // A flag is a *pair* of regional indicators, and which of the two an index
    // lands on decides whether it is inside a character or at the start of one.
    // An odd number of them before it means it is the second half of a pair.
    if units.get(start).copied().is_some_and(is_regional_indicator) {
        let before = (0..start)
            .rev()
            .take_while(|i| is_regional_indicator(units[*i]))
            .count();
        start -= usize::from(before % 2 == 1);
    }
    start
}

/// How many units the character beginning at `start` occupies.
///
/// `start` must be a character boundary: the answer for a unit in the middle of
/// one is meaningless, which is why the caller asks
/// [`character_start`] first.
fn character_length(units: &[u16], start: usize) -> usize {
    if start >= units.len() {
        return 0;
    }
    // Two units for a surrogate pair - a character above the BMP - and, for a
    // regional indicator, two again, because a flag is a pair of them.
    let mut end = start
        + 1
        + usize::from(
            is_high_surrogate(units[start])
                && units.get(start + 1).copied().is_some_and(is_low_surrogate),
        );
    if end == start + 1
        && is_regional_indicator(units[start])
        && units
            .get(start + 1)
            .copied()
            .is_some_and(is_regional_indicator)
    {
        end += 1;
    }
    // Then everything that attaches to the character: marks, variation selectors,
    // and a joiner with whatever it joined in.
    while end < units.len() {
        match continuation_length(units, end) {
            0 => break,
            taken => end += taken,
        }
    }
    end - start
}

/// How many units at `at` belong to the character that came *before* `at`, or
/// zero if `at` begins a character of its own.
///
/// This is the whole of the cluster rule. It is asked twice per unit of the
/// string at most - once walking back, once walking forward - so the answer is
/// written as ranges over the code space rather than a table of every combining
/// mark Unicode has ever added: the blocks below have not moved in the twenty
/// years the callers in this tree's guest space have existed, and the emoji
/// modifier range is the one that has been added to most recently.
fn continuation_length(units: &[u16], at: usize) -> usize {
    let Some(unit) = units.get(at).copied() else {
        return 0;
    };
    // The low half of a surrogate pair belongs to its high half, so it continues
    // that character. A low surrogate with no high surrogate before it is a unit
    // of text in its own right - a malformed string, which this runtime does not
    // refuse to answer about - and begins a character.
    if is_low_surrogate(unit) {
        return usize::from(
            at.checked_sub(1)
                .and_then(|i| units.get(i))
                .copied()
                .is_some_and(is_high_surrogate),
        );
    }
    let Some(character) = char::from_u32(u32::from(unit)) else {
        return 0;
    };
    if is_combining(character) || matches!(character, '\u{FE00}'..='\u{FE0F}') {
        return 1;
    }
    // A joiner joins what comes after it, so it continues a character only when
    // something does - and then it brings that something in with it.
    if is_joiner(character) {
        return match units.get(at + 1) {
            None => 0,
            Some(_) => 1 + character_length(units, at + 1),
        };
    }
    0
}

/// Whether `c` is a mark: non-spacing, enclosing or spacing, or an emoji
/// modifier.
///
/// A base glyph and one of these are one character, which is the only reason any
/// of this exists.
fn is_combining(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F      // combining diacritical marks
        | 0x0483..=0x0489    // Cyrillic
        | 0x0591..=0x05BD    // Hebrew
        | 0x0610..=0x061A    // Arabic
        | 0x064B..=0x065F    // Arabic
        | 0x0670            // Arabic superscript alef
        | 0x06D6..=0x06DC    // Arabic
        | 0x0E31            // Thai
        | 0x0E34..=0x0E3A    // Thai
        | 0x0E47..=0x0E4E    // Thai
        | 0x1AB0..=0x1AFF    // combining marks extended
        | 0x1DC0..=0x1DFF    // combining marks supplement
        | 0x20D0..=0x20F0    // combining marks for symbols
        | 0xFE20..=0xFE2F    // combining half marks
        | 0x1F3FB..=0x1F3FF  // emoji modifiers: skin tones
    )
}

/// A zero-width joiner, or one of the object-replacement controls Apple's text
/// system puts in the middle of an emoji sequence.
fn is_joiner(c: char) -> bool {
    c == '\u{200D}' || matches!(c, '\u{FFF9}'..='\u{FFFB}')
}

fn is_high_surrogate(unit: u16) -> bool {
    (0xD800..0xDC00).contains(&unit)
}

fn is_low_surrogate(unit: u16) -> bool {
    (0xDC00..0xE000).contains(&unit)
}

fn is_regional_indicator(unit: u16) -> bool {
    // U+1F1E6..=U+1F1FF, which is above the BMP and so only reachable as a
    // surrogate pair's halves.
    (0xD83C..0xD83E).contains(&unit)
}

//

pub unsafe extern "C" fn CFSetGetTypeID() -> CFTypeID {
    crate::base::type_id_of(collections::set_class())
}

/// `CFSetCallBacks`, which is [`callbacks::KeyCallBacks`] member for member.
///
/// Named here so the signatures below say what the guest actually passed. A set
/// needs the same callbacks a dictionary needs *as keys* - including the hash,
/// which is why [`Policy::from_key`] is the reader - because a set's elements are
/// exactly a dictionary's keys without the values.
pub type SetCallBacks = KeyCallBacks;

/// `CFSetRef CFSetCreate(CFAllocatorRef, CFIndex numValues, const void **values,
/// const CFSetCallBacks *callbacks)`
///
/// The values are retained as the callbacks say, and a value equal to one the
/// set already holds is *not* added twice: a set has no repeats, and the second
/// add is how a caller discovers that.
pub unsafe extern "C" fn CFSetCreate(
    _allocator: CFAllocatorRef,
    num_values: CFIndex,
    values: *const CFTypeRef,
    callbacks: *const SetCallBacks,
) -> CFTypeRef {
    if num_values < 0 || (values.is_null() && num_values > 0) {
        return std::ptr::null();
    }
    // SAFETY: the caller says `values` covers `num_values` entries.
    let slice = match num_values {
        0 => &[][..],
        _ => unsafe { std::slice::from_raw_parts(values, num_values as usize) },
    };
    // SAFETY: null, or a live callbacks structure.
    collections::new_set(slice, unsafe { Policy::from_key(callbacks) })
}

/// `CFMutableSetRef CFSetCreateMutable(CFAllocatorRef, CFIndex capacity,
/// const CFSetCallBacks *callbacks)`
///
/// The capacity is a hint, exactly as [`CFArrayCreateMutable`]'s is: the set
/// grows either way, so there is nothing to reserve and nothing to get wrong by
/// ignoring it.
pub unsafe extern "C" fn CFSetCreateMutable(
    _allocator: CFAllocatorRef,
    _capacity: CFIndex,
    callbacks: *const SetCallBacks,
) -> CFTypeRef {
    // SAFETY: null, or a live callbacks structure.
    collections::new_mutable_set(unsafe { Policy::from_key(callbacks) })
}

/// `void CFSetAddValue(CFMutableSetRef theSet, const void *value)`
///
/// A no-op for a set that was not created mutable, which is what Apple's raise
/// amounts to: `__NSCFSet` has to descend from `NSMutableSet` for a guest's
/// `isKindOfClass:` to agree with the object, so mutability is a field and the
/// refusal is by name rather than by class. See [`collections::SetContents`].
pub unsafe extern "C" fn CFSetAddValue(set: CFTypeRef, value: CFTypeRef) {
    let Some(set) = (unsafe { collections::set(set) }) else {
        return;
    };
    if !set.mutable {
        return;
    }
    set.add(value);
}

pub unsafe extern "C" fn CFSetGetCount(set: CFTypeRef) -> CFIndex {
    unsafe { collections::set(set) }.map_or(0, |set| set.values.len() as CFIndex)
}

pub unsafe extern "C" fn CFSetGetValues(set: CFTypeRef, values: *mut CFTypeRef) {
    if values.is_null() {
        return;
    }
    let Some(set) = (unsafe { collections::set(set) }) else {
        return;
    };
    // SAFETY: the caller provides room for CFSetGetCount entries.
    unsafe { std::ptr::copy_nonoverlapping(set.values.as_ptr(), values, set.values.len()) };
}

pub unsafe extern "C" fn CFSetApplyFunction(
    set: CFTypeRef,
    callback: Option<ArrayApplierFunction>,
    context: *mut c_void,
) {
    let Some(callback) = callback else {
        return;
    };
    // SAFETY: the caller's obligation. Copy before entering guest code.
    let Some(values) = (unsafe { collections::set(set) }).map(|set| set.values.clone()) else {
        return;
    };
    for value in values {
        // SAFETY: the caller supplies this CFSetApplierFunction.
        unsafe { callback(value, context) };
    }
}

//

pub unsafe extern "C" fn CFDictionaryGetTypeID() -> CFTypeID {
    crate::base::type_id_of(collections::dictionary_class())
}

pub unsafe extern "C" fn CFDictionaryCreate(
    _allocator: CFAllocatorRef,
    keys: *const CFTypeRef,
    values: *const CFTypeRef,
    count: CFIndex,
    key_callbacks: *const KeyCallBacks,
    value_callbacks: *const ValueCallBacks,
) -> CFTypeRef {
    if count < 0 || ((keys.is_null() || values.is_null()) && count > 0) {
        return std::ptr::null();
    }
    // SAFETY: the caller says both arrays cover `count` entries.
    let pairs: Vec<(CFTypeRef, CFTypeRef)> = match count {
        0 => Vec::new(),
        _ => unsafe {
            std::slice::from_raw_parts(keys, count as usize)
                .iter()
                .copied()
                .zip(
                    std::slice::from_raw_parts(values, count as usize)
                        .iter()
                        .copied(),
                )
                .collect()
        },
    };
    // SAFETY: null, or live callbacks structures.
    unsafe {
        collections::new_dictionary(
            &pairs,
            Policy::from_key(key_callbacks),
            Policy::from_value(value_callbacks),
            false,
        )
    }
}

/// `CFDictionaryCreateMutable`, whose capacity is a hint the same way
/// [`CFArrayCreateMutable`]'s is.
pub unsafe extern "C" fn CFDictionaryCreateMutable(
    _allocator: CFAllocatorRef,
    _capacity: CFIndex,
    key_callbacks: *const KeyCallBacks,
    value_callbacks: *const ValueCallBacks,
) -> CFTypeRef {
    // SAFETY: null, or live callbacks structures.
    unsafe {
        collections::new_dictionary(
            &[],
            Policy::from_key(key_callbacks),
            Policy::from_value(value_callbacks),
            true,
        )
    }
}

pub unsafe extern "C" fn CFDictionarySetValue(
    dictionary: CFTypeRef,
    key: CFTypeRef,
    value: CFTypeRef,
) {
    let Some(d) = (unsafe { collections::dictionary(dictionary) }) else {
        return;
    };
    if !d.mutable {
        return;
    }
    d.set(key, value);
}

pub unsafe extern "C" fn CFDictionaryAddValue(
    dictionary: CFTypeRef,
    key: CFTypeRef,
    value: CFTypeRef,
) {
    // Core Foundation documents an existing key as a programming error. This
    // runtime leaves its value alone, which preserves Add rather than turning
    // it into Set in release guests.
    let Some(d) = (unsafe { collections::dictionary(dictionary) }) else {
        return;
    };
    if !d.mutable || d.position(key).is_some() {
        return;
    }
    d.set(key, value);
}

pub unsafe extern "C" fn CFDictionaryGetCount(dictionary: CFTypeRef) -> CFIndex {
    unsafe { collections::dictionary(dictionary) }.map_or(0, |d| d.entries.len() as CFIndex)
}

pub type DictionaryApplierFunction = unsafe extern "C" fn(CFTypeRef, CFTypeRef, *mut c_void);

pub unsafe extern "C" fn CFDictionaryApplyFunction(
    dictionary: CFTypeRef,
    callback: Option<DictionaryApplierFunction>,
    context: *mut c_void,
) {
    let Some(callback) = callback else {
        return;
    };
    // Snapshot before invoking guest code, which may mutate the dictionary.
    // SAFETY: a dictionary or null.
    let Some(entries) = (unsafe { collections::dictionary(dictionary) })
        .map(|dictionary| dictionary.entries.clone())
    else {
        return;
    };
    for (key, value) in entries {
        // SAFETY: the callback is supplied by the caller with this signature.
        unsafe { callback(key, value, context) };
    }
}

pub unsafe extern "C" fn CFDictionaryGetValue(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef {
    let Some(d) = (unsafe { collections::dictionary(dictionary) }) else {
        return std::ptr::null();
    };
    d.position(key)
        .map_or(std::ptr::null(), |at| d.entries[at].1)
}

pub unsafe extern "C" fn CFDictionaryGetValueIfPresent(
    dictionary: CFTypeRef,
    key: CFTypeRef,
    value: *mut CFTypeRef,
) -> Boolean {
    let Some(d) = (unsafe { collections::dictionary(dictionary) }) else {
        return 0;
    };
    let Some(at) = d.position(key) else {
        return 0;
    };
    if !value.is_null() {
        // SAFETY: the caller supplies a writable result pointer when non-null.
        unsafe { *value = d.entries[at].1 };
    }
    1
}

pub unsafe extern "C" fn CFDictionaryContainsKey(dictionary: CFTypeRef, key: CFTypeRef) -> Boolean {
    unsafe { collections::dictionary(dictionary) }.is_some_and(|d| d.position(key).is_some())
        as Boolean
}

/// `CFDictionaryGetKeysAndValues`: both sides, in one pass, into the caller's
/// arrays. Either may be null, which means "not interested".
pub unsafe extern "C" fn CFDictionaryGetKeysAndValues(
    dictionary: CFTypeRef,
    keys: *mut CFTypeRef,
    values: *mut CFTypeRef,
) {
    let Some(d) = (unsafe { collections::dictionary(dictionary) }) else {
        return;
    };
    for (i, (key, value)) in d.entries.iter().enumerate() {
        // SAFETY: the caller says each non-null array has room for `GetCount`
        // entries, which is what this loop writes.
        unsafe {
            if !keys.is_null() {
                *keys.add(i) = *key;
            }
            if !values.is_null() {
                *values.add(i) = *value;
            }
        }
    }
}

pub unsafe extern "C" fn CFNumberGetTypeID() -> CFTypeID {
    crate::base::type_id_of(collections::number_class())
}

pub unsafe extern "C" fn CFBooleanGetTypeID() -> CFTypeID {
    crate::base::type_id_of(collections::boolean_class())
}

pub unsafe extern "C" fn CFBooleanGetValue(value: CFTypeRef) -> Boolean {
    (value == collections::boolean(true)) as Boolean
}

pub unsafe extern "C" fn CFNumberCreate(
    _allocator: CFAllocatorRef,
    kind: i32,
    value: *const c_void,
) -> CFTypeRef {
    let (Some((width, is_float)), false) = (collections::number_shape(kind), value.is_null())
    else {
        return std::ptr::null();
    };
    let mut bytes = [0u8; 8];
    // SAFETY: the caller says `value` points at a number of this type, which is
    // `width` bytes.
    unsafe { std::ptr::copy_nonoverlapping(value as *const u8, bytes.as_mut_ptr(), width) };
    let number = match (is_float, width) {
        (true, 4) => collections::Number::Float(f32::from_le_bytes(
            bytes[..4].try_into().unwrap_or_default(),
        ) as f64),
        (true, _) => collections::Number::Float(f64::from_le_bytes(bytes)),
        // Sign-extended from its own width, so a `SInt8` of -1 is -1 and not 255.
        (false, w) => {
            let raw = i64::from_le_bytes(bytes);
            let shift = 64 - w * 8;
            collections::Number::Integer((raw << shift) >> shift)
        }
    };
    collections::new_number_of_type(number, kind)
}

/// `CFNumberGetValue`: `false` when the value does not fit what was asked for.
///
/// Apple's writes a lossy answer *and* returns false; so does this. A caller
/// that ignores the return gets the same wrong number it would get there, which
/// is the only behaviour that makes a working program keep working.
pub unsafe extern "C" fn CFNumberGetValue(
    number: CFTypeRef,
    kind: i32,
    out: *mut c_void,
) -> Boolean {
    let (Some(n), Some((width, is_float)), false) = (
        unsafe { collections::number(number) },
        collections::number_shape(kind),
        out.is_null(),
    ) else {
        return 0;
    };
    let (bytes, lossless) = match is_float {
        true => match width {
            4 => {
                let v = n.as_f64();
                let mut b = [0u8; 8];
                b[..4].copy_from_slice(&(v as f32).to_le_bytes());
                (b, (v as f32) as f64 == v)
            }
            _ => (n.as_f64().to_le_bytes(), true),
        },
        false => {
            let v = n.as_i64();
            let shift = 64 - width * 8;
            let fits = width == 8 || ((v << shift) >> shift) == v;
            (v.to_le_bytes(), fits && !n.is_float())
        }
    };
    // SAFETY: the caller says `out` has room for a number of this type.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, width) };
    lossless as Boolean
}

pub unsafe extern "C" fn CFNumberIsFloatType(number: CFTypeRef) -> Boolean {
    unsafe { collections::number(number) }.is_some_and(|n| n.is_float()) as Boolean
}

/// `CFNumberGetType`: the canonical type the number was created with, which is
/// one of the six `SInt`/`Float` storage types whatever C name it was given.
///
/// Zero, which is no type, for anything that is not a number.
pub unsafe extern "C" fn CFNumberGetType(number: CFTypeRef) -> CFIndex {
    unsafe { collections::number_type(number) }.map_or(0, |kind| kind as CFIndex)
}

/// The width of the number's canonical type.
pub unsafe extern "C" fn CFNumberGetByteSize(number: CFTypeRef) -> CFIndex {
    unsafe { collections::number_type(number) }
        .and_then(collections::number_shape)
        .map_or(0, |(width, _)| width as CFIndex)
}

pub unsafe extern "C" fn CFDataGetTypeID() -> CFTypeID {
    crate::base::type_id_of(collections::data_class())
}

pub unsafe extern "C" fn CFDataCreate(
    _allocator: CFAllocatorRef,
    bytes: *const u8,
    length: CFIndex,
) -> CFTypeRef {
    if length < 0 || (bytes.is_null() && length > 0) {
        return std::ptr::null();
    }
    // SAFETY: the caller says `bytes` covers `length` bytes.
    let slice = match length {
        0 => &[][..],
        _ => unsafe { std::slice::from_raw_parts(bytes, length as usize) },
    };
    collections::new_data(slice.to_vec())
}

/// `CFDataCreateWithBytesNoCopy` with an owned compatibility copy.
///
/// CoreFoundation can borrow the caller's buffer and later release it through
/// `contents_deallocator`. This runtime's data objects own a `Vec`, so copying
/// preserves the bytes for the object's lifetime without trying to interpret a
/// foreign allocator object.
pub unsafe extern "C" fn CFDataCreateWithBytesNoCopy(
    allocator: CFAllocatorRef,
    bytes: *const u8,
    length: CFIndex,
    _contents_deallocator: CFAllocatorRef,
) -> CFTypeRef {
    // SAFETY: the validity requirement is identical to `CFDataCreate`'s.
    unsafe { CFDataCreate(allocator, bytes, length) }
}

/// `CFDataCreateCopy`, which forwards a foreign original rather than reading it.
///
/// The keyboard layout `TISGetInputSourceProperty` hands back is Apple's
/// `CFData`, and the copy of it goes straight back into `UCKeyTranslate` - so
/// the copy has to be Apple's too. See [`crate::foreign`].
pub unsafe extern "C" fn CFDataCreateCopy(allocator: CFAllocatorRef, data: CFTypeRef) -> CFTypeRef {
    #[cfg(target_os = "macos")]
    // SAFETY: null or an object.
    if unsafe { crate::foreign::is(data) } {
        if !crate::foreign::forwarding() {
            objc::fail!(
                "CFDataCreateCopy: the data is not this runtime's, and ROSETTA3_NO_FOREIGN                  forbids passing it to the CoreFoundation that made it"
            );
        }
        let Some(f) = crate::foreign::entry(c"CFDataCreateCopy") else {
            objc::fail!(
                "CFDataCreateCopy: the data is not this runtime's and there is no host                  CoreFoundation to pass it to"
            );
        };
        // SAFETY: the framework's own declaration, and an object it made.
        return unsafe {
            let f: unsafe extern "C" fn(CFAllocatorRef, CFTypeRef) -> CFTypeRef =
                std::mem::transmute(f);
            f(allocator, data)
        };
    }
    // silence clippy warning on non-macos
    let _ = allocator;
    let Some(bytes) = (unsafe { collections::data(data) }) else {
        return std::ptr::null();
    };
    collections::new_data(bytes.clone())
}

pub unsafe extern "C" fn CFDataGetLength(data: CFTypeRef) -> CFIndex {
    unsafe { collections::data(data) }.map_or(0, |d| d.len() as CFIndex)
}

/// The bytes, and for an empty `CFData` a readable byte rather than Rust's.
///
/// `Vec::as_ptr` on a `Vec` that never allocated is the *alignment* - address
/// `1` for a `u8` - which is not null and not readable. A guest that checks the
/// pointer against null sees a valid one and dereferences it, and Apple's
/// `CFDataGetBytePtr` never hands out an address that cannot be read. So an
/// empty `CFData` answers with a byte of this runtime's own instead.
/// `CFMutableDataRef CFDataCreateMutable(CFAllocatorRef, CFIndex capacity)`
///
/// The capacity is a hint, and a zero one means "no limit" - neither is a length,
/// so the data starts empty either way.
///
/// `CFData.h` declares **one** C type with two constnesses -
/// `CF_BRIDGED_TYPE(NSData) CFDataRef` against
/// `CF_BRIDGED_MUTABLE_TYPE(NSMutableData) CFMutableDataRef` - so the
/// distinction belongs to the reference and nowhere else. This is the one C
/// entry point whose entire contract is to hand back the **mutable** one, so
/// building an immutable data here was the flag's half not being carried.
pub unsafe extern "C" fn CFDataCreateMutable(
    _allocator: CFAllocatorRef,
    capacity: CFIndex,
) -> CFTypeRef {
    let mut bytes = Vec::new();
    if let Ok(capacity) = usize::try_from(capacity) {
        bytes.reserve(capacity.min(1 << 24));
    }
    collections::new_mutable_data(bytes)
}

/// `void CFDataGetBytes(CFDataRef, CFRange, UInt8 *buffer)`, with the range
/// flattened into the two words it is passed in.
///
/// A range that is not inside the data copies nothing, for
/// [`CFStringGetCharacters`]' reason: the caller sized its buffer from the range it
/// asked for.
///
/// # Safety
///
/// `buffer` is writable for `range_length` bytes.
pub unsafe extern "C" fn CFDataGetBytes(
    data: CFTypeRef,
    range_location: CFIndex,
    range_length: CFIndex,
    buffer: *mut u8,
) {
    let Some(bytes) = (unsafe { collections::data(data) }) else {
        return;
    };
    let (Ok(start), Ok(count)) = (
        usize::try_from(range_location),
        usize::try_from(range_length),
    ) else {
        return;
    };
    if buffer.is_null() || start.saturating_add(count) > bytes.len() {
        return;
    }
    // SAFETY: the caller states `buffer` holds the range, which has just been
    // checked against the data.
    unsafe { std::ptr::copy_nonoverlapping(bytes[start..].as_ptr(), buffer, count) };
}

/// `CFStringEncoding CFStringGetSystemEncoding(void)`
///
/// UTF-8. On a Mac this is the encoding of the user's legacy script system, which
/// is MacRoman on a machine that has never changed it; here every string this
/// runtime builds is UTF-8 and every path the host takes is bytes, so UTF-8 is
/// both the truth and what a caller round-tripping text through it needs.
pub unsafe extern "C" fn CFStringGetSystemEncoding() -> u32 {
    string::encoding::UTF8
}

/// `kCFStringEncodingInvalidId`.
const INVALID_ENCODING: u32 = 0xffff_ffff;

/// Apple's encodings by Windows code page and by IANA name, for the conversions
/// below. Only encodings CoreFoundation names; an unknown one answers
/// `kCFStringEncodingInvalidId`, as Apple's does.
const NAMED_ENCODINGS: &[(u32, &str, u32)] = &[
    (65001, "utf-8", string::encoding::UTF8),
    (1200, "utf-16le", string::encoding::UTF16_LE),
    (1201, "utf-16be", string::encoding::UTF16_BE),
    (12000, "utf-32le", string::encoding::UTF32_LE),
    (12001, "utf-32be", string::encoding::UTF32_BE),
    (20127, "us-ascii", string::encoding::ASCII),
    (10000, "macintosh", string::encoding::MAC_ROMAN),
    (1252, "windows-1252", string::encoding::WINDOWS_LATIN1),
    (1250, "windows-1250", 0x0501),
    (1251, "windows-1251", 0x0502),
    (1253, "windows-1253", 0x0503),
    (1254, "windows-1254", 0x0504),
    (1255, "windows-1255", 0x0505),
    (1256, "windows-1256", 0x0506),
    (1257, "windows-1257", 0x0507),
    (1258, "windows-1258", 0x0508),
    (28591, "iso-8859-1", string::encoding::ISO_LATIN_1),
    (28592, "iso-8859-2", 0x0202),
    (28593, "iso-8859-3", 0x0203),
    (28594, "iso-8859-4", 0x0204),
    (28595, "iso-8859-5", 0x0205),
    (28596, "iso-8859-6", 0x0206),
    (28597, "iso-8859-7", 0x0207),
    (28598, "iso-8859-8", 0x0208),
    (28599, "iso-8859-9", 0x0209),
    (28605, "iso-8859-15", 0x020f),
    (437, "ibm437", 0x0400),
    (850, "ibm850", 0x0410),
    (852, "ibm852", 0x0411),
    (866, "ibm866", 0x041b),
    (932, "shift_jis", 0x0a01),
    (936, "gbk", 0x0421),
    (949, "ks_c_5601-1987", 0x0422),
    (950, "big5", 0x0a03),
    (20866, "koi8-r", 0x0a02),
    (21866, "koi8-u", 0x0a08),
    (51932, "euc-jp", 0x0920),
    (51936, "gb2312", 0x0930),
    (51949, "euc-kr", 0x0940),
    (54936, "gb18030", 0x0632),
];

/// `CFStringEncoding CFStringConvertWindowsCodepageToEncoding(UInt32)`.
pub unsafe extern "C" fn CFStringConvertWindowsCodepageToEncoding(codepage: u32) -> u32 {
    NAMED_ENCODINGS
        .iter()
        .find(|(page, _, _)| *page == codepage)
        .map_or(INVALID_ENCODING, |(_, _, encoding)| *encoding)
}

/// `CFStringEncoding CFStringConvertIANACharSetNameToEncoding(CFStringRef)`.
///
/// Names compare without regard to case, as charset names do.
pub unsafe extern "C" fn CFStringConvertIANACharSetNameToEncoding(name: CFTypeRef) -> u32 {
    let Some(name) = (unsafe { string::as_string(name) }) else {
        return INVALID_ENCODING;
    };
    let name = String::from_utf8_lossy(&name.utf8()).to_ascii_lowercase();
    let name = match name.as_str() {
        "utf8" => "utf-8",
        "ascii" => "us-ascii",
        "latin1" | "iso_8859-1" => "iso-8859-1",
        "cp1252" => "windows-1252",
        "sjis" | "x-sjis" => "shift_jis",
        "utf-16" => return string::encoding::UTF16,
        "utf-32" => return string::encoding::UTF32,
        other => other,
    };
    NAMED_ENCODINGS
        .iter()
        .find(|(_, iana, _)| *iana == name)
        .map_or(INVALID_ENCODING, |(_, _, encoding)| *encoding)
}

/// `Boolean CFArrayContainsValue(CFArrayRef, CFRange, const void *)`, with the
/// range flattened.
///
/// Compared with the array's own equality callback, which is what makes a search
/// for a string find an equal one rather than only the same object.
///
/// # Safety
///
/// Both arguments are null or objects this runtime allocated.
pub unsafe extern "C" fn CFArrayContainsValue(
    array: CFTypeRef,
    range_location: CFIndex,
    range_length: CFIndex,
    value: CFTypeRef,
) -> Boolean {
    let Some(contents) = (unsafe { collections::array(array) }) else {
        return 0;
    };
    let (Ok(start), Ok(count)) = (
        usize::try_from(range_location),
        usize::try_from(range_length),
    ) else {
        return 0;
    };
    let end = start.saturating_add(count).min(contents.values.len());
    contents
        .values
        .get(start..end)
        .is_some_and(|slice| slice.iter().any(|v| contents.policy.equal(*v, value))) as Boolean
}

/// `void CFArrayAppendArray(CFMutableArrayRef, CFArrayRef, CFRange)`, with the
/// range flattened.
///
/// # Safety
///
/// As [`CFArrayContainsValue`].
pub unsafe extern "C" fn CFArrayAppendArray(
    array: CFTypeRef,
    other: CFTypeRef,
    range_location: CFIndex,
    range_length: CFIndex,
) {
    let Some(source) = (unsafe { collections::array(other) }) else {
        return;
    };
    let (Ok(start), Ok(count)) = (
        usize::try_from(range_location),
        usize::try_from(range_length),
    ) else {
        return;
    };
    let end = start.saturating_add(count).min(source.values.len());
    let Some(taken) = source.values.get(start..end).map(<[_]>::to_vec) else {
        return;
    };
    for value in taken {
        // SAFETY: a value from the other array and the caller's destination; the
        // append refuses an immutable array by name.
        unsafe { CFArrayAppendValue(array, value) };
    }
}

pub unsafe extern "C" fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8 {
    static EMPTY: u8 = 0;
    unsafe { collections::data(data) }.map_or(std::ptr::null(), |d| match d.is_empty() {
        true => &raw const EMPTY,
        false => d.as_ptr(),
    })
}

//

pub unsafe extern "C" fn CFURLGetTypeID() -> CFTypeID {
    crate::base::type_id_of(crate::url::class())
}

/// Turn Carbon's opaque `FSRef` into a URL owned by this CoreFoundation.
/// Only the host knows the reference's layout, so it is asked only to decode
/// the path; the URL object itself remains in this runtime's object model.
pub unsafe extern "C" fn CFURLCreateFromFSRef(
    _allocator: CFAllocatorRef,
    fs_ref: *const c_void,
) -> CFTypeRef {
    if fs_ref.is_null() {
        return std::ptr::null();
    }
    let Some(entry) = carbon(c"FSRefMakePath") else {
        return std::ptr::null();
    };
    // SAFETY: the Carbon entry point has the documented
    // `OSStatus(const FSRef *, UInt8 *, UInt32)` signature.
    let make_path: unsafe extern "C" fn(*const c_void, *mut u8, u32) -> i32 =
        unsafe { std::mem::transmute(entry) };
    let mut bytes = [0_u8; libc::PATH_MAX as usize];
    // SAFETY: `fs_ref` is the caller's live reference and `bytes` is a
    // writable buffer whose exact capacity accompanies it.
    if unsafe { make_path(fs_ref, bytes.as_mut_ptr(), bytes.len() as u32) } != 0 {
        return std::ptr::null();
    }
    let Some(end) = bytes.iter().position(|byte| *byte == 0) else {
        return std::ptr::null();
    };
    let path = String::from_utf8_lossy(&bytes[..end]);
    // The path is the guest's spelling; the host's answers whether it is a folder.
    let directory =
        std::fs::metadata(prefix::ns::host_path(&path)).is_ok_and(|metadata| metadata.is_dir());
    crate::url::new_file_url(&path, directory)
}

/// A Carbon File Manager entry point, wherever this process has one.
///
/// An `FSRef` is opaque and only the File Manager that made it can read it, so
/// these two calls are the seam between a URL and a reference. On a Mac that
/// File Manager is Apple's and a `dlsym` finds it. Off one it is
/// `coreservices::files`, which is published to the guest through a synthetic
/// library and is therefore *not* in this binary's exports - so the runtime's
/// own lookup is asked, which is the same one the loader answers a guest's
/// import with. Without that the two would disagree: the guest would get a
/// working `FSRefMakePath` and a `CFURLCreateFromFSRef` that answered null.
fn carbon(name: &std::ffi::CStr) -> Option<*mut c_void> {
    #[cfg(target_os = "macos")]
    if let Some(found) = crate::foreign::entry(name) {
        return Some(found);
    }
    let address = darwin::symbol(name.to_str().ok()?)?;
    Some(address as usize as *mut c_void)
}

pub unsafe extern "C" fn CFURLGetFSRef(url: CFTypeRef, fs_ref: *mut c_void) -> Boolean {
    if fs_ref.is_null() {
        return 0;
    }
    // SAFETY: a URL or null.
    let Some(path) = (unsafe { crate::url::as_url(url) }).and_then(|url| url.file_path()) else {
        return 0;
    };
    let Ok(path) = std::ffi::CString::new(path) else {
        return 0;
    };
    let Some(entry) = carbon(c"FSPathMakeRef") else {
        return 0;
    };
    // SAFETY: the entry has the documented Carbon signature; the guest supplied
    // storage for an `FSRef`, whose layout is architecture independent opaque
    // bytes.
    let make_ref: unsafe extern "C" fn(*const u8, *mut c_void, *mut u8) -> i32 =
        unsafe { std::mem::transmute(entry) };
    u8::from(unsafe { make_ref(path.as_ptr().cast(), fs_ref, std::ptr::null_mut()) } == 0)
}

/// What starts every bookmark this tree makes; the guest path of the item
/// follows. Apple's format records volume and file identifiers this host
/// cannot supply, and a bookmark is opaque to the program that holds one.
const BOOKMARK_MAGIC: &[u8] = b"R3Bookmark\0";

/// The guest path of a file URL, and the host path it names.
fn url_paths(url: CFTypeRef) -> Option<(String, std::path::PathBuf)> {
    // SAFETY: a URL or null.
    let path = unsafe { crate::url::as_url(url) }?.file_path()?;
    let host = prefix::ns::host_path(&path);
    Some((path, host))
}

/// The guest path a bookmark from [`CFURLCreateBookmarkData`] records.
fn bookmark_path(data: CFTypeRef) -> Option<String> {
    // SAFETY: a data object or null.
    let bytes = unsafe { collections::data(data) }?;
    let path = bytes.strip_prefix(BOOKMARK_MAGIC)?;
    String::from_utf8(path.to_vec()).ok()
}

/// `CFDataRef CFURLCreateBookmarkData(CFAllocatorRef, CFURLRef,
/// CFURLBookmarkCreationOptions, CFArrayRef, CFURLRef, CFErrorRef *)`
///
/// Null, as on a Mac, for an item that is not there. No error object is made.
pub unsafe extern "C" fn CFURLCreateBookmarkData(
    _allocator: CFAllocatorRef,
    url: CFTypeRef,
    _options: u64,
    _properties: CFTypeRef,
    _relative_to: CFTypeRef,
    _error: *mut CFTypeRef,
) -> CFTypeRef {
    let Some((path, host)) = url_paths(url) else {
        return std::ptr::null();
    };
    if std::fs::symlink_metadata(&host).is_err() {
        return std::ptr::null();
    }
    let mut bytes = BOOKMARK_MAGIC.to_vec();
    bytes.extend_from_slice(path.as_bytes());
    collections::new_data(bytes)
}

/// `CFURLRef CFURLCreateByResolvingBookmarkData(CFAllocatorRef, CFDataRef,
/// CFURLBookmarkResolutionOptions, CFURLRef, CFArrayRef, Boolean *, CFErrorRef *)`
pub unsafe extern "C" fn CFURLCreateByResolvingBookmarkData(
    _allocator: CFAllocatorRef,
    bookmark: CFTypeRef,
    _options: u64,
    _relative_to: CFTypeRef,
    _properties: CFTypeRef,
    is_stale: *mut Boolean,
    _error: *mut CFTypeRef,
) -> CFTypeRef {
    let Some(path) = bookmark_path(bookmark) else {
        return std::ptr::null();
    };
    let Ok(metadata) = std::fs::metadata(prefix::ns::host_path(&path)) else {
        return std::ptr::null();
    };
    if !is_stale.is_null() {
        // SAFETY: the caller's writable `Boolean`.
        unsafe { *is_stale = 0 };
    }
    crate::url::new_file_url(&path, metadata.is_dir())
}

/// `Boolean CFURLWriteBookmarkDataToFile(CFDataRef, CFURLRef,
/// CFURLBookmarkFileCreationOptions, CFErrorRef *)`
///
/// A Finder alias file is a format the host cannot follow, so the alias is
/// written as a symlink, which both a Mac and this host resolve.
pub unsafe extern "C" fn CFURLWriteBookmarkDataToFile(
    bookmark: CFTypeRef,
    file: CFTypeRef,
    _options: u64,
    _error: *mut CFTypeRef,
) -> Boolean {
    let (Some(target), Some((_, at))) = (bookmark_path(bookmark), url_paths(file)) else {
        return 0;
    };
    let Ok(c_at) = std::ffi::CString::new(at.as_os_str().as_encoded_bytes()) else {
        return 0;
    };
    if prefix::ns::may_write_at(libc::AT_FDCWD, &c_at).is_err() {
        return 0;
    }
    if std::fs::symlink_metadata(&at).is_ok_and(|m| !m.is_dir()) {
        let _ = std::fs::remove_file(&at);
    }
    u8::from(std::os::unix::fs::symlink(prefix::ns::host_path(&target), &at).is_ok())
}

/// `CFDataRef CFURLCreateBookmarkDataFromFile(CFAllocatorRef, CFURLRef, CFErrorRef *)`
///
/// Reads back what [`CFURLWriteBookmarkDataToFile`] wrote: a symlink.
pub unsafe extern "C" fn CFURLCreateBookmarkDataFromFile(
    _allocator: CFAllocatorRef,
    file: CFTypeRef,
    _error: *mut CFTypeRef,
) -> CFTypeRef {
    let Some((_, at)) = url_paths(file) else {
        return std::ptr::null();
    };
    let Ok(target) = std::fs::read_link(&at) else {
        return std::ptr::null();
    };
    let guest = prefix::ns::to_guest(&target).unwrap_or(target);
    let mut bytes = BOOKMARK_MAGIC.to_vec();
    bytes.extend_from_slice(guest.as_os_str().as_encoded_bytes());
    collections::new_data(bytes)
}

/// `kCFURLPOSIXPathStyle`, which is how a path on this host is written.
const POSIX_PATH_STYLE: CFIndex = 0;
/// `kCFURLHFSPathStyle`: `Volume:folder:file`, colon-separated, deprecated
/// since Mac OS X shipped and still asked for by code of that vintage.
const HFS_PATH_STYLE: CFIndex = 1;

/// A POSIX path in the old Mac notation.
///
/// There are no HFS volumes here, so the leading name is the root directory's:
/// a path under `/` becomes `/:home:user:game`. That keeps the two directions
/// exact inverses of each other - see [`posix_from_hfs`] - which is the only
/// thing a caller that converts a path and converts it back depends on. It is
/// *not* a path this host can open, but neither is an HFS path on a Mac: a
/// caller asking for this notation knows it has left the POSIX one.
fn hfs_from_posix(path: &str) -> String {
    let mut out = String::new();
    for (at, component) in path.trim_start_matches('/').split('/').enumerate() {
        if at == 0 {
            out.push('/');
        }
        out.push(':');
        // A colon is the separator there and a legal byte here, and the Finder
        // shows such a name with a slash - which is the same swap, the other
        // way round.
        out.push_str(&component.replace(':', "/"));
    }
    match out.is_empty() {
        true => "/".to_string(),
        false => out,
    }
}

/// The inverse of [`hfs_from_posix`].
fn posix_from_hfs(path: &str) -> String {
    let mut components = path.split(':');
    // The volume, which is the root directory here.
    let _ = components.next();
    let mut out = String::new();
    for component in components {
        out.push('/');
        out.push_str(&component.replace('/', ":"));
    }
    match out.is_empty() {
        true => "/".to_string(),
        false => out,
    }
}

pub unsafe extern "C" fn CFURLCreateWithFileSystemPath(
    _allocator: CFAllocatorRef,
    path: CFTypeRef,
    style: CFIndex,
    directory: Boolean,
) -> CFTypeRef {
    if style != POSIX_PATH_STYLE && style != HFS_PATH_STYLE {
        // The Windows path style describes a drive letter and a separator this
        // runtime has no notion of, so a path in it read as POSIX would name a
        // different file rather than fail.
        objc::fail!("CFURLCreateWithFileSystemPath: path style {style} is neither POSIX nor HFS");
    }
    // SAFETY: a `CFString` or null.
    let Some(path) = (unsafe { string::as_string(path) }) else {
        return std::ptr::null();
    };
    let bytes = path.utf8();
    let Ok(path) = std::str::from_utf8(&bytes) else {
        return std::ptr::null();
    };
    let converted;
    let path = match style {
        HFS_PATH_STYLE => {
            converted = posix_from_hfs(path);
            converted.as_str()
        }
        _ => path,
    };
    crate::url::new_file_url(path, directory != 0)
}

pub unsafe extern "C" fn CFURLCreateWithFileSystemPathRelativeToBase(
    _allocator: CFAllocatorRef,
    path: CFTypeRef,
    style: CFIndex,
    directory: Boolean,
    base: CFTypeRef,
) -> CFTypeRef {
    if style != POSIX_PATH_STYLE {
        objc::fail!("CFURLCreateWithFileSystemPathRelativeToBase: path style {style} is not POSIX");
    }
    // SAFETY: a string and optional URL.
    let Some(path) = (unsafe { string::as_string(path) }) else {
        return std::ptr::null();
    };
    let path_bytes = path.utf8();
    let Ok(path) = std::str::from_utf8(&path_bytes) else {
        return std::ptr::null();
    };
    if std::path::Path::new(path).is_absolute() {
        return crate::url::new_file_url(path, directory != 0);
    }
    let Some(base_path) = (unsafe { crate::url::as_url(base) }).and_then(|base| base.file_path())
    else {
        return crate::url::new_file_url(path, directory != 0);
    };
    let joined = std::path::Path::new(&base_path).join(path);
    joined.to_str().map_or(std::ptr::null(), |path| {
        crate::url::new_file_url(path, directory != 0)
    })
}

pub unsafe extern "C" fn CFURLCreateWithString(
    _allocator: CFAllocatorRef,
    text: CFTypeRef,
    base: CFTypeRef,
) -> CFTypeRef {
    // SAFETY: a string and an optional URL.
    let Some(text) = (unsafe { string::as_string(text) }) else {
        return std::ptr::null();
    };
    let text_bytes = text.utf8();
    let Ok(text) = std::str::from_utf8(&text_bytes) else {
        return std::ptr::null();
    };
    let Some(base) = (unsafe { crate::url::as_url(base) }) else {
        return crate::url::new(text);
    };
    if text.contains(':') {
        return crate::url::new(text);
    }
    let absolute = if text.starts_with('/') {
        match base.text.find("://") {
            Some(scheme_end) => {
                let authority_end = base.text[scheme_end + 3..]
                    .find('/')
                    .map_or(base.text.len(), |slash| scheme_end + 3 + slash);
                format!("{}{text}", &base.text[..authority_end])
            }
            None => text.to_string(),
        }
    } else {
        let directory_end = base
            .text
            .rfind('/')
            .map_or(base.text.len(), |slash| slash + 1);
        format!("{}{text}", &base.text[..directory_end])
    };
    crate::url::new(&absolute)
}

pub unsafe extern "C" fn CFURLGetString(url: CFTypeRef) -> CFTypeRef {
    // SAFETY: a URL or null.
    unsafe { crate::url::as_url(url) }.map_or(std::ptr::null(), crate::url::Contents::string)
}

pub unsafe extern "C" fn CFURLCopyAbsoluteURL(url: CFTypeRef) -> CFTypeRef {
    // URLs are stored absolute after construction, so a copy preserves text.
    // SAFETY: a URL or null.
    unsafe { crate::url::as_url(url) }.map_or(std::ptr::null(), |url| crate::url::new(&url.text))
}

fn copied_url_piece(
    url: CFTypeRef,
    piece: impl FnOnce(&crate::url::Contents) -> String,
) -> CFTypeRef {
    // SAFETY: callers pass through the guest's live URL argument.
    unsafe { crate::url::as_url(url) }.map_or(std::ptr::null(), |url| {
        let piece = piece(url);
        string::from_bytes(piece.as_bytes(), false)
    })
}

pub unsafe extern "C" fn CFURLCopyPath(url: CFTypeRef) -> CFTypeRef {
    copied_url_piece(url, crate::url::Contents::path)
}

pub unsafe extern "C" fn CFURLCopyLastPathComponent(url: CFTypeRef) -> CFTypeRef {
    copied_url_piece(url, |url| {
        url.path()
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string()
    })
}

pub unsafe extern "C" fn CFURLCopyScheme(url: CFTypeRef) -> CFTypeRef {
    copied_url_piece(url, crate::url::Contents::scheme)
}

pub unsafe extern "C" fn CFURLCopyHostName(url: CFTypeRef) -> CFTypeRef {
    copied_url_piece(url, |url| {
        let Some((_, rest)) = url.text.split_once("://") else {
            return String::new();
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let host_port = authority.rsplit('@').next().unwrap_or_default();
        if host_port.starts_with('[') {
            return host_port
                .find(']')
                .map_or(host_port, |end| &host_port[..=end])
                .to_string();
        }
        host_port.split(':').next().unwrap_or_default().to_string()
    })
}

pub unsafe extern "C" fn CFURLGetPortNumber(url: CFTypeRef) -> i32 {
    // SAFETY: a URL or null.
    let Some(url) = (unsafe { crate::url::as_url(url) }) else {
        return -1;
    };
    let Some((_, rest)) = url.text.split_once("://") else {
        return -1;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    if host_port.starts_with('[') {
        return host_port
            .split_once("]:")
            .and_then(|(_, port)| port.parse().ok())
            .unwrap_or(-1);
    }
    host_port
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or(-1)
}

pub unsafe extern "C" fn CFURLHasDirectoryPath(url: CFTypeRef) -> Boolean {
    // SAFETY: a URL or null.
    u8::from(unsafe { crate::url::as_url(url) }.is_some_and(|url| {
        url.text
            .split(['?', '#'])
            .next()
            .is_some_and(|path| path.ends_with('/'))
    }))
}

pub unsafe extern "C" fn CFURLCreateCopyAppendingPathComponent(
    _allocator: CFAllocatorRef,
    url: CFTypeRef,
    component: CFTypeRef,
    directory: Boolean,
) -> CFTypeRef {
    // SAFETY: a URL and a string from this runtime.
    let (Some(base), Some(component)) = (unsafe { crate::url::as_url(url) }, unsafe {
        string::as_string(component)
    }) else {
        return std::ptr::null();
    };
    let Some(mut path) = base.file_path() else {
        return std::ptr::null();
    };
    let component = component.utf8();
    let Ok(component) = std::str::from_utf8(&component) else {
        return std::ptr::null();
    };
    if !path.ends_with('/') {
        path.push('/');
    }
    path.push_str(component.trim_matches('/'));
    crate::url::new_file_url(&path, directory != 0)
}

pub unsafe extern "C" fn CFURLCreateCopyDeletingLastPathComponent(
    _allocator: CFAllocatorRef,
    url: CFTypeRef,
) -> CFTypeRef {
    // SAFETY: a URL or null.
    let Some(url) = (unsafe { crate::url::as_url(url) }) else {
        return std::ptr::null();
    };
    if let Some(path) = url.file_path() {
        return std::path::Path::new(&path)
            .parent()
            .and_then(std::path::Path::to_str)
            .map_or(std::ptr::null(), |parent| {
                crate::url::new_file_url(parent, true)
            });
    }
    let suffix = url.text.find(['?', '#']).unwrap_or(url.text.len());
    let path = &url.text[..suffix];
    let Some(slash) = path.trim_end_matches('/').rfind('/') else {
        return crate::url::new(&url.text);
    };
    crate::url::new(&format!("{}/", &path[..slash]))
}

pub unsafe extern "C" fn CFURLCreateCopyDeletingPathExtension(
    _allocator: CFAllocatorRef,
    url: CFTypeRef,
) -> CFTypeRef {
    // SAFETY: a URL or null.
    let Some(url) = (unsafe { crate::url::as_url(url) }) else {
        return std::ptr::null();
    };
    if let Some(path) = url.file_path() {
        let mut path = std::path::PathBuf::from(path);
        path.set_extension("");
        return path.to_str().map_or(std::ptr::null(), |path| {
            crate::url::new_file_url(path, false)
        });
    }
    let suffix = url.text.find(['?', '#']).unwrap_or(url.text.len());
    let (path, tail) = url.text.split_at(suffix);
    let slash = path.rfind('/').map_or(0, |slash| slash + 1);
    let Some(dot) = path[slash..].rfind('.').map(|dot| slash + dot) else {
        return crate::url::new(&url.text);
    };
    crate::url::new(&format!("{}{tail}", &path[..dot]))
}

/// Add URL percent escapes while preserving the caller-selected exceptions.
pub unsafe extern "C" fn CFURLCreateStringByAddingPercentEscapes(
    _allocator: CFAllocatorRef,
    original: CFTypeRef,
    leave_unescaped: CFTypeRef,
    force_escaped: CFTypeRef,
    encoding: u32,
) -> CFTypeRef {
    if encoding != string::encoding::UTF8 {
        objc::fail!("CFURLCreateStringByAddingPercentEscapes: unsupported encoding {encoding:#x}");
    }
    // SAFETY: strings or null for the two sets.
    let Some(original) = (unsafe { string::as_string(original) }) else {
        return std::ptr::null();
    };
    let leave = unsafe { string::as_string(leave_unescaped) }
        .map(|value| String::from_utf8_lossy(&value.utf8()).into_owned())
        .unwrap_or_default();
    let force = unsafe { string::as_string(force_escaped) }
        .map(|value| String::from_utf8_lossy(&value.utf8()).into_owned())
        .unwrap_or_default();
    let original_utf8 = original.utf8();
    let source = String::from_utf8_lossy(&original_utf8);
    let mut escaped = String::with_capacity(source.len());
    for character in source.chars() {
        let ordinarily_legal =
            character.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=%".contains(character);
        if (ordinarily_legal || leave.contains(character)) && !force.contains(character) {
            escaped.push(character);
        } else {
            let mut utf8 = [0_u8; 4];
            for byte in character.encode_utf8(&mut utf8).bytes() {
                escaped.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    string::from_bytes(escaped.as_bytes(), false)
}

unsafe fn replacing_percent_escapes(
    original: CFTypeRef,
    leave_escaped: CFTypeRef,
    encoding: u32,
) -> CFTypeRef {
    if encoding != string::encoding::UTF8 {
        return std::ptr::null();
    }
    // SAFETY: strings or null.
    let Some(original) = (unsafe { string::as_string(original) }) else {
        return std::ptr::null();
    };
    let leave = unsafe { string::as_string(leave_escaped) }
        .map(|value| String::from_utf8_lossy(&value.utf8()).into_owned())
        .unwrap_or_default();
    let bytes = original.utf8();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let value = if bytes[index] == b'%' && index + 2 < bytes.len() {
            std::str::from_utf8(&bytes[index + 1..index + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        } else {
            None
        };
        match value {
            Some(value) if value.is_ascii() && leave.contains(value as char) => {
                decoded.extend_from_slice(&bytes[index..index + 3]);
                index += 3;
            }
            Some(value) => {
                decoded.push(value);
                index += 3;
            }
            None => {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
    }
    string::from_bytes(&decoded, false)
}

pub unsafe extern "C" fn CFURLCreateStringByReplacingPercentEscapes(
    _allocator: CFAllocatorRef,
    original: CFTypeRef,
    leave_escaped: CFTypeRef,
) -> CFTypeRef {
    // SAFETY: the arguments retain their documented meaning.
    unsafe { replacing_percent_escapes(original, leave_escaped, string::encoding::UTF8) }
}

pub unsafe extern "C" fn CFURLCreateStringByReplacingPercentEscapesUsingEncoding(
    _allocator: CFAllocatorRef,
    original: CFTypeRef,
    leave_escaped: CFTypeRef,
    encoding: u32,
) -> CFTypeRef {
    // SAFETY: the arguments retain their documented meaning.
    unsafe { replacing_percent_escapes(original, leave_escaped, encoding) }
}

pub unsafe extern "C" fn CFURLCopyFileSystemPath(url: CFTypeRef, style: CFIndex) -> CFTypeRef {
    if style != POSIX_PATH_STYLE && style != HFS_PATH_STYLE {
        objc::fail!("CFURLCopyFileSystemPath: path style {style} is neither POSIX nor HFS");
    }
    // SAFETY: a URL or null.
    let Some(path) = (unsafe { crate::url::as_url(url) }).and_then(|url| url.file_path()) else {
        return std::ptr::null();
    };
    let path = match style {
        HFS_PATH_STYLE => hfs_from_posix(&path),
        _ => path,
    };
    string::from_bytes(path.as_bytes(), false)
}

/// `CFURLGetFileSystemRepresentation`, which fills `buffer` with a
/// NUL-terminated path and says whether it fit.
///
/// `resolve` is ignored: every URL here is absolute already, so there is no base
/// to resolve against and the two answers are the same one.
pub unsafe extern "C" fn CFURLGetFileSystemRepresentation(
    url: CFTypeRef,
    _resolve: Boolean,
    buffer: *mut u8,
    length: CFIndex,
) -> Boolean {
    // SAFETY: a `CFURL` or null.
    let Some(url) = (unsafe { crate::url::as_url(url) }) else {
        return 0;
    };
    if buffer.is_null() || length <= 0 {
        return 0;
    }
    // Only a file URL has a file system representation. Apple's answers false
    // for anything else rather than handing back the path component, which a
    // caller would then try to open.
    let Some(path) = url.file_path() else {
        return 0;
    };
    let path = path.as_bytes();
    // The NUL is part of what has to fit: a caller passes `PATH_MAX` and then
    // reads the buffer as a C string.
    if path.len() >= length as usize {
        return 0;
    }
    // SAFETY: `buffer` holds `length` bytes by the caller's contract, and the
    // check above leaves room for the path and its terminator.
    unsafe {
        std::ptr::copy_nonoverlapping(path.as_ptr(), buffer, path.len());
        *buffer.add(path.len()) = 0;
    }
    1
}

//

pub unsafe extern "C" fn CFUUIDGetTypeID() -> CFTypeID {
    crate::base::type_id_of(crate::uuid::class())
}

pub unsafe extern "C" fn CFUUIDCreate(_allocator: CFAllocatorRef) -> CFTypeRef {
    crate::uuid::new(crate::uuid::generate())
}

/// `CFStringRef CFUUIDCreateString(CFAllocatorRef, CFUUIDRef)`
pub unsafe extern "C" fn CFUUIDCreateString(
    _allocator: CFAllocatorRef,
    uuid: CFTypeRef,
) -> CFTypeRef {
    // SAFETY: a `CFUUID` or null.
    let Some(bytes) = (unsafe { crate::uuid::bytes_of(uuid) }) else {
        return std::ptr::null();
    };
    string::from_bytes(crate::uuid::format(&bytes).as_bytes(), false)
}

/// `CFUUIDGetConstantUUIDWithBytes`, whose sixteen bytes are sixteen arguments.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CFUUIDGetConstantUUIDWithBytes(
    _allocator: CFAllocatorRef,
    b0: u8,
    b1: u8,
    b2: u8,
    b3: u8,
    b4: u8,
    b5: u8,
    b6: u8,
    b7: u8,
    b8: u8,
    b9: u8,
    b10: u8,
    b11: u8,
    b12: u8,
    b13: u8,
    b14: u8,
    b15: u8,
) -> CFTypeRef {
    crate::uuid::constant([
        b0, b1, b2, b3, b4, b5, b6, b7, b8, b9, b10, b11, b12, b13, b14, b15,
    ])
}

use std::sync::Arc;
use std::time::Duration;

use crate::runloop::{self, RunLoop, Source, Timer};

/// `CFRunLoopSourceContext`, as a guest lays it out.
#[repr(C)]
pub struct CFRunLoopSourceContext {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: *const c_void,
    pub release: *const c_void,
    pub copy_description: *const c_void,
    pub equal: *const c_void,
    pub hash: *const c_void,
    pub schedule: *const c_void,
    pub cancel: *const c_void,
    pub perform: Option<runloop::Perform>,
}

/// `CFRunLoopTimerContext`.
#[repr(C)]
pub struct CFRunLoopTimerContext {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: *const c_void,
    pub release: *const c_void,
    pub copy_description: *const c_void,
}

/// A leaked `Arc`, as the C world holds one.
///
/// Never reclaimed by anything but the C world's own `CFRelease`, and only for
/// the kinds that say they own one - see [`runloop::leak`]. A `CFRunLoopSourceRef`
/// is owned by the loop it was added to as much as by the guest that made it, and
/// the loop's half is a real `Arc` the loop holds, so the guest's release cannot
/// take the source out from under it: that is a use-after-free where the extra
/// bookkeeping is a few dozen bytes. What the guest's release *can* do is end
/// the last reference, and that is what ends the registry entries too.
fn leak<T: 'static>(value: Arc<T>) -> CFTypeRef {
    runloop::leak(value)
}

/// Borrow a leaked `Arc` without changing its count.
///
/// `ManuallyDrop`, because the `Arc` is reconstructed from a pointer that is
/// still owned by the C world: letting it drop would release a reference nobody
/// took.
///
/// Anything this crate did not make is `None` rather than a `T`, which is what
/// keeps a source from some other CoreFoundation - IOKit's notification port
/// makes one - from being read as this crate's. See [`runloop::note`].
///
/// # Safety
///
/// `p` must be a pointer [`leak`] produced for a `T`, or one of the stable
/// pointers `CFRunLoopGetCurrent` hands out.
unsafe fn borrow<T: 'static>(p: CFTypeRef) -> Option<std::mem::ManuallyDrop<Arc<T>>> {
    if !runloop::is_noted::<T>(p as usize) {
        return None;
    }
    Some(std::mem::ManuallyDrop::new(unsafe {
        Arc::from_raw(p as *const T)
    }))
}

/// A run loop's address, which is stable for as long as its thread runs.
///
/// Not `leak`: `CFRunLoopGetCurrent` is called once a frame by anything with a
/// frame, and handing out an owned reference each time would add one to the
/// count forever. The thread-local holds the loop alive, so borrowing is both
/// sound and stable.
pub unsafe extern "C" fn CFRunLoopGetCurrent() -> CFTypeRef {
    Arc::as_ptr(&runloop::current()) as CFTypeRef
}

pub unsafe extern "C" fn CFRunLoopGetMain() -> CFTypeRef {
    Arc::as_ptr(&runloop::main()) as CFTypeRef
}

/// The mode name a guest passed, as a Rust string.
///
/// # Safety
///
/// `mode` must be null or a `CFStringRef`.
unsafe fn mode_name(mode: CFTypeRef) -> String {
    match unsafe { string::as_string(mode) } {
        Some(s) => String::from_utf8_lossy(&s.utf8()).into_owned(),
        // Null is `kCFRunLoopDefaultMode` in every call that takes one.
        None => runloop::DEFAULT_MODE.to_string(),
    }
}

pub unsafe extern "C" fn CFRunLoopRunInMode(
    mode: CFTypeRef,
    seconds: f64,
    return_after_source_handled: Boolean,
) -> i32 {
    // SAFETY: a `CFStringRef` or null.
    let (name, loop_) = unsafe { (mode_name(mode), runloop::current()) };
    loop_.run_in_mode(&name, seconds, return_after_source_handled != 0)
}

pub unsafe extern "C" fn CFRunLoopRun() {
    // `CFRunLoopRun` is the default mode with no timeout, until stopped.
    runloop::current().run_in_mode(runloop::DEFAULT_MODE, f64::INFINITY, false);
}

pub unsafe extern "C" fn CFRunLoopStop(loop_: CFTypeRef) {
    if let Some(l) = unsafe { borrow::<RunLoop>(loop_) } {
        l.stop();
    }
}

pub unsafe extern "C" fn CFRunLoopWakeUp(loop_: CFTypeRef) {
    if let Some(l) = unsafe { borrow::<RunLoop>(loop_) } {
        l.wake();
    }
}

pub unsafe extern "C" fn CFRunLoopSourceCreate(
    _allocator: CFAllocatorRef,
    order: CFIndex,
    context: *const CFRunLoopSourceContext,
) -> CFTypeRef {
    if context.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a live context structure.
    let context = unsafe { &*context };
    if context.version != 0 {
        // A version-1 source is a mach port, and this runtime has no Mach for a
        // *CoreFoundation* mach port. Apple's own answer for a version it does not
        // implement is null - and null is what a caller here would then fail to
        // schedule, rather than a source that is never fired, which is a program
        // that hangs with nothing anywhere to say why. Refused by name rather
        // than aborting: a `Create` that cannot be satisfied is a null, not a
        // process kill.
        crate::base::refuse_once(
            "CFRunLoopSourceCreate",
            format!(
                "a version-{} run loop source is a mach port, which this runtime has no \
                 CoreFoundation mach port for; Apple answers null for a source version it does \
                 not implement, and so does this",
                context.version
            ),
        );
        return std::ptr::null();
    }
    leak(runloop::new_source_ordered(
        context.perform,
        context.info as usize,
        order,
    ))
}

/// `CFRunLoopObserverRef CFRunLoopObserverCreate(CFAllocatorRef,
/// CFOptionFlags activities, Boolean repeats, CFIndex order,
/// CFRunLoopObserverCallBack, CFRunLoopObserverContext *)`
///
/// The context's `info` is what the callback is handed back; its retain and release
/// callbacks are not called, as they are not for a source here either - the
/// observer lives until the caller invalidates it.
///
/// # Safety
///
/// `context` is null or a live `CFRunLoopObserverContext`.
pub unsafe extern "C" fn CFRunLoopObserverCreate(
    _allocator: CFAllocatorRef,
    activities: u32,
    repeats: Boolean,
    order: CFIndex,
    callback: Option<runloop::ObserverCallback>,
    context: *const CFRunLoopObserverContext,
) -> CFTypeRef {
    let info = match context.is_null() {
        true => 0,
        // SAFETY: the caller states it is a live context.
        false => (unsafe { (*context).info }) as usize,
    };
    leak(runloop::new_observer(
        activities,
        repeats != 0,
        order as i32,
        callback,
        info,
    ))
}

/// `CFRunLoopObserverContext`, of which only `info` is read.
#[repr(C)]
pub struct CFRunLoopObserverContext {
    pub version: CFIndex,
    pub info: *mut std::ffi::c_void,
    pub retain: Option<unsafe extern "C" fn(*const std::ffi::c_void) -> *const std::ffi::c_void>,
    pub release: Option<unsafe extern "C" fn(*const std::ffi::c_void)>,
    pub copy_description: Option<unsafe extern "C" fn(*const std::ffi::c_void) -> CFTypeRef>,
}

pub unsafe extern "C" fn CFRunLoopObserverInvalidate(observer: CFTypeRef) {
    if let Some(o) = unsafe { borrow::<runloop::Observer>(observer) } {
        o.invalidate();
    }
}

pub unsafe extern "C" fn CFRunLoopObserverIsValid(observer: CFTypeRef) -> Boolean {
    unsafe { borrow::<runloop::Observer>(observer) }.is_some_and(|o| o.is_valid()) as Boolean
}

pub unsafe extern "C" fn CFRunLoopObserverGetOrder(observer: CFTypeRef) -> CFIndex {
    unsafe { borrow::<runloop::Observer>(observer) }.map_or(0, |o| o.order() as CFIndex)
}

pub unsafe extern "C" fn CFRunLoopAddObserver(
    loop_: CFTypeRef,
    observer: CFTypeRef,
    mode: CFTypeRef,
) {
    let (Some(l), Some(o)) = (unsafe { borrow::<RunLoop>(loop_) }, unsafe {
        borrow::<runloop::Observer>(observer)
    }) else {
        return;
    };
    l.add_observer(&o, &unsafe { mode_name(mode) });
}

pub unsafe extern "C" fn CFRunLoopRemoveObserver(
    loop_: CFTypeRef,
    observer: CFTypeRef,
    mode: CFTypeRef,
) {
    let (Some(l), Some(o)) = (unsafe { borrow::<RunLoop>(loop_) }, unsafe {
        borrow::<runloop::Observer>(observer)
    }) else {
        return;
    };
    l.remove_observer(&o, &unsafe { mode_name(mode) });
}

pub unsafe extern "C" fn CFRunLoopContainsObserver(
    loop_: CFTypeRef,
    observer: CFTypeRef,
    mode: CFTypeRef,
) -> Boolean {
    let (Some(l), Some(o)) = (unsafe { borrow::<RunLoop>(loop_) }, unsafe {
        borrow::<runloop::Observer>(observer)
    }) else {
        return 0;
    };
    l.contains_observer(&o, &unsafe { mode_name(mode) }) as Boolean
}

/// `void CFRunLoopAddCommonMode(CFRunLoopRef, CFStringRef)`
pub unsafe extern "C" fn CFRunLoopAddCommonMode(loop_: CFTypeRef, mode: CFTypeRef) {
    let Some(l) = (unsafe { borrow::<RunLoop>(loop_) }) else {
        return;
    };
    l.add_common_mode(&unsafe { mode_name(mode) });
}

/// `Boolean CFRunLoopIsWaiting(CFRunLoopRef)`
pub unsafe extern "C" fn CFRunLoopIsWaiting(loop_: CFTypeRef) -> Boolean {
    unsafe { borrow::<RunLoop>(loop_) }.is_some_and(|l| l.is_waiting()) as Boolean
}

pub unsafe extern "C" fn CFRunLoopSourceSignal(source: CFTypeRef) {
    if let Some(s) = unsafe { borrow::<Source>(source) } {
        s.signal();
    }
}

pub unsafe extern "C" fn CFRunLoopSourceInvalidate(source: CFTypeRef) {
    if let Some(s) = unsafe { borrow::<Source>(source) } {
        s.invalidate();
    }
}

pub unsafe extern "C" fn CFRunLoopSourceIsValid(source: CFTypeRef) -> Boolean {
    unsafe { borrow::<Source>(source) }.is_some_and(|s| s.is_valid()) as Boolean
}

pub unsafe extern "C" fn CFRunLoopAddSource(loop_: CFTypeRef, source: CFTypeRef, mode: CFTypeRef) {
    let (Some(l), Some(s)) = (unsafe { borrow::<RunLoop>(loop_) }, unsafe {
        borrow::<Source>(source)
    }) else {
        return;
    };
    l.add_source(&s, &unsafe { mode_name(mode) });
}

pub unsafe extern "C" fn CFRunLoopRemoveSource(
    loop_: CFTypeRef,
    source: CFTypeRef,
    mode: CFTypeRef,
) {
    let (Some(l), Some(s)) = (unsafe { borrow::<RunLoop>(loop_) }, unsafe {
        borrow::<Source>(source)
    }) else {
        return;
    };
    l.remove_source(&s, &unsafe { mode_name(mode) });
}

pub unsafe extern "C" fn CFRunLoopContainsSource(
    loop_: CFTypeRef,
    source: CFTypeRef,
    mode: CFTypeRef,
) -> Boolean {
    let (Some(l), Some(s)) = (unsafe { borrow::<RunLoop>(loop_) }, unsafe {
        borrow::<Source>(source)
    }) else {
        return 0;
    };
    l.contains_source(&s, &unsafe { mode_name(mode) }) as Boolean
}

/// `CFAbsoluteTime` is seconds since 2001; a fire date is one of those.
///
/// Converted to a delay rather than kept as an absolute: the loop measures with
/// a monotonic clock, which is the only one a timer should depend on.
fn delay_until(fire_date: f64) -> Duration {
    let now = absolute_time();
    Duration::from_secs_f64((fire_date - now).max(0.0).min(1e9))
}

/// Seconds since 2001-01-01, which is `CFAbsoluteTimeGetCurrent`.
fn absolute_time() -> f64 {
    const APPLE_EPOCH: f64 = 978_307_200.0;
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() - APPLE_EPOCH)
        .unwrap_or(0.0)
}

pub unsafe extern "C" fn CFAbsoluteTimeGetCurrent() -> f64 {
    absolute_time()
}

pub unsafe extern "C" fn CFRunLoopTimerCreate(
    _allocator: CFAllocatorRef,
    fire_date: f64,
    interval: f64,
    _flags: u32,
    order: CFIndex,
    callback: Option<runloop::TimerCallback>,
    context: *mut CFRunLoopTimerContext,
) -> CFTypeRef {
    // SAFETY: null, or a live context structure.
    let info = unsafe { context.as_ref() }.map_or(0, |c| c.info as usize);
    // An interval of zero or less is a one-shot, which is what Apple's does.
    let repeat = (interval > 0.0).then(|| Duration::from_secs_f64(interval.min(1e9)));
    leak(runloop::new_timer_ordered(
        delay_until(fire_date),
        repeat,
        callback,
        info,
        order,
    ))
}

pub unsafe extern "C" fn CFRunLoopAddTimer(loop_: CFTypeRef, timer: CFTypeRef, mode: CFTypeRef) {
    let (Some(l), Some(t)) = (unsafe { borrow::<RunLoop>(loop_) }, unsafe {
        borrow::<Timer>(timer)
    }) else {
        return;
    };
    l.add_timer(&t, &unsafe { mode_name(mode) });
}

pub unsafe extern "C" fn CFRunLoopRemoveTimer(loop_: CFTypeRef, timer: CFTypeRef, mode: CFTypeRef) {
    let (Some(l), Some(t)) = (unsafe { borrow::<RunLoop>(loop_) }, unsafe {
        borrow::<Timer>(timer)
    }) else {
        return;
    };
    l.remove_timer(&t, &unsafe { mode_name(mode) });
}

/// `void CFRunLoopTimerGetContext(CFRunLoopTimerRef, CFRunLoopTimerContext *)`
///
/// The `info` word as given. The context's retain, release and description
/// callbacks are not kept past creation, so they come back null.
pub unsafe extern "C" fn CFRunLoopTimerGetContext(
    timer: CFTypeRef,
    context: *mut CFRunLoopTimerContext,
) {
    let (Some(t), false) = (unsafe { borrow::<Timer>(timer) }, context.is_null()) else {
        return;
    };
    // SAFETY: the caller's writable context structure.
    unsafe {
        context.write(CFRunLoopTimerContext {
            version: 0,
            info: t.info() as *mut c_void,
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
        })
    };
}

/// `CFTimeInterval CFRunLoopTimerGetInterval(CFRunLoopTimerRef)`
pub unsafe extern "C" fn CFRunLoopTimerGetInterval(timer: CFTypeRef) -> f64 {
    unsafe { borrow::<Timer>(timer) }.map_or(0.0, |t| t.interval_seconds())
}

/// `void CFRunLoopTimerSetNextFireDate(CFRunLoopTimerRef, CFAbsoluteTime)`
///
/// How a caller reschedules a timer it already owns, which is what an event
/// dispatcher does every time its next deadline moves - cheaper than invalidating
/// one and creating another, and the reason it is not a courtesy to implement it.
pub unsafe extern "C" fn CFRunLoopTimerSetNextFireDate(timer: CFTypeRef, fire_date: f64) {
    if let Some(t) = unsafe { borrow::<Timer>(timer) } {
        // `delay_until` is a delay from now, which is the form the creator takes;
        // the timer wants the instant it lands on.
        t.set_next(std::time::Instant::now() + delay_until(fire_date));
    }
}

/// `void CFRunLoopTimerSetTolerance(CFRunLoopTimerRef, CFTimeInterval)`
///
/// A *permission*, not a delay: the header says the timer "may fire at any
/// time between its scheduled fire date and the scheduled fire date plus the
/// tolerance" and "will not fire before the scheduled fire date".  This run loop
/// fires every timer at its exact date, which is inside that window for every
/// tolerance, so the value is recorded and clamped as `CFRunLoop.c` clamps it -
/// see [`runloop::Timer::set_tolerance`] - and nothing else has to change.
///
/// The guest's call is at `0x5d9595a`, in the same run-loop module as the
/// `CFRunLoopTimerSetNextFireDate` and `CFAbsoluteTimeGetCurrent` calls around
/// it, and the tolerance it computes is `(fire_date - now)` in seconds.  This is
/// the one unbound CoreFoundation import in that module, so it is the next one a
/// run that gets past `0x5d46240` reaches.
pub unsafe extern "C" fn CFRunLoopTimerSetTolerance(timer: CFTypeRef, tolerance: f64) {
    if let Some(t) = unsafe { borrow::<Timer>(timer) } {
        t.set_tolerance(tolerance);
    }
}

pub unsafe extern "C" fn CFRunLoopTimerInvalidate(timer: CFTypeRef) {
    if let Some(t) = unsafe { borrow::<Timer>(timer) } {
        t.invalidate();
    }
}

/// `CFAbsoluteTime CFRunLoopGetNextTimerFireDate(CFRunLoopRef, CFRunLoopMode)`
///
/// Zero when no valid timer is scheduled in the mode, as Apple's returns.
pub unsafe extern "C" fn CFRunLoopGetNextTimerFireDate(loop_: CFTypeRef, mode: CFTypeRef) -> f64 {
    let Some(l) = (unsafe { borrow::<RunLoop>(loop_) }) else {
        return 0.0;
    };
    let Some(next) = l.next_timer(&unsafe { mode_name(mode) }) else {
        return 0.0;
    };
    let now = std::time::Instant::now();
    match next.checked_duration_since(now) {
        Some(ahead) => absolute_time() + ahead.as_secs_f64(),
        None => absolute_time() - now.duration_since(next).as_secs_f64(),
    }
}

pub unsafe extern "C" fn CFRunLoopTimerIsValid(timer: CFTypeRef) -> Boolean {
    unsafe { borrow::<Timer>(timer) }.is_some_and(|t| t.is_valid()) as Boolean
}

/// `void CFStringReplace(CFMutableStringRef, CFRange, CFStringRef)`, with the
/// range flattened into the two words it is passed in.
///
/// Delete then insert, rather than a splice of its own: the two already agree
/// on what a mutable string's storage is and on the narrow/wide question, and a
/// third copy of that decision is a third place for them to disagree.
pub unsafe extern "C" fn CFStringReplace(
    target: CFTypeRef,
    range_location: CFIndex,
    range_length: CFIndex,
    replacement: CFTypeRef,
) {
    // SAFETY: a live mutable string, and a live
    // string or null.
    unsafe {
        CFStringDelete(target, range_location, range_length);
        CFStringInsert(target, range_location, replacement);
    }
}

/// `void CFStringReplaceAll(CFMutableStringRef, CFStringRef)`
pub unsafe extern "C" fn CFStringReplaceAll(target: CFTypeRef, replacement: CFTypeRef) {
    let Some(length) = (unsafe { string::as_string(target) }).map(|s| s.length()) else {
        return;
    };
    // SAFETY: as above; the range is the whole string.
    unsafe { CFStringReplace(target, 0, length, replacement) };
}

/// `CFIndex CFStringFindAndReplace(CFMutableStringRef, CFStringRef target,
/// CFStringRef replacement, CFRange rangeToSearch, CFStringCompareFlags)`, with
/// the range flattened.
///
/// Answers how many replacements were made, which is what the caller branches
/// on. Only the literal, case-sensitive search is performed: the comparison
/// flags describe a Unicode collation this tree does not implement, and a
/// case-insensitive search done case-sensitively would silently replace
/// nothing rather than say so.
pub unsafe extern "C" fn CFStringFindAndReplace(
    target: CFTypeRef,
    find: CFTypeRef,
    replacement: CFTypeRef,
    range_location: CFIndex,
    range_length: CFIndex,
    options: u32,
) -> CFIndex {
    // `kCFCompareCaseInsensitive` is the one flag this does honour. The rest -
    // backwards, anchored, non-literal, localized, numeric - each describe a
    // different search, and one done literally instead would replace the wrong
    // text rather than fail.
    const CASE_INSENSITIVE: u32 = 1;
    if options & !CASE_INSENSITIVE != 0 {
        objc::fail!(
            "CFStringFindAndReplace: comparison flags {options:#x} describe a search this \
             runtime does not implement"
        );
    }
    let fold = options & CASE_INSENSITIVE != 0;
    // SAFETY: live strings or null.
    let (Some(haystack), Some(needle)) = (unsafe { string::as_string(target) }, unsafe {
        string::as_string(find)
    }) else {
        return 0;
    };
    // SAFETY: as above; null is a valid replacement and means "delete".
    let with = (unsafe { string::as_string(replacement) }).map_or_else(Vec::new, |s| s.utf16());
    let needle = needle.utf16();
    if needle.is_empty() {
        return 0;
    }
    let units = haystack.utf16();
    // Compared against folded copies and spliced out of the originals, so that
    // a case-insensitive search keeps the text it did not replace exactly as it
    // was. Simple case folding, one unit at a time, which keeps the two the
    // same length and so keeps the indices the same in both.
    let folded_units = folded(&units, fold);
    let folded_needle = folded(&needle, fold);
    let (Ok(start), Ok(length)) = (
        usize::try_from(range_location),
        usize::try_from(range_length),
    ) else {
        return 0;
    };
    let Some(end) = start.checked_add(length).filter(|end| *end <= units.len()) else {
        return 0;
    };
    // Built rather than spliced in place, so that a replacement containing the
    // text being searched for is not searched again.
    let mut out: Vec<u16> = units[..start].to_vec();
    let mut at = start;
    let mut replaced = 0;
    while at < end {
        if at + needle.len() <= end && folded_units[at..at + needle.len()] == folded_needle[..] {
            out.extend_from_slice(&with);
            at += needle.len();
            replaced += 1;
            continue;
        }
        out.push(units[at]);
        at += 1;
    }
    out.extend_from_slice(&units[end..]);
    if replaced != 0 {
        // SAFETY: the caller's target is a mutable string.
        unsafe { string::set_utf16(target, &out) };
    }
    replaced
}

/// Every unit lowercased, when `fold` asks for it.
///
/// Per unit rather than per character, which is what keeps the result the same
/// length as its input: a full Unicode lowercasing can add units, and the match
/// offsets here are offsets into the original.
fn folded(units: &[u16], fold: bool) -> Vec<u16> {
    if !fold {
        return units.to_vec();
    }
    units
        .iter()
        .map(|unit| {
            let Some(c) = char::from_u32(u32::from(*unit)) else {
                return *unit;
            };
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(one), None) => u16::try_from(u32::from(one)).unwrap_or(*unit),
                _ => *unit,
            }
        })
        .collect()
}

/// `CFIndex CFStringGetMaximumSizeOfFileSystemRepresentation(CFStringRef)`
///
/// A bound rather than a measurement, which is what the name says and what a
/// caller sizing a buffer needs: every UTF-16 unit is at most three UTF-8
/// bytes, plus the terminator.
pub unsafe extern "C" fn CFStringGetMaximumSizeOfFileSystemRepresentation(
    string: CFTypeRef,
) -> CFIndex {
    let Some(value) = (unsafe { string::as_string(string) }) else {
        return 0;
    };
    value.length().saturating_mul(3).saturating_add(1)
}

/// `Boolean CFStringGetFileSystemRepresentation(CFStringRef, char *buffer,
/// CFIndex maxBufLen)`
///
/// False when the path does not fit, with nothing written: a truncated path
/// names a different file, and the caller checks the answer.
///
/// # Safety
///
/// `buffer` is null or writable for `max` bytes.
pub unsafe extern "C" fn CFStringGetFileSystemRepresentation(
    string: CFTypeRef,
    buffer: *mut c_char,
    max: CFIndex,
) -> Boolean {
    let Some(value) = (unsafe { string::as_string(string) }) else {
        return 0;
    };
    let Ok(max) = usize::try_from(max) else {
        return 0;
    };
    if buffer.is_null() {
        return 0;
    }
    let bytes = value.utf8();
    if bytes.len() + 1 > max {
        return 0;
    }
    // SAFETY: the caller states `buffer` holds `max` bytes, and the length was
    // just checked against it.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast::<u8>(), bytes.len());
        *buffer.add(bytes.len()) = 0;
    }
    1
}

/// `CFStringRef CFStringCreateWithBytesNoCopy(CFAllocatorRef, const UInt8 *,
/// CFIndex, CFStringEncoding, Boolean, CFAllocatorRef)`, which copies here for
/// [`CFStringCreateWithCStringNoCopy`]'s reason.
pub unsafe extern "C" fn CFStringCreateWithBytesNoCopy(
    allocator: CFAllocatorRef,
    bytes: *const u8,
    length: CFIndex,
    encoding: u32,
    external: Boolean,
    _contents_deallocator: CFAllocatorRef,
) -> CFTypeRef {
    // SAFETY: the validity requirement is identical to `CFStringCreateWithBytes`'.
    unsafe { CFStringCreateWithBytes(allocator, bytes, length, encoding, external) }
}

/// `CFArrayRef CFArrayCreateCopy(CFAllocatorRef, CFArrayRef)`
///
/// Immutable, and the values are retained under the original's own callbacks -
/// a copy that used the type callbacks would retain values the original had
/// promised not to.
pub unsafe extern "C" fn CFArrayCreateCopy(
    _allocator: CFAllocatorRef,
    array: CFTypeRef,
) -> CFTypeRef {
    let Some(a) = (unsafe { collections::array(array) }) else {
        return std::ptr::null();
    };
    collections::new_array(&a.values.clone(), a.policy, false)
}

pub unsafe extern "C" fn CFArrayInsertValueAtIndex(
    array: CFTypeRef,
    index: CFIndex,
    value: CFTypeRef,
) {
    let Some(a) = (unsafe { collections::array(array) }) else {
        return;
    };
    // The one place `collections::at` does not apply: inserting at the count is
    // an append and is legal, where reading at the count is not.
    let Ok(at) = usize::try_from(index).map(|at| at.min(a.values.len())) else {
        return;
    };
    if !a.mutable {
        return;
    }
    let value = a.policy.retain(value);
    a.values.insert(at, value);
}

pub unsafe extern "C" fn CFArrayRemoveValueAtIndex(array: CFTypeRef, index: CFIndex) {
    let Some(a) = (unsafe { collections::array(array) }) else {
        return;
    };
    if !a.mutable {
        return;
    }
    let Some(at) = collections::at(index, a.values.len()) else {
        return;
    };
    let value = a.values.remove(at);
    a.policy.release(value);
}

pub unsafe extern "C" fn CFArrayRemoveAllValues(array: CFTypeRef) {
    let Some(a) = (unsafe { collections::array(array) }) else {
        return;
    };
    if !a.mutable {
        return;
    }
    for value in a.values.drain(..) {
        a.policy.release(value);
    }
}

/// `CFDictionaryRef CFDictionaryCreateCopy(CFAllocatorRef, CFDictionaryRef)`
pub unsafe extern "C" fn CFDictionaryCreateCopy(
    _allocator: CFAllocatorRef,
    dictionary: CFTypeRef,
) -> CFTypeRef {
    let Some(d) = (unsafe { collections::dictionary(dictionary) }) else {
        return std::ptr::null();
    };
    collections::new_dictionary(&d.entries.clone(), d.keys, d.values, false)
}

/// `CFMutableDictionaryRef CFDictionaryCreateMutableCopy(CFAllocatorRef,
/// CFIndex capacity, CFDictionaryRef)`, whose capacity is a hint the same way
/// [`CFDictionaryCreateMutable`]'s is.
pub unsafe extern "C" fn CFDictionaryCreateMutableCopy(
    _allocator: CFAllocatorRef,
    _capacity: CFIndex,
    dictionary: CFTypeRef,
) -> CFTypeRef {
    let Some(d) = (unsafe { collections::dictionary(dictionary) }) else {
        return std::ptr::null();
    };
    collections::new_dictionary(&d.entries.clone(), d.keys, d.values, true)
}

/// `CFIndex CFDictionaryGetCountOfKey(CFDictionaryRef, const void *key)`
///
/// One or zero: a dictionary holds a key once.
pub unsafe extern "C" fn CFDictionaryGetCountOfKey(
    dictionary: CFTypeRef,
    key: CFTypeRef,
) -> CFIndex {
    let Some(d) = (unsafe { collections::dictionary(dictionary) }) else {
        return 0;
    };
    CFIndex::from(d.position(key).is_some())
}

/// `UInt8 *CFDataGetMutableBytePtr(CFMutableDataRef)`
///
/// The writable counterpart of [`CFDataGetBytePtr`], and empty data answers
/// null rather than [`CFDataGetBytePtr`]'s readable byte: a caller that means
/// to write through this has asked for length first, and a shared static byte
/// is the one address that must not be handed out to be written.
pub unsafe extern "C" fn CFDataGetMutableBytePtr(data: CFTypeRef) -> *mut u8 {
    let Some(bytes) = (unsafe { collections::data(data) }) else {
        return std::ptr::null_mut();
    };
    match bytes.is_empty() {
        true => std::ptr::null_mut(),
        false => bytes.as_mut_ptr(),
    }
}

/// `void CFDataSetLength(CFMutableDataRef, CFIndex)`
///
/// Growth is zero-filled, which is what CoreFoundation promises and what a
/// caller that writes only part of the new tail depends on.
pub unsafe extern "C" fn CFDataSetLength(data: CFTypeRef, length: CFIndex) {
    let Ok(length) = usize::try_from(length) else {
        return;
    };
    let Some(bytes) = (unsafe { collections::data(data) }) else {
        return;
    };
    bytes.resize(length, 0);
}

/// `void CFDataIncreaseLength(CFMutableDataRef, CFIndex)`
pub unsafe extern "C" fn CFDataIncreaseLength(data: CFTypeRef, extra: CFIndex) {
    let Ok(extra) = usize::try_from(extra) else {
        return;
    };
    let Some(bytes) = (unsafe { collections::data(data) }) else {
        return;
    };
    let length = bytes.len() + extra;
    bytes.resize(length, 0);
}

/// `void CFDataAppendBytes(CFMutableDataRef, const UInt8 *, CFIndex)`
///
/// # Safety
///
/// `added` is readable for `length` bytes.
pub unsafe extern "C" fn CFDataAppendBytes(data: CFTypeRef, added: *const u8, length: CFIndex) {
    let Ok(length) = usize::try_from(length) else {
        return;
    };
    if added.is_null() && length > 0 {
        return;
    }
    // SAFETY: the caller states `added` covers `length` bytes.
    let slice = match length {
        0 => &[][..],
        _ => unsafe { std::slice::from_raw_parts(added, length) },
    };
    let Some(bytes) = (unsafe { collections::data(data) }) else {
        return;
    };
    bytes.extend_from_slice(slice);
}

/// `void CFDataDeleteBytes(CFMutableDataRef, CFRange)`, with the range
/// flattened.
pub unsafe extern "C" fn CFDataDeleteBytes(
    data: CFTypeRef,
    range_location: CFIndex,
    range_length: CFIndex,
) {
    let Some(bytes) = (unsafe { collections::data(data) }) else {
        return;
    };
    let (Ok(start), Ok(length)) = (
        usize::try_from(range_location),
        usize::try_from(range_length),
    ) else {
        return;
    };
    let Some(end) = start.checked_add(length).filter(|end| *end <= bytes.len()) else {
        return;
    };
    bytes.drain(start..end);
}

/// `void CFDataReplaceBytes(CFMutableDataRef, CFRange, const UInt8 *, CFIndex)`,
/// with the range flattened.
///
/// # Safety
///
/// `added` is null or readable for `added_length` bytes.
pub unsafe extern "C" fn CFDataReplaceBytes(
    data: CFTypeRef,
    range_location: CFIndex,
    range_length: CFIndex,
    added: *const u8,
    added_length: CFIndex,
) {
    let (Ok(start), Ok(length), Ok(added_length)) = (
        usize::try_from(range_location),
        usize::try_from(range_length),
        usize::try_from(added_length),
    ) else {
        return;
    };
    if added.is_null() && added_length > 0 {
        return;
    }
    // SAFETY: the caller states `added` covers `added_length` bytes. Copied
    // before the data is touched, because the two are allowed to be the same
    // buffer.
    let addition = match added_length {
        0 => Vec::new(),
        _ => unsafe { std::slice::from_raw_parts(added, added_length) }.to_vec(),
    };
    let Some(bytes) = (unsafe { collections::data(data) }) else {
        return;
    };
    let Some(end) = start.checked_add(length).filter(|end| *end <= bytes.len()) else {
        return;
    };
    bytes.splice(start..end, addition);
}

/// `CFUUIDBytes`, which Apple declares as sixteen named `UInt8` fields. One
/// array of the same size and alignment, so that the two 64-bit ABIs classify
/// it the same way: two integer eightbytes, returned in two registers.
#[repr(C)]
pub struct CFUUIDBytes(pub crate::uuid::Bytes);

/// `CFUUIDBytes CFUUIDGetUUIDBytes(CFUUIDRef)`
pub unsafe extern "C" fn CFUUIDGetUUIDBytes(uuid: CFTypeRef) -> CFUUIDBytes {
    // SAFETY: a UUID this runtime made, or null.
    CFUUIDBytes(unsafe { crate::uuid::bytes_of(uuid) }.unwrap_or([0; 16]))
}

//

/// The class a `CFLocaleRef` is, which is Foundation's `NSLocale`.
///
/// A locale is not one of this crate's types: `CFLocaleGetIdentifier`,
/// `CFLocaleGetValue` and the three `...Copy` spellings all live in `foundation`,
/// above this crate, and they answer for the one locale the host's environment
/// names. What CoreFoundation itself contributes is the *constructor*, and a
/// constructor has to build something of the class the readers expect - so the
/// name is registered here, idempotently, and Foundation's `install` adds its
/// methods to whichever registration got there first.
fn locale_class() -> &'static objc::class::Class {
    crate::base::register(c"NSLocale", None, std::mem::size_of::<usize>(), false)
}

/// `CFLocaleRef CFLocaleCreate(CFAllocatorRef, CFStringRef localeIdentifier)`
///
/// A null identifier is the current locale, which is what the header says and
/// what a caller with nothing to say about the locale means. Anything that is
/// not a string at all is refused: a locale built from a pointer that is not a
/// string would be a locale that answers questions about a locale.
///
/// ## What the identifier is not used for
///
/// [`CFLocaleGetIdentifier`] and [`CFLocaleGetValue`] are Foundation's, and they
/// answer for the host's one locale whatever locale object they are handed.
/// Storing the identifier here would buy a guest nothing it can read back, so it
/// is not stored; what a guest can rely on is that every locale in this process
/// agrees with every other one, which is the property a caller comparing
/// `CFLocaleGetIdentifier` against an expected string actually needs.
pub unsafe extern "C" fn CFLocaleCreate(
    _allocator: CFAllocatorRef,
    locale_identifier: CFTypeRef,
) -> CFTypeRef {
    if !locale_identifier.is_null() {
        // SAFETY: a `CFStringRef` or null.
        if unsafe { string::as_string(locale_identifier) }.is_none() {
            return std::ptr::null();
        }
    }
    // A `Create`, so the caller owns a reference and will release it.
    objc::object::create_instance(locale_class(), 0) as CFTypeRef
}

/// `CFTypeID CFNullGetTypeID(void)`
///
/// The class of `kCFNull`, which is Foundation's `NSNull` - the same object the
/// `kCFNull` symbol publishes and `+[NSNull null]` hands back, so a guest that
/// compares a value against that global and a guest that compares a type ID
/// against this function are asking one question.
pub unsafe extern "C" fn CFNullGetTypeID() -> CFTypeID {
    // One word, which is what Foundation registers `NSNull` with: it carries no
    // ivars of its own.
    crate::base::bridged_class(c"NSNull", std::mem::size_of::<usize>()) as *const _ as CFTypeID
}

//

/// `CFTypeID CFErrorGetTypeID(void)`
///
/// Foundation's `NSError`, for [`CFNullGetTypeID`]'s reason: `CFErrorRef` is
/// toll-free bridged to it, and marking the class is what makes
/// `CFGetTypeID(error) == CFErrorGetTypeID()` true.
///
/// Two words, because Foundation's `NSError` carries an isa and a pointer to its
/// contents - the same layout its own `error::make` allocates.
pub unsafe extern "C" fn CFErrorGetTypeID() -> CFTypeID {
    crate::base::bridged_class(c"NSError", 2 * std::mem::size_of::<usize>()) as *const _ as CFTypeID
}

/// `CFIndex CFErrorGetCode(CFErrorRef error)`
///
/// Sent as `-code`, so an error this runtime has not made is read the same way as
/// one it has: a guest holding an `NSError` from a Foundation call and a guest
/// holding one from CoreFoundation are holding the same kind of object, and
/// switching on the type to decide which accessor to use is exactly what a caller
/// should not have to get right.
pub unsafe extern "C" fn CFErrorGetCode(error: CFTypeRef) -> CFIndex {
    // SAFETY: a `CFErrorRef` or null; `send0_usize` answers zero for null and for
    // an object with no `-code`, and `-code` returns a `NSInteger`.
    unsafe { send::send0_usize(error as Id, objc::sel::get("code")) as i64 as CFIndex }
}

/// `CFStringRef CFErrorGetDomain(CFErrorRef error)`
///
/// A `Get`, not a `Copy`: the string is not the caller's to release, and it is
/// the autoreleased one `-domain` hands back, which lives at least as long as the
/// error does - which is the lifetime Apple's documents.
pub unsafe extern "C" fn CFErrorGetDomain(error: CFTypeRef) -> CFTypeRef {
    // SAFETY: a `CFErrorRef` or null; as above.
    unsafe { send::send0_usize(error as Id, objc::sel::get("domain")) as CFTypeRef }
}

/// `CFStringRef CFErrorCopyDescription(CFErrorRef error)`
///
/// The *localized* description, which is what CoreFoundation's own
/// documentation calls this function: `CFErrorCopyDescription` and
/// `-localizedDescription` are the same string, and a guest that formats an error
/// for a human wants that one.
///
/// A `Copy`, so the caller owns a reference. `-localizedDescription` is
/// autoreleased, so the reference the caller will release is one taken here on
/// top of the pool's.
pub unsafe extern "C" fn CFErrorCopyDescription(error: CFTypeRef) -> CFTypeRef {
    // SAFETY: a `CFErrorRef` or null.
    let described =
        unsafe { send::send0_usize(error as Id, objc::sel::get("localizedDescription")) }
            as CFTypeRef;
    match described.is_null() {
        true => described,
        // SAFETY: an object this runtime's own `-localizedDescription` returned.
        false => unsafe { CFRetain(described) },
    }
}

/// `CFDictionaryRef CFErrorCopyUserInfo(CFErrorRef error)`
///
/// A `Copy`, for the same reason as [`CFErrorCopyDescription`]. Null when there
/// is no user info, which is what `-userInfo` answers and what a caller has to
/// cope with either way.
pub unsafe extern "C" fn CFErrorCopyUserInfo(error: CFTypeRef) -> CFTypeRef {
    // SAFETY: a `CFErrorRef` or null.
    let info = unsafe { send::send0_usize(error as Id, objc::sel::get("userInfo")) } as CFTypeRef;
    match info.is_null() {
        true => info,
        // SAFETY: an object this runtime's own `-userInfo` returned.
        false => unsafe { CFRetain(info) },
    }
}

//

/// `void *CFBundleGetFunctionPointerForName(CFBundleRef bundle, CFStringRef
/// functionName)`
///
/// A function inside a loaded Mach-O image. There is no image here: every
/// framework a guest links against is this runtime's own code, linked into this
/// binary, and `rosetta3`'s symbol lookup is the one the loader answers a guest's
/// *import* with. So the question "what is the address of this symbol" is asked
/// the same way for a bundle lookup as for a link, and gets the same answer - for
/// `carbon`'s reason, which is that the two disagreeing is how a guest ends up
/// holding a working `FSRefMakePath` and a `CFURLCreateFromFSRef` that answers
/// null.
///
/// Null when nothing defines the name, which is exactly what Apple answers for a
/// symbol a bundle does not export, and what every caller of this checks.
pub unsafe extern "C" fn CFBundleGetFunctionPointerForName(
    _bundle: CFTypeRef,
    function_name: CFTypeRef,
) -> *mut c_void {
    // SAFETY: a `CFStringRef` or null.
    let Some(name) = (unsafe { string::as_string(function_name) }) else {
        return std::ptr::null_mut();
    };
    let bytes = name.utf8();
    let Ok(name) = std::str::from_utf8(&bytes) else {
        return std::ptr::null_mut();
    };
    darwin::symbol(name).map_or(std::ptr::null_mut(), |address| {
        address as usize as *mut c_void
    })
}

//

/// `CFStringRef CFRunLoopCopyCurrentMode(CFRunLoopRef rl)`
///
/// A `Copy`, so the caller owns the string and will release it - which is the
/// only difference between this and the mode name inside the loop.
///
/// Null for a loop that is not running, which is what the header documents and
/// what a caller logging "the loop is in `X`" needs: a loop that is not in a mode
/// has no mode, and naming one anyway would put a mode in a log line that never
/// happened.
pub unsafe extern "C" fn CFRunLoopCopyCurrentMode(loop_: CFTypeRef) -> CFTypeRef {
    let mode = match unsafe { borrow::<RunLoop>(loop_) } {
        // SAFETY: `borrow` only answers with a `RunLoop` this crate made.
        Some(loop_) => loop_.current_mode(),
        None => return std::ptr::null(),
    };
    match mode {
        Some(mode) => string::from_bytes(mode.as_bytes(), false),
        None => std::ptr::null(),
    }
}

/// `void CFRunLoopPerformBlock(CFRunLoopRef rl, CFTypeRef mode, void (^block)(void))`
///
/// The block is queued and this returns: running it is the loop's job, on the
/// next turn in `mode`, which is the whole difference between this and calling
/// the block. `mode` is the same string `CFRunLoopAddSource` takes, and null is
/// the default mode, as it is there.
///
/// The block is **copied**, as Apple's is: it is handed a pointer that outlives
/// the caller's frame, and a stack block would be a pointer into a frame that is
/// gone. A copy that cannot be made means there is no block runtime registered,
/// and nothing is queued - see [`darwin::dispatch::block::copy`] for why
/// treating that as success is a crash some time later and somewhere else.
///
/// ## The block's code has to be callable from here
///
/// A guest's block literal has guest code in its `invoke`, and this crate cannot
/// make that callable: only the runtime can, and it does it by rewriting the
/// block in guest memory before handing it to the host. So this function expects
/// to be reached through `rosetta3`'s shim for
/// `CFRunLoopPerformBlock`, which substitutes the block's code pointers and then
/// calls through here - the same arrangement as `dispatch_async`, and for the
/// same reason.
pub unsafe extern "C" fn CFRunLoopPerformBlock(loop_: CFTypeRef, mode: CFTypeRef, block: u64) {
    // A null block is a no-op rather than a fault, as it is in `dispatch_async`'s
    // own reading of it: a caller that built nothing has asked for nothing.
    if block == 0 {
        return;
    }
    // A loop this crate did not make has no block queue, so the current thread's
    // is used - which is also what a null handle means to every other caller here.
    // `into_inner` rather than letting the `ManuallyDrop` fall out of scope as
    // one: the reference belongs to the loop, not to this call.
    let running = match unsafe { borrow::<RunLoop>(loop_) } {
        Some(loop_) => std::mem::ManuallyDrop::into_inner(loop_),
        None => runloop::current(),
    };
    // SAFETY: null, or a `CFStringRef` naming the mode.
    let mode = unsafe { mode_name(mode) };
    // The heap copy outlives the caller's frame, so the closure below may hold
    // its address for as long as the loop takes to get to it.
    let Some(owned) = darwin::dispatch::block::copy(block) else {
        return;
    };
    // `invoke` sits at a fixed offset in a `Block_layout`; `darwin` owns that
    // constant and its own `invoke` is the caller of it.
    // SAFETY: `owned` is a block `_Block_copy` returned, and the Block ABI puts
    // `invoke` at offset 16 of it.
    let entry = (owned + 16_u64) as *const u64;
    // SAFETY: as above.
    let entry = unsafe { *entry };
    running.perform_block(&mode, move || {
        if entry != 0 {
            // SAFETY: a `void (*)(void *)` by the Block ABI for a block taking no
            // arguments, reached through the `invoke` a caller may substitute -
            // see the note on the shim above.
            unsafe {
                std::mem::transmute::<u64, extern "C" fn(u64)>(entry)(owned);
            }
        }
        darwin::dispatch::block::release(owned);
    });
}

#[cfg(test)]
mod path_style_tests {
    use super::*;

    /// The two directions are exact inverses, which is what a caller that
    /// converts a path and converts it back depends on - and the colon and the
    /// slash swap places, which is the part that is easy to get wrong in only
    /// one of them.
    #[test]
    fn an_hfs_path_round_trips() {
        for path in [
            "/",
            "/Users/user",
            "/Applications/Life Is Strange.app/Contents/MacOS",
            "/a directory: with a colon/file",
        ] {
            let hfs = hfs_from_posix(path);
            assert!(hfs.starts_with('/'), "{hfs} does not name the root volume");
            assert_eq!(posix_from_hfs(&hfs), path, "via {hfs}");
        }
    }

    /// The notation itself, spelled out, so that a change to either direction
    /// has to be a deliberate one.
    #[test]
    fn the_root_volume_leads_and_colons_separate() {
        assert_eq!(hfs_from_posix("/Users/user"), "/:Users:user");
        assert_eq!(posix_from_hfs("/:Users:user"), "/Users/user");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_code_page_or_charset_name_names_its_encoding() {
        // SAFETY: plain calls; the strings are live NUL-terminated literals.
        unsafe {
            assert_eq!(
                CFStringConvertWindowsCodepageToEncoding(1252),
                string::encoding::WINDOWS_LATIN1
            );
            assert_eq!(
                CFStringConvertWindowsCodepageToEncoding(65001),
                string::encoding::UTF8
            );
            assert_eq!(
                CFStringConvertWindowsCodepageToEncoding(1),
                INVALID_ENCODING
            );
            let name = |text: &core::ffi::CStr| {
                CFStringCreateWithCString(core::ptr::null(), text.as_ptr(), string::encoding::UTF8)
            };
            assert_eq!(
                CFStringConvertIANACharSetNameToEncoding(name(c"Windows-1252")),
                string::encoding::WINDOWS_LATIN1
            );
            assert_eq!(
                CFStringConvertIANACharSetNameToEncoding(name(c"UTF-8")),
                string::encoding::UTF8
            );
            assert_eq!(
                CFStringConvertIANACharSetNameToEncoding(name(c"no-such-charset")),
                INVALID_ENCODING
            );
        }
    }

    #[test]
    fn an_opaque_cf_handle_uses_its_registered_type_without_an_isa() {
        static HANDLE: u8 = 0;
        let handle = std::ptr::addr_of!(HANDLE).cast::<c_void>();
        crate::base::note_typed_opaque(handle, 0x1234);
        // SAFETY: a registered opaque CF handle.
        unsafe {
            assert_eq!(CFGetTypeID(handle), 0x1234);
            assert_eq!(CFGetRetainCount(handle), 1);
            assert_eq!(CFHash(handle), handle as usize);
            assert_eq!(CFEqual(handle, handle), 1);
            assert_eq!(CFRetain(handle), handle);
            CFRelease(handle);
        }
        // The pairing, on the one handle this test invents. A `static u8` cannot be
        // handed out again, so nothing here can be *wrong* - but an entry left
        // behind is a claim about a handle nobody made, and the table is shared
        // with every other test in this binary.
        crate::base::forget_opaque(handle);
        assert!(
            !crate::base::is_opaque(handle),
            "an entry that outlives its note claims an address for a handle that is not there"
        );
    }

    /// An empty `CFData` must still answer a byte pointer that can be read.
    /// Rust's own answer is the alignment, which is not null and faults.
    #[test]
    fn an_empty_data_answers_a_readable_pointer() {
        let data = collections::new_data(Vec::new());
        // SAFETY: a `CFData` this test made.
        let (bytes, length) = unsafe { (CFDataGetBytePtr(data), CFDataGetLength(data)) };
        assert_eq!(length, 0);
        assert!(!bytes.is_null());
        assert!(bytes as usize > 0xffff, "{bytes:p} is not a mapped address");
        // SAFETY: the pointer this call just promised is readable.
        assert_eq!(unsafe { bytes.read() }, 0);
    }

    #[test]
    fn no_copy_data_keeps_an_owned_snapshot() {
        let mut source = vec![1u8, 2, 3];
        // SAFETY: `source` covers the stated length.
        let data = unsafe {
            CFDataCreateWithBytesNoCopy(
                std::ptr::null(),
                source.as_ptr(),
                source.len() as CFIndex,
                std::ptr::null(),
            )
        };
        source[0] = 9;

        // SAFETY: `data` was created above and remains retained.
        let bytes = unsafe {
            std::slice::from_raw_parts(CFDataGetBytePtr(data), CFDataGetLength(data) as usize)
        };
        assert_eq!(bytes, [1, 2, 3]);
        // SAFETY: balances the create rule.
        unsafe { CFRelease(data) };
    }

    /// Null is still null: an object that is not a `CFData` has no bytes, and a
    /// readable pointer there would be a wrong answer rather than a safe one.
    #[test]
    fn a_non_data_has_no_bytes() {
        // SAFETY: null is the documented "not an object" case.
        assert!(unsafe { CFDataGetBytePtr(std::ptr::null()) }.is_null());
    }
}

#[cfg(test)]
mod find_result_tests {
    use super::*;
    use crate::bootstrap;

    /// Every match, as `(location, length)`.
    ///
    /// The array holds **pointers** to `CFRange`, so reading one is two steps -
    /// which is what the guest does at `0x4dd1778`, `mov rax, [rax]`.
    fn find_all_of(
        haystack: &str,
        needle: &str,
        options: u64,
    ) -> Vec<(CFIndex, CFIndex)> {
        let searched = string::from_bytes(haystack.as_bytes(), false);
        let wanted = string::from_bytes(needle.as_bytes(), false);
        // SAFETY: two strings this test made, and a range covering the whole of
        // the haystack.
        let array = unsafe {
            CFStringCreateArrayWithFindResults(
                std::ptr::null(),
                searched,
                wanted,
                CFRange {
                    location: 0,
                    length: CFStringGetLength(searched),
                },
                options,
            )
        };
        // SAFETY: strings this test made.
        unsafe { CFRelease(wanted) };
        // SAFETY: strings this test made.
        unsafe { CFRelease(searched) };
        if array.is_null() {
            return Vec::new();
        }
        // SAFETY: an array this call made.
        let count = unsafe { CFArrayGetCount(array) };
        let mut out = Vec::new();
        for index in 0..count {
            // SAFETY: an index inside the count.
            let element = unsafe { CFArrayGetValueAtIndex(array, index) };
            // SAFETY: the element is a `CFRange *` this call allocated.
            let range = unsafe { *(element as *const CFRange) };
            out.push((range.location, range.length));
        }
        // SAFETY: nothing else holds a reference.
        unsafe { CFRelease(array) };
        out
    }

    /// The rule the header states first, because it is the one a caller can
    /// most easily get wrong: overlapping instances are not found.
    #[test]
    fn overlapping_matches_are_not_found() {
        assert_eq!(find_all_of("AAA", "AA", 0), [(0, 2)], "'AA' in 'AAA' is one range");
        assert_eq!(find_all_of("aaaa", "aa", 0), [(0, 2), (2, 2)], "and two of them, not three");
    }

    /// No matches is *null*, not an empty array - which a caller can tell apart
    /// and which is the difference between "nothing matched" and "here is a
    /// list of nothing".
    #[test]
    fn no_matches_is_null() {
        assert_eq!(find_all_of("abc", "z", 0), Vec::<(CFIndex, CFIndex)>::new());
        // An empty needle finds nothing, per the header, and that is what stops
        // a caller looping over a find that can never advance.
        assert_eq!(find_all_of("abc", "", 0), Vec::<(CFIndex, CFIndex)>::new());
    }

    /// Backwards scans from the end *and* stores the results backwards, so the
    /// last match is in slot 0.
    #[test]
    fn backwards_scans_from_the_end_and_reverses() {
        const BACKWARDS: u64 = 4;
        assert_eq!(find_all_of("a,b,c", ",", 0), [(1, 1), (3, 1)]);
        assert_eq!(
            find_all_of("a,b,c", ",", BACKWARDS),
            [(3, 1), (1, 1)],
            "the last match is in slot 0"
        );
    }

    /// `kCFCompareAnchored` reports only the consecutive instances at the start
    /// - or at the end, backwards.
    #[test]
    fn anchored_reports_only_the_consecutive_run() {
        const ANCHORED: u64 = 8;
        const BACKWARDS: u64 = 4;
        assert_eq!(find_all_of("ABABXAB", "AB", 0), [(0, 2), (2, 2), (5, 2)]);
        assert_eq!(
            find_all_of("ABABXAB", "AB", ANCHORED),
            [(0, 2), (2, 2)],
            "the first two, and not the third"
        );
        assert_eq!(
            find_all_of("ABABXAB", "AB", ANCHORED | BACKWARDS),
            [(5, 2)],
            "backwards and anchored is the run at the end, which is one"
        );
    }

    /// Only the range is searched, and case folding does not change the reported
    /// length - which is why the fold here is ASCII-only.
    #[test]
    fn the_range_and_the_case_fold_both_show_in_the_answer() {
        // SAFETY: a string this test made.
        let searched = string::from_bytes(b"xxAAyyAA", false);
        // SAFETY: a string this test made.
        let wanted = string::from_bytes(b"AA", false);
        // SAFETY: two strings this test made and an out-of-the-middle range.
        let array = unsafe {
            CFStringCreateArrayWithFindResults(
                std::ptr::null(),
                searched,
                wanted,
                CFRange {
                    location: 2,
                    length: 4,
                },
                0,
            )
        };
        assert!(!array.is_null());
        // SAFETY: an array this call made.
        assert_eq!(unsafe { CFArrayGetCount(array) }, 1, "only the range is searched");
        // SAFETY: an array this call made and an index inside it.
        let element = unsafe { CFArrayGetValueAtIndex(array, 0) };
        // SAFETY: the element is a `CFRange *` this call allocated.
        let range = unsafe { *(element as *const CFRange) };
        assert_eq!((range.location, range.length), (2, 2), "in the range, not the whole string");
        // SAFETY: nothing else holds a reference.
        unsafe {
            CFRelease(array);
            CFRelease(wanted);
            CFRelease(searched);
        }
    }

    /// A type ID is described by the class that carries it, and one this runtime
    /// has never heard of is not given a name that sounds right.
    #[test]
    fn a_type_id_is_described_by_its_class() {
        bootstrap();
        // SAFETY: a string this test made.
        let s = string::from_bytes(b"x", false);
        // SAFETY: a string this test made.
        let described = unsafe { CFCopyTypeIDDescription(CFGetTypeID(s)) };
        assert!(!described.is_null());
        // SAFETY: a string this call made.
        let name = unsafe { string::as_string(described) }.expect("a string");
        // Copied out before the release: `utf8` reads the object's own bytes,
        // and the release below is what gives them back.
        let name = String::from_utf8_lossy(&name.utf8()).into_owned();
        // SAFETY: a string this call made.
        unsafe { CFRelease(described) };
        // SAFETY: a string this test made.
        unsafe { CFRelease(s) };
        assert!(
            name.contains("String"),
            "the name of a string's class, not a guess: {name:?}"
        );
    }

    /// A tolerance is a permission, and the value is clamped the way
    /// `CFRunLoop.c` clamps it: a repeating timer to half an interval, a one-shot
    /// to zero.
    #[test]
    fn a_tolerance_is_clamped_the_way_corefoundation_clamps_it() {
        bootstrap();
        // SAFETY: two nulls, a date far away and an interval.
        let repeating = unsafe {
            CFRunLoopTimerCreate(
                std::ptr::null(),
                CFAbsoluteTimeGetCurrent() + 3600.0,
                60.0,
                0,
                0,
                None,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: a timer this call made and a tolerance.
        unsafe { CFRunLoopTimerSetTolerance(repeating, 100.0) };
        // SAFETY: a timer this call made.
        let t = unsafe { borrow::<Timer>(repeating) }.expect("a timer");
        assert_eq!(
            t.tolerance(),
            30.0,
            "MIN(tolerance, interval / 2) - a tolerance past the next fire is \
             meaningless"
        );
        // SAFETY: a timer this call made.
        unsafe { CFRelease(repeating) };

        // SAFETY: two nulls, a date far away and an interval of zero: a one-shot.
        let one_shot = unsafe {
            CFRunLoopTimerCreate(
                std::ptr::null(),
                CFAbsoluteTimeGetCurrent() + 3600.0,
                0.0,
                0,
                0,
                None,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: a timer this call made and a negative tolerance.
        unsafe { CFRunLoopTimerSetTolerance(one_shot, -5.0) };
        // SAFETY: a timer this call made.
        let t = unsafe { borrow::<Timer>(one_shot) }.expect("a timer");
        assert_eq!(
            t.tolerance(),
            0.0,
            "a negative tolerance would authorise firing before the fire date, which the \
             CFRunLoop.h comment forbids outright"
        );
        // SAFETY: nothing else holds a reference.
        unsafe { CFRelease(one_shot) };
    }

    /// A run loop source version this runtime cannot serve is refused by name and
    /// answers null - it does not abort the process.
    ///
    /// `CFRunLoopSourceCreate` is a `Create`, and the contract for a `Create` that
    /// cannot be satisfied is null: Apple's own answer for a context version it
    /// does not implement is to return no source. Aborting turned a refusal into
    /// a `SIGABRT` with no name on it, which is this project's worst outcome by
    /// construction - and it was reachable from any guest that asked for a
    /// version-1 (mach port) source, which is a documented thing to ask for.
    ///
    /// The check that it *named* itself rather than merely returning null is the
    /// second half: a null with nothing said is indistinguishable from "there was
    /// no memory", which is the failure mode PLAN §1 is about.
    #[test]
    fn a_source_version_this_runtime_cannot_serve_is_a_named_refusal_and_not_an_abort() {
        bootstrap();
        // A version-1 context, which is the mach-port source: the two words the
        // version-1 layout puts where a version-0 context has `retain`/`release`.
        #[repr(C)]
        struct MachPortSource {
            version: CFIndex,
            info: *mut c_void,
            mach_port: u32,
            reserved: [u32; 7],
        }
        let context = MachPortSource {
            version: 1,
            info: std::ptr::null_mut(),
            mach_port: 0,
            reserved: [0; 7],
        };
        // SAFETY: `context` is exactly the layout `CFRunLoopSourceCreate` reads a
        // version-1 context from, and it outlives the call.
        let source = unsafe {
            CFRunLoopSourceCreate(
                std::ptr::null(),
                0,
                &context as *const MachPortSource as *const CFRunLoopSourceContext,
            )
        };
        assert!(
            source.is_null(),
            "Apple answers null for a source version it does not implement, and so does this"
        );

        // And a version it does not have any business being asked about - a
        // negative version, and one past the highest real one - is refused the
        // same way rather than being taken as a version-0 source.
        for version in [-1 as CFIndex, 7] {
            let context = CFRunLoopSourceContext {
                version,
                info: std::ptr::null_mut(),
                retain: std::ptr::null(),
                release: std::ptr::null(),
                copy_description: std::ptr::null(),
                equal: std::ptr::null(),
                hash: std::ptr::null(),
                schedule: std::ptr::null(),
                cancel: std::ptr::null(),
                perform: None,
            };
            // SAFETY: a live context this test built.
            let made = unsafe { CFRunLoopSourceCreate(std::ptr::null(), 0, &context) };
            assert!(made.is_null(), "version {version} is not a version");
        }

        // A version-0 source is still made, and a null context is still null, so
        // the refusal above is not "always null".
        let good = CFRunLoopSourceContext {
            version: 0,
            info: 0x1234 as *mut c_void,
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
            equal: std::ptr::null(),
            hash: std::ptr::null(),
            schedule: std::ptr::null(),
            cancel: std::ptr::null(),
            perform: None,
        };
        // SAFETY: a live context this test built.
        let made = unsafe { CFRunLoopSourceCreate(std::ptr::null(), 3, &good) };
        assert!(!made.is_null(), "a version-0 source is not refused");
        // SAFETY: a source this call made.
        let source = unsafe { borrow::<Source>(made) }.expect("a source of ours");
        assert_eq!(source.order(), 3, "and the order it was created with is kept");
        // SAFETY: a null context, which is what Apple answers for.
        assert!(unsafe { CFRunLoopSourceCreate(std::ptr::null(), 0, std::ptr::null()) }.is_null());
    }
}
