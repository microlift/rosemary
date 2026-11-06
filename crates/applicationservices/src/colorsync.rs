#![allow(non_snake_case)]

use corefoundation::base::CFTypeRef;
use objc::class::Class;

/// A `ColorSyncProfileRef`: the file it was made from, retained, or null for
/// one made from a display id, which has no backing file here.
#[repr(C)]
struct Profile {
    isa: *const std::ffi::c_void,
    url: CFTypeRef,
}

fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        let class =
            corefoundation::base::register(c"__ColorSyncProfile", None, size_of::<Profile>(), true);
        class.replace_method(
            objc::sel::get("dealloc"),
            profile_dealloc as *const std::ffi::c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: a leaked `Class` this module registered.
    unsafe { &*(ptr as *const Class) }
}

unsafe extern "C" fn profile_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__ColorSyncProfile`, whose body is `Profile`.
    unsafe {
        let profile = &mut *(receiver as *mut Profile);
        if !profile.url.is_null() {
            corefoundation::api::CFRelease(profile.url);
            profile.url = std::ptr::null();
        }
        objc::arc::finalize(receiver);
    }
}

fn new_profile(url: CFTypeRef) -> CFTypeRef {
    // SAFETY: `Profile` is the registered class layout.
    let profile = unsafe { corefoundation::base::allocate::<Profile>(class()) };
    if profile.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a fresh allocation.
    unsafe { (*profile).url = url };
    profile as CFTypeRef
}

fn profile_of<'a>(profile: CFTypeRef) -> Option<&'a Profile> {
    // SAFETY: null or an object a guest received from this runtime.
    let is_profile =
        !profile.is_null() && unsafe { corefoundation::base::is_kind_of(profile, class()) };
    // SAFETY: checked above to be a `__ColorSyncProfile`, whose body is `Profile`.
    is_profile.then(|| unsafe { &*(profile as *const Profile) })
}

/// `ColorSyncProfileRef ColorSyncProfileCreateWithDisplayID(CGDirectDisplayID)`.
///
/// FIXME (ACCURACY): every display answers the same generic RGB profile,
/// since this runtime does no real colour management. It has no ICC file
/// behind it, so `ColorSyncProfileGetURL` of one made this way is null.
pub unsafe extern "C" fn ColorSyncProfileCreateWithDisplayID(_display_id: u32) -> CFTypeRef {
    new_profile(std::ptr::null())
}

/// `ColorSyncProfileRef ColorSyncProfileCreateWithURL(CFURLRef, CFErrorRef *)`.
pub unsafe extern "C" fn ColorSyncProfileCreateWithURL(
    url: CFTypeRef,
    error: *mut CFTypeRef,
) -> CFTypeRef {
    if !error.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { error.write(std::ptr::null()) };
    }
    if url.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller's URL, retained for as long as the profile lives.
    new_profile(unsafe { corefoundation::api::CFRetain(url) })
}

/// `CFURLRef ColorSyncProfileGetURL(ColorSyncProfileRef, CFErrorRef *)`: not
/// retained, as Apple documents.
pub unsafe extern "C" fn ColorSyncProfileGetURL(
    profile: CFTypeRef,
    error: *mut CFTypeRef,
) -> CFTypeRef {
    if !error.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { error.write(std::ptr::null()) };
    }
    profile_of(profile).map_or(std::ptr::null(), |profile| profile.url)
}

exports::exports! {
    functions {
        "ColorSyncProfileCreateWithDisplayID" => ColorSyncProfileCreateWithDisplayID,
        "ColorSyncProfileCreateWithURL" => ColorSyncProfileCreateWithURL,
        "ColorSyncProfileGetURL" => ColorSyncProfileGetURL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_made_from_a_display_has_no_backing_url() {
        // SAFETY: the profile stays live until released below.
        unsafe {
            let profile = ColorSyncProfileCreateWithDisplayID(1);
            assert!(!profile.is_null());
            assert!(ColorSyncProfileGetURL(profile, std::ptr::null_mut()).is_null());
            corefoundation::api::CFRelease(profile);
        }
    }

    #[test]
    fn a_profile_made_from_a_url_answers_with_the_same_one() {
        // SAFETY: every object stays live until its matching release.
        unsafe {
            let url = foundation::string::from_str("file:///tmp/sRGB.icc");
            let profile = ColorSyncProfileCreateWithURL(url, std::ptr::null_mut());
            assert!(!profile.is_null());
            assert_eq!(
                ColorSyncProfileGetURL(profile, std::ptr::null_mut()),
                url as CFTypeRef
            );
            corefoundation::api::CFRelease(profile);
            corefoundation::api::CFRelease(url);
        }
    }

    #[test]
    fn a_null_url_is_refused_rather_than_making_a_profile() {
        // SAFETY: a plain value and a null pointer.
        unsafe {
            assert!(
                ColorSyncProfileCreateWithURL(std::ptr::null(), std::ptr::null_mut()).is_null()
            );
        }
    }
}
