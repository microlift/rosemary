// The C spellings are the interface, as they are in `api`.
#![allow(non_snake_case)]

use crate::base::CFTypeRef;

pub const INTERFACE_STYLE: &str = "AppleInterfaceStyle";

/// macOS omits this preference in light mode. Query SDL each time so a theme
/// change is visible without restarting the guest or persisting a host setting.
pub fn interface_style() -> Option<&'static str> {
    // SDL has no system theme until the video driver is initialized, even if
    // the application is asking before creating its first window.
    sdl3::video::init_video();
    (sdl3::video::system_theme() == sdl3::video::SystemTheme::Dark).then_some("Dark")
}

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime};

use crate::plist::Value;

/// Where one process's preferences live, and what they are: a plist per
/// domain, under `~/Library/Preferences`, which is what `defaults`, `plutil`
/// and a program reading its own file all expect.
///
/// Every domain is its own file, so two applications' `"enabled"` are two
/// keys. `NSGlobalDomain` is `.GlobalPreferences.plist` and is the one domain
/// every application is also searched against.
pub const GLOBAL: &str = ".GlobalPreferences";

/// How often a domain's file is looked at again for a change another process
/// made. There is no `cfprefsd` to push one.
const RECHECK: std::time::Duration = std::time::Duration::from_millis(250);

type ApplicationProvider = fn() -> Option<String>;

static APPLICATION: OnceLock<ApplicationProvider> = OnceLock::new();
static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();

/// How `kCFPreferencesCurrentApplication` is named: the main bundle's
/// identifier, which only Foundation can read. Set once; later calls are
/// ignored.
pub fn set_application_provider(provider: ApplicationProvider) {
    let _ = APPLICATION.set(provider);
}

/// Keep the plists in `directory` instead of `~/Library/Preferences`, for a
/// test or an embedder that must not touch the user's own.
pub fn set_directory(directory: PathBuf) {
    let _ = DIRECTORY.set(directory);
}

/// The identifier the running program files its preferences under.
pub fn current_application() -> String {
    APPLICATION
        .get()
        .and_then(|provider| provider())
        .unwrap_or_else(|| "rosemary".to_owned())
}

/// The stored name of `domain`: the three global spellings are one, and the
/// current-application constant is the running program.
pub fn canonical(domain: &str) -> String {
    match domain {
        ANY | "NSGlobalDomain" | GLOBAL | "kCFPreferencesAnyApplication" => GLOBAL.to_owned(),
        "kCFPreferencesCurrentApplication" => current_application(),
        other => other.to_owned(),
    }
}

/// The host file for a canonical domain. An absolute path names a plist
/// directly, as it does for Apple's `CFPreferences`.
pub fn path_of(domain: &str) -> PathBuf {
    if domain.starts_with('/') {
        let base = domain.strip_suffix(".plist").unwrap_or(domain);
        return prefix::ns::host_path(&format!("{base}.plist"));
    }
    // A name never climbs out of the preferences directory.
    let name = domain.replace('/', "_");
    match DIRECTORY.get() {
        Some(directory) => directory.join(format!("{name}.plist")),
        None => prefix::ns::host_path(&format!(
            "{}/Library/Preferences/{name}.plist",
            prefix::ns::home()
        )),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: SystemTime,
    length: u64,
}

fn stamp(path: &std::path::Path) -> Option<Stamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(Stamp {
        modified: metadata.modified().ok()?,
        length: metadata.len(),
    })
}

struct Loaded {
    values: BTreeMap<String, Value>,
    stamp: Option<Stamp>,
    checked: Option<Instant>,
}

fn read_file(path: &std::path::Path) -> BTreeMap<String, Value> {
    let Ok(bytes) = std::fs::read(path) else {
        return BTreeMap::new();
    };
    match crate::plist::parse(&bytes) {
        Ok(Value::Dictionary(values)) => values,
        // A file that is not a dictionary is not a preferences file; it is
        // left as it is until something writes the domain.
        _ => BTreeMap::new(),
    }
}

impl Loaded {
    fn refresh(&mut self, path: &std::path::Path, force: bool) {
        let due = self.checked.is_none_or(|at| at.elapsed() >= RECHECK);
        if !force && !due {
            return;
        }
        self.checked = Some(Instant::now());
        let now = stamp(path);
        if now != self.stamp || self.stamp.is_none() {
            self.values = read_file(path);
            self.stamp = now;
        }
    }
}

fn store() -> &'static std::sync::Mutex<HashMap<String, Loaded>> {
    static STORE: OnceLock<std::sync::Mutex<HashMap<String, Loaded>>> = OnceLock::new();
    STORE.get_or_init(Default::default)
}

