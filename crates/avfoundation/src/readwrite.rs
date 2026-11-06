//! `AVAssetReader`, `AVAssetReaderTrackOutput`, `AVAssetWriter`,
//! `AVAssetWriterInput` and `AVAssetWriterInputPixelBufferAdaptor`.
//!
//! Nothing in this tree demuxes or encodes any media container (see
//! `asset.rs`), so a reader or writer that would need to can never actually
//! do its job. Rather than hand back an object that silently produces no
//! samples, `AVAssetReader`/`AVAssetWriter` fail at construction with a
//! documented `NSError`, the same shape a real `-initWithURL:error:` failure
//! takes - not quite how Apple's reader behaves with an empty-tracked asset
//! (it would normally still construct), but the honest answer here, since
//! this runtime has no read or write path to offer underneath it.
//!
//! FIXME (ACCURACY): a real `AVAssetReader`/`AVAssetWriter` usually succeeds
//! at construction even for an asset with no readable tracks, only failing
//! once reading/writing starts. Failing at `-init` instead is simpler and
//! never silently pretends a session is live.

use objc::object::Id;
use objc::sel::Sel;

use foundation::error::make as error;

use crate::define::class_methods;

/// `AVFoundationErrorDomain`.
const DOMAIN: &str = "AVFoundationErrorDomain";

/// `AVErrorFileFailedToParse`, from AVError.h.
const ERROR_FILE_FAILED_TO_PARSE: i64 = -11829;

/// `AVErrorEncoderNotFound`, from AVError.h.
const ERROR_ENCODER_NOT_FOUND: i64 = -11834;

/// Write `error` through a guest's `NSError **`, if it passed one.
///
/// # Safety
///
/// `out` must be null or a writable `Id` slot.
unsafe fn report(out: *mut Id, code: i64, description: &str) {
    if out.is_null() {
        return;
    }
    // SAFETY: the caller's obligation.
    unsafe { *out = error(DOMAIN, code, description) };
}

pub fn install() {
    asset_reader();
    asset_reader_track_output();
    asset_writer();
    asset_writer_input();
    asset_writer_input_pixel_buffer_adaptor();
}

/// A fresh, plain instance of `class`, autoreleased - the one every
/// constructor below that does not fail returns.
fn instance(class: &'static objc::class::Class) -> Id {
    objc::arc::autorelease(objc::object::create_instance(class, 0))
}

// -- AVAssetReader ----------------------------------------------------------

unsafe extern "C" fn asset_reader_with_asset(
    _class: Id,
    _sel: Sel,
    _asset: Id,
    error_out: *mut Id,
) -> Id {
    // SAFETY: the caller's `NSError **`, or null.
    unsafe {
        report(
            error_out,
            ERROR_FILE_FAILED_TO_PARSE,
            "no demuxer is available to read this asset",
        )
    };
    std::ptr::null_mut()
}

unsafe extern "C" fn init_with_asset(
    receiver: Id,
    _sel: Sel,
    _asset: Id,
    error_out: *mut Id,
) -> Id {
    objc::arc::release(receiver);
    // SAFETY: as above.
    unsafe {
        report(
            error_out,
            ERROR_FILE_FAILED_TO_PARSE,
            "no demuxer is available to read this asset",
        )
    };
    std::ptr::null_mut()
}

fn asset_reader() {
    let class = corefoundation::base::abstract_class(c"AVAssetReader", None);
    class_methods!(class, {
        "assetReaderWithAsset:error:" => asset_reader_with_asset, c"@32@0:8@16^@24";
    });
    crate::define::methods!(class, {
        "initWithAsset:error:" => init_with_asset, c"@32@0:8@16^@24";
    });
}

// -- AVAssetReaderTrackOutput ------------------------------------------------

/// `+[AVAssetReaderTrackOutput assetReaderTrackOutputWithTrack:outputSettings:]`.
///
/// Unreachable in practice - no `AVAssetTrack` this runtime hands out is ever
/// real, since every asset's `-tracks` is empty - but registered and
/// constructible rather than left to crash a caller that tries anyway.
unsafe extern "C" fn track_output_with_track(
    class: Id,
    _sel: Sel,
    _track: Id,
    _output_settings: Id,
) -> Id {
    // SAFETY: this method's own receiver, a registered class.
    let class =
        unsafe { objc::class::from_ptr(class as *const objc::class::Class) }.expect("a class");
    instance(class)
}

fn asset_reader_track_output() {
    let class = corefoundation::base::abstract_class(c"AVAssetReaderTrackOutput", None);
    class_methods!(class, {
        "assetReaderTrackOutputWithTrack:outputSettings:" => track_output_with_track, c"@32@0:8@16@24";
    });
}

