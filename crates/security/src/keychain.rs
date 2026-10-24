#![allow(non_snake_case)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use corefoundation::base::CFTypeRef;

/// `OSStatus`, in the two values these calls answer with.
type OSStatus = i32;
const SUCCESS: OSStatus = 0;
/// `errSecItemNotFound`.
const ITEM_NOT_FOUND: OSStatus = -25300;
/// `errSecDuplicateItem`.
const DUPLICATE_ITEM: OSStatus = -25299;
/// `errSecParam`.
const PARAM: OSStatus = -50;

/// The attribute names a query is built from, as their documented string
/// values.
///
/// The values matter and are not the symbol names: `kSecClass` is the string
/// `"class"`, and a caller that builds its query dictionary by hand - or reads
/// one back - uses the string. Published as data as well, because each is a
/// `CFStringRef` **variable**: unbound it binds to zero and the guest's
/// `CFDictionarySetValue` key is address zero.
pub const KEYS: &[(&str, &str)] = &[
    ("_kSecClass", "class"),
    ("_kSecClassGenericPassword", "genp"),
    ("_kSecClassInternetPassword", "inet"),
    ("_kSecAttrService", "svce"),
    ("_kSecAttrAccount", "acct"),
    ("_kSecAttrLabel", "labl"),
    ("_kSecAttrAccessible", "pdmn"),
    ("_kSecAttrAccessibleAfterFirstUnlock", "ck"),
    ("_kSecAttrAccessibleWhenUnlocked", "ak"),
    ("_kSecValueData", "v_Data"),
    ("_kSecReturnData", "r_Data"),
    ("_kSecReturnAttributes", "r_Attributes"),
    ("_kSecMatchLimit", "m_Limit"),
    ("_kSecMatchLimitOne", "m_LimitOne"),
    ("_kSecMatchLimitAll", "m_LimitAll"),
    ("_kSecReturnRef", "r_Ref"),
    ("_kSecAttrComment", "icmt"),
    ("_kSecAttrDescription", "desc"),
    ("_kSecKeyAlgorithmRSAEncryptionRaw", "algid:encrypt:RSA:raw"),
    ("_kSecTransformInputAttributeName", "INPUT"),
];

/// What identifies an item: its class, its service and its account.
///
/// Not the whole attribute set. Apple's primary key for a generic password is
/// exactly these, and an item that matched on more would be one a caller could
/// never find again after storing it with a label it did not repeat.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Identity {
    class: String,
    service: String,
    account: String,
}

static ITEMS: Mutex<BTreeMap<Identity, Vec<u8>>> = Mutex::new(BTreeMap::new());
static LOADED: std::sync::Once = std::sync::Once::new();

fn items() -> std::sync::MutexGuard<'static, BTreeMap<Identity, Vec<u8>>> {
    LOADED.call_once(|| {
        *ITEMS.lock().unwrap_or_else(|e| e.into_inner()) = load();
    });
    ITEMS.lock().unwrap_or_else(|e| e.into_inner())
}

fn store_path() -> PathBuf {
    foundation::host_home_directory().join("Library/Keychains/rosemary-items.tsv")
}

/// Tab-separated, with the value hex-encoded.
///
/// Hex rather than raw bytes because the value is a `CFData` and may hold
/// anything, including a tab, a newline or a zero - and the three fields around
/// it are the identity, which must stay readable for the file to be debuggable.
fn load() -> BTreeMap<Identity, Vec<u8>> {
    let Ok(text) = std::fs::read_to_string(store_path()) else {
        return BTreeMap::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let identity = Identity {
                class: f.next()?.to_string(),
                service: f.next()?.to_string(),
                account: f.next()?.to_string(),
            };
            Some((identity, from_hex(f.next()?)?))
        })
        .collect()
}

