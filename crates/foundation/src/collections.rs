use corefoundation::api;
use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
struct Predicate {
    isa: *const objc::class::Class,
    block: usize,
}

/// Report a mutation of a collection that was not created mutable.
///
/// A named abort rather than a wrong answer: an `NSArray` that silently accepted
/// `-addObject:` would hand a different object back to the next reader with
/// nothing anywhere to say why. Apple raises here.
#[cold]
fn immutable(what: &str, sel: &str) -> ! {
    objc::fail!("-[{what} {sel}]: the collection was not created mutable");
}

unsafe extern "C" fn count(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: an array.
    unsafe { api::CFArrayGetCount(receiver as CFTypeRef) as usize }
}

unsafe extern "C" fn object_at_index(receiver: Id, _sel: Sel, index: usize) -> Id {
    // SAFETY: an array.
    unsafe { api::CFArrayGetValueAtIndex(receiver as CFTypeRef, index as isize) as Id }
}

/// `-subarrayWithRange:`, or `NSRangeException` by name for a range past the end.
unsafe extern "C" fn subarray_with_range(
    receiver: Id,
    _sel: Sel,
    location: usize,
    length: usize,
) -> Id {
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let Some(end) = location
        .checked_add(length)
        .filter(|end| *end <= array.values.len())
    else {
        objc::fail!(
            "-subarrayWithRange:: range {{{location}, {length}}} exceeds array count {}",
            array.values.len()
        );
    };
    let sub = collections::new_array(&array.values[location..end], array.policy, false);
    // SAFETY: a fresh array.
    unsafe { api::CFAutorelease(sub) as Id }
}

/// `-firstObject` and `-lastObject`, which answer nil for an empty array where
/// `-objectAtIndex:` on one is out of range. That difference is the reason a
/// caller uses them.
unsafe extern "C" fn first_object(receiver: Id, sel: Sel) -> Id {
    // SAFETY: an array; `-objectAtIndex:` is this module's own.
    unsafe {
        match count(receiver, sel) {
            0 => std::ptr::null_mut(),
            _ => object_at_index(receiver, sel, 0),
        }
    }
}

unsafe extern "C" fn last_object(receiver: Id, sel: Sel) -> Id {
    unsafe {
        match count(receiver, sel) {
            0 => std::ptr::null_mut(),
            n => object_at_index(receiver, sel, n - 1),
        }
    }
}

unsafe extern "C" fn sorted_array_using_selector(receiver: Id, _sel: Sel, _compare: Sel) -> Id {
    // Apps use this with NSString keys and `caseInsensitiveCompare:`. Keep
    // the ordering deterministic while avoiding guest callbacks during sort.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let mut values = array.values.clone();
    values.sort_by_cached_key(|value| {
        // SAFETY: this Foundation entry point is used with NSString arrays.
        unsafe { crate::string::bytes(*value as Id) }.to_ascii_lowercase()
    });
    // SAFETY: a fresh array follows the convenience-constructor rule.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &values,
            Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

/// `NSEnumerationReverse`. `NSEnumerationConcurrent` is a permission rather than
/// a promise, so walking in order honours it.
const ENUMERATION_REVERSE: usize = 1 << 1;

/// `NSNotFound`.
const NOT_FOUND: usize = isize::MAX as usize;

/// `-indexOfObjectWithOptions:passingTest:`: the first index whose object the
/// block accepts, walking backwards under `NSEnumerationReverse`.
unsafe extern "C" fn index_of_object_with_options_passing_test(
    receiver: Id,
    _sel: Sel,
    options: usize,
    block: Id,
) -> usize {
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return NOT_FOUND;
    };
    // SAFETY: null or a block.
    let Some(invoke) = (unsafe { objc::block::invoke(block) }) else {
        return NOT_FOUND;
    };
    // SAFETY: the block is `BOOL (^)(id, NSUInteger, BOOL *)`.
    let call: unsafe extern "C" fn(Id, Id, usize, *mut bool) -> bool =
        unsafe { std::mem::transmute(invoke) };
    // A copy, because the block may mutate the array it is testing.
    let values = array.values.clone();
    let order: Box<dyn Iterator<Item = usize>> = match options & ENUMERATION_REVERSE {
        0 => Box::new(0..values.len()),
        _ => Box::new((0..values.len()).rev()),
    };
    let mut stop = false;
    for at in order {
        // SAFETY: as above; `stop` outlives the call.
        if unsafe { call(block, values[at] as Id, at, &mut stop) } {
            return at;
        }
        // A block that stops without accepting anything found nothing.
        if stop {
            break;
        }
    }
    NOT_FOUND
}

/// `-indexOfObjectPassingTest:`, the forward walk of the one above.
unsafe extern "C" fn index_of_object_passing_test(receiver: Id, sel: Sel, block: Id) -> usize {
    // SAFETY: the caller's array and block.
    unsafe { index_of_object_with_options_passing_test(receiver, sel, 0, block) }
}

/// `-enumerateObjectsWithOptions:usingBlock:`: each object with its index, in
/// order or under `NSEnumerationReverse` backwards, until the block sets its
/// `stop`. The walk is over the objects the array held when it began.
unsafe extern "C" fn enumerate_objects_with_options(
    receiver: Id,
    _sel: Sel,
    options: usize,
    block: Id,
) {
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    // SAFETY: null or a block.
    let Some(invoke) = (unsafe { objc::block::invoke(block) }) else {
        return;
    };
    // SAFETY: the block is `void (^)(id, NSUInteger, BOOL *)`.
    let call: unsafe extern "C" fn(Id, Id, usize, *mut bool) =
        unsafe { std::mem::transmute(invoke) };
    let values = array.values.clone();
    let order: Box<dyn Iterator<Item = usize>> = match options & ENUMERATION_REVERSE {
        0 => Box::new(0..values.len()),
        _ => Box::new((0..values.len()).rev()),
    };
    let mut stop = false;
    for at in order {
        // SAFETY: as above; `stop` outlives the call.
        unsafe { call(block, values[at] as Id, at, &mut stop) };
        if stop {
            break;
        }
    }
}

/// `-enumerateObjectsUsingBlock:`.
unsafe extern "C" fn enumerate_objects(receiver: Id, sel: Sel, block: Id) {
    // SAFETY: the caller's array and block.
    unsafe { enumerate_objects_with_options(receiver, sel, 0, block) }
}

/// `-makeObjectsPerformSelector:withObject:`: the message to each object in
/// order, with one argument.
unsafe extern "C" fn make_objects_perform_selector_with_object(
    receiver: Id,
    _sel: Sel,
    selector: Sel,
    argument: Id,
) {
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    if selector.is_null() {
        objc::fail!("-[NSArray makeObjectsPerformSelector:]: selector cannot be NULL");
    }
    let values = array.values.clone();
    // SAFETY: an `id (id, SEL, id)` send; a selector of none ignores the extra
    // register, as on Darwin.
    let send: unsafe extern "C" fn(Id, Sel, Id) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    for value in values {
        // SAFETY: as above, to each object the array held.
        unsafe { send(value as Id, selector, argument) };
    }
}

/// `-makeObjectsPerformSelector:`.
unsafe extern "C" fn make_objects_perform_selector(receiver: Id, sel: Sel, selector: Sel) {
    // SAFETY: the caller's array and selector.
    unsafe {
        make_objects_perform_selector_with_object(receiver, sel, selector, std::ptr::null_mut())
    }
}

/// `-enumerateKeysAndObjectsWithOptions:usingBlock:`: each key and its object
/// until the block sets its `stop`. A dictionary has no order to reverse, so
/// the options change nothing, as `NSEnumerationConcurrent` is only a
/// permission.
unsafe extern "C" fn enumerate_keys_and_objects_with_options(
    receiver: Id,
    _sel: Sel,
    _options: usize,
    block: Id,
) {
    // SAFETY: a dictionary.
    let Some(dictionary) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return;
    };
    // SAFETY: null or a block.
    let Some(invoke) = (unsafe { objc::block::invoke(block) }) else {
        return;
    };
    // SAFETY: the block is `void (^)(id key, id obj, BOOL *stop)`.
    let call: unsafe extern "C" fn(Id, Id, Id, *mut bool) = unsafe { std::mem::transmute(invoke) };
    let entries = dictionary.entries.clone();
    let mut stop = false;
    for (key, value) in entries {
        // SAFETY: as above; `stop` outlives the call.
        unsafe { call(block, key as Id, value as Id, &mut stop) };
        if stop {
            break;
        }
    }
}

/// `-enumerateKeysAndObjectsUsingBlock:`.
unsafe extern "C" fn enumerate_keys_and_objects(receiver: Id, sel: Sel, block: Id) {
    // SAFETY: the caller's dictionary and block.
    unsafe { enumerate_keys_and_objects_with_options(receiver, sel, 0, block) }
}

unsafe extern "C" fn filtered_array_using_predicate(receiver: Id, _sel: Sel, predicate: Id) -> Id {
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    if predicate.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `predicateWithBlock:` creates this layout.
    let block = unsafe { (*(predicate as *const Predicate)).block as Id };
    let mut values = Vec::new();
    if let Some(invoke) = unsafe { objc::block::invoke(block) } {
        // `NSPredicate` block predicates receive the object and a bindings
        // dictionary. There are no substitution variables in this operation.
        let call: unsafe extern "C" fn(Id, Id, Id) -> bool = unsafe { std::mem::transmute(invoke) };
        for value in &array.values {
            if unsafe { call(block, *value as Id, std::ptr::null_mut()) } {
                values.push(*value);
            }
        }
    }
    // SAFETY: a fresh array follows the convenience-constructor rule.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &values,
            Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

unsafe extern "C" fn predicate_with_block(_class: Id, _sel: Sel, block: Id) -> Id {
    let class = objc::class::lookup(c"NSPredicate").expect("NSPredicate is installed");
    // SAFETY: `Predicate` is the registered layout for this class.
    let predicate = unsafe { corefoundation::base::allocate::<Predicate>(class) };
    if predicate.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a fresh instance.
    unsafe { (*predicate).block = objc::block::copy(block) as usize };
    // SAFETY: a fresh object follows the convenience-constructor rule.
    unsafe { api::CFAutorelease(predicate as CFTypeRef) as Id }
}

unsafe extern "C" fn predicate_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a Predicate instance at the end of its lifetime.
    let block = unsafe { (*(receiver as *mut Predicate)).block as Id };
    objc::block::release(block);
    objc::arc::finalize(receiver);
}

/// An array enumerator: a snapshot of the array's elements and a cursor.
///
/// A snapshot rather than the live array, so that a program that mutates the
/// array mid-walk - its error, which Apple raises for - still reads elements
/// that are alive rather than a freed slot.
#[repr(C)]
struct ArrayEnumerator {
    isa: *const objc::class::Class,
    /// An owned immutable array.
    values: CFTypeRef,
    next: usize,
}

fn array_enumerator_class() -> &'static objc::class::Class {
    objc::class::lookup(c"__NSArrayEnumerator").expect("installed with the collections")
}

/// A fresh, autoreleased enumerator over `receiver`'s elements, back to front
/// when `reverse` is set.
unsafe fn new_array_enumerator(receiver: Id, reverse: bool) -> Id {
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let mut values = array.values.clone();
    if reverse {
        values.reverse();
    }
    // SAFETY: `ArrayEnumerator` is the registered layout for this class.
    let enumerator =
        unsafe { corefoundation::base::allocate::<ArrayEnumerator>(array_enumerator_class()) };
    if enumerator.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a fresh instance.
    unsafe {
        (*enumerator).values =
            collections::new_array(&values, Policy::from_type_callbacks(), false);
        (*enumerator).next = 0;
        api::CFAutorelease(enumerator as CFTypeRef) as Id
    }
}

unsafe extern "C" fn object_enumerator(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an array.
    unsafe { new_array_enumerator(receiver, false) }
}

unsafe extern "C" fn reverse_object_enumerator(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an array.
    unsafe { new_array_enumerator(receiver, true) }
}

/// The snapshot and cursor of one of this module's enumerators.
///
/// # Safety
///
/// `receiver` is an `__NSArrayEnumerator`.
unsafe fn enumerator_parts<'a>(receiver: Id) -> Option<(&'a [CFTypeRef], &'a mut usize)> {
    let body = receiver as *mut ArrayEnumerator;
    // SAFETY: the registered layout, and a snapshot this module made.
    let array = unsafe { collections::array((*body).values) }?;
    Some((&array.values, unsafe { &mut (*body).next }))
}

/// `-nextObject`: the next element, owned by the enumerator, or nil at the end.
unsafe extern "C" fn enumerator_next_object(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an array enumerator.
    let Some((values, next)) = (unsafe { enumerator_parts(receiver) }) else {
        return std::ptr::null_mut();
    };
    let Some(value) = values.get(*next) else {
        return std::ptr::null_mut();
    };
    *next += 1;
    *value as Id
}

