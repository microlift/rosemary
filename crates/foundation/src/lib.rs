#![allow(
    clippy::chunks_exact_to_as_chunks,
    clippy::collapsible_if,
    clippy::let_and_return,
    clippy::ptr_eq,
    clippy::unnecessary_cast
)]

pub mod appleevent;
pub mod attributed_string;
mod block;
pub mod boxed;
pub mod bundle;
pub mod calculate;
mod cf_attributed_string;
mod characterset;
mod constant;
pub mod cookies;
pub mod decimal;
pub mod defaults;
mod define;
mod distributed;
mod enumeration;
pub mod error;
pub mod exception;
pub mod filehandle;
pub mod filemanager;
pub mod formatter;
pub mod geometry;
mod indexset;
pub mod json;
pub mod kvc;
pub mod kvo;
pub mod locale;
pub mod notification;
pub mod objectify;
pub mod operation;
pub mod paths;
mod transformer;
pub use corefoundation::plist;
pub mod pool;
pub mod processinfo;
pub mod proxy;
pub mod runtime;
mod signature;
pub mod string;
pub mod strings;
pub mod task;
pub mod thread;
pub mod timer;
mod timezone;
pub mod unarchiver;
pub mod url;
mod urlcomponents;
pub mod urlsession;
pub mod value;

mod collections;

use std::sync::Once;

/// Install this crate's methods and classes. Idempotent.
pub fn bootstrap() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        corefoundation::bootstrap();
        string::install();
        characterset::install();
        indexset::install();
        attributed_string::install();
        collections::install();
        enumeration::install();
        transformer::install();
        decimal::install();
        pool::install();
        geometry::install();
        value::install();
        kvc::install();
        kvo::install();
        runtime::install();
        signature::install();
        notification::install();
        distributed::install();
        constant::install();
        declare_abstract();
        bundle::class::install();
        filemanager::install();
        exception::install();
        locale::install();
        formatter::install();
        timer::install();
        timezone::install();
        thread::install();
        operation::install();
        url::install();
        urlcomponents::install();
        json::install();
        urlsession::install();
        cookies::install();
        defaults::install();
        processinfo::install();
        filehandle::install();
        task::install();
        appleevent::install();
    });
}

exports::group! {
    paths,
    cf_attributed_string,
    /// Bundle and search-path calls are CoreFoundation and Foundation spellings
    /// of questions only this crate can answer - `corefoundation` is underneath
    /// it and has neither an `NSBundle` nor a `$HOME`.
    bundle::cf,
    notification::cf,
    error::cf,
    runtime,
    calculate,
    locale::cf,
    formatter::cf,
    timer::cf,
    appleevent,
    proxy,
}

/// This user's home directory, as the guest spells it.
///
/// `/Users/<name>` under a prefix, and this host's `$HOME` without one. This is
/// the answer `NSHomeDirectory`, `~` expansion and `NSSearchPath...` give, and
/// it is a *guest* path: anything that means to open a file under it wants
/// [`host_home_directory`] instead. See `docs/prefixes.md`.
pub fn home_directory() -> String {
    prefix::ns::home().to_string()
}

/// The same directory as a host path, for opening files under it.
pub fn host_home_directory() -> std::path::PathBuf {
    prefix::ns::host_path(&home_directory())
}

/// An integer as an autoreleased `NSNumber`.
pub(crate) fn number(value: i64) -> objc::object::Id {
    let n = corefoundation::collections::new_number(corefoundation::collections::Number::Integer(
        value,
    ));
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(n) as objc::object::Id }
}

/// A double as an autoreleased `NSNumber`.
pub(crate) fn real(value: f64) -> objc::object::Id {
    let n =
        corefoundation::collections::new_number(corefoundation::collections::Number::Float(value));
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(n) as objc::object::Id }
}

/// An autoreleased, attribute-free `NSAttributedString`.
pub fn attributed_string(text: &str) -> objc::object::Id {
    attributed_string::make(text)
}

