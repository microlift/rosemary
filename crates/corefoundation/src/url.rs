// The C spellings are the interface, as they are in `api`.
#![allow(non_snake_case)]

use std::ffi::c_void;

use objc::class::Class;
use objc::object::Id;

use crate::base::CFTypeRef;

/// A URL object: the `isa`, and the text hanging off it.
#[repr(C)]
struct Body {
    isa: *const Class,
    contents: *mut Contents,
}

pub struct Contents {
    /// The absolute URL, `https://host/path` or `file:///Applications/Foo.app/`.
    pub text: String,
    /// The same text as a stable, borrowed `CFStringRef` for `CFURLGetString`.
    string: CFTypeRef,
}

impl Contents {
    /// The scheme, lowercased, or empty for text that has none.
    ///
    /// Cut at the first `:`, and only when everything before it could be a
    /// scheme: `/tmp/a:b` is a path someone passed by mistake, not a URL whose
    /// scheme is `/tmp/a`.
    pub fn scheme(&self) -> String {
        let Some(end) = self.text.find(':') else {
            return String::new();
        };
        let scheme = &self.text[..end];
        let valid = !scheme.is_empty()
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        match valid {
            true => scheme.to_ascii_lowercase(),
            false => String::new(),
        }
    }

    pub fn is_file_url(&self) -> bool {
        self.scheme() == "file"
    }

    /// The file system path, for a `file:` URL.
    ///
    /// `None` for anything else: a caller asking a `https:` URL for its path
    /// wants the path *component*, which is [`Contents::path`], and answering
    /// with one here would let a file operation open it.
    pub fn file_path(&self) -> Option<String> {
        if !self.is_file_url() {
            return None;
        }
        // `file://<authority>/<path>`; the authority is empty or `localhost`,
        // and either way the path begins at the slash that ends it.
        let rest = self.text.strip_prefix("file://")?;
        let path = match rest.find('/') {
            Some(slash) => &rest[slash..],
            None => "/",
        };
        Some(percent_decode(trim_trailing_slash(path)))
    }

    /// The path component, decoded: everything after the authority and before
    /// the query or fragment.
    pub fn path(&self) -> String {
        if let Some(path) = self.file_path() {
            return path;
        }
        let after_scheme = match self.text.split_once("://") {
            Some((_, rest)) => rest,
            None => return String::new(),
        };
        let path = match after_scheme.find('/') {
            Some(slash) => &after_scheme[slash..],
            None => return String::new(),
        };
        let end = path.find(['?', '#']).unwrap_or(path.len());
        percent_decode(&path[..end])
    }
}

/// Drop a trailing slash, except from the root - which is the one path whose
/// trailing slash is the whole path.
fn trim_trailing_slash(path: &str) -> &str {
    match path.len() > 1 {
        true => path.trim_end_matches('/'),
        false => path,
    }
}

/// The characters a path may carry into a URL unescaped.
///
/// RFC 3986's unreserved set plus the sub-delimiters a file path routinely
/// contains, and `/` itself.
fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~/!$&'()*+,;=:@".contains(&byte)
}

