use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

#[repr(C)]
struct Body {
    isa: *const Class,
    contents: *mut Contents,
}

struct Contents {
    domain: String,
    code: i64,
    description: String,
    /// The dictionary a guest made the error with, retained, or null for one
    /// this runtime made, whose `-userInfo` is built from `description`.
    user_info: Id,
}

pub const NSURL_ERROR_DOMAIN: &str = "NSURLErrorDomain";

/// `NSURLErrorUnknown`. The URL error codes are negative, and this is the one
/// for a failure with no more specific code - which is every failure here, since
/// what this runtime learns from a transport error is a message rather than a
/// classification.
pub const NSURL_ERROR_UNKNOWN: i64 = -1;

fn class() -> &'static Class {
    corefoundation::base::register(c"NSError", None, size_of::<Body>(), false)
}

/// An autoreleased `NSError`.
pub fn make(domain: &str, code: i64, description: &str) -> Id {
    // SAFETY: nil carries no user-info dictionary.
    unsafe { make_with_user_info(domain, code, description, std::ptr::null_mut()) }
}

/// A new error at +1, as `CFErrorCreate` makes one: `description` under
/// `NSLocalizedDescription` in its user info, beside each of `entries` - an
/// underlying error under `NSUnderlyingError`, say. `description` wins over
/// an `entries` item under `NSLocalizedDescription`, and a null result (the
/// allocation failed) leaves nothing owned.
pub fn create(domain: &str, code: i64, description: &str, entries: &[(&str, Id)]) -> Id {
    let text = crate::string::from_str(description);
    let mut pairs = vec![(crate::string::from_str("NSLocalizedDescription"), text)];
    pairs.extend(
        entries
            .iter()
            .filter(|&&(key, _)| key != "NSLocalizedDescription")
            .map(|&(key, value)| (crate::string::from_str(key), objc::arc::retain(value))),
    );
    let as_cf: Vec<_> = pairs
        .iter()
        .map(|&(key, value)| {
            (
                key as corefoundation::base::CFTypeRef,
                value as corefoundation::base::CFTypeRef,
            )
        })
        .collect();
    let user_info = corefoundation::collections::new_dictionary(
        &as_cf,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    ) as Id;
    for (key, value) in pairs {
        objc::arc::release(key);
        objc::arc::release(value);
    }
    let error = made(Contents {
        domain: domain.to_string(),
        code,
        description: description.to_string(),
        user_info,
    });
    if error.is_null() {
        objc::arc::release(user_info);
    }
    error
}

/// An autoreleased error retaining the supplied user-info dictionary.
///
/// # Safety
///
/// `user_info` is nil or a live dictionary.
pub(crate) unsafe fn make_with_user_info(
    domain: &str,
    code: i64,
    description: &str,
    user_info: Id,
) -> Id {
    objc::arc::autorelease(made(Contents {
        domain: domain.to_string(),
        code,
        description: description.to_string(),
        user_info: objc::arc::retain(user_info),
    }))
}

/// A new error holding `contents`, at +1.
fn made(contents: Contents) -> Id {
    // SAFETY: `Body` begins with `isa` and is the layout `class` was registered
    // with.
    let body: *mut Body = unsafe { corefoundation::base::allocate(class()) };
    if body.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a live instance whose second word is ours to fill in.
    unsafe { (*body).contents = Box::into_raw(Box::new(contents)) };
    body as Id
}

/// The text of a string object, or `None` for nil or anything else.
///
/// # Safety
///
/// `object` is nil or a live object.
unsafe fn string_text(object: Id) -> Option<String> {
    unsafe { corefoundation::string::as_string(object as corefoundation::base::CFTypeRef) }
        .map(|s| String::from_utf16_lossy(&s.utf16()))
}

