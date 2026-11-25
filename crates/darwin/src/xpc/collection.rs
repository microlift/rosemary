//! `xpc_dictionary_*` and `xpc_array_*`.
//!
//! A collection holds a reference to each value, and a getter answers the
//! value itself, not retained: it lives as long as the collection keeps it.

use std::ffi::{CStr, CString, c_char};
use std::sync::Mutex;

use super::object::{
    self, Handle, Value, bool_object, c_string, copy, equal, lock, new, release, retain, slice,
};

/// `XPC_ARRAY_APPEND`, the index `xpc_array_set_*` takes to mean the end.
const ARRAY_APPEND: usize = usize::MAX;

/// Where a received message came from, and how its sender waits for an answer.
pub struct Origin {
    /// The connection it arrived on, retained for the message's life.
    pub connection: Handle,
    /// The sender's number for the reply it waits on, if it waits on one.
    pub reply: Option<u64>,
}

#[derive(Default)]
pub struct Dictionary {
    entries: Mutex<Vec<(CString, Handle)>>,
    origin: Option<Origin>,
}

impl Dictionary {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_origin(origin: Origin) -> Self {
        Dictionary {
            entries: Mutex::new(Vec::new()),
            origin: Some(origin),
        }
    }

    pub fn origin(&self) -> Option<&Origin> {
        self.origin.as_ref()
    }

    /// Store `value` under `key`, taking a reference to it; a null value
    /// removes the key, as `xpc_dictionary_set_value` documents.
    pub fn set(&self, key: &CStr, value: Handle) {
        let mut entries = lock(&self.entries);
        let at = entries.iter().position(|(k, _)| k.as_c_str() == key);
        let replaced = match (at, value) {
            (Some(at), 0) => Some(entries.remove(at).1),
            (Some(at), value) => Some(std::mem::replace(&mut entries[at].1, retain(value))),
            (None, 0) => None,
            (None, value) => {
                entries.push((key.to_owned(), retain(value)));
                None
            }
        };
        drop(entries);
        if let Some(old) = replaced {
            // SAFETY: the reference this dictionary held.
            unsafe { release(old) };
        }
    }

    /// Store a value the caller just made, giving this dictionary its reference.
    pub fn set_owned(&self, key: &CStr, value: Handle) {
        self.set(key, value);
        // SAFETY: the creator's reference, now that the dictionary has its own.
        unsafe { release(value) };
    }

    pub fn get(&self, key: &CStr) -> Handle {
        lock(&self.entries)
            .iter()
            .find(|(k, _)| k.as_c_str() == key)
            .map_or(0, |(_, v)| *v)
    }

    pub fn count(&self) -> usize {
        lock(&self.entries).len()
    }

    /// The entries, each value retained, so a caller can visit them without
    /// holding the lock while guest code runs.
    pub fn snapshot(&self) -> Vec<(CString, Handle)> {
        lock(&self.entries)
            .iter()
            .map(|(k, v)| (k.clone(), retain(*v)))
            .collect()
    }

    pub fn deep_copy(&self) -> Dictionary {
        let entries = lock(&self.entries)
            .iter()
            .filter_map(|(k, v)| {
                // SAFETY: a value this dictionary holds a reference to.
                let copied = copy(*v, unsafe { object::value(*v) }?);
                (copied != 0).then(|| (k.clone(), copied))
            })
            .collect();
        Dictionary {
            entries: Mutex::new(entries),
            origin: None,
        }
    }

    pub fn equal(&self, other: &Dictionary) -> bool {
        let (mine, theirs) = (self.snapshot(), other.snapshot());
        let same = mine.len() == theirs.len()
            && mine.iter().all(|(k, v)| {
                theirs.iter().any(|(tk, tv)| {
                    // SAFETY: values both snapshots hold references to.
                    tk == k && unsafe { matches!((object::value(*v), object::value(*tv)), (Some(a), Some(b)) if equal(a, b)) }
                })
            });
        for (_, v) in mine.into_iter().chain(theirs) {
            // SAFETY: the snapshot's references.
            unsafe { release(v) };
        }
        same
    }

