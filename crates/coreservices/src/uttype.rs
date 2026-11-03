//! `UTType`, the Objective-C face of Uniform Type Identifiers that
//! UniformTypeIdentifiers.framework added in macOS 11.
//!
//! One object per identifier, kept for the process, so identity is equality and
//! `NSObject`'s `-isEqual:` and `-hash` are already right. The types are the
//! ones [`crate::uti`] knows plus the hierarchy below; an identifier outside
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
const CONFORMS: &[(&str, &[&str])] = &[
    ("public.item", &[]),
    ("public.content", &[]),
    ("public.data", &["public.item"]),
    ("public.directory", &["public.item"]),
    ("public.folder", &["public.directory"]),
    ("com.apple.package", &["public.directory"]),
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
    ("public.url", &["public.data"]),
    ("public.file-url", &["public.url"]),
    ("public.text", &["public.data", "public.content"]),
    ("public.plain-text", &["public.text"]),
    ("public.utf8-plain-text", &["public.plain-text"]),
    ("public.html", &["public.text"]),
    ("public.rtf", &["public.text"]),
    ("public.json", &["public.text"]),
    ("public.contact", &["public.item"]),
    ("public.vcard", &["public.text", "public.contact"]),
    ("public.image", &["public.data", "public.content"]),
    ("public.png", &["public.image"]),
    ("public.jpeg", &["public.image"]),
    ("public.tiff", &["public.image"]),
    ("com.compuserve.gif", &["public.image"]),
    ("com.microsoft.bmp", &["public.image"]),
    ("public.composite-content", &["public.content"]),
    (
        "com.adobe.pdf",
        &["public.data", "public.composite-content"],
    ),
    ("public.archive", &["public.item"]),
    ("public.zip-archive", &["public.data", "public.archive"]),
];

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
        install(class);
        objc::class::ptr(class) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("the UTType class") }
}

fn is_declared(identifier: &str) -> bool {
    CONFORMS.iter().any(|(id, _)| *id == identifier)
        || crate::uti::TYPES.iter().any(|row| row.0 == identifier)
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
    match is_declared(identifier) || identifier.starts_with(crate::uti::DYNAMIC) {
        true => intern(identifier),
        false => std::ptr::null_mut(),
    }
}

/// # Safety
///
/// `object` is one of this module's types.
unsafe fn identifier_of(object: Id) -> String {
    // SAFETY: this class's layout, as the caller states.
    let text = unsafe { (*(object as *const Body)).identifier };
    // SAFETY: the identifier is a string this module made.
    unsafe { String::from_utf8_lossy(&foundation::string::bytes(text as Id)).into_owned() }
}

/// # Safety
///
/// `text` is an `NSString` or nil.
unsafe fn text(text: Id) -> Option<String> {
    if text.is_null() {
        return None;
    }
    // SAFETY: as the caller states.
    String::from_utf8(unsafe { foundation::string::bytes(text) }).ok()
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
        .and_then(|tag| crate::uti::identifier_for(crate::uti::FILENAME_EXTENSION, &tag))
        .map_or(std::ptr::null_mut(), |id| lookup(&id))
}

unsafe extern "C" fn type_with_mime_type(_class: Id, _sel: Sel, mime: Id) -> Id {
    // SAFETY: an `NSString` or nil.
    unsafe { text(mime) }
        .and_then(|tag| crate::uti::identifier_for(crate::uti::MIME_TYPE, &tag))
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
    let identifier = unsafe { identifier_of(receiver) };
    crate::uti::tag_for(&identifier, tag_class).map_or(std::ptr::null_mut(), |tag| {
        objc::arc::autorelease(foundation::string::from_str(&tag))
    })
}

unsafe extern "C" fn preferred_extension(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: one of this module's types.
    unsafe { preferred_tag(receiver, crate::uti::FILENAME_EXTENSION) }
}

unsafe extern "C" fn preferred_mime_type(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: one of this module's types.
    unsafe { preferred_tag(receiver, crate::uti::MIME_TYPE) }
}

unsafe extern "C" fn conforms_to_type(receiver: Id, _sel: Sel, other: Id) -> u8 {
    // SAFETY: null or an object; checked to be one of this module's types.
    if other.is_null() || !unsafe { corefoundation::base::is_kind_of(other as CFTypeRef, class()) }
    {
        return 0;
    }
    // SAFETY: both are this module's types.
    let (identifier, other) = unsafe { (identifier_of(receiver), identifier_of(other)) };
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
    u8::from(unsafe { identifier_of(receiver) }.starts_with(crate::uti::DYNAMIC))
}

unsafe extern "C" fn is_declared_type(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: one of this module's types.
    u8::from(is_declared(&unsafe { identifier_of(receiver) }))
}

fn install(class: &'static Class) {
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
    foundation::bootstrap();
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
        foundation::bootstrap();
        assert_eq!(lookup("public.vcard"), lookup("public.vcard"));
        assert!(lookup("org.example.undeclared").is_null());
    }
}
