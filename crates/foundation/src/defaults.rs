use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use corefoundation::base::CFTypeRef;
use corefoundation::plist::Value;
use corefoundation::preferences::{self, GLOBAL};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

// `NSUserDefaults`, over the per-domain plists in
// `corefoundation::preferences`.
//
// An application's standard defaults are its bundle identifier's domain, a
// suite is the domain of that name, and every lookup falls through the
// argument domain, the application's domain, the added suites, `NSGlobalDomain`
// and finally what was registered - the order Apple documents.

/// Consume key/value pairs after argv[0]. Later occurrences override earlier ones.
fn parse_arguments(arguments: &[std::ffi::OsString]) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    let mut arguments = arguments.iter().skip(1);
    while let Some(argument) = arguments.next() {
        let argument = argument.to_string_lossy();
        let Some(key) = argument.strip_prefix('-').filter(|key| !key.is_empty()) else {
            continue;
        };
        let Some(value) = arguments.next() else {
            break;
        };
        values.insert(key.to_owned(), value.to_string_lossy().into_owned());
    }
    values
}

fn argument_values() -> BTreeMap<String, String> {
    parse_arguments(&crate::processinfo::guest_arguments())
}

/// What the running program files its preferences under: its bundle's
/// identifier, or the executable's name for a program that is not in a bundle.
fn application_identifier() -> Option<String> {
    if let Some(identifier) = crate::bundle::main().and_then(|layout| layout.identifier()) {
        return Some(identifier.to_owned());
    }
    let arguments = crate::processinfo::guest_arguments();
    let executable = std::path::Path::new(arguments.first()?).file_name()?;
    Some(executable.to_string_lossy().into_owned())
}

/// The suites `-addSuiteNamed:` put in each defaults object's search list,
/// searched after its own domain, in the order they were added.
static ADDED_SUITES: Mutex<BTreeMap<usize, Vec<String>>> = Mutex::new(BTreeMap::new());

/// What each object's `-registerDefaults:` supplied. Volatile, and per object,
/// as Apple's registration domain is: it is never written to a plist.
static REGISTERED: Mutex<BTreeMap<usize, BTreeMap<String, Value>>> = Mutex::new(BTreeMap::new());

/// `-setVolatileDomain:forName:`'s domains, other than the argument domain.
static VOLATILE: Mutex<BTreeMap<String, BTreeMap<String, Value>>> = Mutex::new(BTreeMap::new());

