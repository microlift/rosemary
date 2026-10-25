//! The attribute dictionary a `CGFont` and a `CTFont` both are.
//!
//! A font here is a Core Foundation dictionary naming a face by its file and
//! index, plus the names, size and traits a program reads back. CoreText and
//! CoreGraphics share it so a font made by one is the same object to the other.

#![allow(non_snake_case)]

use std::sync::Arc;

use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections::{self, Number};

use crate::fonts::{self, FaceInfo, LoadedFace};

/// Where a resolved font's face lives. Private to this runtime, so a guest
/// copying attributes between descriptors carries them along unexamined.
pub const PATH_KEY: &str = "RosemaryFontFilePath";
pub const INDEX_KEY: &str = "RosemaryFontFileIndex";

/// A size CoreText uses for a font created with none.
pub const DEFAULT_SIZE: f64 = 12.0;

/// `kCTFontItalicTrait`, `kCTFontBoldTrait` and `kCTFontMonoSpaceTrait`.
pub const ITALIC_TRAIT: u32 = 1 << 0;
pub const BOLD_TRAIT: u32 = 1 << 1;
pub const MONOSPACE_TRAIT: u32 = 1 << 10;

pub fn key(text: &'static str) -> CFTypeRef {
    corefoundation::base::interned_name(text) as CFTypeRef
}

pub fn type_policy() -> Policy {
    Policy::from_type_callbacks()
}

pub fn new_dictionary(pairs: &[(CFTypeRef, CFTypeRef)]) -> CFTypeRef {
    collections::new_dictionary(pairs, Policy::from_key_callbacks(), type_policy(), false)
}

pub fn release(object: CFTypeRef) {
    if !object.is_null() {
        // SAFETY: a reference the caller owns.
        unsafe { corefoundation::api::CFRelease(object) };
    }
}

pub fn get(dictionary: CFTypeRef, name: &'static str) -> CFTypeRef {
    // SAFETY: null, or a dictionary a guest received from this runtime.
    match unsafe { collections::dictionary(dictionary) } {
        Some(_) => unsafe { corefoundation::api::CFDictionaryGetValue(dictionary, key(name)) },
        None => std::ptr::null(),
    }
}

pub fn get_string(dictionary: CFTypeRef, name: &'static str) -> Option<String> {
    let value = get(dictionary, name);
    if value.is_null() {
        return None;
    }
    // SAFETY: font attributes named as strings hold strings.
    let bytes = unsafe { corefoundation::string::as_string(value) }
        .map(|text| text.utf8())
        .unwrap_or_default();
    String::from_utf8(bytes).ok()
}

pub fn get_number(dictionary: CFTypeRef, name: &'static str) -> Option<f64> {
    let value = get(dictionary, name);
    // SAFETY: null or an object this runtime allocated.
    unsafe { collections::number(value) }.map(|number| number.as_f64())
}

pub fn string(text: &str) -> CFTypeRef {
    corefoundation::string::from_bytes(text.as_bytes(), false) as CFTypeRef
}

/// CoreText's weight trait, from -1 to 1, as an OpenType weight class.
pub fn weight_class(weight: f64) -> u16 {
    const STOPS: [(f64, u16); 9] = [
        (-0.8, 100),
        (-0.6, 200),
        (-0.4, 300),
        (0.0, 400),
        (0.23, 500),
        (0.3, 600),
        (0.4, 700),
        (0.56, 800),
        (0.62, 900),
    ];
    STOPS
        .iter()
        .min_by(|a, b| (a.0 - weight).abs().total_cmp(&(b.0 - weight).abs()))
        .map_or(400, |stop| stop.1)
}

/// The inverse of [`weight_class`].
pub fn weight_trait(class: u16) -> f64 {
    match class {
        0..=150 => -0.8,
        151..=250 => -0.6,
        251..=350 => -0.4,
        351..=450 => 0.0,
        451..=550 => 0.23,
        551..=650 => 0.3,
        651..=750 => 0.4,
        751..=850 => 0.56,
        _ => 0.62,
    }
}

pub fn symbolic_traits(info: &FaceInfo) -> u32 {
    let mut traits = 0;
    if info.italic {
        traits |= ITALIC_TRAIT;
    }
    if info.weight >= 600 {
        traits |= BOLD_TRAIT;
    }
    if info.monospace {
        traits |= MONOSPACE_TRAIT;
    }
    traits
}

