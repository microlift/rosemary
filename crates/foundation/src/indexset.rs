//! `NSIndexSet` and `NSMutableIndexSet`: sets of unsigned integers kept as
//! sorted, disjoint half-open ranges, so a set of a million rows is one entry.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};
use crate::geometry::NSRange;

const NOT_FOUND: usize = isize::MAX as usize;

type Ranges = Vec<(usize, usize)>;

fn sets() -> &'static Mutex<HashMap<usize, Ranges>> {
    static SETS: OnceLock<Mutex<HashMap<usize, Ranges>>> = OnceLock::new();
    SETS.get_or_init(Default::default)
}

fn get(set: Id) -> Ranges {
    sets()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(set as usize))
        .cloned()
        .unwrap_or_default()
}

fn put(set: Id, ranges: Ranges) {
    sets()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(set as usize, ranges);
}

fn change(set: Id, f: impl FnOnce(&mut Ranges)) {
    let mut ranges = get(set);
    f(&mut ranges);
    put(set, ranges);
}

fn add_range(ranges: &mut Ranges, start: usize, end: usize) {
    if start >= end {
        return;
    }
    ranges.push((start, end));
    ranges.sort_unstable();
    let mut merged: Ranges = Vec::with_capacity(ranges.len());
    for &(s, e) in ranges.iter() {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    *ranges = merged;
}

fn remove_range(ranges: &mut Ranges, start: usize, end: usize) {
    if start >= end {
        return;
    }
    let mut kept = Ranges::with_capacity(ranges.len() + 1);
    for &(s, e) in ranges.iter() {
        if e <= start || s >= end {
            kept.push((s, e));
            continue;
        }
        if s < start {
            kept.push((s, start));
        }
        if e > end {
            kept.push((end, e));
        }
    }
    *ranges = kept;
}

fn range_end(range: NSRange) -> usize {
    range.location.saturating_add(range.length)
}

fn class(name: &std::ffi::CStr) -> &'static objc::class::Class {
    objc::class::lookup(name).expect("registered by install")
}

/// A new autoreleased instance of `class_name` holding `ranges`.
fn make(class_name: &std::ffi::CStr, ranges: Ranges) -> Id {
    let object = objc::object::create_instance(class(class_name), 0);
    put(object, ranges);
    objc::arc::autorelease(object)
}

fn class_name_of(receiver: Id) -> &'static std::ffi::CStr {
    let mutable = class(c"NSMutableIndexSet");
    // SAFETY: a live object.
    if unsafe { corefoundation::base::is_kind_of(receiver.cast(), mutable) } {
        c"NSMutableIndexSet"
    } else {
        c"NSIndexSet"
    }
}

/// Which of the two classes a class-method constructor receiver names.
fn class_name_for(receiver: Id) -> &'static std::ffi::CStr {
    if std::ptr::eq(
        receiver.cast::<objc::class::Class>(),
        class(c"NSMutableIndexSet"),
    ) {
        c"NSMutableIndexSet"
    } else {
        c"NSIndexSet"
    }
}

unsafe extern "C" fn index_set(class: Id, _sel: Sel) -> Id {
    make(class_name_for(class), Ranges::new())
}

unsafe extern "C" fn index_set_with_index(class: Id, _sel: Sel, index: usize) -> Id {
    make(
        class_name_for(class),
        vec![(index, index.saturating_add(1))],
    )
}

unsafe extern "C" fn index_set_with_range(class: Id, _sel: Sel, range: NSRange) -> Id {
    let mut ranges = Ranges::new();
    add_range(&mut ranges, range.location, range_end(range));
    make(class_name_for(class), ranges)
}

unsafe extern "C" fn init_with_index(receiver: Id, _sel: Sel, index: usize) -> Id {
    put(receiver, vec![(index, index.saturating_add(1))]);
    receiver
}

unsafe extern "C" fn init_with_range(receiver: Id, _sel: Sel, range: NSRange) -> Id {
    let mut ranges = Ranges::new();
    add_range(&mut ranges, range.location, range_end(range));
    put(receiver, ranges);
    receiver
}

unsafe extern "C" fn init_with_index_set(receiver: Id, _sel: Sel, other: Id) -> Id {
    put(receiver, get(other));
    receiver
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    sets()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(receiver as usize));
    objc::arc::finalize(receiver);
}

/// `-copy`: an immutable set is its own copy; a mutable one is snapshotted.
unsafe extern "C" fn copy_with_zone(receiver: Id, _sel: Sel, _zone: *mut std::ffi::c_void) -> Id {
    if class_name_of(receiver) == c"NSIndexSet" {
        return objc::arc::retain(receiver);
    }
    objc::arc::retain(make(c"NSIndexSet", get(receiver)))
}

unsafe extern "C" fn mutable_copy_with_zone(
    receiver: Id,
    _sel: Sel,
    _zone: *mut std::ffi::c_void,
) -> Id {
    objc::arc::retain(make(c"NSMutableIndexSet", get(receiver)))
}

