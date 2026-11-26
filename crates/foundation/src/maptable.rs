//! The C interface to `NSMapTable`: a table whose keys and values are any
//! pointer-sized things, hashed, compared and retained by callbacks the caller
//! supplies.

// The C spellings are the interface.
#![allow(non_snake_case)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use objc::class::Class;
use objc::object::Id;

type Table = *const c_void;

/// `NSMapTableKeyCallBacks`. A null callback means the pointer itself: its
/// address hashes and compares, and nothing is retained.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KeyCallBacks {
    hash: Option<unsafe extern "C" fn(Table, *const c_void) -> usize>,
    is_equal: Option<unsafe extern "C" fn(Table, *const c_void, *const c_void) -> u8>,
    retain: Option<unsafe extern "C" fn(Table, *const c_void)>,
    release: Option<unsafe extern "C" fn(Table, *mut c_void)>,
    describe: Option<unsafe extern "C" fn(Table, *const c_void) -> Id>,
    not_a_key_marker: *const c_void,
}

/// `NSMapTableValueCallBacks`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ValueCallBacks {
    retain: Option<unsafe extern "C" fn(Table, *const c_void)>,
    release: Option<unsafe extern "C" fn(Table, *mut c_void)>,
    describe: Option<unsafe extern "C" fn(Table, *const c_void) -> Id>,
}

struct MapTable {
    keys: KeyCallBacks,
    values: ValueCallBacks,
    /// `(key, value)` pairs by the key's hash.
    buckets: Mutex<HashMap<usize, Vec<(usize, usize)>>>,
}

// SAFETY: the callbacks are plain function pointers, and the entries are
// addresses this table does not dereference itself.
unsafe impl Send for MapTable {}
unsafe impl Sync for MapTable {}

impl MapTable {
    // The callbacks are the caller's code, so each is entered as Darwin code.

    fn hash(&self, table: Table, key: usize) -> usize {
        match self.keys.hash {
            // SAFETY: the caller's own callback, given what it was written for.
            Some(hash) => darwin::call_as_darwin!(unsafe { hash(table, key as *const c_void) }),
            None => key,
        }
    }

    fn equal(&self, table: Table, a: usize, b: usize) -> bool {
        match self.keys.is_equal {
            // SAFETY: as above.
            Some(equal) => darwin::call_as_darwin!(unsafe {
                equal(table, a as *const c_void, b as *const c_void) != 0
            }),
            None => a == b,
        }
    }

    fn retain(&self, table: Table, key: usize, value: usize) {
        // SAFETY: as above.
        darwin::call_as_darwin!(unsafe {
            if let Some(retain) = self.keys.retain {
                retain(table, key as *const c_void);
            }
            if let Some(retain) = self.values.retain {
                retain(table, value as *const c_void);
            }
        })
    }

    fn release(&self, table: Table, key: Option<usize>, value: Option<usize>) {
        // SAFETY: as above.
        darwin::call_as_darwin!(unsafe {
            if let (Some(release), Some(key)) = (self.keys.release, key) {
                release(table, key as *mut c_void);
            }
            if let (Some(release), Some(value)) = (self.values.release, value) {
                release(table, value as *mut c_void);
            }
        })
    }

    /// The entry whose key equals `key`, as `(hash, stored key, value)`.
    ///
    /// The bucket is copied out before any comparison, so a callback that
    /// reaches this table again does not find it locked.
    fn find(&self, table: Table, key: usize) -> (usize, Option<(usize, usize)>) {
        let hash = self.hash(table, key);
        let candidates = self
            .buckets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&hash)
            .cloned()
            .unwrap_or_default();
        let found = candidates
            .into_iter()
            .find(|(stored, _)| self.equal(table, *stored, key));
        (hash, found)
    }

    fn replace(&self, hash: usize, key: usize, entry: Option<(usize, usize)>) {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let bucket = buckets.entry(hash).or_default();
        bucket.retain(|(stored, _)| *stored != key);
        if let Some(entry) = entry {
            bucket.push(entry);
        }
        if bucket.is_empty() {
            buckets.remove(&hash);
        }
    }
}

fn class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        objc::class::ptr(crate::boxed::register::<MapTable>(
            c"NSConcreteMapTable",
            None,
        )) as usize
    });
    // SAFETY: a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

/// # Safety
///
/// `table` must be null or an object this runtime allocated.
unsafe fn table_of<'a>(table: Table) -> Option<&'a MapTable> {
    unsafe { crate::boxed::contents::<MapTable>(table as Id, class()) }.map(|t| &*t)
}

