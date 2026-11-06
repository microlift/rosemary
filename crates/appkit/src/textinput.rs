#![expect(non_snake_case, reason = "the C spelling is the interface")]

use corefoundation::base::CFTypeRef;

/// The value of `kTISNotifySelectedKeyboardInputSourceChanged`.
///
/// A distributed-notification name, so its *text* is what matters: a guest that
/// observes it and a framework that posts it have to name one string.
pub const NOTIFY_SELECTED_INPUT_SOURCE_CHANGED: &str =
    "AppleSelectedInputSourcesChangedNotification";

/// `kTISNotifyEnabledKeyboardInputSourcesChanged`, whose value is its own name
/// on Darwin as well.
pub const NOTIFY_ENABLED_INPUT_SOURCES_CHANGED: &str =
    "AppleEnabledInputSourcesChangedNotification";

/// The property keys, whose values on Darwin are the constant names without the
/// `k` - a guest may pass its own `CFString` rather than the global, so they are
/// compared by text.
pub const PROPERTY_INPUT_SOURCE_CATEGORY: &str = "TISPropertyInputSourceCategory";
pub const PROPERTY_INPUT_SOURCE_TYPE: &str = "TISPropertyInputSourceType";
pub const PROPERTY_INPUT_SOURCE_IS_ASCII_CAPABLE: &str = "TISPropertyInputSourceIsASCIICapable";
pub const PROPERTY_INPUT_SOURCE_IS_ENABLE_CAPABLE: &str = "TISPropertyInputSourceIsEnableCapable";
pub const PROPERTY_INPUT_SOURCE_IS_SELECT_CAPABLE: &str = "TISPropertyInputSourceIsSelectCapable";
pub const PROPERTY_INPUT_SOURCE_IS_ENABLED: &str = "TISPropertyInputSourceIsEnabled";
pub const PROPERTY_INPUT_SOURCE_IS_SELECTED: &str = "TISPropertyInputSourceIsSelected";
pub const PROPERTY_INPUT_SOURCE_ID: &str = "TISPropertyInputSourceID";
pub const PROPERTY_BUNDLE_ID: &str = "TISPropertyBundleID";
pub const PROPERTY_LOCALIZED_NAME: &str = "TISPropertyLocalizedName";
pub const PROPERTY_INPUT_SOURCE_LANGUAGES: &str = "TISPropertyInputSourceLanguages";
pub const PROPERTY_UNICODE_KEY_LAYOUT_DATA: &str = "TISPropertyUnicodeKeyLayoutData";

/// The two `kTIS*` values a category and a type are compared against.
pub const CATEGORY_KEYBOARD_INPUT_SOURCE: &str = "TISCategoryKeyboardInputSource";
pub const TYPE_KEYBOARD_LAYOUT: &str = "TISTypeKeyboardLayout";

/// The identifier and name of the one source, which a guest displays and files
/// preferences under.
pub(crate) const SOURCE_ID: &str = "com.apple.keylayout.US";
const SOURCE_NAME: &str = "U.S.";
const SOURCE_BUNDLE_ID: &str = "com.apple.CarbonComponents";

/// The one input source this host reports.
///
/// A `TISInputSourceRef` is opaque: a caller receives one and passes it back to
/// [`TISGetInputSourceProperty`], never reading it. A string standing in for the
/// object is therefore indistinguishable from a real one to any correct caller,
/// and is a real CoreFoundation object, so retaining and releasing it behave.
fn current_source() -> CFTypeRef {
    foundation::notification::name_object(SOURCE_ID) as CFTypeRef
}

/// A shared object handed out by `Get` rather than `Copy`, built once.
///
/// Everything a property answers with outlives the call, because the caller does
/// not own it and this runtime is not told when it is done.
fn shared(
    slot: &'static std::sync::OnceLock<usize>,
    make: impl FnOnce() -> CFTypeRef,
) -> CFTypeRef {
    *slot.get_or_init(|| {
        let object = make();
        objc::arc::retain(object as objc::object::Id);
        object as usize
    }) as CFTypeRef
}

