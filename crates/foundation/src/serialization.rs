//! `NSPropertyListSerialization`, over CoreFoundation's reader and writers.

use corefoundation::base::{CFIndex, CFTypeRef};
use corefoundation::propertylist as cf;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::class_methods;

/// Hand `object` (owned) back autoreleased, and `error` (owned, or null) to
/// the caller's out-parameter or away.
fn answer(object: CFTypeRef, error: CFTypeRef, out: *mut Id) -> Id {
    if !out.is_null() {
        // SAFETY: the caller's out-parameter, writable for one object.
        unsafe {
            *out = match error.is_null() {
                true => std::ptr::null_mut(),
                false => corefoundation::api::CFAutorelease(error) as Id,
            }
        };
    } else if !error.is_null() {
        // SAFETY: an error this call owns and nobody asked for.
        unsafe { corefoundation::api::CFRelease(error) };
    }
    match object.is_null() {
        true => std::ptr::null_mut(),
        // SAFETY: an object this call owns.
        false => unsafe { corefoundation::api::CFAutorelease(object) as Id },
    }
}

/// `+dataWithPropertyList:format:options:error:`.
unsafe extern "C" fn data_with(
    _class: Id,
    _sel: Sel,
    list: Id,
    format: usize,
    options: usize,
    error: *mut Id,
) -> Id {
    let mut failure: CFTypeRef = std::ptr::null();
    // SAFETY: the guest's object, and a local out-parameter.
    let data = unsafe {
        cf::CFPropertyListCreateData(
            std::ptr::null(),
            list as CFTypeRef,
            format as CFIndex,
            options as u64,
            &mut failure,
        )
    };
    answer(data, failure, error)
}

/// `+propertyListWithData:options:format:error:`.
unsafe extern "C" fn list_with(
    _class: Id,
    _sel: Sel,
    data: Id,
    options: usize,
    format: *mut usize,
    error: *mut Id,
) -> Id {
    let mut failure: CFTypeRef = std::ptr::null();
    // SAFETY: the guest's data and out-parameter, and a local one.
    let list = unsafe {
        cf::CFPropertyListCreateWithData(
            std::ptr::null(),
            data as CFTypeRef,
            options as u64,
            format as *mut CFIndex,
            &mut failure,
        )
    };
    answer(list, failure, error)
}

/// The deprecated `+dataFromPropertyList:format:errorDescription:`.
unsafe extern "C" fn data_from(
    class: Id,
    sel: Sel,
    list: Id,
    format: usize,
    description: *mut Id,
) -> Id {
    // SAFETY: as `+dataWithPropertyList:format:options:error:`.
    let data = unsafe { data_with(class, sel, list, format, 0, std::ptr::null_mut()) };
    if !description.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe {
            *description = match data.is_null() {
                true => objc::arc::autorelease(crate::string::from_str(
                    "the object is not a property list",
                )),
                false => std::ptr::null_mut(),
            }
        };
    }
    data
}

/// The deprecated `+propertyListFromData:mutabilityOption:format:errorDescription:`.
unsafe extern "C" fn list_from(
    class: Id,
    sel: Sel,
    data: Id,
    options: usize,
    format: *mut usize,
    description: *mut Id,
) -> Id {
    // SAFETY: as `+propertyListWithData:options:format:error:`.
    let list = unsafe { list_with(class, sel, data, options, format, std::ptr::null_mut()) };
    if !description.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe {
            *description = match list.is_null() {
                true => objc::arc::autorelease(crate::string::from_str(
                    "the data is not a property list",
                )),
                false => std::ptr::null_mut(),
            }
        };
    }
    list
}

/// `+propertyList:isValidForFormat:`: whether it is made of property list
/// types; both formats this runtime writes hold all of them.
unsafe extern "C" fn is_valid(_class: Id, _sel: Sel, list: Id, _format: usize) -> u8 {
    // SAFETY: the guest's object.
    unsafe { cf::value_of(list as CFTypeRef) }.is_some() as u8
}

pub(crate) fn install() {
    let class = corefoundation::base::register(c"NSPropertyListSerialization", None, 0, false);
    class_methods!(class, {
        "dataWithPropertyList:format:options:error:" => data_with, c"@48@0:8@16Q24Q32^@40";
        "propertyListWithData:options:format:error:" => list_with, c"@48@0:8@16Q24^Q32^@40";
        "dataFromPropertyList:format:errorDescription:" => data_from, c"@40@0:8@16Q24^@32";
        "propertyListFromData:mutabilityOption:format:errorDescription:" => list_from,
        c"@48@0:8@16Q24^Q32^@40";
        "propertyList:isValidForFormat:" => is_valid, c"c32@0:8@16Q24";
    });
}