unsafe extern "C" fn count(receiver: Id, _sel: Sel) -> usize {
    get(receiver).iter().map(|(s, e)| e - s).sum()
}

unsafe extern "C" fn first_index(receiver: Id, _sel: Sel) -> usize {
    get(receiver).first().map_or(NOT_FOUND, |r| r.0)
}

unsafe extern "C" fn last_index(receiver: Id, _sel: Sel) -> usize {
    get(receiver).last().map_or(NOT_FOUND, |r| r.1 - 1)
}

fn greater_or_equal(ranges: &Ranges, index: usize) -> usize {
    for &(s, e) in ranges {
        if index < s {
            return s;
        }
        if index < e {
            return index;
        }
    }
    NOT_FOUND
}

fn less_or_equal(ranges: &Ranges, index: usize) -> usize {
    for &(s, e) in ranges.iter().rev() {
        if index >= e {
            return e - 1;
        }
        if index >= s {
            return index;
        }
    }
    NOT_FOUND
}

unsafe extern "C" fn index_greater_than(receiver: Id, _sel: Sel, index: usize) -> usize {
    match index.checked_add(1) {
        Some(next) => greater_or_equal(&get(receiver), next),
        None => NOT_FOUND,
    }
}

unsafe extern "C" fn index_greater_or_equal(receiver: Id, _sel: Sel, index: usize) -> usize {
    greater_or_equal(&get(receiver), index)
}

unsafe extern "C" fn index_less_than(receiver: Id, _sel: Sel, index: usize) -> usize {
    match index.checked_sub(1) {
        Some(prev) => less_or_equal(&get(receiver), prev),
        None => NOT_FOUND,
    }
}

unsafe extern "C" fn index_less_or_equal(receiver: Id, _sel: Sel, index: usize) -> usize {
    less_or_equal(&get(receiver), index)
}

unsafe extern "C" fn contains_index(receiver: Id, _sel: Sel, index: usize) -> u8 {
    u8::from(greater_or_equal(&get(receiver), index) == index)
}

unsafe extern "C" fn contains_indexes(receiver: Id, _sel: Sel, other: Id) -> u8 {
    let mine = get(receiver);
    let all = get(other)
        .iter()
        .all(|&(s, e)| mine.iter().any(|&(ms, me)| ms <= s && e <= me));
    u8::from(all)
}

unsafe extern "C" fn intersects_range(receiver: Id, _sel: Sel, range: NSRange) -> u8 {
    let end = range_end(range);
    u8::from(
        get(receiver)
            .iter()
            .any(|&(s, e)| s < end && e > range.location),
    )
}

unsafe extern "C" fn count_in_range(receiver: Id, _sel: Sel, range: NSRange) -> usize {
    let end = range_end(range);
    get(receiver)
        .iter()
        .map(|&(s, e)| e.min(end).saturating_sub(s.max(range.location)))
        .sum()
}

unsafe extern "C" fn is_equal_to_index_set(receiver: Id, _sel: Sel, other: Id) -> u8 {
    u8::from(!other.is_null() && get(receiver) == get(other))
}

unsafe extern "C" fn is_equal(receiver: Id, sel: Sel, other: Id) -> u8 {
    // SAFETY: a live object or nil.
    let is_set = !other.is_null()
        && unsafe { corefoundation::base::is_kind_of(other.cast(), class(c"NSIndexSet")) };
    if is_set {
        // SAFETY: the same arguments.
        unsafe { is_equal_to_index_set(receiver, sel, other) }
    } else {
        0
    }
}

unsafe extern "C" fn add_index(receiver: Id, _sel: Sel, index: usize) {
    change(receiver, |r| add_range(r, index, index.saturating_add(1)));
}

unsafe extern "C" fn add_indexes(receiver: Id, _sel: Sel, other: Id) {
    let added = get(other);
    change(receiver, |r| {
        added.iter().for_each(|&(s, e)| add_range(r, s, e))
    });
}

unsafe extern "C" fn add_range_of_indexes(receiver: Id, _sel: Sel, range: NSRange) {
    change(receiver, |r| add_range(r, range.location, range_end(range)));
}

unsafe extern "C" fn remove_index(receiver: Id, _sel: Sel, index: usize) {
    change(receiver, |r| {
        remove_range(r, index, index.saturating_add(1))
    });
}

unsafe extern "C" fn remove_indexes(receiver: Id, _sel: Sel, other: Id) {
    let removed = get(other);
    change(receiver, |r| {
        removed.iter().for_each(|&(s, e)| remove_range(r, s, e))
    });
}

unsafe extern "C" fn remove_range_of_indexes(receiver: Id, _sel: Sel, range: NSRange) {
    change(receiver, |r| {
        remove_range(r, range.location, range_end(range))
    });
}

unsafe extern "C" fn remove_all_indexes(receiver: Id, _sel: Sel) {
    put(receiver, Ranges::new());
}

