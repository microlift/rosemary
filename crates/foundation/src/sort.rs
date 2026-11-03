//! `NSSortDescriptor`, and sorting the collections by a list of them.

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
struct Descriptor {
    isa: *const objc::class::Class,
    /// A key path, copied, or nil for the objects themselves.
    key: Id,
    ascending: u8,
    /// The comparison selector; `compare:` unless one was given.
    selector: usize,
    /// A copied `NSComparator`, or nil.
    comparator: Id,
}

fn descriptor<'a>(receiver: Id) -> &'a mut Descriptor {
    // SAFETY: an instance of the class laid out as `Descriptor`.
    unsafe { &mut *(receiver as *mut Descriptor) }
}

fn send_id(receiver: Id, sel: &str, argument: Id) -> Id {
    // SAFETY: each selector here is `id (id, SEL, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel), argument)
    }
}

unsafe extern "C" fn init_with_key_selector(
    receiver: Id,
    _sel: Sel,
    key: Id,
    ascending: u8,
    selector: Sel,
) -> Id {
    if receiver.is_null() {
        return receiver;
    }
    let d = descriptor(receiver);
    d.key = send_id(key, "copy", std::ptr::null_mut());
    d.ascending = ascending;
    d.selector = match selector.is_null() {
        true => objc::sel::get("compare:") as usize,
        false => selector as usize,
    };
    receiver
}

unsafe extern "C" fn init_with_key(receiver: Id, sel: Sel, key: Id, ascending: u8) -> Id {
    // SAFETY: as the designated initializer.
    unsafe { init_with_key_selector(receiver, sel, key, ascending, std::ptr::null()) }
}

unsafe extern "C" fn init_with_key_comparator(
    receiver: Id,
    sel: Sel,
    key: Id,
    ascending: u8,
    comparator: Id,
) -> Id {
    // SAFETY: as the designated initializer.
    let receiver =
        unsafe { init_with_key_selector(receiver, sel, key, ascending, std::ptr::null()) };
    if !receiver.is_null() {
        descriptor(receiver).comparator = objc::block::copy(comparator);
    }
    receiver
}

fn new_descriptor(class: Id) -> Id {
    // SAFETY: the receiver is the class, or a subclass, laid out as `Descriptor`.
    let class =
        unsafe { objc::class::from_ptr(class as *const objc::class::Class) }.expect("a class");
    let object = objc::object::create_instance(class, 0);
    // SAFETY: a fresh object follows the convenience-constructor rule.
    unsafe { corefoundation::api::CFAutorelease(object as CFTypeRef) as Id }
}

unsafe extern "C" fn with_key(class: Id, sel: Sel, key: Id, ascending: u8) -> Id {
    // SAFETY: a fresh instance.
    unsafe { init_with_key(new_descriptor(class), sel, key, ascending) }
}

unsafe extern "C" fn with_key_selector(
    class: Id,
    sel: Sel,
    key: Id,
    ascending: u8,
    selector: Sel,
) -> Id {
    // SAFETY: a fresh instance.
    unsafe { init_with_key_selector(new_descriptor(class), sel, key, ascending, selector) }
}

unsafe extern "C" fn with_key_comparator(
    class: Id,
    sel: Sel,
    key: Id,
    ascending: u8,
    comparator: Id,
) -> Id {
    // SAFETY: a fresh instance.
    unsafe { init_with_key_comparator(new_descriptor(class), sel, key, ascending, comparator) }
}

unsafe extern "C" fn key(receiver: Id, _sel: Sel) -> Id {
    descriptor(receiver).key
}

unsafe extern "C" fn ascending(receiver: Id, _sel: Sel) -> u8 {
    descriptor(receiver).ascending
}

unsafe extern "C" fn selector(receiver: Id, _sel: Sel) -> Sel {
    descriptor(receiver).selector as Sel
}