/// Language codes for the scripts a face has glyphs for, which is what Qt
/// decides a family's writing systems from.
pub fn languages(face: &LoadedFace) -> Vec<&'static str> {
    const SAMPLES: [(char, &str); 12] = [
        ('a', "en"),
        ('\u{0431}', "ru"),
        ('\u{03b1}', "el"),
        ('\u{05d0}', "he"),
        ('\u{0639}', "ar"),
        ('\u{0e01}', "th"),
        ('\u{0915}', "hi"),
        ('\u{4e2d}', "zh-Hans"),
        ('\u{7e41}', "zh-Hant"),
        ('\u{3042}', "ja"),
        ('\u{d55c}', "ko"),
        ('\u{1ea1}', "vi"),
    ];
    SAMPLES
        .iter()
        .filter(|(sample, _)| face.face().glyph_index(*sample).is_some())
        .map(|(_, language)| *language)
        .collect()
}

/// The attributes of `info` at `size`, as a font or descriptor.
pub fn face_attributes(info: &FaceInfo, size: f64) -> CFTypeRef {
    let mut owned = Vec::new();
    let mut own = |object: CFTypeRef| {
        owned.push(object);
        object
    };
    let weight = own(collections::new_number(Number::Float(weight_trait(
        info.weight,
    ))));
    let slant = own(collections::new_number(Number::Float(match info.italic {
        true => 0.07,
        false => 0.0,
    })));
    let symbolic = own(collections::new_number(Number::Integer(i64::from(
        symbolic_traits(info),
    ))));
    let traits = own(new_dictionary(&[
        (key("CTFontWeightTrait"), weight),
        (key("CTFontSlantTrait"), slant),
        (key("CTFontSymbolicTrait"), symbolic),
    ]));
    let postscript = match info.postscript.is_empty() {
        true => format!(
            "{}-{}",
            info.family.replace(' ', ""),
            info.style.replace(' ', "")
        ),
        false => info.postscript.clone(),
    };
    let language_list: Vec<CFTypeRef> = fonts::load(&info.path, info.index)
        .map(|face| languages(&face).into_iter().map(string).collect())
        .unwrap_or_default();
    let languages = own(collections::new_array(&language_list, type_policy(), false));
    for language in language_list {
        release(language);
    }
    let pairs = [
        (key("CTFontFamilyNameAttribute"), own(string(&info.family))),
        (key("CTFontStyleNameAttribute"), own(string(&info.style))),
        (key("CTFontNameAttribute"), own(string(&postscript))),
        (
            key("CTFontSizeAttribute"),
            own(collections::new_number(Number::Float(size))),
        ),
        (key("CTFontTraitsAttribute"), traits),
        (key("CTFontLanguagesAttribute"), languages),
        (
            key("CTFontURLAttribute"),
            own(corefoundation::url::new_file_url(&info.path, false)),
        ),
        (key(PATH_KEY), own(string(&info.path))),
        (
            key(INDEX_KEY),
            own(collections::new_number(Number::Integer(i64::from(
                info.index,
            )))),
        ),
    ];
    let font = new_dictionary(&pairs);
    for object in owned {
        release(object);
    }
    font
}

