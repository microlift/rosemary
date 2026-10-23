//! `_CFCopySystemVersionDictionary` and `_CFCopyServerVersionDictionary`, from
//! CF's `CFUtilities.c`: the contents of
//! `/System/Library/CoreServices/SystemVersion.plist` (or `ServerVersion.plist`)
//! as a mutable dictionary, with the `Version`, `Build` and
//! `FullVersionString` entries added.
//!
//! A prefix need not have a `SystemVersion.plist`, and when it has none the
//! document is the one this runtime's macOS would have: the version and build
//! `kern.osproductversion` and `kern.osversion` report, so the answers agree
//! with each other. A server's file is never made up: macOS is not macOS
//! Server, so with no file that one is NULL, as it is on a Mac.

#![allow(non_snake_case, reason = "the C spellings are the interface")]

use crate::api;
use crate::base::{CFTypeRef, interned_name};
use crate::callbacks::Policy;
use crate::collections;
use crate::plist::Value;

const SYSTEM_PATH: &str = "/System/Library/CoreServices/SystemVersion.plist";
const SERVER_PATH: &str = "/System/Library/CoreServices/ServerVersion.plist";

/// The `_kCFSystemVersion*Key` globals, each a constant string of its text.
pub(crate) const KEYS: &[(&str, &str)] = &[
    ("__kCFSystemVersionProductNameKey", "ProductName"),
    ("__kCFSystemVersionProductCopyrightKey", "ProductCopyright"),
    ("__kCFSystemVersionProductVersionKey", "ProductVersion"),
    (
        "__kCFSystemVersionProductVersionExtraKey",
        "ProductVersionExtra",
    ),
    (
        "__kCFSystemVersionProductUserVisibleVersionKey",
        "ProductUserVisibleVersion",
    ),
    ("__kCFSystemVersionBuildVersionKey", "ProductBuildVersion"),
    ("__kCFSystemVersionProductVersionStringKey", "Version"),
    ("__kCFSystemVersionBuildStringKey", "Build"),
];

fn key(text: &str) -> CFTypeRef {
    interned_name(text) as usize as CFTypeRef
}

fn string(text: &str) -> CFTypeRef {
    crate::string::from_bytes(text.as_bytes(), false)
}

/// The plist at a guest `path`, as a mutable dictionary, or `None` when it is
/// missing or is not a dictionary.
fn read(path: &str) -> Option<CFTypeRef> {
    let guest = std::ffi::CString::new(path).ok()?;
    let host = prefix::ns::to_host(&guest);
    let bytes = std::fs::read(
        std::os::unix::ffi::OsStrExt::from_bytes(host.to_bytes()) as &std::ffi::OsStr
    )
    .ok()?;
    let value = crate::plist::parse(&bytes).ok()?;
    if !matches!(value, Value::Dictionary(_)) {
        return None;
    }
    crate::propertylist::mutable_object_from_value(&value)
}

/// The document a Mac's `SystemVersion.plist` holds, for this runtime's
/// version: macOS 11 and later name themselves `macOS`.
fn synthesized() -> CFTypeRef {
    let (major, _, _) = darwin::sysctl::macos_version();
    let name = match major >= 11 {
        true => "macOS",
        false => "Mac OS X",
    };
    let version = darwin::sysctl::product_version();
    let entries = [
        ("ProductName", string(name)),
        ("ProductVersion", string(&version)),
        ("ProductUserVisibleVersion", string(&version)),
        ("ProductBuildVersion", string(&darwin::sysctl::build())),
        ("ProductCopyright", string("1983-2024 Apple Inc.")),
    ];
    let pairs: Vec<_> = entries.iter().map(|(k, v)| (key(k), *v)).collect();
    let dictionary = collections::new_dictionary(
        &pairs,
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        true,
    );
    for (_, value) in entries {
        // SAFETY: made above; the dictionary holds its own reference.
        unsafe { api::CFRelease(value) };
    }
    dictionary
}

/// A string entry of `dictionary`, as text.
fn text_of(dictionary: CFTypeRef, name: &str) -> Option<String> {
    // SAFETY: a live dictionary of this module's.
    let value = unsafe { api::CFDictionaryGetValue(dictionary, key(name)) };
    // SAFETY: null or a live object; `as_string` checks the class.
    let string = unsafe { crate::string::as_string(value) }?;
    Some(String::from_utf8_lossy(&string.utf8()).into_owned())
}