fn retained(source: CFTypeRef) -> CFTypeRef {
    objc::arc::retain(source as objc::object::Id);
    source
}

/// `TISInputSourceRef TISCopyCurrentKeyboardInputSource(void)`.
///
/// `Copy` in the name, so the caller owns a reference and will release it.
pub unsafe extern "C" fn TISCopyCurrentKeyboardInputSource() -> CFTypeRef {
    retained(current_source())
}

/// `TISInputSourceRef TISCopyInputMethodKeyboardLayoutOverride(void)`.
///
/// No input method is active, so there is no layout override and callers fall
/// back to the current keyboard input source.
pub unsafe extern "C" fn TISCopyInputMethodKeyboardLayoutOverride() -> CFTypeRef {
    std::ptr::null()
}

/// `TISCopyCurrentKeyboardLayoutInputSource`, and the two ASCII-capable
/// spellings beside it.
///
/// One source, and it is a US keyboard layout: the current input source, the
/// current layout and the ASCII-capable fallback cannot disagree here.
pub unsafe extern "C" fn TISCopyCurrentKeyboardLayoutInputSource() -> CFTypeRef {
    retained(current_source())
}

pub unsafe extern "C" fn TISCopyCurrentASCIICapableKeyboardInputSource() -> CFTypeRef {
    retained(current_source())
}

pub unsafe extern "C" fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> CFTypeRef {
    retained(current_source())
}

/// `TISInputSourceRef TISCopyInputSourceForLanguage(CFStringRef)`.
///
/// Null for a language this layout does not type, which is the documented answer
/// for one no installed source handles - and the honest one, since a guest that
/// asked for Japanese and got a US layout would type mojibake rather than
/// falling back.
pub unsafe extern "C" fn TISCopyInputSourceForLanguage(language: CFTypeRef) -> CFTypeRef {
    // SAFETY: null or a `CFStringRef`.
    let Some(language) = (unsafe { text_of(language) }) else {
        return std::ptr::null();
    };
    match language.split(['-', '_']).next() == Some("en") {
        true => retained(current_source()),
        false => std::ptr::null(),
    }
}

/// `CFArrayRef TISCreateInputSourceList(CFDictionaryRef properties, Boolean)`.
///
/// `Create`, so the caller owns the array. The filter is a dictionary of the
/// same property keys [`TISGetInputSourceProperty`] answers, and a source that
/// fails any of them is not in the list - an empty array rather than null, which
/// is what a caller enumerating one expects.
pub unsafe extern "C" fn TISCreateInputSourceList(
    properties: CFTypeRef,
    _include_all_installed: u8,
) -> CFTypeRef {
    // SAFETY: null or a `CFDictionaryRef`.
    let matches = unsafe { matches_filter(properties) };
    source_list(matches)
}

/// `CFArrayRef TISCreateASCIICapableInputSourceList(void)`.
pub unsafe extern "C" fn TISCreateASCIICapableInputSourceList() -> CFTypeRef {
    source_list(true)
}

fn source_list(include: bool) -> CFTypeRef {
    let values = [current_source()];
    let count = include as isize;
    // SAFETY: a live array of one object pointer, described by the count that
    // follows it, with this tree's own type callbacks.
    unsafe {
        corefoundation::api::CFArrayCreate(
            std::ptr::null(),
            values.as_ptr(),
            count,
            &corefoundation::callbacks::TYPE_VALUE_CALLBACKS,
        )
    }
}