/// `-allObjects`: whatever the enumerator has not yet handed out, which it
/// then has not got any more.
unsafe extern "C" fn enumerator_all_objects(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an array enumerator.
    let rest = match unsafe { enumerator_parts(receiver) } {
        Some((values, next)) => {
            let rest = values.get(*next..).unwrap_or_default().to_vec();
            *next = values.len();
            rest
        }
        None => Vec::new(),
    };
    // SAFETY: a fresh object holding live ones.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &rest,
            Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

/// `for (id x in enumerator)`, which consumes the enumerator as `-nextObject`
/// would: a batch into the caller's buffer at a time.
unsafe extern "C" fn enumerator_enumerate(
    receiver: Id,
    _sel: Sel,
    state: *mut FastEnumerationState,
    buffer: *mut Id,
    len: usize,
) -> usize {
    if state.is_null() || buffer.is_null() || len == 0 {
        return 0;
    }
    // SAFETY: an array enumerator.
    let Some((values, next)) = (unsafe { enumerator_parts(receiver) }) else {
        return 0;
    };
    let batch = values.len().saturating_sub(*next).min(len);
    for i in 0..batch {
        // SAFETY: the caller said `buffer` holds `len` items, and `batch` is at
        // most that.
        unsafe { *buffer.add(i) = values[*next + i] as Id };
    }
    *next += batch;
    // SAFETY: the guest's own state structure.
    let state = unsafe { &mut *state };
    state.state = 1;
    state.items = buffer;
    state.mutations = &NEVER_MUTATED as *const usize as *mut usize;
    batch
}

unsafe extern "C" fn enumerator_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: an enumerator at the end of its lifetime, and the snapshot it owns.
    unsafe { api::CFRelease((*(receiver as *mut ArrayEnumerator)).values) };
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn add_object(receiver: Id, _sel: Sel, object: Id) {
    // SAFETY: an array.
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    if !a.mutable {
        immutable("NSArray", "addObject:");
    }
    a.values.push(a.policy.retain(object as CFTypeRef));
}

/// `-insertObject:atIndex:`. Past the end is Apple's range exception, which
/// fails here by name.
unsafe extern "C" fn insert_object_at_index(receiver: Id, _sel: Sel, object: Id, index: usize) {
    // SAFETY: an array.
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    if !a.mutable {
        immutable("NSArray", "insertObject:atIndex:");
    }
    if object.is_null() || index > a.values.len() {
        objc::fail!(
            "-[NSMutableArray insertObject:atIndex:]: {} at index {index} of {}",
            if object.is_null() {
                "nil object"
            } else {
                "an object"
            },
            a.values.len()
        );
    }
    let retained = a.policy.retain(object as CFTypeRef);
    a.values.insert(index, retained);
}

/// `-replaceObjectAtIndex:withObject:`. Past the end is Apple's range
/// exception, which fails here by name.
unsafe extern "C" fn replace_object_at_index(receiver: Id, _sel: Sel, index: usize, object: Id) {
    // SAFETY: an array.
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    if !a.mutable {
        immutable("NSArray", "replaceObjectAtIndex:withObject:");
    }
    if object.is_null() || index >= a.values.len() {
        objc::fail!(
            "-[NSMutableArray replaceObjectAtIndex:withObject:]: {} at index {index} of {}",
            if object.is_null() {
                "nil object"
            } else {
                "an object"
            },
            a.values.len()
        );
    }
    let retained = a.policy.retain(object as CFTypeRef);
    let old = std::mem::replace(&mut a.values[index], retained);
    a.policy.release(old);
}

/// `-setObject:atIndexedSubscript:`, the `array[i] = x` spelling: a replace,
/// or an append when `index` is one past the end.
unsafe extern "C" fn set_object_at_indexed_subscript(
    receiver: Id,
    sel: Sel,
    object: Id,
    index: usize,
) {
    // SAFETY: an array.
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    // SAFETY: the same receiver and arguments, in the order each one takes.
    match index == a.values.len() {
        true => unsafe { insert_object_at_index(receiver, sel, object, index) },
        false => unsafe { replace_object_at_index(receiver, sel, index, object) },
    }
}

unsafe extern "C" fn add_objects_from_array(receiver: Id, _sel: Sel, other: Id) {
    // SAFETY: an array.
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    if !a.mutable {
        immutable("NSArray", "addObjectsFromArray:");
    }
    if other.is_null() {
        return;
    }
    // Cloned first: an array may be asked to add its own contents.
    // SAFETY: the argument is an array if it is one of ours.
    let values = match unsafe { collections::array(other as CFTypeRef) } {
        Some(o) => o.values.clone(),
        None => objc::fail!("-[NSMutableArray addObjectsFromArray:]: the argument is not an array"),
    };
    for value in values {
        a.values.push(a.policy.retain(value));
    }
}

unsafe extern "C" fn remove_object_at_index(receiver: Id, _sel: Sel, index: usize) {
    // SAFETY: an array.
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    if !a.mutable {
        immutable("NSArray", "removeObjectAtIndex:");
    }
    if index < a.values.len() {
        let gone = a.values.remove(index);
        a.policy.release(gone);
    }
}

unsafe extern "C" fn remove_last_object(receiver: Id, sel: Sel) {
    // SAFETY: an array.
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    if let Some(last) = a.values.len().checked_sub(1) {
        // SAFETY: the same receiver, at an index it has.
        unsafe { remove_object_at_index(receiver, sel, last) };
    }
}

/// An `NSComparisonResult (*)(id, id, void *)`, called directly: a native
/// image's function is this process's code, as `qsort`'s comparator is.
type Comparator = unsafe extern "C" fn(Id, Id, *mut std::ffi::c_void) -> isize;

/// `values` ordered by `compare`, stably, as Apple's merge sort is.
fn sort_by_function(values: &mut [CFTypeRef], compare: Comparator, context: *mut std::ffi::c_void) {
    // SAFETY: the guest's comparator, given two of the array's objects.
    values.sort_by(
        |a, b| match unsafe { compare(*a as Id, *b as Id, context) } {
            ..0 => std::cmp::Ordering::Less,
            0 => std::cmp::Ordering::Equal,
            _ => std::cmp::Ordering::Greater,
        },
    );
}

unsafe extern "C" fn sorted_array_using_function(
    receiver: Id,
    _sel: Sel,
    compare: Option<Comparator>,
    context: *mut std::ffi::c_void,
) -> Id {
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let mut values = array.values.clone();
    if let Some(compare) = compare {
        sort_by_function(&mut values, compare, context);
    }
    // SAFETY: a fresh array follows the convenience-constructor rule.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &values,
            Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

unsafe extern "C" fn sorted_array_using_comparator(receiver: Id, _sel: Sel, block: Id) -> Id {
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: null or a block.
    let Some(invoke) = (unsafe { objc::block::invoke(block) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the block is `NSComparisonResult (^)(id, id)`.
    let call: unsafe extern "C" fn(Id, Id, Id) -> isize = unsafe { std::mem::transmute(invoke) };
    // Sorted out of place: the block may read the array it is sorting.
    let mut values = array.values.clone();
    values.sort_by(|a, b| match unsafe { call(block, *a as Id, *b as Id) } {
        ..0 => std::cmp::Ordering::Less,
        0 => std::cmp::Ordering::Equal,
        _ => std::cmp::Ordering::Greater,
    });
    // SAFETY: a fresh array follows the convenience-constructor rule.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &values,
            Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

unsafe extern "C" fn sort_using_function(
    receiver: Id,
    _sel: Sel,
    compare: Option<Comparator>,
    context: *mut std::ffi::c_void,
) {
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    if !array.mutable {
        immutable("NSArray", "sortUsingFunction:context:");
    }
    // Sorted out of place: the comparator may read the array it is sorting.
    let mut values = array.values.clone();
    if let Some(compare) = compare {
        sort_by_function(&mut values, compare, context);
    }
    // SAFETY: the same array; the comparator is done with it.
    if let Some(array) = unsafe { collections::array(receiver as CFTypeRef) } {
        array.values = values;
    }
}

/// `-componentsJoinedByString:`, the other half of
/// `-[NSString componentsSeparatedByString:]`.
///
/// Every element is *sent* `-UTF8String` rather than read as a string, because
/// an array is entitled to hold anything and a guest joining an array of its own
/// objects expects each one's own answer. An element that answers nothing joins
/// as nothing, which is what a nil description would give.
unsafe extern "C" fn components_joined_by_string(receiver: Id, _sel: Sel, separator: Id) -> Id {
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: null or a string.
    let separator = unsafe { crate::string::bytes(separator) };
    let mut out: Vec<u8> = Vec::new();
    for (i, value) in array.values.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(&separator);
        }
        // SAFETY: an element of a live array.
        out.extend_from_slice(&unsafe { crate::string::bytes(*value as Id) });
    }
    let joined = corefoundation::string::from_bytes(&out, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(joined) as Id }
}

/// One element's `-description`, for the container descriptions below.
///
/// The element is asked, rather than read as a string, because a container
/// holds whatever was put in it - a number, another container - and the answer
/// has to be that object's own text. An object that does not answer is written
/// the way Apple writes one with no description: its class and its address.
///
/// # Safety
///
/// `object` must be null or live.
unsafe fn describe(object: Id) -> Vec<u8> {
    if object.is_null() {
        return b"(null)".to_vec();
    }
    let sel = objc::sel::get("description");
    let described = (unsafe { objc::object::class_of(object) })
        .and_then(|class| class.method(sel))
        .map(|_| {
            // SAFETY: `-description` is `id (id, SEL)`.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel) -> Id =
                    std::mem::transmute(objc::msg::entry());
                f(object, sel)
            }
        })
        .filter(|described| !described.is_null());
    match described {
        // SAFETY: a string this object just answered with.
        Some(described) => unsafe { crate::string::bytes(described) },
        None => {
            let name = (unsafe { objc::object::class_of(object) })
                // SAFETY: a realized class names itself with a live C string.
                .map(|class| unsafe { std::ffi::CStr::from_ptr(class.name()) })
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| String::from("?"));
            format!("<{name}: {object:p}>").into_bytes()
        }
    }
}

/// `-[NSArray description]`, in Apple's bracketed layout.
///
/// A program logs a container to see what is in it, so the elements are what
/// this has to spell out; the exact whitespace is Apple's and matters to
/// nothing, but a reader comparing a log against a Mac's should not have to
/// translate.
unsafe extern "C" fn array_description(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let mut out: Vec<u8> = b"(\n".to_vec();
    for (i, value) in array.values.iter().enumerate() {
        out.extend_from_slice(b"    ");
        // SAFETY: an element of a live array.
        out.extend_from_slice(&unsafe { describe(*value as Id) });
        if i + 1 < array.values.len() {
            out.push(b',');
        }
        out.push(b'\n');
    }
    out.push(b')');
    let text = corefoundation::string::from_bytes(&out, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(text) as Id }
}

/// `-[NSDictionary description]`, in Apple's braced layout.
unsafe extern "C" fn dictionary_description(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a dictionary.
    let Some(dictionary) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let mut out: Vec<u8> = b"{\n".to_vec();
    for (key, value) in &dictionary.entries {
        out.extend_from_slice(b"    ");
        // SAFETY: a key of a live dictionary.
        out.extend_from_slice(&unsafe { describe(*key as Id) });
        out.extend_from_slice(b" = ");
        // SAFETY: a value of a live dictionary.
        out.extend_from_slice(&unsafe { describe(*value as Id) });
        out.extend_from_slice(b";\n");
    }
    out.push(b'}');
    let text = corefoundation::string::from_bytes(&out, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(text) as Id }
}

/// `+[NSData dataWithContentsOfFile:]`.
///
/// Nil for a file that cannot be read, which is Apple's answer and the one a
/// caller checks for - a program asking for a file it may not have is asking a
/// question, not making a mistake.
unsafe extern "C" fn data_with_contents_of_file(_class: Id, _sel: Sel, path: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let path = unsafe { crate::string::bytes(path) };
    let Ok(path) = String::from_utf8(path) else {
        return std::ptr::null_mut();
    };
    // The guest named it, so it is read at this host's spelling.
    let Ok(bytes) = std::fs::read(prefix::ns::existing_host_path(&path)) else {
        return std::ptr::null_mut();
    };
    let data = collections::new_data(bytes);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(data) as Id }
}

/// `+dataWithContentsOfFile:options:error:`. The reading options only tune
/// how the file is read (mapped, uncached), not what is answered.
unsafe extern "C" fn data_with_contents_of_file_options(
    class: Id,
    sel: Sel,
    path: Id,
    _options: u64,
    error: *mut Id,
) -> Id {
    // SAFETY: forwarded unchanged.
    let data = unsafe { data_with_contents_of_file(class, sel, path) };
    if !error.is_null() {
        // `NSFileReadNoSuchFileError` or nothing: the caller passed a writable
        // `NSError **`.
        unsafe {
            *error = match data.is_null() {
                true => {
                    crate::error::make("NSCocoaErrorDomain", 260, "The file couldn't be opened.")
                }
                false => std::ptr::null_mut(),
            };
        }
    }
    data
}

/// A property list file's root object, autoreleased, if it is an array
/// (`want_array`) or a dictionary (otherwise). XML and binary lists go
/// through the parser bundle metadata uses; nil for a file that is unreadable,
/// malformed, or has the other root.
fn plist_file(path: &str, want_array: bool) -> Id {
    let Ok(bytes) = std::fs::read(prefix::ns::host_path(path)) else {
        return std::ptr::null_mut();
    };
    let Ok(value) = crate::plist::parse(&bytes) else {
        return std::ptr::null_mut();
    };
    let matches = match want_array {
        true => matches!(value, crate::plist::Value::Array(_)),
        false => matches!(value, crate::plist::Value::Dictionary(_)),
    };
    if !matches {
        return std::ptr::null_mut();
    }
    crate::objectify::object(&value, path)
}

/// The guest path an `NSString` names, or the file path of a `file:` `NSURL`.
unsafe fn file_argument(argument: Id, url: bool) -> Option<String> {
    match url {
        // SAFETY: an `NSURL`, or nil.
        true => unsafe { corefoundation::url::as_url(argument as CFTypeRef) }?.file_path(),
        // SAFETY: an `NSString`, or nil.
        false => String::from_utf8(unsafe { crate::string::bytes(argument) }).ok(),
    }
}

/// The `...WithContentsOfFile:` and `...WithContentsOfURL:` family, class and
/// instance forms. An instance form replaces the `+alloc` placeholder, and a
/// mutable class gets a mutable copy, as with every cluster initialiser here.
unsafe fn contents_of(receiver: Id, argument: Id, want_array: bool, url: bool, init: bool) -> Id {
    // SAFETY: as `file_argument` requires.
    let found = match unsafe { file_argument(argument, url) } {
        Some(path) => plist_file(&path, want_array),
        None => std::ptr::null_mut(),
    };
    // SAFETY: a class object, or an instance of one this runtime knows.
    let class = match init {
        true => unsafe { objc::object::class_of(receiver) },
        false => unsafe { objc::class::from_ptr(receiver as *const objc::class::Class) },
    };
    let mutable = match want_array {
        true => c"NSMutableArray",
        false => c"NSMutableDictionary",
    };
    let mutable = objc::class::lookup(mutable)
        .is_some_and(|m| class.is_some_and(|c| c.chain().any(|k| std::ptr::eq(k, m))));
    let owned = match (found.is_null(), mutable) {
        (true, _) => std::ptr::null_mut(),
        // SAFETY: `-mutableCopy` is `id (id, SEL)` and answers +1.
        (false, true) => unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(found, objc::sel::get("mutableCopy"))
        },
        (false, false) => objc::arc::retain(found),
    };
    if init {
        objc::arc::release(receiver);
        owned
    } else if owned.is_null() {
        owned
    } else {
        objc::arc::autorelease(owned)
    }
}

macro_rules! contents_of {
    ($($name:ident => $array:literal, $url:literal, $init:literal;)*) => {$(
        unsafe extern "C" fn $name(receiver: Id, _sel: Sel, argument: Id) -> Id {
            // SAFETY: forwarded from a method of the matching class and arity.
            unsafe { contents_of(receiver, argument, $array, $url, $init) }
        }
    )*};
}

contents_of! {
    dictionary_with_contents_of_file => false, false, false;
    dictionary_with_contents_of_url => false, true, false;
    dictionary_init_with_contents_of_file => false, false, true;
    dictionary_init_with_contents_of_url => false, true, true;
    array_with_contents_of_file => true, false, false;
    array_with_contents_of_url => true, true, false;
    array_init_with_contents_of_file => true, false, true;
    array_init_with_contents_of_url => true, true, true;
}

/// The `...WithContentsOfURL:error:` forms. A failure reports
/// `NSFileReadUnknownError` (256) in `NSCocoaErrorDomain`; the reason a load
/// failed is not classified further.
unsafe fn contents_of_url_error(
    receiver: Id,
    argument: Id,
    error: *mut Id,
    want_array: bool,
    init: bool,
) -> Id {
    // SAFETY: as `contents_of` requires.
    let found = unsafe { contents_of(receiver, argument, want_array, true, init) };
    if !error.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe {
            *error = match found.is_null() {
                true => crate::error::make(
                    "NSCocoaErrorDomain",
                    256,
                    "The file couldn\u{2019}t be opened.",
                ),
                false => std::ptr::null_mut(),
            }
        };
    }
    found
}

macro_rules! contents_of_url_error {
    ($($name:ident => $array:literal, $init:literal;)*) => {$(
        unsafe extern "C" fn $name(receiver: Id, _sel: Sel, url: Id, error: *mut Id) -> Id {
            // SAFETY: forwarded from a method of the matching class and arity.
            unsafe { contents_of_url_error(receiver, url, error, $array, $init) }
        }
    )*};
}

