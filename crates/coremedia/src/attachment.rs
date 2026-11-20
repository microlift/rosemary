//! `CMAttachmentBearer`: the attachments of whatever CoreMedia or CoreVideo
//! object carries them.
//!
//! A sample buffer's own attachments would come from `CMSetAttachment`, which
//! is not implemented, so it has none. A CoreVideo buffer's are CoreVideo's.

#![allow(non_snake_case)]

use corefoundation::api::CFAllocatorRef;
use corefoundation::base::CFTypeRef;

/// `CFDictionaryRef CMCopyDictionaryOfAttachments(CFAllocatorRef allocator,
/// CMAttachmentBearerRef target, CMAttachmentMode attachmentMode)`: retained,
/// or null when there are none.
///
/// # Safety
///
/// `target` is null or a live CF object.
pub unsafe extern "C" fn CMCopyDictionaryOfAttachments(
    _allocator: CFAllocatorRef,
    target: CFTypeRef,
    mode: u32,
) -> CFTypeRef {
    // SAFETY: null or the caller's object, which CoreVideo checks is its own.
    unsafe { corevideo::pixel_buffer::CVBufferCopyAttachments(target, mode) }
}

exports::exports! {
    functions {
        "CMCopyDictionaryOfAttachments" => CMCopyDictionaryOfAttachments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `kCVAttachmentMode_ShouldNotPropagate` and `kCVAttachmentMode_ShouldPropagate`.
    const SHOULD_NOT_PROPAGATE: u32 = 0;
    const SHOULD_PROPAGATE: u32 = 1;

    fn key(text: &str) -> CFTypeRef {
        corefoundation::base::interned_name(text) as usize as CFTypeRef
    }

    #[test]
    fn a_pixel_buffer_copies_its_attachments_of_a_mode() {
        corefoundation::bootstrap();
        let bgra = u32::from_be_bytes(*b"BGRA");
        let mut buffer = std::ptr::null();
        unsafe {
            assert_eq!(
                corevideo::pixel_buffer::CVPixelBufferCreate(
                    std::ptr::null(),
                    4,
                    4,
                    bgra,
                    std::ptr::null(),
                    &mut buffer
                ),
                0
            );
            assert!(
                CMCopyDictionaryOfAttachments(std::ptr::null(), buffer, SHOULD_PROPAGATE).is_null()
            );
            corevideo::pixel_buffer::CVBufferSetAttachment(
                buffer,
                key("CVImageBufferYCbCrMatrix"),
                key("ITU_R_709_2"),
                SHOULD_PROPAGATE,
            );
            let copied = CMCopyDictionaryOfAttachments(std::ptr::null(), buffer, SHOULD_PROPAGATE);
            assert!(!copied.is_null());
            assert_eq!(
                corefoundation::api::CFDictionaryGetValue(copied, key("CVImageBufferYCbCrMatrix")),
                key("ITU_R_709_2")
            );
            assert!(
                CMCopyDictionaryOfAttachments(std::ptr::null(), buffer, SHOULD_NOT_PROPAGATE)
                    .is_null()
            );
            corefoundation::api::CFRelease(copied);
            corefoundation::api::CFRelease(buffer);
        }
    }

    #[test]
    fn something_without_attachments_copies_null() {
        corefoundation::bootstrap();
        unsafe {
            assert!(
                CMCopyDictionaryOfAttachments(std::ptr::null(), std::ptr::null(), SHOULD_PROPAGATE)
                    .is_null()
            );
            let not_a_buffer = key("CVImageBufferYCbCrMatrix");
            assert!(CMCopyDictionaryOfAttachments(std::ptr::null(), not_a_buffer, 1).is_null());
        }
    }
}
