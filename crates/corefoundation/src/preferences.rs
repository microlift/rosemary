// The C spellings are the interface, as they are in `api`.
#![allow(non_snake_case)]

use std::collections::HashMap;
use std::sync::Mutex;

use crate::base::CFTypeRef;

/// One domain's keys, each holding a retained object as an address.
///
/// Keyed by text rather than by object, because a caller builds the key string
/// afresh on each call and two equal strings are two objects. The value is an
/// address rather than a `CFTypeRef` so the table can be a `static`: a raw
/// pointer is not `Send`, and what is stored here is owned by this module for
/// the life of the process rather than shared with a thread.
type Domain = HashMap<String, usize>;

fn store() -> &'static Mutex<HashMap<String, Domain>> {
    static STORE: std::sync::OnceLock<Mutex<HashMap<String, Domain>>> = std::sync::OnceLock::new();
    STORE.get_or_init(Default::default)
}

/// The text of a `CFStringRef` argument, or `None` for one that is not a string.
///
/// # Safety
///
/// `value` is null or an object this runtime allocated.
unsafe fn text(value: CFTypeRef) -> Option<String> {
    unsafe { crate::string::as_string(value) }
        .map(|s| String::from_utf8_lossy(&s.utf8()).into_owned())
}

/// The domain name to file a key under.
///
/// A null application id is `kCFPreferencesAnyApplication` as far as this store
/// is concerned - one place for a caller that did not name a domain, rather
/// than a silent failure. It is spelled with that constant's *value* rather
/// than its name so that a caller passing the constant and a caller passing
/// null land in one domain instead of two.
///
/// # Safety
///
/// `application` is null or a `CFStringRef`.
unsafe fn domain_of(application: CFTypeRef) -> String {
    unsafe { text(application) }.unwrap_or_else(|| ANY.to_string())
}

/// The value Apple's CoreFoundation gives `kCFPreferencesAnyApplication`,
/// `kCFPreferencesAnyUser` and `kCFPreferencesAnyHost` - all three are this one
/// string, and a guest comparing any of them against another finds them equal.
const ANY: &str = "*";

/// The `CFStringRef` domain globals, as `(Mach-O name, value)`.
///
/// These are **variables** holding string objects, not functions, so an
/// unresolved one binds to zero and the guest's own initializer dereferences it
/// before `main` - see [`crate::data_symbols`]. Qt is the case in hand: its
/// `qsettings_mac.cpp` module initializer reads `kCFPreferencesCurrentHost` and
/// `kCFPreferencesAnyHost` into a static array, so a null slot is a segfault at
/// address zero during `run_initializers` with nothing to name the symbol.
///
/// The texts are Apple's own, not chosen here. They have to be, because a guest
/// may compare two of them - the three `Any` domains are one string on a Mac -
/// and because this store files keys by the text.
const DOMAIN_STRINGS: &[(&str, &str)] = &[
    ("_kCFPreferencesAnyApplication", ANY),
    ("_kCFPreferencesAnyUser", ANY),
    ("_kCFPreferencesAnyHost", ANY),
    (
        "_kCFPreferencesCurrentApplication",
        "kCFPreferencesCurrentApplication",
    ),
    ("_kCFPreferencesCurrentUser", "kCFPreferencesCurrentUser"),
    ("_kCFPreferencesCurrentHost", "kCFPreferencesCurrentHost"),
];

/// One interned string per distinct text, so that the three `Any` domains are
/// the same object and a guest comparing them by pointer agrees with one
/// comparing them with `CFEqual`.
fn interned(text: &str) -> CFTypeRef {
    static STRINGS: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());
    let mut strings = STRINGS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, object)) = strings.iter().find(|(t, _)| t == text) {
        return *object as CFTypeRef;
    }
    // Immortal: a global a guest may release is one it can make dangle.
    let object = crate::string::immortal(text);
    strings.push((text.to_string(), object as usize));
    object
}

/// The data symbols this module publishes, for [`crate::data_symbols`].
pub fn data_symbols() -> Vec<(String, u64)> {
    crate::bootstrap();
    DOMAIN_STRINGS
        .iter()
        .map(|(symbol, text)| {
            (
                (*symbol).to_string(),
                crate::base::global_cell(symbol, interned(text) as usize as u64),
            )
        })
        .collect()
}

