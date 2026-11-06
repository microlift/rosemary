//! `<strhash.h>`: the old FreeBSD string-keyed chained hash table libSystem
//! still exports. glibc has nothing like it, so it is answered here.
//!
//! The two structures are public in the header and a caller may walk them by
//! hand, so they are laid out exactly as Darwin's, and every block comes from
//! the allocator the guest frees with.

use std::ffi::{CStr, c_char, c_int, c_void};

/// Buckets used when `hash_create` is asked for zero.
const HASH_SZ: c_int = 97;

/// `hash_node`.
#[repr(C)]
struct Node {
    key: *mut c_char,
    data: *mut c_void,
    next: *mut Node,
}

/// `hash_table`.
#[repr(C)]
struct Table {
    size: c_int,
    buckets: *mut *mut Node,
}

type Replace = unsafe extern "C" fn(*mut c_void);
type KeyData = unsafe extern "C" fn(*mut c_char, *mut c_void);
type Visit = unsafe extern "C" fn(*mut c_char, *mut c_void, *mut c_void) -> c_int;

/// The bucket `key` falls in. The same function as libc's, so a table built
/// here chains its keys where a caller reading the buckets expects them.
///
/// # Safety
///
/// `key` is a NUL-terminated C string.
unsafe fn bucket(size: c_int, key: *const c_char) -> usize {
    let mut h: u32 = 0;
    for &byte in unsafe { CStr::from_ptr(key) }.to_bytes() {
        h = (h << 1) ^ (h ^ u32::from(byte));
    }
    (h % size.max(1) as u32) as usize
}

/// The chain `key` hashes to, or `None` for a table or key that is null.
///
/// # Safety
///
/// `table` is null or a table [`hash_create`] made; `key` is null or a C string.
unsafe fn chain<'a>(table: *mut Table, key: *const c_char) -> Option<&'a mut *mut Node> {
    if table.is_null() || key.is_null() {
        return None;
    }
    let table = unsafe { &mut *table };
    if table.buckets.is_null() || table.size <= 0 {
        return None;
    }
    let index = unsafe { bucket(table.size, key) };
    Some(unsafe { &mut *table.buckets.add(index) })
}

/// # Safety
///
/// `a` and `b` are NUL-terminated C strings.
unsafe fn same(a: *const c_char, b: *const c_char) -> bool {
    unsafe { libc::strcmp(a, b) == 0 }
}

/// `hash_table *hash_create(int size)`.
pub unsafe extern "C" fn hash_create(size: c_int) -> *mut c_void {
    if size < 0 {
        return std::ptr::null_mut();
    }
    let size = if size == 0 { HASH_SZ } else { size };
    let table = unsafe { crate::heap::malloc(size_of::<Table>()) }.cast::<Table>();
    if table.is_null() {
        return std::ptr::null_mut();
    }
    let bytes = size as usize * size_of::<*mut Node>();
    let buckets = unsafe { crate::heap::malloc(bytes) }.cast::<*mut Node>();
    if buckets.is_null() {
        unsafe { crate::heap::free(table.cast()) };
        return std::ptr::null_mut();
    }
    // SAFETY: both blocks were just allocated at these sizes.
    unsafe {
        std::ptr::write_bytes(buckets, 0, size as usize);
        table.write(Table { size, buckets });
    }
    table.cast()
}

