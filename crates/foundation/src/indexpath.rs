//! `NSIndexPath`: an immutable list of indexes naming a node in a tree, as
//! `NSTreeController` and `NSOutlineView` address their items.

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};
use crate::geometry::NSRange;

#[repr(C)]
struct IndexPath {
    isa: *const std::ffi::c_void,
    /// A leaked `Box<[usize]>`, freed by `-dealloc`.
    indexes: *mut usize,
    length: usize,
}

/// `NSNotFound`, which `-indexAtPosition:` answers past the end.
const NOT_FOUND: usize = isize::MAX as usize;

fn class() -> &'static objc::class::Class {
    objc::class::lookup(c"NSIndexPath").expect("registered by install")
}

fn state(path: Id) -> &'static IndexPath {
    // SAFETY: an `NSIndexPath` this module allocated.
    unsafe { &*(path as *const IndexPath) }
}

fn store(path: Id, indexes: &[usize]) {
    let boxed: Box<[usize]> = indexes.into();
    let length = boxed.len();
    // SAFETY: an `NSIndexPath` this module allocated, still empty.
    unsafe {
        let path = &mut *(path as *mut IndexPath);
        path.indexes = Box::into_raw(boxed) as *mut usize;
        path.length = length;
    }
}

/// The indexes `path` holds, from the root down.
pub fn indexes(path: Id) -> &'static [usize] {
    let path = state(path);
    match path.indexes.is_null() {
        true => &[],
        // SAFETY: `length` indexes, owned by the path.
        false => unsafe { std::slice::from_raw_parts(path.indexes, path.length) },
    }
}

/// A new, autoreleased path holding `indexes`.
pub fn from_indexes(indexes: &[usize]) -> Id {
    let path = objc::object::create_instance(class(), 0);
    store(path, indexes);
    objc::arc::autorelease(path)
}

fn is_index_path(object: Id) -> bool {
    // SAFETY: a live object or nil.
    !object.is_null() && unsafe { corefoundation::base::is_kind_of(object.cast(), class()) }
}

unsafe extern "C" fn index_path_with_index(_class: Id, _sel: Sel, index: usize) -> Id {
    from_indexes(&[index])
}

unsafe extern "C" fn index_path_with_indexes(
    _class: Id,
    _sel: Sel,
    indexes: *const usize,
    length: usize,
) -> Id {
    match length {
        0 => from_indexes(&[]),
        // SAFETY: `length` indexes, by the caller.
        _ => from_indexes(unsafe { std::slice::from_raw_parts(indexes, length) }),
    }
}

unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    store(receiver, &[]);
    receiver
}

unsafe extern "C" fn init_with_index(receiver: Id, _sel: Sel, index: usize) -> Id {
    store(receiver, &[index]);
    receiver
}

unsafe extern "C" fn init_with_indexes(
    receiver: Id,
    _sel: Sel,
    indexes: *const usize,
    length: usize,
) -> Id {
    match length {
        0 => store(receiver, &[]),
        // SAFETY: `length` indexes, by the caller.
        _ => store(receiver, unsafe {
            std::slice::from_raw_parts(indexes, length)
        }),
    }
    receiver
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let path = state(receiver);
    if !path.indexes.is_null() {
        // SAFETY: the box `store` leaked.
        drop(unsafe {
            Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                path.indexes,
                path.length,
            ))
        });
    }
    objc::arc::finalize(receiver);
}

/// `-copy`: the path is immutable, so it is its own copy.
unsafe extern "C" fn copy_with_zone(receiver: Id, _sel: Sel, _zone: *mut std::ffi::c_void) -> Id {
    objc::arc::retain(receiver)
}

unsafe extern "C" fn length(receiver: Id, _sel: Sel) -> usize {
    indexes(receiver).len()
}

unsafe extern "C" fn index_at_position(receiver: Id, _sel: Sel, position: usize) -> usize {
    indexes(receiver)
        .get(position)
        .copied()
        .unwrap_or(NOT_FOUND)
}

unsafe extern "C" fn by_adding_index(receiver: Id, _sel: Sel, index: usize) -> Id {
    let mut longer = indexes(receiver).to_vec();
    longer.push(index);
    from_indexes(&longer)
}

unsafe extern "C" fn by_removing_last_index(receiver: Id, _sel: Sel) -> Id {
    let all = indexes(receiver);
    from_indexes(&all[..all.len().saturating_sub(1)])
}

unsafe extern "C" fn get_indexes_range(receiver: Id, _sel: Sel, out: *mut usize, range: NSRange) {
    let all = indexes(receiver);
    let end = range.location.saturating_add(range.length);
    if end > all.len() {
        objc::fail!(
            "-[NSIndexPath getIndexes:range:]: range {{{}, {}}} exceeds length {}",
            range.location,
            range.length,
            all.len()
        );
    }
    // SAFETY: room for `range.length` indexes, by the caller.
    unsafe { std::ptr::copy_nonoverlapping(all[range.location..].as_ptr(), out, range.length) };
}