    pub fn describe(&self) -> String {
        let snapshot = self.snapshot();
        let parts: Vec<String> = snapshot
            .iter()
            .map(|(k, v)| {
                // SAFETY: a value the snapshot holds a reference to.
                let text = unsafe { object::value(*v) }
                    .map(|value| object::describe(*v, value))
                    .unwrap_or_default();
                format!("\"{}\" => {text}", k.to_string_lossy())
            })
            .collect();
        for (_, v) in snapshot {
            // SAFETY: the snapshot's references.
            unsafe { release(v) };
        }
        parts.join(", ")
    }
}

impl Drop for Dictionary {
    fn drop(&mut self) {
        for (_, value) in lock(&self.entries).drain(..) {
            // SAFETY: the reference this dictionary held.
            unsafe { release(value) };
        }
        if let Some(origin) = self.origin.take() {
            // SAFETY: the reference the origin held.
            unsafe { release(origin.connection) };
        }
    }
}

#[derive(Default)]
pub struct Array {
    items: Mutex<Vec<Handle>>,
}

impl Array {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, value: Handle) {
        if value != 0 {
            lock(&self.items).push(retain(value));
        }
    }

    pub fn push_owned(&self, value: Handle) {
        self.push(value);
        // SAFETY: the creator's reference, now that the array has its own.
        unsafe { release(value) };
    }

    /// Replace the item at `index`, or append when it is [`ARRAY_APPEND`].
    /// Darwin aborts on any other index out of range; this declines instead.
    fn set(&self, index: usize, value: Handle) {
        if value == 0 {
            return;
        }
        let mut items = lock(&self.items);
        if index == ARRAY_APPEND {
            items.push(retain(value));
            return;
        }
        let Some(slot) = items.get_mut(index) else {
            return;
        };
        let old = std::mem::replace(slot, retain(value));
        drop(items);
        // SAFETY: the reference this array held.
        unsafe { release(old) };
    }

    fn set_owned(&self, index: usize, value: Handle) {
        self.set(index, value);
        // SAFETY: the creator's reference.
        unsafe { release(value) };
    }

    pub fn get(&self, index: usize) -> Handle {
        lock(&self.items).get(index).copied().unwrap_or(0)
    }

    pub fn count(&self) -> usize {
        lock(&self.items).len()
    }

    pub fn snapshot(&self) -> Vec<Handle> {
        lock(&self.items).iter().map(|v| retain(*v)).collect()
    }

    pub fn deep_copy(&self) -> Array {
        let items = lock(&self.items)
            .iter()
            // SAFETY: a value this array holds a reference to.
            .filter_map(|v| unsafe { object::value(*v) }.map(|value| copy(*v, value)))
            .filter(|copied| *copied != 0)
            .collect();
        Array {
            items: Mutex::new(items),
        }
    }

    pub fn equal(&self, other: &Array) -> bool {
        let (mine, theirs) = (self.snapshot(), other.snapshot());
        let same = mine.len() == theirs.len() && mine.iter().zip(&theirs).all(|(a, b)| {
            // SAFETY: values both snapshots hold references to.
            unsafe {
                matches!((object::value(*a), object::value(*b)), (Some(a), Some(b)) if equal(a, b))
            }
        });
        for v in mine.into_iter().chain(theirs) {
            // SAFETY: the snapshot's references.
            unsafe { release(v) };
        }
        same
    }

    pub fn describe(&self) -> String {
        let snapshot = self.snapshot();
        let parts: Vec<String> = snapshot
            .iter()
            .enumerate()
            .map(|(i, v)| {
                // SAFETY: a value the snapshot holds a reference to.
                let text = unsafe { object::value(*v) }
                    .map(|value| object::describe(*v, value))
                    .unwrap_or_default();
                format!("{i}: {text}")
            })
            .collect();
        for v in snapshot {
            // SAFETY: the snapshot's references.
            unsafe { release(v) };
        }
        parts.join(", ")
    }
}

