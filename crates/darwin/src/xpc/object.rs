//! The XPC object model: what `xpc_object_t` points at, and the scalar types.
//!
//! On Darwin every XPC object is an `OS_object`, retained and released by the
//! same machinery as a dispatch object, and ARC treats both alike. Each object
//! here therefore begins with dispatch's [`Header`], so `xpc_retain`,
//! `os_retain`, `dispatch_retain` and `objc_retain` all move one count.

use std::ffi::{CStr, CString, c_char};
use std::sync::Arc;
use std::sync::Mutex;

use crate::dispatch::object::{self as dispatch, Header, Kind};

use super::collection::{Array, Dictionary};
use super::connection::{Connection, Endpoint};

/// `xpc_type_t`, which a caller only ever compares by address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum Type {
    Connection,
    Endpoint,
    Null,
    Bool,
    Int64,
    Uint64,
    Double,
    Date,
    Data,
    String,
    Uuid,
    Fd,
    Shmem,
    Array,
    Dictionary,
    Error,
}

pub const TYPES: [Type; 16] = [
    Type::Connection,
    Type::Endpoint,
    Type::Null,
    Type::Bool,
    Type::Int64,
    Type::Uint64,
    Type::Double,
    Type::Date,
    Type::Data,
    Type::String,
    Type::Uuid,
    Type::Fd,
    Type::Shmem,
    Type::Array,
    Type::Dictionary,
    Type::Error,
];

impl Type {
    /// The `_xpc_type_*` symbol, which is also what `xpc_type_get_name` answers
    /// without its prefix.
    pub fn name(self) -> &'static CStr {
        match self {
            Type::Connection => c"connection",
            Type::Endpoint => c"endpoint",
            Type::Null => c"null",
            Type::Bool => c"bool",
            Type::Int64 => c"int64",
            Type::Uint64 => c"uint64",
            Type::Double => c"double",
            Type::Date => c"date",
            Type::Data => c"data",
            Type::String => c"string",
            Type::Uuid => c"uuid",
            Type::Fd => c"fd",
            Type::Shmem => c"shmem",
            Type::Array => c"array",
            Type::Dictionary => c"dictionary",
            Type::Error => c"error",
        }
    }

    /// The address a guest's `XPC_TYPE_*` is.
    pub fn token(self) -> u64 {
        &TYPE_TOKENS[self as usize] as *const u64 as u64
    }

    pub fn from_token(token: u64) -> Option<Type> {
        TYPES.into_iter().find(|t| t.token() == token)
    }
}

/// One word per type; only the addresses matter.
static TYPE_TOKENS: [u64; TYPES.len()] = [0; TYPES.len()];

/// What an object holds.
pub enum Value {
    Null,
    Bool(bool),
    Int64(i64),
    Uint64(u64),
    Double(f64),
    /// Nanoseconds since the Unix epoch, as `xpc_date_create` takes them.
    Date(i64),
    Data(Box<[u8]>),
    String(CString),
    Uuid([u8; 16]),
    Array(Array),
    Dictionary(Dictionary),
    /// What a connection's handler receives when the connection breaks:
    /// a dictionary with a description, of type `XPC_TYPE_ERROR`.
    Error(Dictionary),
    Connection(Arc<Connection>),
    Endpoint(Endpoint),
}

impl Value {
    pub fn kind(&self) -> Type {
        match self {
            Value::Null => Type::Null,
            Value::Bool(_) => Type::Bool,
            Value::Int64(_) => Type::Int64,
            Value::Uint64(_) => Type::Uint64,
            Value::Double(_) => Type::Double,
            Value::Date(_) => Type::Date,
            Value::Data(_) => Type::Data,
            Value::String(_) => Type::String,
            Value::Uuid(_) => Type::Uuid,
            Value::Array(_) => Type::Array,
            Value::Dictionary(_) => Type::Dictionary,
            Value::Error(_) => Type::Error,
            Value::Connection(_) => Type::Connection,
            Value::Endpoint(_) => Type::Endpoint,
        }
    }
}

/// `#[repr(C)]` with the header first: [`dispatch`] recovers this object by
/// casting the handle to a `*mut Header`.
#[repr(C)]
pub struct Object {
    #[allow(dead_code)]
    header: Header,
    pub value: Value,
}

/// An `xpc_object_t` as a guest holds it.
pub type Handle = u64;

/// A fresh object with one reference, which the caller owns.
pub fn new(value: Value) -> Handle {
    Box::into_raw(Box::new(Object {
        header: Header::new(Kind::Xpc),
        value,
    })) as Handle
}

/// An object the process owns for its whole life, as `XPC_BOOL_TRUE` and the
/// connection errors are: releasing one does nothing.
pub fn new_global(value: Value) -> Handle {
    let handle = new(value);
    // SAFETY: just allocated by this module.
    unsafe { dispatch::make_global(handle) };
    handle
}