unsafe extern "C" fn comparator(receiver: Id, _sel: Sel) -> Id {
    descriptor(receiver).comparator
}

unsafe extern "C" fn reversed(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a live descriptor.
    let class = unsafe { objc::object::class_of(receiver) }.expect("a descriptor");
    let d = descriptor(receiver);
    let copy = new_descriptor(objc::class::ptr(class) as Id);
    // SAFETY: a fresh instance.
    unsafe {
        init_with_key_comparator(
            copy,
            objc::sel::get("initWithKey:ascending:comparator:"),
            d.key,
            u8::from(d.ascending == 0),
            d.comparator,
        )
    };
    descriptor(copy).selector = d.selector;
    copy
}

/// `-compareObject:toObject:`: the two values at the key, by the comparator
/// or the selector, nil first, and reversed when descending.
unsafe extern "C" fn compare_objects(receiver: Id, _sel: Sel, a: Id, b: Id) -> isize {
    let d = descriptor(receiver);
    let value = |object: Id| match d.key.is_null() || object.is_null() {
        true => object,
        false => send_id(object, "valueForKeyPath:", d.key),
    };
    let (a, b) = (value(a), value(b));
    let result = if a.is_null() || b.is_null() {
        b.is_null() as isize - a.is_null() as isize
    } else if !d.comparator.is_null() {
        // SAFETY: a copied `NSComparator`, `NSComparisonResult (^)(id, id)`.
        unsafe {
            match objc::block::invoke(d.comparator) {
                Some(invoke) => {
                    let f: unsafe extern "C" fn(Id, Id, Id) -> isize = std::mem::transmute(invoke);
                    f(d.comparator, a, b)
                }
                None => 0,
            }
        }
    } else {
        // SAFETY: a comparison selector is `NSComparisonResult (id, SEL, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> isize =
                std::mem::transmute(objc::msg::entry());
            f(a, d.selector as Sel, b)
        }
    };
    match d.ascending != 0 {
        true => result,
        false => -result,
    }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let d = descriptor(receiver);
    objc::arc::release(d.key);
    objc::block::release(d.comparator);
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn copy_with_zone(receiver: Id, _sel: Sel, _zone: *const c_void) -> Id {
    objc::arc::retain(receiver)
}

/// `values` sorted by `descriptors` (an `NSArray` of sort descriptors), stably.
fn sorted(values: &[CFTypeRef], descriptors: Id) -> Vec<CFTypeRef> {
    // SAFETY: an array or nil.
    let descriptors: Vec<Id> =
        unsafe { corefoundation::collections::array(descriptors as CFTypeRef) }
            .map(|a| a.values.iter().map(|v| *v as Id).collect())
            .unwrap_or_default();
    let sel = objc::sel::get("compareObject:toObject:");
    let mut out = values.to_vec();
    out.sort_by(|a, b| {
        for descriptor in &descriptors {
            // SAFETY: `NSComparisonResult (id, SEL, id, id)`.
            let result = unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> isize =
                    std::mem::transmute(objc::msg::entry());
                f(*descriptor, sel, *a as Id, *b as Id)
            };
            if result != 0 {
                return result.cmp(&0);
            }
        }
        std::cmp::Ordering::Equal
    });
    out
}

unsafe extern "C" fn sorted_array(receiver: Id, _sel: Sel, descriptors: Id) -> Id {
    // SAFETY: an array.
    let values = unsafe { corefoundation::collections::array(receiver as CFTypeRef) }
        .map(|a| a.values.clone())
        .unwrap_or_default();
    let values = sorted(&values, descriptors);
    // SAFETY: a fresh array follows the convenience-constructor rule.
    unsafe {
        corefoundation::api::CFAutorelease(corefoundation::collections::new_array(
            &values,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        )) as Id
    }
}