impl Drop for Array {
    fn drop(&mut self) {
        for value in lock(&self.items).drain(..) {
            // SAFETY: the reference this array held.
            unsafe { release(value) };
        }
    }
}

/// The dictionary `handle` is. An error answers too: a connection's error is
/// read with the same getters, `XPC_ERROR_KEY_DESCRIPTION` foremost.
///
/// # Safety
///
/// As [`object::get`].
pub unsafe fn dictionary<'a>(handle: Handle) -> Option<&'a Dictionary> {
    match unsafe { object::value(handle) }? {
        Value::Dictionary(dictionary) | Value::Error(dictionary) => Some(dictionary),
        _ => None,
    }
}

/// # Safety
///
/// As [`object::get`].
pub unsafe fn array<'a>(handle: Handle) -> Option<&'a Array> {
    match unsafe { object::value(handle) }? {
        Value::Array(array) => Some(array),
        _ => None,
    }
}

/// `xpc_object_t xpc_dictionary_create(const char *const *keys,
/// const xpc_object_t *values, size_t count)`.
///
/// # Safety
///
/// `keys` and `values` each address `count` elements, or `count` is zero.
pub unsafe extern "C" fn xpc_dictionary_create(
    keys: *const *const c_char,
    values: *const Handle,
    count: usize,
) -> Handle {
    let dictionary = Dictionary::new();
    for i in 0..count {
        // SAFETY: the caller's arrays of `count`.
        let (key, value) = unsafe { (keys.add(i).read(), values.add(i).read()) };
        if let Some(key) = unsafe { c_string(key) } {
            dictionary.set(key, value);
        }
    }
    new(Value::Dictionary(dictionary))
}

pub extern "C" fn xpc_dictionary_create_empty() -> Handle {
    new(Value::Dictionary(Dictionary::new()))
}

/// `xpc_object_t xpc_dictionary_create_reply(xpc_object_t original)`: `NULL`
/// unless `original` came from a peer waiting on an answer.
pub unsafe extern "C" fn xpc_dictionary_create_reply(original: Handle) -> Handle {
    let Some(origin) = (unsafe { dictionary(original) }).and_then(Dictionary::origin) else {
        return 0;
    };
    if origin.reply.is_none() {
        return 0;
    }
    new(Value::Dictionary(Dictionary::with_origin(Origin {
        connection: retain(origin.connection),
        reply: origin.reply,
    })))
}

/// `xpc_connection_t xpc_dictionary_get_remote_connection(xpc_object_t)`.
pub unsafe extern "C" fn xpc_dictionary_get_remote_connection(message: Handle) -> Handle {
    unsafe { dictionary(message) }
        .and_then(Dictionary::origin)
        .map_or(0, |origin| origin.connection)
}

pub unsafe extern "C" fn xpc_dictionary_set_value(
    dictionary_handle: Handle,
    key: *const c_char,
    value: Handle,
) {
    if let (Some(d), Some(key)) = unsafe { (dictionary(dictionary_handle), c_string(key)) } {
        d.set(key, value);
    }
}

pub unsafe extern "C" fn xpc_dictionary_get_value(
    dictionary_handle: Handle,
    key: *const c_char,
) -> Handle {
    match unsafe { (dictionary(dictionary_handle), c_string(key)) } {
        (Some(d), Some(key)) => d.get(key),
        _ => 0,
    }
}

pub unsafe extern "C" fn xpc_dictionary_get_count(dictionary_handle: Handle) -> usize {
    unsafe { dictionary(dictionary_handle) }.map_or(0, Dictionary::count)
}