contents_of_url_error! {
    dictionary_with_contents_of_url_error => false, false;
    dictionary_init_with_contents_of_url_error => false, true;
    array_with_contents_of_url_error => true, false;
    array_init_with_contents_of_url_error => true, true;
}

/// `-writeToFile:atomically:`.
///
/// The atomic flag is honoured by writing beside the target and renaming, which
/// is what makes it atomic - a program that asked for it and got a plain write
/// would have a half-written save file after a crash and nothing to say so.
unsafe extern "C" fn write_to_file(receiver: Id, _sel: Sel, path: Id, atomically: u8) -> u8 {
    // SAFETY: an `NSString`, or nil.
    let path = unsafe { crate::string::bytes(path) };
    let Ok(path) = String::from_utf8(path) else {
        return 0;
    };
    // SAFETY: a data object.
    let Some(bytes) = (unsafe { collections::data(receiver as CFTypeRef) }) else {
        return 0;
    };
    write_bytes_to_file(&path, bytes, atomically != 0).is_ok() as u8
}

/// `-[NSArray writeToFile:atomically:]` and `-[NSDictionary
/// writeToFile:atomically:]`: the receiver as an XML property list.
///
/// `NO`, and nothing written, for a collection holding anything a property
/// list cannot - which is checked all the way down before a byte is written,
/// as Apple's documents.
unsafe extern "C" fn write_property_list_to_file(
    receiver: Id,
    _sel: Sel,
    path: Id,
    atomically: u8,
) -> u8 {
    // SAFETY: an `NSString`, or nil.
    let path = unsafe { crate::string::bytes(path) };
    match String::from_utf8(path) {
        // SAFETY: the receiver is a collection.
        Ok(path) if !path.is_empty() => unsafe { write_property_list(receiver, &path, atomically) },
        _ => 0,
    }
}

/// `-writeToURL:atomically:`, the same for a file URL. Any other URL is `NO`.
unsafe extern "C" fn write_property_list_to_url(
    receiver: Id,
    _sel: Sel,
    url: Id,
    atomically: u8,
) -> u8 {
    // SAFETY: an `NSURL`, or nil.
    let path = unsafe { corefoundation::url::as_url(url as CFTypeRef) }.and_then(|u| u.file_path());
    match path {
        // SAFETY: the receiver is a collection.
        Some(path) => unsafe { write_property_list(receiver, &path, atomically) },
        None => 0,
    }
}

/// # Safety
///
/// `receiver` is an array or a dictionary.
unsafe fn write_property_list(receiver: Id, path: &str, atomically: u8) -> u8 {
    // SAFETY: the caller's.
    let Some(value) = (unsafe { corefoundation::propertylist::value_of(receiver as CFTypeRef) })
    else {
        return 0;
    };
    let xml = corefoundation::plist::write_xml(&value);
    write_bytes_to_file(path, &xml, atomically != 0).is_ok() as u8
}

pub(crate) fn write_bytes_to_file(
    guest_path: &str,
    bytes: &[u8],
    atomically: bool,
) -> std::io::Result<()> {
    let path = prefix::ns::host_path(guest_path);
    if !atomically {
        return std::fs::write(&path, bytes);
    }
    // Appended, not `with_extension`, which would *replace* `.dat` and could
    // collide with a real neighbour.
    let mut temporary = path.clone().into_os_string();
    temporary.push(format!(".rosemary-{}", std::process::id()));
    let temporary = std::path::PathBuf::from(temporary);
    std::fs::write(&temporary, bytes)?;
    match std::fs::rename(&temporary, &path) {
        Ok(()) => Ok(()),
        Err(why) => {
            let _ = std::fs::remove_file(&temporary);
            Err(why)
        }
    }
}

/// `-[NSDictionary valueForKey:]`, which is *not* the `NSObject` one.
///
/// A dictionary overrides key-value coding to mean its own contents: Apple's
/// answers `-objectForKey:` for any key not beginning with `@`. Without the
/// override the inherited implementation looks for a getter or an ivar of that
/// name, finds neither, and reports a class that in fact has the key.
unsafe extern "C" fn dictionary_value_for_key(receiver: Id, sel: Sel, key: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let name = unsafe { crate::string::bytes(key) };
    if name.first() == Some(&b'@') {
        // `@count` and its relatives are the operators, which address the
        // dictionary itself rather than its contents. Refused by name: an
        // operator answered as a missing key would be nil where a number was
        // expected.
        objc::fail!(
            "-[NSDictionary valueForKey:]: {} is a key-value coding operator, which this runtime \
             does not implement",
            String::from_utf8_lossy(&name)
        );
    }
    // SAFETY: `-objectForKey:` is `id (id, SEL, id)`.
    unsafe { object_for_key(receiver, sel, key) }
}

/// `-[NSArray valueForKey:]`, which maps the key over the elements.
///
/// Apple answers an array of each element's own `-valueForKey:`, nil becoming
/// `NSNull`; a guest chains it into array methods, so answering for the array
/// itself sends those to whatever the inherited lookup finds.
unsafe extern "C" fn array_value_for_key(receiver: Id, _sel: Sel, key: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let name = unsafe { crate::string::bytes(key) };
    if name.first() == Some(&b'@') {
        objc::fail!(
            "-[NSArray valueForKey:]: {} is a key-value coding operator, which this runtime \
             does not implement",
            String::from_utf8_lossy(&name)
        );
    }
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let send: unsafe extern "C" fn(Id, Sel, Id) -> Id =
        // SAFETY: `-valueForKey:` is `id (id, SEL, id)`.
        unsafe { std::mem::transmute(objc::msg::entry()) };
    let null = objc::class::lookup(c"NSNull").map(|class| {
        let null: unsafe extern "C" fn(Id, Sel) -> Id =
            // SAFETY: `+null` is `id (Class, SEL)`.
            unsafe { std::mem::transmute(objc::msg::entry()) };
        // SAFETY: the class answers `+null`.
        unsafe { null(class as *const _ as Id, objc::sel::get("null")) }
    });
    let sel = objc::sel::get("valueForKey:");
    let values: Vec<CFTypeRef> = array
        .values
        .iter()
        .map(|value| {
            // SAFETY: an element of a live array, sent a KVC question.
            let answer = unsafe { send(*value as Id, sel, key) };
            match answer.is_null() {
                true => null.unwrap_or(answer) as CFTypeRef,
                false => answer as CFTypeRef,
            }
        })
        .filter(|value| !value.is_null())
        .collect();
    // SAFETY: a fresh object holding live ones.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &values,
            Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

/// `-[NSMutableDictionary setValue:forKey:]`, whose nil means *remove*.
unsafe extern "C" fn dictionary_set_value_for_key(receiver: Id, sel: Sel, value: Id, key: Id) {
    match value.is_null() {
        // SAFETY: `-removeObjectForKey:` is `void (id, SEL, id)`.
        true => unsafe { remove_object_for_key(receiver, sel, key) },
        // SAFETY: `-setObject:forKey:` is `void (id, SEL, id, id)`.
        false => unsafe { set_object_for_key(receiver, sel, value, key) },
    }
}

/// `NSFastEnumerationState`, which the guest allocates on its own stack.
///
/// The layout is ABI: `for (id x in collection)` is compiled by clang into a
/// call to `-countByEnumeratingWithState:objects:count:` and then a walk of
/// `items_ptr`, so a field at the wrong offset is a loop over the wrong memory.
#[repr(C)]
struct FastEnumerationState {
    state: usize,
    items: *mut Id,
    mutations: *mut usize,
    extra: [usize; 5],
}

/// A word every collection points `mutations` at.
///
/// The compiler reads it before and after the loop body and raises if it
/// changed, which is how a program is told it mutated a collection while
/// enumerating it. One shared word means this runtime never reports that - it
/// has no per-collection version counter - so the check always passes. The
/// alternative is pointing at the collection's own storage, whose value changes
/// for reasons that are not mutations and would raise on a correct program.
static NEVER_MUTATED: usize = 0;

/// `-countByEnumeratingWithState:objects:count:`, for anything whose elements
/// are a contiguous vector.
///
/// One pass: everything is already in one buffer, so the first call hands the
/// guest a pointer straight into it and the second says there is no more. That
/// is what Apple's array does too, and it is why `buffer` is untouched - it is
/// there for a collection that has to *materialise* its elements, which none of
/// these do.
///
/// # Safety
///
/// `state` must be a writable `NSFastEnumerationState`, and `values` must
/// outlive the loop - which it does, because a mutation during enumeration is
/// the program's error and the one this cannot detect.
unsafe fn enumerate(state: *mut FastEnumerationState, values: &mut [CFTypeRef]) -> usize {
    if state.is_null() {
        return 0;
    }
    let state = unsafe { &mut *state };
    if state.state != 0 {
        return 0;
    }
    state.state = 1;
    state.items = values.as_mut_ptr() as *mut Id;
    state.mutations = &NEVER_MUTATED as *const usize as *mut usize;
    values.len()
}

unsafe extern "C" fn array_enumerate(
    receiver: Id,
    _sel: Sel,
    state: *mut FastEnumerationState,
    _buffer: *mut Id,
    _len: usize,
) -> usize {
    // SAFETY: an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return 0;
    };
    // SAFETY: the guest's own state structure, and storage that outlives the
    // loop.
    unsafe { enumerate(state, &mut array.values) }
}

/// The dictionary's, which enumerates **keys** - as `for (id key in dict)` does.
///
/// A dictionary's keys are not contiguous here: it is a vector of *pairs*. So
/// this fills the caller's buffer a batch at a time, which is what that argument
/// is for and the reason the protocol has one. Copying them into a buffer of
/// this object's own would either leak it - a `break` means the second call
/// never comes - or need a lifetime nothing here can give it.
///
/// `state` is the cursor: zero on entry, and the index of the next key after
/// that.
unsafe extern "C" fn dictionary_enumerate(
    receiver: Id,
    _sel: Sel,
    state: *mut FastEnumerationState,
    buffer: *mut Id,
    len: usize,
) -> usize {
    // SAFETY: a dictionary.
    let Some(dictionary) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return 0;
    };
    if state.is_null() || buffer.is_null() || len == 0 {
        return 0;
    }
    // SAFETY: the guest's own state structure.
    let state = unsafe { &mut *state };
    let from = state.state;
    let batch = dictionary.entries.len().saturating_sub(from).min(len);
    if batch == 0 {
        return 0;
    }
    for i in 0..batch {
        // SAFETY: the caller said `buffer` holds `len` items, and `batch` is at
        // most that.
        unsafe { *buffer.add(i) = dictionary.entries[from + i].0 as Id };
    }
    state.state = from + batch;
    state.items = buffer;
    state.mutations = &NEVER_MUTATED as *const usize as *mut usize;
    batch
}

unsafe extern "C" fn remove_all_objects(receiver: Id, _sel: Sel) {
    // SAFETY: an array.
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    if !a.mutable {
        immutable("NSArray", "removeAllObjects");
    }
    for value in a.values.drain(..) {
        a.policy.release(value);
    }
}

/// `-removeObject:` and `-removeObjectIdenticalTo:`: every element `matches`
/// takes out, not just the first.
///
/// Released after the array is consistent, because a release can run a
/// `dealloc` that looks at it.
fn remove_matching(receiver: Id, sel: &str, matches: impl Fn(&Policy, CFTypeRef) -> bool) {
    // SAFETY: an array.
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return;
    };
    if !a.mutable {
        immutable("NSArray", sel);
    }
    let mut gone = Vec::new();
    a.values.retain(|v| {
        let keep = !matches(&a.policy, *v);
        if !keep {
            gone.push(*v);
        }
        keep
    });
    for value in gone {
        a.policy.release(value);
    }
}

unsafe extern "C" fn remove_object(receiver: Id, _sel: Sel, object: Id) {
    remove_matching(receiver, "removeObject:", |policy, v| {
        policy.equal(v, object as CFTypeRef)
    });
}

unsafe extern "C" fn remove_object_identical_to(receiver: Id, _sel: Sel, object: Id) {
    remove_matching(receiver, "removeObjectIdenticalTo:", |_, v| {
        std::ptr::eq(v, object as CFTypeRef)
    });
}

unsafe extern "C" fn contains_object(receiver: Id, _sel: Sel, object: Id) -> bool {
    // SAFETY: an array.
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return false;
    };
    a.values
        .iter()
        .any(|v| a.policy.equal(*v, object as CFTypeRef))
}

unsafe extern "C" fn index_of_object(receiver: Id, _sel: Sel, object: Id) -> usize {
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return NOT_FOUND;
    };
    a.values
        .iter()
        .position(|v| a.policy.equal(*v, object as CFTypeRef))
        .unwrap_or(NOT_FOUND)
}

unsafe extern "C" fn index_of_object_identical_to(receiver: Id, _sel: Sel, object: Id) -> usize {
    let Some(a) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return NOT_FOUND;
    };
    a.values
        .iter()
        .position(|v| std::ptr::eq(*v, object as CFTypeRef))
        .unwrap_or(NOT_FOUND)
}

unsafe extern "C" fn new_array(_class: Id, _sel: Sel) -> Id {
    collections::new_array(&[], Policy::from_type_callbacks(), true) as Id
}

unsafe extern "C" fn array_fn(_class: Id, _sel: Sel) -> Id {
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &[],
            Policy::from_type_callbacks(),
            true,
        )) as Id
    }
}

unsafe extern "C" fn array_with_capacity(_class: Id, _sel: Sel, _capacity: usize) -> Id {
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &[],
            Policy::from_type_callbacks(),
            true,
        )) as Id
    }
}

/// Whether a cluster's convenience constructor was sent to its mutable class.
///
/// Apple declares `+arrayWithObject:` and `+dictionaryWithObject:forKey:` on the
/// immutable class, and the mutable class inherits them - so one method answers
/// for both, and only the receiver says which was asked for. A program that
/// sends one to `NSMutableDictionary` writes to what comes back, and a
/// collection made immutable there fails on the next line.
fn mutable_receiver(class: Id, mutable: &std::ffi::CStr) -> bool {
    // SAFETY: the receiver of a class method is a class this runtime knows.
    let Some(class) = (unsafe { objc::class::from_ptr(class as *const objc::class::Class) }) else {
        return false;
    };
    class.chain().any(|c| {
        // SAFETY: a realized class names itself with a live C string.
        unsafe { std::ffi::CStr::from_ptr(c.name()) == mutable }
    })
}

/// The class an `-init...` placeholder was allocated as, for a constructor
/// that decides mutability by its receiver.
fn class_of_placeholder(receiver: Id) -> Id {
    // SAFETY: an instance `+alloc` made, or nil.
    unsafe { objc::object::class_of(receiver) }
        .map_or(std::ptr::null_mut(), |c| objc::class::ptr(c) as Id)
}

/// `-initWithObjects:count:`, the counted method `-initWithObjects:` reaches.
unsafe extern "C" fn array_init_with_objects_count(
    receiver: Id,
    sel: Sel,
    objects: *const Id,
    count: usize,
) -> Id {
    let class = class_of_placeholder(receiver);
    objc::arc::release(receiver);
    // SAFETY: forwarded; the answer is autoreleased and the caller owns an init.
    objc::arc::retain(unsafe { array_with_objects_count(class, sel, objects, count) })
}

