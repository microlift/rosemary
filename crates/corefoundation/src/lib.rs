#![allow(
    clippy::cast_slice_from_raw_parts,
    clippy::chunks_exact_to_as_chunks,
    clippy::doc_lazy_continuation,
    clippy::manual_clamp,
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref
)]

pub mod api;
pub mod base;
pub mod calendar;
pub mod callbacks;
pub mod collections;
mod diskarbitration;
pub mod eightbit;
/// Only on Darwin: nowhere else is there another CoreFoundation to meet.
#[cfg(target_os = "macos")]
pub mod foreign;
mod fsevents;
pub mod plist;
pub mod preferences;
pub mod propertylist;
pub mod runloop;
mod send;
mod socket;
pub mod string;
pub mod sysconfig;
pub mod timezone;
pub mod tzif;
pub mod url;
pub mod urlresource;
pub mod uuid;

/// Install the classes this crate owns. Idempotent.
pub fn bootstrap() {
    objc::bootstrap();
    string::class();
    string::constant_class();
    collections::array_class();
    collections::set_class();
    collections::dictionary_class();
    collections::number_class();
    collections::boolean_class();
    collections::data_class();
    url::class();
    uuid::class();
}

exports::exports! {
    functions {
        "CFRetain" => api::CFRetain,
        "CFRelease" => api::CFRelease,
        "CFAutorelease" => api::CFAutorelease,
        "CFGetRetainCount" => api::CFGetRetainCount,
        "CFGetTypeID" => api::CFGetTypeID,
        "CFEqual" => api::CFEqual,
        "CFHash" => api::CFHash,
        "CFAllocatorGetTypeID" => api::CFAllocatorGetTypeID,
        "CFAllocatorGetDefault" => api::CFAllocatorGetDefault,
        "CFAllocatorCreate" => api::CFAllocatorCreate,
        "CFStringGetTypeID" => api::CFStringGetTypeID,
        "CFStringCreateWithCString" => api::CFStringCreateWithCString,
        "CFStringCreateWithFileSystemRepresentation" => api::CFStringCreateWithFileSystemRepresentation,
        "CFStringCreateWithCStringNoCopy" => api::CFStringCreateWithCStringNoCopy,
        "CFStringCreateWithBytes" => api::CFStringCreateWithBytes,
        "CFStringCreateWithCharacters" => api::CFStringCreateWithCharacters,
        "CFStringCreateWithCharactersNoCopy" => api::CFStringCreateWithCharactersNoCopy,
        "CFStringCreateCopy" => api::CFStringCreateCopy,
        "CFStringCreateWithSubstring" => api::CFStringCreateWithSubstring,
        "CFStringGetLength" => api::CFStringGetLength,
        "CFStringGetIntValue" => api::CFStringGetIntValue,
        "CFStringGetDoubleValue" => api::CFStringGetDoubleValue,
        "CFStringGetMaximumSizeForEncoding" => api::CFStringGetMaximumSizeForEncoding,
        "CFStringGetCString" => api::CFStringGetCString,
        "CFStringGetCStringPtr" => api::CFStringGetCStringPtr,
        "CFStringGetBytes" => api::CFStringGetBytes,
        "CFStringCreateExternalRepresentation" => api::CFStringCreateExternalRepresentation,
        "CFStringCompare" => api::CFStringCompare,
        "CFStringHasPrefix" => api::CFStringHasPrefix,
        "CFStringHasSuffix" => api::CFStringHasSuffix,
        "CFNullGetTypeID" => api::CFNullGetTypeID,
        "CFBagGetTypeID" => api::CFBagGetTypeID,
        "CFCopyDescription" => api::CFCopyDescription,
        "CFCopyTypeIDDescription" => api::CFCopyTypeIDDescription,
        "CFStringCompareWithOptions" => api::CFStringCompareWithOptions,
        "CFStringFindWithOptions" => api::CFStringFindWithOptions,
        "CFStringCreateMutable" => api::CFStringCreateMutable,
        "CFStringCreateMutableCopy" => api::CFStringCreateMutableCopy,
        "CFStringUppercase" => api::CFStringUppercase,
        "CFStringLowercase" => api::CFStringLowercase,
        "CFStringGetCharacterAtIndex" => api::CFStringGetCharacterAtIndex,
        "CFStringGetCharacters" => api::CFStringGetCharacters,
        "CFStringGetCharactersPtr" => api::CFStringGetCharactersPtr,
        "CFStringAppendCharacters" => api::CFStringAppendCharacters,
        "CFStringAppend" => api::CFStringAppend,
        "CFStringInsert" => api::CFStringInsert,
        "CFStringDelete" => api::CFStringDelete,
        "CFStringAppendCString" => api::CFStringAppendCString,
        "CFStringReplace" => api::CFStringReplace,
        "CFStringReplaceAll" => api::CFStringReplaceAll,
        "CFStringFindAndReplace" => api::CFStringFindAndReplace,
        "CFStringCreateWithBytesNoCopy" => api::CFStringCreateWithBytesNoCopy,
        "CFStringGetFileSystemRepresentation" => api::CFStringGetFileSystemRepresentation,
        "CFStringGetMaximumSizeOfFileSystemRepresentation"
            => api::CFStringGetMaximumSizeOfFileSystemRepresentation,
        "CFArrayGetTypeID" => api::CFArrayGetTypeID,
        "CFArrayCreate" => api::CFArrayCreate,
        "CFArrayApplyFunction" => api::CFArrayApplyFunction,
        "CFArrayGetCount" => api::CFArrayGetCount,
        "CFArrayGetValueAtIndex" => api::CFArrayGetValueAtIndex,
        "CFArrayCreateMutable" => api::CFArrayCreateMutable,
        "CFArrayAppendValue" => api::CFArrayAppendValue,
        "CFArrayExchangeValuesAtIndices" => api::CFArrayExchangeValuesAtIndices,
        "CFArrayCreateCopy" => api::CFArrayCreateCopy,
        "CFArrayInsertValueAtIndex" => api::CFArrayInsertValueAtIndex,
        "CFArrayRemoveValueAtIndex" => api::CFArrayRemoveValueAtIndex,
        "CFArrayRemoveAllValues" => api::CFArrayRemoveAllValues,
        "CFSetGetTypeID" => api::CFSetGetTypeID,
        "CFSetApplyFunction" => api::CFSetApplyFunction,
        "CFSetGetCount" => api::CFSetGetCount,
        "CFSetGetValues" => api::CFSetGetValues,
        "CFDictionaryGetTypeID" => api::CFDictionaryGetTypeID,
        "CFDictionaryCreate" => api::CFDictionaryCreate,
        "CFDictionaryGetCount" => api::CFDictionaryGetCount,
        "CFDictionaryGetCountOfKey" => api::CFDictionaryGetCountOfKey,
        "CFDictionaryCreateCopy" => api::CFDictionaryCreateCopy,
        "CFDictionaryCreateMutableCopy" => api::CFDictionaryCreateMutableCopy,
        "CFDictionaryApplyFunction" => api::CFDictionaryApplyFunction,
        "CFDictionaryGetValue" => api::CFDictionaryGetValue,
        "CFDictionaryGetValueIfPresent" => api::CFDictionaryGetValueIfPresent,
        "CFDictionaryContainsKey" => api::CFDictionaryContainsKey,
        "CFDictionaryGetKeysAndValues" => api::CFDictionaryGetKeysAndValues,
        "CFDictionaryCreateMutable" => api::CFDictionaryCreateMutable,
        "CFDictionarySetValue" => api::CFDictionarySetValue,
        "CFDictionaryAddValue" => api::CFDictionaryAddValue,
        "CFNumberGetTypeID" => api::CFNumberGetTypeID,
        "CFBooleanGetTypeID" => api::CFBooleanGetTypeID,
        "CFBooleanGetValue" => api::CFBooleanGetValue,
        "CFNumberCreate" => api::CFNumberCreate,
        "CFNumberGetValue" => api::CFNumberGetValue,
        "CFNumberGetType" => api::CFNumberGetType,
        "CFNumberIsFloatType" => api::CFNumberIsFloatType,
        "CFNumberGetByteSize" => api::CFNumberGetByteSize,
        "CFDataGetTypeID" => api::CFDataGetTypeID,
        "CFDataCreate" => api::CFDataCreate,
        "CFDataCreateWithBytesNoCopy" => api::CFDataCreateWithBytesNoCopy,
        "CFDataCreateCopy" => api::CFDataCreateCopy,
        "CFDataGetLength" => api::CFDataGetLength,
        "CFDataGetBytePtr" => api::CFDataGetBytePtr,
        "CFDataGetMutableBytePtr" => api::CFDataGetMutableBytePtr,
        "CFDataSetLength" => api::CFDataSetLength,
        "CFDataIncreaseLength" => api::CFDataIncreaseLength,
        "CFDataAppendBytes" => api::CFDataAppendBytes,
        "CFDataDeleteBytes" => api::CFDataDeleteBytes,
        "CFDataReplaceBytes" => api::CFDataReplaceBytes,
        "CFUUIDGetTypeID" => api::CFUUIDGetTypeID,
        "CFUUIDCreate" => api::CFUUIDCreate,
        "CFUUIDCreateFromUUIDBytes" => api::CFUUIDCreateFromUUIDBytes,
        "CFUUIDGetUUIDBytes" => api::CFUUIDGetUUIDBytes,
        "CFUUIDCreateString" => api::CFUUIDCreateString,
        "CFUUIDGetConstantUUIDWithBytes" => api::CFUUIDGetConstantUUIDWithBytes,
        "CFURLGetTypeID" => api::CFURLGetTypeID,
        "CFTimeZoneCopyDefault" => timezone::copy_default,
        "CFTimeZoneCopySystem" => timezone::copy_system,
        "CFTimeZoneCreateWithName" => timezone::create_with_name,
        "CFTimeZoneGetName" => timezone::get_name,
        "CFURLCreateFromFSRef" => api::CFURLCreateFromFSRef,
        "CFURLGetFSRef" => api::CFURLGetFSRef,
        "CFURLCreateBookmarkData" => api::CFURLCreateBookmarkData,
        "CFURLCreateByResolvingBookmarkData" => api::CFURLCreateByResolvingBookmarkData,
        "CFURLWriteBookmarkDataToFile" => api::CFURLWriteBookmarkDataToFile,
        "CFURLCreateBookmarkDataFromFile" => api::CFURLCreateBookmarkDataFromFile,
        "CFURLCreateWithFileSystemPath" => api::CFURLCreateWithFileSystemPath,
        "CFURLCreateWithFileSystemPathRelativeToBase" => api::CFURLCreateWithFileSystemPathRelativeToBase,
        "CFURLCreateWithString" => api::CFURLCreateWithString,
        "CFURLGetString" => api::CFURLGetString,
        "CFURLCopyAbsoluteURL" => api::CFURLCopyAbsoluteURL,
        "CFURLCopyPath" => api::CFURLCopyPath,
        "CFURLCopyLastPathComponent" => api::CFURLCopyLastPathComponent,
        "CFURLCopyScheme" => api::CFURLCopyScheme,
        "CFURLCopyHostName" => api::CFURLCopyHostName,
        "CFURLGetPortNumber" => api::CFURLGetPortNumber,
        "CFURLHasDirectoryPath" => api::CFURLHasDirectoryPath,
        "CFURLCreateCopyAppendingPathComponent" => api::CFURLCreateCopyAppendingPathComponent,
        "CFURLCreateCopyDeletingLastPathComponent" => api::CFURLCreateCopyDeletingLastPathComponent,
        "CFURLCreateCopyDeletingPathExtension" => api::CFURLCreateCopyDeletingPathExtension,
        "CFURLCreateStringByAddingPercentEscapes" => api::CFURLCreateStringByAddingPercentEscapes,
        "CFURLCreateStringByReplacingPercentEscapes" => api::CFURLCreateStringByReplacingPercentEscapes,
        "CFURLCreateStringByReplacingPercentEscapesUsingEncoding" => api::CFURLCreateStringByReplacingPercentEscapesUsingEncoding,
        "CFURLCopyFileSystemPath" => api::CFURLCopyFileSystemPath,
        "CFURLGetFileSystemRepresentation" => api::CFURLGetFileSystemRepresentation,
        "CFRunLoopGetCurrent" => api::CFRunLoopGetCurrent,
        "CFRunLoopGetMain" => api::CFRunLoopGetMain,
        "CFRunLoopRunInMode" => api::CFRunLoopRunInMode,
        "CFRunLoopRun" => api::CFRunLoopRun,
        "CFRunLoopStop" => api::CFRunLoopStop,
        "CFRunLoopWakeUp" => api::CFRunLoopWakeUp,
        "CFRunLoopSourceCreate" => api::CFRunLoopSourceCreate,
        "CFRunLoopSourceSignal" => api::CFRunLoopSourceSignal,
        "CFRunLoopSourceInvalidate" => api::CFRunLoopSourceInvalidate,
        "CFRunLoopSourceIsValid" => api::CFRunLoopSourceIsValid,
        "CFRunLoopAddSource" => api::CFRunLoopAddSource,
        "CFRunLoopObserverCreate" => api::CFRunLoopObserverCreate,
        "CFRunLoopObserverInvalidate" => api::CFRunLoopObserverInvalidate,
        "CFRunLoopObserverIsValid" => api::CFRunLoopObserverIsValid,
        "CFRunLoopObserverGetOrder" => api::CFRunLoopObserverGetOrder,
        "CFRunLoopAddObserver" => api::CFRunLoopAddObserver,
        "CFRunLoopRemoveObserver" => api::CFRunLoopRemoveObserver,
        "CFRunLoopContainsObserver" => api::CFRunLoopContainsObserver,
        "CFRunLoopAddCommonMode" => api::CFRunLoopAddCommonMode,
        "CFSocketCreateWithNative" => socket::create_with_native,
        "CFSocketCreateRunLoopSource" => socket::create_run_loop_source,
        "CFSocketGetNative" => socket::get_native,
        "CFSocketIsValid" => socket::is_valid,
        "CFSocketGetSocketFlags" => socket::get_flags,
        "CFSocketSetSocketFlags" => socket::set_flags,
        "CFSocketEnableCallBacks" => socket::enable_callbacks,
        "CFSocketDisableCallBacks" => socket::disable_callbacks,
        "CFSocketInvalidate" => socket::invalidate,
        "CFRunLoopIsWaiting" => api::CFRunLoopIsWaiting,
        "CFRunLoopTimerGetInterval" => api::CFRunLoopTimerGetInterval,
        "CFRunLoopTimerSetNextFireDate" => api::CFRunLoopTimerSetNextFireDate,
        "CFDataCreateMutable" => api::CFDataCreateMutable,
        "CFDataGetBytes" => api::CFDataGetBytes,
        "CFStringGetSystemEncoding" => api::CFStringGetSystemEncoding,
        "CFStringConvertWindowsCodepageToEncoding" => api::CFStringConvertWindowsCodepageToEncoding,
        "CFStringConvertIANACharSetNameToEncoding" => api::CFStringConvertIANACharSetNameToEncoding,
        "CFArrayContainsValue" => api::CFArrayContainsValue,
        "CFArrayAppendArray" => api::CFArrayAppendArray,
        "CFRunLoopRemoveSource" => api::CFRunLoopRemoveSource,
        "CFRunLoopContainsSource" => api::CFRunLoopContainsSource,
        "CFAbsoluteTimeGetCurrent" => api::CFAbsoluteTimeGetCurrent,
        "CFRunLoopTimerCreate" => api::CFRunLoopTimerCreate,
        "CFRunLoopTimerGetContext" => api::CFRunLoopTimerGetContext,
        "CFRunLoopAddTimer" => api::CFRunLoopAddTimer,
        "CFRunLoopRemoveTimer" => api::CFRunLoopRemoveTimer,
        "CFRunLoopTimerInvalidate" => api::CFRunLoopTimerInvalidate,
        "CFRunLoopTimerIsValid" => api::CFRunLoopTimerIsValid,
        "CFRunLoopGetNextTimerFireDate" => api::CFRunLoopGetNextTimerFireDate,
    }
    groups {
        calendar,
        preferences,
        propertylist,
        diskarbitration,
        fsevents,
        urlresource,
        sysconfig,
    }
}