fn percent_encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match unreserved(byte) {
            true => out.push(byte as char),
            false => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The reverse. A `%` that is not followed by two hexadecimal digits is a
/// literal `%`, which is what a path containing one and never encoded produces.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let decoded = (bytes[index] == b'%' && index + 2 < bytes.len())
            .then(|| {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
                u8::from_str_radix(hex, 16).ok()
            })
            .flatten();
        match decoded {
            Some(byte) => {
                out.push(byte);
                index += 3;
            }
            None => {
                out.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The concrete class, under `NSURL` so that a guest's `isKindOfClass:` and its
/// `_OBJC_CLASS_$_NSURL` binding agree about the object.
pub fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        let base = crate::base::abstract_class(c"NSURL", None);
        crate::base::note_type_class(base);
        let class = crate::base::register(c"__NSCFURL", Some(base), size_of::<Body>(), false);
        class.replace_method(
            objc::sel::get("dealloc"),
            dealloc as *const c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: the cell holds a pointer to a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` is reached with the last reference gone, and the
    // receiver is a `Body` whose `contents` came from `Box::into_raw`.
    unsafe {
        let body = receiver as *mut Body;
        if !(*body).contents.is_null() {
            let contents = Box::from_raw((*body).contents);
            objc::arc::release(contents.string as Id);
            drop(contents);
            (*body).contents = std::ptr::null_mut();
        }
        objc::arc::finalize(receiver);
    }
}

/// A URL holding `text`, owned by the caller.
pub fn new(text: &str) -> CFTypeRef {
    // SAFETY: `Body` begins with `isa` and is the layout `class` was registered
    // with.
    let body: *mut Body = unsafe { crate::base::allocate(class()) };
    if body.is_null() {
        return std::ptr::null();
    }
    let contents = Contents {
        text: text.to_string(),
        string: crate::string::from_bytes(text.as_bytes(), false),
    };
    // SAFETY: a live instance whose second word is ours to fill in.
    unsafe { (*body).contents = Box::into_raw(Box::new(contents)) };
    body as CFTypeRef
}

impl Contents {
    /// The stable string object owned by this URL.
    pub fn string(&self) -> CFTypeRef {
        self.string
    }
}

/// A file URL naming `path`, owned by the caller.
///
/// `directory` decides the trailing slash, which is what a caller appending a
/// name onto the result depends on.
pub fn new_file_url(path: &str, directory: bool) -> CFTypeRef {
    let trimmed = trim_trailing_slash(path);
    let mut text = String::from("file://");
    text.push_str(&percent_encode(trimmed));
    if directory && !text.ends_with('/') {
        text.push('/');
    }
    new(&text)
}

/// The contents of `url`, if it is one.
///
/// # Safety
///
/// `url` must be null or an object this runtime allocated.
pub unsafe fn as_url<'a>(url: CFTypeRef) -> Option<&'a Contents> {
    if !unsafe { crate::base::is_kind_of(url, class()) } {
        return None;
    }
    // SAFETY: an instance of `class()` is a `Body` with a live `contents`.
    unsafe {
        let body = url as *const Body;
        (!(*body).contents.is_null()).then(|| &*(*body).contents)
    }
}

//
// -- the private property-list representation --------------------------------
//
// `_CFURLCopyPropertyListRepresentation` and `_CFURLCreateFromPropertyList-
// Representation`, which the Mach-O exports as `__CFURLCopyPropertyList-
// Representation` - the single underscore of the C name plus the Mach-O's own.
// The Finder uses them to hand a URL to another process as a property list, and
// `CFURL.c` carries both with the comment "Private API for Finder to use".
//
// Chromium's only caller is `chrome/browser/mac/dock.mm`, which reads the
// `com.apple.dock` `persistent-apps` plist and turns each app's `file-data` into
// a URL so it can add its own tile (`0x5af6fe6`-`0x5af708d`, and the nil check at
// `0x5af708d`'s `test rax,rax`).  So a null answer is a path the caller already
// handles - and the whole feature is macOS shell integration that a headless run
// has no use for.
//
// The dictionary is four keys, and its shape is not a guess: it is
// `__CFURLCopyPropertyListKeysAndValues` in `CFURL.c`, which is the function
// `_CFURLCopyPropertyListRepresentation` is a two-line wrapper around.

/// `FULL_URL_REPRESENTATION`, the value of `_CFURLStringType` that means "the
/// string is the whole URL" rather than a path in some `CFURLPathStyle`.
///
/// `CFURL.c` says of it: "The value of FULL_URL_REPRESENTATION must not be in
/// the CFURLPathStyle enums. Also, its value is exposed via
/// `_CFURLCopyPropertyListRepresentation` to the Finder so don't change it."
const FULL_URL_REPRESENTATION: i64 = 0xF;

/// The `CFURLPathStyle` values a `_...StringType` may carry instead: 0, 1 and 2.
const PATH_STYLES: [i64; 3] = [
    0, // kCFURLPOSIXPathStyle
    1, // kCFURLHFSPathStyle
    2, // kCFURLWindowsPathStyle
];

/// `CFPropertyListRef _CFURLCopyPropertyListRepresentation(CFURLRef url)`
pub unsafe extern "C" fn _CFURLCopyPropertyListRepresentation(url: CFTypeRef) -> CFTypeRef {
    let Some(contents) = (unsafe { as_url(url) }) else {
        return std::ptr::null();
    };
    let key = |name: &str| crate::base::interned_name(name) as CFTypeRef;
    let text = contents.string();
    // SAFETY: a live string of ours; the dictionary retains it.
    let string_value = unsafe { crate::api::CFRetain(text) };
    let pairs = [
        (
            key("_CFURLStringType"),
            number(FULL_URL_REPRESENTATION) as usize as CFTypeRef,
        ),
        (key("_CFURLString"), string_value),
    ];
    let dict = crate::collections::new_dictionary(
        &pairs,
        crate::callbacks::Policy::from_key_callbacks(),
        crate::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: the value was retained for the dictionary above and the dictionary
    // has taken its own reference.
    unsafe { crate::api::CFRelease(string_value) };
    dict
}

/// `CFURLRef _CFURLCreateFromPropertyListRepresentation(CFAllocatorRef, CFPropertyListRef)`
///
/// Null for anything that is not exactly the dictionary
/// `_CFURLCopyPropertyListRepresentation` produces, which is what `CFURL.c`
/// returns in every one of its failure cases: the type is not a dictionary, the
/// string is missing or is not a string, a type number is missing or is not a
/// number or is not one of the four accepted values, or the base string is
/// present without its type.
pub unsafe extern "C" fn _CFURLCreateFromPropertyListRepresentation(
    _alloc: crate::api::CFAllocatorRef,
    plist: CFTypeRef,
) -> CFTypeRef {
    // SAFETY: the property list, which may be null.
    let Some(dictionary) = (unsafe { crate::collections::dictionary(plist) }) else {
        return std::ptr::null();
    };
    let get = |name: &str| -> CFTypeRef {
        let key = crate::base::interned_name(name) as CFTypeRef;
        dictionary.get(key)
    };
    // Two values out of a live dictionary, both checked for type below.
    let text = get("_CFURLString");
    let style = get("_CFURLStringType");
    if text.is_null() || style.is_null() {
        return std::ptr::null();
    }
    if unsafe { crate::api::CFGetTypeID(text) } != unsafe { crate::api::CFStringGetTypeID() } {
        return std::ptr::null();
    }
    let Some(style) = number_value(style) else {
        return std::ptr::null();
    };
    if style != FULL_URL_REPRESENTATION && !PATH_STYLES.contains(&style) {
        return std::ptr::null();
    }
    // The base is optional, and its type number is not: a base string with no
    // type is one of `CFURL.c`'s refusals.
    let base = match get("_CFURLBaseURLString") {
        base if base.is_null() => std::ptr::null(),
        base => {
            if unsafe { crate::api::CFGetTypeID(base) } != unsafe { crate::api::CFStringGetTypeID() }
            {
                return std::ptr::null();
            }
            let base_style = get("_CFURLBaseStringType");
            let Some(base_style) = number_value(base_style) else {
                return std::ptr::null();
            };
            if base_style != FULL_URL_REPRESENTATION && !PATH_STYLES.contains(&base_style) {
                return std::ptr::null();
            }
            unsafe { build(base, base_style) }
        }
    };
    let url = unsafe { build(text, style) };
    // SAFETY: `build` handed back a reference of its own, and the dictionary is
    // still holding its base until this point.
    unsafe {
        if !base.is_null() {
            crate::api::CFRelease(base);
        }
    }
    url
}

/// A URL from one of the representation's two string-and-style pairs.
unsafe fn build(text: CFTypeRef, style: i64) -> CFTypeRef {
    // SAFETY: a string; the text is read and copied.
    let Some(contents) = (unsafe { crate::string::as_string(text) }) else {
        return std::ptr::null();
    };
    let value = String::from_utf8_lossy(&contents.utf8()).into_owned();
    match style {
        FULL_URL_REPRESENTATION => new(&value),
        // A path style, and the last character says whether it names a
        // directory - which is what `CFURLCreateWithFileSystemPathRelativeToBase`
        // takes.  The style itself is a `CFURLPathStyle`, of which this runtime
        // implements the POSIX one; a caller asking for an HFS or Windows path
        // gets a POSIX path, which is the only kind that exists here, and the
        // value is a four-number enumeration rather than a request this runtime
        // could confuse with anything.
        _ => new_file_url(&value, value.ends_with('/')),
    }
}

/// A `CFNumberRef` of `kCFNumberSInt32Type` holding `value`.
fn number(value: i64) -> CFTypeRef {
    crate::collections::new_number_of_type(
        crate::collections::Number::Integer(value),
        crate::collections::number_type::SINT32,
    )
}

/// The `SInt32` out of a `CFNumberRef`, or `None` for anything else.
fn number_value(number: CFTypeRef) -> Option<i64> {
    // SAFETY: null-safe; `number_value` reads only a number of this runtime.
    match unsafe { crate::collections::number_value(number) } {
        Some((crate::collections::Number::Integer(value), _)) => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property a bundle depends on: a path with characters a URL reserves
    /// comes back as the path that went in.
    #[test]
    fn a_file_path_round_trips_through_its_url() {
        for path in [
            "/Applications/A Game.app/Contents/Resources",
            "/tmp/100% sure",
            "/tmp/a#b?c",
            "/",
        ] {
            crate::bootstrap();
            let url = new_file_url(path, false);
            // SAFETY: a URL this call just made.
            let contents = unsafe { as_url(url) }.expect("a URL");
            assert_eq!(contents.file_path().as_deref(), Some(path), "{path}");
            // SAFETY: nothing else holds a reference.
            unsafe { crate::api::CFRelease(url) };
        }
    }

    #[test]
    fn a_directory_url_keeps_its_trailing_slash() {
        crate::bootstrap();
        let url = new_file_url("/tmp/dir", true);
        // SAFETY: a URL this call just made.
        let contents = unsafe { as_url(url) }.expect("a URL");
        assert_eq!(contents.text, "file:///tmp/dir/");
        // The path is still the directory, without the slash the text carries.
        assert_eq!(contents.file_path().as_deref(), Some("/tmp/dir"));
        // SAFETY: nothing else holds a reference.
        unsafe { crate::api::CFRelease(url) };
    }

    #[test]
    fn a_http_url_reports_its_scheme_and_path_but_no_file_path() {
        crate::bootstrap();
        let url = new("https://example.com/a/b?c=d");
        // SAFETY: a URL this call just made.
        let contents = unsafe { as_url(url) }.expect("a URL");
        assert_eq!(contents.scheme(), "https");
        assert_eq!(contents.path(), "/a/b");
        assert!(!contents.is_file_url());
        assert_eq!(contents.file_path(), None, "not a file to be opened");
        // SAFETY: nothing else holds a reference.
        unsafe { crate::api::CFRelease(url) };
    }


    /// The Finder's URL property-list pair, round-tripped.
    ///
    /// The dictionary is not a guess: `CFURL.c`'s
    /// `__CFURLCopyPropertyListKeysAndValues` builds it with four keys, and
    /// `_CFURLCreateFromPropertyListRepresentation` reads exactly those four
    /// back and refuses anything else.
    #[test]
    fn a_url_survives_the_property_list_representation() {
        crate::bootstrap();
        for text in [
            "file:///Applications/Chromium.app/",
            "https://example.com/a/b?c=d#e",
        ] {
            let url = new(text);
            // SAFETY: a URL this test made.
            let plist = unsafe { _CFURLCopyPropertyListRepresentation(url) };
            assert!(!plist.is_null(), "{text}: a dictionary");
            // SAFETY: a dictionary this call made, and the guest checks exactly
            // this - `CFGetTypeID` against `CFDictionaryGetTypeID` at
            // `0x5af78a4`.
            assert_eq!(
                unsafe { crate::api::CFGetTypeID(plist) },
                unsafe { crate::api::CFDictionaryGetTypeID() }
            );
            // SAFETY: a dictionary this call made and a null allocator.
            let back = unsafe { _CFURLCreateFromPropertyListRepresentation(std::ptr::null(), plist) };
            assert!(!back.is_null(), "{text}: and a URL back");
            // SAFETY: a URL this call made.
            let contents = unsafe { as_url(back) }.expect("a URL");
            assert_eq!(contents.text, text);
            // SAFETY: nothing else holds a reference.
            unsafe {
                crate::api::CFRelease(back);
                crate::api::CFRelease(plist);
                crate::api::CFRelease(url);
            }
        }
    }

    /// Every refusal `_CFURLCreateFromPropertyListRepresentation` in `CFURL.c`
    /// has, because a caller that gets a URL where it should have got null is
    /// worse than one that gets the refusal it can see.
    #[test]
    fn a_malformed_representation_is_null() {
        crate::bootstrap();
        let string = crate::string::from_bytes(b"file:///tmp", false);
        let good_type = number(FULL_URL_REPRESENTATION);

        // A dictionary with the two keys, and one with each of them missing.
        let cases: Vec<Vec<(CFTypeRef, CFTypeRef)>> = vec![
            // Not a dictionary at all.
            Vec::new(),
            // No string.
            vec![(crate::base::interned_name("_CFURLStringType") as CFTypeRef, good_type)],
            // No type.
            vec![(crate::base::interned_name("_CFURLString") as CFTypeRef, string)],
            // A type that is not one of the four accepted values.
            vec![
                (crate::base::interned_name("_CFURLString") as CFTypeRef, string),
                (crate::base::interned_name("_CFURLStringType") as CFTypeRef, number(99)),
            ],
            // A base string with no base type.
            vec![
                (crate::base::interned_name("_CFURLString") as CFTypeRef, string),
                (crate::base::interned_name("_CFURLStringType") as CFTypeRef, good_type),
                (
                    crate::base::interned_name("_CFURLBaseURLString") as CFTypeRef,
                    string,
                ),
            ],
        ];
        for (index, pairs) in cases.into_iter().enumerate() {
            let plist = if pairs.is_empty() {
                // A non-dictionary object: a string is the cheapest one.
                string
            } else {
                crate::collections::new_dictionary(
                    &pairs,
                    crate::callbacks::Policy::from_key_callbacks(),
                    crate::callbacks::Policy::from_type_callbacks(),
                    false,
                )
            };
            // SAFETY: whatever this case is, which is what the point is.
            let url = unsafe { _CFURLCreateFromPropertyListRepresentation(std::ptr::null(), plist) };
            assert!(url.is_null(), "case {index} is refused");
            if !pairs.is_empty() {
                // SAFETY: a dictionary this test made.
                unsafe { crate::api::CFRelease(plist) };
            }
        }
        // SAFETY: a string this test made.
        unsafe { crate::api::CFRelease(string) };
    }

    /// Text with a colon in it is not a scheme just because it has one.
    #[test]
    fn a_bare_path_has_no_scheme() {
        crate::bootstrap();
        let url = new("/tmp/a:b");
        // SAFETY: a URL this call just made.
        let contents = unsafe { as_url(url) }.expect("a URL");
        assert_eq!(contents.scheme(), "");
        // SAFETY: nothing else holds a reference.
        unsafe { crate::api::CFRelease(url) };
    }
}
