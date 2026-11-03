//! The block-based enumeration and test methods of the collections.
//!
//! Each walks a snapshot of the collection taken before the first call, as
//! Apple's do: the block may mutate what it is enumerating, which is an error
//! on a Mac, and a snapshot keeps that error from reading freed slots here.

use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

/// `NSEnumerationReverse`. `NSEnumerationConcurrent` permits, and does not
/// require, running in parallel; walking in order honours it.
const REVERSE: usize = 1 << 1;

/// `NSNotFound`.
const NOT_FOUND: usize = isize::MAX as usize;

/// A block's `invoke`, as the function type `F`.
///
/// # Safety
///
/// `block` is null or a block whose signature is `F` minus the leading
/// block pointer.
unsafe fn invoke<F: Copy>(block: Id) -> Option<F> {
    // SAFETY: the caller's obligation.
    let entry = unsafe { objc::block::invoke(block) }?;
    debug_assert_eq!(size_of::<F>(), size_of::<usize>());
    // SAFETY: `F` is a function pointer type, per the caller.
    Some(unsafe { std::mem::transmute_copy::<usize, F>(&entry) })
}

/// The indexes `0..len`, backwards under [`REVERSE`].
fn order(len: usize, options: usize) -> Box<dyn Iterator<Item = usize>> {
    match options & REVERSE {
        0 => Box::new(0..len),
        _ => Box::new((0..len).rev()),
    }
}

/// The array's elements, each retained for the walk, or `None` if the
/// receiver is not one of this crate's arrays.
fn array_snapshot(receiver: Id) -> Option<Snapshot> {
    // SAFETY: the receiver of an NSArray method.
    let array = unsafe { collections::array(receiver as CFTypeRef) }?;
    Some(Snapshot::new(array.values.clone()))
}

fn set_snapshot(receiver: Id) -> Option<Snapshot> {
    // SAFETY: the receiver of an NSSet method.
    let set = unsafe { collections::set(receiver as CFTypeRef) }?;
    Some(Snapshot::new(set.values.clone()))
}

/// Objects held for the length of an enumeration.
struct Snapshot(Vec<CFTypeRef>);

impl Snapshot {
    fn new(values: Vec<CFTypeRef>) -> Snapshot {
        for value in &values {
            objc::arc::retain(*value as Id);
        }
        Snapshot(values)
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        for value in &self.0 {
            objc::arc::release(*value as Id);
        }
    }
}

type ObjectIndexStop = unsafe extern "C" fn(Id, Id, usize, *mut u8);
type ObjectIndexStopTest = unsafe extern "C" fn(Id, Id, usize, *mut u8) -> u8;
type KeyObjectStop = unsafe extern "C" fn(Id, Id, Id, *mut u8);
type KeyObjectStopTest = unsafe extern "C" fn(Id, Id, Id, *mut u8) -> u8;
type ObjectStop = unsafe extern "C" fn(Id, Id, *mut u8);
type ObjectStopTest = unsafe extern "C" fn(Id, Id, *mut u8) -> u8;
type IndexStop = unsafe extern "C" fn(Id, usize, *mut u8);
type RangeStop = unsafe extern "C" fn(Id, crate::geometry::NSRange, *mut u8);

fn autoreleased_array(values: &[CFTypeRef]) -> Id {
    let array = collections::new_array(values, Policy::from_type_callbacks(), false);
    objc::arc::autorelease(array as Id)
}

fn autoreleased_set(values: &[CFTypeRef]) -> Id {
    let set = collections::new_set(values, Policy::from_type_callbacks());
    objc::arc::autorelease(set as Id)
}

// NSArray

unsafe extern "C" fn array_enumerate_with_options(
    receiver: Id,
    _sel: Sel,
    options: usize,
    block: Id,
) {
    let (Some(values), Some(call)) = (array_snapshot(receiver), unsafe {
        invoke::<ObjectIndexStop>(block)
    }) else {
        return;
    };
    let mut stop = 0u8;
    for at in order(values.0.len(), options) {
        // SAFETY: `void (^)(id, NSUInteger, BOOL *)`.
        unsafe { call(block, values.0[at] as Id, at, &mut stop) };
        if stop != 0 {
            break;
        }
    }
}

unsafe extern "C" fn array_enumerate(receiver: Id, sel: Sel, block: Id) {
    unsafe { array_enumerate_with_options(receiver, sel, 0, block) };
}