/// The value an `NSNumber` holds, asked for rather than reached into.
///
/// A number is not always a `__NSCFNumber`: `@(9.25)` with a constant operand is
/// an `NSConstantDoubleNumber` the compiler laid out in the image, and a guest
/// may subclass `NSNumber` itself. So the accessor is *sent*, which every one of
/// them answers, instead of the storage being read - which only one of them has.
///
/// # Safety
///
/// `value` must be null or an object this runtime allocated.
pub(crate) unsafe fn number_of(value: objc::object::Id) -> Option<i64> {
    // SAFETY: the caller's obligation; `-longLongValue` is `long long (id, SEL)`.
    unsafe { send_scalar::<i64>(value, "longLongValue") }
}

/// # Safety
///
/// As [`number_of`].
pub(crate) unsafe fn real_of(value: objc::object::Id) -> Option<f64> {
    // SAFETY: the caller's obligation; `-doubleValue` is `double (id, SEL)`.
    unsafe { send_scalar::<f64>(value, "doubleValue") }
}

/// Send a nullary message returning `T`, or `None` if the receiver has no such
/// method.
///
/// # Safety
///
/// `receiver` must be null or a live object, and the method - if there is one -
/// must return `T`.
unsafe fn send_scalar<T>(receiver: objc::object::Id, name: &str) -> Option<T> {
    let sel = objc::sel::get(name);
    let method = unsafe { objc::object::class_of(receiver) }?.method(sel)?;
    let f: unsafe extern "C" fn(objc::object::Id, objc::sel::Sel) -> T =
        unsafe { std::mem::transmute(method.implementation()) };
    Some(unsafe { f(receiver, sel) })
}

/// The class objects this crate publishes as *data*, Mach-O spelling included.
///
/// A guest's `[NSMutableArray array]` binds `_OBJC_CLASS_$_NSMutableArray` and
/// sends to whatever it gets, so the address has to be the class itself.
pub fn class_data() -> Vec<(String, u64)> {
    bootstrap();
    let mut out = Vec::new();
    for name in CLASSES
        .iter()
        .chain(constant::CLASSES)
        .chain(urlsession::CLASSES)
        .copied()
    {
        let Some(class) = std::ffi::CString::new(name)
            .ok()
            .and_then(|n| objc::class::lookup(&n))
        else {
            continue;
        };
        out.push((
            format!("_OBJC_CLASS_$_{name}"),
            objc::class::ptr(class) as usize as u64,
        ));
        out.push((
            format!("_OBJC_METACLASS_$_{name}"),
            objc::class::ptr(class.metaclass()) as usize as u64,
        ));
    }
    out
}

/// Classes declared so a guest can subclass them, with no methods of their own.
///
/// Declaring one is strictly better than leaving it out. A missing superclass
/// refuses the guest's *whole subclass* - every method on it, and every message
/// to it - whereas a declared one registers, and only the inherited messages the
/// guest actually sends are refused, each by name. That difference is the whole
/// diagnostic.
///
/// Only classes a guest in this tree has been seen to subclass. AppKit's -
/// `NSView`, `NSWindow`, `NSApplication` - belong to AppKit and arrive with it.
fn declare_abstract() {
    for name in [c"NSFormatter", c"NSThread", c"NSLock", c"NSRecursiveLock"] {
        corefoundation::base::abstract_class(name, None);
    }
}

/// Every class this crate answers for.
pub const CLASSES: &[&str] = &[
    "NSString",
    "NSAttributedString",
    "NSMutableAttributedString",
    "NSCharacterSet",
    "NSIndexSet",
    "NSMutableIndexSet",
    "NSURLComponents",
    "NSHTTPCookie",
    "NSHTTPCookieStorage",
    "NSUserDefaults",
    "NSMutableString",
    "NSArray",
    "NSMutableArray",
    "NSSet",
    "NSMutableSet",
    "NSDictionary",
    "NSMutableDictionary",
    "NSData",
    "NSMutableData",
    "NSNumber",
    "NSDecimalNumber",
    "NSDecimalNumberHandler",
    "NSPredicate",
    "NSProxy",
    "NSValueTransformer",
    "NSURLProtocol",
    "NSScriptCommand",
    "NSEnumerator",
    "NSValue",
    "NSNull",
    "NSNotification",
    "NSNotificationCenter",
    "NSDistributedNotificationCenter",
    "NSAutoreleasePool",
    "NSBundle",
    "NSDate",
    "NSFileManager",
    "NSLocale",
    "NSException",
    "NSRunLoop",
    "NSTimer",
    "NSTimeZone",
    "NSFormatter",
    "NSThread",
    "NSLock",
    "NSRecursiveLock",
    "NSOperation",
    "NSBlockOperation",
    "NSOperationQueue",
    "NSURL",
    "NSJSONSerialization",
    "NSProcessInfo",
    "NSDateFormatter",
    "NSNumberFormatter",
    "NSFileHandle",
    "NSPipe",
    "NSTask",
    "NSAppleEventManager",
    "NSAppleEventDescriptor",
    "NSMethodSignature",
];