/// `-initWithObjects:forKeys:count:`.
unsafe extern "C" fn dictionary_init_with_objects_for_keys_count(
    receiver: Id,
    sel: Sel,
    objects: *const Id,
    keys: *const Id,
    count: usize,
) -> Id {
    let class = class_of_placeholder(receiver);
    objc::arc::release(receiver);
    // SAFETY: as above.
    objc::arc::retain(unsafe {
        dictionary_with_objects_for_keys_count(class, sel, objects, keys, count)
    })
}

unsafe extern "C" fn array_with_object(class: Id, _sel: Sel, object: Id) -> Id {
    // SAFETY: a fresh object holding a live one.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &[object as CFTypeRef],
            Policy::from_type_callbacks(),
            mutable_receiver(class, c"NSMutableArray"),
        )) as Id
    }
}

unsafe extern "C" fn array_with_array(_class: Id, _sel: Sel, other: Id) -> Id {
    let values: Vec<CFTypeRef> = if other.is_null() {
        Vec::new()
    } else {
        // SAFETY: the argument is an array if it is one of ours.
        match unsafe { collections::array(other as CFTypeRef) } {
            Some(a) => a.values.clone(),
            None => objc::fail!("+[NSMutableArray arrayWithArray:]: the argument is not an array"),
        }
    };
    // SAFETY: a fresh object holding live ones.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &values,
            Policy::from_type_callbacks(),
            true,
        )) as Id
    }
}

/// `+arrayWithObjects:count:`, which is the counted spelling
/// `objc::dispatch` rewrites the nil-terminated variadic into.
unsafe extern "C" fn array_with_objects_count(
    class: Id,
    _sel: Sel,
    objects: *const Id,
    count: usize,
) -> Id {
    if objects.is_null() && count > 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller says `objects` covers `count` entries.
    let slice = match count {
        0 => &[][..],
        _ => unsafe { std::slice::from_raw_parts(objects as *const CFTypeRef, count) },
    };
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_array(
            slice,
            Policy::from_type_callbacks(),
            mutable_receiver(class, c"NSMutableArray"),
        )) as Id
    }
}

unsafe extern "C" fn dictionary_count(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: a dictionary.
    unsafe { api::CFDictionaryGetCount(receiver as CFTypeRef) as usize }
}

unsafe extern "C" fn object_for_key(receiver: Id, _sel: Sel, key: Id) -> Id {
    // SAFETY: a dictionary and a key.
    unsafe { api::CFDictionaryGetValue(receiver as CFTypeRef, key as CFTypeRef) as Id }
}

unsafe extern "C" fn set_object_for_key(receiver: Id, _sel: Sel, object: Id, key: Id) {
    // SAFETY: a dictionary.
    let Some(d) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return;
    };
    if !d.mutable {
        immutable("NSDictionary", "setObject:forKey:");
    }
    d.set(key as CFTypeRef, object as CFTypeRef);
}

unsafe extern "C" fn remove_object_for_key(receiver: Id, _sel: Sel, key: Id) {
    // SAFETY: a dictionary.
    let Some(d) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return;
    };
    if !d.mutable {
        immutable("NSDictionary", "removeObjectForKey:");
    }
    if let Some(at) = d.position(key as CFTypeRef) {
        let (key, value) = d.entries.remove(at);
        d.keys.release(key);
        d.values.release(value);
    }
}

unsafe extern "C" fn dictionary_remove_all_objects(receiver: Id, _sel: Sel) {
    // SAFETY: a dictionary.
    let Some(d) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return;
    };
    if !d.mutable {
        immutable("NSDictionary", "removeAllObjects");
    }
    // Taken first: a release can run a guest `dealloc` that reads this
    // dictionary again.
    let (keys, values) = (d.keys, d.values);
    for (key, value) in std::mem::take(&mut d.entries) {
        keys.release(key);
        values.release(value);
    }
}

/// A snapshot of `other`'s pairs, for the methods that read one dictionary
/// while writing another.
///
/// Taken whole first because the two can be the same object, and a retain or
/// release callback can run guest code that reads the dictionary again.
/// `None` for nil, which Apple's methods treat as an empty argument.
fn entries_of(other: Id, what: &str) -> Option<Vec<(CFTypeRef, CFTypeRef)>> {
    if other.is_null() {
        return None;
    }
    // SAFETY: a dictionary if it is one of ours.
    match unsafe { collections::dictionary(other as CFTypeRef) } {
        Some(d) => Some(d.entries.clone()),
        None => objc::fail!("{what}: the argument is not a dictionary"),
    }
}

/// `-addEntriesFromDictionary:`: every pair of the argument is set, replacing
/// the value of a key already present and keeping the receiver's key object.
unsafe extern "C" fn add_entries_from_dictionary(receiver: Id, _sel: Sel, other: Id) {
    // SAFETY: a dictionary.
    let Some(d) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return;
    };
    if !d.mutable {
        immutable("NSDictionary", "addEntriesFromDictionary:");
    }
    let Some(entries) = entries_of(other, "-[NSMutableDictionary addEntriesFromDictionary:]")
    else {
        return;
    };
    for (key, value) in entries {
        d.set(key, value);
    }
}

/// `-setDictionary:`: the receiver's pairs become the argument's.
unsafe extern "C" fn set_dictionary(receiver: Id, _sel: Sel, other: Id) {
    // SAFETY: a dictionary.
    let Some(d) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return;
    };
    if !d.mutable {
        immutable("NSDictionary", "setDictionary:");
    }
    let entries = entries_of(other, "-[NSMutableDictionary setDictionary:]").unwrap_or_default();
    // Retained before the old pairs go, since the argument may be the receiver
    // or share its objects.
    let (keys, values) = (d.keys, d.values);
    let fresh: Vec<_> = entries
        .into_iter()
        .map(|(k, v)| (keys.retain(k), values.retain(v)))
        .collect();
    for (key, value) in std::mem::replace(&mut d.entries, fresh) {
        keys.release(key);
        values.release(value);
    }
}

/// `-removeObjectsForKeys:`: keys not present are ignored.
unsafe extern "C" fn remove_objects_for_keys(receiver: Id, _sel: Sel, keys: Id) {
    // SAFETY: a dictionary.
    let Some(d) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return;
    };
    if !d.mutable {
        immutable("NSDictionary", "removeObjectsForKeys:");
    }
    if keys.is_null() {
        return;
    }
    // SAFETY: an array if it is one of ours.
    let Some(array) = (unsafe { collections::array(keys as CFTypeRef) }) else {
        objc::fail!("-[NSMutableDictionary removeObjectsForKeys:]: the argument is not an array");
    };
    let doomed = array.values.clone();
    for key in doomed {
        if let Some(at) = d.position(key) {
            let (k, v) = d.entries.remove(at);
            d.keys.release(k);
            d.values.release(v);
        }
    }
}

/// `+dictionaryWithDictionary:`, mutable when sent to `NSMutableDictionary`.
unsafe extern "C" fn dictionary_with_dictionary(class: Id, _sel: Sel, other: Id) -> Id {
    let entries =
        entries_of(other, "+[NSDictionary dictionaryWithDictionary:]").unwrap_or_default();
    let dictionary = collections::new_dictionary(
        &entries,
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        mutable_receiver(class, c"NSMutableDictionary"),
    );
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(dictionary) as Id }
}

/// `-initWithDictionary:`.
unsafe extern "C" fn dictionary_init_with_dictionary(receiver: Id, sel: Sel, other: Id) -> Id {
    let class = class_of_placeholder(receiver);
    objc::arc::release(receiver);
    // SAFETY: forwarded; the answer is autoreleased and the caller owns an init.
    objc::arc::retain(unsafe { dictionary_with_dictionary(class, sel, other) })
}

/// `+dictionaryWithObjects:forKeys:`, the two-array spelling.
unsafe extern "C" fn dictionary_with_objects_for_keys(
    class: Id,
    _sel: Sel,
    objects: Id,
    keys: Id,
) -> Id {
    if objects.is_null() || keys.is_null() {
        objc::fail!("+[NSDictionary dictionaryWithObjects:forKeys:]: a nil array");
    }
    // SAFETY: arrays if they are ours.
    let (Some(o), Some(k)) = (unsafe {
        (
            collections::array(objects as CFTypeRef),
            collections::array(keys as CFTypeRef),
        )
    }) else {
        objc::fail!("+[NSDictionary dictionaryWithObjects:forKeys:]: the arguments are not arrays");
    };
    if o.values.len() != k.values.len() {
        objc::fail!(
            "+[NSDictionary dictionaryWithObjects:forKeys:]: count of objects ({}) differs from count of keys ({})",
            o.values.len(),
            k.values.len()
        );
    }
    let pairs: Vec<_> = k
        .values
        .iter()
        .copied()
        .zip(o.values.iter().copied())
        .collect();
    let dictionary = collections::new_dictionary(
        &pairs,
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        mutable_receiver(class, c"NSMutableDictionary"),
    );
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(dictionary) as Id }
}