fn locked<T>(mutex: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The key space `receiver` writes to and reads first.
///
/// # Safety
///
/// `receiver` must be null or an `NSUserDefaults` this module made.
unsafe fn domain(receiver: Id) -> String {
    let name = match receiver.is_null() {
        true => None,
        // SAFETY: a leaked `String` this module made, or null.
        false => unsafe { (*(receiver as *const Defaults)).suite.as_ref() }.cloned(),
    };
    let name = match name {
        Some(name) => preferences::canonical(&name),
        None => preferences::current_application(),
    };
    migrate(&name);
    name
}

/// The old flat store, `key\tvalue` per line. Suites were kept apart in it by a
/// `"<suite>\x01"` prefix; an unprefixed key belonged to every application.
fn legacy_path() -> PathBuf {
    crate::host_home_directory().join("Library/Preferences/rosemary-defaults.tsv")
}

/// Carry what the flat store held into `domain`'s own plist, once.
///
/// Only when the domain has no plist yet, so a later launch never resurrects a
/// key the program has since removed. The flat file is left alone: other
/// applications may not have been launched since it was written, and it is
/// theirs to import.
fn migrate(domain: &str) {
    static DONE: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    if domain == GLOBAL
        || !locked(&DONE)
            .get_or_insert_with(Default::default)
            .insert(domain.to_owned())
        || preferences::exists(domain)
    {
        return;
    }
    let Ok(text) = std::fs::read_to_string(legacy_path()) else {
        return;
    };
    let standard = domain == preferences::current_application();
    let prefix = format!("{domain}\u{1}");
    let imported: BTreeMap<String, Value> = text
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter_map(|(key, value)| {
            let key = match key.strip_prefix(&prefix) {
                Some(key) => key,
                None if standard && !key.contains('\u{1}') => key,
                None => return None,
            };
            (key != corefoundation::preferences::INTERFACE_STYLE)
                .then(|| (key.to_owned(), Value::String(value.to_owned())))
        })
        .collect();
    if !imported.is_empty() {
        preferences::replace(domain, imported);
    }
}

/// The value `key` has for `receiver`, searching in Apple's order.
///
/// # Safety
///
/// `receiver` is an `NSUserDefaults` this module made.
unsafe fn lookup(receiver: Id, key: &str) -> Option<Value> {
    if key == corefoundation::preferences::INTERFACE_STYLE {
        return corefoundation::preferences::interface_style().map(|s| Value::String(s.to_owned()));
    }
    if let Some(argument) = argument_values().remove(key) {
        return Some(Value::String(argument));
    }
    // SAFETY: the caller's guarantee.
    let own = unsafe { domain(receiver) };
    if let Some(value) = preferences::value(&own, key) {
        return Some(value);
    }
    let suites = locked(&ADDED_SUITES).get(&(receiver as usize)).cloned();
    for suite in suites.into_iter().flatten() {
        let suite = preferences::canonical(&suite);
        migrate(&suite);
        if let Some(value) = preferences::value(&suite, key) {
            return Some(value);
        }
    }
    if own != GLOBAL
        && let Some(value) = preferences::value(GLOBAL, key)
    {
        return Some(value);
    }
    if let Some(value) = locked(&VOLATILE).values().find_map(|d| d.get(key)) {
        return Some(value.clone());
    }
    locked(&REGISTERED)
        .get(&(receiver as usize))
        .and_then(|d| d.get(key))
        .cloned()
}

unsafe extern "C" fn add_suite_named(receiver: Id, _sel: Sel, name: Id) {
    // SAFETY: null or an `NSString`.
    let Some(name) = (unsafe { key_text(name) }) else {
        return;
    };
    let mut suites = locked(&ADDED_SUITES);
    let list = suites.entry(receiver as usize).or_default();
    if !list.contains(&name) {
        list.push(name);
    }
}

unsafe extern "C" fn remove_suite_named(receiver: Id, _sel: Sel, name: Id) {
    // SAFETY: null or an `NSString`.
    let Some(name) = (unsafe { key_text(name) }) else {
        return;
    };
    if let Some(list) = locked(&ADDED_SUITES).get_mut(&(receiver as usize)) {
        list.retain(|suite| *suite != name);
    }
}

/// An `NSUserDefaults`. The suite name is a leaked `String` rather than an
/// inline one, because the object is created by the runtime's allocator and its
/// size has to be a constant.
#[repr(C)]
struct Defaults {
    isa: *const std::ffi::c_void,
    suite: *mut String,
}

/// `-initWithSuiteName:`, which opens another domain through the same store.
///
/// A nil name is `+standardUserDefaults`'s own space, which is Apple's
/// behaviour and what a caller passing a nil group identifier means.
unsafe extern "C" fn init_with_suite_name(receiver: Id, _sel: Sel, name: Id) -> Id {
    if receiver.is_null() {
        return receiver;
    }
    // SAFETY: null or an `NSString`.
    let suite = unsafe { key_text(name) };
    // SAFETY: a fresh instance of this module's own class.
    unsafe {
        (*(receiver as *mut Defaults)).suite = match suite {
            Some(name) => Box::into_raw(Box::new(name)),
            None => std::ptr::null_mut(),
        };
    }
    receiver
}

/// The one defaults object, retained forever - the reason `NSLocale`'s
/// singleton is.
static SHARED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

unsafe extern "C" fn standard_defaults(class: Id, _sel: Sel) -> Id {
    *SHARED.get_or_init(|| {
        // SAFETY: `class` is `NSUserDefaults`, which this module registered.
        let defaults = unsafe {
            objc::object::create_instance(
                objc::class::from_ptr(class as *const objc::class::Class).expect("a class"),
                0,
            )
        };
        objc::arc::retain(defaults);
        defaults as usize
    }) as Id
}

/// # Safety
///
/// `key` must be null or an `NSString`.
unsafe fn key_text(key: Id) -> Option<String> {
    if key.is_null() {
        return None;
    }
    let bytes = unsafe { crate::string::bytes(key) };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (!text.is_empty()).then_some(text)
}

/// `value` as an autoreleased object, or nil.
fn autoreleased(value: Option<Value>) -> Id {
    let Some(object) = value.and_then(|v| corefoundation::propertylist::object_from_value(&v))
    else {
        return std::ptr::null_mut();
    };
    // SAFETY: a fresh object this call owns.
    unsafe { corefoundation::api::CFAutorelease(object) as Id }
}

/// What `value` says as text, for the getters that coerce.
fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Integer(v) => Some(v.to_string()),
        Value::Real(v) => Some(v.to_string()),
        Value::Bool(v) => Some(u8::from(*v).to_string()),
        _ => None,
    }
}

