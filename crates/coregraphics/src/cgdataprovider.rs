//! `CGDataProvider`: bytes an image is made over, and their release callback.

#![allow(non_snake_case)]

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;

/// `CGDataProviderReleaseDataCallback`.
pub type ReleaseData = Option<unsafe extern "C" fn(*mut c_void, *const c_void, usize)>;

#[repr(C)]
pub(crate) struct DataProvider {
    isa: *const c_void,
    pub(crate) info: *mut c_void,
    pub(crate) data: *const u8,
    pub(crate) size: usize,
    release: ReleaseData,
}

pub fn install() {
    let provider =
        corefoundation::base::register(c"__CGDataProvider", None, size_of::<DataProvider>(), true);
    provider.replace_method(
        objc::sel::get("dealloc"),
        provider_dealloc as *const c_void,
        c"v16@0:8",
    );
}

/// Hands the bytes back to their owner, which is how Qt learns that the
/// `QImage` a `CGImage` was made over may be detached again.
unsafe extern "C" fn provider_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__CGDataProvider`, whose body is `DataProvider`.
    unsafe {
        let provider = &mut *(receiver as *mut DataProvider);
        if let Some(release) = provider.release.take() {
            release(provider.info, provider.data.cast(), provider.size);
        }
        objc::arc::finalize(receiver);
    }
}

/// The bytes a data provider of this runtime's serves, or `None` for anything
/// else.
pub fn provider_bytes(provider: CFTypeRef) -> Option<&'static [u8]> {
    let class = objc::class::lookup(c"__CGDataProvider")?;
    // SAFETY: null or an object this runtime allocated; type-checked.
    if !unsafe { corefoundation::base::is_kind_of(provider, class) } {
        return None;
    }
    // SAFETY: a `__CGDataProvider`, whose body is `DataProvider` and whose
    // bytes live as long as it does - which the caller holds a reference for.
    unsafe {
        let provider = &*(provider as *const DataProvider);
        if provider.data.is_null() {
            return None;
        }
        Some(std::slice::from_raw_parts(provider.data, provider.size))
    }
}

pub unsafe extern "C" fn CGDataProviderCreateWithData(
    info: *mut c_void,
    data: *const c_void,
    size: usize,
    release: ReleaseData,
) -> CFTypeRef {
    let class = objc::class::lookup(c"__CGDataProvider").expect("installed during bootstrap");
    // SAFETY: `DataProvider` is the registered class layout.
    let provider = unsafe { corefoundation::base::allocate::<DataProvider>(class) };
    // SAFETY: a fresh allocation.
    unsafe {
        (*provider).info = info;
        (*provider).data = data.cast();
        (*provider).size = size;
        (*provider).release = release;
    }
    provider as CFTypeRef
}

pub unsafe extern "C" fn CGDataProviderRelease(provider: CFTypeRef) {
    if !provider.is_null() {
        unsafe { corefoundation::api::CFRelease(provider) };
    }
}

exports::exports! {
    functions {
        "CGDataProviderCreateWithData" => CGDataProviderCreateWithData,
        "CGDataProviderRelease" => CGDataProviderRelease,
    }
}
