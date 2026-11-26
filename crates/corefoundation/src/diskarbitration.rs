use crate::base::CFTypeRef;
use crate::callbacks::Policy;

const DEVICE_MODEL: &str = "DADeviceModel";

fn device_model_key() -> CFTypeRef {
    static KEY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *KEY.get_or_init(|| {
        let key = crate::string::from_bytes(DEVICE_MODEL.as_bytes(), false);
        // A process-lifetime data-symbol value must survive guest releases.
        objc::arc::retain(key as objc::object::Id);
        key as usize
    }) as CFTypeRef
}

pub unsafe extern "C" fn session_create(_allocator: CFTypeRef) -> CFTypeRef {
    crate::collections::new_dictionary(
        &[],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        false,
    )
}

pub unsafe extern "C" fn disk_create_from_volume_path(
    _allocator: CFTypeRef,
    session: CFTypeRef,
    path: CFTypeRef,
) -> CFTypeRef {
    if session.is_null() || path.is_null() {
        return std::ptr::null();
    }
    // The URL is an adequate handle: no caller observes a DADisk type ID, and
    // retaining it gives the returned handle normal CF ownership semantics.
    unsafe { crate::api::CFRetain(path) }
}

/// `DADiskRef DADiskCreateFromBSDName(CFAllocatorRef, DASessionRef, const char *name)`
///
/// The name itself is the handle, for the reason the volume path is above.
///
/// # Safety
///
/// `name` is null or a NUL-terminated string.
pub unsafe extern "C" fn disk_create_from_bsd_name(
    _allocator: CFTypeRef,
    session: CFTypeRef,
    name: *const std::ffi::c_char,
) -> CFTypeRef {
    if session.is_null() || name.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller's NUL-terminated string.
    let name = unsafe { std::ffi::CStr::from_ptr(name) };
    crate::string::from_bytes(name.to_bytes(), false)
}

pub unsafe extern "C" fn disk_copy_description(disk: CFTypeRef) -> CFTypeRef {
    if disk.is_null() {
        return std::ptr::null();
    }
    let value = crate::string::from_bytes(b"Generic", false);
    let dictionary = crate::collections::new_dictionary(
        &[(device_model_key(), value)],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        false,
    );
    // The dictionary retained the value.
    unsafe { crate::api::CFRelease(value) };
    dictionary
}

exports::exports! {
    functions {
        "DASessionCreate" => session_create,
        "DADiskCreateFromVolumePath" => disk_create_from_volume_path,
        "DADiskCreateFromBSDName" => disk_create_from_bsd_name,
        "DADiskCopyDescription" => disk_copy_description,
    }
}

pub fn data_symbols() -> Vec<(String, u64)> {
    vec![(
        "_kDADiskDescriptionDeviceModelKey".to_string(),
        crate::base::global_cell(
            "_kDADiskDescriptionDeviceModelKey",
            device_model_key() as usize as u64,
        ),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_volume_description_has_a_device_model() {
        let path = crate::url::new_file_url("/", true);
        // SAFETY: all three objects are live CoreFoundation values.
        unsafe {
            let session = session_create(std::ptr::null());
            let disk = disk_create_from_volume_path(std::ptr::null(), session, path);
            let description = disk_copy_description(disk);
            assert!(!crate::api::CFDictionaryGetValue(description, device_model_key()).is_null());
            crate::api::CFRelease(description);
            crate::api::CFRelease(disk);
            crate::api::CFRelease(session);
            crate::api::CFRelease(path);
        }
    }
}