/// `-shiftIndexesStartingAtIndex:by:`: members at or past `index` move by
/// `delta`; any that would go below zero are dropped.
unsafe extern "C" fn shift_indexes(receiver: Id, _sel: Sel, index: usize, delta: isize) {
    let old = get(receiver);
    let mut shifted = Ranges::new();
    for (s, e) in old {
        if e <= index {
            add_range(&mut shifted, s, e);
            continue;
        }
        if s < index {
            add_range(&mut shifted, s, index);
        }
        let from = s.max(index);
        let (from, to) = if delta >= 0 {
            (
                from.saturating_add(delta as usize),
                e.saturating_add(delta as usize),
            )
        } else {
            let back = delta.unsigned_abs();
            (from.saturating_sub(back), e.saturating_sub(back))
        };
        add_range(&mut shifted, from, to);
    }
    put(receiver, shifted);
}

pub(crate) fn install() {
    let immutable = corefoundation::base::register(c"NSIndexSet", None, size_of::<usize>(), false);
    let mutable = corefoundation::base::register(
        c"NSMutableIndexSet",
        Some(immutable),
        size_of::<usize>(),
        false,
    );
    class_methods!(immutable, {
        "indexSet" => index_set, c"@16@0:8";
        "indexSetWithIndex:" => index_set_with_index, c"@24@0:8Q16";
        "indexSetWithIndexesInRange:" => index_set_with_range, c"@32@0:8{_NSRange=QQ}16";
    });
    methods!(immutable, {
        "initWithIndex:" => init_with_index, c"@24@0:8Q16";
        "initWithIndexesInRange:" => init_with_range, c"@32@0:8{_NSRange=QQ}16";
        "initWithIndexSet:" => init_with_index_set, c"@24@0:8@16";
        "dealloc" => dealloc, c"v16@0:8";
        "copyWithZone:" => copy_with_zone, c"@24@0:8^v16";
        "mutableCopyWithZone:" => mutable_copy_with_zone, c"@24@0:8^v16";
        "count" => count, c"Q16@0:8";
        "firstIndex" => first_index, c"Q16@0:8";
        "lastIndex" => last_index, c"Q16@0:8";
        "indexGreaterThanIndex:" => index_greater_than, c"Q24@0:8Q16";
        "indexGreaterThanOrEqualToIndex:" => index_greater_or_equal, c"Q24@0:8Q16";
        "indexLessThanIndex:" => index_less_than, c"Q24@0:8Q16";
        "indexLessThanOrEqualToIndex:" => index_less_or_equal, c"Q24@0:8Q16";
        "containsIndex:" => contains_index, c"c24@0:8Q16";
        "containsIndexes:" => contains_indexes, c"c24@0:8@16";
        "intersectsIndexesInRange:" => intersects_range, c"c32@0:8{_NSRange=QQ}16";
        "countOfIndexesInRange:" => count_in_range, c"Q32@0:8{_NSRange=QQ}16";
        "isEqualToIndexSet:" => is_equal_to_index_set, c"c24@0:8@16";
        "isEqual:" => is_equal, c"c24@0:8@16";
    });
    methods!(mutable, {
        "addIndex:" => add_index, c"v24@0:8Q16";
        "addIndexes:" => add_indexes, c"v24@0:8@16";
        "addIndexesInRange:" => add_range_of_indexes, c"v32@0:8{_NSRange=QQ}16";
        "removeIndex:" => remove_index, c"v24@0:8Q16";
        "removeIndexes:" => remove_indexes, c"v24@0:8@16";
        "removeIndexesInRange:" => remove_range_of_indexes, c"v32@0:8{_NSRange=QQ}16";
        "removeAllIndexes" => remove_all_indexes, c"v16@0:8";
        "shiftIndexesStartingAtIndex:by:" => shift_indexes, c"v32@0:8Q16q24";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_and_overlapping_ranges_merge() {
        let mut r = Ranges::new();
        add_range(&mut r, 5, 8);
        add_range(&mut r, 8, 10);
        add_range(&mut r, 1, 2);
        add_range(&mut r, 9, 12);
        assert_eq!(r, vec![(1, 2), (5, 12)]);
    }

    #[test]
    fn removing_the_middle_splits_a_range() {
        let mut r = vec![(0, 10)];
        remove_range(&mut r, 3, 5);
        assert_eq!(r, vec![(0, 3), (5, 10)]);
    }

    #[test]
    fn neighbours_skip_gaps() {
        let r = vec![(1, 3), (7, 9)];
        assert_eq!(greater_or_equal(&r, 3), 7);
        assert_eq!(less_or_equal(&r, 6), 2);
        assert_eq!(greater_or_equal(&r, 9), NOT_FOUND);
        assert_eq!(less_or_equal(&r, 0), NOT_FOUND);
    }
}

/// The indexes `set` holds, in order, as half-open ranges.
pub(crate) fn ranges_of(set: Id) -> Vec<(usize, usize)> {
    get(set)
}

/// A new, autoreleased immutable set of `indexes`, which need not be sorted.
pub(crate) fn from_indexes(indexes: &[usize]) -> Id {
    let mut ranges = Ranges::new();
    for &index in indexes {
        add_range(&mut ranges, index, index.saturating_add(1));
    }
    make(c"NSIndexSet", ranges)
}