/// `value` as a number: a boolean is one or zero, and `"YES"` and `"true"`
/// count as one, because a string-typed value written by an older build must
/// still read the way `-boolForKey:` would have read it.
fn number_of(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(v) => Some(f64::from(u8::from(*v))),
        Value::Integer(v) => Some(*v as f64),
        Value::Real(v) => Some(*v),
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "yes" | "true" => Some(1.0),
            "no" | "false" => Some(0.0),
            other => other.parse().ok(),
        },
        _ => None,
    }
}

/// `-objectForKey:`.
unsafe extern "C" fn object_for_key(receiver: Id, _sel: Sel, key: Id) -> Id {
    // SAFETY: an `NSUserDefaults` this module made, and null or an `NSString`.
    let Some(key) = (unsafe { key_text(key) }) else {
        return std::ptr::null_mut();
    };
    autoreleased(unsafe { lookup(receiver, &key) })
}

/// `-stringForKey:`, which coerces a number to its text as Apple's does.
unsafe extern "C" fn string_for_key(receiver: Id, _sel: Sel, key: Id) -> Id {
    // SAFETY: as [`object_for_key`].
    let Some(key) = (unsafe { key_text(key) }) else {
        return std::ptr::null_mut();
    };
    let text = unsafe { lookup(receiver, &key) }.as_ref().and_then(text_of);
    autoreleased(text.map(Value::String))
}

/// `-arrayForKey:`, which is nil for a value of any other type.
unsafe extern "C" fn array_for_key(receiver: Id, _sel: Sel, key: Id) -> Id {
    typed(receiver, key, |v| matches!(v, Value::Array(_)))
}

unsafe extern "C" fn dictionary_for_key(receiver: Id, _sel: Sel, key: Id) -> Id {
    typed(receiver, key, |v| matches!(v, Value::Dictionary(_)))
}

unsafe extern "C" fn data_for_key(receiver: Id, _sel: Sel, key: Id) -> Id {
    typed(receiver, key, |v| matches!(v, Value::Data(_)))
}

unsafe extern "C" fn string_array_for_key(receiver: Id, _sel: Sel, key: Id) -> Id {
    typed(receiver, key, |v| match v {
        Value::Array(items) => items.iter().all(|i| matches!(i, Value::String(_))),
        _ => false,
    })
}

fn typed(receiver: Id, key: Id, accept: impl Fn(&Value) -> bool) -> Id {
    // SAFETY: as [`object_for_key`].
    let Some(key) = (unsafe { key_text(key) }) else {
        return std::ptr::null_mut();
    };
    autoreleased(unsafe { lookup(receiver, &key) }.filter(accept))
}

fn number_for(receiver: Id, key: Id) -> f64 {
    // SAFETY: as [`object_for_key`].
    let Some(key) = (unsafe { key_text(key) }) else {
        return 0.0;
    };
    unsafe { lookup(receiver, &key) }
        .as_ref()
        .and_then(number_of)
        .unwrap_or(0.0)
}

unsafe extern "C" fn bool_for_key(receiver: Id, _sel: Sel, key: Id) -> u8 {
    u8::from(number_for(receiver, key) != 0.0)
}

unsafe extern "C" fn integer_for_key(receiver: Id, _sel: Sel, key: Id) -> i64 {
    // SAFETY: as [`object_for_key`].
    let Some(key) = (unsafe { key_text(key) }) else {
        return 0;
    };
    // An integer is read as one, so a value past 2^53 is not rounded.
    match unsafe { lookup(receiver, &key) } {
        Some(Value::Integer(v)) => v,
        Some(Value::String(text)) if text.trim().parse::<i64>().is_ok() => {
            text.trim().parse().unwrap_or(0)
        }
        other => other.as_ref().and_then(number_of).unwrap_or(0.0) as i64,
    }
}

unsafe extern "C" fn double_for_key(receiver: Id, _sel: Sel, key: Id) -> f64 {
    number_for(receiver, key)
}

unsafe extern "C" fn float_for_key(receiver: Id, _sel: Sel, key: Id) -> f32 {
    number_for(receiver, key) as f32
}