/// `void *hash_search(hash_table *, char *key, void *datum, void (*replace)(void *))`.
///
/// Four answers, as libc gives them: a plain lookup returns the data; with a
/// replace function an existing entry's old data is handed to it and the datum
/// takes its place (returning null); a missing key with a datum is inserted at
/// the head of its chain and the new *node* is returned; anything else is null.
pub unsafe extern "C" fn hash_search(
    table: *mut c_void,
    key: *mut c_char,
    datum: *mut c_void,
    replace: Option<Replace>,
) -> *mut c_void {
    let Some(head) = (unsafe { chain(table.cast(), key) }) else {
        return std::ptr::null_mut();
    };
    let mut node = *head;
    while !node.is_null() {
        // SAFETY: every node in a chain was made by this module.
        let n = unsafe { &mut *node };
        if unsafe { same(n.key, key) } {
            let Some(replace) = replace else {
                return n.data;
            };
            unsafe { replace(n.data) };
            n.data = datum;
            return std::ptr::null_mut();
        }
        node = n.next;
    }
    if datum.is_null() {
        return std::ptr::null_mut();
    }
    let node = unsafe { crate::heap::malloc(size_of::<Node>()) }.cast::<Node>();
    if node.is_null() {
        return std::ptr::null_mut();
    }
    let length = unsafe { libc::strlen(key) } + 1;
    let copy = unsafe { crate::heap::malloc(length) }.cast::<c_char>();
    if copy.is_null() {
        unsafe { crate::heap::free(node.cast()) };
        return std::ptr::null_mut();
    }
    // SAFETY: `copy` is `length` bytes, which is the key with its NUL.
    unsafe {
        std::ptr::copy_nonoverlapping(key, copy, length);
        node.write(Node {
            key: copy,
            data: datum,
            next: *head,
        });
    }
    *head = node;
    node.cast()
}

/// `void hash_destroy(hash_table *, char *key, void (*nuke)(char *, void *))`:
/// remove one key, handing its key and data to `nuke` to dispose of.
///
/// libc frees the node but not the key copy it made, leaving that to `nuke`;
/// the same here, because a `nuke` that frees the key would otherwise free it
/// twice.
pub unsafe extern "C" fn hash_destroy(table: *mut c_void, key: *mut c_char, nuke: Option<KeyData>) {
    let Some(mut link) = (unsafe { chain(table.cast(), key) }) else {
        return;
    };
    while !link.is_null() {
        let node = *link;
        // SAFETY: a node this module made.
        let n = unsafe { &mut *node };
        if unsafe { same(n.key, key) } {
            *link = n.next;
            if let Some(nuke) = nuke {
                unsafe { nuke(n.key, n.data) };
            }
            unsafe { crate::heap::free(node.cast()) };
            return;
        }
        link = &mut n.next;
    }
}

/// `void hash_traverse(hash_table *, int (*func)(char *, void *, void *), void *arg)`:
/// visit every entry, bucket by bucket, until `func` returns zero.
pub unsafe extern "C" fn hash_traverse(table: *mut c_void, visit: Option<Visit>, arg: *mut c_void) {
    let (Some(visit), Some(table)) = (visit, unsafe { table.cast::<Table>().as_ref() }) else {
        return;
    };
    for index in 0..table.size.max(0) as usize {
        let mut node = unsafe { *table.buckets.add(index) };
        while !node.is_null() {
            // SAFETY: a node this module made.
            let n = unsafe { &*node };
            if unsafe { visit(n.key, n.data, arg) } == 0 {
                return;
            }
            node = n.next;
        }
    }
}

/// `void hash_purge(hash_table *, void (*purge)(char *, void *))`: empty the
/// table, handing every entry to `purge` and freeing its node.
pub unsafe extern "C" fn hash_purge(table: *mut c_void, purge: Option<KeyData>) {
    let Some(table) = (unsafe { table.cast::<Table>().as_ref() }) else {
        return;
    };
    for index in 0..table.size.max(0) as usize {
        // SAFETY: the bucket array is `size` long.
        let slot = unsafe { &mut *table.buckets.add(index) };
        let mut node = std::mem::replace(slot, std::ptr::null_mut());
        while !node.is_null() {
            let (key, data, next) = unsafe { ((*node).key, (*node).data, (*node).next) };
            if let Some(purge) = purge {
                unsafe { purge(key, data) };
            }
            unsafe { crate::heap::free(node.cast()) };
            node = next;
        }
    }
}