/// An error from a guest's domain, code and dictionary, at +1.
///
/// # Safety
///
/// `domain` is a string or nil, and `info` a dictionary or nil.
unsafe fn from_parts(domain: Id, code: i64, info: Id) -> Id {
    let domain = unsafe { string_text(domain) }.unwrap_or_default();
    let described = (!info.is_null())
        .then(|| {
            let key = crate::string::from_str("NSLocalizedDescription");
            // SAFETY: a dictionary and a key this call owns.
            let value =
                unsafe { corefoundation::api::CFDictionaryGetValue(info as _, key as _) as Id };
            unsafe { corefoundation::api::CFRelease(key as _) };
            unsafe { string_text(value) }
        })
        .flatten();
    let description = described.unwrap_or_else(|| {
        format!("The operation couldn\u{2019}t be completed. ({domain} error {code}.)")
    });
    made(Contents {
        domain,
        code,
        description,
        user_info: objc::arc::retain(info),
    })
}

unsafe extern "C" fn error_with_domain(
    _class: Id,
    _sel: Sel,
    domain: Id,
    code: i64,
    info: Id,
) -> Id {
    // SAFETY: the caller's string and dictionary.
    objc::arc::autorelease(unsafe { from_parts(domain, code, info) })
}

unsafe extern "C" fn init_with_domain(
    receiver: Id,
    _sel: Sel,
    domain: Id,
    code: i64,
    info: Id,
) -> Id {
    objc::arc::release(receiver);
    // SAFETY: as above.
    unsafe { from_parts(domain, code, info) }
}

/// # Safety
///
/// `receiver` must be an `NSError` this module made.
unsafe fn contents<'a>(receiver: Id) -> Option<&'a Contents> {
    if receiver.is_null() {
        return None;
    }
    unsafe {
        let body = receiver as *const Body;
        (!(*body).contents.is_null()).then(|| &*(*body).contents)
    }
}