/// The face a descriptor names: its own file where it has been resolved, and
/// otherwise the best match for its family, weight and slant.
pub fn resolve(descriptor: CFTypeRef) -> Option<FaceInfo> {
    if let Some(path) = get_string(descriptor, PATH_KEY) {
        let index = get_number(descriptor, INDEX_KEY).unwrap_or(0.0) as u32;
        let family = get_string(descriptor, "CTFontFamilyNameAttribute").unwrap_or_default();
        let style = get_string(descriptor, "CTFontStyleNameAttribute").unwrap_or_default();
        if let Some(found) = fonts::all_faces()
            .into_iter()
            .find(|face| face.path == path && face.index == index)
        {
            return Some(found);
        }
        return Some(FaceInfo {
            family,
            style,
            postscript: String::new(),
            path,
            index,
            weight: 400,
            italic: false,
            monospace: false,
        });
    }
    let traits = get(descriptor, "CTFontTraitsAttribute");
    let weight = get_number(traits, "CTFontWeightTrait").map_or(400, weight_class);
    let symbolic = get_number(traits, "CTFontSymbolicTrait").unwrap_or(0.0) as u32;
    let slant = get_number(traits, "CTFontSlantTrait").unwrap_or(0.0);
    let weight = match symbolic & BOLD_TRAIT != 0 {
        true => weight.max(700),
        false => weight,
    };
    let italic = symbolic & ITALIC_TRAIT != 0 || slant > 0.0;
    let family = get_string(descriptor, "CTFontFamilyNameAttribute")
        .or_else(|| get_string(descriptor, "CTFontNameAttribute"))
        .unwrap_or_default();
    let style = get_string(descriptor, "CTFontStyleNameAttribute");
    if let Some(style) = style
        && let Some(face) = fonts::all_faces().into_iter().find(|face| {
            face.family.eq_ignore_ascii_case(&family) && face.style.eq_ignore_ascii_case(&style)
        }) {
            return Some(face);
        }
    fonts::resolve(&family, weight, italic)
}

/// A font for `descriptor` at `size`, keeping every attribute the descriptor
/// had that the face does not replace.
pub fn make_font(descriptor: CFTypeRef, size: f64) -> CFTypeRef {
    let size = match size > 0.0 {
        true => size,
        false => get_number(descriptor, "CTFontSizeAttribute")
            .filter(|size| *size > 0.0)
            .unwrap_or(DEFAULT_SIZE),
    };
    let Some(info) = resolve(descriptor) else {
        // No face at all: the descriptor with its size, so that the font
        // still answers its names.
        let size = collections::new_number(Number::Float(size));
        let overrides = new_dictionary(&[(key("CTFontSizeAttribute"), size)]);
        let font = merged_attributes(descriptor, overrides);
        release(overrides);
        release(size);
        return font;
    };
    let resolved = face_attributes(&info, size);
    let font = merged_attributes(descriptor, resolved);
    release(resolved);
    font
}

/// Copy `base`'s attributes with `overrides` layered on top.
pub fn merged_attributes(base: CFTypeRef, overrides: CFTypeRef) -> CFTypeRef {
    let mut pairs = Vec::new();
    for object in [base, overrides] {
        // SAFETY: null, or a dictionary a guest received from this runtime.
        if let Some(contents) = unsafe { collections::dictionary(object) } {
            pairs.extend(contents.entries.iter().copied());
        }
    }
    new_dictionary(&pairs)
}

pub fn loaded(font: CFTypeRef) -> Option<Arc<LoadedFace>> {
    let path = get_string(font, PATH_KEY)?;
    let index = get_number(font, INDEX_KEY).unwrap_or(0.0) as u32;
    fonts::load(&path, index)
}

pub fn font_size(font: CFTypeRef) -> f64 {
    get_number(font, "CTFontSizeAttribute").unwrap_or(0.0)
}

/// The face a font draws with and its size in points, for text AppKit draws
/// itself.
pub fn face_of(font: CFTypeRef) -> Option<(Arc<LoadedFace>, f64)> {
    if font.is_null() {
        return None;
    }
    Some((loaded(font)?, font_size(font)))
}

/// Points per font unit.
pub fn scale(font: CFTypeRef, face: &LoadedFace) -> f64 {
    font_size(font) / f64::from(face.face().units_per_em().max(1))
}

/// Write a font held in memory where the face database can read it, named by
/// its contents so the same font is written once. The path, or `None` when it
/// cannot be written.
pub fn spill_font(bytes: &[u8]) -> Option<String> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let directory = std::env::temp_dir().join("rosemary-fonts");
    let _ = std::fs::create_dir_all(&directory);
    let path = directory.join(format!("{:016x}.ttf", hasher.finish()));
    if !path.exists() && std::fs::write(&path, bytes).is_err() {
        return None;
    }
    Some(path.to_string_lossy().into_owned())
}

/// The file and face index a font draws with.
pub fn face_location(font: CFTypeRef) -> Option<(String, u32)> {
    let path = get_string(font, PATH_KEY)?;
    let index = get_number(font, INDEX_KEY).unwrap_or(0.0) as u32;
    Some((path, index))
}