/// `-allKeysForObject:`: every key whose value is equal to the argument.
unsafe extern "C" fn all_keys_for_object(receiver: Id, _sel: Sel, object: Id) -> Id {
    // SAFETY: a dictionary.
    let Some(d) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let keys: Vec<CFTypeRef> = d
        .entries
        .iter()
        .filter(|(_, v)| d.values.equal(*v, object as CFTypeRef))
        .map(|(k, _)| *k)
        .collect();
    // SAFETY: a fresh object holding live ones.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &keys,
            Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

/// `-objectsForKeys:notFoundMarker:`: one value per key, the marker for a key
/// that is missing.
unsafe extern "C" fn objects_for_keys_not_found_marker(
    receiver: Id,
    _sel: Sel,
    keys: Id,
    marker: Id,
) -> Id {
    // SAFETY: a dictionary.
    let Some(d) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: an array if it is one of ours.
    let Some(keys) = (unsafe { collections::array(keys as CFTypeRef) }) else {
        objc::fail!("-[NSDictionary objectsForKeys:notFoundMarker:]: the keys are not an array");
    };
    let values: Vec<CFTypeRef> = keys
        .values
        .iter()
        .map(|key| match d.position(*key) {
            Some(at) => d.entries[at].1,
            None => marker as CFTypeRef,
        })
        .collect();
    // SAFETY: a fresh object holding live ones.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &values,
            Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

/// `-isEqual:` of an array: an array with equal elements in the same order.
unsafe extern "C" fn array_is_equal(receiver: Id, sel: Sel, other: Id) -> bool {
    receiver == other || unsafe { is_equal_to_array(receiver, sel, other) }
}

/// `-isEqualToArray:`: the same count, each element equal to its peer as the
/// receiver's callbacks compare them.
unsafe extern "C" fn is_equal_to_array(receiver: Id, _sel: Sel, other: Id) -> bool {
    // SAFETY: arrays if they are ours.
    let (Some(a), Some(b)) = (
        unsafe { collections::array(receiver as CFTypeRef) },
        unsafe { collections::array(other as CFTypeRef) },
    ) else {
        return false;
    };
    a.values.len() == b.values.len()
        && a.values
            .iter()
            .zip(&b.values)
            .all(|(x, y)| a.policy.equal(*x, *y))
}

/// `-hash` of an array: its count, as `CFArray`'s is, so that equal arrays
/// hash alike whatever they hold.
unsafe extern "C" fn array_hash(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: an array if it is ours.
    unsafe { collections::array(receiver as CFTypeRef) }
        .map_or(receiver as usize, |a| a.values.len())
}

/// `-isEqual:` of a dictionary: a dictionary with the same entries.
unsafe extern "C" fn dictionary_is_equal(receiver: Id, sel: Sel, other: Id) -> bool {
    receiver == other || unsafe { is_equal_to_dictionary(receiver, sel, other) }
}

/// `-hash` of a dictionary: its count, as `CFDictionary`'s is.
unsafe extern "C" fn dictionary_hash(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: a dictionary if it is ours.
    unsafe { collections::dictionary(receiver as CFTypeRef) }
        .map_or(receiver as usize, |d| d.entries.len())
}

/// `-isEqual:` of a set: a set with the same members.
unsafe extern "C" fn set_is_equal(receiver: Id, sel: Sel, other: Id) -> bool {
    receiver == other || unsafe { is_equal_to_set(receiver, sel, other) }
}

/// `-isEqualToSet:`: the same count, and every member of one in the other.
unsafe extern "C" fn is_equal_to_set(receiver: Id, _sel: Sel, other: Id) -> bool {
    // SAFETY: sets if they are ours.
    let (Some(a), Some(b)) = (unsafe { collections::set(receiver as CFTypeRef) }, unsafe {
        collections::set(other as CFTypeRef)
    }) else {
        return false;
    };
    a.values.len() == b.values.len()
        && a.values
            .iter()
            .all(|x| b.values.iter().any(|y| b.policy.equal(*x, *y)))
}

/// `-hash` of a set: its count, as `CFSet`'s is.
unsafe extern "C" fn set_hash(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: a set if it is ours.
    unsafe { collections::set(receiver as CFTypeRef) }.map_or(receiver as usize, |s| s.values.len())
}

/// `-isEqualToDictionary:`: the same keys, each with an equal value.
unsafe extern "C" fn is_equal_to_dictionary(receiver: Id, _sel: Sel, other: Id) -> bool {
    // SAFETY: dictionaries if they are ours.
    let (Some(a), Some(b)) = (
        unsafe { collections::dictionary(receiver as CFTypeRef) },
        unsafe { collections::dictionary(other as CFTypeRef) },
    ) else {
        return false;
    };
    a.entries.len() == b.entries.len()
        && a.entries.iter().all(|(k, v)| {
            b.position(*k)
                .is_some_and(|at| a.values.equal(*v, b.entries[at].1))
        })
}

/// `-keyEnumerator`: a snapshot, so the dictionary may change under it.
unsafe extern "C" fn key_enumerator(receiver: Id, sel: Sel) -> Id {
    // SAFETY: a dictionary.
    unsafe { new_array_enumerator(all_keys(receiver, sel), false) }
}

/// `-objectEnumerator` of a dictionary: the values.
unsafe extern "C" fn dictionary_object_enumerator(receiver: Id, sel: Sel) -> Id {
    // SAFETY: a dictionary.
    unsafe { new_array_enumerator(all_values(receiver, sel), false) }
}

/// `-allKeys`, which is an `NSArray` of the keys in insertion order.
unsafe extern "C" fn all_keys(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a dictionary.
    let Some(d) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let keys: Vec<CFTypeRef> = d.entries.iter().map(|(k, _)| *k).collect();
    // SAFETY: a fresh object holding live ones.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &keys,
            Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

unsafe extern "C" fn all_values(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a dictionary.
    let Some(d) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let values: Vec<CFTypeRef> = d.entries.iter().map(|(_, v)| *v).collect();
    // SAFETY: a fresh object holding live ones.
    unsafe {
        api::CFAutorelease(collections::new_array(
            &values,
            Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

/// `-copy` and `-mutableCopy` of an array: a new array of the same objects.
/// Always a new one, even for an immutable receiver, because a receiver that
/// is really an `NSMutableArray` must not change under the copy.
fn copy_array(receiver: Id, mutable: bool) -> Id {
    // SAFETY: the receiver is an array.
    let Some(array) = (unsafe { collections::array(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    let values = array.values.clone();
    collections::new_array(&values, Policy::from_type_callbacks(), mutable) as Id
}

unsafe extern "C" fn array_copy(receiver: Id, _sel: Sel) -> Id {
    copy_array(receiver, false)
}

unsafe extern "C" fn array_mutable_copy(receiver: Id, _sel: Sel) -> Id {
    copy_array(receiver, true)
}

unsafe extern "C" fn array_copy_with_zone(receiver: Id, _sel: Sel, _zone: *const ()) -> Id {
    copy_array(receiver, false)
}

unsafe extern "C" fn array_mutable_copy_with_zone(receiver: Id, _sel: Sel, _zone: *const ()) -> Id {
    copy_array(receiver, true)
}

unsafe extern "C" fn dictionary_copy(receiver: Id, _sel: Sel) -> Id {
    copy_dictionary(receiver, false)
}

unsafe extern "C" fn dictionary_mutable_copy(receiver: Id, _sel: Sel) -> Id {
    copy_dictionary(receiver, true)
}

fn copy_dictionary(receiver: Id, mutable: bool) -> Id {
    // SAFETY: the receiver is a dictionary.
    let Some(dictionary) = (unsafe { collections::dictionary(receiver as CFTypeRef) }) else {
        return std::ptr::null_mut();
    };
    collections::new_dictionary(
        &dictionary.entries,
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        mutable,
    ) as Id
}

unsafe extern "C" fn dictionary_copy_with_zone(receiver: Id, sel: Sel, _zone: *const ()) -> Id {
    unsafe { dictionary_copy(receiver, sel) }
}

unsafe extern "C" fn dictionary_mutable_copy_with_zone(
    receiver: Id,
    sel: Sel,
    _zone: *const (),
) -> Id {
    unsafe { dictionary_mutable_copy(receiver, sel) }
}

/// `-init` on `NSMutableArray` and `NSMutableDictionary`, which are class
/// clusters.
///
/// `+alloc` on one of those answers an instance of the abstract class itself -
/// an isa and nothing else - because that is all an abstract class has room
/// for. Every read and every write goes through the contents a concrete
/// `__NSCF*` object carries, so a placeholder handed back from `-init` answers
/// empty to every question and swallows every write, silently: nothing here is
/// wrong enough to fail on, and the program carries on with a collection that
/// never fills. Apple's clusters substitute here too, which is why `-init` is
/// documented there as returning "an object other than the receiver".
///
/// # Safety
///
/// `receiver` is null or an instance of the class this was installed on, and
/// `concrete` answers a fresh object with one reference.
unsafe fn cluster_init(receiver: Id, known: bool, concrete: impl FnOnce() -> CFTypeRef) -> Id {
    if receiver.is_null() || known {
        // Already concrete: `-init` on one of those is the identity, and
        // replacing it would throw away contents the caller just made.
        return receiver;
    }
    let object = concrete() as Id;
    // The placeholder's one reference was the caller's, and it belongs to what
    // is returned in its place.
    objc::arc::release(receiver);
    object
}

unsafe extern "C" fn init_mutable_array(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an object of this class.
    let known = unsafe { collections::array(receiver as CFTypeRef) }.is_some();
    // SAFETY: as above, with a fresh empty array.
    unsafe {
        cluster_init(receiver, known, || {
            collections::new_array(&[], Policy::from_type_callbacks(), true)
        })
    }
}

/// `-initWithCapacity:`, where the capacity is a hint this store has no use for.
unsafe extern "C" fn init_mutable_array_with_capacity(
    receiver: Id,
    sel: Sel,
    _capacity: usize,
) -> Id {
    unsafe { init_mutable_array(receiver, sel) }
}

unsafe extern "C" fn init_mutable_dictionary(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an object of this class.
    let known = unsafe { collections::dictionary(receiver as CFTypeRef) }.is_some();
    // SAFETY: as above, with a fresh empty dictionary.
    unsafe {
        cluster_init(receiver, known, || {
            collections::new_dictionary(
                &[],
                Policy::from_key_callbacks(),
                Policy::from_type_callbacks(),
                true,
            )
        })
    }
}

unsafe extern "C" fn init_mutable_dictionary_with_capacity(
    receiver: Id,
    sel: Sel,
    _capacity: usize,
) -> Id {
    unsafe { init_mutable_dictionary(receiver, sel) }
}

unsafe extern "C" fn new_dictionary(_class: Id, _sel: Sel) -> Id {
    collections::new_dictionary(
        &[],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        true,
    ) as Id
}

unsafe extern "C" fn dictionary(_class: Id, _sel: Sel) -> Id {
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(new_dictionary(_class, _sel) as CFTypeRef) as Id }
}

/// `+[NSDictionary dictionary]`: an empty dictionary that stays empty.
unsafe extern "C" fn empty_dictionary(_class: Id, _sel: Sel) -> Id {
    let policy = corefoundation::callbacks::Policy::from_type_callbacks();
    let dictionary = collections::new_dictionary(&[], policy, policy, false);
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(dictionary) as Id }
}

unsafe extern "C" fn dictionary_with_capacity(_class: Id, _sel: Sel, _capacity: usize) -> Id {
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(new_dictionary(_class, _sel) as CFTypeRef) as Id }
}

/// `+dictionaryWithObject:forKey:`, the one-pair spelling.
unsafe extern "C" fn dictionary_with_object_for_key(
    class: Id,
    _sel: Sel,
    object: Id,
    key: Id,
) -> Id {
    if object.is_null() || key.is_null() {
        return std::ptr::null_mut();
    }
    let dictionary = collections::new_dictionary(
        &[(key as CFTypeRef, object as CFTypeRef)],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        mutable_receiver(class, c"NSMutableDictionary"),
    );
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(dictionary) as Id }
}

/// `+dictionaryWithObjects:forKeys:count:`, the counted spelling.
unsafe extern "C" fn dictionary_with_objects_for_keys_count(
    class: Id,
    _sel: Sel,
    objects: *const Id,
    keys: *const Id,
    count: usize,
) -> Id {
    if (objects.is_null() || keys.is_null()) && count > 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller says both arrays cover `count` entries.
    let pairs: Vec<(CFTypeRef, CFTypeRef)> = match count {
        0 => Vec::new(),
        _ => unsafe {
            std::slice::from_raw_parts(keys as *const CFTypeRef, count)
                .iter()
                .copied()
                .zip(
                    std::slice::from_raw_parts(objects as *const CFTypeRef, count)
                        .iter()
                        .copied(),
                )
                .collect()
        },
    };
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_dictionary(
            &pairs,
            Policy::from_key_callbacks(),
            Policy::from_type_callbacks(),
            mutable_receiver(class, c"NSMutableDictionary"),
        )) as Id
    }
}

unsafe extern "C" fn data_length(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: a data object.
    unsafe { api::CFDataGetLength(receiver as CFTypeRef) as usize }
}

unsafe extern "C" fn data_bytes(receiver: Id, _sel: Sel) -> *const u8 {
    // SAFETY: a data object.
    unsafe { api::CFDataGetBytePtr(receiver as CFTypeRef) }
}

unsafe extern "C" fn data_with_bytes(_class: Id, _sel: Sel, bytes: *const u8, length: usize) -> Id {
    // SAFETY: the caller says `bytes` covers `length` bytes.
    unsafe { api::CFAutorelease(api::CFDataCreate(std::ptr::null(), bytes, length as isize)) as Id }
}

/// `+[NSData dataWithBytesNoCopy:length:]` with an owned compatibility copy.
///
/// CoreFoundation's data storage owns a `Vec`, so use the same compatibility
/// behavior as `CFDataCreateWithBytesNoCopy` rather than borrowing guest memory.
unsafe extern "C" fn data_with_bytes_no_copy(
    _class: Id,
    _sel: Sel,
    bytes: *const u8,
    length: usize,
) -> Id {
    // SAFETY: the caller says `bytes` covers `length` bytes.
    unsafe {
        api::CFAutorelease(api::CFDataCreateWithBytesNoCopy(
            std::ptr::null(),
            bytes,
            length as isize,
            std::ptr::null(),
        )) as Id
    }
}

/// `+[NSData dataWithBytesNoCopy:length:freeWhenDone:]`, which is the copy above:
/// the buffer a guest asked to be freed is simply left to it.
unsafe extern "C" fn data_with_bytes_no_copy_free(
    class: Id,
    sel: Sel,
    bytes: *const u8,
    length: usize,
    _free_when_done: u8,
) -> Id {
    // SAFETY: the same contract.
    unsafe { data_with_bytes_no_copy(class, sel, bytes, length) }
}

/// The bytes of a data object, or empty for nil or anything else.
///
/// # Safety
///
/// `object` is nil or a live object.
unsafe fn data_contents<'a>(object: Id) -> &'a [u8] {
    match unsafe { collections::data(object as CFTypeRef) } {
        Some(bytes) => bytes.as_slice(),
        None => &[],
    }
}

/// `bytes` for `length` bytes as a slice; empty for a null pointer.
///
/// # Safety
///
/// Non-null `bytes` covers `length` bytes.
unsafe fn raw<'a>(bytes: *const u8, length: usize) -> &'a [u8] {
    match bytes.is_null() || length == 0 {
        true => &[],
        false => unsafe { std::slice::from_raw_parts(bytes, length) },
    }
}

/// Every `-init...` below answers a new object in place of the one `+alloc`
/// made, as the string initialisers do: data here is a CoreFoundation object
/// whatever class was asked for, and one class serves both mutabilities.
fn replace_receiver(receiver: Id, bytes: Vec<u8>) -> Id {
    objc::arc::release(receiver);
    collections::new_data(bytes) as Id
}

fn autoreleased_data(bytes: Vec<u8>) -> Id {
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(collections::new_data(bytes)) as Id }
}

unsafe extern "C" fn data_init(receiver: Id, _sel: Sel) -> Id {
    replace_receiver(receiver, Vec::new())
}

unsafe extern "C" fn data_init_with_data(receiver: Id, _sel: Sel, other: Id) -> Id {
    // SAFETY: a data object or nil.
    replace_receiver(receiver, unsafe { data_contents(other) }.to_vec())
}

unsafe extern "C" fn data_init_with_bytes(
    receiver: Id,
    _sel: Sel,
    bytes: *const u8,
    length: usize,
) -> Id {
    // SAFETY: the caller says `bytes` covers `length` bytes.
    replace_receiver(receiver, unsafe { raw(bytes, length) }.to_vec())
}

/// `-initWithBytesNoCopy:length:freeWhenDone:`. Copied, as
/// [`data_with_bytes_no_copy`] explains, and the caller's buffer released
/// now when it handed it over.
unsafe extern "C" fn data_init_no_copy_free(
    receiver: Id,
    _sel: Sel,
    bytes: *mut u8,
    length: usize,
    free: bool,
) -> Id {
    // SAFETY: the caller says `bytes` covers `length` bytes.
    let copy = unsafe { raw(bytes, length) }.to_vec();
    if free && !bytes.is_null() {
        // SAFETY: a `malloc` block the caller gave away.
        unsafe { darwin::heap::free(bytes.cast()) };
    }
    replace_receiver(receiver, copy)
}

unsafe extern "C" fn data_init_no_copy(
    receiver: Id,
    sel: Sel,
    bytes: *mut u8,
    length: usize,
) -> Id {
    // SAFETY: forwarded; `-initWithBytesNoCopy:length:` frees when done.
    unsafe { data_init_no_copy_free(receiver, sel, bytes, length, true) }
}

unsafe extern "C" fn data_init_with_contents_of_file(receiver: Id, sel: Sel, path: Id) -> Id {
    objc::arc::release(receiver);
    // SAFETY: forwarded; the answer is autoreleased, and `-init` answers +1.
    objc::arc::retain(unsafe { data_with_contents_of_file(receiver, sel, path) })
}

unsafe extern "C" fn data_init_with_capacity(receiver: Id, _sel: Sel, capacity: usize) -> Id {
    replace_receiver(receiver, Vec::with_capacity(capacity.min(1 << 24)))
}

unsafe extern "C" fn data_init_with_length(receiver: Id, _sel: Sel, length: usize) -> Id {
    replace_receiver(receiver, vec![0; length])
}

unsafe extern "C" fn data_empty(_class: Id, _sel: Sel) -> Id {
    autoreleased_data(Vec::new())
}

unsafe extern "C" fn data_with_data(_class: Id, _sel: Sel, other: Id) -> Id {
    // SAFETY: a data object or nil.
    autoreleased_data(unsafe { data_contents(other) }.to_vec())
}

unsafe extern "C" fn data_with_capacity(_class: Id, _sel: Sel, capacity: usize) -> Id {
    autoreleased_data(Vec::with_capacity(capacity.min(1 << 24)))
}

unsafe extern "C" fn data_with_length(_class: Id, _sel: Sel, length: usize) -> Id {
    autoreleased_data(vec![0; length])
}

unsafe extern "C" fn data_copy(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a data object.
    collections::new_data(unsafe { data_contents(receiver) }.to_vec()) as Id
}

unsafe extern "C" fn data_copy_with_zone(
    receiver: Id,
    sel: Sel,
    _zone: *mut std::ffi::c_void,
) -> Id {
    // SAFETY: forwarded.
    unsafe { data_copy(receiver, sel) }
}

unsafe extern "C" fn data_is_equal(receiver: Id, _sel: Sel, other: Id) -> bool {
    // SAFETY: a data object, and nil or a live object; anything that is not
    // data is not equal.
    let Some(theirs) = (unsafe { collections::data(other as CFTypeRef) }) else {
        return false;
    };
    (unsafe { data_contents(receiver) }) == theirs.as_slice()
}

unsafe extern "C" fn data_hash(receiver: Id, _sel: Sel) -> usize {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // SAFETY: a data object.
    unsafe { data_contents(receiver) }.hash(&mut hasher);
    hasher.finish() as usize
}

unsafe extern "C" fn data_get_bytes_length(receiver: Id, _sel: Sel, out: *mut u8, length: usize) {
    // SAFETY: a data object.
    let bytes = unsafe { data_contents(receiver) };
    let count = length.min(bytes.len());
    if !out.is_null() {
        // SAFETY: the caller says `out` holds `length` bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, count) };
    }
}

unsafe extern "C" fn data_get_bytes(receiver: Id, sel: Sel, out: *mut u8) {
    // SAFETY: the caller's buffer holds the whole of the data.
    let length = unsafe { data_contents(receiver) }.len();
    unsafe { data_get_bytes_length(receiver, sel, out, length) };
}

/// The bytes `location..location+length`, or `NSRangeException` by name.
///
/// # Safety
///
/// `receiver` is a data object.
unsafe fn data_range<'a>(receiver: Id, location: usize, length: usize, what: &str) -> &'a [u8] {
    let bytes = unsafe { data_contents(receiver) };
    match location
        .checked_add(length)
        .filter(|end| *end <= bytes.len())
    {
        Some(end) => &bytes[location..end],
        None => objc::fail!(
            "{what}: range {{{location}, {length}}} exceeds data length {}",
            bytes.len()
        ),
    }
}

unsafe extern "C" fn data_get_bytes_range(
    receiver: Id,
    _sel: Sel,
    out: *mut u8,
    location: usize,
    length: usize,
) {
    // SAFETY: a data object.
    let bytes = unsafe { data_range(receiver, location, length, "-getBytes:range:") };
    if !out.is_null() {
        // SAFETY: the caller says `out` holds `length` bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    }
}

unsafe extern "C" fn data_subdata(receiver: Id, _sel: Sel, location: usize, length: usize) -> Id {
    // SAFETY: a data object.
    autoreleased_data(
        unsafe { data_range(receiver, location, length, "-subdataWithRange:") }.to_vec(),
    )
}

unsafe extern "C" fn data_set_length(receiver: Id, _sel: Sel, length: usize) {
    // SAFETY: a data object.
    if let Some(bytes) = unsafe { collections::data(receiver as CFTypeRef) } {
        bytes.resize(length, 0);
    }
}

unsafe extern "C" fn data_increase_length(receiver: Id, _sel: Sel, extra: usize) {
    // SAFETY: a data object.
    if let Some(bytes) = unsafe { collections::data(receiver as CFTypeRef) } {
        let length = bytes.len().saturating_add(extra);
        bytes.resize(length, 0);
    }
}

unsafe extern "C" fn data_append_data(receiver: Id, _sel: Sel, other: Id) {
    // SAFETY: data objects; copied first, since `other` may be the receiver.
    let theirs = unsafe { data_contents(other) }.to_vec();
    if let Some(bytes) = unsafe { collections::data(receiver as CFTypeRef) } {
        bytes.extend_from_slice(&theirs);
    }
}

unsafe extern "C" fn data_replace_bytes(
    receiver: Id,
    _sel: Sel,
    location: usize,
    length: usize,
    with: *const u8,
) {
    // SAFETY: a data object; `with` covers `length` bytes by contract.
    unsafe {
        data_range(
            receiver,
            location,
            length,
            "-replaceBytesInRange:withBytes:",
        )
    };
    let replacement = unsafe { raw(with, length) }.to_vec();
    if let Some(bytes) = unsafe { collections::data(receiver as CFTypeRef) } {
        bytes[location..location + length].copy_from_slice(&replacement);
    }
}

unsafe extern "C" fn data_reset_bytes(receiver: Id, _sel: Sel, location: usize, length: usize) {
    // SAFETY: a data object.
    unsafe { data_range(receiver, location, length, "-resetBytesInRange:") };
    if let Some(bytes) = unsafe { collections::data(receiver as CFTypeRef) } {
        bytes[location..location + length].fill(0);
    }
}

unsafe extern "C" fn append_bytes(receiver: Id, _sel: Sel, bytes: *const u8, length: usize) {
    // SAFETY: a data object.
    let Some(d) = (unsafe { collections::data(receiver as CFTypeRef) }) else {
        return;
    };
    if bytes.is_null() {
        return;
    }
    // SAFETY: the caller says `bytes` covers `length` bytes.
    d.extend_from_slice(unsafe { std::slice::from_raw_parts(bytes, length) });
}

unsafe extern "C" fn int_value(receiver: Id, _sel: Sel) -> i32 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0, |n| n.as_i64() as i32)
}

/// The narrower integer accessors.
///
/// Each truncates the stored value the way a C cast does, which is what Apple's
/// do: an `NSNumber` holds one value and every accessor is a *conversion* of
/// it, not a claim about how it was made.
unsafe extern "C" fn unsigned_int_value(receiver: Id, _sel: Sel) -> u32 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0, |n| n.as_i64() as u32)
}