/// `CFPropertyListRef CFPreferencesCopyAppValue(CFStringRef, CFStringRef)`
///
/// Copy, so the caller owns a reference - which is what the name says and what
/// its `CFRelease` expects.
///
/// # Safety
///
/// Both arguments are null or objects this runtime allocated.
pub unsafe extern "C" fn CFPreferencesCopyAppValue(
    key: CFTypeRef,
    application: CFTypeRef,
) -> CFTypeRef {
    let (Some(key), domain) = (unsafe { text(key) }, unsafe { domain_of(application) }) else {
        return std::ptr::null();
    };
    let store = store().lock().unwrap_or_else(|e| e.into_inner());
    let Some(value) = store.get(&domain).and_then(|d| d.get(&key)).copied() else {
        return std::ptr::null();
    };
    let value = value as CFTypeRef;
    // SAFETY: an object this module owns a reference to.
    unsafe { crate::api::CFRetain(value) }
}

/// `void CFPreferencesSetAppValue(CFStringRef, CFPropertyListRef, CFStringRef)`
///
/// A null value removes the key, which is how a program clears a setting.
///
/// # Safety
///
/// As [`CFPreferencesCopyAppValue`].
pub unsafe extern "C" fn CFPreferencesSetAppValue(
    key: CFTypeRef,
    value: CFTypeRef,
    application: CFTypeRef,
) {
    let (Some(key), domain) = (unsafe { text(key) }, unsafe { domain_of(application) }) else {
        return;
    };
    let mut store = store().lock().unwrap_or_else(|e| e.into_inner());
    let domain = store.entry(domain).or_default();
    if let Some(previous) = domain.remove(&key) {
        // SAFETY: a reference this module took when it stored the value.
        unsafe { crate::api::CFRelease(previous as CFTypeRef) };
    }
    if !value.is_null() {
        // SAFETY: the caller's object, which this store keeps a reference to
        // for as long as the key holds it.
        domain.insert(key, unsafe { crate::api::CFRetain(value) } as usize);
    }
}

/// `Boolean CFPreferencesAppValueIsForced(CFStringRef key, CFStringRef applicationID)`
///
/// "Forced" is the managed-preferences mechanism: a value installed by a
/// configuration profile the user is not allowed to change, so that reading the
/// *user's* value answers something the user did not choose.  It is a separate
/// store from the user's own, which is why the question is a question at all.
///
/// There is no managed store on this host - no configuration profiles, no
/// directory of them, nothing that has ever written one - and Apple's own
/// open-source `CFPreferences.c` does not contain this function either.  So the
/// answer is derived rather than hardcoded: [`MANAGED`] is a real prefix in this
/// store's domain names, no domain under it has ever been created, and the
/// question is whether `key` is in the one for `applicationID`.
///
/// The guest's call (`0x484451`, in a `bool*`-taking predicate) reads the
/// preference `MetricsReportingEnabled` and asks this only after
/// `CFPreferencesGetAppBooleanValue` found it, so on a machine with no
/// `MetricsReportingEnabled` preference the function is never reached at all.
pub unsafe extern "C" fn CFPreferencesAppValueIsForced(
    key: CFTypeRef,
    application: CFTypeRef,
) -> crate::api::Boolean {
    let (Some(key), Some(application)) = (unsafe { text(key) }, unsafe { text(application) })
    else {
        return 0;
    };
    // Per application, because that is how a profile scopes itself: a machine
    // managed for one application is not managed for another, and a store that
    // filed every forced value together could not say which.
    let domain = format!("{MANAGED}/{application}");
    let store = store().lock().unwrap_or_else(|e| e.into_inner());
    store
        .get(&domain)
        .is_some_and(|domain| domain.contains_key(&key)) as crate::api::Boolean
}

/// The prefix the managed, user-unalterable domains are filed under.
///
/// Named after what it is rather than after the mechanism, because the
/// mechanism - Mobile Directory Services, then MCX, then Configuration Profiles -
/// is three names for a store that does not exist here.
const MANAGED: &str = "com.rosetta3.managed";

/// `CFPropertyListRef CFPreferencesCopyValue(CFStringRef key, CFStringRef
/// applicationID, CFStringRef userName, CFStringRef hostName)`
///
/// The user and host arguments are ignored: this store is one process's, so
/// every domain in it is already "current user, current host".
///
/// # Safety
///
/// As [`CFPreferencesCopyAppValue`].
pub unsafe extern "C" fn CFPreferencesCopyValue(
    key: CFTypeRef,
    application: CFTypeRef,
    _user: CFTypeRef,
    _host: CFTypeRef,
) -> CFTypeRef {
    unsafe { CFPreferencesCopyAppValue(key, application) }
}

/// `void CFPreferencesSetValue(CFStringRef, CFPropertyListRef, CFStringRef,
/// CFStringRef, CFStringRef)`
///
/// # Safety
///
/// As [`CFPreferencesCopyAppValue`].
pub unsafe extern "C" fn CFPreferencesSetValue(
    key: CFTypeRef,
    value: CFTypeRef,
    application: CFTypeRef,
    _user: CFTypeRef,
    _host: CFTypeRef,
) {
    unsafe { CFPreferencesSetAppValue(key, value, application) }
}