unsafe extern "C" fn sort_array(receiver: Id, _sel: Sel, descriptors: Id) {
    // SAFETY: a mutable array.
    if let Some(array) = unsafe { corefoundation::collections::array(receiver as CFTypeRef) } {
        let values = array.values.clone();
        array.values = sorted(&values, descriptors);
    }
}

/// `-[NSSet sortedArrayUsingDescriptors:]`.
unsafe extern "C" fn sorted_set(receiver: Id, sel: Sel, descriptors: Id) -> Id {
    let all = send_id(receiver, "allObjects", std::ptr::null_mut());
    // SAFETY: an array.
    unsafe { sorted_array(all, sel, descriptors) }
}

pub(crate) fn install() {
    let class =
        corefoundation::base::register(c"NSSortDescriptor", None, size_of::<Descriptor>(), false);
    class_methods!(class, {
        "sortDescriptorWithKey:ascending:" => with_key, c"@28@0:8@16c24";
        "sortDescriptorWithKey:ascending:selector:" => with_key_selector, c"@36@0:8@16c24:28";
        "sortDescriptorWithKey:ascending:comparator:" => with_key_comparator, c"@36@0:8@16c24@?28";
    });
    methods!(class, {
        "initWithKey:ascending:" => init_with_key, c"@28@0:8@16c24";
        "initWithKey:ascending:selector:" => init_with_key_selector, c"@36@0:8@16c24:28";
        "initWithKey:ascending:comparator:" => init_with_key_comparator, c"@36@0:8@16c24@?28";
        "key" => key, c"@16@0:8";
        "ascending" => ascending, c"c16@0:8";
        "selector" => selector, c":16@0:8";
        "comparator" => comparator, c"@?16@0:8";
        "reversedSortDescriptor" => reversed, c"@16@0:8";
        "compareObject:toObject:" => compare_objects, c"q32@0:8@16@24";
        "copyWithZone:" => copy_with_zone, c"@24@0:8^v16";
        "dealloc" => dealloc, c"v16@0:8";
    });

    let array = objc::class::lookup(c"NSArray").expect("NSArray");
    methods!(array, {
        "sortedArrayUsingDescriptors:" => sorted_array, c"@24@0:8@16";
    });
    let mutable_array = objc::class::lookup(c"NSMutableArray").expect("NSMutableArray");
    methods!(mutable_array, {
        "sortUsingDescriptors:" => sort_array, c"v24@0:8@16";
    });
    let set = objc::class::lookup(c"NSSet").expect("NSSet");
    methods!(set, {
        "sortedArrayUsingDescriptors:" => sorted_set, c"@24@0:8@16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_sort_by_key_then_by_the_next_descriptor() {
        crate::bootstrap();
        let entry = |name: &str, size: i64| {
            let keys = [
                crate::string::from_str("name") as CFTypeRef,
                crate::string::from_str("size") as CFTypeRef,
            ];
            let entries = [
                (keys[0], crate::string::from_str(name) as CFTypeRef),
                (keys[1], crate::number(size) as CFTypeRef),
            ];
            corefoundation::collections::new_dictionary(
                &entries,
                corefoundation::callbacks::Policy::from_key_callbacks(),
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            )
        };
        let values = [entry("b", 1), entry("a", 2), entry("a", 1)];
        let class = objc::class::ptr(objc::class::lookup(c"NSSortDescriptor").expect("installed"));
        // SAFETY: the constructors installed above.
        let (by_name, by_size_descending) = unsafe {
            (
                with_key(
                    class as Id,
                    std::ptr::null(),
                    crate::string::from_str("name"),
                    1,
                ),
                with_key(
                    class as Id,
                    std::ptr::null(),
                    crate::string::from_str("size"),
                    0,
                ),
            )
        };
        let descriptors = corefoundation::collections::new_array(
            &[by_name as CFTypeRef, by_size_descending as CFTypeRef],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        );
        let out = sorted(&values, descriptors as Id);
        assert_eq!(out, [values[1], values[2], values[0]]);
    }
}
