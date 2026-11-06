//! `NSPropertyListSerialization`: a thin Objective-C face on the
//! `CFPropertyList...` entry points `corefoundation::propertylist` already
//! implements, exactly as `NSJSONSerialization` (see `crate::json`) is a face
//! on this crate's own JSON reader and writer. Nothing here parses or writes a
//! plist a second time.
//!
//! Stream-based `+propertyListWithStream:...` and `+writePropertyList:
//! toStream:...` are not implemented, for the reason `NSJSONSerialization`'s
//! stream-based methods are not: this runtime has no `NSInputStream`/
//! `NSOutputStream`.

use corefoundation::base::CFTypeRef;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::class_methods;

/// `NSPropertyListReadCorruptError`.
const READ_CORRUPT: i64 = 3840;
/// `NSPropertyListWriteInvalidError`.
const WRITE_INVALID: i64 = 3851;

fn fail(error: *mut Id, code: i64, message: &str) {
    if error.is_null() {
        return;
    }
    let object = crate::error::make("NSCocoaErrorDomain", code, message);
    // SAFETY: the guest handed us the address of its own `NSError *`.
    unsafe { *error = object };
}

/// `+propertyList:isValidForFormat:`: whether `plist` is a plist value - a
/// boolean, number, string, data, date, array or dictionary with string keys,
/// nested only in each other. `format` does not change the answer: every
/// format this runtime reads and writes accepts the same shapes.
unsafe extern "C" fn is_valid(_class: Id, _sel: Sel, plist: Id, _format: i64) -> bool {
    // SAFETY: a live object, or nil.
    unsafe { corefoundation::propertylist::value_of(plist as CFTypeRef) }.is_some()
}

/// `+dataWithPropertyList:format:options:error:`.
///
/// FIXME (ACCURACY): `format` is read only to reject values this runtime
/// cannot represent as any plist at all; a binary request
/// (`NSPropertyListBinaryFormat_v1_0`) is still written as XML, since this
/// tree has no binary plist *writer* - `corefoundation::plist` only reads one.
/// The bytes still read back correctly through `+propertyListWithData:...`,
/// which detects the format from the data rather than trusting a caller's
/// claim about it.
unsafe extern "C" fn data_with_property_list(
    _class: Id,
    _sel: Sel,
    plist: Id,
    _format: i64,
    _options: usize,
    error: *mut Id,
) -> Id {
    let plist = plist as CFTypeRef;
    // SAFETY: a live object, or nil.
    if unsafe { corefoundation::propertylist::value_of(plist) }.is_none() {
        fail(
            error,
            WRITE_INVALID,
            "this object cannot be written as a property list",
        );
        return std::ptr::null_mut();
    }
    // SAFETY: as above; a value a property list can hold.
    let data = unsafe {
        corefoundation::propertylist::CFPropertyListCreateXMLData(std::ptr::null(), plist)
    };
    // SAFETY: a fresh object this function owns a reference to.
    unsafe { corefoundation::api::CFAutorelease(data) as Id }
}

/// `+propertyListWithData:options:format:error:`.
unsafe extern "C" fn property_list_with_data(
    _class: Id,
    _sel: Sel,
    data: Id,
    options: u64,
    format: *mut i64,
    error: *mut Id,
) -> Id {
    if !error.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { *error = std::ptr::null_mut() };
    }
    let mut cf_format: corefoundation::base::CFIndex = 0;
    let mut cf_error: CFTypeRef = std::ptr::null();
    // SAFETY: `data` is the caller's `NSData`, type-checked inside; the two
    // locals are this function's own, and live for the call.
    let object = unsafe {
        corefoundation::propertylist::CFPropertyListCreateWithData(
            std::ptr::null(),
            data as CFTypeRef,
            options,
            &mut cf_format,
            &mut cf_error,
        )
    };
    if object.is_null() {
        fail(
            error,
            READ_CORRUPT,
            "the data is not a property list in a format this runtime reads",
        );
        return std::ptr::null_mut();
    }
    if !format.is_null() {
        // SAFETY: writable, per the caller. `NSPropertyListFormat` and
        // `CFPropertyListFormat` share the same two values, 100 and 200.
        unsafe { *format = cf_format as i64 };
    }
    // SAFETY: a fresh object this function owns a reference to.
    unsafe { corefoundation::api::CFAutorelease(object) as Id }
}

pub(crate) fn install() {
    let class = corefoundation::base::abstract_class(c"NSPropertyListSerialization", None);
    class_methods!(class, {
        "propertyList:isValidForFormat:" => is_valid, c"B32@0:8@16q24";
        "dataWithPropertyList:format:options:error:" => data_with_property_list,
            c"@48@0:8@16q24Q32^@40";
        "propertyListWithData:options:format:error:" => property_list_with_data,
            c"@48@0:8@16Q24^q32^@40";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send(sel: &str, data: Id, options: u64, format: *mut i64, error: *mut Id) -> Id {
        let class = objc::class::lookup(c"NSPropertyListSerialization").expect("the class");
        // SAFETY: `property_list_with_data`'s own prototype.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, u64, *mut i64, *mut Id) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                objc::class::ptr(class) as Id,
                objc::sel::get(sel),
                data,
                options,
                format,
                error,
            )
        }
    }

    #[test]
    fn a_plist_object_round_trips_through_data_and_back() {
        crate::bootstrap();
        let string = crate::string::from_str("hello");
        let mut write_error: Id = std::ptr::null_mut();
        // SAFETY: `string` is a live `NSString`.
        let data = unsafe {
            data_with_property_list(
                std::ptr::null_mut(),
                objc::sel::get("x"),
                string,
                100,
                0,
                &mut write_error,
            )
        };
        assert!(!data.is_null() && write_error.is_null());

        let mut format: i64 = 0;
        let mut read_error: Id = std::ptr::null_mut();
        let object = send(
            "propertyListWithData:options:format:error:",
            data,
            0,
            &mut format,
            &mut read_error,
        );
        assert!(!object.is_null() && read_error.is_null());
        assert_eq!(format, 100, "this runtime only ever writes XML");
        // SAFETY: both strings, by the round trip above.
        assert_eq!(unsafe { crate::string::bytes(object) }, b"hello");
    }

    #[test]
    fn an_object_with_no_plist_shape_is_invalid_and_refused() {
        crate::bootstrap();
        // An `NSValue` boxing a point is not one of the plist types.
        let point = crate::geometry::box_point(crate::geometry::NSPoint { x: 1.0, y: 2.0 });
        // SAFETY: a live object.
        let valid = unsafe { is_valid(std::ptr::null_mut(), objc::sel::get("x"), point, 100) };
        assert!(!valid);
        let mut error: Id = std::ptr::null_mut();
        // SAFETY: as above.
        let data = unsafe {
            data_with_property_list(
                std::ptr::null_mut(),
                objc::sel::get("x"),
                point,
                100,
                0,
                &mut error,
            )
        };
        assert!(data.is_null());
        assert!(!error.is_null());
    }
}
