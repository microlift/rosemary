//! `AVURLAsset`, `AVMutableComposition` and `AVAssetImageGenerator`.
//!
//! Nothing in this crate (or anywhere else in the tree) demuxes a media
//! container, so every asset this module makes answers with zero tracks and
//! `kCMTimeZero`, whatever file or composition it was made from - the honest
//! reading of "no decoder is available", not a guess at what a real file
//! would report.

use std::ffi::c_void;

use objc::object::Id;
use objc::sel::Sel;

use foundation::error::make as error;

use crate::define::{class_methods, methods};

/// `CMTime`, laid out as `CMTime.h` declares it. Duplicated from
/// `rosemary::shims::coremedia`'s own copy: the two crates don't share one,
/// and this is a value type simple enough that carrying it twice is cheaper
/// than inventing a crate to hold it.
#[repr(C)]
#[derive(Clone, Copy)]
struct CmTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

/// `kCMTimeFlags_Valid`.
const CM_TIME_FLAGS_VALID: u32 = 1;

/// `kCMTimeZero`: zero numeric seconds. Any nonzero `timescale` gives the same
/// answer through `CMTimeGetSeconds`/`CMTimeCompare`, which is the only way
/// the documented API observes this value - the stored `timescale` is not
/// itself part of what "time zero" promises.
const CM_TIME_ZERO: CmTime = CmTime {
    value: 0,
    timescale: 1,
    flags: CM_TIME_FLAGS_VALID,
    epoch: 0,
};

/// `AVFoundationErrorDomain`.
const DOMAIN: &str = "AVFoundationErrorDomain";

/// `AVErrorNoImageAtTime`, from AVError.h.
const ERROR_NO_IMAGE_AT_TIME: i64 = -11832;

pub fn install() {
    url_asset();
    mutable_composition();
    image_generator();
}

fn empty_array() -> Id {
    let array = corefoundation::collections::new_array(
        &[],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    objc::arc::autorelease(array as Id)
}

// -- AVURLAsset -----------------------------------------------------------

/// An asset's state: just the URL it was made from, kept as text so
/// `-URL` can hand back a fresh `NSURL` the way `NSURLRequest`'s `-URL` does
/// in `foundation`.
#[derive(Default)]
struct UrlAsset {
    url: String,
}

fn url_asset_class() -> &'static objc::class::Class {
    foundation::boxed::register::<UrlAsset>(c"AVURLAsset", None)
}

unsafe fn url_asset_of<'a>(receiver: Id) -> Option<&'a mut UrlAsset> {
    unsafe { foundation::boxed::contents(receiver, url_asset_class()) }
}

fn made_with_url(url: Id) -> Id {
    // SAFETY: the caller's `NSURL`, or nil.
    let text = unsafe { foundation::url::text_of(url) }.unwrap_or_default();
    foundation::boxed::make(url_asset_class(), UrlAsset { url: text })
}

/// `+[AVURLAsset URLAssetWithURL:options:]`.
unsafe extern "C" fn url_asset_with_url(_class: Id, _sel: Sel, url: Id, _options: Id) -> Id {
    objc::arc::autorelease(made_with_url(url))
}

/// `-[AVURLAsset initWithURL:options:]`, the designated initializer.
unsafe extern "C" fn init_with_url(receiver: Id, _sel: Sel, url: Id, _options: Id) -> Id {
    objc::arc::release(receiver);
    made_with_url(url)
}