exports::exports! {
    functions {
        "hash_create" => hash_create,
        "hash_search" => hash_search,
        "hash_destroy" => hash_destroy,
        "hash_traverse" => hash_traverse,
        "hash_purge" => hash_purge,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn search(table: *mut c_void, key: &CStr, datum: usize) -> usize {
        // SAFETY: a table from `hash_create` and a live C string.
        unsafe { hash_search(table, key.as_ptr().cast_mut(), datum as *mut c_void, None) as usize }
    }

    #[test]
    fn a_new_table_has_the_default_size_and_empty_buckets() {
        // SAFETY: the table is read as the layout this module wrote.
        unsafe {
            let table = hash_create(0).cast::<Table>();
            assert_eq!((*table).size, HASH_SZ);
            for i in 0..HASH_SZ as usize {
                assert!((*(*table).buckets.add(i)).is_null());
            }
            assert!(hash_create(-1).is_null());
        }
    }

    #[test]
    fn an_insert_returns_the_node_and_a_lookup_returns_the_data() {
        let table = unsafe { hash_create(7) };
        let node = search(table, c"alpha", 0x10);
        assert_ne!(node, 0);
        // SAFETY: the node this module returned, and its own key copy.
        unsafe {
            let node = &*(node as *const Node);
            assert_eq!(CStr::from_ptr(node.key), c"alpha");
            assert_eq!(node.data as usize, 0x10);
        }
        assert_eq!(search(table, c"alpha", 0), 0x10);
        assert_eq!(search(table, c"beta", 0), 0, "a miss with no datum");
        assert_eq!(search(table, c"beta", 0), 0, "and it inserted nothing");
    }

    static REPLACED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

    unsafe extern "C" fn note_replaced(data: *mut c_void) {
        REPLACED.lock().unwrap().push(data as usize);
    }

    #[test]
    fn a_replace_hands_over_the_old_data_and_installs_the_new() {
        let table = unsafe { hash_create(3) };
        search(table, c"key", 1);
        // SAFETY: as above, with a host callback.
        let result = unsafe {
            hash_search(
                table,
                c"key".as_ptr().cast_mut(),
                2 as *mut c_void,
                Some(note_replaced),
            )
        };
        assert!(result.is_null());
        assert_eq!(*REPLACED.lock().unwrap(), [1]);
        assert_eq!(search(table, c"key", 0), 2);
    }

    unsafe extern "C" fn count(_key: *mut c_char, data: *mut c_void, arg: *mut c_void) -> c_int {
        let total = unsafe { &mut *(arg as *mut usize) };
        *total += data as usize;
        // Stop once the running total passes 100, to prove a zero ends the walk.
        (*total < 100) as c_int
    }

    #[test]
    fn a_traversal_visits_everything_and_stops_on_zero() {
        let table = unsafe { hash_create(1) };
        for (i, key) in [c"a", c"b", c"c"].into_iter().enumerate() {
            search(table, key, i + 1);
        }
        let mut total = 0usize;
        unsafe { hash_traverse(table, Some(count), (&mut total as *mut usize).cast()) };
        assert_eq!(total, 6);
        search(table, c"big", 200);
        let mut total = 0usize;
        unsafe { hash_traverse(table, Some(count), (&mut total as *mut usize).cast()) };
        // `big` is at the head of the only chain, so nothing after it runs.
        assert_eq!(total, 200);
    }

    static NUKED: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());

    unsafe extern "C" fn nuke(key: *mut c_char, data: *mut c_void) {
        let key = unsafe { CStr::from_ptr(key) }
            .to_string_lossy()
            .into_owned();
        NUKED.lock().unwrap().push((key, data as usize));
    }

    #[test]
    fn destroy_removes_one_key_and_purge_removes_the_rest() {
        let table = unsafe { hash_create(1) };
        for (i, key) in [c"one", c"two", c"three"].into_iter().enumerate() {
            search(table, key, i + 1);
        }
        unsafe { hash_destroy(table, c"two".as_ptr().cast_mut(), Some(nuke)) };
        assert_eq!(search(table, c"two", 0), 0);
        assert_eq!(search(table, c"one", 0), 1);
        unsafe { hash_purge(table, Some(nuke)) };
        assert_eq!(search(table, c"one", 0), 0);
        assert_eq!(search(table, c"three", 0), 0);
        let mut nuked = NUKED.lock().unwrap().clone();
        nuked.sort();
        assert_eq!(
            nuked,
            [("one".into(), 1), ("three".into(), 3), ("two".into(), 2)]
        );
    }

    #[test]
    fn keys_chain_into_the_bucket_libc_would_choose() {
        // h = (h << 1) ^ (h ^ c): 'a' = 97 gives 97, then 'b' = 98 gives
        // (194 ^ (97 ^ 98)) = 193; 193 % 10 = 3.
        assert_eq!(unsafe { bucket(10, c"ab".as_ptr()) }, 3);
    }
}