/// The symbols that are **data**, as `(Mach-O name, address)`.
///
/// `___CFConstantStringClassReference` is the whole reason a guest's `@"literal"`
/// works: the four words the compiler laid out in `__DATA` become a live object
/// of this runtime the moment that one symbol is bound, with nothing copied.
///
/// Two shapes, and mixing them up is a segfault at address zero. A class
/// reference and a callbacks table are the *thing itself*, and the guest uses
/// the symbol's address as the value. Every `kCF...` pointer global is a
/// **variable**: the guest loads the symbol's address and then loads the object
/// out of it, so those go through [`base::global_cell`].
///
/// `kCFAllocatorDefault` and its relatives are cells holding null. Null *is* the
/// default allocator in Apple's CoreFoundation, and this runtime has only the
/// one - so a guest comparing against it, or passing it along, gets the right
/// answer.
pub fn data_symbols() -> Vec<(String, u64)> {
    bootstrap();
    static EMPTY_ARRAY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    static EMPTY_DICTIONARY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let empty_array = *EMPTY_ARRAY.get_or_init(|| {
        collections::new_array(&[], callbacks::Policy::from_type_callbacks(), false) as usize
    });
    let empty_dictionary = *EMPTY_DICTIONARY.get_or_init(|| {
        collections::new_dictionary(
            &[],
            callbacks::Policy::from_key_callbacks(),
            callbacks::Policy::from_type_callbacks(),
            false,
        ) as usize
    });
    let mut out = vec![
        (
            "___NSArray0__".to_string(),
            base::global_cell("___NSArray0__", empty_array as u64),
        ),
        (
            "___NSDictionary0__".to_string(),
            base::global_cell("___NSDictionary0__", empty_dictionary as u64),
        ),
        (
            "___CFConstantStringClassReference".to_string(),
            objc::class::ptr(string::constant_class()) as usize as u64,
        ),
        (
            "_kCFTypeArrayCallBacks".to_string(),
            &callbacks::TYPE_VALUE_CALLBACKS as *const _ as usize as u64,
        ),
        (
            "_kCFTypeDictionaryValueCallBacks".to_string(),
            &callbacks::TYPE_VALUE_CALLBACKS as *const _ as usize as u64,
        ),
        (
            "_kCFTypeDictionaryKeyCallBacks".to_string(),
            &callbacks::TYPE_KEY_CALLBACKS as *const _ as usize as u64,
        ),
        (
            "_kCFCopyStringDictionaryKeyCallBacks".to_string(),
            &callbacks::COPY_STRING_KEY_CALLBACKS as *const _ as usize as u64,
        ),
    ];
    // A `const CFTimeInterval`, so the symbol is the double itself. Qt's
    // `QDateTime::toCFDate` subtracts it before `CFDateCreate`.
    static INTERVAL_SINCE_1970: f64 = 978_307_200.0;
    out.push((
        "_kCFAbsoluteTimeIntervalSince1970".to_string(),
        &INTERVAL_SINCE_1970 as *const f64 as usize as u64,
    ));
    for name in [
        "_kCFAllocatorDefault",
        "_kCFAllocatorSystemDefault",
        "_kCFAllocatorMalloc",
        "_kCFAllocatorNull",
    ] {
        out.push((name.to_string(), base::global_cell(name, 0)));
    }
    // The run loop's mode names, which a guest passes straight back in and
    // which `CFRunLoopAddSource` compares as strings.
    for (name, mode) in [
        ("_kCFRunLoopDefaultMode", runloop::DEFAULT_MODE),
        ("_kCFRunLoopCommonModes", runloop::COMMON_MODES),
        // Foundation spells the default mode this way, and it is the same mode:
        // a guest that schedules with one and runs with the other has to be
        // talking about the same thing or nothing ever fires.
        ("_NSDefaultRunLoopMode", runloop::DEFAULT_MODE),
        // ...and Foundation's spelling of the common-modes set, which is the
        // same set for the same reason.
        ("_NSRunLoopCommonModes", runloop::COMMON_MODES),
    ] {
        out.push((
            name.to_string(),
            base::global_cell(name, base::interned_name(mode)),
        ));
    }
    // The string-valued globals, each of which is exactly its own name without
    // the `k` and the `Key`. Interned so that a guest comparing one against a
    // key it read back gets identity as well as equality.
    for (symbol, text) in KEY_STRINGS {
        out.push((
            symbol.to_string(),
            base::global_cell(symbol, base::interned_name(text)),
        ));
    }
    for (symbol, value) in [
        ("_kCFBooleanTrue", collections::boolean(true)),
        ("_kCFBooleanFalse", collections::boolean(false)),
    ] {
        out.push((
            symbol.to_string(),
            base::global_cell(symbol, value as usize as u64),
        ));
    }
    out.push((
        "_kCFNull".to_string(),
        base::global_cell("_kCFNull", null_object()),
    ));
    // A `double` variable, not a pointer: the guest loads the symbol's address
    // and reads a floating-point number out of it. 1600 is the Big Sur value,
    // which is the floor every version test in a program of this vintage asks
    // about.
    out.push((
        "_kCFCoreFoundationVersionNumber".to_string(),
        base::global_cell("_kCFCoreFoundationVersionNumber", (1600.0f64).to_bits()),
    ));
    // The calendar identifiers, each of which is its own text - see
    // `calendar::IDENTIFIERS`.
    for (symbol, text) in calendar::IDENTIFIERS {
        out.push((
            (*symbol).to_string(),
            base::global_cell(symbol, base::interned_name(text)),
        ));
    }
    // A `const CFTimeInterval` like the one above it: the seconds from the 1904
    // epoch - the one QuickTime and the old Mac OS counted from - to
    // CoreFoundation's.
    static INTERVAL_SINCE_1904: f64 = 3_061_152_000.0;
    out.push((
        "_kCFAbsoluteTimeIntervalSince1904".to_string(),
        &INTERVAL_SINCE_1904 as *const f64 as usize as u64,
    ));
    out.extend(preferences::data_symbols());
    out.extend(diskarbitration::data_symbols());
    out.extend(urlresource::data_symbols());
    out.extend(sysconfig::data_symbols());
    out
}

