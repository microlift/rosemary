use std::ffi::{CStr, CString, c_char};

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

pub use coregraphics::CGFloat;

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct NSPoint {
    pub x: CGFloat,
    pub y: CGFloat,
}

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct NSSize {
    pub width: CGFloat,
    pub height: CGFloat,
}

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct NSRect {
    pub origin: NSPoint,
    pub size: NSSize,
}

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct NSRange {
    pub location: usize,
    pub length: usize,
}

/// The type encodings these have, which `-objCType` answers with and which a
/// guest compares against to decide how to unpack a value.
mod encoding {
    pub const POINT: &std::ffi::CStr = c"{CGPoint=dd}";
    pub const SIZE: &std::ffi::CStr = c"{CGSize=dd}";
    pub const RECT: &std::ffi::CStr = c"{CGRect={CGPoint=dd}{CGSize=dd}}";
    pub const RANGE: &std::ffi::CStr = c"{_NSRange=QQ}";
}

/// A boxed value: the bytes, and what they are.
///
/// Bytes rather than an enum of the four shapes above, because `+valueWithBytes:objCType:`
/// boxes anything a guest hands it and `-getValue:` hands it back. The named
/// constructors are the same thing with the encoding filled in.
struct Boxed {
    bytes: Vec<u8>,
    encoding: CString,
}

#[repr(C)]
struct Body {
    isa: *const Class,
    boxed: *mut Boxed,
}

fn make(bytes: &[u8], encoding: &CStr) -> Id {
    let class = objc::class::lookup(c"NSValue").expect("NSValue is registered");
    let object = objc::object::create_instance(class, 0);
    if object.is_null() {
        return object;
    }
    // SAFETY: a live `NSValue`, whose second word is this crate's to fill in.
    unsafe {
        (*(object as *mut Body)).boxed = Box::into_raw(Box::new(Boxed {
            bytes: bytes.to_vec(),
            encoding: CString::from(encoding),
        }));
    }
    // SAFETY: a fresh object; the caller is a `+value...` method, which returns
    // an autoreleased one.
    unsafe { corefoundation::api::CFAutorelease(object as corefoundation::base::CFTypeRef) as Id }
}

/// The contents of a boxed value.
///
/// # Safety
///
/// `receiver` must be null or an object this runtime allocated.
unsafe fn boxed<'a>(receiver: Id) -> Option<&'a Boxed> {
    let class = objc::class::lookup(c"NSValue")?;
    if !unsafe { objc::object::class_of(receiver) }
        .is_some_and(|c| c.chain().any(|k| std::ptr::eq(k, class)))
    {
        return None;
    }
    // SAFETY: an `NSValue` is a `Body` with a live `boxed`.
    unsafe {
        let body = receiver as *const Body;
        (!(*body).boxed.is_null()).then(|| &*(*body).boxed)
    }
}

/// Read a boxed value back as `T`, or the default if it is not one or is the
/// wrong size.
///
/// # Safety
///
/// As [`boxed`], and `T` must be plain data.
unsafe fn unbox<T: Default + Copy>(receiver: Id) -> T {
    let Some(boxed) = (unsafe { boxed(receiver) }) else {
        return T::default();
    };
    if boxed.bytes.len() != size_of::<T>() {
        return T::default();
    }
    let mut out = T::default();
    // SAFETY: the sizes match, and `T` is plain data.
    unsafe {
        std::ptr::copy_nonoverlapping(
            boxed.bytes.as_ptr(),
            &mut out as *mut T as *mut u8,
            size_of::<T>(),
        )
    };
    out
}