/// The object `handle` names, if it is an XPC object.
///
/// # Safety
///
/// `handle` is null or an object of this crate's dispatch or XPC modules, and
/// stays alive for `'a`.
pub unsafe fn get<'a>(handle: Handle) -> Option<&'a Object> {
    // SAFETY: the caller's guarantee.
    unsafe { dispatch::typed(handle, Kind::Xpc) }?;
    // SAFETY: the kind check says the allocation is an `Object`.
    Some(unsafe { &*(handle as *const Object) })
}

/// The value `handle` holds.
///
/// # Safety
///
/// As [`get`].
pub unsafe fn value<'a>(handle: Handle) -> Option<&'a Value> {
    unsafe { get(handle) }.map(|object| &object.value)
}

pub fn retain(handle: Handle) -> Handle {
    dispatch::retain(handle);
    handle
}

/// # Safety
///
/// `handle` is null or a live object whose reference the caller gives up.
pub unsafe fn release(handle: Handle) {
    unsafe { dispatch::release(handle) };
}

/// The two booleans, which Darwin keeps as two global objects.
pub fn bool_object(value: bool) -> Handle {
    static TRUE: std::sync::OnceLock<Handle> = std::sync::OnceLock::new();
    static FALSE: std::sync::OnceLock<Handle> = std::sync::OnceLock::new();
    let cell = match value {
        true => &TRUE,
        false => &FALSE,
    };
    *cell.get_or_init(|| new_global(Value::Bool(value)))
}

/// `xpc_object_t xpc_retain(xpc_object_t)`.
pub unsafe extern "C" fn xpc_retain(object: Handle) -> Handle {
    retain(object)
}

/// `void xpc_release(xpc_object_t)`.
pub unsafe extern "C" fn xpc_release(object: Handle) {
    unsafe { release(object) };
}

/// `xpc_type_t xpc_get_type(xpc_object_t)`.
pub unsafe extern "C" fn xpc_get_type(object: Handle) -> u64 {
    match unsafe { value(object) } {
        Some(value) => value.kind().token(),
        None => 0,
    }
}

/// `const char *xpc_type_get_name(xpc_type_t)`.
pub unsafe extern "C" fn xpc_type_get_name(token: u64) -> *const c_char {
    match Type::from_token(token) {
        Some(kind) => kind.name().as_ptr(),
        None => std::ptr::null(),
    }
}

/// `xpc_object_t xpc_null_create(void)`.
pub extern "C" fn xpc_null_create() -> Handle {
    new(Value::Null)
}

/// `xpc_object_t xpc_bool_create(bool)`.
pub extern "C" fn xpc_bool_create(value: bool) -> Handle {
    bool_object(value)
}

/// `bool xpc_bool_get_value(xpc_object_t)`.
pub unsafe extern "C" fn xpc_bool_get_value(object: Handle) -> bool {
    matches!(unsafe { value(object) }, Some(Value::Bool(true)))
}

pub extern "C" fn xpc_int64_create(value: i64) -> Handle {
    new(Value::Int64(value))
}

pub unsafe extern "C" fn xpc_int64_get_value(object: Handle) -> i64 {
    match unsafe { value(object) } {
        Some(Value::Int64(v)) => *v,
        _ => 0,
    }
}

pub extern "C" fn xpc_uint64_create(value: u64) -> Handle {
    new(Value::Uint64(value))
}

pub unsafe extern "C" fn xpc_uint64_get_value(object: Handle) -> u64 {
    match unsafe { value(object) } {
        Some(Value::Uint64(v)) => *v,
        _ => 0,
    }
}

pub extern "C" fn xpc_double_create(value: f64) -> Handle {
    new(Value::Double(value))
}

pub unsafe extern "C" fn xpc_double_get_value(object: Handle) -> f64 {
    match unsafe { value(object) } {
        Some(Value::Double(v)) => *v,
        // Darwin answers NaN for an object that is not a double.
        _ => f64::NAN,
    }
}

pub extern "C" fn xpc_date_create(nanoseconds: i64) -> Handle {
    new(Value::Date(nanoseconds))
}

pub extern "C" fn xpc_date_create_from_current() -> Handle {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0);
    new(Value::Date(since))
}

pub unsafe extern "C" fn xpc_date_get_value(object: Handle) -> i64 {
    match unsafe { value(object) } {
        Some(Value::Date(v)) => *v,
        _ => 0,
    }
}

/// `xpc_object_t xpc_data_create(const void *bytes, size_t length)`.
///
/// # Safety
///
/// `bytes` addresses `length` readable bytes, or `length` is zero.
pub unsafe extern "C" fn xpc_data_create(bytes: *const u8, length: usize) -> Handle {
    new(Value::Data(unsafe { slice(bytes, length) }.into()))
}

