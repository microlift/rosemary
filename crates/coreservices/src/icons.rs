use std::ffi::c_void;

const NO_SUCH_ICON_ERR: i16 = -2582;

pub unsafe extern "C" fn GetIconRef(
    _volume: i16,
    _creator: u32,
    _kind: u32,
    icon: *mut *mut c_void,
) -> i16 {
    if !icon.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *icon = std::ptr::null_mut() };
    }
    NO_SUCH_ICON_ERR
}

pub unsafe extern "C" fn ReleaseIconRef(_icon: *mut c_void) -> i16 {
    0
}

exports::exports! {
    functions {
        "GetIconRef" => GetIconRef,
        "ReleaseIconRef" => ReleaseIconRef,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_icon_is_handed_out() {
        let mut icon = std::ptr::dangling_mut::<c_void>();
        // SAFETY: a writable out-parameter.
        assert_eq!(
            unsafe { GetIconRef(-32768, 0, 0, &mut icon) },
            NO_SUCH_ICON_ERR
        );
        assert!(icon.is_null());
    }
}
