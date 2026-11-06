//! `UTType`, the Objective-C face of Uniform Type Identifiers that
//! UniformTypeIdentifiers.framework added in macOS 11.
//!
//! One object per identifier, kept for the process, so identity is equality and
//! `NSObject`'s `-isEqual:` and `-hash` are already right. The types are the
//! ones [`corefoundation::uti`] knows plus the hierarchy below; an identifier outside
//! both is undeclared, and `+typeWithIdentifier:` answers nil for it as Apple's
//! does.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

use corefoundation::base::CFTypeRef;
use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

/// Each declared type and the types it directly conforms to.
const CONFORMS: &[(&str, &[&str])] = corefoundation::uti::PARENTS;

/// The `UTType*` constants a program binds, as (Mach-O name, identifier).
const CONSTANTS: &[(&str, &str)] = &[
    ("_UTTypeItem", "public.item"),
    ("_UTTypeContent", "public.content"),
    ("_UTTypeData", "public.data"),
    ("_UTTypeDirectory", "public.directory"),
    ("_UTTypeFolder", "public.folder"),
    ("_UTTypePackage", "com.apple.package"),
    ("_UTTypeBundle", "com.apple.bundle"),
    ("_UTTypeApplication", "com.apple.application"),
    ("_UTTypeApplicationBundle", "com.apple.application-bundle"),
    ("_UTTypeURL", "public.url"),
    ("_UTTypeFileURL", "public.file-url"),
    ("_UTTypeText", "public.text"),
    ("_UTTypePlainText", "public.plain-text"),
    ("_UTTypeUTF8PlainText", "public.utf8-plain-text"),
    ("_UTTypeHTML", "public.html"),
    ("_UTTypeRTF", "public.rtf"),
    ("_UTTypeJSON", "public.json"),
    ("_UTTypeVCard", "public.vcard"),
    ("_UTTypeImage", "public.image"),
    ("_UTTypePNG", "public.png"),
    ("_UTTypeJPEG", "public.jpeg"),
    ("_UTTypeTIFF", "public.tiff"),
    ("_UTTypeGIF", "com.compuserve.gif"),
    ("_UTTypeBMP", "com.microsoft.bmp"),
    ("_UTTypePDF", "com.adobe.pdf"),
    ("_UTTypeZIP", "public.zip-archive"),
    ("_UTTypeMovie", "public.movie"),
    ("_UTTypeMPEG4Movie", "public.mpeg-4"),
];

#[repr(C)]
struct Body {
    isa: *const Class,
    identifier: CFTypeRef,
}

fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        let class = corefoundation::base::register(c"UTType", None, size_of::<Body>(), false);
        install_methods(class);
        objc::class::ptr(class) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("the UTType class") }
}

fn is_declared(identifier: &str) -> bool {
    CONFORMS.iter().any(|(id, _)| *id == identifier)
        || corefoundation::uti::TYPES
            .iter()
            .any(|row| row.0 == identifier)
}

/// The one object for `identifier`, made on first use and never freed.
fn intern(identifier: &str) -> Id {
    static TYPES: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);
    let mut types = TYPES.lock().unwrap_or_else(|e| e.into_inner());
    let types = types.get_or_insert_with(HashMap::new);
    if let Some(&object) = types.get(identifier) {
        return object as Id;
    }
    // SAFETY: `Body` is the registered layout.
    let body = unsafe { corefoundation::base::allocate::<Body>(class()) };
    // SAFETY: a freshly allocated body; the string is owned by it for good.
    unsafe {
        (*body).identifier = corefoundation::string::from_bytes(identifier.as_bytes(), false)
    };
    types.insert(identifier.to_string(), body as usize);
    body as Id
}

/// The type for `identifier`, or nil for one that is neither declared nor
/// dynamic.
fn lookup(identifier: &str) -> Id {
    match is_declared(identifier) || identifier.starts_with(corefoundation::uti::DYNAMIC) {
        true => intern(identifier),
        false => std::ptr::null_mut(),
    }
}

/// # Safety
///
/// `object` is one of this module's types.
unsafe fn identifier_text(object: Id) -> String {
    // SAFETY: this class's layout, as the caller states.
    let text = unsafe { (*(object as *const Body)).identifier };
    // SAFETY: the identifier is a string this module made.
    unsafe { String::from_utf8_lossy(&crate::string::bytes(text as Id)).into_owned() }
}

