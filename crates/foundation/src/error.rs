use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
struct Body {
    isa: *const Class,
    contents: *mut Contents,
}

/// What an `NSError` holds, which is exactly the three things its constructor
/// takes and nothing else.
///
/// `domain` and `user_info` are the *objects* the error was made with, retained,
/// not copies of their text. That is what Apple stores, and it is observable: a
/// caller may compare `error.domain` or `error.userInfo` against the very
/// dictionary it passed in, by identity. A copy of the text would answer
/// `isEqual:` and fail `==`, which is a silent wrong answer rather than a
/// refusal.
///
/// `description` is this runtime's own: it is the message
/// [`NSURL_ERROR_UNKNOWN`](self) and friends were given, and there is no field
/// on `NSError` for it that Darwin reads back.
struct Contents {
    domain: Id,
    code: i64,
    user_info: Id,
    description: String,
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
    // `made` takes ownership of both objects, and these two were made for this
    // error alone, so there is nothing to release afterwards: the error holds the
    // only reference to each and an autoreleased error is its owner.
    objc::arc::autorelease(made(Contents {
        domain: crate::string::from_str(domain),
        code,
        user_info: make_user_info(description),
        description: description.to_string(),
    }))
}

/// An autoreleased `NSError` carrying a `userInfo` the caller built.
///
/// The dictionary is retained, so the error owns a reference for as long as it
/// lives and `-userInfo` hands back the object that was written rather than an
/// equal one.
pub fn make_with_user_info(domain: Id, code: i64, user_info: Id) -> Id {
    // **Both** objects, not just `user_info`: `Contents` holds `domain` as a
    // retained `Id` too (see its doc comment), so a caller that did not take a
    // reference for the domain would leave the error pointing at storage it does
    // not own. This function is the only path into `made` that does not already
    // own both, so both are taken here.
    objc::arc::retain(domain);
    objc::arc::retain(user_info);
    objc::arc::autorelease(made(Contents {
        domain,
        code,
        user_info,
        description: String::new(),
    }))
}

/// The one allocation path, so every error in this runtime has the same shape.
///
/// **At +1, and it retains nothing and autoreleases nothing.** [`Contents`]
/// holds `domain` and `user_info` as *retained objects*, so this takes ownership
/// of a reference to each: every caller above already holds one (having just made
/// it, or having retained the caller's) and hands it over by calling this. That
/// is the whole ownership contract, stated once, instead of each caller
/// remembering to retain on the way in and release on the way out.
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

/// Upstream's eager half of this module is gone, and the reason is a
/// behavioural difference rather than a merge accident.
///
/// Upstream builds a description **at construction**: `from_parts` reads
/// `NSLocalizedDescription` out of the caller's dictionary and, when the
/// dictionary has none, synthesises Apple's text - "The operation couldn't be
/// completed. (<domain> error <code>.)". This module reads the same key
/// **lazily**, in `-localizedDescription`, and refuses **by name** when the
/// dictionary has no such key, because that text is a *localised* string and a
/// hard-coded English sentence under a localised key is a claim this runtime
/// cannot make. The lazy read is also the more faithful of the two: Apple retains
/// the dictionary, so a caller that adds the key afterwards sees its own value.
///
/// So `string_text`, `from_parts`, `error_with_domain` and `init_with_domain` are
/// all unreachable from the installed table and are removed. Measured, with
/// `strings -a` over the Framework and a positive control of three selectors
/// known to be sent (`localizedDescription`, `appearanceNamed:`,
/// `copyWithZone:`, one occurrence each):
///
/// | selector | occurrences | installed here |
/// |---|--:|---|
/// | `errorWithDomain:code:userInfo:` | 1 | yes, as `error_with_domain_code_user_info` |
/// | `initWithDomain:code:userInfo:` | **0** | no, and cannot be called |
/// | `localizedDescription` | 1 | yes |
/// | `NSLocalizedDescription` | 0 | written and read by us both, never by the guest |
///
/// `initWithDomain:code:userInfo:` being absent from the image is why
/// `init_with_domain` was never registered upstream either: there is nothing to
/// receive the message. What is left installed is the one selector the guest
/// sends, and it refuses a nil domain by name rather than making an error with
/// no domain to report.

