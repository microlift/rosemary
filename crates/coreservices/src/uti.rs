//! Uniform Type Identifiers: converting between a type identifier and the MIME
//! types and filename extensions that name the same kind of data.
//!
//! Only a table of the common types; Launch Services' database of every
//! installed application's declarations has no counterpart here. A tag the
//! table lacks gets a dynamic identifier, as Apple's does, which converts back
//! to the tag it came from.

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;

/// `kUTTagClassMIMEType` and `kUTTagClassFilenameExtension`, as published by
/// AppKit's string globals - the same characters, which is what is compared.
pub(crate) const MIME_TYPE: &str = "public.mime-type";
pub(crate) const FILENAME_EXTENSION: &str = "public.filename-extension";

/// Identifier, preferred MIME type, preferred extension.
pub(crate) type Row = (&'static str, &'static str, &'static str);

pub(crate) const TYPES: &[Row] = &[
    ("public.utf8-plain-text", "text/plain;charset=utf-8", "txt"),
    ("public.plain-text", "text/plain", "txt"),
    ("public.html", "text/html", "html"),
    ("public.rtf", "text/rtf", "rtf"),
    ("public.url", "text/uri-list", "url"),
    ("public.png", "image/png", "png"),
    ("public.jpeg", "image/jpeg", "jpeg"),
    ("com.compuserve.gif", "image/gif", "gif"),
    ("public.tiff", "image/tiff", "tiff"),
    ("com.microsoft.bmp", "image/bmp", "bmp"),
    ("public.json", "application/json", "json"),
    ("com.adobe.pdf", "application/pdf", "pdf"),
    ("public.zip-archive", "application/zip", "zip"),
    ("public.vcard", "text/vcard", "vcf"),
];

/// The prefix of an identifier made up for a tag the table lacks. Apple's
/// encode the tag opaquely after `dyn.`; this one keeps it readable.
pub(crate) const DYNAMIC: &str = "dyn.rosemary.";

fn read(string: *const c_void) -> Option<String> {
    // SAFETY: a CoreFoundation string or null, by the callers' contracts.
    let s = unsafe { corefoundation::string::as_string(string as CFTypeRef) }?;
    Some(String::from_utf8_lossy(&s.utf8()).into_owned())
}

fn make(text: &str) -> *const c_void {
    corefoundation::string::from_bytes(text.as_bytes(), false)
}

fn column(tag_class: &str) -> Option<fn(&Row) -> &'static str> {
    match tag_class {
        MIME_TYPE => Some(|t| t.1),
        FILENAME_EXTENSION => Some(|t| t.2),
        _ => None,
    }
}

/// The identifier for `tag`, matched case-insensitively, as MIME types and
/// extensions both are. A MIME type's parameters only narrow it.
pub(crate) fn identifier_for(tag_class: &str, tag: &str) -> Option<String> {
    let of = column(tag_class)?;
    let tag = tag.to_ascii_lowercase();
    let found = TYPES
        .iter()
        .find(|t| of(t) == tag)
        .or_else(|| {
            let bare = tag.split(';').next().unwrap_or_default().trim();
            TYPES.iter().find(|t| of(t) == bare)
        })
        .map(|t| t.0.to_string());
    Some(found.unwrap_or_else(|| format!("{DYNAMIC}{tag_class}.{tag}")))
}

pub(crate) fn tag_for(identifier: &str, tag_class: &str) -> Option<String> {
    let of = column(tag_class)?;
    if let Some(t) = TYPES.iter().find(|t| t.0 == identifier) {
        return Some(of(t).to_string());
    }
    identifier
        .strip_prefix(DYNAMIC)?
        .strip_prefix(tag_class)?
        .strip_prefix('.')
        .map(str::to_string)
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

    #[test]
    fn maps_known_mime_types_both_ways() {
        assert_eq!(
            identifier_for(MIME_TYPE, "image/PNG").as_deref(),
            Some("public.png")
        );
        assert_eq!(
            identifier_for(MIME_TYPE, "text/plain;charset=utf-8").as_deref(),
            Some("public.utf8-plain-text")
        );
        assert_eq!(
            tag_for("public.png", MIME_TYPE).as_deref(),
            Some("image/png")
        );
        assert_eq!(
            tag_for("public.png", FILENAME_EXTENSION).as_deref(),
            Some("png")
        );
    }

    #[test]
    fn an_unknown_tag_round_trips_through_a_dynamic_identifier() {
        let id = identifier_for(MIME_TYPE, "application/x-terraria").unwrap();
        assert!(id.starts_with("dyn."));
        assert_eq!(
            tag_for(&id, MIME_TYPE).as_deref(),
            Some("application/x-terraria")
        );
        assert_eq!(tag_for(&id, FILENAME_EXTENSION), None);
        assert_eq!(identifier_for("public.bogus", "x"), None);
    }
}