unsafe extern "C" fn domain(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is an `NSError` this module made.
    match unsafe { contents(receiver) } {
        Some(c) => autoreleased(&c.domain),
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn code(receiver: Id, _sel: Sel) -> i64 {
    unsafe { contents(receiver) }.map_or(0, |c| c.code)
}

unsafe extern "C" fn localized_description(receiver: Id, _sel: Sel) -> Id {
    match unsafe { contents(receiver) } {
        Some(c) => autoreleased(&c.description),
        None => std::ptr::null_mut(),
    }
}

/// `-userInfo`: the description under its documented key, and nothing else.
///
/// A dictionary rather than nil, because a caller reads
/// `userInfo[NSLocalizedDescriptionKey]` directly as often as it sends
/// `-localizedDescription`.
unsafe extern "C" fn user_info(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is an `NSError` this module made.
    let Some(c) = (unsafe { contents(receiver) }) else {
        return std::ptr::null_mut();
    };
    if !c.user_info.is_null() {
        return c.user_info;
    }
    let key = crate::string::from_str("NSLocalizedDescription");
    let value = crate::string::from_str(&c.description);
    let dictionary = corefoundation::collections::new_dictionary(
        &[(
            key as corefoundation::base::CFTypeRef,
            value as corefoundation::base::CFTypeRef,
        )],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: the dictionary retained both, and this call owns the two it made.
    unsafe {
        corefoundation::api::CFRelease(key as corefoundation::base::CFTypeRef);
        corefoundation::api::CFRelease(value as corefoundation::base::CFTypeRef);
        corefoundation::api::CFAutorelease(dictionary) as Id
    }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: `-dealloc` is reached with the last reference gone, and the
    // receiver is a `Body` whose `contents` came from `Box::into_raw`.
    unsafe {
        let body = receiver as *mut Body;
        if !(*body).contents.is_null() {
            objc::arc::release((*(*body).contents).user_info);
            drop(Box::from_raw((*body).contents));
            (*body).contents = std::ptr::null_mut();
        }
    }
    objc::arc::finalize(receiver);
}

/// The value a guest's user-info dictionary holds under `key`, or nil. An
/// error this runtime made carries only its description.
unsafe fn user_info_value(receiver: Id, key: &str) -> Id {
    // SAFETY: the receiver is an `NSError`.
    let Some(c) = (unsafe { contents(receiver) }) else {
        return std::ptr::null_mut();
    };
    if c.user_info.is_null() {
        return std::ptr::null_mut();
    }
    let key = crate::string::from_str(key);
    // SAFETY: a dictionary the error retains, and a string this call made.
    let value = unsafe {
        corefoundation::api::CFDictionaryGetValue(
            c.user_info as corefoundation::base::CFTypeRef,
            key as corefoundation::base::CFTypeRef,
        )
    };
    objc::arc::release(key);
    value as Id
}

unsafe extern "C" fn localized_failure_reason(receiver: Id, _sel: Sel) -> Id {
    unsafe { user_info_value(receiver, "NSLocalizedFailureReason") }
}

unsafe extern "C" fn localized_recovery_suggestion(receiver: Id, _sel: Sel) -> Id {
    unsafe { user_info_value(receiver, "NSLocalizedRecoverySuggestion") }
}

unsafe extern "C" fn localized_recovery_options(receiver: Id, _sel: Sel) -> Id {
    unsafe { user_info_value(receiver, "NSLocalizedRecoveryOptions") }
}

fn autoreleased(text: &str) -> Id {
    let string = crate::string::from_str(text);
    // SAFETY: a string this call just created.
    unsafe { corefoundation::api::CFAutorelease(string as corefoundation::base::CFTypeRef) as Id }
}

/// The CoreFoundation spellings, for the `CFErrorRef` an `NSError` is.
pub mod cf {
    // The C spellings are the interface; renaming them would only hide it.
    #![allow(non_snake_case)]

    use objc::object::Id;
    use objc::sel::Sel;

    /// `CFStringRef CFErrorCopyDescription(CFErrorRef err)`.
    ///
    /// What Apple's `CFError.c` answers for a bridged error: its
    /// `-localizedDescription`, sent so that a subclass's own is the one
    /// read, and retained, as a `Copy` is.
    unsafe extern "C" fn CFErrorCopyDescription(err: Id) -> Id {
        if err.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: `objc_msgSend`, called as `-localizedDescription` is.
        let description = unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            send(err, objc::sel::get("localizedDescription"))
        };
        match description.is_null() {
            true => description,
            false => objc::arc::retain(description),
        }
    }

    /// `CFIndex CFErrorGetCode(CFErrorRef)`: CFIndex is a signed pointer-width
    /// integer. CFError and NSError are toll-free bridged, so use `-code` to
    /// retrieve the error's actual domain-specific value without truncation.
    unsafe extern "C" fn CFErrorGetCode(err: Id) -> isize {
        if err.is_null() {
            return 0;
        }
        // SAFETY: the caller's live CFError handle responds to -code and
        // Objective-C NSInteger has the same width/sign as CFIndex.
        unsafe {
            let msg: unsafe extern "C" fn(Id, Sel) -> isize =
                std::mem::transmute(objc::msg::entry());
            msg(err, objc::sel::get("code"))
        }
    }

    exports::exports! {
        functions {
            "CFErrorCopyDescription" => CFErrorCopyDescription,
            "CFErrorGetCode" => CFErrorGetCode,
        }
    }

    #[cfg(test)]
    #[test]
    fn cf_error_get_code_preserves_signed_cfindex() {
        crate::bootstrap();
        let error_code: i64 = -0x1_0000_0001;
        let error = super::create("NSOSStatusErrorDomain", error_code, "test", &[]);
        assert!(!error.is_null());
        // SAFETY: the error above is an owned Foundation NSError handle.
        unsafe {
            assert_eq!(CFErrorGetCode(error), error_code as isize);
            objc::arc::release(error);
        }
    }
}

pub(crate) fn install() {
    let class = class();
    methods!(class, {
        "domain" => domain, c"@16@0:8";
        "code" => code, c"q16@0:8";
        "localizedDescription" => localized_description, c"@16@0:8";
        "description" => localized_description, c"@16@0:8";
        "userInfo" => user_info, c"@16@0:8";
        "localizedFailureReason" => localized_failure_reason, c"@16@0:8";
        "localizedRecoverySuggestion" => localized_recovery_suggestion, c"@16@0:8";
        "localizedRecoveryOptions" => localized_recovery_options, c"@16@0:8";
        "dealloc" => dealloc, c"v16@0:8";
        "initWithDomain:code:userInfo:" => init_with_domain, c"@40@0:8@16q24@32";
    });
    crate::define::class_methods!(class, {
        "errorWithDomain:code:userInfo:" => error_with_domain, c"@40@0:8@16q24@32";
    });
}