/// `Boolean CFPreferencesGetAppBooleanValue(CFStringRef, CFStringRef,
/// Boolean *keyExistsAndHasValidFormat)`
///
/// The out-parameter is the whole interface: false with `*exists` false means
/// "not set", and a caller reads its own default in that case. Reporting false
/// with `*exists` **true** would tell it the user had turned the setting off.
///
/// # Safety
///
/// `exists` is null or a writable `Boolean`.
pub unsafe extern "C" fn CFPreferencesGetAppBooleanValue(
    key: CFTypeRef,
    application: CFTypeRef,
    exists: *mut u8,
) -> u8 {
    let value = unsafe { CFPreferencesCopyAppValue(key, application) };
    // SAFETY: a value this store holds, or null.
    let number = unsafe { crate::collections::number(value) }.map(|n| n.as_i64() != 0);
    if !value.is_null() {
        // SAFETY: the reference `CFPreferencesCopyAppValue` handed over.
        unsafe { crate::api::CFRelease(value) };
    }
    if !exists.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *exists = number.is_some() as u8 };
    }
    number.unwrap_or(false) as u8
}

/// `CFIndex CFPreferencesGetAppIntegerValue(CFStringRef, CFStringRef,
/// Boolean *)`
///
/// # Safety
///
/// As [`CFPreferencesGetAppBooleanValue`].
pub unsafe extern "C" fn CFPreferencesGetAppIntegerValue(
    key: CFTypeRef,
    application: CFTypeRef,
    exists: *mut u8,
) -> i64 {
    let value = unsafe { CFPreferencesCopyAppValue(key, application) };
    let number = unsafe { crate::collections::number(value) }.map(|n| n.as_i64());
    if !value.is_null() {
        // SAFETY: the reference just handed over.
        unsafe { crate::api::CFRelease(value) };
    }
    if !exists.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *exists = number.is_some() as u8 };
    }
    number.unwrap_or(0)
}