/// `T` as its bytes.
fn bytes_of<T: Copy>(value: &T) -> &[u8] {
    // SAFETY: `T` is plain data, and this borrows it for no longer than it lives.
    unsafe { std::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: reached with the last reference gone; the receiver is an `NSValue`.
    unsafe {
        let body = receiver as *mut Body;
        if !(*body).boxed.is_null() {
            drop(Box::from_raw((*body).boxed));
            (*body).boxed = std::ptr::null_mut();
        }
        objc::arc::finalize(receiver);
    }
}

unsafe extern "C" fn obj_c_type(receiver: Id, _sel: Sel) -> *const c_char {
    // SAFETY: an `NSValue`.
    unsafe { boxed(receiver) }.map_or(std::ptr::null(), |b| b.encoding.as_ptr())
}

unsafe extern "C" fn get_value(receiver: Id, _sel: Sel, out: *mut u8) {
    // SAFETY: an `NSValue`.
    let Some(b) = (unsafe { boxed(receiver) }) else {
        return;
    };
    if out.is_null() {
        return;
    }
    // SAFETY: the caller says `out` is at least as big as what was boxed, which
    // is the contract of `-getValue:` and why Apple deprecated it.
    unsafe { std::ptr::copy_nonoverlapping(b.bytes.as_ptr(), out, b.bytes.len()) };
}

unsafe extern "C" fn value_with_bytes(
    _class: Id,
    _sel: Sel,
    bytes: *const u8,
    encoding: *const c_char,
) -> Id {
    if bytes.is_null() || encoding.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a NUL-terminated encoding the caller supplied.
    let encoding = unsafe { CStr::from_ptr(encoding) };
    // The size is what the encoding says, and this runtime only knows the four
    // it makes itself. Anything else is refused by name rather than boxed at a
    // guessed width, which would read past the caller's value.
    let size = match encoding.to_bytes() {
        b if b == encoding::POINT.to_bytes() || b == encoding::SIZE.to_bytes() => 16,
        b if b == encoding::RECT.to_bytes() => 32,
        b if b == encoding::RANGE.to_bytes() => 16,
        _ => objc::fail!(
            "+[NSValue valueWithBytes:objCType:]: this runtime does not know the width of {}",
            encoding.to_string_lossy()
        ),
    };
    // SAFETY: the caller says `bytes` points at a value of this encoding.
    make(unsafe { std::slice::from_raw_parts(bytes, size) }, encoding)
}

unsafe extern "C" fn value_with_point(_class: Id, _sel: Sel, point: NSPoint) -> Id {
    make(bytes_of(&point), encoding::POINT)
}

unsafe extern "C" fn value_with_size(_class: Id, _sel: Sel, size: NSSize) -> Id {
    boxed_size(size)
}

/// An autoreleased `NSValue` holding `size`, for this tree's own dictionaries.
///
/// `-[NSScreen deviceDescription]` is the caller: its values are boxed sizes,
/// and a guest reads one back with `-sizeValue`.
pub fn boxed_size(size: NSSize) -> Id {
    make(bytes_of(&size), encoding::SIZE)
}

unsafe extern "C" fn value_with_rect(_class: Id, _sel: Sel, rect: NSRect) -> Id {
    make(bytes_of(&rect), encoding::RECT)
}

unsafe extern "C" fn value_with_range(_class: Id, _sel: Sel, range: NSRange) -> Id {
    make(bytes_of(&range), encoding::RANGE)
}

unsafe extern "C" fn point_value(receiver: Id, _sel: Sel) -> NSPoint {
    // SAFETY: an `NSValue` holding plain data.
    unsafe { unbox(receiver) }
}

unsafe extern "C" fn size_value(receiver: Id, _sel: Sel) -> NSSize {
    unsafe { unbox(receiver) }
}

unsafe extern "C" fn rect_value(receiver: Id, _sel: Sel) -> NSRect {
    unsafe { unbox(receiver) }
}

unsafe extern "C" fn range_value(receiver: Id, _sel: Sel) -> NSRange {
    unsafe { unbox(receiver) }
}

/// Box one of the four shapes, for [`crate::kvc`].
pub(crate) fn box_point(value: NSPoint) -> Id {
    make(bytes_of(&value), encoding::POINT)
}

pub(crate) fn box_size(value: NSSize) -> Id {
    make(bytes_of(&value), encoding::SIZE)
}

pub(crate) fn box_rect(value: NSRect) -> Id {
    make(bytes_of(&value), encoding::RECT)
}

pub(crate) fn box_range(value: NSRange) -> Id {
    make(bytes_of(&value), encoding::RANGE)
}

/// The bytes a boxed value holds, whatever shape it is.
///
/// # Safety
///
/// `value` must be null or an object this runtime allocated.
pub(crate) unsafe fn unbox_bytes(value: Id) -> Option<Vec<u8>> {
    unsafe { boxed(value) }.map(|b| b.bytes.clone())
}

pub(crate) fn install() {
    // Register the storage-bearing class before `value::install` adds the
    // pointer-value constructor. A class's size is fixed at registration, so a
    // prior abstract registration would leave no room for `boxed`.
    let class =
        corefoundation::base::register(c"NSValue", None, std::mem::size_of::<Body>(), false);
    methods!(class, {
        "objCType" => obj_c_type, c"*16@0:8";
        "getValue:" => get_value, c"v24@0:8^v16";
        "getValue:size:" => get_value, c"v32@0:8^v16Q24";
        "pointValue" => point_value, c"{CGPoint=dd}16@0:8";
        "sizeValue" => size_value, c"{CGSize=dd}16@0:8";
        "rectValue" => rect_value, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "rangeValue" => range_value, c"{_NSRange=QQ}16@0:8";
        "dealloc" => dealloc, c"v16@0:8"
    });
    class_methods!(class, {
        "valueWithBytes:objCType:" => value_with_bytes, c"@32@0:8^v16*24";
        "value:withObjCType:" => value_with_bytes, c"@32@0:8^v16*24";
        "valueWithPoint:" => value_with_point, c"@32@0:8{CGPoint=dd}16";
        "valueWithSize:" => value_with_size, c"@32@0:8{CGSize=dd}16";
        "valueWithRect:" => value_with_rect, c"@48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "valueWithRange:" => value_with_range, c"@32@0:8{_NSRange=QQ}16"
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nsvalue_has_room_for_its_boxed_payload() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSValue").expect("registered");
        assert!(class.instance_size() >= size_of::<Body>());
    }

    /// Each of the three shapes survives being boxed and read back.
    ///
    /// Called directly rather than through `objc_msgSend`: what a *guest* send
    /// does to these is `rosemary`'s marshaller's business and is covered by
    /// `tests/msgsend.rs`. What this pins is that the storage is the right width
    /// and the encoding is the right string.
    #[test]
    fn the_three_aggregates_round_trip() {
        crate::bootstrap();
        // SAFETY: real class objects, and each prototype is the one declared
        // above.
        unsafe {
            let class = objc::class::ptr(objc::class::lookup(c"NSValue").unwrap()) as Id;
            let sel = objc::sel::get("x");

            let rect = NSRect {
                origin: NSPoint { x: 1.5, y: 2.5 },
                size: NSSize {
                    width: 3.5,
                    height: 4.5,
                },
            };
            let boxed = value_with_rect(class, sel, rect);
            assert_eq!(rect_value(boxed, sel), rect);
            assert_eq!(
                CStr::from_ptr(obj_c_type(boxed, sel)),
                encoding::RECT,
                "a guest switches on this string"
            );

            let point = NSPoint { x: 6.5, y: 7.5 };
            assert_eq!(point_value(value_with_point(class, sel, point), sel), point);

            let range = NSRange {
                location: 8,
                length: 9,
            };
            assert_eq!(range_value(value_with_range(class, sel, range), sel), range);

            // A value read back as the wrong shape is zero rather than the
            // neighbouring bytes: the widths disagree, and answering with
            // whatever happened to be there is the failure mode this checks for.
            assert_eq!(point_value(boxed, sel), NSPoint::default());
        }
    }
}