/// Whether the one source satisfies every property in a filter dictionary.
///
/// # Safety
///
/// `properties` is null or a `CFDictionaryRef`.
unsafe fn matches_filter(properties: CFTypeRef) -> bool {
    if properties.is_null() {
        return true;
    }
    let keys = [
        PROPERTY_INPUT_SOURCE_CATEGORY,
        PROPERTY_INPUT_SOURCE_TYPE,
        PROPERTY_INPUT_SOURCE_IS_ASCII_CAPABLE,
        PROPERTY_INPUT_SOURCE_IS_ENABLED,
        PROPERTY_INPUT_SOURCE_IS_SELECTED,
        PROPERTY_INPUT_SOURCE_ID,
        PROPERTY_BUNDLE_ID,
        PROPERTY_LOCALIZED_NAME,
    ];
    keys.iter().all(|key| {
        let name = foundation::notification::name_object(key) as CFTypeRef;
        // SAFETY: the caller's obligation, and a key this module made.
        let wanted = unsafe { corefoundation::api::CFDictionaryGetValue(properties, name.cast()) };
        if wanted.is_null() {
            return true;
        }
        let ours = property(key);
        // SAFETY: two objects this runtime made, or null.
        !ours.is_null() && unsafe { corefoundation::api::CFEqual(ours, wanted) != 0 }
    })
}

/// `void *TISGetInputSourceProperty(TISInputSourceRef, CFStringRef key)`.
///
/// Not retained - `Get` rather than `Copy` - so everything returned here has to
/// outlive the call on its own.
pub unsafe extern "C" fn TISGetInputSourceProperty(
    _source: CFTypeRef,
    key: CFTypeRef,
) -> CFTypeRef {
    // SAFETY: null or a `CFStringRef`.
    match unsafe { text_of(key) } {
        Some(key) => property(&key),
        None => std::ptr::null(),
    }
}

/// The text of a `CFStringRef` argument.
///
/// # Safety
///
/// `value` is null or an object this runtime allocated.
unsafe fn text_of(value: CFTypeRef) -> Option<String> {
    unsafe { corefoundation::string::as_string(value) }
        .map(|s| String::from_utf8_lossy(&s.utf8()).into_owned())
}

/// The value of one property, by the key's text.
///
/// A property this host has no answer for is null, which is what Darwin returns
/// for one a source does not carry and the branch every caller already has.
fn property(key: &str) -> CFTypeRef {
    let string = |slot: &'static std::sync::OnceLock<usize>, text: &'static str| {
        shared(slot, || {
            foundation::notification::name_object(text) as CFTypeRef
        })
    };
    static CATEGORY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    static KIND: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    static ID: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    static BUNDLE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    static NAME: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    match key {
        PROPERTY_INPUT_SOURCE_CATEGORY => string(&CATEGORY, CATEGORY_KEYBOARD_INPUT_SOURCE),
        PROPERTY_INPUT_SOURCE_TYPE => string(&KIND, TYPE_KEYBOARD_LAYOUT),
        PROPERTY_INPUT_SOURCE_ID => string(&ID, SOURCE_ID),
        PROPERTY_BUNDLE_ID => string(&BUNDLE, SOURCE_BUNDLE_ID),
        PROPERTY_LOCALIZED_NAME => string(&NAME, SOURCE_NAME),
        PROPERTY_INPUT_SOURCE_IS_ASCII_CAPABLE
        | PROPERTY_INPUT_SOURCE_IS_ENABLE_CAPABLE
        | PROPERTY_INPUT_SOURCE_IS_SELECT_CAPABLE
        | PROPERTY_INPUT_SOURCE_IS_ENABLED
        | PROPERTY_INPUT_SOURCE_IS_SELECTED => corefoundation::collections::boolean(true),
        PROPERTY_INPUT_SOURCE_LANGUAGES => language_list(),
        PROPERTY_UNICODE_KEY_LAYOUT_DATA => layout_data(),
        _ => std::ptr::null(),
    }
}

/// `["en"]`, as `kTISPropertyInputSourceLanguages` is an array of language codes.
fn language_list() -> CFTypeRef {
    static LIST: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    shared(&LIST, || {
        let values = [foundation::notification::name_object("en") as CFTypeRef];
        // SAFETY: a live array of one object pointer, described by the count
        // that follows it, with this tree's own type callbacks.
        unsafe {
            corefoundation::api::CFArrayCreate(
                std::ptr::null(),
                values.as_ptr(),
                1,
                &corefoundation::callbacks::TYPE_VALUE_CALLBACKS,
            )
        }
    })
}