static KEY_STRINGS: &[(&str, &str)] = &[
    ("_kCFBundleIdentifierKey", "CFBundleIdentifier"),
    ("_kCFBundleNameKey", "CFBundleName"),
    ("_kCFBundleVersionKey", "CFBundleVersion"),
    ("_kCFBundleExecutableKey", "CFBundleExecutable"),
    ("_kCFBundleLocalizationsKey", "CFBundleLocalizations"),
    (
        "_kCFBundleDevelopmentRegionKey",
        "CFBundleDevelopmentRegion",
    ),
    (
        "_kCFBundleInfoDictionaryVersionKey",
        "CFBundleInfoDictionaryVersion",
    ),
];

/// `kCFNull`, the singleton `NSNull` also answers for.
fn null_object() -> u64 {
    let class = objc::class::lookup(c"NSNull");
    let Some(class) = class else {
        // Foundation registers `NSNull`, and a process using CoreFoundation
        // without it has no null to publish. Zero here is the same as not
        // publishing the symbol, which is what it was before.
        return 0;
    };
    let sel = objc::sel::get("null");
    // SAFETY: `+null` is `id (Class, SEL)` and hands back a singleton kept for
    // the process.
    let null = unsafe {
        let f: unsafe extern "C" fn(objc::object::Id, objc::sel::Sel) -> objc::object::Id =
            std::mem::transmute(objc::msg::entry());
        f(objc::class::ptr(class) as objc::object::Id, sel)
    };
    null as usize as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use base::CFTypeRef;
    // `c_char` is signed on Darwin and unsigned on aarch64 Linux; a buffer
    // spelled `[i8; N]` compiles on one host and not the other.
    use std::ffi::c_char;

    #[test]
    fn custom_allocator_has_identity_but_default_is_null() {
        bootstrap();
        // SAFETY: this implementation does not inspect the callback context.
        unsafe {
            assert!(api::CFAllocatorGetDefault().is_null());
            let allocator = api::CFAllocatorCreate(std::ptr::null(), std::ptr::null());
            assert!(!allocator.is_null());
            assert_eq!(api::CFGetTypeID(allocator), api::CFAllocatorGetTypeID());
            api::CFRelease(allocator);
        }
    }

    /// The fixture in `rosemary/tests/cfstring.rs`, run against this crate
    /// directly: create, measure, size a buffer, convert, release.
    #[test]
    fn a_string_round_trips_the_way_the_guest_fixture_does() {
        bootstrap();
        let text = c"fmodstudio";
        // SAFETY: a NUL-terminated literal and a real encoding.
        unsafe {
            let s = api::CFStringCreateWithCString(
                std::ptr::null(),
                text.as_ptr(),
                string::encoding::UTF8,
            );
            assert!(!s.is_null());
            let len = api::CFStringGetLength(s);
            assert_eq!(len, 10);
            let max = api::CFStringGetMaximumSizeForEncoding(len, string::encoding::UTF8);
            assert_eq!(max, 30, "three bytes a UTF-16 unit, as Apple's answers");

            let mut buf = [0 as c_char; 256];
            let ok = api::CFStringGetCString(s, buf.as_mut_ptr(), max + 1, string::encoding::UTF8);
            assert_eq!(ok, 1, "the conversion must succeed");
            assert_eq!(
                std::ffi::CStr::from_ptr(buf.as_ptr()).to_bytes(),
                b"fmodstudio"
            );
            api::CFRelease(s);
        }
    }

    /// The characters a caller passes are the characters it gets back, counted
    /// in code units rather than in the bytes they encode to.
    #[test]
    fn a_string_is_built_from_code_units_and_keeps_them() {
        bootstrap();
        // "hi" then U+00E9 and U+4E2D - one byte, two and three in UTF-8, so a
        // length in bytes and one in characters cannot agree.
        let text = "hi\u{00E9}\u{4E2D}";
        let units: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: a live buffer described by the count that follows it.
        unsafe {
            let s = api::CFStringCreateWithCharacters(
                std::ptr::null(),
                units.as_ptr(),
                units.len() as base::CFIndex,
            );
            assert!(!s.is_null());
            assert_eq!(api::CFStringGetLength(s), 4, "characters, not bytes");
            let read = string::as_string(s).expect("a string");
            assert!(read.is_wide());
            assert_eq!(read.utf16(), units);
            assert_eq!(read.utf8(), text.as_bytes());

            // A copy is the same string, still measured in characters.
            let copy = api::CFStringCreateCopy(std::ptr::null(), s);
            assert_eq!(api::CFStringGetLength(copy), 4);
            assert_eq!(api::CFEqual(s, copy), 1);
            api::CFRelease(copy);
            api::CFRelease(s);
        }
    }

    /// No characters is the empty string; a negative count and a null pointer
    /// with characters to read are refused.
    #[test]
    fn a_string_of_no_characters_is_empty_rather_than_null() {
        bootstrap();
        // SAFETY: null with a zero count reads nothing.
        unsafe {
            let empty = api::CFStringCreateWithCharacters(std::ptr::null(), std::ptr::null(), 0);
            assert!(!empty.is_null());
            assert_eq!(api::CFStringGetLength(empty), 0);
            api::CFRelease(empty);
            assert!(
                api::CFStringCreateWithCharacters(std::ptr::null(), std::ptr::null(), 4).is_null(),
                "there is nothing to read at null"
            );
            let units = [b'a' as u16];
            assert!(
                api::CFStringCreateWithCharacters(std::ptr::null(), units.as_ptr(), -1).is_null(),
                "a negative count is not a length"
            );
        }
    }

    /// A mutable copy is a separate, writable string that starts out equal -
    /// and stays measured the way the original was.
    #[test]
    fn a_mutable_copy_is_independent_and_keeps_its_characters() {
        bootstrap();
        let text = "caf\u{00E9}";
        let units: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: a live buffer, and strings this test made.
        unsafe {
            let original = api::CFStringCreateWithCharacters(
                std::ptr::null(),
                units.as_ptr(),
                units.len() as base::CFIndex,
            );
            let copy = api::CFStringCreateMutableCopy(std::ptr::null(), 0, original);
            assert!(!copy.is_null());
            assert_eq!(api::CFEqual(original, copy), 1);
            assert_eq!(api::CFStringGetLength(copy), 4, "characters, not bytes");

            api::CFStringAppendCString(copy, c" au lait".as_ptr(), string::encoding::UTF8);
            assert_eq!(
                api::CFStringGetLength(copy),
                12,
                "the append is counted in characters too"
            );
            assert_eq!(
                string::as_string(copy).unwrap().utf8(),
                "caf\u{00E9} au lait".as_bytes()
            );
            // The original is untouched: a copy shares no storage with it.
            assert_eq!(api::CFStringGetLength(original), 4);
            assert_eq!(api::CFEqual(original, copy), 0);
            api::CFRelease(copy);
            api::CFRelease(original);
        }
    }

    /// The 8-bit path, which is what almost every guest copies, and the two
    /// arguments that are not a string.
    #[test]
    fn a_mutable_copy_of_an_ordinary_string_appends() {
        bootstrap();
        // SAFETY: a literal, and null in place of a string.
        unsafe {
            let original = api::CFStringCreateWithCString(
                std::ptr::null(),
                c"abc".as_ptr(),
                string::encoding::UTF8,
            );
            // A maximum length is ignored, as `CFStringCreateMutable`'s is.
            let copy = api::CFStringCreateMutableCopy(std::ptr::null(), 4, original);
            api::CFStringAppendCString(copy, c"defgh".as_ptr(), string::encoding::UTF8);
            assert_eq!(string::as_string(copy).unwrap().utf8(), b"abcdefgh");
            api::CFRelease(copy);
            api::CFRelease(original);
            assert!(
                api::CFStringCreateMutableCopy(std::ptr::null(), 0, std::ptr::null()).is_null(),
                "there is nothing to copy"
            );
        }
    }

    /// Case mapping is Unicode's, in whichever representation the string has,
    /// and only a mutable string is rewritten.
    #[test]
    fn a_mutable_string_is_recased_in_place() {
        bootstrap();
        // SAFETY: strings this test made, and a null locale.
        unsafe {
            let s = api::CFStringCreateWithCString(
                std::ptr::null(),
                c"Stra\xc3\x9fe".as_ptr(),
                string::encoding::UTF8,
            );
            let copy = api::CFStringCreateMutableCopy(std::ptr::null(), 0, s);
            api::CFStringUppercase(copy, std::ptr::null());
            assert_eq!(
                string::as_string(copy).unwrap().utf8(),
                "STRASSE".as_bytes(),
                "the full mapping, so the sharp s becomes two letters"
            );
            api::CFStringLowercase(copy, std::ptr::null());
            assert_eq!(string::as_string(copy).unwrap().utf8(), b"strasse");

            // An immutable string is left alone rather than written through.
            api::CFStringUppercase(s, std::ptr::null());
            assert_eq!(
                string::as_string(s).unwrap().utf8(),
                "Stra\u{00DF}e".as_bytes()
            );
            api::CFRelease(copy);
            api::CFRelease(s);

            // The wide path, which stays wide and stays counted in characters.
            let units: Vec<u16> = "caf\u{00E9}".encode_utf16().collect();
            let wide = api::CFStringCreateWithCharacters(
                std::ptr::null(),
                units.as_ptr(),
                units.len() as base::CFIndex,
            );
            let wide = api::CFStringCreateMutableCopy(std::ptr::null(), 0, wide);
            api::CFStringUppercase(wide, std::ptr::null());
            let read = string::as_string(wide).unwrap();
            assert!(read.is_wide());
            assert_eq!(api::CFStringGetLength(wide), 4);
            assert_eq!(read.utf8(), "CAF\u{00C9}".as_bytes());
            api::CFRelease(wide);
        }
    }

    /// The characters come back the way they went in, and a range outside the
    /// string writes nothing rather than a short answer.
    #[test]
    fn the_characters_are_read_back_by_index_and_by_range() {
        bootstrap();
        let units: Vec<u16> = "caf\u{00E9}s".encode_utf16().collect();
        // SAFETY: a live buffer, and out-parameters sized below.
        unsafe {
            let s = api::CFStringCreateWithCharacters(
                std::ptr::null(),
                units.as_ptr(),
                units.len() as base::CFIndex,
            );
            assert_eq!(api::CFStringGetCharacterAtIndex(s, 3), 0x00E9);
            assert_eq!(api::CFStringGetCharacterAtIndex(s, 5), 0, "past the end");
            assert_eq!(api::CFStringGetCharacterAtIndex(s, -1), 0);

            let mut buffer = [0u16; 8];
            api::CFStringGetCharacters(s, 1, 3, buffer.as_mut_ptr());
            assert_eq!(&buffer[..3], &units[1..4]);
            buffer = [0u16; 8];
            api::CFStringGetCharacters(s, 3, 4, buffer.as_mut_ptr());
            assert_eq!(buffer, [0u16; 8], "a range past the end writes nothing");

            // The pointer is only handed out when it costs nothing.
            assert!(!api::CFStringGetCharactersPtr(s).is_null());
            let eight_bit = api::CFStringCreateWithCString(
                std::ptr::null(),
                c"abc".as_ptr(),
                string::encoding::UTF8,
            );
            assert!(api::CFStringGetCharactersPtr(eight_bit).is_null());

            // Appending characters keeps the target wide.
            let mutable = api::CFStringCreateMutableCopy(std::ptr::null(), 0, s);
            let tail: Vec<u16> = "!".encode_utf16().collect();
            api::CFStringAppendCharacters(mutable, tail.as_ptr(), 1);
            assert_eq!(api::CFStringGetLength(mutable), 6);
            assert!(string::as_string(mutable).unwrap().is_wide());

            api::CFRelease(mutable);
            api::CFRelease(eight_bit);
            api::CFRelease(s);
        }
    }

    /// Unreal hands `wchar_t` text over as UTF-32LE; a nil here was a nil
    /// window title.
    #[test]
    fn a_string_can_be_created_from_utf32_and_utf16_bytes() {
        bootstrap();
        let text = "A Hat in Time \u{1F3A9}";
        let read = |bytes: &[u8], encoding: u32| {
            // SAFETY: a live buffer; the string is released here.
            unsafe {
                let s = api::CFStringCreateWithBytes(
                    std::ptr::null(),
                    bytes.as_ptr(),
                    bytes.len() as base::CFIndex,
                    encoding,
                    0,
                );
                assert!(!s.is_null(), "encoding {encoding:#x} was refused");
                let utf8 = string::as_string(s).unwrap().utf8();
                api::CFRelease(s);
                String::from_utf8(utf8).unwrap()
            }
        };
        let le32: Vec<u8> = text
            .chars()
            .flat_map(|c| (c as u32).to_le_bytes())
            .collect();
        let be32: Vec<u8> = text
            .chars()
            .flat_map(|c| (c as u32).to_be_bytes())
            .collect();
        let be16: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
        assert_eq!(read(&le32, string::encoding::UTF32_LE), text);
        assert_eq!(read(&be32, string::encoding::UTF32_BE), text);
        assert_eq!(read(&le32, string::encoding::UTF32), text);
        let marked: Vec<u8> = [0, 0, 0xFE, 0xFF].into_iter().chain(be32).collect();
        assert_eq!(read(&marked, string::encoding::UTF32), text);
        assert_eq!(read(&be16, string::encoding::UTF16_BE), text);
    }

    /// `CFStringGetBytes` in UTF-16: the byte order the encoding names, a mark
    /// only where Apple writes one, and a character count that is not the byte
    /// count.
    #[test]
    fn the_bytes_can_be_asked_for_as_utf16() {
        bootstrap();
        let units: Vec<u16> = "hi\u{00E9}".encode_utf16().collect();
        // SAFETY: a live buffer and out-parameters sized here.
        unsafe {
            let s = api::CFStringCreateWithCharacters(
                std::ptr::null(),
                units.as_ptr(),
                units.len() as base::CFIndex,
            );
            let mut buffer = [0u8; 16];
            let mut used = 0;
            let converted = api::CFStringGetBytes(
                s,
                0,
                3,
                string::encoding::UTF16_BE,
                0,
                0,
                buffer.as_mut_ptr(),
                buffer.len() as base::CFIndex,
                &mut used,
            );
            assert_eq!(converted, 3, "characters, not bytes");
            assert_eq!(used, 6);
            assert_eq!(&buffer[..6], &[0, b'h', 0, b'i', 0x00, 0xE9]);

            // The external representation of the order-less encoding carries a
            // byte-order mark, and the mark is not a character.
            let mut used = 0;
            let converted = api::CFStringGetBytes(
                s,
                0,
                3,
                string::encoding::UTF16,
                0,
                1,
                buffer.as_mut_ptr(),
                buffer.len() as base::CFIndex,
                &mut used,
            );
            assert_eq!(converted, 3);
            assert_eq!(used, 8);
            assert_eq!(&buffer[..4], &[0xFF, 0xFE, b'h', 0]);

            // A buffer that holds one and a half characters holds one.
            let mut used = 0;
            let converted = api::CFStringGetBytes(
                s,
                0,
                3,
                string::encoding::UTF16_LE,
                0,
                0,
                buffer.as_mut_ptr(),
                3,
                &mut used,
            );
            assert_eq!(converted, 1, "half a character is not a character");
            assert_eq!(used, 2);

            // A null buffer is the question of how much room is needed.
            let mut used = 0;
            api::CFStringGetBytes(
                s,
                0,
                3,
                string::encoding::UTF16,
                0,
                0,
                std::ptr::null_mut(),
                0,
                &mut used,
            );
            assert_eq!(used, 6);
            api::CFRelease(s);
        }
    }

    /// `-UTF8String` has to answer for a wide string too, where
    /// `CFStringGetCStringPtr` correctly does not.
    #[test]
    fn a_wide_string_still_answers_utf8_string() {
        bootstrap();
        let units: Vec<u16> = "caf\u{00E9}".encode_utf16().collect();
        // SAFETY: a live buffer, and a message to a string.
        unsafe {
            let s = api::CFStringCreateWithCharacters(
                std::ptr::null(),
                units.as_ptr(),
                units.len() as base::CFIndex,
            );
            assert!(
                api::CFStringGetCStringPtr(s, string::encoding::UTF8).is_null(),
                "the answer is not free, so there is none"
            );
            let text = send::send0_usize(s as objc::object::Id, objc::sel::get("UTF8String"));
            assert_ne!(text, 0, "but -UTF8String always has one");
            assert_eq!(
                std::ffi::CStr::from_ptr(text as *const c_char).to_bytes(),
                "caf\u{00E9}".as_bytes()
            );
            api::CFRelease(s);
        }
    }

    /// A buffer one byte too small is refused rather than truncated.
    #[test]
    fn a_buffer_that_does_not_fit_is_refused() {
        bootstrap();
        unsafe {
            let s = api::CFStringCreateWithCString(
                std::ptr::null(),
                c"abcde".as_ptr(),
                string::encoding::UTF8,
            );
            let mut buf = [0 as c_char; 8];
            // Five characters plus a NUL needs six.
            assert_eq!(
                api::CFStringGetCString(s, buf.as_mut_ptr(), 5, string::encoding::UTF8),
                0
            );
            assert_eq!(
                api::CFStringGetCString(s, buf.as_mut_ptr(), 6, string::encoding::UTF8),
                1
            );
            api::CFRelease(s);
        }
    }

    /// A constant string is one of this runtime's objects with nothing copied,
    /// and releasing it does not free the image's `__DATA`.
    #[test]
    fn a_constant_string_is_read_in_place_and_never_freed() {
        bootstrap();
        // The four words the compiler lays out, built here by hand: the class
        // reference the image would import, flags, the bytes, and the length.
        #[repr(C)]
        struct Literal {
            isa: u64,
            flags: u64,
            str: *const u8,
            length: i64,
        }
        let bytes = c"a host literal";
        let literal = Literal {
            isa: objc::class::ptr(string::constant_class()) as usize as u64,
            flags: 0x7c8,
            str: bytes.as_ptr() as *const u8,
            length: 14,
        };
        let object = &literal as *const Literal as CFTypeRef;

        // SAFETY: `object` is exactly the layout `__NSCFConstantString` has.
        unsafe {
            assert_eq!(api::CFStringGetLength(object), 14);
            assert_eq!(api::CFGetTypeID(object), api::CFStringGetTypeID());
            let ptr = api::CFStringGetCStringPtr(object, string::encoding::UTF8);
            assert_eq!(
                ptr as *const u8,
                bytes.as_ptr() as *const u8,
                "not in place"
            );

            // Released as many times as anyone likes, and still there.
            api::CFRelease(object);
            api::CFRelease(object);
            assert_eq!(api::CFStringGetLength(object), 14);
        }
    }

    /// `CFEqual` on two strings is about the characters, not the pointers -
    /// which is the first thing toll-free bridging has to get right.
    #[test]
    fn two_strings_with_the_same_characters_are_equal() {
        bootstrap();
        // SAFETY: real strings and a real encoding.
        unsafe {
            let a = api::CFStringCreateWithCString(
                std::ptr::null(),
                c"same".as_ptr(),
                string::encoding::UTF8,
            );
            let b = api::CFStringCreateWithCString(
                std::ptr::null(),
                c"same".as_ptr(),
                string::encoding::UTF8,
            );
            let c = api::CFStringCreateWithCString(
                std::ptr::null(),
                c"other".as_ptr(),
                string::encoding::UTF8,
            );
            assert_ne!(a, b, "two separate objects");
            assert_eq!(api::CFEqual(a, b), 1);
            assert_eq!(api::CFEqual(a, c), 0);
            assert_eq!(api::CFHash(a), api::CFHash(b));
            api::CFRelease(a);
            api::CFRelease(b);
            api::CFRelease(c);
        }
    }
}

