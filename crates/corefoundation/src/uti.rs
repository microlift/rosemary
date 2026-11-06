//! Uniform Type Identifiers: converting between a type identifier and the MIME
//! types and filename extensions that name the same kind of data.
//!
//! Plain string logic with no CoreFoundation types of its own, so that both
//! `coreservices` (the legacy `UTTypeCreatePreferredIdentifierForTag` C API)
//! and `foundation` (the `UTType` class and `NSURLContentTypeKey`) answer from
//! the one table instead of two that could drift apart.
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
pub const DYNAMIC: &str = "dyn.rosemary.";

fn column(tag_class: &str) -> Option<fn(&Row) -> &'static str> {
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

/// The preferred tag of `identifier`'s class, or `None` for one without a tag
/// of that class.
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

/// Direct `conformsTo` edges, from Apple's "System-Declared Uniform Type
/// Identifiers" reference. Not the Launch Services database: only the common
/// types this tree otherwise answers about.
///
/// FIXME (ACCURACY): a type outside this table conforms to nothing but
/// `public.item` and itself here, where a real system also knows every
/// installed application's declared conformance.
pub const PARENTS: &[(&str, &[&str])] = &[
    ("public.data", &["public.item"]),
    ("public.directory", &["public.item"]),
    ("public.url", &["public.data"]),
    ("public.file-url", &["public.url"]),
    ("public.image", &["public.data", "public.content"]),
    ("public.png", &["public.image"]),
    ("public.jpeg", &["public.image"]),
    ("com.compuserve.gif", &["public.image"]),
    ("public.tiff", &["public.image"]),
    ("com.microsoft.bmp", &["public.image"]),
    ("public.text", &["public.data", "public.content"]),
    ("public.plain-text", &["public.text"]),
    ("public.utf8-plain-text", &["public.plain-text"]),
    ("public.html", &["public.text"]),
    ("public.rtf", &["public.text"]),
    ("public.json", &["public.text"]),
    (
        "public.audiovisual-content",
        &["public.content", "public.data"],
    ),
    ("public.movie", &["public.audiovisual-content"]),
    ("public.mpeg-4", &["public.movie"]),
    ("public.composite-content", &["public.content"]),
    (
        "com.adobe.pdf",
        &["public.data", "public.composite-content"],
    ),
    ("public.folder", &["public.directory"]),
    ("com.apple.package", &["public.directory"]),
    ("public.zip-archive", &["public.data", "public.archive"]),
    ("public.item", &[]),
    ("public.content", &[]),
    ("com.apple.bundle", &["public.directory"]),
    ("public.executable", &["public.item"]),
    ("com.apple.application", &["public.executable"]),
    (
        "com.apple.application-bundle",
        &[
            "com.apple.application",
            "com.apple.bundle",
            "com.apple.package",
        ],
    ),
    ("public.contact", &["public.item"]),
    ("public.vcard", &["public.text", "public.contact"]),
    ("public.archive", &["public.item"]),
];

/// Whether `id` conforms to `other`: equal, or `other` reachable by walking
/// [`PARENTS`]. `public.item` is the UTI root, so everything conforms to it,
/// even a type this table has no entry for.
pub fn conforms(id: &str, other: &str) -> bool {
    if id.eq_ignore_ascii_case(other) {
        return true;
    }
    if other.eq_ignore_ascii_case("public.item") {
        return true;
    }
    let mut seen = std::collections::HashSet::new();
    let mut queue = vec![id.to_ascii_lowercase()];
    while let Some(current) = queue.pop() {
        if !seen.insert(current.clone()) {
            continue;
        }
        let Some((_, parents)) = PARENTS
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&current))
        else {
            continue;
        };
        for parent in *parents {
            if parent.eq_ignore_ascii_case(other) {
                return true;
            }
            queue.push((*parent).to_string());
        }
    }
    false
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

    #[test]
    fn conformance_walks_the_declared_chain_to_the_root() {
        assert!(conforms("public.png", "public.image"));
        assert!(conforms("public.png", "public.data"));
        assert!(conforms("public.png", "public.item"));
        assert!(conforms("public.png", "public.png"));
        assert!(!conforms("public.png", "public.movie"));
        // Nothing is known about a type outside the table, beyond the root.
        assert!(conforms("com.example.mystery", "public.item"));
        assert!(!conforms("com.example.mystery", "public.data"));
    }
}