// The image-resident number classes are `constant::CLASSES`, published the same
// way and listed there because their layouts are that module's business.

/// Everything this crate publishes as data: the class objects, and the string
/// globals a program reads through their own symbol.
///
/// A key global is a **variable** holding an `NSString *`, not the string, so
/// each goes through a cell - see `corefoundation::base::global_cell`.
pub fn data_symbols() -> Vec<(String, u64)> {
    let mut out = class_data();
    for table in [
        locale::KEYS,
        formatter::cf::KEYS,
        kvo::KEYS,
        decimal::KEYS,
        proxy::KEYS,
        STRING_GLOBALS,
    ] {
        out.extend(corefoundation::base::string_globals(table));
    }
    out.extend(corefoundation::base::named_string_globals(
        NOTIFICATION_NAMES,
    ));
    // `NSZeroPoint`, `NSZeroSize` and `NSZeroRect` are **structs** in Foundation's `__DATA`,
    // not pointers to them: the guest loads the symbol's address and reads two
    // or four doubles out of it. A one-word cell would leave the rest of the
    // read off the end of the allocation.
    // `const int64_t`, read as a number.
    out.push((
        "_NSURLSessionTransferSizeUnknown".to_string(),
        corefoundation::base::global_cell("_NSURLSessionTransferSizeUnknown", (-1i64) as u64),
    ));
    out.push(("_NSZeroPoint".to_string(), zeroed::<geometry::NSPoint>()));
    out.push(("_NSZeroSize".to_string(), zeroed::<geometry::NSSize>()));
    out.push(("_NSZeroRect".to_string(), zeroed::<geometry::NSRect>()));
    // The CoreGraphics spellings of the same three, which are separate symbols
    // in a separate framework's `__DATA` even though the types are identical.
    out.push(("_CGPointZero".to_string(), zeroed::<geometry::NSPoint>()));
    out.push(("_CGSizeZero".to_string(), zeroed::<geometry::NSSize>()));
    out.push(("_CGRectZero".to_string(), zeroed::<geometry::NSRect>()));
    out
}

/// The address of a zeroed `T` that lives for the process.
///
/// Not interned: each of these names a distinct global, and a guest that writes
/// through one - which nothing should, but the memory is writable either way -
/// must not disturb the other.
fn zeroed<T>() -> u64 {
    Box::leak(vec![0u8; size_of::<T>()].into_boxed_slice()).as_ptr() as usize as u64
}

