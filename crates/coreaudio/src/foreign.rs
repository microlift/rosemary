use std::ffi::{CStr, c_void};

/// Whether a foreign handle is handed back to the framework that made it.
///
/// `ROSEMARY_NO_FOREIGN` turns it off, which is how a run proves it needs no
/// host framework at all: a unit that is not this crate's is then refused with
/// the framework's own `kAudioUnitErr_InvalidElement` rather than serviced by
/// Apple's. `corefoundation::foreign` reads the same variable, for the same
/// reason and with the same default.
#[cfg(target_os = "macos")]
fn forwarding() -> bool {
    use static_init::dynamic;
    #[dynamic]
    static ON: bool = std::env::var_os("ROSEMARY_NO_FOREIGN").is_none();
    *ON
}

/// The framework's own entry point, by `dlsym` rather than a link.
///
/// This crate's own names are published to a guest through a synthetic library
/// rather than exported from the binary, so the lookup cannot find its way back
/// here.
#[cfg(target_os = "macos")]
fn entry(name: &CStr) -> Option<*mut c_void> {
    if !forwarding() {
        return None;
    }
    // SAFETY: a NUL-terminated name, and the default search order.
    let found = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    (!found.is_null()).then_some(found)
}

/// Off Darwin there is no other AudioToolbox, so every forward below stops here.
#[cfg(not(target_os = "macos"))]
fn entry(_name: &CStr) -> Option<*mut c_void> {
    None
}

/// Forward a one-argument `OSStatus` call, or `None` where there is nothing to
/// forward to.
pub fn status(name: &CStr, handle: *mut c_void) -> Option<i32> {
    let f = entry(name)?;
    // SAFETY: every name passed here is `OSStatus (*)(AudioUnit)`, and the
    // handle is one that framework made.
    Some(unsafe {
        let f: unsafe extern "C" fn(*mut c_void) -> i32 = std::mem::transmute(f);
        f(handle)
    })
}

/// Forward `AudioUnitSetProperty`.
///
/// # Safety
///
/// As the entry point's: `data` is `size` readable bytes for the property.
pub unsafe fn set_property(
    handle: *mut c_void,
    id: u32,
    scope: u32,
    element: u32,
    data: *const c_void,
    size: u32,
) -> Option<i32> {
    let f = entry(c"AudioUnitSetProperty")?;
    // SAFETY: the declaration is the framework's, and the handle is one it made.
    Some(unsafe {
        let f: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void, u32) -> i32 =
            std::mem::transmute(f);
        f(handle, id, scope, element, data, size)
    })
}

/// Forward `AudioUnitGetProperty`.
///
/// # Safety
///
/// As the entry point's: `out` is writable for `*size` bytes.
pub unsafe fn get_property(
    handle: *mut c_void,
    id: u32,
    scope: u32,
    element: u32,
    out: *mut c_void,
    size: *mut u32,
) -> Option<i32> {
    let f = entry(c"AudioUnitGetProperty")?;
    // SAFETY: the declaration is the framework's, and the handle is one it made.
    Some(unsafe {
        let f: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut c_void, *mut u32) -> i32 =
            std::mem::transmute(f);
        f(handle, id, scope, element, out, size)
    })
}

/// Forward `AudioUnitGetPropertyInfo`.
///
/// # Safety
///
/// As the entry point's: each out-parameter is null or writable.
pub unsafe fn get_property_info(
    handle: *mut c_void,
    id: u32,
    scope: u32,
    element: u32,
    size: *mut u32,
    writable: *mut u8,
) -> Option<i32> {
    let f = entry(c"AudioUnitGetPropertyInfo")?;
    // SAFETY: the declaration is the framework's, and the handle is one it made.
    Some(unsafe {
        let f: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut u32, *mut u8) -> i32 =
            std::mem::transmute(f);
        f(handle, id, scope, element, size, writable)
    })
}