unsafe extern "C" fn short_value(receiver: Id, _sel: Sel) -> i16 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0, |n| n.as_i64() as i16)
}

unsafe extern "C" fn unsigned_short_value(receiver: Id, _sel: Sel) -> u16 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0, |n| n.as_i64() as u16)
}

/// `-objCType`: the encoding of the canonical type the number was made with,
/// `c` for a boolean, which is a `kCFNumberCharType` here.
unsafe extern "C" fn obj_c_type(receiver: Id, _sel: Sel) -> *const std::ffi::c_char {
    use corefoundation::collections::number_type as t;
    // SAFETY: a number.
    let encoding: &'static std::ffi::CStr =
        match unsafe { corefoundation::collections::number_value(receiver as CFTypeRef) } {
            Some((_, t::SINT8 | t::CHAR)) => c"c",
            Some((_, t::SINT16 | t::SHORT)) => c"s",
            Some((_, t::SINT32 | t::INT)) => c"i",
            Some((_, t::FLOAT32 | t::FLOAT)) => c"f",
            Some((_, t::FLOAT64 | t::DOUBLE | t::CGFLOAT)) => c"d",
            _ => c"q",
        };
    encoding.as_ptr()
}

unsafe extern "C" fn char_value(receiver: Id, _sel: Sel) -> i8 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0, |n| n.as_i64() as i8)
}

unsafe extern "C" fn unsigned_char_value(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0, |n| n.as_i64() as u8)
}

/// Whether two numbers hold the same value: exactly, for two integers, so
/// that values past 2^53 are not folded onto their neighbours.
fn numbers_equal(a: collections::Number, b: collections::Number) -> bool {
    use collections::Number::Integer;
    match (a, b) {
        (Integer(x), Integer(y)) => x == y,
        _ => a.as_f64() == b.as_f64(),
    }
}

/// `-isEqualToNumber:` and `-isEqual:`; a non-number is never equal.
unsafe extern "C" fn number_is_equal(receiver: Id, _sel: Sel, other: Id) -> bool {
    if receiver == other {
        return true;
    }
    // SAFETY: numbers if they are ours or the compiler's constants.
    let (Some(a), Some(b)) = (
        unsafe { collections::number(receiver as CFTypeRef) },
        unsafe { collections::number(other as CFTypeRef) },
    ) else {
        return false;
    };
    numbers_equal(a, b)
}

/// `-hash`, equal for numbers `-isEqual:` calls equal: an integral float
/// hashes as the integer.
unsafe extern "C" fn number_hash(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: a number.
    let Some(n) = (unsafe { collections::number(receiver as CFTypeRef) }) else {
        return receiver as usize;
    };
    match n {
        collections::Number::Float(f) if f.fract() != 0.0 || !f.is_finite() => f.to_bits() as usize,
        n => n.as_i64() as usize,
    }
}

/// `-compare:`: `NSOrderedAscending` is -1, `NSOrderedDescending` is 1.
unsafe extern "C" fn number_compare(receiver: Id, _sel: Sel, other: Id) -> isize {
    // SAFETY: numbers if they are ours or the compiler's constants.
    let (Some(a), Some(b)) = (
        unsafe { collections::number(receiver as CFTypeRef) },
        unsafe { collections::number(other as CFTypeRef) },
    ) else {
        objc::fail!("-[NSNumber compare:]: the argument is not a number");
    };
    use collections::Number::Integer;
    let order = match (a, b) {
        (Integer(x), Integer(y)) => x.cmp(&y),
        _ => a
            .as_f64()
            .partial_cmp(&b.as_f64())
            .unwrap_or(std::cmp::Ordering::Equal),
    };
    order as isize
}

unsafe extern "C" fn integer_value(receiver: Id, _sel: Sel) -> i64 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0, |n| n.as_i64())
}

unsafe extern "C" fn double_value(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0.0, |n| n.as_f64())
}

unsafe extern "C" fn float_value(receiver: Id, _sel: Sel) -> f32 {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.map_or(0.0, |n| n.as_f64() as f32)
}

unsafe extern "C" fn bool_value(receiver: Id, _sel: Sel) -> bool {
    // SAFETY: a number.
    unsafe { collections::number(receiver as CFTypeRef) }.is_some_and(|n| n.as_i64() != 0)
}

/// `-stringValue`, which is also `-description` for a number.
///
/// An integer prints as itself; a float prints the way Rust's `{}` does, which
/// is the shortest text that reads back as the same double - the same rule
/// Apple's uses, so `@(9.25)` is `9.25` and `@(9.0)` is `9` on both.
unsafe extern "C" fn string_value(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a number.
    let text = match unsafe { collections::number(receiver as CFTypeRef) } {
        Some(collections::Number::Integer(value)) => value.to_string(),
        Some(collections::Number::Float(value)) => value.to_string(),
        // Not a number this runtime made. Apple would raise; an empty string is
        // the answer that lets the caller print something and carry on.
        None => String::new(),
    };
    let string = crate::string::from_str(&text);
    // SAFETY: a string this call just created.
    unsafe { api::CFAutorelease(string as CFTypeRef) as Id }
}

unsafe extern "C" fn number_with_int(_class: Id, _sel: Sel, value: i32) -> Id {
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_number_of_type(
            collections::Number::Integer(value as i64),
            collections::number_type::SINT32,
        )) as Id
    }
}

unsafe extern "C" fn number_with_integer(_class: Id, _sel: Sel, value: i64) -> Id {
    // SAFETY: a fresh object.
    unsafe {
        api::CFAutorelease(collections::new_number(collections::Number::Integer(value))) as Id
    }
}

/// The unsigned 64-bit constructors. A value past `i64::MAX` is kept as its bit
/// pattern, which `-unsignedLongLongValue` reads back unchanged.
unsafe extern "C" fn number_with_unsigned_long(class: Id, sel: Sel, value: u64) -> Id {
    // SAFETY: the same receiver and selector, forwarded.
    unsafe { number_with_integer(class, sel, value as i64) }
}

unsafe extern "C" fn number_with_unsigned_int(class: Id, sel: Sel, value: u32) -> Id {
    // SAFETY: as above.
    unsafe { number_with_integer(class, sel, i64::from(value)) }
}

unsafe extern "C" fn number_with_short(class: Id, sel: Sel, value: i16) -> Id {
    // SAFETY: as above.
    unsafe { number_with_int(class, sel, i32::from(value)) }
}

unsafe extern "C" fn number_with_unsigned_short(class: Id, sel: Sel, value: u16) -> Id {
    // SAFETY: as above.
    unsafe { number_with_int(class, sel, i32::from(value)) }
}

unsafe extern "C" fn number_with_char(class: Id, sel: Sel, value: i8) -> Id {
    // SAFETY: as above.
    unsafe { number_with_int(class, sel, i32::from(value)) }
}

unsafe extern "C" fn number_with_unsigned_char(class: Id, sel: Sel, value: u8) -> Id {
    // SAFETY: as above.
    unsafe { number_with_int(class, sel, i32::from(value)) }
}

unsafe extern "C" fn number_with_float(class: Id, sel: Sel, value: f32) -> Id {
    // SAFETY: as above.
    unsafe { number_with_double(class, sel, f64::from(value)) }
}

unsafe extern "C" fn number_with_double(_class: Id, _sel: Sel, value: f64) -> Id {
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(collections::new_number(collections::Number::Float(value))) as Id }
}

/// `+numberWithBool:` is `kCFBooleanTrue` or `kCFBooleanFalse` itself, as
/// Apple's is: a guest compares the answer against `@YES` by identity, and
/// `CFGetTypeID` of it is the boolean type.
unsafe extern "C" fn number_with_bool(_class: Id, _sel: Sel, value: bool) -> Id {
    // Not autoreleased: the two objects live for the process.
    collections::boolean(value) as Id
}

/// The `-initWith...:` forms of the constructors above. A number is a
/// CoreFoundation object whatever `+alloc` made, so the placeholder is
/// released and a new number returned in its place, owned by the caller.
macro_rules! number_init {
    ($($name:ident => $with:ident: $ty:ty),* $(,)?) => {$(
        unsafe extern "C" fn $name(receiver: Id, sel: Sel, value: $ty) -> Id {
            objc::arc::release(receiver);
            // SAFETY: the constructor ignores its receiver.
            objc::arc::retain(unsafe { $with(std::ptr::null_mut(), sel, value) })
        }
    )*};
}

number_init! {
    number_init_int => number_with_int: i32,
    number_init_integer => number_with_integer: i64,
    number_init_double => number_with_double: f64,
    number_init_bool => number_with_bool: bool,
    number_init_unsigned_long => number_with_unsigned_long: u64,
    number_init_unsigned_int => number_with_unsigned_int: u32,
    number_init_short => number_with_short: i16,
    number_init_unsigned_short => number_with_unsigned_short: u16,
    number_init_char => number_with_char: i8,
    number_init_unsigned_char => number_with_unsigned_char: u8,
    number_init_float => number_with_float: f32,
}

unsafe extern "C" fn set_with_object(_class: Id, _sel: Sel, object: Id) -> Id {
    let values: &[CFTypeRef] = match object.is_null() {
        true => &[],
        false => &[object as CFTypeRef],
    };
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(collections::new_set(values, Policy::from_type_callbacks())) as Id }
}

unsafe extern "C" fn set_with_capacity(_class: Id, _sel: Sel, _capacity: usize) -> Id {
    // SAFETY: a fresh object.
    unsafe { api::CFAutorelease(collections::new_set(&[], Policy::from_type_callbacks())) as Id }
}

unsafe extern "C" fn set_with_array(_class: Id, _sel: Sel, array: Id) -> Id {
    // SAFETY: the argument is an NSArray or nil.
    let values = unsafe { collections::array(array as CFTypeRef) }
        .map(|array| array.values.as_slice())
        .unwrap_or(&[]);
    // SAFETY: the new set retains its elements.
    unsafe { api::CFAutorelease(collections::new_set(values, Policy::from_type_callbacks())) as Id }
}

unsafe extern "C" fn set_count(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: a set.
    unsafe { collections::set(receiver as CFTypeRef) }.map_or(0, |set| set.values.len())
}

unsafe extern "C" fn set_contains_object(receiver: Id, _sel: Sel, object: Id) -> bool {
    // SAFETY: a set.
    unsafe { collections::set(receiver as CFTypeRef) }.is_some_and(|set| {
        set.values
            .iter()
            .any(|value| set.policy.equal(*value, object as CFTypeRef))
    })
}

/// `-addObject:`, which keeps the set's contents unique under its own equality.
unsafe extern "C" fn set_add_object(receiver: Id, _sel: Sel, object: Id) {
    if object.is_null() {
        objc::fail!("-[NSMutableSet addObject:]: object cannot be nil");
    }
    // SAFETY: a set.
    let Some(set) = (unsafe { collections::set(receiver as CFTypeRef) }) else {
        return;
    };
    let object = object as CFTypeRef;
    if !set
        .values
        .iter()
        .any(|value| set.policy.equal(*value, object))
    {
        set.values.push(set.policy.retain(object));
    }
}

unsafe extern "C" fn set_any_object(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a set.
    unsafe { collections::set(receiver as CFTypeRef) }
        .and_then(|set| set.values.first().copied())
        .map_or(std::ptr::null_mut(), |value| value as Id)
}

unsafe extern "C" fn set_enumerate(
    receiver: Id,
    _sel: Sel,
    state: *mut FastEnumerationState,
    _buffer: *mut Id,
    _len: usize,
) -> usize {
    // SAFETY: a set.
    let Some(set) = (unsafe { collections::set(receiver as CFTypeRef) }) else {
        return 0;
    };
    // SAFETY: the guest's own state structure, and storage that outlives the
    // loop.
    unsafe { enumerate(state, &mut set.values) }
}