/// `kTISPropertyUnicodeKeyLayoutData`: the `'uchr'` resource as a `CFData`.
///
/// One object for the process, because the caller is handed it rather than a
/// copy and may hold the byte pointer for as long as it runs.
fn layout_data() -> CFTypeRef {
    static DATA: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    shared(&DATA, || {
        corefoundation::collections::new_data(crate::uchr::layout().to_vec())
    })
}

/// The four calls that turn a source on and off.
///
/// This host has one source and it is always enabled and selected, so each is
/// `noErr` for that source and `paramErr` for anything else - the difference
/// being that a guest asking to select a source that does not exist has made an
/// error, and one asking for the source it is already using has not.
pub unsafe extern "C" fn TISSelectInputSource(source: CFTypeRef) -> i32 {
    match source == current_source() {
        true => 0,
        false => crate::uchr::PARAM_ERR,
    }
}

/// Deselecting the only source would leave the keyboard typing nothing, which
/// this host cannot represent - and Darwin refuses it too for a source that is
/// not deselectable.
pub unsafe extern "C" fn TISDeselectInputSource(_source: CFTypeRef) -> i32 {
    crate::uchr::PARAM_ERR
}

pub unsafe extern "C" fn TISEnableInputSource(source: CFTypeRef) -> i32 {
    // SAFETY: takes the same argument and reads nothing through it.
    unsafe { TISSelectInputSource(source) }
}

pub unsafe extern "C" fn TISDisableInputSource(_source: CFTypeRef) -> i32 {
    crate::uchr::PARAM_ERR
}

/// `PhysicalKeyboardLayoutType KBGetLayoutType(SInt16 keyboardType)`.
///
/// `kKeyboardANSI`. The keyboard type names a hardware model, while this
/// function reduces it to the physical ANSI, ISO, or JIS arrangement. This
/// host exposes one ANSI keyboard through [`LMGetKbdType`], matching the key
/// positions in [`crate::keymap`] and the layout in [`crate::uchr`].
pub unsafe extern "C" fn KBGetLayoutType(_keyboard_type: i16) -> u32 {
    u32::from_be_bytes(*b"ANSI")
}

/// `UInt8 LMGetKbdLast(void)`.
///
/// `KbdLast` is the ADB address of the keyboard last used, and 2 is the address
/// an encoded device answers on - the one a keyboard has had since ADB. This
/// host reports one keyboard, so the last one used is that one, and the value
/// cannot change.
///
/// It is not [`LMGetKbdType`]: a caller that confuses the two is asking
/// [`UCKeyTranslate`] to translate for keyboard type 2, and the layout
/// [`crate::uchr`] builds answers for every type rather than one.
pub unsafe extern "C" fn LMGetKbdLast() -> u8 {
    2
}

/// `UInt8 LMGetKbdType(void)`.
///
/// `kKeyboardANSI`. The value only selects between the physical layouts that
/// differ in where a handful of keys sit, and ANSI is the one [`crate::keymap`]
/// describes. The layout [`crate::uchr`] builds answers for every keyboard type,
/// so a guest that passes a different one still translates.
pub unsafe extern "C" fn LMGetKbdType() -> u8 {
    40
}