/// `NSMapTable *NSCreateMapTable(NSMapTableKeyCallBacks, NSMapTableValueCallBacks,
/// NSUInteger capacity)`, owned by the caller.
unsafe extern "C" fn NSCreateMapTable(
    keys: KeyCallBacks,
    values: ValueCallBacks,
    capacity: usize,
) -> Id {
    crate::boxed::make(
        class(),
        MapTable {
            keys,
            values,
            buckets: Mutex::new(HashMap::with_capacity(capacity)),
        },
    )
}

/// `void NSFreeMapTable(NSMapTable *)`: every entry released, then the table.
unsafe extern "C" fn NSFreeMapTable(table: Table) {
    // SAFETY: a table or null.
    let Some(map) = (unsafe { table_of(table) }) else {
        return;
    };
    let entries: Vec<(usize, usize)> =
        std::mem::take(&mut *map.buckets.lock().unwrap_or_else(|e| e.into_inner()))
            .into_values()
            .flatten()
            .collect();
    for (key, value) in entries {
        map.release(table, Some(key), Some(value));
    }
    objc::arc::release(table as Id);
}

/// `void *NSMapGet(NSMapTable *, const void *key)`: the value, or null.
unsafe extern "C" fn NSMapGet(table: Table, key: *const c_void) -> *const c_void {
    // SAFETY: a table or null.
    let Some(map) = (unsafe { table_of(table) }) else {
        return std::ptr::null();
    };
    map.find(table, key as usize)
        .1
        .map_or(std::ptr::null(), |(_, value)| value as *const c_void)
}

/// `void NSMapInsert(NSMapTable *, const void *key, const void *value)`.
///
/// A key already present keeps its stored key and takes the new value, the
/// old value released.
unsafe extern "C" fn NSMapInsert(table: Table, key: *const c_void, value: *const c_void) {
    // SAFETY: a table or null.
    let Some(map) = (unsafe { table_of(table) }) else {
        return;
    };
    let (key, value) = (key as usize, value as usize);
    if key == map.keys.not_a_key_marker as usize {
        objc::fail!("NSInvalidArgumentException: NSMapInsert: attempt to insert notAKeyMarker");
    }
    match map.find(table, key) {
        (hash, Some((stored, old))) => {
            map.retain(table, stored, value);
            // The stored key was retained once for this entry and still is.
            map.release(table, Some(stored), Some(old));
            map.replace(hash, stored, Some((stored, value)));
        }
        (hash, None) => {
            map.retain(table, key, value);
            map.replace(hash, key, Some((key, value)));
        }
    }
}

/// `void NSMapRemove(NSMapTable *, const void *key)`.
unsafe extern "C" fn NSMapRemove(table: Table, key: *const c_void) {
    // SAFETY: a table or null.
    let Some(map) = (unsafe { table_of(table) }) else {
        return;
    };
    if let (hash, Some((stored, value))) = map.find(table, key as usize) {
        map.replace(hash, stored, None);
        map.release(table, Some(stored), Some(value));
    }
}

exports::exports! {
    functions {
        "NSCreateMapTable" => NSCreateMapTable,
        "NSFreeMapTable" => NSFreeMapTable,
        "NSMapGet" => NSMapGet,
        "NSMapInsert" => NSMapInsert,
        "NSMapRemove" => NSMapRemove,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POINTERS: KeyCallBacks = KeyCallBacks {
        hash: None,
        is_equal: None,
        retain: None,
        release: None,
        describe: None,
        not_a_key_marker: std::ptr::null(),
    };
    const VALUES: ValueCallBacks = ValueCallBacks {
        retain: None,
        release: None,
        describe: None,
    };

    #[test]
    fn a_pointer_table_inserts_replaces_and_removes() {
        crate::bootstrap();
        // SAFETY: plain pointer callbacks and a table this test owns.
        unsafe {
            let table = NSCreateMapTable(POINTERS, VALUES, 0) as Table;
            let key = 0x10 as *const c_void;
            assert!(NSMapGet(table, key).is_null());
            NSMapInsert(table, key, 0x20 as *const c_void);
            NSMapInsert(table, key, 0x30 as *const c_void);
            assert_eq!(NSMapGet(table, key) as usize, 0x30);
            NSMapRemove(table, key);
            assert!(NSMapGet(table, key).is_null());
            NSFreeMapTable(table);
        }
    }
}