/// The collections, checked against what a guest actually does with them.
///
/// `libYoYoGamepad` builds an array of dictionaries of numbers to hand IOKit as
/// HID matching criteria, then reads numbers back out of the properties IOKit
/// returns - which is every one of these in one shape.
#[cfg(test)]
mod collection_tests {
    use super::*;
    use base::CFTypeRef;
    use collections::number_type;
    use std::ffi::c_void;

    fn number(kind: i32, value: i64) -> CFTypeRef {
        // SAFETY: `value` outlives the call and is wider than any `kind` here.
        unsafe {
            api::CFNumberCreate(
                std::ptr::null(),
                kind,
                &value as *const i64 as *const c_void,
            )
        }
    }

    fn string(text: &std::ffi::CStr) -> CFTypeRef {
        // SAFETY: a NUL-terminated literal.
        unsafe {
            api::CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), string::encoding::UTF8)
        }
    }

    #[test]
    fn a_matching_dictionary_reads_back_the_way_iokit_would_build_one() {
        bootstrap();
        let page = string(c"DeviceUsagePage");
        let usage = string(c"DeviceUsage");
        let (one, four) = (
            number(number_type::SINT32, 1),
            number(number_type::SINT32, 4),
        );

        // SAFETY: real objects and the type callbacks this crate exports.
        unsafe {
            let keys = [page, usage];
            let values = [one, four];
            let dict = api::CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                2,
                &callbacks::TYPE_KEY_CALLBACKS,
                &callbacks::TYPE_VALUE_CALLBACKS,
            );
            assert!(!dict.is_null());
            assert_eq!(api::CFDictionaryGetCount(dict), 2);
            assert_eq!(api::CFGetTypeID(dict), api::CFDictionaryGetTypeID());

            // Looked up by a *different* string object with the same
            // characters, which is what a guest always does.
            let same = string(c"DeviceUsage");
            assert_ne!(same, usage, "a distinct object, or this proves nothing");
            let found = api::CFDictionaryGetValue(dict, same);
            assert_eq!(found, four);

            let mut read: i32 = 0;
            assert_eq!(
                api::CFNumberGetValue(
                    found,
                    number_type::SINT32,
                    &mut read as *mut i32 as *mut c_void
                ),
                1
            );
            assert_eq!(read, 4);

            // And in an array, as the matching call takes it.
            let array = api::CFArrayCreate(
                std::ptr::null(),
                [dict].as_ptr(),
                1,
                &callbacks::TYPE_VALUE_CALLBACKS,
            );
            assert_eq!(api::CFArrayGetCount(array), 1);
            assert_eq!(api::CFArrayGetValueAtIndex(array, 0), dict);
            assert!(
                api::CFArrayGetValueAtIndex(array, 1).is_null(),
                "past the end"
            );

            api::CFRelease(same);
            api::CFRelease(array);
            api::CFRelease(dict);
            api::CFRelease(page);
            api::CFRelease(usage);
            api::CFRelease(one);
            api::CFRelease(four);
        }
    }

    #[test]
    fn dictionary_get_value_if_present_reports_and_writes_only_a_match() {
        bootstrap();
        let key = string(c"key");
        let missing = string(c"missing");
        let value = number(number_type::SINT32, 7);

        // SAFETY: real objects, callbacks, and a writable result pointer.
        unsafe {
            let dictionary = api::CFDictionaryCreate(
                std::ptr::null(),
                [key].as_ptr(),
                [value].as_ptr(),
                1,
                &callbacks::TYPE_KEY_CALLBACKS,
                &callbacks::TYPE_VALUE_CALLBACKS,
            );
            let mut result = missing;
            assert_eq!(
                api::CFDictionaryGetValueIfPresent(dictionary, key, &mut result),
                1
            );
            assert_eq!(result, value);

            result = value;
            assert_eq!(
                api::CFDictionaryGetValueIfPresent(dictionary, missing, &mut result),
                0
            );
            assert_eq!(result, value, "an absent key leaves the result unchanged");
            assert_eq!(
                api::CFDictionaryGetValueIfPresent(dictionary, key, std::ptr::null_mut()),
                1
            );

            api::CFRelease(dictionary);
            api::CFRelease(key);
            api::CFRelease(missing);
            api::CFRelease(value);
        }
    }

    /// A collection retains what it holds, so an element released by its
    /// creator is still there.
    #[test]
    fn a_collection_holds_a_reference_of_its_own() {
        bootstrap();
        let element = string(c"held");
        // SAFETY: real objects and the type callbacks.
        unsafe {
            let array = api::CFArrayCreate(
                std::ptr::null(),
                [element].as_ptr(),
                1,
                &callbacks::TYPE_VALUE_CALLBACKS,
            );
            assert_eq!(api::CFGetRetainCount(element), 2);
            api::CFRelease(element);
            // Still readable: the array's reference is the one left.
            assert_eq!(
                api::CFStringGetLength(api::CFArrayGetValueAtIndex(array, 0)),
                4
            );
            api::CFRelease(array);
        }
    }

    /// Null callbacks mean "these are not objects": nothing is retained and
    /// equality is by pointer. A guest that puts integers in an array relies on
    /// it.
    #[test]
    fn null_callbacks_leave_the_elements_alone() {
        bootstrap();
        // SAFETY: the values are never dereferenced, which is the claim.
        unsafe {
            let raw = [7 as CFTypeRef, 9 as CFTypeRef];
            let array = api::CFArrayCreate(std::ptr::null(), raw.as_ptr(), 2, std::ptr::null());
            assert_eq!(api::CFArrayGetCount(array), 2);
            assert_eq!(api::CFArrayGetValueAtIndex(array, 1), 9 as CFTypeRef);
            api::CFRelease(array);
        }
    }

    #[test]
    fn array_apply_function_visits_only_the_requested_range() {
        bootstrap();
        unsafe extern "C" fn collect(value: CFTypeRef, context: *mut c_void) {
            // SAFETY: the test passes a pointer to this exact vector.
            unsafe { (&mut *(context as *mut Vec<CFTypeRef>)).push(value) };
        }

        // SAFETY: the raw values are not objects and the callback context is
        // valid for the synchronous invocation.
        unsafe {
            let values = [
                3usize as CFTypeRef,
                5usize as CFTypeRef,
                7usize as CFTypeRef,
            ];
            let array = api::CFArrayCreate(std::ptr::null(), values.as_ptr(), 3, std::ptr::null());
            let mut seen = Vec::new();
            api::CFArrayApplyFunction(
                array,
                1,
                2,
                Some(collect),
                &mut seen as *mut Vec<CFTypeRef> as *mut c_void,
            );
            assert_eq!(seen, values[1..]);

            api::CFArrayApplyFunction(
                array,
                2,
                2,
                Some(collect),
                &mut seen as *mut Vec<CFTypeRef> as *mut c_void,
            );
            assert_eq!(seen, values[1..], "an out-of-bounds range is ignored");
            api::CFRelease(array);
        }
    }

    /// A `SInt64` past the range of a `double` comes back exactly. Holding
    /// every number as an `f64` would lose the low bits of a HID cookie or a
    /// serial number with nothing to say so.
    #[test]
    fn a_wide_integer_survives_the_round_trip() {
        bootstrap();
        let big = (1i64 << 62) + 12345;
        let n = number(number_type::SINT64, big);
        // SAFETY: a real number and a matching out-parameter.
        unsafe {
            let mut read: i64 = 0;
            assert_eq!(
                api::CFNumberGetValue(n, number_type::SINT64, &mut read as *mut i64 as *mut c_void),
                1
            );
            assert_eq!(read, big);
            assert_eq!(api::CFNumberIsFloatType(n), 0);

            // And asked for as a `SInt32` it says so rather than lying.
            let mut narrow: i32 = 0;
            assert_eq!(
                api::CFNumberGetValue(
                    n,
                    number_type::SINT32,
                    &mut narrow as *mut i32 as *mut c_void
                ),
                0,
                "a value that does not fit must report that it does not"
            );
            api::CFRelease(n);
        }
    }

    /// A number reports the canonical type it was created with, and the width
    /// of that type, whatever C name the caller used.
    #[test]
    fn a_number_reports_its_canonical_type() {
        bootstrap();
        for (created, reported, size) in [
            (number_type::CHAR, number_type::SINT8, 1),
            (number_type::SHORT, number_type::SINT16, 2),
            (number_type::INT, number_type::SINT32, 4),
            (number_type::SINT32, number_type::SINT32, 4),
            (number_type::LONG, number_type::SINT64, 8),
            (number_type::CFINDEX, number_type::SINT64, 8),
            (number_type::FLOAT, number_type::FLOAT32, 4),
            (number_type::DOUBLE, number_type::FLOAT64, 8),
            (number_type::CGFLOAT, number_type::FLOAT64, 8),
        ] {
            let n = number(created, 0);
            // SAFETY: a number made above.
            unsafe {
                assert_eq!(api::CFNumberGetType(n), reported as isize, "{created}");
                assert_eq!(api::CFNumberGetByteSize(n), size, "{created}");
                api::CFRelease(n);
            }
        }
        // SAFETY: an object that is not a number.
        unsafe {
            let s = string(c"not a number");
            assert_eq!(api::CFNumberGetType(s), 0);
            api::CFRelease(s);
        }
    }

    /// A boolean reads as the number it is, and reports the char type Apple's
    /// does.
    #[test]
    fn a_boolean_is_read_as_a_number() {
        bootstrap();
        // SAFETY: the process's two booleans.
        unsafe {
            for (object, expected) in [
                (collections::boolean(true), 1),
                (collections::boolean(false), 0),
            ] {
                let mut read: i32 = -1;
                assert_eq!(
                    api::CFNumberGetValue(
                        object,
                        number_type::SINT32,
                        &mut read as *mut i32 as *mut c_void
                    ),
                    1
                );
                assert_eq!(read, expected);
                assert_eq!(api::CFNumberGetType(object), number_type::CHAR as isize);
                assert_eq!(api::CFNumberGetByteSize(object), 1);
                assert_eq!(api::CFNumberIsFloatType(object), 0);
            }
        }
    }

    /// The constant numbers clang lays out are numbers to CoreFoundation too,
    /// and an integer's encoding gives its type.
    #[test]
    fn a_constant_number_is_read_by_its_layout() {
        bootstrap();
        let nsnumber = base::abstract_class(c"NSNumber", None);
        let integer = objc::class::lookup(c"NSConstantIntegerNumber").unwrap_or_else(|| {
            let class = objc::class::allocate_pair(Some(nsnumber), c"NSConstantIntegerNumber", 0)
                .expect("a fresh class");
            objc::class::register_pair(class);
            class
        });
        let double = objc::class::lookup(c"NSConstantDoubleNumber").unwrap_or_else(|| {
            let class = objc::class::allocate_pair(Some(nsnumber), c"NSConstantDoubleNumber", 0)
                .expect("a fresh class");
            objc::class::register_pair(class);
            class
        });
        #[repr(C)]
        struct Integer(*const objc::class::Class, *const std::ffi::c_char, i64);
        #[repr(C)]
        struct Double(*const objc::class::Class, f64);
        let int = Integer(objc::class::ptr(integer), c"i".as_ptr(), -9);
        let unsigned = Integer(objc::class::ptr(integer), c"I".as_ptr(), 9);
        let real = Double(objc::class::ptr(double), 2.5);
        // SAFETY: each is laid out as its class's instances are.
        unsafe {
            let int = &int as *const Integer as CFTypeRef;
            assert_eq!(api::CFNumberGetType(int), number_type::SINT32 as isize);
            let mut read: i64 = 0;
            assert_eq!(
                api::CFNumberGetValue(
                    int,
                    number_type::SINT64,
                    &mut read as *mut i64 as *mut c_void
                ),
                1
            );
            assert_eq!(read, -9);
            let unsigned = &unsigned as *const Integer as CFTypeRef;
            assert_eq!(api::CFNumberGetType(unsigned), number_type::SINT64 as isize);
            let real = &real as *const Double as CFTypeRef;
            assert_eq!(api::CFNumberGetType(real), number_type::FLOAT64 as isize);
            assert_eq!(api::CFNumberIsFloatType(real), 1);
        }
    }

    /// A negative `SInt8` is negative, which sign extension is the whole of.
    #[test]
    fn a_narrow_signed_number_is_sign_extended() {
        bootstrap();
        let byte: i8 = -3;
        // SAFETY: a one-byte value read as `kCFNumberSInt8Type`.
        unsafe {
            let n = api::CFNumberCreate(
                std::ptr::null(),
                number_type::SINT8,
                &byte as *const i8 as *const c_void,
            );
            let mut read: i64 = 0;
            assert_eq!(
                api::CFNumberGetValue(n, number_type::SINT64, &mut read as *mut i64 as *mut c_void),
                1
            );
            assert_eq!(read, -3);
            api::CFRelease(n);
        }
    }
}