/// `OSStatus UCKeyTranslate(const UCKeyboardLayout *, UInt16 code, UInt16 action,
/// UInt32 modifiers, UInt32 kbdType, OptionBits options, UInt32 *deadKeyState,
/// UniCharCount maxLength, UniCharCount *actualLength, UniChar *string)`.
///
/// The layout argument is parsed rather than assumed to be this host's: a caller
/// may pass a copy, or a resource of its own, and the characters have to be the
/// ones its bytes describe.
pub unsafe extern "C" fn UCKeyTranslate(
    layout: *const std::ffi::c_void,
    code: u16,
    action: u16,
    modifiers: u32,
    kbd_type: u32,
    options: u32,
    dead_key_state: *mut u32,
    max_length: usize,
    actual_length: *mut usize,
    string: *mut u16,
) -> i32 {
    let mut state = match dead_key_state.is_null() {
        // SAFETY: the caller states this is a writable `UInt32 *`.
        false => unsafe { *dead_key_state },
        true => 0,
    };
    // SAFETY: the layout it was handed.
    let chars = unsafe {
        crate::uchr::translate(
            layout.cast(),
            code,
            action,
            modifiers,
            kbd_type,
            options,
            &mut state,
        )
    };
    let chars = match chars {
        Ok(chars) => chars,
        Err(status) => {
            if !actual_length.is_null() {
                // SAFETY: the caller states this is a writable `UniCharCount *`.
                unsafe { *actual_length = 0 };
            }
            return status;
        }
    };
    if !dead_key_state.is_null() {
        unsafe { *dead_key_state = state };
    }
    if !actual_length.is_null() {
        // SAFETY: as above. Written before the buffer check, because a caller
        // that sized its buffer from a first call reads it after the failure.
        unsafe { *actual_length = chars.len() };
    }
    if chars.len() > max_length || (string.is_null() && !chars.is_empty()) {
        return crate::uchr::BUFFER_TOO_SMALL;
    }
    if !string.is_null() {
        // SAFETY: the caller states `string` holds `max_length` characters, and
        // no more than that are written.
        unsafe { std::ptr::copy_nonoverlapping(chars.as_ptr(), string, chars.len()) };
    }
    0
}

