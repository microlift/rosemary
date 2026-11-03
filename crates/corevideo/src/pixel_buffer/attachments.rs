//! `CVBuffer` attachments: the colour, timing and aperture metadata a decoder
//! tags a frame with and a consumer reads back.

use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections::DictionaryContents;

use super::{foreign, refuse_foreign, state};

/// `kCVAttachmentMode_ShouldNotPropagate` and `kCVAttachmentMode_ShouldPropagate`.
const SHOULD_PROPAGATE: u32 = 1;

/// One dictionary per mode, made when the first attachment of that mode is.
///
/// Dictionaries rather than a list because `CVBufferGetAttachments` hands one
/// out under the get rule: it has to stay the same object while the buffer
/// lives, and is changed in place as Apple's is.
#[derive(Default)]
pub(super) struct Attachments {
    by_mode: [Option<CFTypeRef>; 2],
}

impl Drop for Attachments {
    fn drop(&mut self) {
        for dictionary in self.by_mode.iter().flatten() {
            // SAFETY: a dictionary this struct made and owns.
            unsafe { corefoundation::api::CFRelease(*dictionary) };
        }
    }
}

fn index(mode: u32) -> usize {
    usize::from(mode == SHOULD_PROPAGATE)
}

fn contents<'a>(dictionary: CFTypeRef) -> &'a mut DictionaryContents {
    // SAFETY: a dictionary `Attachments` made.
    unsafe { corefoundation::collections::dictionary(dictionary) }
        .expect("an attachment dictionary is a CFDictionary")
}

impl Attachments {
    fn dictionary(&mut self, mode: u32) -> &mut DictionaryContents {
        let dictionary = self.by_mode[index(mode)].get_or_insert_with(|| {
            let policy = Policy::from_type_callbacks();
            corefoundation::collections::new_dictionary(&[], policy, policy, true)
        });
        contents(*dictionary)
    }

    fn remove(&mut self, key: CFTypeRef) {
        for dictionary in self.by_mode.iter().flatten() {
            let contents = contents(*dictionary);
            if let Some(at) = contents.position(key) {
                let (key, value) = contents.entries.remove(at);
                contents.keys.release(key);
                contents.values.release(value);
            }
        }
    }

    fn set(&mut self, key: CFTypeRef, value: CFTypeRef, mode: u32) {
        // A key has one mode at a time; setting it again moves it.
        let other = self.by_mode[1 - index(mode)];
        if let Some(at) = other.and_then(|d| contents(d).position(key)) {
            let contents = contents(other.unwrap());
            let (key, value) = contents.entries.remove(at);
            contents.keys.release(key);
            contents.values.release(value);
        }
        self.dictionary(mode).set(key, value);
    }

    fn get(&self, key: CFTypeRef) -> Option<(CFTypeRef, u32)> {
        self.by_mode
            .iter()
            .zip([0, SHOULD_PROPAGATE])
            .find_map(|(d, mode)| {
                let contents = contents((*d)?);
                let at = contents.position(key)?;
                Some((contents.entries[at].1, mode))
            })
    }

    /// The dictionary for `mode`, or null when it has nothing in it.
    fn of_mode(&self, mode: u32) -> CFTypeRef {
        self.by_mode[index(mode)]
            .filter(|d| !contents(*d).entries.is_empty())
            .unwrap_or(std::ptr::null())
    }
}

fn with<R>(buffer: CFTypeRef, f: impl FnOnce(&mut Attachments) -> R) -> Option<R> {
    let state = state(buffer)?;
    let mut attachments = state.attachments.lock().unwrap_or_else(|e| e.into_inner());
    Some(f(&mut attachments))
}

/// `void CVBufferSetAttachment(CVBufferRef, CFStringRef key, CFTypeRef value,
/// CVAttachmentMode)`
pub unsafe extern "C" fn CVBufferSetAttachment(
    buffer: CFTypeRef,
    key: CFTypeRef,
    value: CFTypeRef,
    mode: u32,
) {
    refuse_foreign(c"CVBufferSetAttachment", buffer);
    if key.is_null() || value.is_null() {
        return;
    }
    with(buffer, |a| a.set(key, value, mode));
}

/// `CFTypeRef CVBufferGetAttachment(CVBufferRef, CFStringRef key,
/// CVAttachmentMode *)`, under the get rule.
pub unsafe extern "C" fn CVBufferGetAttachment(
    buffer: CFTypeRef,
    key: CFTypeRef,
    mode: *mut u32,
) -> CFTypeRef {
    refuse_foreign(c"CVBufferGetAttachment", buffer);
    let Some((value, found)) = with(buffer, |a| a.get(key)).flatten() else {
        return std::ptr::null();
    };
    if !mode.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *mode = found };
    }
    value
}

