//! `CFAttributedString`, which is `NSAttributedString`: each function is the
//! method it is toll-free bridged to.
#![allow(non_snake_case)]

use corefoundation::base::{CFIndex, CFTypeRef};
use objc::object::Id;
use objc::sel::Sel;

use crate::geometry::NSRange;

/// `CFRange`, which has `NSRange`'s layout with signed words.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CFRange {
    location: CFIndex,
    length: CFIndex,
}

impl From<CFRange> for NSRange {
    fn from(range: CFRange) -> NSRange {
        NSRange {
            location: range.location.max(0) as usize,
            length: range.length.max(0) as usize,
        }
    }
}

fn class(name: &std::ffi::CStr) -> Id {
    objc::class::ptr(objc::class::lookup(name).expect("Foundation's class")) as Id
}

fn send0(receiver: Id, sel: &str) -> Id {
    // SAFETY: `id (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel))
    }
}

/// `CFAttributedStringCreate(CFAllocatorRef, CFStringRef, CFDictionaryRef)`.
unsafe extern "C" fn CFAttributedStringCreate(
    _allocator: CFTypeRef,
    string: CFTypeRef,
    attributes: CFTypeRef,
) -> CFTypeRef {
    let object = send0(class(c"NSAttributedString"), "alloc");
    // SAFETY: `-initWithString:attributes:`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            object,
            objc::sel::get("initWithString:attributes:"),
            string as Id,
            attributes as Id,
        ) as CFTypeRef
    }
}

/// `CFAttributedStringCreateMutable(CFAllocatorRef, CFIndex maxLength)`.
unsafe extern "C" fn CFAttributedStringCreateMutable(
    _allocator: CFTypeRef,
    _max: CFIndex,
) -> CFTypeRef {
    let object = send0(class(c"NSMutableAttributedString"), "alloc");
    send0(object, "init") as CFTypeRef
}

/// `CFAttributedStringCreateCopy(CFAllocatorRef, CFAttributedStringRef)`.
unsafe extern "C" fn CFAttributedStringCreateCopy(
    _allocator: CFTypeRef,
    string: CFTypeRef,
) -> CFTypeRef {
    send0(string as Id, "copy") as CFTypeRef
}

/// `CFAttributedStringCreateMutableCopy(CFAllocatorRef, CFIndex, CFAttributedStringRef)`.
unsafe extern "C" fn CFAttributedStringCreateMutableCopy(
    _allocator: CFTypeRef,
    _max: CFIndex,
    string: CFTypeRef,
) -> CFTypeRef {
    send0(string as Id, "mutableCopy") as CFTypeRef
}

unsafe extern "C" fn CFAttributedStringGetLength(string: CFTypeRef) -> CFIndex {
    if string.is_null() {
        return 0;
    }
    // SAFETY: `-length`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());
        f(string as Id, objc::sel::get("length")) as CFIndex
    }
}

/// `CFAttributedStringGetString`: the string, owned by the attributed one.
unsafe extern "C" fn CFAttributedStringGetString(string: CFTypeRef) -> CFTypeRef {
    send0(string as Id, "string") as CFTypeRef
}

unsafe extern "C" fn CFAttributedStringReplaceString(
    string: CFTypeRef,
    range: CFRange,
    replacement: CFTypeRef,
) {
    // SAFETY: `-replaceCharactersInRange:withString:`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRange, Id) = std::mem::transmute(objc::msg::entry());
        f(
            string as Id,
            objc::sel::get("replaceCharactersInRange:withString:"),
            range.into(),
            replacement as Id,
        );
    }
}

unsafe extern "C" fn CFAttributedStringReplaceAttributedString(
    string: CFTypeRef,
    range: CFRange,
    replacement: CFTypeRef,
) {
    // SAFETY: `-replaceCharactersInRange:withAttributedString:`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRange, Id) = std::mem::transmute(objc::msg::entry());
        f(
            string as Id,
            objc::sel::get("replaceCharactersInRange:withAttributedString:"),
            range.into(),
            replacement as Id,
        );
    }
}

unsafe extern "C" fn CFAttributedStringSetAttribute(
    string: CFTypeRef,
    range: CFRange,
    name: CFTypeRef,
    value: CFTypeRef,
) {
    // SAFETY: `-addAttribute:value:range:`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id, NSRange) =
            std::mem::transmute(objc::msg::entry());
        f(
            string as Id,
            objc::sel::get("addAttribute:value:range:"),
            name as Id,
            value as Id,
            range.into(),
        );
    }
}