/// The one-entry `userInfo` a `make()`-built error reports.
///
/// `NSLocalizedDescription` rather than a private key, because that is the key a
/// caller reads directly about as often as it sends `-localizedDescription`, and
/// an error with a message in it should be readable that way.
fn make_user_info(description: &str) -> Id {
    let key = crate::string::from_str("NSLocalizedDescription");
    let value = crate::string::from_str(description);
    let dictionary = corefoundation::collections::new_dictionary(
        &[(
            key as corefoundation::base::CFTypeRef,
            value as corefoundation::base::CFTypeRef,
        )],
        corefoundation::callbacks::Policy::from_key_callbacks(),
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

/// `-domain`: the `NSString` the error was made with, itself.
unsafe extern "C" fn domain(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is an `NSError` this module made.
    unsafe { contents(receiver) }.map_or(std::ptr::null_mut(), |c| c.domain)
}

unsafe extern "C" fn code(receiver: Id, _sel: Sel) -> i64 {
    // SAFETY: the receiver is an `NSError` this module made.
    unsafe { contents(receiver) }.map_or(0, |c| c.code)
}

/// `-localizedDescription`.
///
/// Read out of the `userInfo` under `NSLocalizedDescription`, because that is
/// where both of this runtime's producers put it and where a guest's own does -
/// and it is the only description an `NSError` really has. An error made by
/// [`make`] also carries this runtime's own text, which is the same string.
///
/// Refuses by name in the one case where there is nothing to read: an error a
/// *guest* built whose `userInfo` has no description. Apple's answer there is a
/// localised sentence from a `.strings` bundle this runtime does not have, so
/// anything returned would be a sentence invented here and shown to a user as
/// though macOS had said it. See the note in `test/PLAN.md` §1.
unsafe extern "C" fn localized_description(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is an `NSError` this module made.
    let Some(c) = (unsafe { contents(receiver) }) else {
        return std::ptr::null_mut();
    };
    if let Some(value) = lookup(c, "NSLocalizedDescription") {
        return value;
    }
    match c.description.is_empty() {
        true => objc::fail!(
            "-[NSError localizedDescription]: the userInfo this error was made with has no \
             NSLocalizedDescription, and macOS's text for that case is a localised string this \
             runtime does not have"
        ),
        false => autoreleased(&c.description),
    }
}

/// The value at `key` in an error's `userInfo`, or null.
///
/// Sent rather than read out of the dictionary, because a `userInfo` a guest
/// built is an `NSDictionary` and this runtime's own is a `CFDictionary`, and
/// only asking works for both.
fn lookup(error: &Contents, key: &str) -> Option<Id> {
    if error.user_info.is_null() {
        return None;
    }
    // SAFETY: `objectForKey:` is `id (id, SEL, id)`, and the userInfo is the live
    // dictionary the error was made with.
    let value = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(error.user_info, objc::sel::get("objectForKey:"), crate::string::from_str(key))
    };
    (!value.is_null()).then_some(value)
}

/// `-userInfo`: the dictionary the error was made with.
///
/// Which is the object that was written, not an equal one. The one exception is
/// an error this runtime built through [`make`], which has no caller-supplied
/// dictionary to keep and reports the one it made.
unsafe extern "C" fn user_info(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is an `NSError` this module made.
    let Some(c) = (unsafe { contents(receiver) }) else {
        return std::ptr::null_mut();
    };
    if !c.user_info.is_null() {
        return c.user_info;
    }
    let dictionary = make_user_info(&c.description);
    // SAFETY: a dictionary this call just made.
    unsafe { corefoundation::api::CFAutorelease(dictionary as corefoundation::base::CFTypeRef) as Id }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: `-dealloc` is reached with the last reference gone, and the
    // receiver is a `Body` whose `contents` came from `Box::into_raw`.
    unsafe {
        let body = receiver as *mut Body;
        if !(*body).contents.is_null() {
            let contents = Box::from_raw((*body).contents);
            objc::arc::release(contents.domain);
            objc::arc::release(contents.user_info);
            (*body).contents = std::ptr::null_mut();
        }
    }
    objc::arc::finalize(receiver);
}

fn autoreleased(text: &str) -> Id {
    let string = crate::string::from_str(text);
    // SAFETY: a string this call just created.
    unsafe { corefoundation::api::CFAutorelease(string as corefoundation::base::CFTypeRef) as Id }
}

/// `+[NSError errorWithDomain:code:userInfo:]`.
///
/// The encoding is the signature, read off the guest rather than assumed: at
/// `0x56a2653` the domain goes in `rdx` as a pointer, the code in `ecx` as a
/// 32-bit immediate, and the `userInfo` in `r8` - which is `xor r8d, r8d` at
/// four of this guest's five sites, i.e. a null dictionary, and a live object at
/// the fifth. `@16` is the domain, `q24` the code (`NSInteger`, eight bytes on
/// LP64 - the immediates are 1, 3 and 4, all of which fit in the 32-bit form the
/// compiler chose) and `@32` the `userInfo`, for five words in all.
///
/// Autoreleased, as a `+` method's result is, and it retains the two objects it
/// was handed so that handing back nil for `-domain` or `-userInfo` cannot happen
/// to an object the caller still owns.
unsafe extern "C" fn error_with_domain_code_user_info(
    _class: Id,
    _sel: Sel,
    domain: Id,
    code: i64,
    user_info: Id,
) -> Id {
    if domain.is_null() {
        objc::fail!(
            "+[NSError errorWithDomain:code:userInfo:]: the domain is nil, and an NSError with \
             no domain cannot be reported as one"
        );
    }
    make_with_user_info(domain, code, user_info)
}

pub(crate) fn install() {
    let class = class();
    methods!(class, {
        "domain" => domain, c"@16@0:8";
        "code" => code, c"q16@0:8";
        "localizedDescription" => localized_description, c"@16@0:8";
        "description" => localized_description, c"@16@0:8";
        "userInfo" => user_info, c"@16@0:8";
        "dealloc" => dealloc, c"v16@0:8";
    });
    class_methods!(class, {
        "errorWithDomain:code:userInfo:" => error_with_domain_code_user_info, c"@40@0:8@16q24@32";
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::define::methods;

    /// Send a message the way a guest would, through the runtime's own dispatch.
    ///
    /// # Safety
    ///
    /// `R` must be the method's real return type.
    unsafe fn send<R>(receiver: Id, name: &str) -> R {
        // SAFETY: the caller's obligation.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> R =
                std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get(name))
        }
    }

    fn class_object() -> Id {
        crate::bootstrap();
        objc::class::ptr(objc::class::lookup(c"NSError").expect("NSError")) as Id
    }

    /// The write-then-read round trip through the real dispatch, for the
    /// three-argument constructor. A constructor that answered a non-nil object
    /// but dropped one of the three on the floor would pass a null-check test, so
    /// each field is read back and compared.
    #[test]
    fn a_guest_built_error_reads_back_the_domain_code_and_user_info_it_was_given() {
        crate::bootstrap();
        let domain = crate::string::from_str("com.example.test");
        let user_info = crate::string::from_str("a reason");
        // SAFETY: `+errorWithDomain:code:userInfo:` is
        // `id (Class, SEL, id, NSInteger, id)`.
        unsafe {
            let construct: unsafe extern "C" fn(Id, Sel, Id, i64, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            let error = construct(
                class_object(),
                objc::sel::get("errorWithDomain:code:userInfo:"),
                domain,
                -12345,
                user_info,
            );
            assert!(!error.is_null(), "the constructor answered nil");

            // The objects come back as themselves, not as equal copies: Darwin
            // stores what it was given, and a caller is entitled to `==` it.
            assert_eq!(send::<Id>(error, "domain"), domain);
            assert_eq!(send::<Id>(error, "userInfo"), user_info);
            assert_eq!(send::<i64>(error, "code"), -12345);
        }
    }

    /// `-localizedDescription` is the description the `userInfo` carries, read
    /// back out of it - which is the only description an `NSError` has.
    #[test]
    fn the_localized_description_is_the_one_the_user_info_carries() {
        crate::bootstrap();
        let domain = crate::string::from_str("com.example.test");
        // SAFETY: an `NSDictionary` holding one documented key.
        let user_info = unsafe {
            let key = crate::string::from_str("NSLocalizedDescription");
            let value = crate::string::from_str("it went wrong");
            let dictionary = corefoundation::collections::new_dictionary(
                &[(
                    key as corefoundation::base::CFTypeRef,
                    value as corefoundation::base::CFTypeRef,
                )],
                corefoundation::callbacks::Policy::from_key_callbacks(),
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            );
            corefoundation::api::CFRelease(key as corefoundation::base::CFTypeRef);
            corefoundation::api::CFRelease(value as corefoundation::base::CFTypeRef);
            corefoundation::api::CFAutorelease(dictionary) as Id
        };
        // SAFETY: `+errorWithDomain:code:userInfo:` is
        // `id (Class, SEL, id, NSInteger, id)`.
        let error = unsafe {
            let construct: unsafe extern "C" fn(Id, Sel, Id, i64, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            construct(
                class_object(),
                objc::sel::get("errorWithDomain:code:userInfo:"),
                domain,
                7,
                user_info,
            )
        };
        // SAFETY: the error is live and its description is a string.
        let described = unsafe { send::<Id>(error, "localizedDescription") };
        assert_eq!(unsafe { crate::string::bytes(described) }, b"it went wrong");
    }

    /// An error this runtime builds still answers the same way, and its
    /// `userInfo` still carries the description under the documented key.
    #[test]
    fn an_error_this_runtime_builds_still_reads_back() {
        crate::bootstrap();
        let error = make("com.example.runtime", 42, "the transport gave up");
        // SAFETY: the error is live and was made by `make`.
        unsafe {
            assert_eq!(
                crate::string::bytes(send::<Id>(error, "domain")),
                b"com.example.runtime"
            );
            assert_eq!(send::<i64>(error, "code"), 42);
            assert_eq!(
                crate::string::bytes(send::<Id>(error, "localizedDescription")),
                b"the transport gave up"
            );
            let info = send::<Id>(error, "userInfo");
            assert!(!info.is_null());
            // SAFETY: `userInfo` is a dictionary this runtime made.
            let described = unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Id) -> Id =
                    std::mem::transmute(objc::msg::entry());
                f(
                    info,
                    objc::sel::get("objectForKey:"),
                    crate::string::from_str("NSLocalizedDescription"),
                )
            };
            assert_eq!(crate::string::bytes(described), b"the transport gave up");
        }
    }

    /// The error keeps the objects alive, and lets go of them on `-dealloc`.
    ///
    /// The retain count is read rather than the behaviour inferred: a
    /// constructor that stored the pointers without retaining would pass every
    /// round-trip test above and then hand the guest a freed object.
    #[test]
    fn the_error_owns_a_reference_to_what_it_was_given() {
        crate::bootstrap();
        let domain = crate::string::from_str("com.example.test");
        let user_info = crate::string::from_str("a reason");
        let before = objc::arc::retain_count(domain);
        // SAFETY: `+errorWithDomain:code:userInfo:` is
        // `id (Class, SEL, id, NSInteger, id)`.
        let error = unsafe {
            let construct: unsafe extern "C" fn(Id, Sel, Id, i64, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            construct(
                class_object(),
                objc::sel::get("errorWithDomain:code:userInfo:"),
                domain,
                1,
                user_info,
            )
        };
        assert_eq!(
            objc::arc::retain_count(domain),
            before + 1,
            "the error holds its own reference to the domain"
        );
        // SAFETY: the error was autoreleased, so releasing the pool's reference
        // reaches `-dealloc` with the last one gone.
        unsafe { objc::arc::release(error) };
        assert_eq!(
            objc::arc::retain_count(domain),
            before,
            "the reference went back on dealloc"
        );
    }

    /// A code outside the range an `NSInteger` can hold would be truncated, and
    /// truncation here would be silent: the guest stores these in an `int32_t`
    /// and prints them.
    #[test]
    fn a_code_at_the_edge_of_the_integer_range_survives() {
        crate::bootstrap();
        // SAFETY: `+errorWithDomain:code:userInfo:` is
        // `id (Class, SEL, id, NSInteger, id)`.
        let error = unsafe {
            let construct: unsafe extern "C" fn(Id, Sel, Id, i64, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            construct(
                class_object(),
                objc::sel::get("errorWithDomain:code:userInfo:"),
                crate::string::from_str("com.example.test"),
                i64::MIN,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: the error is live.
        assert_eq!(unsafe { send::<i64>(error, "code") }, i64::MIN);
    }

    /// The methods this module installs are on the class a guest looks them up
    /// on: the instance ones on `NSError` and the constructor on its metaclass.
    #[test]
    fn the_constructor_is_a_class_method_on_nserror() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSError").expect("NSError");
        let sel = objc::sel::get("errorWithDomain:code:userInfo:");
        assert!(class.method(sel).is_none(), "not an instance method");
        let imp = class
            .metaclass()
            .method(sel)
            .expect("a + method on NSError")
            .implementation();
        assert!(!imp.is_null());
        // The encoding is the interface: five words, and the code a `q`.
        let method = class.metaclass().method(sel).expect("the method");
        // SAFETY: a registered method's types are a NUL-terminated string.
        let types = unsafe { std::ffi::CStr::from_ptr(method.types()) };
        assert_eq!(types, c"@40@0:8@16q24@32");
    }

    /// Every other `-` this crate relies on is still on the class, so a change
    /// to this module that dropped one shows up here rather than in a guest.
    #[test]
    fn the_error_accessors_are_still_installed() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSError").expect("NSError");
        for name in [
            "domain",
            "code",
            "userInfo",
            "localizedDescription",
            "dealloc",
        ] {
            assert!(
                class.method(objc::sel::get(name)).is_some(),
                "-{name} is not installed"
            );
        }
    }

    // `methods` is used by the module itself; re-imported here so the test module
    // can name the macro if it needs to.
    #[allow(unused_imports)]
    use methods as _methods;
}
