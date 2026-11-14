//! Uniform Type Identifiers: converting between a type identifier and the MIME
//! types and filename extensions that name the same kind of data.
//!
//! Only a table of the common types; Launch Services' database of every
//! installed application's declarations has no counterpart here. A tag the
//! table lacks gets a dynamic identifier, as Apple's does, which converts back
//! to the tag it came from.

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;

pub(crate) use corefoundation::uti::{FILENAME_EXTENSION, MIME_TYPE, identifier_for, tag_for};

fn read(string: *const c_void) -> Option<String> {
    // SAFETY: a CoreFoundation string or null, by the callers' contracts.
    let s = unsafe { corefoundation::string::as_string(string as CFTypeRef) }?;
    Some(String::from_utf8_lossy(&s.utf8()).into_owned())
}

fn make(text: &str) -> *const c_void {
    corefoundation::string::from_bytes(text.as_bytes(), false)
}

/// `CFStringRef UTTypeCreatePreferredIdentifierForTag(CFStringRef inTagClass,
/// CFStringRef inTag, CFStringRef inConformingToUTI)`
///
/// The conformance hint is not consulted: each tag in the table names one type.
pub unsafe extern "C" fn UTTypeCreatePreferredIdentifierForTag(
    tag_class: *const c_void,
    tag: *const c_void,
    _conforming_to: *const c_void,
) -> *const c_void {
    let (Some(class), Some(tag)) = (read(tag_class), read(tag)) else {
        return std::ptr::null();
    };
    match identifier_for(&class, &tag) {
        Some(id) => make(&id),
        None => std::ptr::null(),
    }
}

/// `CFStringRef UTTypeCopyPreferredTagWithClass(CFStringRef inUTI, CFStringRef
/// inTagClass)`, null for a type without a tag of that class.
pub unsafe extern "C" fn UTTypeCopyPreferredTagWithClass(
    identifier: *const c_void,
    tag_class: *const c_void,
) -> *const c_void {
    let (Some(id), Some(class)) = (read(identifier), read(tag_class)) else {
        return std::ptr::null();
    };
    match tag_for(&id, &class) {
        Some(tag) => make(&tag),
        None => std::ptr::null(),
    }
}

/// `Boolean UTTypeConformsTo(CFStringRef inUTI, CFStringRef inConformsToUTI)`
///
/// Against the declared types' table, so a type only an application declares
/// - which Launch Services would know from that application - conforms only
/// to itself, as on a Mac without that application.
pub unsafe extern "C" fn UTTypeConformsTo(identifier: *const c_void, other: *const c_void) -> u8 {
    match (read(identifier), read(other)) {
        (Some(identifier), Some(other)) => u8::from(crate::uttype::conforms(&identifier, &other)),
        _ => 0,
    }
}

exports::exports! {
    functions {
        "UTTypeConformsTo" => UTTypeConformsTo,
        "UTTypeCreatePreferredIdentifierForTag" => UTTypeCreatePreferredIdentifierForTag,
        "UTTypeCopyPreferredTagWithClass" => UTTypeCopyPreferredTagWithClass,
    }
}