pub const STRING_GLOBALS: &[(&str, &str)] = &[
    ("_NSLocalizedDescriptionKey", "NSLocalizedDescription"),
    (
        "_NSLocalizedFailureReasonErrorKey",
        "NSLocalizedFailureReason",
    ),
    (
        "_NSLocalizedRecoverySuggestionErrorKey",
        "NSLocalizedRecoverySuggestion",
    ),
    (
        "_NSLocalizedRecoveryOptionsErrorKey",
        "NSLocalizedRecoveryOptions",
    ),
    ("_NSLocalizedFailureErrorKey", "NSLocalizedFailure"),
    ("_NSHelpAnchorErrorKey", "NSHelpAnchor"),
    ("_NSStringEncodingErrorKey", "NSStringEncodingErrorKey"),
    ("_NSOSStatusErrorDomain", "NSOSStatusErrorDomain"),
    ("_NSMachErrorDomain", "NSMachErrorDomain"),
    (
        "_NSFileHandleNotificationDataItem",
        "NSFileHandleNotificationDataItem",
    ),
    (
        "_NSFileHandleNotificationFileHandleItem",
        "NSFileHandleNotificationFileHandleItem",
    ),
    ("_NSCalendarIdentifierGregorian", "gregorian"),
    ("_NSCalendarIdentifierISO8601", "iso8601"),
    ("_NSURLIsWritableKey", "NSURLIsWritableKey"),
    ("_NSURLIsReadableKey", "NSURLIsReadableKey"),
    ("_NSURLIsExecutableKey", "NSURLIsExecutableKey"),
    ("_NSURLTypeIdentifierKey", "NSURLTypeIdentifierKey"),
    ("_NSArgumentDomain", "NSArgumentDomain"),
    ("_NSGlobalDomain", "NSGlobalDomain"),
    ("_NSFilePosixPermissions", "NSFilePosixPermissions"),
    ("_NSFileSystemFreeSize", "NSFileSystemFreeSize"),
    ("_NSFileSystemSize", "NSFileSystemSize"),
    ("_NSFileSystemNumber", "NSFileSystemNumber"),
    ("_NSFileHFSCreatorCode", "NSFileHFSCreatorCode"),
    ("_NSFileHFSTypeCode", "NSFileHFSTypeCode"),
    ("_NSFileTypeUnknown", "NSFileTypeUnknown"),
    ("_NSFileTypeDirectory", "NSFileTypeDirectory"),
    ("_NSFileTypeRegular", "NSFileTypeRegular"),
    ("_NSFileTypeSymbolicLink", "NSFileTypeSymbolicLink"),
    ("_NSPOSIXErrorDomain", "NSPOSIXErrorDomain"),
    ("_NSCocoaErrorDomain", "NSCocoaErrorDomain"),
    ("_NSFilePathErrorKey", "NSFilePath"),
    ("_NSURLErrorKey", "NSURL"),
    ("_NSUnderlyingErrorKey", "NSUnderlyingError"),
    ("_NSURLErrorDomain", "NSURLErrorDomain"),
    ("_NSHTTPCookieDiscard", "Discard"),
    ("_NSHTTPCookieDomain", "Domain"),
    ("_NSHTTPCookieExpires", "Expires"),
    ("_NSHTTPCookieName", "Name"),
    ("_NSHTTPCookieOriginURL", "OriginURL"),
    ("_NSHTTPCookiePath", "Path"),
    ("_NSHTTPCookieSecure", "Secure"),
    ("_NSHTTPCookieValue", "Value"),
    ("_NSHTTPCookieVersion", "Version"),
    (
        "_NSURLVolumeAvailableCapacityForImportantUsageKey",
        "NSURLVolumeAvailableCapacityForImportantUsageKey",
    ),
    // Apple moved the exception names to CoreFoundation; a guest built
    // against a newer SDK binds them there.
    ("_NSGenericException", "NSGenericException"),
    ("_NSInvalidArgumentException", "NSInvalidArgumentException"),
    ("_NSNegateBooleanTransformerName", "NSNegateBoolean"),
    ("_NSIsNilTransformerName", "NSIsNil"),
    ("_NSIsNotNilTransformerName", "NSIsNotNil"),
    ("_NSRecoveryAttempterErrorKey", "NSRecoveryAttempter"),
    (
        "_NSProgressEstimatedTimeRemainingKey",
        "NSProgressEstimatedTimeRemainingKey",
    ),
    (
        "_NSProgressFileOperationKindDownloading",
        "NSProgressFileOperationKindDownloading",
    ),
    (
        "_NSProgressFileOperationKindKey",
        "NSProgressFileOperationKindKey",
    ),
    ("_NSProgressKindFile", "NSProgressKindFile"),
];

pub const NOTIFICATION_NAMES: &[&str] = &[
    "NSProcessInfoThermalStateDidChangeNotification",
    "NSProcessInfoPowerStateDidChangeNotification",
    "NSCurrentLocaleDidChangeNotification",
    "NSUserDefaultsDidChangeNotification",
];

#[cfg(test)]
mod tests {
    use super::*;
    use objc::object::Id;
    use objc::sel::Sel;