/// # Safety
///
/// `text` is an `NSString` or nil.
unsafe fn text(text: Id) -> Option<String> {
    if text.is_null() {
        return None;
    }
    // SAFETY: as the caller states.
    String::from_utf8(unsafe { crate::string::bytes(text) }).ok()
}

/// Whether `identifier` is `other` or reaches it through [`CONFORMS`].
fn conforms(identifier: &str, other: &str) -> bool {
    identifier == other
        || CONFORMS
            .iter()
            .find(|(id, _)| *id == identifier)
            .is_some_and(|(_, parents)| parents.iter().any(|parent| conforms(parent, other)))
}

unsafe extern "C" fn type_with_identifier(_class: Id, _sel: Sel, identifier: Id) -> Id {
    // SAFETY: an `NSString` or nil.
    unsafe { text(identifier) }.map_or(std::ptr::null_mut(), |id| lookup(&id))
}

unsafe extern "C" fn type_with_extension(_class: Id, _sel: Sel, extension: Id) -> Id {
    // SAFETY: an `NSString` or nil.
    unsafe { text(extension) }
        .and_then(|tag| {
            corefoundation::uti::identifier_for(corefoundation::uti::FILENAME_EXTENSION, &tag)
        })
        .map_or(std::ptr::null_mut(), |id| lookup(&id))
}

/// The conformance hint follows the legacy tag lookup's current behavior:
/// one identifier per extension, without filtering by the hint.
unsafe extern "C" fn type_with_extension_conforming(
    class: Id,
    sel: Sel,
    extension: Id,
    _conforming: Id,
) -> Id {
    unsafe { type_with_extension(class, sel, extension) }
}

unsafe extern "C" fn type_with_mime_type(_class: Id, _sel: Sel, mime: Id) -> Id {
    // SAFETY: an `NSString` or nil.
    unsafe { text(mime) }
        .and_then(|tag| corefoundation::uti::identifier_for(corefoundation::uti::MIME_TYPE, &tag))
        .map_or(std::ptr::null_mut(), |id| lookup(&id))
}

unsafe extern "C" fn identifier(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: one of this module's types; the string lives as long as it does.
    unsafe { (*(receiver as *const Body)).identifier as Id }
}

/// The preferred tag of `tag_class`, autoreleased, or nil.
///
/// # Safety
///
/// `receiver` is one of this module's types.
unsafe fn preferred_tag(receiver: Id, tag_class: &str) -> Id {
    // SAFETY: as the caller states.
    let identifier = unsafe { identifier_text(receiver) };
    corefoundation::uti::tag_for(&identifier, tag_class).map_or(std::ptr::null_mut(), |tag| {
        objc::arc::autorelease(crate::string::from_str(&tag))
    })
}

unsafe extern "C" fn preferred_extension(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: one of this module's types.
    unsafe { preferred_tag(receiver, corefoundation::uti::FILENAME_EXTENSION) }
}

unsafe extern "C" fn preferred_mime_type(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: one of this module's types.
    unsafe { preferred_tag(receiver, corefoundation::uti::MIME_TYPE) }
}

unsafe extern "C" fn conforms_to_type(receiver: Id, _sel: Sel, other: Id) -> u8 {
    // SAFETY: null or an object; checked to be one of this module's types.
    if other.is_null() || !unsafe { corefoundation::base::is_kind_of(other as CFTypeRef, class()) }
    {
        return 0;
    }
    // SAFETY: both are this module's types.
    let (identifier, other) = unsafe { (identifier_text(receiver), identifier_text(other)) };
    u8::from(conforms(&identifier, &other))
}

unsafe extern "C" fn is_subtype_of_type(receiver: Id, sel: Sel, other: Id) -> u8 {
    // SAFETY: as `conforms_to_type`; a type is not its own subtype.
    u8::from(receiver != other && unsafe { conforms_to_type(receiver, sel, other) } != 0)
}

unsafe extern "C" fn is_supertype_of_type(receiver: Id, sel: Sel, other: Id) -> u8 {
    // SAFETY: null or an object; checked by `conforms_to_type`.
    if other.is_null() || !unsafe { corefoundation::base::is_kind_of(other as CFTypeRef, class()) }
    {
        return 0;
    }
    // SAFETY: as above, with the roles swapped.
    unsafe { is_subtype_of_type(other, sel, receiver) }
}

unsafe extern "C" fn is_dynamic(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: one of this module's types.
    u8::from(unsafe { identifier_text(receiver) }.starts_with(corefoundation::uti::DYNAMIC))
}