/// `bool xpc_dictionary_apply(xpc_object_t, xpc_dictionary_applier_t)`:
/// visit each entry until the applier answers false.
pub unsafe extern "C" fn xpc_dictionary_apply(dictionary_handle: Handle, applier: u64) -> bool {
    let Some(d) = (unsafe { dictionary(dictionary_handle) }) else {
        return false;
    };
    let snapshot = d.snapshot();
    let mut completed = true;
    for (key, value) in &snapshot {
        if completed {
            // SAFETY: a block of the applier's type, already callable here.
            completed = unsafe {
                crate::dispatch::block::invoke_applier(applier, key.as_ptr() as u64, *value)
            };
        }
    }
    for (_, value) in snapshot {
        // SAFETY: the snapshot's references.
        unsafe { release(value) };
    }
    completed
}

/// The typed accessors, which are `set_value`/`get_value` and a scalar's own
/// constructor or getter.
macro_rules! typed_entries {
    ($($set:ident / $get:ident: $ty:ty => $make:path, $read:path, $none:expr;)*) => {
        $(
            pub unsafe extern "C" fn $set(dictionary_handle: Handle, key: *const c_char, value: $ty) {
                if let (Some(d), Some(key)) = unsafe { (dictionary(dictionary_handle), c_string(key)) } {
                    d.set_owned(key, $make(value));
                }
            }

            pub unsafe extern "C" fn $get(dictionary_handle: Handle, key: *const c_char) -> $ty {
                match unsafe { (dictionary(dictionary_handle), c_string(key)) } {
                    (Some(d), Some(key)) => unsafe { $read(d.get(key)) },
                    _ => $none,
                }
            }
        )*
    };
}

fn make_bool(value: bool) -> Handle {
    retain(bool_object(value))
}

typed_entries! {
    xpc_dictionary_set_bool / xpc_dictionary_get_bool: bool
        => make_bool, object::xpc_bool_get_value, false;
    xpc_dictionary_set_int64 / xpc_dictionary_get_int64: i64
        => object::xpc_int64_create, object::xpc_int64_get_value, 0;
    xpc_dictionary_set_uint64 / xpc_dictionary_get_uint64: u64
        => object::xpc_uint64_create, object::xpc_uint64_get_value, 0;
    xpc_dictionary_set_double / xpc_dictionary_get_double: f64
        => object::xpc_double_create, object::xpc_double_get_value, f64::NAN;
    xpc_dictionary_set_date / xpc_dictionary_get_date: i64
        => object::xpc_date_create, object::xpc_date_get_value, 0;
}

pub unsafe extern "C" fn xpc_dictionary_set_string(
    dictionary_handle: Handle,
    key: *const c_char,
    string: *const c_char,
) {
    if let (Some(d), Some(key)) = unsafe { (dictionary(dictionary_handle), c_string(key)) } {
        d.set_owned(key, unsafe { object::xpc_string_create(string) });
    }
}

pub unsafe extern "C" fn xpc_dictionary_get_string(
    dictionary_handle: Handle,
    key: *const c_char,
) -> *const c_char {
    match unsafe { (dictionary(dictionary_handle), c_string(key)) } {
        (Some(d), Some(key)) => unsafe { object::xpc_string_get_string_ptr(d.get(key)) },
        _ => std::ptr::null(),
    }
}

/// # Safety
///
/// `bytes` addresses `length` readable bytes.
pub unsafe extern "C" fn xpc_dictionary_set_data(
    dictionary_handle: Handle,
    key: *const c_char,
    bytes: *const u8,
    length: usize,
) {
    if let (Some(d), Some(key)) = unsafe { (dictionary(dictionary_handle), c_string(key)) } {
        d.set_owned(
            key,
            new(Value::Data(unsafe { slice(bytes, length) }.into())),
        );
    }
}