/// The `kTIS*` string globals, as `(symbol, the string it holds)`.
pub const STRING_GLOBALS: &[(&str, &str)] = &[
    (
        "_kTISNotifySelectedKeyboardInputSourceChanged",
        NOTIFY_SELECTED_INPUT_SOURCE_CHANGED,
    ),
    (
        "_kTISNotifyEnabledKeyboardInputSourcesChanged",
        NOTIFY_ENABLED_INPUT_SOURCES_CHANGED,
    ),
    (
        "_kTISPropertyInputSourceCategory",
        PROPERTY_INPUT_SOURCE_CATEGORY,
    ),
    ("_kTISPropertyInputSourceType", PROPERTY_INPUT_SOURCE_TYPE),
    (
        "_kTISPropertyInputSourceIsASCIICapable",
        PROPERTY_INPUT_SOURCE_IS_ASCII_CAPABLE,
    ),
    (
        "_kTISPropertyInputSourceIsEnableCapable",
        PROPERTY_INPUT_SOURCE_IS_ENABLE_CAPABLE,
    ),
    (
        "_kTISPropertyInputSourceIsSelectCapable",
        PROPERTY_INPUT_SOURCE_IS_SELECT_CAPABLE,
    ),
    (
        "_kTISPropertyInputSourceIsEnabled",
        PROPERTY_INPUT_SOURCE_IS_ENABLED,
    ),
    (
        "_kTISPropertyInputSourceIsSelected",
        PROPERTY_INPUT_SOURCE_IS_SELECTED,
    ),
    ("_kTISPropertyInputSourceID", PROPERTY_INPUT_SOURCE_ID),
    ("_kTISPropertyBundleID", PROPERTY_BUNDLE_ID),
    ("_kTISPropertyLocalizedName", PROPERTY_LOCALIZED_NAME),
    (
        "_kTISPropertyInputSourceLanguages",
        PROPERTY_INPUT_SOURCE_LANGUAGES,
    ),
    (
        "_kTISPropertyUnicodeKeyLayoutData",
        PROPERTY_UNICODE_KEY_LAYOUT_DATA,
    ),
    (
        "_kTISCategoryKeyboardInputSource",
        CATEGORY_KEYBOARD_INPUT_SOURCE,
    ),
    ("_kTISTypeKeyboardLayout", TYPE_KEYBOARD_LAYOUT),
    // No source here is of either type; both exist to be compared against.
    ("_kTISTypeKeyboardInputMode", "TISTypeKeyboardInputMode"),
    (
        "_kTISTypeKeyboardInputMethodModeEnabled",
        "TISTypeKeyboardInputMethodModeEnabled",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> CFTypeRef {
        foundation::string::from_str(text) as CFTypeRef
    }

    fn ask(property: &str) -> CFTypeRef {
        // SAFETY: a live source and a string this test made.
        unsafe { TISGetInputSourceProperty(current_source(), key(property)) }
    }

    /// The key is matched by text, so a caller that built its own `CFString`
    /// rather than reading the global gets the same answer.
    #[test]
    fn a_property_is_found_by_its_text_and_not_its_address() {
        crate::bootstrap();
        assert!(!ask(PROPERTY_INPUT_SOURCE_LANGUAGES).is_null());
        assert!(!ask(PROPERTY_INPUT_SOURCE_ID).is_null());
        assert!(
            ask("TISPropertyInputSourceIsNothing").is_null(),
            "a property this host does not carry is null"
        );
        // SAFETY: null is documented, and a non-string key cannot be read.
        assert!(unsafe { TISGetInputSourceProperty(current_source(), std::ptr::null()) }.is_null());
    }

    /// The layout is handed out, and what comes back translates - the round trip
    /// a guest makes, through the byte pointer rather than this tree's slice.
    #[test]
    fn the_layout_data_is_the_resource_and_it_translates() {
        crate::bootstrap();
        let data = ask(PROPERTY_UNICODE_KEY_LAYOUT_DATA);
        assert!(!data.is_null(), "the layout has to be answered");
        // SAFETY: a `CFData` this module made.
        let (bytes, length) = unsafe {
            (
                corefoundation::api::CFDataGetBytePtr(data),
                corefoundation::api::CFDataGetLength(data) as usize,
            )
        };
        assert_eq!(length, crate::uchr::layout().len());
        assert!(!bytes.is_null());

        let mut state = 0;
        let mut chars = [0u16; 4];
        let mut written = 0;
        // SAFETY: the layout just handed out, and live out-parameters.
        let status = unsafe {
            UCKeyTranslate(
                bytes.cast(),
                crate::keymap::vk::ANSI_LEFT_BRACKET,
                0,
                0,
                LMGetKbdType() as u32,
                0,
                &mut state,
                chars.len(),
                &mut written,
                chars.as_mut_ptr(),
            )
        };
        assert_eq!(status, 0);
        assert_eq!(String::from_utf16_lossy(&chars[..written]), "[");
    }

    /// The same object every time: the caller is handed the data rather than a
    /// copy, and may hold its byte pointer for as long as it runs.
    #[test]
    fn the_layout_data_is_one_object() {
        crate::bootstrap();
        assert_eq!(ask(PROPERTY_UNICODE_KEY_LAYOUT_DATA), layout_data());
    }

    /// The two keyboard globals are different globals: one is an ADB address
    /// and the other a keyboard type, and that type describes the ANSI layout
    /// implemented by this module.
    #[test]
    fn the_keyboard_globals_and_layout_agree() {
        // SAFETY: these functions take no pointers and touch no caller memory.
        unsafe {
            assert_eq!(LMGetKbdType(), 40, "kKeyboardANSI");
            assert_eq!(LMGetKbdLast(), 2, "the ADB address of a keyboard");
            assert_eq!(
                KBGetLayoutType(LMGetKbdType().into()),
                u32::from_be_bytes(*b"ANSI"),
                "the physical layout described by the exported keyboard type"
            );
        }
    }

    /// A buffer that cannot hold the translation is an error and a length, not
    /// a write past the end of it.
    #[test]
    fn a_short_buffer_is_refused_with_the_length_it_needed() {
        let mut state = 0;
        let mut written = 99;
        // SAFETY: live out-parameters and no output buffer.
        let status = unsafe {
            UCKeyTranslate(
                crate::uchr::layout().as_ptr().cast(),
                crate::keymap::vk::ANSI_A,
                0,
                0,
                40,
                0,
                &mut state,
                0,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, crate::uchr::BUFFER_TOO_SMALL);
        assert_eq!(written, 1, "the length it would have written");
    }

    /// A caller that ignored the layout it was handed must get an error rather
    /// than characters from whatever it passed instead.
    #[test]
    fn translating_without_a_layout_is_refused() {
        let mut length = 99usize;
        let mut dead = 7u32;
        // SAFETY: live out-parameters; a null layout is never read.
        let status = unsafe {
            UCKeyTranslate(
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                &mut dead,
                4,
                &mut length,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, crate::uchr::PARAM_ERR);
        assert_eq!(length, 0);
    }

    #[test]
    fn a_source_is_asked_for_by_language_this_layout_types() {
        crate::bootstrap();
        // SAFETY: strings this test made.
        unsafe {
            assert_eq!(TISCopyInputSourceForLanguage(key("en")), current_source());
            assert_eq!(
                TISCopyInputSourceForLanguage(key("en-US")),
                current_source()
            );
            assert!(TISCopyInputSourceForLanguage(key("ja")).is_null());
            assert!(TISCopyInputSourceForLanguage(std::ptr::null()).is_null());
        }
    }

    fn count(array: CFTypeRef) -> isize {
        // SAFETY: an array this module made.
        unsafe { corefoundation::api::CFArrayGetCount(array) }
    }

    #[test]
    fn the_source_list_holds_the_one_source_unless_the_filter_excludes_it() {
        crate::bootstrap();
        // SAFETY: no filter, so every source is listed.
        assert_eq!(
            count(unsafe { TISCreateInputSourceList(std::ptr::null(), 0) }),
            1
        );
        assert_eq!(count(unsafe { TISCreateASCIICapableInputSourceList() }), 1);

        let matching = dictionary(
            PROPERTY_INPUT_SOURCE_CATEGORY,
            CATEGORY_KEYBOARD_INPUT_SOURCE,
        );
        let other = dictionary(PROPERTY_INPUT_SOURCE_ID, "com.apple.inputmethod.Kotoeri");
        // SAFETY: dictionaries this test made.
        unsafe {
            assert_eq!(count(TISCreateInputSourceList(matching, 0)), 1);
            assert_eq!(
                count(TISCreateInputSourceList(other, 0)),
                0,
                "a filter naming another source lists none"
            );
        }
    }

    fn dictionary(property: &'static str, value: &str) -> CFTypeRef {
        let keys = [foundation::notification::name_object(property) as CFTypeRef];
        let values = [key(value)];
        // SAFETY: one key and one value, described by the count that follows.
        unsafe {
            corefoundation::api::CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr().cast(),
                values.as_ptr().cast(),
                1,
                &corefoundation::callbacks::TYPE_KEY_CALLBACKS,
                &corefoundation::callbacks::TYPE_VALUE_CALLBACKS,
            )
        }
    }

    /// Selecting the source in use is not an error; naming one that does not
    /// exist is.
    #[test]
    fn selecting_the_only_source_succeeds_and_deselecting_it_does_not() {
        crate::bootstrap();
        // SAFETY: a live source, and a string that is not one.
        unsafe {
            assert_eq!(TISSelectInputSource(current_source()), 0);
            assert_eq!(TISEnableInputSource(current_source()), 0);
            assert_eq!(
                TISSelectInputSource(key("com.apple.keylayout.German")),
                crate::uchr::PARAM_ERR
            );
            assert_eq!(
                TISDeselectInputSource(current_source()),
                crate::uchr::PARAM_ERR
            );
            assert_eq!(
                TISDisableInputSource(current_source()),
                crate::uchr::PARAM_ERR
            );
        }
    }
}