unsafe extern "C" fn url(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a live asset.
    let Some(asset) = (unsafe { url_asset_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: a URL this call just made.
    unsafe { corefoundation::api::CFAutorelease(corefoundation::url::new(&asset.url)) as Id }
}

unsafe extern "C" fn tracks(_receiver: Id, _sel: Sel) -> Id {
    empty_array()
}

unsafe extern "C" fn tracks_with_media_type(_receiver: Id, _sel: Sel, _media_type: Id) -> Id {
    empty_array()
}

unsafe extern "C" fn duration(_receiver: Id, _sel: Sel) -> CmTime {
    CM_TIME_ZERO
}

/// `-isPlayable`: no, since an asset with no tracks has nothing to play.
unsafe extern "C" fn is_playable(_receiver: Id, _sel: Sel) -> u8 {
    0
}

fn url_asset() {
    let class = url_asset_class();
    class_methods!(class, {
        "URLAssetWithURL:options:" => url_asset_with_url, c"@32@0:8@16@24";
    });
    methods!(class, {
        "initWithURL:options:" => init_with_url, c"@32@0:8@16@24";
        "URL" => url, c"@16@0:8";
        "tracks" => tracks, c"@16@0:8";
        "tracksWithMediaType:" => tracks_with_media_type, c"@24@0:8@16";
        "duration" => duration, c"{CMTime=qiIq}16@0:8";
        "isPlayable" => is_playable, c"c16@0:8";
    });
}

// -- AVMutableComposition ---------------------------------------------------

/// `+[AVMutableComposition composition]`.
unsafe extern "C" fn composition(class: Id, _sel: Sel) -> Id {
    // SAFETY: this method's own receiver, a registered class.
    let class =
        unsafe { objc::class::from_ptr(class as *const objc::class::Class) }.expect("a class");
    objc::arc::autorelease(objc::object::create_instance(class, 0))
}

unsafe extern "C" fn composition_tracks(_receiver: Id, _sel: Sel) -> Id {
    empty_array()
}

unsafe extern "C" fn composition_duration(_receiver: Id, _sel: Sel) -> CmTime {
    CM_TIME_ZERO
}

fn mutable_composition() {
    let class = corefoundation::base::abstract_class(c"AVMutableComposition", None);
    class_methods!(class, {
        "composition" => composition, c"@16@0:8";
    });
    methods!(class, {
        "tracks" => composition_tracks, c"@16@0:8";
        "duration" => composition_duration, c"{CMTime=qiIq}16@0:8";
    });
}

// -- AVAssetImageGenerator --------------------------------------------------

/// An image generator's state: the asset it reads from, retained the way the
/// real class keeps a reference to it.
struct ImageGenerator {
    asset: Id,
}

impl Drop for ImageGenerator {
    fn drop(&mut self) {
        objc::arc::release(self.asset);
    }
}

fn image_generator_class() -> &'static objc::class::Class {
    foundation::boxed::register::<ImageGenerator>(c"AVAssetImageGenerator", None)
}

fn made_with_asset(asset: Id) -> Id {
    foundation::boxed::make(
        image_generator_class(),
        ImageGenerator {
            asset: objc::arc::retain(asset),
        },
    )
}

/// `+[AVAssetImageGenerator assetImageGeneratorWithAsset:]`.
unsafe extern "C" fn asset_image_generator_with_asset(_class: Id, _sel: Sel, asset: Id) -> Id {
    objc::arc::autorelease(made_with_asset(asset))
}

/// `-[AVAssetImageGenerator initWithAsset:]`.
unsafe extern "C" fn init_with_asset(receiver: Id, _sel: Sel, asset: Id) -> Id {
    objc::arc::release(receiver);
    made_with_asset(asset)
}

/// `-copyCGImageAtTime:actualTime:error:`: no decoder exists to produce a
/// frame from any asset, so this always takes the documented failure path -
/// a null image and `AVErrorNoImageAtTime`, the same thing a real session
/// reports when the requested time has no readable frame.
unsafe extern "C" fn copy_cg_image_at_time(
    _receiver: Id,
    _sel: Sel,
    _requested_time: CmTime,
    actual_time: *mut CmTime,
    error_out: *mut Id,
) -> *const c_void {
    if !actual_time.is_null() {
        // SAFETY: the caller's writable `CMTime *`, or null.
        unsafe { *actual_time = CM_TIME_ZERO };
    }
    if !error_out.is_null() {
        // SAFETY: the caller's writable `NSError **`, or null.
        unsafe {
            *error_out = error(
                DOMAIN,
                ERROR_NO_IMAGE_AT_TIME,
                "no decoder is available to generate an image from this asset",
            )
        };
    }
    std::ptr::null()
}

fn image_generator() {
    let class = image_generator_class();
    class_methods!(class, {
        "assetImageGeneratorWithAsset:" => asset_image_generator_with_asset, c"@24@0:8@16";
    });
    methods!(class, {
        "initWithAsset:" => init_with_asset, c"@24@0:8@16";
        "copyCGImageAtTime:actualTime:error:" => copy_cg_image_at_time,
            c"^{CGImage=}56@0:8{CMTime=qiIq}16^{CMTime=qiIq}40^@48";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_asset_has_no_tracks_and_zero_duration() {
        crate::bootstrap();
        let class = url_asset_class();
        let url_string = corefoundation::url::new("file:///tmp/example.mov");

        let implementation: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id = unsafe {
            std::mem::transmute(
                class
                    .metaclass()
                    .method(objc::sel::get("URLAssetWithURL:options:"))
                    .unwrap()
                    .implementation(),
            )
        };
        let pool = objc::arc::pool_push();
        let asset = unsafe {
            implementation(
                std::ptr::null_mut(),
                objc::sel::get("URLAssetWithURL:options:"),
                url_string as Id,
                std::ptr::null_mut(),
            )
        };
        assert!(!asset.is_null());

        let tracks_impl: unsafe extern "C" fn(Id, Sel) -> Id = unsafe {
            std::mem::transmute(
                class
                    .method(objc::sel::get("tracks"))
                    .unwrap()
                    .implementation(),
            )
        };
        let tracks = unsafe { tracks_impl(asset, objc::sel::get("tracks")) };
        let contents = unsafe { corefoundation::collections::array(tracks as _) }.expect("array");
        assert!(contents.values.is_empty());

        let duration_impl: unsafe extern "C" fn(Id, Sel) -> CmTime = unsafe {
            std::mem::transmute(
                class
                    .method(objc::sel::get("duration"))
                    .unwrap()
                    .implementation(),
            )
        };
        let duration = unsafe { duration_impl(asset, objc::sel::get("duration")) };
        assert_eq!(duration.value, 0);
        assert_eq!(duration.flags & CM_TIME_FLAGS_VALID, CM_TIME_FLAGS_VALID);

        objc::arc::pool_pop(pool);
        // SAFETY: a URL this test made and still owns a reference to.
        unsafe { corefoundation::api::CFRelease(url_string) };
    }

    #[test]
    fn an_image_generator_reports_no_image_rather_than_crashing() {
        crate::bootstrap();
        let class = image_generator_class();
        let implementation: unsafe extern "C" fn(Id, Sel, Id) -> Id = unsafe {
            std::mem::transmute(
                class
                    .metaclass()
                    .method(objc::sel::get("assetImageGeneratorWithAsset:"))
                    .unwrap()
                    .implementation(),
            )
        };
        let pool = objc::arc::pool_push();
        let generator = unsafe {
            implementation(
                std::ptr::null_mut(),
                objc::sel::get("assetImageGeneratorWithAsset:"),
                std::ptr::null_mut(),
            )
        };
        assert!(!generator.is_null());

        let copy_impl: unsafe extern "C" fn(
            Id,
            Sel,
            CmTime,
            *mut CmTime,
            *mut Id,
        ) -> *const c_void = unsafe {
            std::mem::transmute(
                class
                    .method(objc::sel::get("copyCGImageAtTime:actualTime:error:"))
                    .unwrap()
                    .implementation(),
            )
        };
        let mut error: Id = std::ptr::null_mut();
        let image = unsafe {
            copy_impl(
                generator,
                objc::sel::get("copyCGImageAtTime:actualTime:error:"),
                CM_TIME_ZERO,
                std::ptr::null_mut(),
                &mut error,
            )
        };
        assert!(image.is_null());
        assert!(!error.is_null());
        objc::arc::pool_pop(pool);
    }
}