/// `CFTypeRef CVBufferCopyAttachment(CVBufferRef, CFStringRef key,
/// CVAttachmentMode *)`, retained.
pub unsafe extern "C" fn CVBufferCopyAttachment(
    buffer: CFTypeRef,
    key: CFTypeRef,
    mode: *mut u32,
) -> CFTypeRef {
    refuse_foreign(c"CVBufferCopyAttachment", buffer);
    // The lookup and the retain share one lock, so a concurrent removal cannot
    // free the value between them.
    let Some((value, found)) = with(buffer, |a| {
        a.get(key).map(|(value, found)| {
            // SAFETY: a live value the dictionary holds.
            (unsafe { corefoundation::api::CFRetain(value) }, found)
        })
    })
    .flatten() else {
        return std::ptr::null();
    };
    if !mode.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *mode = found };
    }
    value
}

/// `void CVBufferRemoveAttachment(CVBufferRef, CFStringRef key)`
pub unsafe extern "C" fn CVBufferRemoveAttachment(buffer: CFTypeRef, key: CFTypeRef) {
    refuse_foreign(c"CVBufferRemoveAttachment", buffer);
    with(buffer, |a| a.remove(key));
}

/// `void CVBufferRemoveAllAttachments(CVBufferRef)`
pub unsafe extern "C" fn CVBufferRemoveAllAttachments(buffer: CFTypeRef) {
    forward_foreign!(
        c"CVBufferRemoveAllAttachments",
        buffer,
        fn(CFTypeRef) -> (),
        (buffer)
    );
    with(buffer, |a| {
        for dictionary in a.by_mode.iter().flatten() {
            let contents = contents(*dictionary);
            for (key, value) in contents.entries.drain(..) {
                contents.keys.release(key);
                contents.values.release(value);
            }
        }
    });
}

/// `CFDictionaryRef CVBufferGetAttachments(CVBufferRef, CVAttachmentMode)`,
/// under the get rule; null when the buffer has none of that mode.
pub unsafe extern "C" fn CVBufferGetAttachments(buffer: CFTypeRef, mode: u32) -> CFTypeRef {
    refuse_foreign(c"CVBufferGetAttachments", buffer);
    with(buffer, |a| a.of_mode(mode)).unwrap_or(std::ptr::null())
}

/// `CFDictionaryRef CVBufferCopyAttachments(CVBufferRef, CVAttachmentMode)`,
/// an immutable copy; null when the buffer has none of that mode.
pub unsafe extern "C" fn CVBufferCopyAttachments(buffer: CFTypeRef, mode: u32) -> CFTypeRef {
    refuse_foreign(c"CVBufferCopyAttachments", buffer);
    with(buffer, |a| {
        let dictionary = a.of_mode(mode);
        if dictionary.is_null() {
            return dictionary;
        }
        // SAFETY: a live dictionary, copied under the lock that guards it.
        unsafe { corefoundation::api::CFDictionaryCreateCopy(std::ptr::null(), dictionary) }
    })
    .unwrap_or(std::ptr::null())
}

/// `void CVBufferSetAttachments(CVBufferRef, CFDictionaryRef, CVAttachmentMode)`
pub unsafe extern "C" fn CVBufferSetAttachments(
    buffer: CFTypeRef,
    dictionary: CFTypeRef,
    mode: u32,
) {
    refuse_foreign(c"CVBufferSetAttachments", buffer);
    // SAFETY: null or a dictionary the caller passed.
    let Some(source) = (unsafe { corefoundation::collections::dictionary(dictionary) }) else {
        return;
    };
    let pairs = source.entries.clone();
    with(buffer, |a| {
        for (key, value) in pairs {
            a.set(key, value, mode);
        }
    });
}