pub unsafe extern "C" fn xpc_data_get_bytes_ptr(object: Handle) -> *const u8 {
    match unsafe { value(object) } {
        Some(Value::Data(bytes)) if !bytes.is_empty() => bytes.as_ptr(),
        _ => std::ptr::null(),
    }
}

pub unsafe extern "C" fn xpc_data_get_length(object: Handle) -> usize {
    match unsafe { value(object) } {
        Some(Value::Data(bytes)) => bytes.len(),
        _ => 0,
    }
}

/// `size_t xpc_data_get_bytes(xpc_object_t, void *buffer, size_t offset,
/// size_t length)`: copy up to `length` bytes from `offset`, answering how many.
///
/// # Safety
///
/// `buffer` addresses `length` writable bytes.
pub unsafe extern "C" fn xpc_data_get_bytes(
    object: Handle,
    buffer: *mut u8,
    offset: usize,
    length: usize,
) -> usize {
    let Some(Value::Data(bytes)) = (unsafe { value(object) }) else {
        return 0;
    };
    let Some(available) = bytes.get(offset..) else {
        return 0;
    };
    let count = available.len().min(length);
    if count > 0 {
        // SAFETY: `count` bytes of both, by the caller's guarantee and the slice.
        unsafe { std::ptr::copy_nonoverlapping(available.as_ptr(), buffer, count) };
    }
    count
}

/// # Safety
///
/// `string` is a NUL-terminated string, or null.
pub unsafe extern "C" fn xpc_string_create(string: *const c_char) -> Handle {
    match unsafe { c_string(string) } {
        Some(string) => new(Value::String(string.to_owned())),
        None => 0,
    }
}

pub unsafe extern "C" fn xpc_string_get_string_ptr(object: Handle) -> *const c_char {
    match unsafe { value(object) } {
        Some(Value::String(string)) => string.as_ptr(),
        _ => std::ptr::null(),
    }
}

pub unsafe extern "C" fn xpc_string_get_length(object: Handle) -> usize {
    match unsafe { value(object) } {
        Some(Value::String(string)) => string.as_bytes().len(),
        _ => 0,
    }
}

/// # Safety
///
/// `uuid` addresses 16 readable bytes.
pub unsafe extern "C" fn xpc_uuid_create(uuid: *const u8) -> Handle {
    if uuid.is_null() {
        return 0;
    }
    let mut bytes = [0u8; 16];
    // SAFETY: the caller's sixteen bytes.
    unsafe { std::ptr::copy_nonoverlapping(uuid, bytes.as_mut_ptr(), 16) };
    new(Value::Uuid(bytes))
}

pub unsafe extern "C" fn xpc_uuid_get_bytes(object: Handle) -> *const u8 {
    match unsafe { value(object) } {
        Some(Value::Uuid(bytes)) => bytes.as_ptr(),
        _ => std::ptr::null(),
    }
}

/// `xpc_object_t xpc_copy(xpc_object_t)`: an equal object the caller owns.
///
/// Collections are copied whole. A connection has no copy and answers `NULL`;
/// the immutable globals answer themselves.
pub unsafe extern "C" fn xpc_copy(object: Handle) -> Handle {
    match unsafe { value(object) } {
        Some(value) => copy(object, value),
        None => 0,
    }
}

/// A copy of `value`, which `handle` holds.
pub fn copy(handle: Handle, value: &Value) -> Handle {
    match value {
        Value::Null => new(Value::Null),
        Value::Bool(_) | Value::Error(_) => retain(handle),
        Value::Int64(v) => new(Value::Int64(*v)),
        Value::Uint64(v) => new(Value::Uint64(*v)),
        Value::Double(v) => new(Value::Double(*v)),
        Value::Date(v) => new(Value::Date(*v)),
        Value::Data(bytes) => new(Value::Data(bytes.clone())),
        Value::String(string) => new(Value::String(string.clone())),
        Value::Uuid(bytes) => new(Value::Uuid(*bytes)),
        Value::Array(array) => new(Value::Array(array.deep_copy())),
        Value::Dictionary(dictionary) => new(Value::Dictionary(dictionary.deep_copy())),
        Value::Endpoint(endpoint) => new(Value::Endpoint(endpoint.clone())),
        Value::Connection(_) => 0,
    }
}

/// `bool xpc_equal(xpc_object_t, xpc_object_t)`.
pub unsafe extern "C" fn xpc_equal(a: Handle, b: Handle) -> bool {
    if a == b {
        return true;
    }
    match unsafe { (value(a), value(b)) } {
        (Some(a), Some(b)) => equal(a, b),
        _ => false,
    }
}