/// `CFAttributedStringSetAttributes(..., CFDictionaryRef, Boolean
/// clearOtherAttributes)`: replace, or add to, the range's attributes.
unsafe extern "C" fn CFAttributedStringSetAttributes(
    string: CFTypeRef,
    range: CFRange,
    attributes: CFTypeRef,
    clear: u8,
) {
    let sel = match clear {
        0 => "addAttributes:range:",
        _ => "setAttributes:range:",
    };
    // SAFETY: both are `void (id, SEL, id, NSRange)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, NSRange) = std::mem::transmute(objc::msg::entry());
        f(
            string as Id,
            objc::sel::get(sel),
            attributes as Id,
            range.into(),
        );
    }
}

unsafe extern "C" fn CFAttributedStringRemoveAttribute(
    string: CFTypeRef,
    range: CFRange,
    name: CFTypeRef,
) {
    // SAFETY: `-removeAttribute:range:`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, NSRange) = std::mem::transmute(objc::msg::entry());
        f(
            string as Id,
            objc::sel::get("removeAttribute:range:"),
            name as Id,
            range.into(),
        );
    }
}

/// `CFAttributedStringGetAttributes(CFAttributedStringRef, CFIndex, CFRange
/// *effectiveRange)`.
unsafe extern "C" fn CFAttributedStringGetAttributes(
    string: CFTypeRef,
    location: CFIndex,
    effective: *mut CFRange,
) -> CFTypeRef {
    let mut range = NSRange::default();
    // SAFETY: `-attributesAtIndex:effectiveRange:`.
    let attributes = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, usize, *mut NSRange) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            string as Id,
            objc::sel::get("attributesAtIndex:effectiveRange:"),
            location.max(0) as usize,
            &mut range,
        )
    };
    // SAFETY: null or writable, by the caller.
    if let Some(out) = unsafe { effective.as_mut() } {
        *out = CFRange {
            location: range.location as CFIndex,
            length: range.length as CFIndex,
        };
    }
    attributes as CFTypeRef
}

unsafe extern "C" fn CFAttributedStringGetAttribute(
    string: CFTypeRef,
    location: CFIndex,
    name: CFTypeRef,
    effective: *mut CFRange,
) -> CFTypeRef {
    let mut range = NSRange::default();
    // SAFETY: `-attribute:atIndex:effectiveRange:`.
    let value = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, usize, *mut NSRange) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            string as Id,
            objc::sel::get("attribute:atIndex:effectiveRange:"),
            name as Id,
            location.max(0) as usize,
            &mut range,
        )
    };
    // SAFETY: as above.
    if let Some(out) = unsafe { effective.as_mut() } {
        *out = CFRange {
            location: range.location as CFIndex,
            length: range.length as CFIndex,
        };
    }
    value as CFTypeRef
}

/// `CFAttributedStringBeginEditing` and `...EndEditing`: batching hints.
unsafe extern "C" fn CFAttributedStringEditing(_string: CFTypeRef) {}

exports::exports! {
    functions {
        "CFAttributedStringCreate" => CFAttributedStringCreate,
        "CFAttributedStringCreateMutable" => CFAttributedStringCreateMutable,
        "CFAttributedStringCreateCopy" => CFAttributedStringCreateCopy,
        "CFAttributedStringCreateMutableCopy" => CFAttributedStringCreateMutableCopy,
        "CFAttributedStringGetLength" => CFAttributedStringGetLength,
        "CFAttributedStringGetString" => CFAttributedStringGetString,
        "CFAttributedStringReplaceString" => CFAttributedStringReplaceString,
        "CFAttributedStringReplaceAttributedString" => CFAttributedStringReplaceAttributedString,
        "CFAttributedStringSetAttribute" => CFAttributedStringSetAttribute,
        "CFAttributedStringSetAttributes" => CFAttributedStringSetAttributes,
        "CFAttributedStringRemoveAttribute" => CFAttributedStringRemoveAttribute,
        "CFAttributedStringGetAttributes" => CFAttributedStringGetAttributes,
        "CFAttributedStringGetAttribute" => CFAttributedStringGetAttribute,
        "CFAttributedStringBeginEditing" | "CFAttributedStringEndEditing" => CFAttributedStringEditing,
    }
}