/// What `_CFCopyVersionDictionary` adds to the document: the English
/// `Version` and `Build` - there is no `SystemVersion` table here to localize
/// them from - and `FullVersionString`, `Version 15.0 (Build 24A335)`, with
/// the version extra after the version when there is one.
fn completed(dictionary: CFTypeRef) -> CFTypeRef {
    let mut version = text_of(dictionary, "ProductVersion");
    if let (Some(v), Some(extra)) = (&version, text_of(dictionary, "ProductVersionExtra")) {
        version = Some(format!("{v} {extra}"));
    }
    let build = text_of(dictionary, "ProductBuildVersion");
    let full = format!(
        "Version {} (Build {})",
        version.as_deref().unwrap_or("?"),
        build.as_deref().unwrap_or("?"),
    );
    for (name, value) in [
        ("Version", "Version"),
        ("Build", "Build"),
        ("FullVersionString", &full),
    ] {
        let value = string(value);
        // SAFETY: a mutable dictionary of this module's, and a live string.
        unsafe {
            api::CFDictionarySetValue(dictionary, key(name), value);
            api::CFRelease(value);
        }
    }
    dictionary
}

/// `CFDictionaryRef _CFCopySystemVersionDictionary(void)`.
pub unsafe extern "C" fn _CFCopySystemVersionDictionary() -> CFTypeRef {
    crate::bootstrap();
    completed(read(SYSTEM_PATH).unwrap_or_else(synthesized))
}

/// `CFDictionaryRef _CFCopyServerVersionDictionary(void)`: NULL without a
/// `ServerVersion.plist`, which is every Mac that is not a server.
pub unsafe extern "C" fn _CFCopyServerVersionDictionary() -> CFTypeRef {
    crate::bootstrap();
    match read(SERVER_PATH) {
        Some(dictionary) => completed(dictionary),
        None => std::ptr::null(),
    }
}

/// `CFStringRef CFCopySystemVersionString(void)`: the server's
/// `FullVersionString`, or the system's.
pub unsafe extern "C" fn CFCopySystemVersionString() -> CFTypeRef {
    // SAFETY: no arguments.
    let mut dictionary = unsafe { _CFCopyServerVersionDictionary() };
    if dictionary.is_null() {
        // SAFETY: as above.
        dictionary = unsafe { _CFCopySystemVersionDictionary() };
    }
    // SAFETY: a live dictionary, released after its value is retained.
    unsafe {
        let value = api::CFDictionaryGetValue(dictionary, key("FullVersionString"));
        let value = match value.is_null() {
            true => value,
            false => api::CFRetain(value),
        };
        api::CFRelease(dictionary);
        value
    }
}

exports::exports! {
    functions {
        "_CFCopySystemVersionDictionary" => _CFCopySystemVersionDictionary,
        "_CFCopyServerVersionDictionary" => _CFCopyServerVersionDictionary,
        "CFCopySystemVersionString" => CFCopySystemVersionString,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_version_agrees_with_the_kernel() {
        // SAFETY: no arguments; the dictionary is released below.
        let dictionary = unsafe { _CFCopySystemVersionDictionary() };
        assert!(!dictionary.is_null());
        let version = darwin::sysctl::product_version();
        let build = darwin::sysctl::build();
        if text_of(dictionary, "ProductName").as_deref() == Some("macOS") {
            assert_eq!(text_of(dictionary, "ProductVersion"), Some(version.clone()));
            assert_eq!(
                text_of(dictionary, "ProductBuildVersion"),
                Some(build.clone())
            );
        }
        assert_eq!(text_of(dictionary, "Version").as_deref(), Some("Version"));
        assert_eq!(text_of(dictionary, "Build").as_deref(), Some("Build"));
        let full = text_of(dictionary, "FullVersionString").expect("a full version");
        assert!(
            full.starts_with("Version ") && full.contains("(Build "),
            "{full}"
        );
        // SAFETY: the reference made above.
        unsafe { api::CFRelease(dictionary) };

        // SAFETY: no arguments.
        let string = unsafe { CFCopySystemVersionString() };
        assert!(!string.is_null());
        // SAFETY: a live string.
        let text = unsafe { crate::string::as_string(string) }.expect("a string");
        assert_eq!(String::from_utf8_lossy(&text.utf8()), full);
        unsafe { api::CFRelease(string) };
    }

    #[test]
    fn a_version_extra_follows_the_version() {
        crate::bootstrap();
        let dictionary = synthesized();
        let extra = string("(a)");
        // SAFETY: a mutable dictionary of this module's and a live string.
        unsafe {
            api::CFDictionarySetValue(dictionary, key("ProductVersionExtra"), extra);
            api::CFRelease(extra);
        }
        let dictionary = completed(dictionary);
        let full = text_of(dictionary, "FullVersionString").unwrap();
        let version = darwin::sysctl::product_version();
        let build = darwin::sysctl::build();
        assert_eq!(full, format!("Version {version} (a) (Build {build})"));
        // SAFETY: the reference made above.
        unsafe { api::CFRelease(dictionary) };
    }
}