/// Write `value` to the receiver's own domain, or remove the key for `None`.
fn store(receiver: Id, key: Id, value: Option<Value>) {
    // SAFETY: as [`object_for_key`].
    let Some(key) = (unsafe { key_text(key) }) else {
        return;
    };
    let domain = unsafe { domain(receiver) };
    // Observers of the receiver, and of the standard defaults' mirror when the
    // write is to the application's own domain, hear of the change.
    let mut announced = vec![receiver];
    let mirror = MIRROR.load(std::sync::atomic::Ordering::Acquire);
    if mirror != 0 && domain == preferences::current_application() {
        announced.push(mirror as Id);
    }
    for object in &announced {
        crate::kvo::will_change(*object, key.as_bytes());
    }
    preferences::set_value(&domain, &key, value);
    for object in announced.iter().rev() {
        crate::kvo::did_change(*object, key.as_bytes());
    }
}

/// An object that is told of every write to the standard defaults as a change
/// of the key written - `NSUserDefaultsController`'s `values`, which AppKit makes.
static MIRROR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Make `object` hear the standard defaults' writes, as [`MIRROR`] describes.
pub fn mirror_changes_to(object: Id) {
    MIRROR.store(object as usize, std::sync::atomic::Ordering::Release);
}

unsafe extern "C" fn set_object_for_key(receiver: Id, _sel: Sel, value: Id, key: Id) {
    // Apple's `-setObject:nil forKey:` is a removal, not a store of nil.
    if value.is_null() {
        return store(receiver, key, None);
    }
    // SAFETY: whatever the caller stored.
    match unsafe { corefoundation::propertylist::value_of(value as CFTypeRef) } {
        Some(value) => store(receiver, key, Some(value)),
        None => {
            // SAFETY: null or an `NSString`.
            let name = unsafe { key_text(key) }.unwrap_or_default();
            eprintln!(
                "NSUserDefaults: {name:?} was not set: the value is not a property list type"
            );
        }
    }
}

unsafe extern "C" fn remove_object_for_key(receiver: Id, _sel: Sel, key: Id) {
    store(receiver, key, None);
}

unsafe extern "C" fn set_bool_for_key(receiver: Id, _sel: Sel, value: u8, key: Id) {
    store(receiver, key, Some(Value::Bool(value != 0)));
}

unsafe extern "C" fn set_integer_for_key(receiver: Id, _sel: Sel, value: i64, key: Id) {
    store(receiver, key, Some(Value::Integer(value)));
}

unsafe extern "C" fn set_double_for_key(receiver: Id, _sel: Sel, value: f64, key: Id) {
    store(receiver, key, Some(Value::Real(value)));
}

unsafe extern "C" fn set_float_for_key(receiver: Id, _sel: Sel, value: f32, key: Id) {
    store(receiver, key, Some(Value::Real(f64::from(value))));
}

/// `-synchronize`: every write is already in its file, so this picks up what
/// another process wrote. `YES`, because that is what a caller checks.
unsafe extern "C" fn synchronize(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: an `NSUserDefaults` this module made.
    let domain = unsafe { domain(receiver) };
    corefoundation::preferences::contents(&domain);
    1
}

/// `-registerDefaults:`, which supplies values for keys nothing has set.
///
/// Registered defaults are **not** persisted: Apple's live in a separate
/// volatile domain and are re-registered on every launch, so writing them to
/// the store would turn a default into a stored value the program can no longer
/// change by changing its own registration.
unsafe extern "C" fn register_defaults(receiver: Id, _sel: Sel, registration: Id) {
    // SAFETY: null or an `NSDictionary` this tree made.
    let Some(Value::Dictionary(entries)) =
        (unsafe { corefoundation::propertylist::value_of(registration as CFTypeRef) })
    else {
        return;
    };
    locked(&REGISTERED)
        .entry(receiver as usize)
        .or_default()
        .extend(entries);
}

/// An empty dictionary, autoreleased.
fn empty_dictionary() -> Id {
    autoreleased(Some(Value::Dictionary(BTreeMap::new())))
}

fn dictionary_of(values: BTreeMap<String, Value>) -> Id {
    autoreleased(Some(Value::Dictionary(values)))
}

/// The argument domain is volatile and never enters a plist.
unsafe extern "C" fn volatile_domain_for_name(_receiver: Id, _sel: Sel, name: Id) -> Id {
    // SAFETY: the domain name is null or an NSString.
    let Some(name) = (unsafe { key_text(name) }) else {
        return empty_dictionary();
    };
    if name == "NSArgumentDomain" {
        let values = argument_values()
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        return dictionary_of(values);
    }
    match locked(&VOLATILE).get(&name) {
        Some(values) => dictionary_of(values.clone()),
        None => empty_dictionary(),
    }
}

