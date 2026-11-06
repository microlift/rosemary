//! The `NSDictionary` / `NSMutableDictionary` methods programs reach for when
//! merging, copying and querying dictionaries. Semantics follow Apple's
//! NSMutableDictionary and NSDictionary class references.

use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections::{self, Number};
use objc::{object::Id, sel::Sel};

fn imp(receiver: Id, selector: &str) -> (usize, Sel) {
    let sel = objc::sel::get(selector);
    // SAFETY: callers supply live objects.
    let class = unsafe { objc::object::class_of(receiver) }.expect("an object");
    (objc::class::method_implementation(class, sel) as usize, sel)
}

unsafe fn send1(receiver: Id, selector: &str, a: Id) -> Id {
    let (f, sel) = imp(receiver, selector);
    unsafe {
        std::mem::transmute::<usize, unsafe extern "C" fn(Id, Sel, Id) -> Id>(f)(receiver, sel, a)
    }
}

unsafe fn send0(receiver: Id, selector: &str) -> Id {
    let (f, sel) = imp(receiver, selector);
    unsafe { std::mem::transmute::<usize, unsafe extern "C" fn(Id, Sel) -> Id>(f)(receiver, sel) }
}

unsafe fn send2(receiver: Id, selector: &str, a: Id, b: Id) -> Id {
    let (f, sel) = imp(receiver, selector);
    unsafe {
        std::mem::transmute::<usize, unsafe extern "C" fn(Id, Sel, Id, Id) -> Id>(f)(
            receiver, sel, a, b,
        )
    }
}

unsafe fn flag1(receiver: Id, selector: &str, a: Id) -> bool {
    let (f, sel) = imp(receiver, selector);
    unsafe {
        std::mem::transmute::<usize, unsafe extern "C" fn(Id, Sel, Id) -> bool>(f)(receiver, sel, a)
    }
}

unsafe fn class_send1(class: &str, selector: &str, a: Id) -> Id {
    let class = objc::class::lookup(std::ffi::CString::new(class).unwrap().as_c_str()).unwrap();
    let sel = objc::sel::get(selector);
    let meta = class.metaclass();
    let f = objc::class::method_implementation(meta, sel) as usize;
    unsafe {
        std::mem::transmute::<usize, unsafe extern "C" fn(Id, Sel, Id) -> Id>(f)(
            objc::class::ptr(class) as Id,
            sel,
            a,
        )
    }
}

fn text(s: &str) -> Id {
    foundation::string::from_str(s)
}

fn number(n: i64) -> Id {
    collections::new_number(Number::Integer(n)) as Id
}

fn dictionary(pairs: &[(&str, i64)], mutable: bool) -> Id {
    let pairs: Vec<(CFTypeRef, CFTypeRef)> = pairs
        .iter()
        .map(|(k, v)| (text(k) as CFTypeRef, number(*v) as CFTypeRef))
        .collect();
    collections::new_dictionary(
        &pairs,
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        mutable,
    ) as Id
}

fn value(d: Id, key: &str) -> Option<i64> {
    let v = unsafe { send1(d, "objectForKey:", text(key)) };
    (!v.is_null()).then(|| {
        unsafe { collections::number(v as CFTypeRef) }
            .unwrap()
            .as_i64()
    })
}

fn count(d: Id) -> usize {
    unsafe { collections::dictionary(d as CFTypeRef) }
        .unwrap()
        .len()
}

fn array_of(items: &[Id]) -> Id {
    let items: Vec<CFTypeRef> = items.iter().map(|i| *i as CFTypeRef).collect();
    collections::new_array(&items, Policy::from_type_callbacks(), false) as Id
}

#[test]
fn dictionaries() {
    foundation::bootstrap();
    adds_entries_replacing_existing_values();
    adding_a_dictionary_to_itself_is_stable();
    adding_nil_changes_nothing();
    set_dictionary_replaces_contents();
    removes_objects_for_keys();
    dictionary_with_dictionary_copies_and_respects_receiver();
    queries();
    many_keys_build_and_look_up_quickly();
}

fn adds_entries_replacing_existing_values() {
    let target = dictionary(&[("a", 1), ("b", 2)], true);
    let source = dictionary(&[("b", 20), ("c", 30)], false);
    unsafe { send1(target, "addEntriesFromDictionary:", source) };
    assert_eq!(count(target), 3);
    assert_eq!(value(target, "a"), Some(1));
    assert_eq!(value(target, "b"), Some(20));
    assert_eq!(value(target, "c"), Some(30));
    // The source is untouched.
    assert_eq!(count(source), 2);
    assert_eq!(value(source, "b"), Some(20));
}

fn adding_a_dictionary_to_itself_is_stable() {
    let d = dictionary(&[("a", 1), ("b", 2)], true);
    unsafe { send1(d, "addEntriesFromDictionary:", d) };
    assert_eq!(count(d), 2);
    assert_eq!(value(d, "a"), Some(1));
}

