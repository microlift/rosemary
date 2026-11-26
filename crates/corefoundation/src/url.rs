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
    /// The relative reference this URL was made from, when it has a base.
    /// `text` stays the resolved absolute URL so every reader sees one form.
    pub relative: Option<String>,
    /// The retained base URL of a relative URL, or null.
    base: CFTypeRef,
    /// For a file reference URL (`file:///.file/id=...`): the file URL of the
    /// file it names, which is where `path` and `file_path` read from.
    pub reference_target: Option<String>,
}

impl Contents {
    /// The decoded path of a bare reference such as `notes/a%20b?x`.
    pub fn probe_path(text: &str) -> String {
        Contents::bare(text).path()
    }

    fn bare(text: &str) -> Contents {
        Contents {
            text: text.to_string(),
            string: std::ptr::null(),
            relative: None,
            base: std::ptr::null(),
            reference_target: None,
        }
    }

    /// The base URL object of a relative URL, borrowed.
    pub fn base(&self) -> CFTypeRef {
        self.base
    }

    /// The scheme of arbitrary text, as [`Contents::scheme`] reads it.
    pub fn probe(text: &str) -> String {
        Contents::bare(text).scheme()
    }

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
        Some(self.path())
    }

    /// The encoded path, excluding the authority, query and fragment.
    pub fn encoded_path(&self) -> &str {
        &self.text[self.path_range()]
    }

    fn path_range(&self) -> std::ops::Range<usize> {
        let start = if self.scheme().is_empty() {
            0
        } else {
            self.text.find(':').unwrap() + 1
        };
        let rest = &self.text[start..];
        let start = match rest.strip_prefix("//") {
            Some(authority) => {
                start + 2 + authority.find(['/', '?', '#']).unwrap_or(authority.len())
            }
            None => start,
        };
        let end = start
            + self.text[start..]
                .find(['?', '#'])
                .unwrap_or(self.text.len() - start);
        start..end
    }

    /// The authority, including any user information and port.
    pub fn authority(&self) -> Option<&str> {
        let start = if self.scheme().is_empty() {
            0
        } else {
            self.text.find(':')? + 1
        };
        let authority = self.text[start..].strip_prefix("//")?;
        Some(&authority[..authority.find(['/', '?', '#']).unwrap_or(authority.len())])
    }

    /// The decoded user name. Password, query and fragment stay encoded.
    pub fn user(&self) -> Option<String> {
        let (user_info, _) = self.authority()?.rsplit_once('@')?;
        Some(percent_decode(
            user_info.split(':').next().unwrap_or_default(),
            b"",
        ))
    }

    pub fn password(&self) -> Option<&str> {
        self.authority()?
            .rsplit_once('@')?
            .0
            .split_once(':')
            .map(|(_, password)| password)
    }

    pub fn query(&self) -> Option<&str> {
        self.text
            .split('#')
            .next()?
            .split_once('?')
            .map(|(_, query)| query)
    }

    pub fn fragment(&self) -> Option<&str> {
        self.text.split_once('#').map(|(_, fragment)| fragment)
    }

    /// The path component, decoded: everything after the authority and before
    /// the query or fragment.
    pub fn path(&self) -> String {
        if let Some(target) = &self.reference_target {
            return Contents::bare(target).path();
        }
        // Escaped separators and NUL stay literal, as in Foundation, rather
        // than naming a different directory or truncating a C path.
        percent_decode(trim_trailing_slash(self.encoded_path()), b"/\0")
    }

    /// Delete only the final extension, preserving the other URL components.
    pub fn deleting_path_extension(&self) -> String {
        let range = self.path_range();
        let path = &self.text[range.clone()];
        let trimmed = path.trim_end_matches('/');
        let component_start = trimmed.rfind('/').map_or(0, |slash| slash + 1);
        let component = &trimmed[component_start..];
        let Some(dot) = component
            .rfind('.')
            .filter(|dot| *dot > 0 && *dot + 1 < component.len())
        else {
            return self.text.clone();
        };
        let dot = range.start + component_start + dot;
        let end = range.start + trimmed.len();
        format!("{}{}", &self.text[..dot], &self.text[end..])
    }
}

/// Drop a trailing slash, except from the root - which is the one path whose
/// trailing slash is the whole path.
fn trim_trailing_slash(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() && path.starts_with('/') {
        "/"
    } else {
        trimmed
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
pub fn percent_decode(text: &str, preserving: &[u8]) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let decoded = (bytes[index] == b'%' && index + 2 < bytes.len())
            .then(|| {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
                u8::from_str_radix(hex, 16)
                    .ok()
                    .filter(|byte| !preserving.contains(byte))
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
            if !contents.base.is_null() {
                objc::arc::release(contents.base as Id);
            }
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
        string: crate::string::from_bytes(text.as_bytes(), false),
        ..Contents::bare(text)
    };
    // SAFETY: a live instance whose second word is ours to fill in.
    unsafe { (*body).contents = Box::into_raw(Box::new(contents)) };
    body as CFTypeRef
}

/// A URL made from `relative` against `base`, whose resolved form is
/// `absolute`. The base is retained; null makes a plain URL.
pub fn new_relative(absolute: &str, relative: &str, base: CFTypeRef) -> CFTypeRef {
    let url = new(absolute);
    // SAFETY: `url` is a URL `new` just made, with live contents.
    unsafe {
        if let Some(body) = (url as *mut Body).as_mut()
            && let Some(contents) = body.contents.as_mut()
            && !base.is_null()
        {
            contents.relative = Some(relative.to_string());
            contents.base = crate::api::CFRetain(base);
        }
    }
    url
}

/// A file reference URL naming the file at `target` (a file URL's text).
pub fn new_file_reference(id: &str, target: &str) -> CFTypeRef {
    let url = new(&format!("file:///.file/id={id}"));
    // SAFETY: `url` is a URL `new` just made, with live contents.
    unsafe {
        if let Some(body) = (url as *mut Body).as_mut()
            && let Some(contents) = body.contents.as_mut()
        {
            contents.reference_target = Some(target.to_string());
        }
    }
    url
}

/// A path spelled for a URL: reserved bytes percent-escaped.
pub fn encode_path(path: &str) -> String {
    percent_encode(path)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_separate_suffixes_and_preserve_escaped_separators() {
        for (text, path) in [
            ("file:///tmp/a%23b%3Fc?query#fragment", "/tmp/a#b?c"),
            ("FILE:///tmp/directory/?query", "/tmp/directory"),
            ("file:/tmp/directory/", "/tmp/directory"),
            ("file://localhost/tmp/a%2Fb%00c", "/tmp/a%2Fb%00c"),
            ("file:///", "/"),
        ] {
            let url = new(text);
            // SAFETY: this test owns the live URL.
            unsafe {
                assert_eq!(as_url(url).unwrap().file_path().as_deref(), Some(path));
                crate::api::CFRelease(url);
            }
        }
    }

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