fn with_loaded<R>(domain: &str, force: bool, f: impl FnOnce(&mut Loaded, &PathBuf) -> R) -> R {
    let domain = canonical(domain);
    let path = path_of(&domain);
    let mut store = store().lock().unwrap_or_else(|e| e.into_inner());
    let loaded = store.entry(domain).or_insert_with(|| Loaded {
        values: BTreeMap::new(),
        stamp: None,
        checked: None,
    });
    loaded.refresh(&path, force);
    f(loaded, &path)
}

/// Write the domain through a temporary file and a rename, so a reader never
/// sees half a plist and a crash leaves the old one.
fn persist(loaded: &mut Loaded, path: &std::path::Path) {
    if loaded.values.is_empty() && loaded.stamp.is_none() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let bytes = crate::plist::write_xml(&Value::Dictionary(loaded.values.clone()));
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    if std::fs::write(&temporary, bytes).is_ok() && std::fs::rename(&temporary, path).is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    loaded.stamp = stamp(path);
    loaded.checked = Some(Instant::now());
}

/// The value `key` holds in exactly `domain`.
pub fn value(domain: &str, key: &str) -> Option<Value> {
    with_loaded(domain, false, |loaded, _| loaded.values.get(key).cloned())
}

/// Store `value` under `key` in `domain`, or remove the key for `None`. The
/// file is up to date when this returns.
pub fn set_value(domain: &str, key: &str, value: Option<Value>) {
    with_loaded(domain, true, |loaded, path| {
        let changed = match value {
            Some(value) => loaded.values.insert(key.to_owned(), value.clone()) != Some(value),
            None => loaded.values.remove(key).is_some(),
        };
        if changed {
            persist(loaded, path);
        }
    });
}

/// Every key `domain` holds, with its value.
pub fn contents(domain: &str) -> BTreeMap<String, Value> {
    with_loaded(domain, false, |loaded, _| loaded.values.clone())
}

/// Make `domain` hold exactly `values`; an empty map removes the file.
pub fn replace(domain: &str, values: BTreeMap<String, Value>) {
    with_loaded(domain, true, |loaded, path| {
        if values.is_empty() {
            let _ = std::fs::remove_file(path);
            loaded.values.clear();
            loaded.stamp = None;
            return;
        }
        loaded.values = values;
        // A file that was not there is written, rather than skipped as empty.
        persist(loaded, path);
    });
}

/// Whether a plist for `domain` has been written, by this process or another.
pub fn exists(domain: &str) -> bool {
    path_of(&canonical(domain)).exists()
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
    canonical(&unsafe { text(application) }.unwrap_or_else(|| ANY.to_string()))
}

/// The retained object for `value`, or null.
fn object_of(value: Option<Value>) -> CFTypeRef {
    value
        .and_then(|v| crate::propertylist::object_from_value(&v))
        .unwrap_or(std::ptr::null())
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
/// its `CFRelease` expects. The application's own domain is searched first and
/// the global one after it, as Apple's is.
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
    if key == INTERFACE_STYLE {
        return interface_style().map_or(std::ptr::null(), |value| {
            crate::string::from_bytes(value.as_bytes(), false)
        });
    }
    let found = value(&domain, &key).or_else(|| match domain == GLOBAL {
        true => None,
        false => value(GLOBAL, &key),
    });
    object_of(found)
}

/// `void CFPreferencesSetAppValue(CFStringRef, CFPropertyListRef, CFStringRef)`
///
/// A null value removes the key, which is how a program clears a setting.
/// Something a property list cannot hold is refused by name and not stored.
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
    if value.is_null() {
        set_value(&domain, &key, None);
        return;
    }
    // SAFETY: the caller's object.
    match unsafe { crate::propertylist::value_of(value) } {
        Some(value) => set_value(&domain, &key, Some(value)),
        None => eprintln!(
            "CFPreferences: {key:?} in {domain:?} was not set: the value is not a property \
             list type"
        ),
    }
}

/// `CFPropertyListRef CFPreferencesCopyValue(CFStringRef key, CFStringRef
/// applicationID, CFStringRef userName, CFStringRef hostName)`
///
/// Exactly the named domain, with no fall back to the global one. The user and
/// host arguments are ignored: this store is one user's, on one host.
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
    let (Some(key), domain) = (unsafe { text(key) }, unsafe { domain_of(application) }) else {
        return std::ptr::null();
    };
    if key == INTERFACE_STYLE && domain == GLOBAL {
        return interface_style().map_or(std::ptr::null(), |value| {
            crate::string::from_bytes(value.as_bytes(), false)
        });
    }
    object_of(value(&domain, &key))
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
    let mut names: Vec<String> = contents(&domain)
        .into_keys()
        .filter(|k| k.as_str() != INTERFACE_STYLE)
        .collect();
    if domain == GLOBAL && interface_style().is_some() {
        names.push(INTERFACE_STYLE.to_owned());
    }
    if names.is_empty() {
        return std::ptr::null();
    }
    let keys: Vec<CFTypeRef> = names
        .iter()
        .map(|k| crate::string::from_bytes(k.as_bytes(), false))
        .collect();
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