unsafe extern "C" fn is_declared_type(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: one of this module's types.
    u8::from(is_declared(&unsafe { identifier_text(receiver) }))
}

fn install_methods(class: &'static Class) {
    let meta = class.metaclass();
    for (name, imp, types) in [
        (
            "typeWithIdentifier:",
            type_with_identifier as *const c_void,
            c"@24@0:8@16",
        ),
        (
            "typeWithFilenameExtension:",
            type_with_extension as *const c_void,
            c"@24@0:8@16",
        ),
        (
            "typeWithFilenameExtension:conformingToType:",
            type_with_extension_conforming as *const c_void,
            c"@32@0:8@16@24",
        ),
        (
            "typeWithMIMEType:",
            type_with_mime_type as *const c_void,
            c"@24@0:8@16",
        ),
    ] {
        meta.replace_method(objc::sel::get(name), imp, types);
    }
    for (name, imp, types) in [
        ("identifier", identifier as *const c_void, c"@16@0:8"),
        ("description", identifier as *const c_void, c"@16@0:8"),
        // No localized type database; preserve the identifier fallback.
        (
            "localizedDescription",
            identifier as *const c_void,
            c"@16@0:8",
        ),
        (
            "preferredFilenameExtension",
            preferred_extension as *const c_void,
            c"@16@0:8",
        ),
        (
            "preferredMIMEType",
            preferred_mime_type as *const c_void,
            c"@16@0:8",
        ),
        (
            "conformsToType:",
            conforms_to_type as *const c_void,
            c"c24@0:8@16",
        ),
        (
            "isSubtypeOfType:",
            is_subtype_of_type as *const c_void,
            c"c24@0:8@16",
        ),
        (
            "isSupertypeOfType:",
            is_supertype_of_type as *const c_void,
            c"c24@0:8@16",
        ),
        ("isDynamic", is_dynamic as *const c_void, c"c16@0:8"),
        ("isDeclared", is_declared_type as *const c_void, c"c16@0:8"),
    ] {
        class.replace_method(objc::sel::get(name), imp, types);
    }
}

/// The class objects and the `UTType*` constants, each a cell holding its type.
pub fn data_symbols() -> Vec<(String, u64)> {
    crate::bootstrap();
    let class = class();
    let mut symbols: Vec<_> = CONSTANTS
        .iter()
        .map(|(symbol, identifier)| {
            (
                (*symbol).to_string(),
                corefoundation::base::global_cell(symbol, intern(identifier) as u64),
            )
        })
        .collect();
    symbols.push((
        "_OBJC_CLASS_$_UTType".to_string(),
        objc::class::ptr(class) as usize as u64,
    ));
    symbols.push((
        "_OBJC_METACLASS_$_UTType".to_string(),
        objc::class::ptr(class.metaclass()) as usize as u64,
    ));
    symbols
}

/// Initialize the class before URL resource values are requested.
pub fn install() {
    class();
}

#[cfg(test)]
pub(crate) unsafe fn identifier_of(object: Id) -> Option<String> {
    if object.is_null() || !unsafe { corefoundation::base::is_kind_of(object as _, class()) } {
        return None;
    }
    Some(unsafe { identifier_text(object) })
}

pub(crate) fn for_path(path: &std::path::Path) -> Option<Id> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let identifier = if metadata.is_dir() {
        "public.folder".to_string()
    } else {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(|e| {
                corefoundation::uti::identifier_for(corefoundation::uti::FILENAME_EXTENSION, e)
            })
            .unwrap_or_else(|| "public.data".to_string())
    };
    Some(objc::arc::retain(intern(&identifier)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_constant_names_a_declared_type() {
        for (symbol, identifier) in CONSTANTS {
            assert!(is_declared(identifier), "{symbol}");
        }
    }

    #[test]
    fn conformance_follows_the_hierarchy() {
        assert!(conforms("com.apple.application-bundle", "com.apple.bundle"));
        assert!(conforms("com.apple.application-bundle", "public.directory"));
        assert!(conforms("public.file-url", "public.data"));
        assert!(!conforms("public.directory", "public.data"));
        assert!(!conforms("public.bundle", "com.apple.application-bundle"));
    }

    #[test]
    fn a_type_is_one_object_per_identifier() {
        crate::bootstrap();
        assert_eq!(lookup("public.vcard"), lookup("public.vcard"));
        assert!(lookup("org.example.undeclared").is_null());
    }
}