// -- AVAssetWriter ----------------------------------------------------------

unsafe extern "C" fn asset_writer_with_url(
    _class: Id,
    _sel: Sel,
    _url: Id,
    _file_type: Id,
    error_out: *mut Id,
) -> Id {
    // SAFETY: the caller's `NSError **`, or null.
    unsafe {
        report(
            error_out,
            ERROR_ENCODER_NOT_FOUND,
            "no encoder is available to write media",
        )
    };
    std::ptr::null_mut()
}

unsafe extern "C" fn init_with_url(
    receiver: Id,
    _sel: Sel,
    _url: Id,
    _file_type: Id,
    error_out: *mut Id,
) -> Id {
    objc::arc::release(receiver);
    // SAFETY: as above.
    unsafe {
        report(
            error_out,
            ERROR_ENCODER_NOT_FOUND,
            "no encoder is available to write media",
        )
    };
    std::ptr::null_mut()
}

fn asset_writer() {
    let class = corefoundation::base::abstract_class(c"AVAssetWriter", None);
    class_methods!(class, {
        "assetWriterWithURL:fileType:error:" => asset_writer_with_url, c"@40@0:8@16@24^@32";
    });
    crate::define::methods!(class, {
        "initWithURL:fileType:error:" => init_with_url, c"@40@0:8@16@24^@32";
    });
}

// -- AVAssetWriterInput ------------------------------------------------------

/// `+[AVAssetWriterInput assetWriterInputWithMediaType:outputSettings:]`.
///
/// This much is harmless to construct on its own - a real app builds inputs
/// before trying to add them to a writer - so it succeeds; it is adding one
/// to an `AVAssetWriter` that can never happen, because the writer itself
/// never exists.
unsafe extern "C" fn writer_input_with_media_type(
    class: Id,
    _sel: Sel,
    _media_type: Id,
    _output_settings: Id,
) -> Id {
    // SAFETY: this method's own receiver, a registered class.
    let class =
        unsafe { objc::class::from_ptr(class as *const objc::class::Class) }.expect("a class");
    instance(class)
}

fn asset_writer_input() {
    let class = corefoundation::base::abstract_class(c"AVAssetWriterInput", None);
    class_methods!(class, {
        "assetWriterInputWithMediaType:outputSettings:" => writer_input_with_media_type, c"@32@0:8@16@24";
    });
}

// -- AVAssetWriterInputPixelBufferAdaptor ------------------------------------

unsafe extern "C" fn pixel_buffer_adaptor_with_input(
    class: Id,
    _sel: Sel,
    _input: Id,
    _attributes: Id,
) -> Id {
    // SAFETY: this method's own receiver, a registered class.
    let class =
        unsafe { objc::class::from_ptr(class as *const objc::class::Class) }.expect("a class");
    instance(class)
}

fn asset_writer_input_pixel_buffer_adaptor() {
    let class = corefoundation::base::abstract_class(c"AVAssetWriterInputPixelBufferAdaptor", None);
    class_methods!(class, {
        "assetWriterInputPixelBufferAdaptorWithAssetWriterInput:sourcePixelBufferAttributes:"
            => pixel_buffer_adaptor_with_input, c"@32@0:8@16@24";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_reader_fails_with_a_documented_error() {
        crate::bootstrap();
        let class = objc::class::lookup(c"AVAssetReader").expect("registered");
        let implementation: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id = unsafe {
            std::mem::transmute(
                class
                    .metaclass()
                    .method(objc::sel::get("assetReaderWithAsset:error:"))
                    .unwrap()
                    .implementation(),
            )
        };
        let mut error: Id = std::ptr::null_mut();
        let pool = objc::arc::pool_push();
        let result = unsafe {
            implementation(
                std::ptr::null_mut(),
                objc::sel::get("assetReaderWithAsset:error:"),
                std::ptr::null_mut(),
                &mut error,
            )
        };
        assert!(result.is_null());
        assert!(!error.is_null());
        objc::arc::pool_pop(pool);
    }

    #[test]
    fn asset_writer_input_constructs_without_a_writer() {
        crate::bootstrap();
        let class = objc::class::lookup(c"AVAssetWriterInput").expect("registered");
        let implementation: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id = unsafe {
            std::mem::transmute(
                class
                    .metaclass()
                    .method(objc::sel::get(
                        "assetWriterInputWithMediaType:outputSettings:",
                    ))
                    .unwrap()
                    .implementation(),
            )
        };
        let pool = objc::arc::pool_push();
        let input = unsafe {
            implementation(
                objc::class::ptr(class) as Id,
                objc::sel::get("assetWriterInputWithMediaType:outputSettings:"),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert!(!input.is_null());
        objc::arc::pool_pop(pool);
    }
}