pub(crate) fn install() {
    let set = objc::class::lookup(c"NSSet").expect("CoreFoundation registered NSSet");
    class_methods!(set, {
        "setWithObject:" => set_with_object, c"@24@0:8@16";
        "setWithCapacity:" => set_with_capacity, c"@24@0:8Q16";
        "setWithArray:" => set_with_array, c"@24@0:8@16";
    });
    methods!(set, {
        "count" => set_count, c"Q16@0:8";
        "isEqual:" => set_is_equal, c"B24@0:8@16";
        "isEqualToSet:" => is_equal_to_set, c"B24@0:8@16";
        "hash" => set_hash, c"Q16@0:8";
        "containsObject:" => set_contains_object, c"B24@0:8@16";
        "anyObject" => set_any_object, c"@16@0:8";
        "countByEnumeratingWithState:objects:count:" => set_enumerate, c"Q40@0:8^{?=Q^@^QQ5}16^@24Q32";
    });
    let mutable_set = objc::class::lookup(c"NSMutableSet").expect("NSMutableSet");
    methods!(mutable_set, {
        "addObject:" => set_add_object, c"v24@0:8@16";
    });

    let enumerator = corefoundation::base::abstract_class(c"NSEnumerator", None);
    let array_enumerator = corefoundation::base::register(
        c"__NSArrayEnumerator",
        Some(enumerator),
        std::mem::size_of::<ArrayEnumerator>(),
        false,
    );
    methods!(array_enumerator, {
        "dealloc" => enumerator_dealloc, c"v16@0:8";
        "nextObject" => enumerator_next_object, c"@16@0:8";
        "allObjects" => enumerator_all_objects, c"@16@0:8";
        "countByEnumeratingWithState:objects:count:" => enumerator_enumerate, c"Q40@0:8^{?=Q^@^QQ5}16^@24Q32";
    });

    let array = objc::class::lookup(c"NSArray").expect("CoreFoundation registered NSArray");
    let mutable_array = objc::class::lookup(c"NSMutableArray").expect("NSMutableArray");
    methods!(array, {
        "writeToFile:atomically:" => write_property_list_to_file, c"B28@0:8@16B24";
        "writeToURL:atomically:" => write_property_list_to_url, c"B28@0:8@16B24";
        "count" => count, c"Q16@0:8";
        "isEqual:" => array_is_equal, c"B24@0:8@16";
        "isEqualToArray:" => is_equal_to_array, c"B24@0:8@16";
        "hash" => array_hash, c"Q16@0:8";
        "objectAtIndex:" => object_at_index, c"@24@0:8Q16";
        "subarrayWithRange:" => subarray_with_range, c"@32@0:8{_NSRange=QQ}16";
        "objectAtIndexedSubscript:" => object_at_index, c"@24@0:8Q16";
        "containsObject:" => contains_object, c"B24@0:8@16";
        "indexOfObject:" => index_of_object, c"Q24@0:8@16";
        "indexOfObjectIdenticalTo:" => index_of_object_identical_to, c"Q24@0:8@16";
        "firstObject" => first_object, c"@16@0:8";
        "lastObject" => last_object, c"@16@0:8";
        "componentsJoinedByString:" => components_joined_by_string, c"@24@0:8@16";
        "valueForKey:" => array_value_for_key, c"@24@0:8@16";
        "sortedArrayUsingSelector:" => sorted_array_using_selector, c"@24@0:8:16";
        "sortedArrayUsingComparator:" => sorted_array_using_comparator, c"@24@0:8@?16";
        "sortedArrayUsingFunction:context:" => sorted_array_using_function, c"@32@0:8^?16^v24";
        "filteredArrayUsingPredicate:" => filtered_array_using_predicate, c"@24@0:8@16";
        "indexOfObjectWithOptions:passingTest:" => index_of_object_with_options_passing_test, c"Q32@0:8Q16@?24";
        "indexOfObjectPassingTest:" => index_of_object_passing_test, c"Q24@0:8@?16";
        "enumerateObjectsUsingBlock:" => enumerate_objects, c"v24@0:8@?16";
        "enumerateObjectsWithOptions:usingBlock:" => enumerate_objects_with_options, c"v32@0:8Q16@?24";
        "makeObjectsPerformSelector:" => make_objects_perform_selector, c"v24@0:8:16";
        "makeObjectsPerformSelector:withObject:" => make_objects_perform_selector_with_object, c"v32@0:8:16@24";
        "countByEnumeratingWithState:objects:count:" => array_enumerate, c"Q40@0:8^{?=Q^@^QQ5}16^@24Q32";
        "description" => array_description, c"@16@0:8";
        "objectEnumerator" => object_enumerator, c"@16@0:8";
        "reverseObjectEnumerator" => reverse_object_enumerator, c"@16@0:8";
        "copy" => array_copy, c"@16@0:8";
        "copyWithZone:" => array_copy_with_zone, c"@24@0:8^v16";
        "mutableCopy" => array_mutable_copy, c"@16@0:8";
        "mutableCopyWithZone:" => array_mutable_copy_with_zone, c"@24@0:8^v16";
    });
    class_methods!(array, {
        "array" => array_fn, c"@16@0:8";
        "arrayWithObject:" => array_with_object, c"@24@0:8@16";
        "arrayWithObjects:count:" => array_with_objects_count, c"@32@0:8^@16Q24";
        "arrayWithContentsOfFile:" => array_with_contents_of_file, c"@24@0:8@16";
        "arrayWithContentsOfURL:" => array_with_contents_of_url, c"@24@0:8@16";
        "arrayWithContentsOfURL:error:" => array_with_contents_of_url_error, c"@32@0:8@16^@24";
    });
    methods!(array, {
        "initWithObjects:count:" => array_init_with_objects_count, c"@32@0:8^@16Q24";
        "initWithContentsOfFile:" => array_init_with_contents_of_file, c"@24@0:8@16";
        "initWithContentsOfURL:" => array_init_with_contents_of_url, c"@24@0:8@16";
        "initWithContentsOfURL:error:" => array_init_with_contents_of_url_error, c"@32@0:8@16^@24";
    });
    methods!(mutable_array, {
        "init" => init_mutable_array, c"@16@0:8";
        "initWithCapacity:" => init_mutable_array_with_capacity, c"@24@0:8Q16";
        "addObject:" => add_object, c"v24@0:8@16";
        "addObjectsFromArray:" => add_objects_from_array, c"v24@0:8@16";
        "removeObjectAtIndex:" => remove_object_at_index, c"v24@0:8Q16";
        "removeObject:" => remove_object, c"v24@0:8@16";
        "removeObjectIdenticalTo:" => remove_object_identical_to, c"v24@0:8@16";
        "removeLastObject" => remove_last_object, c"v16@0:8";
        "insertObject:atIndex:" => insert_object_at_index, c"v32@0:8@16Q24";
        "replaceObjectAtIndex:withObject:" => replace_object_at_index, c"v32@0:8Q16@24";
        "setObject:atIndexedSubscript:" => set_object_at_indexed_subscript, c"v32@0:8@16Q24";
        "sortUsingFunction:context:" => sort_using_function, c"v32@0:8^?16^v24";
        "removeAllObjects" => remove_all_objects, c"v16@0:8";
    });
    class_methods!(mutable_array, {
        "new" => new_array, c"@16@0:8";
        "array" => array_fn, c"@16@0:8";
        "arrayWithCapacity:" => array_with_capacity, c"@24@0:8Q16";
        "arrayWithArray:" => array_with_array, c"@24@0:8@16";
    });

    let dictionary_class =
        objc::class::lookup(c"NSDictionary").expect("CoreFoundation registered NSDictionary");
    let mutable_dictionary =
        objc::class::lookup(c"NSMutableDictionary").expect("NSMutableDictionary");
    methods!(dictionary_class, {
        "writeToFile:atomically:" => write_property_list_to_file, c"B28@0:8@16B24";
        "writeToURL:atomically:" => write_property_list_to_url, c"B28@0:8@16B24";
        "count" => dictionary_count, c"Q16@0:8";
        "objectForKey:" => object_for_key, c"@24@0:8@16";
        "objectForKeyedSubscript:" => object_for_key, c"@24@0:8@16";
        "allKeys" => all_keys, c"@16@0:8";
        "allValues" => all_values, c"@16@0:8";
        "description" => dictionary_description, c"@16@0:8";
        "copy" => dictionary_copy, c"@16@0:8";
        "copyWithZone:" => dictionary_copy_with_zone, c"@24@0:8^v16";
        "mutableCopy" => dictionary_mutable_copy, c"@16@0:8";
        "mutableCopyWithZone:" => dictionary_mutable_copy_with_zone, c"@24@0:8^v16";
        "valueForKey:" => dictionary_value_for_key, c"@24@0:8@16";
        "allKeysForObject:" => all_keys_for_object, c"@24@0:8@16";
        "objectsForKeys:notFoundMarker:" => objects_for_keys_not_found_marker, c"@32@0:8@16@24";
        "isEqualToDictionary:" => is_equal_to_dictionary, c"B24@0:8@16";
        "isEqual:" => dictionary_is_equal, c"B24@0:8@16";
        "hash" => dictionary_hash, c"Q16@0:8";
        "keyEnumerator" => key_enumerator, c"@16@0:8";
        "objectEnumerator" => dictionary_object_enumerator, c"@16@0:8";
        "countByEnumeratingWithState:objects:count:" => dictionary_enumerate, c"Q40@0:8^{?=Q^@^QQ5}16^@24Q32";
        "enumerateKeysAndObjectsUsingBlock:" => enumerate_keys_and_objects, c"v24@0:8@?16";
        "enumerateKeysAndObjectsWithOptions:usingBlock:" => enumerate_keys_and_objects_with_options, c"v32@0:8Q16@?24";
    });
    class_methods!(dictionary_class, {
        "dictionary" => empty_dictionary, c"@16@0:8";
        "dictionaryWithObjects:forKeys:count:" => dictionary_with_objects_for_keys_count, c"@40@0:8^@16^@24Q32";
        "dictionaryWithObject:forKey:" => dictionary_with_object_for_key, c"@32@0:8@16@24";
        "dictionaryWithDictionary:" => dictionary_with_dictionary, c"@24@0:8@16";
        "dictionaryWithObjects:forKeys:" => dictionary_with_objects_for_keys, c"@32@0:8@16@24";
        "dictionaryWithContentsOfFile:" => dictionary_with_contents_of_file, c"@24@0:8@16";
        "dictionaryWithContentsOfURL:" => dictionary_with_contents_of_url, c"@24@0:8@16";
        "dictionaryWithContentsOfURL:error:" => dictionary_with_contents_of_url_error, c"@32@0:8@16^@24";
    });
    methods!(dictionary_class, {
        "initWithObjects:forKeys:count:" => dictionary_init_with_objects_for_keys_count, c"@40@0:8^@16^@24Q32";
        "initWithDictionary:" => dictionary_init_with_dictionary, c"@24@0:8@16";
        "initWithContentsOfFile:" => dictionary_init_with_contents_of_file, c"@24@0:8@16";
        "initWithContentsOfURL:" => dictionary_init_with_contents_of_url, c"@24@0:8@16";
        "initWithContentsOfURL:error:" => dictionary_init_with_contents_of_url_error, c"@32@0:8@16^@24";
    });
    methods!(mutable_dictionary, {
        "init" => init_mutable_dictionary, c"@16@0:8";
        "initWithCapacity:" => init_mutable_dictionary_with_capacity, c"@24@0:8Q16";
        "setObject:forKey:" => set_object_for_key, c"v32@0:8@16@24";
        "setObject:forKeyedSubscript:" => set_object_for_key, c"v32@0:8@16@24";
        "removeObjectForKey:" => remove_object_for_key, c"v24@0:8@16";
        "removeAllObjects" => dictionary_remove_all_objects, c"v16@0:8";
        "setValue:forKey:" => dictionary_set_value_for_key, c"v32@0:8@16@24";
        "addEntriesFromDictionary:" => add_entries_from_dictionary, c"v24@0:8@16";
        "setDictionary:" => set_dictionary, c"v24@0:8@16";
        "removeObjectsForKeys:" => remove_objects_for_keys, c"v24@0:8@16";
    });
    class_methods!(mutable_dictionary, {
        "new" => new_dictionary, c"@16@0:8";
        "dictionary" => dictionary, c"@16@0:8";
        "dictionaryWithCapacity:" => dictionary_with_capacity, c"@24@0:8Q16";
    });

    let predicate = corefoundation::base::register(
        c"NSPredicate",
        None,
        std::mem::size_of::<Predicate>(),
        false,
    );
    methods!(predicate, {
        "dealloc" => predicate_dealloc, c"v16@0:8";
    });
    class_methods!(predicate, {
        "predicateWithBlock:" => predicate_with_block, c"@24@0:8@?16";
    });

    let data = objc::class::lookup(c"NSData").expect("CoreFoundation registered NSData");
    let mutable_data = objc::class::lookup(c"NSMutableData").expect("NSMutableData");
    methods!(data, {
        "length" => data_length, c"Q16@0:8";
        "bytes" => data_bytes, c"^v16@0:8";
    });
    class_methods!(data, {
        "dataWithBytes:length:" => data_with_bytes, c"@32@0:8^v16Q24";
        "dataWithBytesNoCopy:length:" => data_with_bytes_no_copy, c"@32@0:8^v16Q24";
        "dataWithBytesNoCopy:length:freeWhenDone:"
            => data_with_bytes_no_copy_free, c"@36@0:8^v16Q24c32";
        "dataWithContentsOfFile:" => data_with_contents_of_file, c"@24@0:8@16";
        "dataWithContentsOfFile:options:error:"
            => data_with_contents_of_file_options, c"@40@0:8@16Q24^@32";
    });
    methods!(data, {
        "writeToFile:atomically:" => write_to_file, c"B28@0:8@16c24";
        "init" => data_init, c"@16@0:8";
        "initWithData:" => data_init_with_data, c"@24@0:8@16";
        "initWithBytes:length:" => data_init_with_bytes, c"@32@0:8r^v16Q24";
        "initWithBytesNoCopy:length:" => data_init_no_copy, c"@32@0:8^v16Q24";
        "initWithBytesNoCopy:length:freeWhenDone:" => data_init_no_copy_free, c"@36@0:8^v16Q24B32";
        "initWithContentsOfFile:" => data_init_with_contents_of_file, c"@24@0:8@16";
        "copy" => data_copy, c"@16@0:8";
        "copyWithZone:" => data_copy_with_zone, c"@24@0:8^v16";
        "mutableCopy" => data_copy, c"@16@0:8";
        "mutableCopyWithZone:" => data_copy_with_zone, c"@24@0:8^v16";
        "isEqual:" => data_is_equal, c"B24@0:8@16";
        "isEqualToData:" => data_is_equal, c"B24@0:8@16";
        "hash" => data_hash, c"Q16@0:8";
        "getBytes:" => data_get_bytes, c"v24@0:8^v16";
        "getBytes:length:" => data_get_bytes_length, c"v32@0:8^v16Q24";
        "getBytes:range:" => data_get_bytes_range, c"v40@0:8^v16{_NSRange=QQ}24";
        "subdataWithRange:" => data_subdata, c"@32@0:8{_NSRange=QQ}16";
    });
    class_methods!(data, {
        "data" => data_empty, c"@16@0:8";
        "dataWithData:" => data_with_data, c"@24@0:8@16";
    });
    methods!(mutable_data, {
        "appendBytes:length:" => append_bytes, c"v32@0:8^v16Q24";
        "mutableBytes" => data_bytes, c"^v16@0:8";
        "initWithCapacity:" => data_init_with_capacity, c"@24@0:8Q16";
        "initWithLength:" => data_init_with_length, c"@24@0:8Q16";
        "setLength:" => data_set_length, c"v24@0:8Q16";
        "increaseLengthBy:" => data_increase_length, c"v24@0:8Q16";
        "appendData:" => data_append_data, c"v24@0:8@16";
        "replaceBytesInRange:withBytes:" => data_replace_bytes, c"v40@0:8{_NSRange=QQ}16r^v32";
        "resetBytesInRange:" => data_reset_bytes, c"v32@0:8{_NSRange=QQ}16";
    });
    class_methods!(mutable_data, {
        "dataWithCapacity:" => data_with_capacity, c"@24@0:8Q16";
        "dataWithLength:" => data_with_length, c"@24@0:8Q16";
    });

    let number = objc::class::lookup(c"NSNumber").expect("CoreFoundation registered NSNumber");
    methods!(number, {
        "intValue" => int_value, c"i16@0:8";
        "isEqualToNumber:" => number_is_equal, c"B24@0:8@16";
        "isEqual:" => number_is_equal, c"B24@0:8@16";
        "hash" => number_hash, c"Q16@0:8";
        "compare:" => number_compare, c"q24@0:8@16";
        "integerValue" => integer_value, c"q16@0:8";
        "longValue" => integer_value, c"q16@0:8";
        "longLongValue" => integer_value, c"q16@0:8";
        "unsignedIntegerValue" => integer_value, c"Q16@0:8";
        "unsignedLongValue" => integer_value, c"Q16@0:8";
        "unsignedLongLongValue" => integer_value, c"Q16@0:8";
        "unsignedIntValue" => unsigned_int_value, c"I16@0:8";
        "shortValue" => short_value, c"s16@0:8";
        "unsignedShortValue" => unsigned_short_value, c"S16@0:8";
        "charValue" => char_value, c"c16@0:8";
        "objCType" => obj_c_type, c"r*16@0:8";
        "unsignedCharValue" => unsigned_char_value, c"C16@0:8";
        "doubleValue" => double_value, c"d16@0:8";
        "floatValue" => float_value, c"f16@0:8";
        "boolValue" => bool_value, c"B16@0:8";
        "stringValue" => string_value, c"@16@0:8";
        "description" => string_value, c"@16@0:8";
    });
    methods!(number, {
        "initWithInt:" => number_init_int, c"@24@0:8i16";
        "initWithInteger:" => number_init_integer, c"@24@0:8q16";
        "initWithLong:" => number_init_integer, c"@24@0:8q16";
        "initWithLongLong:" => number_init_integer, c"@24@0:8q16";
        "initWithDouble:" => number_init_double, c"@24@0:8d16";
        "initWithBool:" => number_init_bool, c"@24@0:8B16";
        "initWithUnsignedInteger:" => number_init_unsigned_long, c"@24@0:8Q16";
        "initWithUnsignedLong:" => number_init_unsigned_long, c"@24@0:8Q16";
        "initWithUnsignedLongLong:" => number_init_unsigned_long, c"@24@0:8Q16";
        "initWithUnsignedInt:" => number_init_unsigned_int, c"@24@0:8I16";
        "initWithShort:" => number_init_short, c"@24@0:8s16";
        "initWithUnsignedShort:" => number_init_unsigned_short, c"@24@0:8S16";
        "initWithChar:" => number_init_char, c"@24@0:8c16";
        "initWithUnsignedChar:" => number_init_unsigned_char, c"@24@0:8C16";
        "initWithFloat:" => number_init_float, c"@24@0:8f16";
    });
    class_methods!(number, {
        "numberWithInt:" => number_with_int, c"@24@0:8i16";
        "numberWithInteger:" => number_with_integer, c"@24@0:8q16";
        "numberWithLong:" => number_with_integer, c"@24@0:8q16";
        "numberWithLongLong:" => number_with_integer, c"@24@0:8q16";
        "numberWithDouble:" => number_with_double, c"@24@0:8d16";
        "numberWithBool:" => number_with_bool, c"@24@0:8B16";
        "numberWithUnsignedInteger:" => number_with_unsigned_long, c"@24@0:8Q16";
        "numberWithUnsignedLong:" => number_with_unsigned_long, c"@24@0:8Q16";
        "numberWithUnsignedLongLong:" => number_with_unsigned_long, c"@24@0:8Q16";
        "numberWithUnsignedInt:" => number_with_unsigned_int, c"@24@0:8I16";
        "numberWithShort:" => number_with_short, c"@24@0:8s16";
        "numberWithUnsignedShort:" => number_with_unsigned_short, c"@24@0:8S16";
        "numberWithChar:" => number_with_char, c"@24@0:8c16";
        "numberWithUnsignedChar:" => number_with_unsigned_char, c"@24@0:8C16";
        "numberWithFloat:" => number_with_float, c"@24@0:8f16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block literal as the compiler lays one out, carrying a pointer to
    /// what the test's invoke function records.
    #[repr(C)]
    struct Seen {
        isa: *const std::ffi::c_void,
        flags: i32,
        reserved: i32,
        invoke: *const std::ffi::c_void,
        descriptor: *const std::ffi::c_void,
        /// Indexes seen, and the indexes past which the block stops.
        seen: *mut Vec<(usize, Id)>,
        keep: *const dyn Fn(usize) -> bool,
    }

    unsafe extern "C" fn record(block: *const Seen, object: Id, at: usize, stop: *mut bool) {
        // SAFETY: a `Seen` of the test's, live for the enumeration.
        unsafe {
            let block = &*block;
            if (*block.keep)(at) {
                (*block.seen).push((at, object));
            } else {
                *stop = true;
            }
        }
    }

    fn enumerate(array: Id, options: usize, keep: &dyn Fn(usize) -> bool) -> Vec<(usize, Id)> {
        let mut seen = Vec::new();
        let block = Seen {
            isa: std::ptr::null(),
            flags: 0,
            reserved: 0,
            invoke: record as *const std::ffi::c_void,
            descriptor: std::ptr::null(),
            seen: &mut seen,
            // SAFETY: only read during the call below, while `keep` lives.
            keep: unsafe {
                std::mem::transmute::<&dyn Fn(usize) -> bool, *const dyn Fn(usize) -> bool>(keep)
            },
        };
        let sel = objc::sel::get("enumerateObjectsWithOptions:usingBlock:");
        // SAFETY: an array and a block of the selector's type.
        unsafe { enumerate_objects_with_options(array, sel, options, (&raw const block) as Id) };
        seen
    }

    /// Equal contents make equal collections with equal hashes, as
    /// swift-corelibs-foundation's `TestNSArray.test_equality` and
    /// `TestNSDictionary.test_equality` check.
    #[test]
    fn collections_with_equal_contents_are_equal_and_hash_alike() {
        crate::bootstrap();
        use corefoundation::collections::{Number, new_array, new_dictionary, new_number};
        let policy = corefoundation::callbacks::Policy::from_type_callbacks;
        let number = |n| new_number(Number::Integer(n));
        let (one, two, other_one) = (number(1), number(2), number(1));
        let a = new_array(&[one, two], policy(), false) as Id;
        let b = new_array(&[other_one, two], policy(), true) as Id;
        let reversed = new_array(&[two, one], policy(), false) as Id;
        let d = new_dictionary(&[(one, two)], policy(), policy(), false) as Id;
        let e = new_dictionary(&[(other_one, two)], policy(), policy(), true) as Id;
        let f = new_dictionary(&[(one, one)], policy(), policy(), false) as Id;
        let equal = |x: Id, y: Id| unsafe {
            corefoundation::api::CFEqual(x as CFTypeRef, y as CFTypeRef) != 0
        };
        let hash = |x: Id| unsafe { corefoundation::api::CFHash(x as CFTypeRef) };
        assert!(equal(a, b), "mutability does not matter");
        assert_eq!(hash(a), hash(b));
        assert!(!equal(a, reversed), "order does");
        assert!(equal(d, e));
        assert_eq!(hash(d), hash(e));
        assert!(!equal(d, f), "values do");
        assert!(!equal(a, d), "an array is not a dictionary");
    }

    /// Ported from swift-corelibs-foundation's `TestNSArray.test_enumerationUsingBlock`:
    /// run to completion, and stop half way, under each combination of the
    /// concurrent and reverse options. Its `at:` half needs `NSIndexSet`.
    #[test]
    fn an_array_enumerates_its_objects_with_a_block() {
        crate::bootstrap();
        let numbers: Vec<CFTypeRef> = (0..100i64)
            .map(|n| {
                corefoundation::collections::new_number(
                    corefoundation::collections::Number::Integer(n),
                )
            })
            .collect();
        let array = corefoundation::collections::new_array(
            &numbers,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        ) as Id;
        const CONCURRENT: usize = 1 << 0;
        for options in [
            0,
            CONCURRENT,
            ENUMERATION_REVERSE,
            CONCURRENT | ENUMERATION_REVERSE,
        ] {
            let reverse = options & ENUMERATION_REVERSE != 0;
            let all = enumerate(array, options, &|_| true);
            assert_eq!(all.len(), 100, "options {options}");
            for (at, object) in &all {
                // SAFETY: an array of this test's.
                assert_eq!(*object, unsafe {
                    object_at_index(array, std::ptr::null(), *at)
                });
            }
            let order: Vec<usize> = all.iter().map(|(at, _)| *at).collect();
            let expected: Vec<usize> = match reverse {
                false => (0..100).collect(),
                true => (0..100).rev().collect(),
            };
            assert_eq!(order, expected, "options {options}");

            let half = |at: usize| if reverse { at >= 50 } else { at < 50 };
            let mut stopped: Vec<usize> = enumerate(array, options, &half)
                .iter()
                .map(|(at, _)| *at)
                .collect();
            stopped.sort_unstable();
            let expected: Vec<usize> = (0..100).filter(|at| half(*at)).collect();
            assert_eq!(stopped, expected, "options {options}");
        }
        // The plain spelling is the forward walk.
        let mut seen = Vec::new();
        let keep: &dyn Fn(usize) -> bool = &|_| true;
        let block = Seen {
            isa: std::ptr::null(),
            flags: 0,
            reserved: 0,
            invoke: record as *const std::ffi::c_void,
            descriptor: std::ptr::null(),
            seen: &mut seen,
            // SAFETY: as in `enumerate`.
            keep: unsafe {
                std::mem::transmute::<&dyn Fn(usize) -> bool, *const dyn Fn(usize) -> bool>(keep)
            },
        };
        // SAFETY: as above.
        unsafe {
            enumerate_objects(
                array,
                objc::sel::get("enumerateObjectsUsingBlock:"),
                (&raw const block) as Id,
            )
        };
        assert_eq!(seen.first().map(|(at, _)| *at), Some(0));
        assert_eq!(seen.len(), 100);
        for number in numbers {
            // SAFETY: the references made above; the array holds its own.
            unsafe { api::CFRelease(number) };
        }
    }

    #[repr(C)]
    struct Pairs {
        isa: *const std::ffi::c_void,
        flags: i32,
        reserved: i32,
        invoke: *const std::ffi::c_void,
        descriptor: *const std::ffi::c_void,
        seen: *mut Vec<(Id, Id)>,
        stop_after: usize,
    }

    unsafe extern "C" fn record_pair(block: *const Pairs, key: Id, value: Id, stop: *mut bool) {
        // SAFETY: a `Pairs` of the test's, live for the enumeration.
        unsafe {
            let block = &*block;
            (*block.seen).push((key, value));
            if (*block.seen).len() >= block.stop_after {
                *stop = true;
            }
        }
    }

    /// Every key arrives with its own object, once, and `stop` ends the walk.
    #[test]
    fn a_dictionary_enumerates_its_keys_and_objects_with_a_block() {
        crate::bootstrap();
        let policy = corefoundation::callbacks::Policy::from_type_callbacks();
        let texts = [("foo", "bar"), ("whiz", "bang"), ("toil", "trouble")];
        let pairs: Vec<(CFTypeRef, CFTypeRef)> = texts
            .iter()
            .map(|(k, v)| {
                (
                    crate::string::from_str(k) as CFTypeRef,
                    crate::string::from_str(v) as CFTypeRef,
                )
            })
            .collect();
        let dictionary =
            corefoundation::collections::new_dictionary(&pairs, policy, policy, false) as Id;
        for stop_after in [usize::MAX, 1] {
            let mut seen = Vec::new();
            let block = Pairs {
                isa: std::ptr::null(),
                flags: 0,
                reserved: 0,
                invoke: record_pair as *const std::ffi::c_void,
                descriptor: std::ptr::null(),
                seen: &mut seen,
                stop_after,
            };
            let sel = objc::sel::get("enumerateKeysAndObjectsUsingBlock:");
            // SAFETY: a dictionary and a block of the selector's type.
            unsafe { enumerate_keys_and_objects(dictionary, sel, (&raw const block) as Id) };
            let mut got: Vec<(String, String)> = seen
                .iter()
                .map(|(k, v)| {
                    // SAFETY: strings this test made.
                    let text = |s: Id| {
                        String::from_utf8_lossy(&unsafe { crate::string::bytes(s) }).into_owned()
                    };
                    (text(*k), text(*v))
                })
                .collect();
            got.sort();
            match stop_after {
                1 => assert_eq!(got.len(), 1),
                _ => {
                    let mut want: Vec<(String, String)> = texts
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect();
                    want.sort();
                    assert_eq!(got, want);
                }
            }
        }
    }

    /// A dictionary written as a property list reads back as the same values,
    /// and one holding something a property list cannot is refused with
    /// nothing written.
    #[test]
    fn a_dictionary_is_written_as_a_property_list() {
        crate::bootstrap();
        let directory = std::env::temp_dir().join(format!("rosemary-plist-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("a scratch directory");
        let path = directory.join("written.plist");
        let policy = corefoundation::callbacks::Policy::from_type_callbacks();
        let key = crate::string::from_str("Name");
        let value = crate::string::from_str("League");
        let dictionary = corefoundation::collections::new_dictionary(
            &[(key as CFTypeRef, value as CFTypeRef)],
            policy,
            policy,
            false,
        ) as Id;
        let sel = objc::sel::get("writeToFile:atomically:");
        let target = crate::string::from_str(path.to_str().expect("a UTF-8 path"));
        // SAFETY: `BOOL (id, SEL, id, BOOL)`, with a dictionary and a string.
        assert_eq!(
            unsafe { write_property_list_to_file(dictionary, sel, target, 1) },
            1
        );
        let written = std::fs::read(&path).expect("the file was written");
        let parsed = corefoundation::plist::parse(&written).expect("a property list");
        assert_eq!(parsed.get("Name").and_then(|v| v.as_str()), Some("League"));

        let not_a_plist =
            objc::object::create_instance(objc::class::lookup(c"NSObject").expect("NSObject"), 0);
        let refused_path = directory.join("refused.plist");
        let refused = corefoundation::collections::new_dictionary(
            &[(key as CFTypeRef, not_a_plist as CFTypeRef)],
            policy,
            policy,
            false,
        ) as Id;
        let refused_target = crate::string::from_str(refused_path.to_str().expect("UTF-8"));
        // SAFETY: as above.
        assert_eq!(
            unsafe { write_property_list_to_file(refused, sel, refused_target, 0) },
            0
        );
        assert!(!refused_path.exists());
        let _ = std::fs::remove_dir_all(&directory);
    }
}