/// `const void *xpc_dictionary_get_data(xpc_object_t, const char *, size_t *length)`.
///
/// # Safety
///
/// `length` is a writable `size_t`, or null.
pub unsafe extern "C" fn xpc_dictionary_get_data(
    dictionary_handle: Handle,
    key: *const c_char,
    length: *mut usize,
) -> *const u8 {
    let value = match unsafe { (dictionary(dictionary_handle), c_string(key)) } {
        (Some(d), Some(key)) => d.get(key),
        _ => 0,
    };
    let (bytes, len) = match unsafe { object::value(value) } {
        Some(Value::Data(data)) => (data.as_ptr(), data.len()),
        _ => (std::ptr::null(), 0),
    };
    if !length.is_null() {
        // SAFETY: the caller's `size_t`.
        unsafe { length.write(len) };
    }
    bytes
}

/// # Safety
///
/// `uuid` addresses 16 readable bytes.
pub unsafe extern "C" fn xpc_dictionary_set_uuid(
    dictionary_handle: Handle,
    key: *const c_char,
    uuid: *const u8,
) {
    if let (Some(d), Some(key)) = unsafe { (dictionary(dictionary_handle), c_string(key)) } {
        let value = unsafe { object::xpc_uuid_create(uuid) };
        if value != 0 {
            d.set_owned(key, value);
        }
    }
}

pub unsafe extern "C" fn xpc_dictionary_get_uuid(
    dictionary_handle: Handle,
    key: *const c_char,
) -> *const u8 {
    match unsafe { (dictionary(dictionary_handle), c_string(key)) } {
        (Some(d), Some(key)) => unsafe { object::xpc_uuid_get_bytes(d.get(key)) },
        _ => std::ptr::null(),
    }
}

pub unsafe extern "C" fn xpc_dictionary_get_dictionary(
    dictionary_handle: Handle,
    key: *const c_char,
) -> Handle {
    let value = unsafe { xpc_dictionary_get_value(dictionary_handle, key) };
    match unsafe { object::value(value) } {
        Some(Value::Dictionary(_)) => value,
        _ => 0,
    }
}

pub unsafe extern "C" fn xpc_dictionary_get_array(
    dictionary_handle: Handle,
    key: *const c_char,
) -> Handle {
    let value = unsafe { xpc_dictionary_get_value(dictionary_handle, key) };
    match unsafe { object::value(value) } {
        Some(Value::Array(_)) => value,
        _ => 0,
    }
}

/// # Safety
///
/// `objects` addresses `count` elements, or `count` is zero.
pub unsafe extern "C" fn xpc_array_create(objects: *const Handle, count: usize) -> Handle {
    let array = Array::new();
    for i in 0..count {
        // SAFETY: the caller's array of `count`.
        array.push(unsafe { objects.add(i).read() });
    }
    new(Value::Array(array))
}

pub extern "C" fn xpc_array_create_empty() -> Handle {
    new(Value::Array(Array::new()))
}

pub unsafe extern "C" fn xpc_array_append_value(array_handle: Handle, value: Handle) {
    if let Some(a) = unsafe { array(array_handle) } {
        a.push(value);
    }
}

pub unsafe extern "C" fn xpc_array_set_value(array_handle: Handle, index: usize, value: Handle) {
    if let Some(a) = unsafe { array(array_handle) } {
        a.set(index, value);
    }
}

pub unsafe extern "C" fn xpc_array_get_value(array_handle: Handle, index: usize) -> Handle {
    unsafe { array(array_handle) }.map_or(0, |a| a.get(index))
}

pub unsafe extern "C" fn xpc_array_get_count(array_handle: Handle) -> usize {
    unsafe { array(array_handle) }.map_or(0, Array::count)
}

/// `bool xpc_array_apply(xpc_object_t, xpc_array_applier_t)`: the applier
/// takes the index and the value.
pub unsafe extern "C" fn xpc_array_apply(array_handle: Handle, applier: u64) -> bool {
    let Some(a) = (unsafe { array(array_handle) }) else {
        return false;
    };
    let snapshot = a.snapshot();
    let mut completed = true;
    for (index, value) in snapshot.iter().enumerate() {
        if completed {
            // SAFETY: a block of the applier's type, already callable here.
            completed =
                unsafe { crate::dispatch::block::invoke_applier(applier, index as u64, *value) };
        }
    }
    for value in snapshot {
        // SAFETY: the snapshot's references.
        unsafe { release(value) };
    }
    completed
}

