//! Font collections, and choosing a font for text another font lacks.
//!
//! A collection is the array of descriptors it matches, as a font and a
//! descriptor here are both their attribute dictionary.

use corefoundation::base::CFTypeRef;
use corefoundation::collections;

use crate::{face_attributes, font_size, fonts, get_string, loaded, make_font, release};

/// `CTFontCollectionRef CTFontCollectionCreateFromAvailableFonts(CFDictionaryRef)`.
pub unsafe extern "C" fn CTFontCollectionCreateFromAvailableFonts(
    _options: CFTypeRef,
) -> CFTypeRef {
    let descriptors: Vec<CFTypeRef> = fonts::all_faces()
        .iter()
        .map(|face| face_attributes(face, 0.0))
        .collect();
    let array = collections::new_array(&descriptors, crate::type_policy(), false);
    descriptors.into_iter().for_each(release);
    array
}

/// `CFArrayRef CTFontCollectionCreateMatchingFontDescriptors(CTFontCollectionRef)`:
/// null for an empty collection, as CoreText answers.
pub unsafe extern "C" fn CTFontCollectionCreateMatchingFontDescriptors(
    collection: CFTypeRef,
) -> CFTypeRef {
    // SAFETY: null or an object this runtime allocated.
    match unsafe { collections::array(collection) } {
        Some(array) if !array.values.is_empty() => {
            // SAFETY: an immutable array, shared rather than copied.
            unsafe { corefoundation::api::CFRetain(collection) }
        }
        _ => std::ptr::null(),
    }
}

/// `CFRange`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CFRange {
    pub location: isize,
    pub length: isize,
}

/// `CTFontRef CTFontCreateForString(CTFontRef current, CFStringRef, CFRange)`:
/// `current` if it has a glyph for every character of the range, and
/// otherwise a face of the same size with one for the first it lacks.
pub unsafe extern "C" fn CTFontCreateForString(
    current: CFTypeRef,
    text: CFTypeRef,
    range: CFRange,
) -> CFTypeRef {
    if current.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a string, or something `bytes` answers empty for.
    let utf8 = unsafe { foundation::string::bytes(text as _) };
    let units: Vec<u16> = String::from_utf8_lossy(&utf8).encode_utf16().collect();
    let start = usize::try_from(range.location)
        .unwrap_or(0)
        .min(units.len());
    let end = start
        .saturating_add(usize::try_from(range.length).unwrap_or(0))
        .min(units.len());
    let face = loaded(current);
    let covers = |c: char| {
        face.as_ref()
            .is_some_and(|face| face.face().glyph_index(c).is_some())
    };
    let missing = char::decode_utf16(units[start..end].iter().copied())
        .filter_map(Result::ok)
        .find(|c| !covers(*c));
    let Some(missing) = missing else {
        // SAFETY: a live font, retained for the caller.
        return unsafe { corefoundation::api::CFRetain(current) };
    };
    let family = get_string(current, "CTFontFamilyNameAttribute");
    // The current family's other faces first, then any face at all.
    let mut faces = fonts::all_faces();
    faces.sort_by_key(|face| Some(&face.family) != family.as_ref());
    let Some(found) = faces.into_iter().find(|face| {
        fonts::load(&face.path, face.index)
            .is_some_and(|loaded| loaded.face().glyph_index(missing).is_some())
    }) else {
        // SAFETY: as above. No face has the character, so `current` is as
        // good as any.
        return unsafe { corefoundation::api::CFRetain(current) };
    };
    let descriptor = face_attributes(&found, font_size(current));
    let font = make_font(descriptor, font_size(current));
    release(descriptor);
    font
}