pub fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int64(a), Value::Int64(b)) => a == b,
        (Value::Uint64(a), Value::Uint64(b)) => a == b,
        (Value::Double(a), Value::Double(b)) => a == b,
        (Value::Date(a), Value::Date(b)) => a == b,
        (Value::Data(a), Value::Data(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Uuid(a), Value::Uuid(b)) => a == b,
        (Value::Array(a), Value::Array(b)) => a.equal(b),
        (Value::Dictionary(a), Value::Dictionary(b)) | (Value::Error(a), Value::Error(b)) => {
            a.equal(b)
        }
        (Value::Connection(a), Value::Connection(b)) => Arc::ptr_eq(a, b),
        (Value::Endpoint(a), Value::Endpoint(b)) => a == b,
        _ => false,
    }
}

/// `size_t xpc_hash(xpc_object_t)`: equal objects hash equally.
pub unsafe extern "C" fn xpc_hash(object: Handle) -> usize {
    match unsafe { value(object) } {
        Some(value) => hash(value) as usize,
        None => 0,
    }
}

fn hash(value: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (value.kind() as usize).hash(&mut hasher);
    match value {
        Value::Bool(v) => v.hash(&mut hasher),
        Value::Int64(v) | Value::Date(v) => v.hash(&mut hasher),
        Value::Uint64(v) => v.hash(&mut hasher),
        Value::Double(v) => v.to_bits().hash(&mut hasher),
        Value::Data(bytes) => bytes.hash(&mut hasher),
        Value::String(string) => string.hash(&mut hasher),
        Value::Uuid(bytes) => bytes.hash(&mut hasher),
        // Order-independent for dictionaries, as their equality is.
        Value::Array(array) => array.count().hash(&mut hasher),
        Value::Dictionary(dictionary) | Value::Error(dictionary) => {
            dictionary.count().hash(&mut hasher)
        }
        Value::Null | Value::Connection(_) | Value::Endpoint(_) => {}
    }
    hasher.finish()
}

/// `char *xpc_copy_description(xpc_object_t)`: a string the caller frees.
pub unsafe extern "C" fn xpc_copy_description(object: Handle) -> *mut c_char {
    let text = match unsafe { value(object) } {
        Some(value) => describe(object, value),
        None => String::from("<null>"),
    };
    guest_string(&text)
}

/// The text `xpc_copy_description` gives `value`.
///
/// FIXME (ACCURACY): Darwin's layout - indentation, addresses, `<dictionary: ...>`
/// headers - is approximated, not reproduced byte for byte.
pub fn describe(handle: Handle, value: &Value) -> String {
    let kind = value.kind().name().to_string_lossy();
    let contents = match value {
        Value::Null => String::from("null"),
        Value::Bool(v) => v.to_string(),
        Value::Int64(v) | Value::Date(v) => v.to_string(),
        Value::Uint64(v) => v.to_string(),
        Value::Double(v) => v.to_string(),
        Value::Data(bytes) => format!("{} bytes", bytes.len()),
        Value::String(string) => format!("\"{}\"", string.to_string_lossy()),
        Value::Uuid(bytes) => bytes.iter().map(|b| format!("{b:02X}")).collect(),
        Value::Array(array) => array.describe(),
        Value::Dictionary(dictionary) | Value::Error(dictionary) => dictionary.describe(),
        Value::Connection(connection) => connection.describe(),
        Value::Endpoint(_) => String::new(),
    };
    format!("<{kind}: {handle:#x}> {{ {contents} }}")
}

/// `text` in a block the guest's `free` accepts.
pub fn guest_string(text: &str) -> *mut c_char {
    let bytes = text.as_bytes();
    // SAFETY: the allocator the guest frees with.
    let block = unsafe { crate::heap::malloc(bytes.len() + 1) }.cast::<u8>();
    if block.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a fresh block one byte longer than `bytes`.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), block, bytes.len());
        block.add(bytes.len()).write(0);
    }
    block.cast()
}

/// # Safety
///
/// `bytes` addresses `length` readable bytes, or `length` is zero.
pub unsafe fn slice<'a>(bytes: *const u8, length: usize) -> &'a [u8] {
    match bytes.is_null() || length == 0 {
        true => &[],
        // SAFETY: the caller's guarantee.
        false => unsafe { std::slice::from_raw_parts(bytes, length) },
    }
}

/// # Safety
///
/// `string` is a NUL-terminated string, or null.
pub unsafe fn c_string<'a>(string: *const c_char) -> Option<&'a CStr> {
    match string.is_null() {
        true => None,
        // SAFETY: the caller's guarantee.
        false => Some(unsafe { CStr::from_ptr(string) }),
    }
}

/// A lock that a panic elsewhere does not poison for every later caller.
pub fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