macro_rules! typed_items {
    ($($set:ident / $get:ident: $ty:ty => $make:path, $read:path, $none:expr;)*) => {
        $(
            pub unsafe extern "C" fn $set(array_handle: Handle, index: usize, value: $ty) {
                if let Some(a) = unsafe { array(array_handle) } {
                    a.set_owned(index, $make(value));
                }
            }

            pub unsafe extern "C" fn $get(array_handle: Handle, index: usize) -> $ty {
                match unsafe { array(array_handle) } {
                    Some(a) => unsafe { $read(a.get(index)) },
                    None => $none,
                }
            }
        )*
    };
}

typed_items! {
    xpc_array_set_bool / xpc_array_get_bool: bool
        => make_bool, object::xpc_bool_get_value, false;
    xpc_array_set_int64 / xpc_array_get_int64: i64
        => object::xpc_int64_create, object::xpc_int64_get_value, 0;
    xpc_array_set_uint64 / xpc_array_get_uint64: u64
        => object::xpc_uint64_create, object::xpc_uint64_get_value, 0;
    xpc_array_set_double / xpc_array_get_double: f64
        => object::xpc_double_create, object::xpc_double_get_value, f64::NAN;
    xpc_array_set_date / xpc_array_get_date: i64
        => object::xpc_date_create, object::xpc_date_get_value, 0;
}

pub unsafe extern "C" fn xpc_array_set_string(
    array_handle: Handle,
    index: usize,
    string: *const c_char,
) {
    if let Some(a) = unsafe { array(array_handle) } {
        let value = unsafe { object::xpc_string_create(string) };
        if value != 0 {
            a.set_owned(index, value);
        }
    }
}

pub unsafe extern "C" fn xpc_array_get_string(array_handle: Handle, index: usize) -> *const c_char {
    match unsafe { array(array_handle) } {
        Some(a) => unsafe { object::xpc_string_get_string_ptr(a.get(index)) },
        None => std::ptr::null(),
    }
}

/// # Safety
///
/// `bytes` addresses `length` readable bytes.
pub unsafe extern "C" fn xpc_array_set_data(
    array_handle: Handle,
    index: usize,
    bytes: *const u8,
    length: usize,
) {
    if let Some(a) = unsafe { array(array_handle) } {
        a.set_owned(
            index,
            new(Value::Data(unsafe { slice(bytes, length) }.into())),
        );
    }
}

/// # Safety
///
/// `length` is a writable `size_t`, or null.
pub unsafe extern "C" fn xpc_array_get_data(
    array_handle: Handle,
    index: usize,
    length: *mut usize,
) -> *const u8 {
    let value = unsafe { xpc_array_get_value(array_handle, index) };
    let (bytes, len) = match unsafe { object::value(value) } {
        Some(Value::Data(data)) => (data.as_ptr(), data.len()),
        _ => (std::ptr::null(), 0),
    };
    if !length.is_null() {
        // SAFETY: the caller's `size_t`.
        unsafe { length.write(len) };
    }
    bytes
}

pub unsafe extern "C" fn xpc_array_get_dictionary(array_handle: Handle, index: usize) -> Handle {
    let value = unsafe { xpc_array_get_value(array_handle, index) };
    match unsafe { object::value(value) } {
        Some(Value::Dictionary(_)) => value,
        _ => 0,
    }
}

pub unsafe extern "C" fn xpc_array_get_array(array_handle: Handle, index: usize) -> Handle {
    let value = unsafe { xpc_array_get_value(array_handle, index) };
    match unsafe { object::value(value) } {
        Some(Value::Array(_)) => value,
        _ => 0,
    }
}