/// `CFDictionaryRef CFPreferencesCopyMultiple(CFArrayRef, CFStringRef,
/// CFStringRef, CFStringRef)`. A null key list requests the entire domain.
///
/// # Safety
///
/// Arguments are null or objects of their declared types from this runtime.
pub unsafe extern "C" fn CFPreferencesCopyMultiple(
    keys: CFTypeRef,
    application: CFTypeRef,
    user: CFTypeRef,
    host: CFTypeRef,
) -> CFTypeRef {
    let owned_keys = if keys.is_null() {
        unsafe { CFPreferencesCopyKeyList(application, user, host) }
    } else {
        std::ptr::null()
    };
    let keys = if keys.is_null() { owned_keys } else { keys };
    let mut pairs = Vec::new();
    if let Some(keys) = unsafe { crate::collections::array(keys) } {
        for &key in &keys.values {
            let value = unsafe { CFPreferencesCopyValue(key, application, user, host) };
            if !value.is_null() {
                pairs.push((key, value));
            }
        }
    }
    let result = crate::collections::new_dictionary(
        &pairs,
        crate::callbacks::Policy::from_key_callbacks(),
        crate::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for (_, value) in pairs {
        unsafe { crate::api::CFRelease(value) };
    }
    if !owned_keys.is_null() {
        unsafe { crate::api::CFRelease(owned_keys) };
    }
    result
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
        for (key, value) in set.entries().to_vec() {
            // SAFETY: a pair from the caller's dictionary and the caller's domain.
            unsafe { CFPreferencesSetAppValue(key, value, application) };
        }
    }
}

/// `Boolean CFPreferencesAppSynchronize(CFStringRef)`
///
/// Every write is already on disk. What there is to do is pick up what another
/// process wrote since, so the next read is not up to a recheck interval old.
pub extern "C" fn CFPreferencesAppSynchronize(application: CFTypeRef) -> u8 {
    // SAFETY: null or a string object, per the declared signature.
    let domain = unsafe { domain_of(application) };
    with_loaded(&domain, true, |_, _| ());
    1
}

/// `Boolean CFPreferencesSynchronize(CFStringRef, CFStringRef, CFStringRef)`
pub extern "C" fn CFPreferencesSynchronize(
    application: CFTypeRef,
    _user: CFTypeRef,
    _host: CFTypeRef,
) -> u8 {
    CFPreferencesAppSynchronize(application)
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
        "CFPreferencesCopyMultiple" => CFPreferencesCopyMultiple,
        "CFPreferencesSetMultiple" => CFPreferencesSetMultiple,
        "CFPreferencesAppSynchronize" => CFPreferencesAppSynchronize,
        "CFPreferencesSynchronize" => CFPreferencesSynchronize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let directory =
                std::env::temp_dir().join(format!("rosemary-cfprefs-{}", std::process::id()));
            set_directory(directory);
        });
    }

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
        scratch();
        let (key, app) = (string("RosemaryNeverSet"), string("com.example.absent"));
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
        scratch();
        let key = string("RosemaryEnabled");
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

    /// The point of a plist per domain: what one application wrote is a file
    /// of its own, which a fresh read of the disk finds again, and which the
    /// global domain does not shadow.
    #[test]
    fn a_domain_is_a_plist_of_its_own() {
        scratch();
        let domain = "com.example.persisted";
        set_value(domain, "Name", Some(Value::String("rose".into())));
        set_value(domain, "Count", Some(Value::Integer(3)));
        set_value(GLOBAL, "Shared", Some(Value::Bool(true)));
        let file = path_of(domain);
        let on_disk =
            crate::plist::parse(&std::fs::read(&file).expect("a plist")).expect("readable");
        assert_eq!(on_disk.get("Name"), Some(&Value::String("rose".into())));
        assert_eq!(on_disk.get("Shared"), None);
        let app = string(domain);
        // SAFETY: a string this test made.
        unsafe {
            let shared = CFPreferencesCopyAppValue(string("Shared"), app);
            assert!(!shared.is_null(), "the global domain is searched after");
            let other =
                CFPreferencesCopyValue(string("Shared"), app, std::ptr::null(), std::ptr::null());
            assert!(other.is_null(), "CopyValue names one domain exactly");
        }
    }

    /// A second process writing the file is seen by this one.
    #[test]
    fn a_change_made_elsewhere_is_picked_up() {
        scratch();
        let domain = "com.example.elsewhere";
        set_value(domain, "Key", Some(Value::Integer(1)));
        let mut changed = BTreeMap::new();
        changed.insert("Key".to_owned(), Value::Integer(2));
        let bytes = crate::plist::write_xml(&Value::Dictionary(changed));
        std::fs::write(path_of(domain), bytes).unwrap();
        let app = string(domain);
        CFPreferencesAppSynchronize(app);
        assert_eq!(value(domain, "Key"), Some(Value::Integer(2)));
    }
}