unsafe extern "C" fn get_indexes(receiver: Id, _sel: Sel, out: *mut usize) {
    let all = indexes(receiver);
    // SAFETY: room for `-length` indexes, by the caller.
    unsafe { std::ptr::copy_nonoverlapping(all.as_ptr(), out, all.len()) };
}

/// `-compare:`: index by index, a path ordering before any it is a prefix of.
unsafe extern "C" fn compare(receiver: Id, _sel: Sel, other: Id) -> isize {
    // `NSComparisonResult` is `Ordering`'s -1, 0 and 1.
    indexes(receiver).cmp(indexes(other)) as isize
}

unsafe extern "C" fn is_equal(receiver: Id, _sel: Sel, other: Id) -> u8 {
    (is_index_path(other) && indexes(receiver) == indexes(other)) as u8
}

unsafe extern "C" fn hash(receiver: Id, _sel: Sel) -> usize {
    indexes(receiver)
        .iter()
        .fold(indexes(receiver).len(), |hash, &index| {
            hash.wrapping_mul(31).wrapping_add(index)
        })
}

unsafe extern "C" fn description(receiver: Id, _sel: Sel) -> Id {
    let all = indexes(receiver);
    let path = all
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(" - ");
    let text = format!(
        "<NSIndexPath: {receiver:p}> {{length = {}, path = {path}}}",
        all.len()
    );
    objc::arc::autorelease(crate::string::from_str(&text))
}

pub(crate) fn install() {
    let class = corefoundation::base::register(c"NSIndexPath", None, size_of::<IndexPath>(), false);
    class_methods!(class, {
        "indexPathWithIndex:" => index_path_with_index, c"@24@0:8Q16";
        "indexPathWithIndexes:length:" => index_path_with_indexes, c"@32@0:8^Q16Q24";
    });
    methods!(class, {
        "init" => init, c"@16@0:8";
        "initWithIndex:" => init_with_index, c"@24@0:8Q16";
        "initWithIndexes:length:" => init_with_indexes, c"@32@0:8^Q16Q24";
        "dealloc" => dealloc, c"v16@0:8";
        "copyWithZone:" => copy_with_zone, c"@24@0:8^v16";
        "length" => length, c"Q16@0:8";
        "indexAtPosition:" => index_at_position, c"Q24@0:8Q16";
        "indexPathByAddingIndex:" => by_adding_index, c"@24@0:8Q16";
        "indexPathByRemovingLastIndex" => by_removing_last_index, c"@16@0:8";
        "getIndexes:range:" => get_indexes_range, c"v40@0:8^Q16{_NSRange=QQ}24";
        "getIndexes:" => get_indexes, c"v24@0:8^Q16";
        "compare:" => compare, c"q24@0:8@16";
        "isEqual:" => is_equal, c"c24@0:8@16";
        "hash" => hash, c"Q16@0:8";
        "description" => description, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send<R>(receiver: Id, selector: &str) -> R {
        // SAFETY: each test names a selector returning `R` with no arguments.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> R = std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get(selector))
        }
    }

    fn compare_paths(a: &[usize], b: &[usize]) -> isize {
        crate::bootstrap();
        let (a, b) = (from_indexes(a), from_indexes(b));
        // SAFETY: `-compare:` is `NSComparisonResult (id, SEL, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> isize =
                std::mem::transmute(objc::msg::entry());
            f(a, objc::sel::get("compare:"), b)
        }
    }

    #[test]
    fn a_prefix_orders_before_the_paths_it_begins() {
        assert_eq!(compare_paths(&[1], &[1, 0]), -1);
        assert_eq!(compare_paths(&[1, 2], &[1, 2]), 0);
        assert_eq!(compare_paths(&[2], &[1, 9]), 1);
    }

    #[test]
    fn removing_the_last_index_names_the_parent() {
        crate::bootstrap();
        let parent: Id = send(from_indexes(&[3, 1, 4]), "indexPathByRemovingLastIndex");
        assert_eq!(indexes(parent), &[3, 1]);
        assert_eq!(send::<usize>(parent, "length"), 2);
    }

    #[test]
    fn a_position_past_the_end_is_not_found() {
        crate::bootstrap();
        let path = from_indexes(&[7]);
        // SAFETY: `-indexAtPosition:` is `NSUInteger (id, SEL, NSUInteger)`.
        let at = |position: usize| unsafe {
            let f: unsafe extern "C" fn(Id, Sel, usize) -> usize =
                std::mem::transmute(objc::msg::entry());
            f(path, objc::sel::get("indexAtPosition:"), position)
        };
        assert_eq!(at(0), 7);
        assert_eq!(at(1), NOT_FOUND);
    }
}