unsafe extern "C" fn array_enumerate_at_indexes(
    receiver: Id,
    _sel: Sel,
    indexes: Id,
    options: usize,
    block: Id,
) {
    let (Some(values), Some(call)) = (array_snapshot(receiver), unsafe {
        invoke::<ObjectIndexStop>(block)
    }) else {
        return;
    };
    let mut wanted: Vec<usize> = crate::indexset::ranges_of(indexes)
        .into_iter()
        .flat_map(|(start, end)| start..end)
        .take_while(|&at| at < values.0.len())
        .collect();
    if options & REVERSE != 0 {
        wanted.reverse();
    }
    let mut stop = 0u8;
    for at in wanted {
        // SAFETY: as above.
        unsafe { call(block, values.0[at] as Id, at, &mut stop) };
        if stop != 0 {
            break;
        }
    }
}

/// The indexes whose objects pass, all of them unless `first_only`.
unsafe fn array_passing(receiver: Id, options: usize, block: Id, first_only: bool) -> Vec<usize> {
    let (Some(values), Some(call)) = (array_snapshot(receiver), unsafe {
        invoke::<ObjectIndexStopTest>(block)
    }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stop = 0u8;
    for at in order(values.0.len(), options) {
        // SAFETY: `BOOL (^)(id, NSUInteger, BOOL *)`.
        if unsafe { call(block, values.0[at] as Id, at, &mut stop) } != 0 {
            out.push(at);
            if first_only {
                break;
            }
        }
        if stop != 0 {
            break;
        }
    }
    out
}

unsafe extern "C" fn index_of_object_passing_test(receiver: Id, _sel: Sel, block: Id) -> usize {
    unsafe { array_passing(receiver, 0, block, true) }
        .first()
        .copied()
        .unwrap_or(NOT_FOUND)
}

unsafe extern "C" fn indexes_of_objects_with_options_passing_test(
    receiver: Id,
    _sel: Sel,
    options: usize,
    block: Id,
) -> Id {
    crate::indexset::from_indexes(&unsafe { array_passing(receiver, options, block, false) })
}

unsafe extern "C" fn indexes_of_objects_passing_test(receiver: Id, sel: Sel, block: Id) -> Id {
    unsafe { indexes_of_objects_with_options_passing_test(receiver, sel, 0, block) }
}

// NSDictionary

fn dictionary_snapshot(receiver: Id) -> Option<(Snapshot, Snapshot)> {
    // SAFETY: the receiver of an NSDictionary method.
    let dictionary = unsafe { collections::dictionary(receiver as CFTypeRef) }?;
    let (keys, values) = dictionary.entries.iter().copied().unzip();
    Some((Snapshot::new(keys), Snapshot::new(values)))
}

unsafe extern "C" fn dictionary_enumerate_with_options(
    receiver: Id,
    _sel: Sel,
    options: usize,
    block: Id,
) {
    let (Some((keys, values)), Some(call)) = (dictionary_snapshot(receiver), unsafe {
        invoke::<KeyObjectStop>(block)
    }) else {
        return;
    };
    let mut stop = 0u8;
    for at in order(keys.0.len(), options) {
        // SAFETY: `void (^)(id key, id obj, BOOL *)`.
        unsafe { call(block, keys.0[at] as Id, values.0[at] as Id, &mut stop) };
        if stop != 0 {
            break;
        }
    }
}

unsafe extern "C" fn dictionary_enumerate(receiver: Id, sel: Sel, block: Id) {
    unsafe { dictionary_enumerate_with_options(receiver, sel, 0, block) };
}

unsafe extern "C" fn keys_of_entries_with_options_passing_test(
    receiver: Id,
    _sel: Sel,
    options: usize,
    block: Id,
) -> Id {
    let (Some((keys, values)), Some(call)) = (dictionary_snapshot(receiver), unsafe {
        invoke::<KeyObjectStopTest>(block)
    }) else {
        return autoreleased_set(&[]);
    };
    let mut passed = Vec::new();
    let mut stop = 0u8;
    for at in order(keys.0.len(), options) {
        // SAFETY: `BOOL (^)(id key, id obj, BOOL *)`.
        if unsafe { call(block, keys.0[at] as Id, values.0[at] as Id, &mut stop) } != 0 {
            passed.push(keys.0[at]);
        }
        if stop != 0 {
            break;
        }
    }
    autoreleased_set(&passed)
}

unsafe extern "C" fn keys_of_entries_passing_test(receiver: Id, sel: Sel, block: Id) -> Id {
    unsafe { keys_of_entries_with_options_passing_test(receiver, sel, 0, block) }
}

// NSSet

unsafe extern "C" fn set_enumerate_with_options(
    receiver: Id,
    _sel: Sel,
    _options: usize,
    block: Id,
) {
    let (Some(values), Some(call)) = (set_snapshot(receiver), unsafe {
        invoke::<ObjectStop>(block)
    }) else {
        return;
    };
    let mut stop = 0u8;
    for value in &values.0 {
        // SAFETY: `void (^)(id, BOOL *)`.
        unsafe { call(block, *value as Id, &mut stop) };
        if stop != 0 {
            break;
        }
    }
}

unsafe extern "C" fn set_enumerate(receiver: Id, sel: Sel, block: Id) {
    unsafe { set_enumerate_with_options(receiver, sel, 0, block) };
}

unsafe extern "C" fn objects_with_options_passing_test(
    receiver: Id,
    _sel: Sel,
    _options: usize,
    block: Id,
) -> Id {
    let (Some(values), Some(call)) = (set_snapshot(receiver), unsafe {
        invoke::<ObjectStopTest>(block)
    }) else {
        return autoreleased_set(&[]);
    };
    let mut passed = Vec::new();
    let mut stop = 0u8;
    for value in &values.0 {
        // SAFETY: `BOOL (^)(id, BOOL *)`.
        if unsafe { call(block, *value as Id, &mut stop) } != 0 {
            passed.push(*value);
        }
        if stop != 0 {
            break;
        }
    }
    autoreleased_set(&passed)
}

unsafe extern "C" fn objects_passing_test(receiver: Id, sel: Sel, block: Id) -> Id {
    unsafe { objects_with_options_passing_test(receiver, sel, 0, block) }
}

// NSIndexSet

unsafe extern "C" fn index_set_enumerate_with_options(
    receiver: Id,
    _sel: Sel,
    options: usize,
    block: Id,
) {
    let Some(call) = (unsafe { invoke::<IndexStop>(block) }) else {
        return;
    };
    let mut indexes: Vec<usize> = crate::indexset::ranges_of(receiver)
        .into_iter()
        .flat_map(|(start, end)| start..end)
        .collect();
    if options & REVERSE != 0 {
        indexes.reverse();
    }
    let mut stop = 0u8;
    for index in indexes {
        // SAFETY: `void (^)(NSUInteger, BOOL *)`.
        unsafe { call(block, index, &mut stop) };
        if stop != 0 {
            break;
        }
    }
}

unsafe extern "C" fn index_set_enumerate(receiver: Id, sel: Sel, block: Id) {
    unsafe { index_set_enumerate_with_options(receiver, sel, 0, block) };
}

unsafe extern "C" fn index_set_enumerate_ranges_with_options(
    receiver: Id,
    _sel: Sel,
    options: usize,
    block: Id,
) {
    let Some(call) = (unsafe { invoke::<RangeStop>(block) }) else {
        return;
    };
    let mut ranges = crate::indexset::ranges_of(receiver);
    if options & REVERSE != 0 {
        ranges.reverse();
    }
    let mut stop = 0u8;
    for (start, end) in ranges {
        let range = crate::geometry::NSRange {
            location: start,
            length: end - start,
        };
        // SAFETY: `void (^)(NSRange, BOOL *)`.
        unsafe { call(block, range, &mut stop) };
        if stop != 0 {
            break;
        }
    }
}

unsafe extern "C" fn index_set_enumerate_ranges(receiver: Id, sel: Sel, block: Id) {
    unsafe { index_set_enumerate_ranges_with_options(receiver, sel, 0, block) };
}

/// `-[NSData enumerateByteRangesUsingBlock:]`: the bytes are one range.
unsafe extern "C" fn data_enumerate_byte_ranges(receiver: Id, _sel: Sel, block: Id) {
    type Bytes = unsafe extern "C" fn(Id, *const u8, crate::geometry::NSRange, *mut u8);
    let Some(call) = (unsafe { invoke::<Bytes>(block) }) else {
        return;
    };
    // SAFETY: `-bytes` and `-length`, which every NSData answers.
    let (bytes, length) = unsafe {
        let bytes: unsafe extern "C" fn(Id, Sel) -> *const u8 =
            std::mem::transmute(objc::msg::entry());
        let length: unsafe extern "C" fn(Id, Sel) -> usize =
            std::mem::transmute(objc::msg::entry());
        (
            bytes(receiver, objc::sel::get("bytes")),
            length(receiver, objc::sel::get("length")),
        )
    };
    let mut stop = 0u8;
    // SAFETY: `void (^)(const void *, NSRange, BOOL *)`.
    unsafe {
        call(
            block,
            bytes,
            crate::geometry::NSRange {
                location: 0,
                length,
            },
            &mut stop,
        )
    };
}

/// `-objectsAtIndexes:`.
unsafe extern "C" fn objects_at_indexes(receiver: Id, _sel: Sel, indexes: Id) -> Id {
    let Some(values) = array_snapshot(receiver) else {
        return std::ptr::null_mut();
    };
    let picked: Vec<CFTypeRef> = crate::indexset::ranges_of(indexes)
        .into_iter()
        .flat_map(|(start, end)| start..end)
        .filter_map(|at| values.0.get(at).copied())
        .collect();
    autoreleased_array(&picked)
}

pub(crate) fn install() {
    let array = objc::class::lookup(c"NSArray").expect("NSArray");
    methods!(array, {
        "enumerateObjectsUsingBlock:" => array_enumerate, c"v24@0:8@?16";
        "enumerateObjectsWithOptions:usingBlock:" => array_enumerate_with_options, c"v32@0:8Q16@?24";
        "enumerateObjectsAtIndexes:options:usingBlock:" => array_enumerate_at_indexes, c"v40@0:8@16Q24@?32";
        "indexOfObjectPassingTest:" => index_of_object_passing_test, c"Q24@0:8@?16";
        "indexesOfObjectsPassingTest:" => indexes_of_objects_passing_test, c"@24@0:8@?16";
        "indexesOfObjectsWithOptions:passingTest:" => indexes_of_objects_with_options_passing_test, c"@32@0:8Q16@?24";
        "objectsAtIndexes:" => objects_at_indexes, c"@24@0:8@16";
    });
    let dictionary = objc::class::lookup(c"NSDictionary").expect("NSDictionary");
    methods!(dictionary, {
        "enumerateKeysAndObjectsUsingBlock:" => dictionary_enumerate, c"v24@0:8@?16";
        "enumerateKeysAndObjectsWithOptions:usingBlock:" => dictionary_enumerate_with_options, c"v32@0:8Q16@?24";
        "keysOfEntriesPassingTest:" => keys_of_entries_passing_test, c"@24@0:8@?16";
        "keysOfEntriesWithOptions:passingTest:" => keys_of_entries_with_options_passing_test, c"@32@0:8Q16@?24";
    });
    let set = objc::class::lookup(c"NSSet").expect("NSSet");
    methods!(set, {
        "enumerateObjectsUsingBlock:" => set_enumerate, c"v24@0:8@?16";
        "enumerateObjectsWithOptions:usingBlock:" => set_enumerate_with_options, c"v32@0:8Q16@?24";
        "objectsPassingTest:" => objects_passing_test, c"@24@0:8@?16";
        "objectsWithOptions:passingTest:" => objects_with_options_passing_test, c"@32@0:8Q16@?24";
    });
    let data = objc::class::lookup(c"NSData").expect("NSData");
    methods!(data, {
        "enumerateByteRangesUsingBlock:" => data_enumerate_byte_ranges, c"v24@0:8@?16";
    });
    let index_set = objc::class::lookup(c"NSIndexSet").expect("NSIndexSet");
    methods!(index_set, {
        "enumerateIndexesUsingBlock:" => index_set_enumerate, c"v24@0:8@?16";
        "enumerateIndexesWithOptions:usingBlock:" => index_set_enumerate_with_options, c"v32@0:8Q16@?24";
        "enumerateRangesUsingBlock:" => index_set_enumerate_ranges, c"v24@0:8@?16";
        "enumerateRangesWithOptions:usingBlock:" => index_set_enumerate_ranges_with_options, c"v32@0:8Q16@?24";
    });
}
