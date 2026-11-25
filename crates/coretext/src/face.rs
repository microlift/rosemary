//! What a font's face says about itself: its tables, characters and names.

use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections;

use crate::{get, key, loaded, string};

/// The name keys, as `kCTFont...NameKey` spell them, and the `name` table
/// record each reads.
pub(crate) const NAME_KEYS: &[(&str, &str, u16)] = &[
    ("_kCTFontCopyrightNameKey", "CTFontCopyrightNameKey", 0),
    ("_kCTFontFamilyNameKey", "CTFontFamilyNameKey", 1),
    ("_kCTFontSubFamilyNameKey", "CTFontSubFamilyNameKey", 2),
    ("_kCTFontStyleNameKey", "CTFontStyleNameKey", 2),
    ("_kCTFontUniqueNameKey", "CTFontUniqueNameKey", 3),
    ("_kCTFontFullNameKey", "CTFontFullNameKey", 4),
    ("_kCTFontVersionNameKey", "CTFontVersionNameKey", 5),
    ("_kCTFontPostScriptNameKey", "CTFontPostScriptNameKey", 6),
    ("_kCTFontTrademarkNameKey", "CTFontTrademarkNameKey", 7),
    (
        "_kCTFontManufacturerNameKey",
        "CTFontManufacturerNameKey",
        8,
    ),
    ("_kCTFontDesignerNameKey", "CTFontDesignerNameKey", 9),
    ("_kCTFontDescriptionNameKey", "CTFontDescriptionNameKey", 10),
    ("_kCTFontVendorURLNameKey", "CTFontVendorURLNameKey", 11),
    ("_kCTFontDesignerURLNameKey", "CTFontDesignerURLNameKey", 12),
    ("_kCTFontLicenseNameKey", "CTFontLicenseNameKey", 13),
    ("_kCTFontLicenseURLNameKey", "CTFontLicenseURLNameKey", 14),
    ("_kCTFontSampleTextNameKey", "CTFontSampleTextNameKey", 19),
    (
        "_kCTFontPostScriptCIDNameKey",
        "CTFontPostScriptCIDNameKey",
        20,
    ),
];

/// The language `CTFontCopyLocalizedName` reports.
///
/// FIXME (ACCURACY): a name read from a record in another language, used when
/// the face has none in English, is still reported as English.
const ENGLISH: &str = "en";

/// The `name` record `key` reads, or `None` for a key that is not a name key.
fn name_id(name_key: CFTypeRef) -> Option<u16> {
    if name_key.is_null() {
        return None;
    }
    NAME_KEYS
        .iter()
        // SAFETY: both are strings.
        .find(|(_, value, _)| unsafe { corefoundation::api::CFEqual(name_key, key(value)) } != 0)
        .map(|(_, _, id)| *id)
}

/// Record `id` of `font`'s `name` table, US English first.
pub(crate) fn name_record(font: CFTypeRef, id: u16) -> Option<String> {
    let face = loaded(font)?;
    let names = face.face().names();
    let readable = || {
        names
            .into_iter()
            .filter(|name| name.name_id == id && name.is_unicode())
    };
    readable()
        .find(|name| name.language() == owned_ttf_parser::Language::English_UnitedStates)
        .or_else(|| readable().next())
        .and_then(|name| name.to_string())
}

/// A name of `font`, from its `name` table where it has the record.
pub(crate) fn copy_name(font: CFTypeRef, name_key: CFTypeRef) -> Option<CFTypeRef> {
    let id = name_id(name_key)?;
    name_record(font, id).map(|name| string(&name))
}

/// `CFStringRef CTFontCopyFullName(CTFontRef)`.
pub unsafe extern "C" fn CTFontCopyFullName(font: CFTypeRef) -> CFTypeRef {
    // SAFETY: the caller's font, and a name key.
    unsafe { crate::CTFontCopyName(font, key("CTFontFullNameKey")) }
}

/// `CFStringRef CTFontCopyLocalizedName(CTFontRef, CFStringRef nameKey,
/// CFStringRef *actualLanguage)`.
pub unsafe extern "C" fn CTFontCopyLocalizedName(
    font: CFTypeRef,
    name_key: CFTypeRef,
    actual_language: *mut CFTypeRef,
) -> CFTypeRef {
    // SAFETY: the caller's arguments, unchanged.
    let name = unsafe { crate::CTFontCopyName(font, name_key) };
    if !actual_language.is_null() {
        let language = match name.is_null() {
            true => std::ptr::null(),
            false => string(ENGLISH),
        };
        // SAFETY: the caller's optional out-parameter.
        unsafe { *actual_language = language };
    }
    name
}

/// `CFArrayRef CTFontCopyAvailableTables(CTFontRef, CTFontTableOptions)`:
/// each table tag as the value itself, which is how CoreText's array holds
/// them.
pub unsafe extern "C" fn CTFontCopyAvailableTables(font: CFTypeRef, _options: u32) -> CFTypeRef {
    let Some(face) = loaded(font) else {
        return std::ptr::null();
    };
    let tags: Vec<CFTypeRef> = face
        .face()
        .raw_face()
        .table_records
        .into_iter()
        .map(|record| record.tag.0 as usize as CFTypeRef)
        .collect();
    collections::new_array(&tags, Policy::NONE, false)
}

/// `CFCharacterSetRef CTFontCopyCharacterSet(CTFontRef)`: every character the
/// face's Unicode character maps give a glyph.
pub unsafe extern "C" fn CTFontCopyCharacterSet(font: CFTypeRef) -> CFTypeRef {
    let Some(face) = loaded(font) else {
        return std::ptr::null();
    };
    let mut characters = Vec::new();
    if let Some(cmap) = face.face().tables().cmap {
        for subtable in cmap.subtables {
            if !subtable.is_unicode() {
                continue;
            }
            subtable.codepoints(|c| {
                if subtable.glyph_index(c).is_some_and(|glyph| glyph.0 != 0) {
                    characters.push(c);
                }
            });
        }
    }
    characters.sort_unstable();
    characters.dedup();
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for c in characters {
        match ranges.last_mut() {
            Some((_, last)) if *last + 1 == c => *last = c,
            _ => ranges.push((c, c)),
        }
    }
    foundation::characterset::create_with_ranges(ranges) as CFTypeRef
}

/// `CFDictionaryRef CTFontCopyVariation(CTFontRef)`: the axis values the font
/// was made with, an empty dictionary for a variable font at its defaults,
/// and null for a face with no axes.
pub unsafe extern "C" fn CTFontCopyVariation(font: CFTypeRef) -> CFTypeRef {
    let variation = get(font, "NSCTFontVariationAttribute");
    if !variation.is_null() {
        // SAFETY: a live attribute value.
        return unsafe { corefoundation::api::CFRetain(variation) };
    }
    match loaded(font) {
        Some(face) if face.face().is_variable() => crate::new_dictionary(&[]),
        _ => std::ptr::null(),
    }
}
