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
/// Only on Darwin: nowhere else is there another CoreFoundation to meet.
#[cfg(target_os = "macos")]
pub mod foreign;
mod fsevents;
/// Mach ports bridged into the run loop. Off Darwin: there the host's own Mach
/// is the guest's, as [`foreign`] is for the host's CoreFoundation.
#[cfg(not(target_os = "macos"))]
pub mod machport;
pub mod preferences;
pub mod runloop;
mod send;
mod socket;
pub mod string;
pub mod sysconfig;
pub mod timezone;
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
        "CFArrayCreateCopy" => api::CFArrayCreateCopy,
        "CFArrayInsertValueAtIndex" => api::CFArrayInsertValueAtIndex,
        "CFArrayRemoveValueAtIndex" => api::CFArrayRemoveValueAtIndex,
        "CFArrayRemoveAllValues" => api::CFArrayRemoveAllValues,
        "CFSetGetTypeID" => api::CFSetGetTypeID,
        "CFSetApplyFunction" => api::CFSetApplyFunction,
        "CFSetGetCount" => api::CFSetGetCount,
        "CFSetGetValues" => api::CFSetGetValues,
        "CFSetCreate" => api::CFSetCreate,
        "CFSetAddValue" => api::CFSetAddValue,
        "CFSetCreateMutable" => api::CFSetCreateMutable,
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
        "CFRunLoopCopyCurrentMode" => api::CFRunLoopCopyCurrentMode,
        "CFRunLoopPerformBlock" => api::CFRunLoopPerformBlock,
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
        "CFArrayContainsValue" => api::CFArrayContainsValue,
        "CFArrayAppendArray" => api::CFArrayAppendArray,
        "CFRunLoopRemoveSource" => api::CFRunLoopRemoveSource,
        "CFRunLoopContainsSource" => api::CFRunLoopContainsSource,
        "CFAbsoluteTimeGetCurrent" => api::CFAbsoluteTimeGetCurrent,
        "CFRunLoopTimerCreate" => api::CFRunLoopTimerCreate,
        "CFRunLoopAddTimer" => api::CFRunLoopAddTimer,
        "CFRunLoopRemoveTimer" => api::CFRunLoopRemoveTimer,
        "CFRunLoopTimerInvalidate" => api::CFRunLoopTimerInvalidate,
        "CFRunLoopTimerIsValid" => api::CFRunLoopTimerIsValid,
        "CFStringHasPrefix" => api::CFStringHasPrefix,
        "CFStringHasSuffix" => api::CFStringHasSuffix,
        "CFStringGetRangeOfCharacterClusterAtIndex" => api::CFStringGetRangeOfCharacterClusterAtIndex,
        "CFNullGetTypeID" => api::CFNullGetTypeID,
        "CFLocaleCreate" => api::CFLocaleCreate,
        "CFErrorGetTypeID" => api::CFErrorGetTypeID,
        "CFErrorGetCode" => api::CFErrorGetCode,
        "CFErrorGetDomain" => api::CFErrorGetDomain,
        "CFErrorCopyDescription" => api::CFErrorCopyDescription,
        "CFErrorCopyUserInfo" => api::CFErrorCopyUserInfo,
        "CFBundleGetFunctionPointerForName" => api::CFBundleGetFunctionPointerForName,
        // The Mach port family, which is absent on Darwin: there the host's own
        // Mach is the guest's and this tree's port space is not the one in play.
        #[cfg(not(target_os = "macos"))]
        "CFMachPortCreateRunLoopSource" => machport::create_run_loop_source,
        #[cfg(not(target_os = "macos"))]
        "CFMachPortInvalidate" => machport::invalidate,
        #[cfg(not(target_os = "macos"))]
        "CFMachPortGetTypeID" => machport::get_type_id,
        #[cfg(not(target_os = "macos"))]
        "CFMachPortCreateWithPort" => machport::create_with_port,
        #[cfg(not(target_os = "macos"))]
        "CFMachPortGetPort" => machport::get_port,
        #[cfg(not(target_os = "macos"))]
        "CFMachPortIsValid" => machport::is_valid,
    }
    groups {
        calendar,
        preferences,
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
    let mut out = vec![
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
    let Some(class) = objc::class::lookup(c"NSNull") else {
        // Foundation registers `NSNull`, and a process using CoreFoundation
        // without it has no null to publish. Zero here is the same as not
        // publishing the symbol, which is what it was before.
        return 0;
    };
    // `CFNullGetTypeID` registers the name in a process that has no Foundation to
    // install `+null` on it, so the class being found is not the same statement as
    // there being a null to hand out. Sending `+null` to a class that has never
    // had it is a message to nothing, and the answer is the same zero.
    if class.method(objc::sel::get("null")).is_none() {
        return 0;
    }
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

    /// The fixture in `rosetta3/tests/cfstring.rs`, run against this crate
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

    /// A prefix is a *character* prefix, in whichever representation each side
    /// happens to keep - and a string is a prefix of itself, which is the case a
    /// caller testing "starts with" relies on when it passes its own buffer.
    #[test]
    fn a_prefix_is_compared_by_characters_in_either_representation() {
        bootstrap();
        // "cafe" with the accent as a *combining* mark, so its wide form is five
        // code units for four characters: an 8-bit string and a wide one that
        // spell the same thing must agree about the prefix.
        let narrow = string::from_bytes("com.example.app".as_bytes(), false);
        let units: Vec<u16> = "com.example".encode_utf16().collect();
        let wide = string::from_utf16(&units);
        let accented_units: Vec<u16> = "cafe\u{0301}".encode_utf16().collect();
        let accented = string::from_utf16(&accented_units);

        // SAFETY: four strings this test made.
        unsafe {
            assert_eq!(api::CFStringHasPrefix(narrow, wide), 1);
            assert_eq!(api::CFStringHasPrefix(wide, narrow), 0, "not symmetric");
            assert_eq!(
                api::CFStringHasPrefix(narrow, narrow),
                1,
                "a string is its own prefix"
            );
            assert_eq!(
                api::CFStringHasSuffix(narrow, string::from_bytes(b"app", false)),
                1
            );
            assert_eq!(api::CFStringHasSuffix(narrow, wide), 0);

            // A prefix is a run of *code units*, which is what this function
            // compares and what a caller matching text expects: the mark after
            // "cafe" does not stop "cafe" from being a prefix, and nothing after
            // it does either.
            let cafe: Vec<u16> = "cafe".encode_utf16().collect();
            assert_eq!(
                api::CFStringHasPrefix(accented, string::from_utf16(&cafe)),
                1,
                "the base spelling is still a prefix of the combined one"
            );
            let beyond: Vec<u16> = "cafe\u{0301}!".encode_utf16().collect();
            assert_eq!(
                api::CFStringHasPrefix(accented, string::from_utf16(&beyond)),
                0,
                "but one character longer is not"
            );
            // A character above the BMP is two units, and either half of it is
            // half a prefix - so the two halves are prefixes and the whole is too.
            let emoji_units: Vec<u16> = "\u{1F600}x".encode_utf16().collect();
            let with_emoji = string::from_utf16(&emoji_units);
            let whole_emoji: Vec<u16> = "\u{1F600}".encode_utf16().collect();
            assert_eq!(
                api::CFStringHasPrefix(with_emoji, string::from_utf16(&whole_emoji)),
                1
            );
            let half_emoji = vec![whole_emoji[0]];
            assert_eq!(
                api::CFStringHasPrefix(with_emoji, string::from_utf16(&half_emoji)),
                1,
                "an unpaired surrogate is one unit of text like any other"
            );
            let one_more: Vec<u16> = "\u{1F600}y".encode_utf16().collect();
            assert_eq!(
                api::CFStringHasPrefix(with_emoji, string::from_utf16(&one_more)),
                0
            );

            // Nothing to compare is not a match, rather than a fault.
            assert_eq!(api::CFStringHasPrefix(narrow, std::ptr::null()), 0);
            assert_eq!(api::CFStringHasSuffix(std::ptr::null(), narrow), 0);
            assert_eq!(api::CFStringHasSuffix(narrow, narrow), 1);

            for made in [narrow, wide, accented, with_emoji] {
                api::CFRelease(made);
            }
        }
    }

    /// What a character is, as a caller measuring where one begins can see it.
    ///
    /// Every case here is a case where counting code units puts the boundary in
    /// the wrong place, which is the whole reason the function exists.
    #[test]
    fn a_character_cluster_is_a_character_and_not_its_code_units() {
        bootstrap();
        // "e" + combining acute, an emoji above the BMP, a family emoji, a flag,
        // and two ordinary characters. Fifteen code units, six characters.
        let text = "e\u{0301}\u{1F600}\u{1F469}\u{200D}\u{1F4BB}\u{1F1FA}\u{1F1F8}ab";
        let units: Vec<u16> = text.encode_utf16().collect();
        let s = string::from_utf16(&units);
        assert_eq!(
            units.len(),
            15,
            "the count the boundaries below are written for"
        );
        // The boundaries, as a caller walking the string would meet them.
        let expected = [(0, 2), (2, 2), (4, 5), (9, 2), (13, 1), (14, 1)];

        // SAFETY: a string this test made and an out-parameter below.
        unsafe {
            for &(location, length) in &expected {
                let mut range = api::CFRange {
                    location: -1,
                    length: -1,
                };
                // Probed at the *start* of each character, which is the index a
                // caller walking a string asks about.
                let found = api::CFStringGetRangeOfCharacterClusterAtIndex(
                    s,
                    location as base::CFIndex,
                    &mut range,
                );
                assert_eq!(
                    (found.location, found.length),
                    (location as base::CFIndex, length as base::CFIndex),
                    "the returned range for the character at {location} of {text:?}"
                );
                assert_eq!(
                    (range.location, range.length),
                    (found.location, found.length),
                    "and the out-parameter carries the same answer, which is the \
                     one no signature in this tree can return"
                );
                // Every unit inside the cluster answers for the whole of it.
                for inside in location..location + length {
                    let mut inner = api::CFRange {
                        location: -1,
                        length: -1,
                    };
                    api::CFStringGetRangeOfCharacterClusterAtIndex(
                        s,
                        inside as base::CFIndex,
                        &mut inner,
                    );
                    assert_eq!(
                        (inner.location, inner.length),
                        (found.location, found.length),
                        "unit {inside} is inside the character, so it must answer \
                         for the whole of it"
                    );
                }
            }

            // Outside the string, and a null string, are the empty range - which is
            // what the header documents, including for an index equal to the
            // length.
            for index in [units.len() as base::CFIndex, 999, -1] {
                let mut range = api::CFRange {
                    location: -1,
                    length: -1,
                };
                let found = api::CFStringGetRangeOfCharacterClusterAtIndex(s, index, &mut range);
                assert_eq!((found.location, found.length), (0, 0), "index {index}");
                assert_eq!(
                    (range.location, range.length),
                    (0, 0),
                    "and through the pointer"
                );
            }
            let empty = string::from_utf16(&[]);
            let mut range = api::CFRange {
                location: -1,
                length: -1,
            };
            let found = api::CFStringGetRangeOfCharacterClusterAtIndex(empty, 0, &mut range);
            assert_eq!((found.location, found.length), (0, 0));
            let found = api::CFStringGetRangeOfCharacterClusterAtIndex(
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
            );
            assert_eq!(
                (found.location, found.length),
                (0, 0),
                "and a null string is not a fault"
            );

            api::CFRelease(empty);
            api::CFRelease(s);
        }
    }

    /// `kCFNull`'s type ID has to be the type `CFGetTypeID` reports for the object
    /// behind that symbol, or a guest testing for a null by either spelling
    /// disagrees with itself.
    #[test]
    fn the_null_type_id_is_the_class_of_the_null() {
        bootstrap();
        // SAFETY: a `CFTypeID` and the class it names; both null-safe.
        unsafe {
            let id = api::CFNullGetTypeID();
            assert_ne!(
                id, 0,
                "a type ID of zero is every non-CoreFoundation object"
            );
            // Asked twice it is the same value, which also says the class is
            // registered once however many times a guest asks.
            assert_eq!(api::CFNullGetTypeID(), id);
            // And `CFGetTypeID` of the null itself agrees - which only holds
            // because the class is *marked*, so the two cannot drift apart.
            let null = null_object() as base::CFTypeRef;
            if !null.is_null() {
                assert_eq!(api::CFGetTypeID(null), id);
                assert_ne!(api::CFGetTypeID(null), api::CFStringGetTypeID());
            }
        }
    }

    /// A locale is an object the caller owns, and every locale in the process
    /// answers the same questions - which is the property a caller comparing an
    /// identifier has and the reason the identifier is not stored.
    #[test]
    fn a_locale_is_an_object_and_a_string_is_not_one() {
        bootstrap();
        // SAFETY: strings and calls this test makes.
        unsafe {
            let identifier = string::from_bytes(b"fr_CA", false);
            let locale = api::CFLocaleCreate(std::ptr::null(), identifier);
            assert!(!locale.is_null(), "a locale is a live CF object");
            assert_eq!(
                api::CFGetRetainCount(locale),
                1,
                "a Create hands over a reference"
            );
            // `CFRetain`/`CFRelease` work on it, which is the part that matters: it
            // is a real object of this runtime and not a handle.
            api::CFRetain(locale);
            assert_eq!(api::CFGetRetainCount(locale), 2);
            api::CFRelease(locale);
            api::CFRelease(locale);

            // A null identifier is the current locale, and a pointer that is not a
            // string is not a locale at all.
            assert!(!api::CFLocaleCreate(std::ptr::null(), std::ptr::null()).is_null());
            assert!(
                !api::CFLocaleCreate(std::ptr::null(), identifier).is_null(),
                "and the same identifier twice is two objects"
            );
            let number = collections::new_number(collections::Number::Integer(1));
            assert!(
                api::CFLocaleCreate(std::ptr::null(), number).is_null(),
                "a number is not a locale identifier"
            );

            api::CFRelease(number);
            api::CFRelease(locale);
            api::CFRelease(identifier);
        }
    }

    /// A set holds each value once, keeps what it holds, and only a set created
    /// mutable may be added to - the same three things an array does, which is
    /// why the code is shared.
    #[test]
    fn a_set_holds_each_value_once_and_only_a_mutable_one_grows() {
        bootstrap();
        // SAFETY: objects and calls this test makes.
        unsafe {
            let one = string::from_bytes(b"one", false);
            let other = string::from_bytes(b"other", false);
            let values = [one, one, other];
            let set = api::CFSetCreate(
                std::ptr::null(),
                3,
                values.as_ptr(),
                &callbacks::TYPE_KEY_CALLBACKS,
            );
            assert!(!set.is_null());
            assert_eq!(api::CFSetGetCount(set), 2, "the repeat is not in the set");
            assert_eq!(api::CFGetTypeID(set), api::CFSetGetTypeID());
            // The set took its own reference, so releasing the creator's leaves it
            // readable - which is what a guest that puts a local into a set and
            // drops the local relies on.
            assert_eq!(api::CFGetRetainCount(one), 2);
            api::CFRelease(one);
            api::CFRelease(other);
            let mut read: [base::CFTypeRef; 2] = [std::ptr::null(); 2];
            api::CFSetGetValues(set, read.as_mut_ptr());
            assert!(read.contains(&one), "the elements are still there");

            // An immutable set does not grow, by name rather than by class.
            api::CFSetAddValue(set, string::from_bytes(b"third", false));
            assert_eq!(api::CFSetGetCount(set), 2, "an immutable set does not grow");

            // A mutable one does, and equality is by characters: a *different*
            // string object with the same text is already in the set.
            let mutable_set =
                api::CFSetCreateMutable(std::ptr::null(), 0, &callbacks::TYPE_KEY_CALLBACKS);
            assert_eq!(api::CFSetGetCount(mutable_set), 0);
            api::CFSetAddValue(mutable_set, one);
            api::CFSetAddValue(mutable_set, one);
            api::CFSetAddValue(mutable_set, string::from_bytes(b"one", false));
            assert_eq!(
                api::CFSetGetCount(mutable_set),
                1,
                "equal values are one value"
            );
            api::CFSetAddValue(mutable_set, other);
            assert_eq!(api::CFSetGetCount(mutable_set), 2);
            // And it is a set a guest can mutate: `isKindOfClass:` on the
            // Foundation side has to say so, which is why mutability is a field
            // and not a class.
            let mutable_class = objc::class::lookup(c"NSMutableSet").expect("registered");
            assert!(base::is_kind_of(mutable_set, mutable_class));
            assert!(base::is_kind_of(
                set,
                objc::class::lookup(c"NSSet").unwrap()
            ));

            // A negative count, and a count with no array, are refused.
            assert!(
                api::CFSetCreate(std::ptr::null(), -1, values.as_ptr(), std::ptr::null()).is_null()
            );
            assert!(
                api::CFSetCreate(std::ptr::null(), 2, std::ptr::null(), std::ptr::null()).is_null()
            );
            // Null callbacks means "not objects": nothing is retained and
            // equality is by pointer, which is what a set of integers needs.
            let raw = [
                7 as base::CFTypeRef,
                7 as base::CFTypeRef,
                9 as base::CFTypeRef,
            ];
            let by_pointer = api::CFSetCreate(std::ptr::null(), 3, raw.as_ptr(), std::ptr::null());
            assert_eq!(
                api::CFSetGetCount(by_pointer),
                2,
                "the same pointer is one value"
            );

            api::CFRelease(by_pointer);
            api::CFRelease(mutable_set);
            api::CFRelease(set);
        }
    }

    /// The error accessors are asked about an object this crate did not make, so
    /// they are sent rather than read out of it - and the `...Copy` spellings hand
    /// over a reference the caller owns while the `Get` does not.
    #[test]
    fn an_error_answers_its_code_domain_description_and_user_info() {
        bootstrap();
        // An `NSError` laid out the way `foundation`'s is, with a class this test
        // registers and the three methods these four functions send.
        #[repr(C)]
        struct Body {
            isa: *const objc::class::Class,
            code: i64,
        }
        let class = base::register(c"NSError", None, std::mem::size_of::<Body>(), true);
        unsafe extern "C" fn code(receiver: objc::object::Id, _sel: objc::sel::Sel) -> i64 {
            // SAFETY: the receiver is a `Body` this test laid out.
            unsafe { (*(receiver as *const Body)).code }
        }
        unsafe extern "C" fn domain(
            receiver: objc::object::Id,
            _sel: objc::sel::Sel,
        ) -> base::CFTypeRef {
            // SAFETY: the receiver is a `Body` this test laid out.
            let _ = unsafe { &*(receiver as *const Body) };
            string::from_bytes(b"com.example.test", false)
        }
        unsafe extern "C" fn description(
            receiver: objc::object::Id,
            _sel: objc::sel::Sel,
        ) -> base::CFTypeRef {
            // SAFETY: the receiver is a `Body` this test laid out.
            let _ = unsafe { &*(receiver as *const Body) };
            string::from_bytes(b"something went wrong", false)
        }
        class.add_method(
            objc::sel::get("code"),
            code as *const std::ffi::c_void,
            c"q16@0:8",
        );
        class.add_method(
            objc::sel::get("domain"),
            domain as *const std::ffi::c_void,
            c"@16@0:8",
        );
        class.add_method(
            objc::sel::get("localizedDescription"),
            description as *const std::ffi::c_void,
            c"@16@0:8",
        );
        class.add_method(
            objc::sel::get("userInfo"),
            description as *const std::ffi::c_void,
            c"@16@0:8",
        );

        // SAFETY: `Body` begins with `isa` and is the layout `class` was
        // registered with.
        let body: *mut Body = unsafe { base::allocate(class) };
        // SAFETY: a live instance whose second word is this test's.
        unsafe { (*body).code = -42 };
        let error = body as base::CFTypeRef;

        // SAFETY: an object this test made.
        unsafe {
            assert_eq!(api::CFErrorGetCode(error), -42);
            // A negative code is a failure, and a wide one survives - which is the
            // case a POSIX `errno` mapped into an error lands in.
            let wide = -((1i64 << 40) + 7);
            (*body).code = wide;
            assert_eq!(api::CFErrorGetCode(error), wide as base::CFIndex);

            // The type ID is the class, and `CFGetTypeID` agrees.
            assert_eq!(api::CFGetTypeID(error), api::CFErrorGetTypeID());
            assert_ne!(api::CFErrorGetTypeID(), 0);
            assert_ne!(api::CFErrorGetTypeID(), api::CFNullGetTypeID());

            // A `Get`: the object is not the caller's to release, and asking twice
            // gives the same string.
            let domain = api::CFErrorGetDomain(error);
            assert!(!domain.is_null());
            assert_eq!(
                string::as_string(domain).unwrap().utf8(),
                b"com.example.test"
            );

            // A `Copy`: the caller owns it, so the count is the creator's plus one
            // and the caller's release balances.
            let described = api::CFErrorCopyDescription(error);
            assert_eq!(
                api::CFGetRetainCount(described),
                2,
                "a Copy hands over a reference"
            );
            assert_eq!(
                string::as_string(described).unwrap().utf8(),
                b"something went wrong"
            );
            api::CFRelease(described);

            let info = api::CFErrorCopyUserInfo(error);
            assert!(!info.is_null(), "a `Copy` of whatever -userInfo answered");
            api::CFRelease(info);

            // Nothing at all is answered rather than a fault: zero for the code, and
            // null for the three object-valued ones.
            assert_eq!(api::CFErrorGetCode(std::ptr::null()), 0);
            assert!(api::CFErrorGetDomain(std::ptr::null()).is_null());
            assert!(api::CFErrorCopyDescription(std::ptr::null()).is_null());
            assert!(api::CFErrorCopyUserInfo(std::ptr::null()).is_null());
            // And an object that is not an error at all answers the same way, since
            // it has none of the methods.
            let not_an_error = string::from_bytes(b"x", false);
            assert_eq!(api::CFErrorGetCode(not_an_error), 0);
            assert!(api::CFErrorCopyDescription(not_an_error).is_null());
            api::CFRelease(not_an_error);

            (*body).code = 0;
        }
    }

    /// A bundle's function pointer is this runtime's own answer to "where does
    /// this symbol live", and null for one nothing defines.
    ///
    /// The lookup is the runtime's, not this crate's - a bundle function lives in
    /// a linked image and here every framework is linked into the host. A
    /// provider registered for the test stands in for the framework list the
    /// runtime installs at start-up, so what is being checked is that the name
    /// is *asked of the right place* rather than that a particular symbol exists.
    #[test]
    fn a_bundle_function_pointer_is_the_runtime_s_own_symbol() {
        bootstrap();
        /// The one name this test's provider answers, and the address it answers
        /// with.
        const FOUND: &str = "CFStringGetFunctionPointerForNameFixture";
        extern "C" fn entry() -> u32 {
            0xC0FF_EE00
        }
        fn provider(name: &str) -> Option<u64> {
            (name == FOUND).then_some(entry as *const () as usize as u64)
        }
        darwin::register_provider(provider);

        // SAFETY: strings and calls this test makes.
        unsafe {
            let known = string::from_bytes(FOUND.as_bytes(), false);
            let found = api::CFBundleGetFunctionPointerForName(std::ptr::null(), known);
            assert_eq!(found as *const (), entry as *const ());
            // It is an address that can be called as what it names, which is what
            // a guest holding this pointer does with it.
            let call: extern "C" fn() -> u32 = std::mem::transmute(found);
            assert_eq!(call(), 0xC0FF_EE00);

            // Nothing defines this one, which is what Apple answers for a symbol a
            // bundle does not export.
            let unknown = string::from_bytes(b"NoSuchFrameworkFunction12345", false);
            assert!(api::CFBundleGetFunctionPointerForName(std::ptr::null(), unknown).is_null());
            // And a name that is not a string is not a function name.
            assert!(
                api::CFBundleGetFunctionPointerForName(std::ptr::null(), std::ptr::null())
                    .is_null()
            );

            api::CFRelease(unknown);
            api::CFRelease(known);
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