    /// Send a message whose prototype the caller states.
    ///
    /// # Safety
    ///
    /// `R` and the arguments must be the method's actual signature.
    unsafe fn send<R, F>(receiver: Id, name: &str, call: F) -> R
    where
        F: FnOnce(Id, Sel) -> R,
    {
        call(receiver, objc::sel::get(name))
    }

    fn class(name: &std::ffi::CStr) -> Id {
        bootstrap();
        objc::class::ptr(objc::class::lookup(name).expect("registered")) as Id
    }

    /// Qt's Cocoa file picker loads these as Objective-C pointer globals.
    /// Missing cells leave Mach-O GOT slots zero and crash the picker when it
    /// categorizes a directory, regular file, or symbolic link.
    #[test]
    fn file_type_globals_are_present_with_the_expected_string_values() {
        bootstrap();
        let symbols = data_symbols();
        for value in [
            "NSFileTypeDirectory",
            "NSFileTypeRegular",
            "NSFileTypeSymbolicLink",
        ] {
            let symbol = format!("_{value}");
            let (_, address) = symbols
                .iter()
                .find(|(name, _)| name == &symbol)
                .unwrap_or_else(|| panic!("missing Foundation data symbol {symbol}"));
            // SAFETY: `string_globals` registers each value as a pointer-sized
            // cell and the test just looked up its actual returned address.
            let object = unsafe { *(*address as *const Id) };
            assert!(!object.is_null(), "{symbol} must hold an NSString");
            assert_eq!(unsafe { string::bytes(object) }, value.as_bytes());
        }
    }

    #[test]
    fn every_class_this_crate_claims_is_registered() {
        bootstrap();
        let missing: Vec<&str> = CLASSES
            .iter()
            .chain(constant::CLASSES)
            .copied()
            .filter(|n| {
                std::ffi::CString::new(*n)
                    .ok()
                    .and_then(|c| objc::class::lookup(&c))
                    .is_none()
            })
            .collect();
        assert!(
            missing.is_empty(),
            "claimed but not registered: {missing:?}"
        );
    }

    /// The whole of toll-free bridging, in one assertion each way: a string
    /// built through CoreFoundation answers `-length`, and one built through
    /// Foundation answers `CFStringGetLength`.
    #[test]
    fn a_string_answers_to_both_vocabularies() {
        bootstrap();
        // SAFETY: `+stringWithUTF8String:` is `id (*)(Class, SEL, const char *)`
        // and `-length` is `NSUInteger (*)(id, SEL)`.
        unsafe {
            let ns: Id = {
                let f: unsafe extern "C" fn(Id, Sel, *const std::ffi::c_char) -> Id =
                    std::mem::transmute(objc::msg::entry());
                f(
                    class(c"NSString"),
                    objc::sel::get("stringWithUTF8String:"),
                    c"bridged".as_ptr(),
                )
            };
            assert!(!ns.is_null());
            assert_eq!(
                corefoundation::api::CFStringGetLength(ns as corefoundation::base::CFTypeRef),
                7,
                "a Foundation-built string is not a CFString"
            );

            let cf = corefoundation::api::CFStringCreateWithCString(
                std::ptr::null(),
                c"bridged".as_ptr(),
                corefoundation::string::encoding::UTF8,
            );
            let length: usize = send(cf as Id, "length", |r, s| {
                let f: unsafe extern "C" fn(Id, Sel) -> usize =
                    std::mem::transmute(objc::msg::entry());
                f(r, s)
            });
            assert_eq!(length, 7, "a CFString does not answer -length");
            corefoundation::api::CFRelease(cf);
        }
    }