fn save(items: &BTreeMap<Identity, Vec<u8>>) {
    let mut text = String::new();
    for (identity, value) in items {
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            identity.class,
            identity.service,
            identity.account,
            to_hex(value)
        ));
    }
    let path = store_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, text);
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// The value at `key` in `query`, as text.
///
/// # Safety
///
/// `query` must be null or a `CFDictionary`.
unsafe fn attribute(query: CFTypeRef, key: &str) -> Option<String> {
    let name = foundation::string::from_str(key);
    // SAFETY: the caller's obligation for `query`; `name` is a live string.
    let value = unsafe { corefoundation::api::CFDictionaryGetValue(query, name as CFTypeRef) };
    // SAFETY: a fresh string this call owns.
    unsafe { corefoundation::api::CFRelease(name as CFTypeRef) };
    if value.is_null() {
        return None;
    }
    // SAFETY: whatever the caller stored, read as a string.
    let bytes = unsafe { foundation::string::bytes(value as objc::object::Id) };
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// # Safety
///
/// `query` must be null or a `CFDictionary`.
unsafe fn identity_of(query: CFTypeRef) -> Identity {
    Identity {
        class: unsafe { attribute(query, "class") }.unwrap_or_else(|| "genp".to_string()),
        service: unsafe { attribute(query, "svce") }.unwrap_or_default(),
        account: unsafe { attribute(query, "acct") }.unwrap_or_default(),
    }
}

/// The bytes of the `kSecValueData` in `query`, if it has one.
///
/// # Safety
///
/// `query` must be null or a `CFDictionary`.
unsafe fn value_data(query: CFTypeRef) -> Option<Vec<u8>> {
    let name = foundation::string::from_str("v_Data");
    let value = unsafe { corefoundation::api::CFDictionaryGetValue(query, name as CFTypeRef) };
    // SAFETY: a fresh string this call owns.
    unsafe { corefoundation::api::CFRelease(name as CFTypeRef) };
    if value.is_null() {
        return None;
    }
    // A `CFData` or a string, and **type-checked** rather than read as bytes on
    // the strength of the key it was under. `CFDataGetLength` on an object that
    // is not a `CFData` reads a length out of another type's field, and the
    // first thing done with a length is to allocate it: the failure is
    // `memory allocation of 140733193388032 bytes failed`, in this runtime,
    // nowhere near the call that passed the wrong object.
    //
    // SAFETY: the caller's obligation; both accessors answer `None` for an
    // object of the wrong class.
    if let Some(data) = unsafe { corefoundation::collections::data(value) } {
        return Some(data.clone());
    }
    // SAFETY: as above - answers empty for anything that is not a string.
    let text = unsafe { foundation::string::bytes(value as objc::object::Id) };
    (!text.is_empty()).then_some(text)
}

/// `OSStatus SecItemAdd(CFDictionaryRef attributes, CFTypeRef *result)`.
///
/// `errSecDuplicateItem` for an identity already stored, which is Apple's
/// answer and the one a caller branches on to decide between `SecItemAdd` and
/// `SecItemUpdate`.
///
/// # Safety
///
/// `attributes` is null or a `CFDictionary`; `result` is null or writable.
pub unsafe extern "C" fn SecItemAdd(attributes: CFTypeRef, result: *mut CFTypeRef) -> OSStatus {
    if !result.is_null() {
        // Nothing is returned: `kSecReturnData` on an *add* asks for the item
        // back, and this store has nothing to say about it that the caller did
        // not just provide.
        unsafe { result.write(std::ptr::null_mut()) };
    }
    if attributes.is_null() {
        return PARAM;
    }
    let identity = unsafe { identity_of(attributes) };
    let Some(data) = (unsafe { value_data(attributes) }) else {
        return PARAM;
    };
    let mut items = items();
    if items.contains_key(&identity) {
        return DUPLICATE_ITEM;
    }
    items.insert(identity, data);
    save(&items);
    SUCCESS
}

/// `OSStatus SecItemCopyMatching(CFDictionaryRef query, CFTypeRef *result)`.
///
/// Answers the item's data when `kSecReturnData` asked for it. A caller that
/// asked for attributes instead gets `errSecItemNotFound` rather than a
/// fabricated dictionary: the attributes this store keeps are the three it was
/// given, and handing those back as though they were the item's would tell a
/// caller its creation date and accessibility were things it could rely on.
///
/// # Safety
///
/// As [`SecItemAdd`].
pub unsafe extern "C" fn SecItemCopyMatching(query: CFTypeRef, result: *mut CFTypeRef) -> OSStatus {
    if !result.is_null() {
        unsafe { result.write(std::ptr::null_mut()) };
    }
    if query.is_null() {
        return PARAM;
    }
    let identity = unsafe { identity_of(query) };
    let Some(data) = items().get(&identity).cloned() else {
        return ITEM_NOT_FOUND;
    };
    if result.is_null() {
        // A query with no out parameter is asking only whether the item is
        // there, which it is.
        return SUCCESS;
    }
    let object = corefoundation::collections::new_data(data);
    // SAFETY: a `Copy`, so the caller owns what it is handed.
    unsafe { result.write(object) };
    SUCCESS
}

/// `OSStatus SecItemDelete(CFDictionaryRef query)`.
///
/// # Safety
///
/// `query` is null or a `CFDictionary`.
pub unsafe extern "C" fn SecItemDelete(query: CFTypeRef) -> OSStatus {
    if query.is_null() {
        return PARAM;
    }
    let identity = unsafe { identity_of(query) };
    let mut items = items();
    if items.remove(&identity).is_none() {
        return ITEM_NOT_FOUND;
    }
    save(&items);
    SUCCESS
}

/// `OSStatus SecItemUpdate(CFDictionaryRef query, CFDictionaryRef changes)`.
///
/// Only the value changes. An update that renamed the account would move the
/// item to an identity the caller did not ask about, and no caller in this tree
/// sends one.
///
/// # Safety
///
/// Both are null or `CFDictionary`s.
pub unsafe extern "C" fn SecItemUpdate(query: CFTypeRef, changes: CFTypeRef) -> OSStatus {
    if query.is_null() || changes.is_null() {
        return PARAM;
    }
    let identity = unsafe { identity_of(query) };
    let Some(data) = (unsafe { value_data(changes) }) else {
        return PARAM;
    };
    let mut items = items();
    if !items.contains_key(&identity) {
        return ITEM_NOT_FOUND;
    }
    items.insert(identity, data);
    save(&items);
    SUCCESS
}

exports::exports! {
    functions {
        "SecItemAdd" => SecItemAdd,
        "SecItemCopyMatching" => SecItemCopyMatching,
        "SecItemDelete" => SecItemDelete,
        "SecItemUpdate" => SecItemUpdate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_every_byte() {
        let bytes: Vec<u8> = (0..=255u8).collect();
        assert_eq!(from_hex(&to_hex(&bytes)), Some(bytes));
    }

    /// An odd-length line is a truncated file, not an item with half a byte.
    #[test]
    fn a_malformed_value_is_refused_rather_than_half_read() {
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
    }
}