/// `CFArrayRef CFPreferencesCopyKeyList(CFStringRef applicationID, CFStringRef
/// userName, CFStringRef hostName)`
///
/// NULL rather than an empty array for a domain with nothing in it, because that
/// is the documented answer and what a caller tests for: Qt's `QSettings` reads
/// this to list a group's children and treats NULL as "no such domain", where an
/// empty array would have it report a domain that exists and is empty.
///
/// The user and host arguments are ignored, as in [`CFPreferencesCopyValue`].
///
/// # Safety
///
/// As [`CFPreferencesCopyAppValue`].
pub unsafe extern "C" fn CFPreferencesCopyKeyList(
    application: CFTypeRef,
    _user: CFTypeRef,
    _host: CFTypeRef,
) -> CFTypeRef {
    let domain = unsafe { domain_of(application) };
    let store = store().lock().unwrap_or_else(|e| e.into_inner());
    let keys: Vec<CFTypeRef> = match store.get(&domain) {
        Some(keys) if !keys.is_empty() => keys
            .keys()
            .map(|k| crate::string::from_bytes(k.as_bytes(), false))
            .collect(),
        // Absent *and* present-but-empty are both "nothing to list": a domain
        // left empty by removing its last key is not a domain a Mac would still
        // report keys for.
        _ => return std::ptr::null(),
    };
    let array = crate::collections::new_array(
        &keys,
        crate::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // The array took its own reference to each key, so the ones made above are
    // this function's to drop.
    for key in keys {
        // SAFETY: a string this function created and still holds.
        unsafe { crate::api::CFRelease(key) };
    }
    array
}

/// `void CFPreferencesSetMultiple(CFDictionaryRef keysToSet, CFArrayRef
/// keysToRemove, CFStringRef applicationID, CFStringRef userName, CFStringRef
/// hostName)`
///
/// Removals first, then the writes, which is the order that makes a caller
/// naming one key in both lists end up with the value rather than without it -
/// Qt's `QSettings` sync does exactly that when a key is rewritten.
///
/// # Safety
///
/// `keys_to_set` is null or a `CFDictionaryRef` and `keys_to_remove` is null or
/// a `CFArrayRef`; both are objects this runtime allocated.
pub unsafe extern "C" fn CFPreferencesSetMultiple(
    keys_to_set: CFTypeRef,
    keys_to_remove: CFTypeRef,
    application: CFTypeRef,
    _user: CFTypeRef,
    _host: CFTypeRef,
) {
    if let Some(remove) = unsafe { crate::collections::array(keys_to_remove) } {
        for key in remove.values.clone() {
            // SAFETY: a key from the caller's array and the caller's domain.
            unsafe { CFPreferencesSetAppValue(key, std::ptr::null(), application) };
        }
    }
    if let Some(set) = unsafe { crate::collections::dictionary(keys_to_set) } {
        for (key, value) in set.entries.clone() {
            // SAFETY: a pair from the caller's dictionary and the caller's domain.
            unsafe { CFPreferencesSetAppValue(key, value, application) };
        }
    }
}

/// `Boolean CFPreferencesAppSynchronize(CFStringRef)`
///
/// True, because there is nothing to flush: the store *is* the value every
/// later read sees. A false here is "the write did not land", which is not the
/// case and which callers treat as a failure of whatever they were configuring.
pub extern "C" fn CFPreferencesAppSynchronize(_application: CFTypeRef) -> u8 {
    1
}

/// `Boolean CFPreferencesSynchronize(CFStringRef, CFStringRef, CFStringRef)`
pub extern "C" fn CFPreferencesSynchronize(
    _application: CFTypeRef,
    _user: CFTypeRef,
    _host: CFTypeRef,
) -> u8 {
    1
}

exports::exports! {
    functions {
        "CFPreferencesCopyAppValue" => CFPreferencesCopyAppValue,
        "CFPreferencesSetAppValue" => CFPreferencesSetAppValue,
        "CFPreferencesCopyValue" => CFPreferencesCopyValue,
        "CFPreferencesSetValue" => CFPreferencesSetValue,
        "CFPreferencesGetAppBooleanValue" => CFPreferencesGetAppBooleanValue,
        "CFPreferencesGetAppIntegerValue" => CFPreferencesGetAppIntegerValue,
        "CFPreferencesCopyKeyList" => CFPreferencesCopyKeyList,
        "CFPreferencesSetMultiple" => CFPreferencesSetMultiple,
        "CFPreferencesAppSynchronize" => CFPreferencesAppSynchronize,
        "CFPreferencesSynchronize" => CFPreferencesSynchronize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(text: &str) -> CFTypeRef {
        crate::string::from_bytes(text.as_bytes(), false)
    }

    /// Qt's `QMacSettingsPrivate` releases `kCFPreferencesAnyHost` without
    /// having retained it, once per `QSettings`.
    #[test]
    fn a_domain_constant_survives_any_number_of_releases() {
        crate::bootstrap();
        let any = interned(ANY);
        for _ in 0..8 {
            // SAFETY: a string this module made.
            unsafe { crate::api::CFRelease(any) };
        }
        // SAFETY: as above, and still live.
        assert_eq!(unsafe { crate::api::CFStringGetLength(any) }, 1);
        assert_eq!(interned(ANY), any);
    }

    /// A key nothing set is NULL and "does not exist", which is what a program
    /// reads on a Mac it has never run on - and the case that has to work.
    #[test]
    fn an_unset_key_is_absent_rather_than_false() {
        let (key, app) = (string("R3NeverSet"), string("com.example.absent"));
        let mut exists = 1u8;
        // SAFETY: two strings this test made, and a live `Boolean`.
        unsafe {
            assert!(CFPreferencesCopyAppValue(key, app).is_null());
            assert_eq!(
                CFPreferencesGetAppBooleanValue(key, app, &raw mut exists),
                0
            );
            assert_eq!(
                exists, 0,
                "false because it is unset, not because it is off"
            );
        }
    }

    /// What was written is read back, in the domain it was written to and not
    /// in another.
    #[test]
    fn a_value_reads_back_in_its_own_domain() {
        let key = string("R3Enabled");
        let (mine, theirs) = (string("com.example.mine"), string("com.example.theirs"));
        let value = crate::collections::new_number(crate::collections::Number::Integer(1));
        let mut exists = 0u8;
        // SAFETY: strings and a number this test made.
        unsafe {
            CFPreferencesSetAppValue(key, value, mine);
            assert_eq!(
                CFPreferencesGetAppBooleanValue(key, mine, &raw mut exists),
                1
            );
            assert_eq!(exists, 1);
            assert!(CFPreferencesCopyAppValue(key, theirs).is_null());
            assert_eq!(
                CFPreferencesGetAppIntegerValue(key, mine, std::ptr::null_mut()),
                1
            );

            // And a null value removes it again.
            CFPreferencesSetAppValue(key, std::ptr::null(), mine);
            assert!(CFPreferencesCopyAppValue(key, mine).is_null());
            crate::api::CFRelease(value);
        }
    }
}