    /// A mutable array built the way a guest builds one, and read back the same
    /// way - through the runtime's real dispatch rather than by calling the
    /// implementations here.
    #[test]
    fn a_mutable_array_holds_what_is_added_to_it() {
        bootstrap();
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            let send0: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(objc::msg::entry());
            let send1: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            let count: unsafe extern "C" fn(Id, Sel) -> usize =
                std::mem::transmute(objc::msg::entry());
            let at: unsafe extern "C" fn(Id, Sel, usize) -> Id =
                std::mem::transmute(objc::msg::entry());

            let array = send0(class(c"NSMutableArray"), objc::sel::get("array"));
            assert!(!array.is_null());
            let element = send0(class(c"NSNumber"), objc::sel::get("class"));
            send1(array, objc::sel::get("addObject:"), element);
            assert_eq!(count(array, objc::sel::get("count")), 1);
            assert_eq!(at(array, objc::sel::get("objectAtIndex:"), 0), element);
        }
    }

    /// `-indexOfObject:` finds the first equal element, and says `NSNotFound`
    /// for one that is not there.
    #[test]
    fn index_of_object_finds_the_first_match() {
        bootstrap();
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            let send0: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(objc::msg::entry());
            let send1: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            let index_of: unsafe extern "C" fn(Id, Sel, Id) -> usize =
                std::mem::transmute(objc::msg::entry());

            let array = send0(class(c"NSMutableArray"), objc::sel::get("array"));
            let (a, b, c) = (class(c"NSNumber"), class(c"NSArray"), class(c"NSSet"));
            for element in [a, b, a] {
                send1(array, objc::sel::get("addObject:"), element);
            }
            let sel = objc::sel::get("indexOfObject:");
            assert_eq!(index_of(array, sel, a), 0);
            assert_eq!(index_of(array, sel, b), 1);
            assert_eq!(index_of(array, sel, c), isize::MAX as usize);
        }
    }

    /// An enumerator hands out each element once, in order - or back to front
    /// for the reverse one - and then nil; `-allObjects` takes what is left.
    #[test]
    fn an_array_enumerator_walks_the_array_once() {
        bootstrap();
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            let send0: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(objc::msg::entry());
            let send1: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            let count: unsafe extern "C" fn(Id, Sel) -> usize =
                std::mem::transmute(objc::msg::entry());

            let array = send0(class(c"NSMutableArray"), objc::sel::get("array"));
            let elements = [class(c"NSNumber"), class(c"NSArray"), class(c"NSSet")];
            for element in elements {
                send1(array, objc::sel::get("addObject:"), element);
            }
            let next = objc::sel::get("nextObject");
            let forward = send0(array, objc::sel::get("objectEnumerator"));
            for element in elements {
                assert_eq!(send0(forward, next), element);
            }
            assert!(send0(forward, next).is_null());

            let reverse = send0(array, objc::sel::get("reverseObjectEnumerator"));
            assert_eq!(send0(reverse, next), elements[2]);
            let rest = send0(reverse, objc::sel::get("allObjects"));
            assert_eq!(count(rest, objc::sel::get("count")), 2);
            assert!(send0(reverse, next).is_null());
        }
    }

    /// Adding an object the set already holds leaves one copy of it.
    #[test]
    fn a_mutable_set_holds_each_added_object_once() {
        bootstrap();
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            let with_capacity: unsafe extern "C" fn(Id, Sel, usize) -> Id =
                std::mem::transmute(objc::msg::entry());
            let send1: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            let contains: unsafe extern "C" fn(Id, Sel, Id) -> bool =
                std::mem::transmute(objc::msg::entry());
            let count: unsafe extern "C" fn(Id, Sel) -> usize =
                std::mem::transmute(objc::msg::entry());

            let set = with_capacity(
                class(c"NSMutableSet"),
                objc::sel::get("setWithCapacity:"),
                0,
            );
            let element = string::from_str("element");
            send1(set, objc::sel::get("addObject:"), element);
            send1(set, objc::sel::get("addObject:"), element);
            assert_eq!(count(set, objc::sel::get("count")), 1);
            assert!(contains(set, objc::sel::get("containsObject:"), element));
        }
    }

    /// `[[NSMutableDictionary alloc] init]`, which is a class cluster: what
    /// `+alloc` answers cannot hold anything, so `-init` has to substitute.
    ///
    /// The write-then-read is the point. A placeholder answers `-objectForKey:`
    /// with nil and drops `-setObject:forKey:` on the floor without failing, so
    /// a test that only checked the object was non-nil would pass on the bug
    /// this covers.
    #[test]
    fn alloc_init_gives_a_collection_that_holds_what_is_put_in_it() {
        bootstrap();
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            let send0: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(objc::msg::entry());
            let send1: unsafe extern "C" fn(Id, Sel, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            let send2: unsafe extern "C" fn(Id, Sel, Id, Id) =
                std::mem::transmute(objc::msg::entry());
            let count: unsafe extern "C" fn(Id, Sel) -> usize =
                std::mem::transmute(objc::msg::entry());

            let dictionary = send0(
                send0(class(c"NSMutableDictionary"), objc::sel::get("alloc")),
                objc::sel::get("init"),
            );
            let key = string::from_str("deeplink");
            let value = string::from_str("example-player://experiences/start");
            send2(dictionary, objc::sel::get("setObject:forKey:"), value, key);
            assert_eq!(count(dictionary, objc::sel::get("count")), 1);
            assert_eq!(
                send1(dictionary, objc::sel::get("objectForKey:"), key),
                value
            );

            let array = send0(
                send0(class(c"NSMutableArray"), objc::sel::get("alloc")),
                objc::sel::get("init"),
            );
            send1(array, objc::sel::get("addObject:"), value);
            assert_eq!(count(array, objc::sel::get("count")), 1);
        }
    }

    /// `+dictionaryWithObject:forKey:` is declared on the immutable class and
    /// inherited by the mutable one, so what comes back depends on which was
    /// asked - and a program that asks `NSMutableDictionary` writes to it.
    #[test]
    fn a_convenience_constructor_answers_the_class_it_was_sent_to() {
        bootstrap();
        // SAFETY: `+dictionaryWithObject:forKey:` is `id (*)(Class, SEL, id, id)`.
        unsafe {
            let with: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            let sel = objc::sel::get("dictionaryWithObject:forKey:");
            let key = string::from_str("k");
            let value = string::from_str("v");
            for (name, mutable) in [(c"NSDictionary", false), (c"NSMutableDictionary", true)] {
                let d = with(class(name), sel, value, key);
                assert_eq!(
                    corefoundation::collections::dictionary(d as corefoundation::base::CFTypeRef)
                        .expect("a dictionary")
                        .mutable,
                    mutable,
                    "{name:?}"
                );
            }
        }
    }

    /// `-description` on a container spells out what it holds, by asking each
    /// element for its own - which is what a program logging one is after.
    #[test]
    fn a_container_describes_its_elements() {
        bootstrap();
        // SAFETY: `-description` is `id (*)(id, SEL)` and `+arrayWithObject:`
        // is `id (*)(Class, SEL, id)`.
        unsafe {
            let send0: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(objc::msg::entry());
            let send1: unsafe extern "C" fn(Id, Sel, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            let array = send1(
                class(c"NSArray"),
                objc::sel::get("arrayWithObject:"),
                string::from_str("example-player://experiences/start"),
            );
            let text = send0(array, objc::sel::get("description"));
            assert_eq!(
                string::bytes(text),
                b"(\n    example-player://experiences/start\n)"
            );
        }
    }

    /// An array built the immutable way is marked immutable, which is what
    /// `-addObject:` refuses on.
    ///
    /// Asserted on the flag rather than by sending the message: the refusal is
    /// an `objc::fail!`, which aborts the process. It has to - an `IMP` is an
    /// `extern "C"` function and a panic cannot unwind out of one - so there is
    /// nothing for a test to catch.
    #[test]
    fn an_array_created_immutable_says_so() {
        bootstrap();
        // SAFETY: `+arrayWithObject:` is `id (*)(Class, SEL, id)`.
        unsafe {
            let with_object: unsafe extern "C" fn(Id, Sel, Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            let fixed = with_object(
                class(c"NSArray"),
                objc::sel::get("arrayWithObject:"),
                class(c"NSNumber"),
            );
            assert!(
                !corefoundation::collections::array(fixed as corefoundation::base::CFTypeRef)
                    .expect("an array")
                    .mutable
            );

            let send0: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(objc::msg::entry());
            let growable = send0(class(c"NSMutableArray"), objc::sel::get("array"));
            assert!(
                corefoundation::collections::array(growable as corefoundation::base::CFTypeRef)
                    .expect("an array")
                    .mutable
            );
        }
    }
}
