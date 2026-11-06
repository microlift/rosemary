//! Uniform Type Identifiers: the legacy C API. The identifier table itself is
//! `corefoundation::uti`, shared with `foundation`'s `UTType` class so the two
//! cannot answer differently about the same tag.

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;

use corefoundation::uti::{identifier_for, tag_for};

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

exports::exports! {
    functions {
        "UTTypeCreatePreferredIdentifierForTag" => UTTypeCreatePreferredIdentifierForTag,
        "UTTypeCopyPreferredTagWithClass" => UTTypeCopyPreferredTagWithClass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use corefoundation::uti::{FILENAME_EXTENSION, MIME_TYPE};

    #[test]
    fn round_trips_a_known_tag_through_corefoundation_s_table() {
        assert_eq!(
            identifier_for(MIME_TYPE, "image/PNG").as_deref(),
            Some("public.png")
        );
        assert_eq!(
            tag_for("public.png", FILENAME_EXTENSION).as_deref(),
            Some("png")
        );
    }
}