fn adding_nil_changes_nothing() {
    let d = dictionary(&[("a", 1)], true);
    unsafe { send1(d, "addEntriesFromDictionary:", std::ptr::null_mut()) };
    assert_eq!(count(d), 1);
}

fn set_dictionary_replaces_contents() {
    let d = dictionary(&[("a", 1), ("b", 2)], true);
    let other = dictionary(&[("z", 26)], false);
    unsafe { send1(d, "setDictionary:", other) };
    assert_eq!(count(d), 1);
    assert_eq!(value(d, "a"), None);
    assert_eq!(value(d, "z"), Some(26));
    unsafe { send1(d, "setDictionary:", d) };
    assert_eq!(value(d, "z"), Some(26));
}

fn removes_objects_for_keys() {
    let d = dictionary(&[("a", 1), ("b", 2), ("c", 3)], true);
    let keys = array_of(&[text("a"), text("c"), text("missing")]);
    unsafe { send1(d, "removeObjectsForKeys:", keys) };
    assert_eq!(count(d), 1);
    assert_eq!(value(d, "b"), Some(2));
}

fn dictionary_with_dictionary_copies_and_respects_receiver() {
    let source = dictionary(&[("a", 1)], false);
    let plain = unsafe { class_send1("NSDictionary", "dictionaryWithDictionary:", source) };
    let growable =
        unsafe { class_send1("NSMutableDictionary", "dictionaryWithDictionary:", source) };
    assert_eq!(value(plain, "a"), Some(1));
    assert!(
        !unsafe { collections::dictionary(plain as CFTypeRef) }
            .unwrap()
            .mutable
    );
    assert!(
        unsafe { collections::dictionary(growable as CFTypeRef) }
            .unwrap()
            .mutable
    );
    unsafe { send2(growable, "setObject:forKey:", number(2), text("b")) };
    // The copy is independent of its source.
    assert_eq!(count(source), 1);
    assert_eq!(count(growable), 2);
}

fn queries() {
    let d = dictionary(&[("a", 1), ("b", 2), ("c", 1)], false);
    let keys = unsafe { send1(d, "allKeysForObject:", number(1)) };
    let keys = unsafe { collections::array(keys as CFTypeRef) }.unwrap();
    assert_eq!(keys.values.len(), 2);

    let marker = number(-1);
    let wanted = array_of(&[text("b"), text("nope")]);
    let found = unsafe { send2(d, "objectsForKeys:notFoundMarker:", wanted, marker) };
    let found = unsafe { collections::array(found as CFTypeRef) }.unwrap();
    assert_eq!(found.values[1], marker as CFTypeRef);
    let first = unsafe { collections::number(found.values[0]) }.unwrap();
    assert_eq!(first.as_i64(), 2);

    let same = dictionary(&[("c", 1), ("a", 1), ("b", 2)], true);
    let different = dictionary(&[("a", 1), ("b", 2), ("c", 9)], true);
    assert!(unsafe { flag1(d, "isEqualToDictionary:", same) });
    assert!(!unsafe { flag1(d, "isEqualToDictionary:", different) });

    let enumerator = unsafe { send0(d, "keyEnumerator") };
    let mut seen = 0;
    while !unsafe { send0(enumerator, "nextObject") }.is_null() {
        seen += 1;
    }
    assert_eq!(seen, 3);
}

/// Lookup by hash: with a scan per key, building this many would send
/// `-isEqual:` over a billion times.
fn many_keys_build_and_look_up_quickly() {
    const KEYS: i64 = 50_000;
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(1);
    let started = std::time::Instant::now();
    let d = dictionary(&[], true);
    for n in 0..KEYS {
        unsafe { send2(d, "setObject:forKey:", number(n), text(&format!("key {n}"))) };
    }
    for n in 0..KEYS {
        assert_eq!(value(d, &format!("key {n}")), Some(n));
    }
    let doomed: Vec<Id> = (0..KEYS)
        .step_by(2)
        .map(|n| text(&format!("key {n}")))
        .collect();
    unsafe { send1(d, "removeObjectsForKeys:", array_of(&doomed)) };
    let elapsed = started.elapsed();

    assert_eq!(count(d), (KEYS / 2) as usize);
    assert_eq!(value(d, "key 0"), None);
    let mut seen: Vec<i64> = unsafe { collections::dictionary(d as CFTypeRef) }
        .unwrap()
        .entries()
        .iter()
        .map(|(_, v)| unsafe { collections::number(*v) }.unwrap().as_i64())
        .collect();
    seen.sort_unstable();
    assert!(seen.iter().copied().eq((1..KEYS).step_by(2)));
    // Debug builds are too slow to say anything about the hash.
    if !cfg!(debug_assertions) {
        assert!(elapsed < BUDGET, "took {elapsed:?}");
    }
}