/// `void CVBufferPropagateAttachments(CVBufferRef source, CVBufferRef
/// destination)`: the source's propagating attachments, onto the destination.
pub unsafe extern "C" fn CVBufferPropagateAttachments(source: CFTypeRef, destination: CFTypeRef) {
    refuse_foreign(c"CVBufferPropagateAttachments", source);
    refuse_foreign(c"CVBufferPropagateAttachments", destination);
    let pairs = with(source, |a| {
        let dictionary = a.of_mode(SHOULD_PROPAGATE);
        if dictionary.is_null() {
            return Vec::new();
        }
        // The values are retained while the source's lock is released, so
        // the source dropping them meanwhile cannot free them.
        contents(dictionary)
            .entries
            .iter()
            // SAFETY: live keys and values the dictionary holds.
            .map(|&(k, v)| unsafe {
                (
                    corefoundation::api::CFRetain(k),
                    corefoundation::api::CFRetain(v),
                )
            })
            .collect()
    })
    .unwrap_or_default();
    with(destination, |a| {
        for &(key, value) in &pairs {
            a.set(key, value, SHOULD_PROPAGATE);
        }
    });
    for (key, value) in pairs {
        // SAFETY: the references taken above.
        unsafe {
            corefoundation::api::CFRelease(key);
            corefoundation::api::CFRelease(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{BGRA, CVPixelBufferCreate, CVPixelBufferRelease, SUCCESS, key};
    use super::*;

    fn create() -> CFTypeRef {
        let mut out = std::ptr::null();
        assert_eq!(
            unsafe {
                CVPixelBufferCreate(std::ptr::null(), 7, 5, BGRA, std::ptr::null(), &mut out)
            },
            SUCCESS
        );
        out
    }

    #[test]
    fn an_attachment_is_read_back_with_its_mode() {
        let b = create();
        let matrix = key("CVImageBufferYCbCrMatrix");
        let value = key("ITU_R_709_2");
        unsafe {
            assert!(CVBufferGetAttachment(b, matrix, std::ptr::null_mut()).is_null());
            assert!(CVBufferGetAttachments(b, SHOULD_PROPAGATE).is_null());
            CVBufferSetAttachment(b, matrix, value, SHOULD_PROPAGATE);
            let mut mode = 7;
            assert_eq!(CVBufferGetAttachment(b, matrix, &mut mode), value);
            assert_eq!(mode, SHOULD_PROPAGATE);
            let all = CVBufferGetAttachments(b, SHOULD_PROPAGATE);
            assert_eq!(
                corefoundation::api::CFDictionaryGetValue(all, matrix),
                value
            );
            assert!(CVBufferGetAttachments(b, 0).is_null());

            // Setting it in the other mode moves it.
            CVBufferSetAttachment(b, matrix, value, 0);
            assert_eq!(CVBufferGetAttachment(b, matrix, &mut mode), value);
            assert_eq!(mode, 0);
            assert!(CVBufferGetAttachments(b, SHOULD_PROPAGATE).is_null());

            let copied = CVBufferCopyAttachment(b, matrix, std::ptr::null_mut());
            assert_eq!(copied, value);
            corefoundation::api::CFRelease(copied);
            CVBufferRemoveAttachment(b, matrix);
            assert!(CVBufferGetAttachment(b, matrix, std::ptr::null_mut()).is_null());
            CVPixelBufferRelease(b);
        }
    }

    #[test]
    fn only_propagating_attachments_propagate() {
        let source = create();
        let destination = create();
        let (kept, dropped) = (key("CVFieldCount"), key("CVImageBufferCGColorSpace"));
        let value = key("value");
        unsafe {
            CVBufferSetAttachment(source, kept, value, SHOULD_PROPAGATE);
            CVBufferSetAttachment(source, dropped, value, 0);
            CVBufferPropagateAttachments(source, destination);
            assert_eq!(
                CVBufferGetAttachment(destination, kept, std::ptr::null_mut()),
                value
            );
            assert!(CVBufferGetAttachment(destination, dropped, std::ptr::null_mut()).is_null());

            let copy = CVBufferCopyAttachments(source, 0);
            assert_eq!(corefoundation::api::CFDictionaryGetCount(copy), 1);
            CVBufferRemoveAllAttachments(source);
            assert!(CVBufferGetAttachments(source, 0).is_null());
            // The copy is its own and outlives the removal.
            assert_eq!(
                corefoundation::api::CFDictionaryGetValue(copy, dropped),
                value
            );
            corefoundation::api::CFRelease(copy);

            CVBufferSetAttachments(destination, copy_of(&[(dropped, value)]), 0);
            assert_eq!(
                CVBufferGetAttachment(destination, dropped, std::ptr::null_mut()),
                value
            );
            CVPixelBufferRelease(source);
            CVPixelBufferRelease(destination);
        }
    }

    /// An autoreleased-in-spirit dictionary: leaked, which a test can afford.
    fn copy_of(pairs: &[(CFTypeRef, CFTypeRef)]) -> CFTypeRef {
        let policy = Policy::from_type_callbacks();
        corefoundation::collections::new_dictionary(pairs, policy, policy, false)
    }
}