/// The domain as it is on disk, with the system's own preference for the
/// global one.
unsafe extern "C" fn domain_for_name(_receiver: Id, _sel: Sel, name: Id) -> Id {
    // SAFETY: the domain name is null or an NSString.
    let Some(name) = (unsafe { key_text(name) }) else {
        return empty_dictionary();
    };
    let domain = preferences::canonical(&name);
    migrate(&domain);
    let mut values = preferences::contents(&domain);
    if domain == GLOBAL {
        add_interface_style(&mut values);
    }
    dictionary_of(values)
}

/// `-dictionaryRepresentation`: what a lookup could find, with the nearer
/// domain winning.
unsafe extern "C" fn dictionary_representation(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSUserDefaults` this module made.
    let own = unsafe { domain(receiver) };
    let mut merged = BTreeMap::new();
    if let Some(registered) = locked(&REGISTERED).get(&(receiver as usize)) {
        merged.extend(registered.clone());
    }
    for values in locked(&VOLATILE).values() {
        merged.extend(values.clone());
    }
    merged.extend(preferences::contents(GLOBAL));
    let suites = locked(&ADDED_SUITES).get(&(receiver as usize)).cloned();
    for suite in suites.into_iter().flatten().rev() {
        let suite = preferences::canonical(&suite);
        migrate(&suite);
        merged.extend(preferences::contents(&suite));
    }
    if own != GLOBAL {
        merged.extend(preferences::contents(&own));
    }
    for (key, value) in argument_values() {
        merged.insert(key, Value::String(value));
    }
    add_interface_style(&mut merged);
    dictionary_of(merged)
}

fn add_interface_style(values: &mut BTreeMap<String, Value>) {
    values.remove(corefoundation::preferences::INTERFACE_STYLE);
    if let Some(style) = corefoundation::preferences::interface_style() {
        values.insert(
            corefoundation::preferences::INTERFACE_STYLE.to_owned(),
            Value::String(style.to_owned()),
        );
    }
}

/// `-setPersistentDomain:forName:`, which makes the domain's plist exactly that
/// dictionary.
unsafe extern "C" fn set_persistent_domain(_receiver: Id, _sel: Sel, domain: Id, name: Id) {
    // SAFETY: null or an `NSDictionary` and an `NSString`.
    let (Some(name), Some(Value::Dictionary(values))) = (unsafe { key_text(name) }, unsafe {
        corefoundation::propertylist::value_of(domain as CFTypeRef)
    }) else {
        return;
    };
    let domain = preferences::canonical(&name);
    migrate(&domain);
    preferences::replace(&domain, values);
}

unsafe extern "C" fn remove_persistent_domain(_receiver: Id, _sel: Sel, name: Id) {
    // SAFETY: null or an `NSString`.
    let Some(name) = (unsafe { key_text(name) }) else {
        return;
    };
    let domain = preferences::canonical(&name);
    // Migrated first so the flat store cannot bring it back on a later read.
    migrate(&domain);
    preferences::replace(&domain, BTreeMap::new());
}

unsafe extern "C" fn set_volatile_domain(_receiver: Id, _sel: Sel, domain: Id, name: Id) {
    // SAFETY: null or an `NSDictionary` and an `NSString`.
    let (Some(name), Some(Value::Dictionary(values))) = (unsafe { key_text(name) }, unsafe {
        corefoundation::propertylist::value_of(domain as CFTypeRef)
    }) else {
        return;
    };
    // The argument domain is the process's own argv and is not replaceable.
    if name != "NSArgumentDomain" {
        locked(&VOLATILE).insert(name, values);
    }
}

unsafe extern "C" fn remove_volatile_domain(_receiver: Id, _sel: Sel, name: Id) {
    // SAFETY: null or an `NSString`.
    if let Some(name) = unsafe { key_text(name) } {
        locked(&VOLATILE).remove(&name);
    }
}

/// `-persistentDomainNames`: the plists under the preferences directory.
unsafe extern "C" fn persistent_domain_names(_receiver: Id, _sel: Sel) -> Id {
    let directory = preferences::path_of("x").parent().map(PathBuf::from);
    let mut names = Vec::new();
    for entry in std::fs::read_dir(directory.unwrap_or_default())
        .into_iter()
        .flatten()
        .flatten()
    {
        let file = entry.file_name().to_string_lossy().into_owned();
        if let Some(name) = file.strip_suffix(".plist") {
            names.push(Value::String(match name {
                GLOBAL => "NSGlobalDomain".to_owned(),
                other => other.to_owned(),
            }));
        }
    }
    autoreleased(Some(Value::Array(names)))
}

pub(crate) fn install() {
    corefoundation::preferences::set_application_provider(application_identifier);
    let class =
        corefoundation::base::register(c"NSUserDefaults", None, size_of::<Defaults>(), false);
    class_methods!(class, {
        "standardUserDefaults" => standard_defaults, c"@16@0:8";
    });
    methods!(class, {
        "objectForKey:" => object_for_key, c"@24@0:8@16";
        "valueForKey:" => object_for_key, c"@24@0:8@16";
        "valueForKeyPath:" => object_for_key, c"@24@0:8@16";
        "stringForKey:" => string_for_key, c"@24@0:8@16";
        "stringArrayForKey:" => string_array_for_key, c"@24@0:8@16";
        "arrayForKey:" => array_for_key, c"@24@0:8@16";
        "dictionaryForKey:" => dictionary_for_key, c"@24@0:8@16";
        "dataForKey:" => data_for_key, c"@24@0:8@16";
        "setObject:forKey:" => set_object_for_key, c"v32@0:8@16@24";
        "setValue:forKey:" => set_object_for_key, c"v32@0:8@16@24";
        "removeObjectForKey:" => remove_object_for_key, c"v24@0:8@16";
        "boolForKey:" => bool_for_key, c"c24@0:8@16";
        "integerForKey:" => integer_for_key, c"q24@0:8@16";
        "doubleForKey:" => double_for_key, c"d24@0:8@16";
        "floatForKey:" => float_for_key, c"f24@0:8@16";
        "setBool:forKey:" => set_bool_for_key, c"v28@0:8c16@20";
        "setInteger:forKey:" => set_integer_for_key, c"v32@0:8q16@24";
        "setDouble:forKey:" => set_double_for_key, c"v32@0:8d16@24";
        "setFloat:forKey:" => set_float_for_key, c"v28@0:8f16@20";
        "synchronize" => synchronize, c"c16@0:8";
        "registerDefaults:" => register_defaults, c"v24@0:8@16";
        "initWithSuiteName:" => init_with_suite_name, c"@24@0:8@16";
        "addSuiteNamed:" => add_suite_named, c"v24@0:8@16";
        "removeSuiteNamed:" => remove_suite_named, c"v24@0:8@16";
        "volatileDomainForName:" => volatile_domain_for_name, c"@24@0:8@16";
        "persistentDomainForName:" => domain_for_name, c"@24@0:8@16";
        "persistentDomainNames" => persistent_domain_names, c"@16@0:8";
        "dictionaryRepresentation" => dictionary_representation, c"@16@0:8";
        "setVolatileDomain:forName:" => set_volatile_domain, c"v32@0:8@16@24";
        "setPersistentDomain:forName:" => set_persistent_domain, c"v32@0:8@16@24";
        "removePersistentDomainForName:" => remove_persistent_domain, c"v24@0:8@16";
        "removeVolatileDomainForName:" => remove_volatile_domain, c"v24@0:8@16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_boolean_reads_as_a_number() {
        assert_eq!(number_of(&Value::String("YES".into())), Some(1.0));
        assert_eq!(number_of(&Value::String("true".into())), Some(1.0));
        assert_eq!(number_of(&Value::String("NO".into())), Some(0.0));
        assert_eq!(number_of(&Value::String("42".into())), Some(42.0));
        assert_eq!(number_of(&Value::String("not a number".into())), None);
        assert_eq!(number_of(&Value::Bool(true)), Some(1.0));
    }

    #[test]
    fn a_number_reads_as_text() {
        assert_eq!(text_of(&Value::Integer(7)).as_deref(), Some("7"));
        assert_eq!(text_of(&Value::Bool(false)).as_deref(), Some("0"));
        assert_eq!(text_of(&Value::Array(Vec::new())), None);
    }

    #[test]
    fn arguments_after_the_executable_become_values() {
        let args: Vec<std::ffi::OsString> = ["/Guest", "-a", "1", "positional", "-a", "2"]
            .map(Into::into)
            .into();
        assert_eq!(
            parse_arguments(&args).get("a").map(String::as_str),
            Some("2")
        );
    }
}
