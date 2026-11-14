//! Uniform Type Identifiers by tag: the common types' MIME types and filename
//! extensions. CoreServices' UTType API answers from this table, and so do
//! URL type identifiers here, so the two cannot disagree.
//!
//! Only a table of the common types; Launch Services' database of every
//! installed application's declarations has no counterpart here. A tag the
//! table lacks gets a dynamic identifier, as Apple's does, which converts back
//! to the tag it came from.

/// `kUTTagClassMIMEType` and `kUTTagClassFilenameExtension`, as published by
/// AppKit's string globals - the same characters, which is what is compared.
pub const MIME_TYPE: &str = "public.mime-type";
pub const FILENAME_EXTENSION: &str = "public.filename-extension";

/// Identifier, preferred MIME type, preferred extension.
pub type Row = (&'static str, &'static str, &'static str);

pub const TYPES: &[Row] = &[
    // `.txt` is plain text on a Mac, so that row comes first: the first row
    // with a tag is the type the tag names.
    ("public.plain-text", "text/plain", "txt"),
    ("public.utf8-plain-text", "text/plain;charset=utf-8", "txt"),
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
pub const DYNAMIC: &str = "dyn.rosemary.";

pub fn column(tag_class: &str) -> Option<fn(&Row) -> &'static str> {
    match tag_class {
        MIME_TYPE => Some(|t| t.1),
        FILENAME_EXTENSION => Some(|t| t.2),
        _ => None,
    }
}

/// The identifier for `tag`, matched case-insensitively, as MIME types and
/// extensions both are. A MIME type's parameters only narrow it.
pub fn identifier_for(tag_class: &str, tag: &str) -> Option<String> {
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

pub fn tag_for(identifier: &str, tag_class: &str) -> Option<String> {
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
