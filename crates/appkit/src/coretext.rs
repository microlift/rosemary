#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::Arc;

use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections::{self, Number};
use foundation::geometry::{NSPoint, NSRect, NSSize};

use crate::fonts::{self, FaceInfo, LoadedFace};

const ATTRIBUTE_GLOBALS: &[(&str, &str)] = &[
    ("_kCTFontCopyrightNameKey", "CTFontCopyrightNameKey"),
    // The attributed-string keys are shared with AppKit's where both
    // exist, which is how one attributed string serves both frameworks.
    ("_kCTFontAttributeName", "NSFont"),
    ("_kCTFontCascadeListAttribute", "CTFontCascadeListAttribute"),
    ("_kCTFontFamilyNameAttribute", "CTFontFamilyNameAttribute"),
    (
        "_kCTFontFeatureSelectorIdentifierKey",
        "CTFeatureSelectorIdentifier",
    ),
    (
        "_kCTFontFeatureSettingsAttribute",
        "CTFontFeatureSettingsAttribute",
    ),
    (
        "_kCTFontFeatureTypeIdentifierKey",
        "CTFeatureTypeIdentifier",
    ),
    ("_kCTFontLanguagesAttribute", "CTFontLanguagesAttribute"),
    ("_kCTFontNameAttribute", "CTFontNameAttribute"),
    ("_kCTFontPostScriptNameKey", "CTFontPostScriptNameKey"),
    ("_kCTFontSizeAttribute", "CTFontSizeAttribute"),
    ("_kCTFontSlantTrait", "CTFontSlantTrait"),
    ("_kCTFontStyleNameAttribute", "CTFontStyleNameAttribute"),
    ("_kCTFontSymbolicTrait", "CTFontSymbolicTrait"),
    ("_kCTFontTraitsAttribute", "CTFontTraitsAttribute"),
    ("_kCTFontURLAttribute", "CTFontURLAttribute"),
    ("_kCTFontUniqueNameKey", "CTFontUniqueNameKey"),
    ("_kCTFontWeightTrait", "CTFontWeightTrait"),
    ("_kCTForegroundColorAttributeName", "CTForegroundColor"),
    (
        "_kCTForegroundColorFromContextAttributeName",
        "CTForegroundColorFromContext",
    ),
    ("_kCTKernAttributeName", "NSKern"),
    ("_kCTLigatureAttributeName", "NSLigature"),
    ("_kCTParagraphStyleAttributeName", "NSParagraphStyle"),
    ("_kCTUnderlineStyleAttributeName", "NSUnderline"),
    ("_kCTUnderlineColorAttributeName", "CTUnderlineColor"),
    ("_kCTStrokeWidthAttributeName", "NSStrokeWidth"),
    ("_kCTStrokeColorAttributeName", "CTStrokeColor"),
    ("_kCTSuperscriptAttributeName", "NSSuperScript"),
    ("_kCTGlyphInfoAttributeName", "NSGlyphInfo"),
    ("_kCTLanguageAttributeName", "NSLanguage"),
    (
        "_kCTTypesetterOptionForcedEmbeddingLevel",
        "CTTypesetterOptionForcedEmbeddingLevel",
    ),
    ("_kCTVerticalFormsAttributeName", "CTVerticalForms"),
];

/// Where a resolved font's face lives. Private to this runtime, so a guest
/// copying attributes between descriptors carries them along unexamined.
const PATH_KEY: &str = "RosemaryFontFilePath";
const INDEX_KEY: &str = "RosemaryFontFileIndex";

/// A size CoreText uses for a font created with none.
const DEFAULT_SIZE: f64 = 12.0;

/// `kCTFontItalicTrait`, `kCTFontBoldTrait` and `kCTFontMonoSpaceTrait`.
const ITALIC_TRAIT: u32 = 1 << 0;
const BOLD_TRAIT: u32 = 1 << 1;
const MONOSPACE_TRAIT: u32 = 1 << 10;

fn key(text: &'static str) -> CFTypeRef {
    foundation::notification::name_object(text) as CFTypeRef
}

fn type_policy() -> Policy {
    Policy::from_type_callbacks()
}

fn new_dictionary(pairs: &[(CFTypeRef, CFTypeRef)]) -> CFTypeRef {
    collections::new_dictionary(pairs, Policy::from_key_callbacks(), type_policy(), false)
}

fn release(object: CFTypeRef) {
    if !object.is_null() {
        // SAFETY: a reference the caller owns.
        unsafe { corefoundation::api::CFRelease(object) };
    }
}

pub(crate) fn get(dictionary: CFTypeRef, name: &'static str) -> CFTypeRef {
    // SAFETY: null, or a dictionary a guest received from this runtime.
    match unsafe { collections::dictionary(dictionary) } {
        Some(_) => unsafe { corefoundation::api::CFDictionaryGetValue(dictionary, key(name)) },
        None => std::ptr::null(),
    }
}

fn get_string(dictionary: CFTypeRef, name: &'static str) -> Option<String> {
    let value = get(dictionary, name);
    if value.is_null() {
        return None;
    }
    // SAFETY: font attributes named as strings hold strings.
    let bytes = unsafe { foundation::string::bytes(value as _) };
    String::from_utf8(bytes).ok()
}

fn get_number(dictionary: CFTypeRef, name: &'static str) -> Option<f64> {
    let value = get(dictionary, name);
    // SAFETY: null or an object this runtime allocated.
    unsafe { collections::number(value) }.map(|number| number.as_f64())
}

fn string(text: &str) -> CFTypeRef {
    foundation::string::from_str(text) as CFTypeRef
}

/// CoreText's weight trait, from -1 to 1, as an OpenType weight class.
fn weight_class(weight: f64) -> u16 {
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
fn weight_trait(class: u16) -> f64 {
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

fn symbolic_traits(info: &FaceInfo) -> u32 {
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
fn languages(face: &LoadedFace) -> Vec<&'static str> {
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
pub(crate) fn face_attributes(info: &FaceInfo, size: f64) -> CFTypeRef {
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
fn resolve(descriptor: CFTypeRef) -> Option<FaceInfo> {
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
    if let Some(style) = style {
        if let Some(face) = fonts::all_faces().into_iter().find(|face| {
            face.family.eq_ignore_ascii_case(&family) && face.style.eq_ignore_ascii_case(&style)
        }) {
            return Some(face);
        }
    }
    fonts::resolve(&family, weight, italic)
}

/// A font for `descriptor` at `size`, keeping every attribute the descriptor
/// had that the face does not replace.
fn make_font(descriptor: CFTypeRef, size: f64) -> CFTypeRef {
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
fn merged_attributes(base: CFTypeRef, overrides: CFTypeRef) -> CFTypeRef {
    let mut pairs = Vec::new();
    for object in [base, overrides] {
        // SAFETY: null, or a dictionary a guest received from this runtime.
        if let Some(contents) = unsafe { collections::dictionary(object) } {
            pairs.extend(contents.entries.iter().copied());
        }
    }
    new_dictionary(&pairs)
}

pub(crate) fn loaded(font: CFTypeRef) -> Option<Arc<LoadedFace>> {
    let path = get_string(font, PATH_KEY)?;
    let index = get_number(font, INDEX_KEY).unwrap_or(0.0) as u32;
    fonts::load(&path, index)
}

fn font_size(font: CFTypeRef) -> f64 {
    get_number(font, "CTFontSizeAttribute").unwrap_or(0.0)
}

/// The face a font draws with and its size in points, for text AppKit draws
/// itself.
pub(crate) fn face_of(font: CFTypeRef) -> Option<(Arc<LoadedFace>, f64)> {
    if font.is_null() {
        return None;
    }
    Some((loaded(font)?, font_size(font)))
}

/// Points per font unit.
fn scale(font: CFTypeRef, face: &LoadedFace) -> f64 {
    font_size(font) / f64::from(face.face().units_per_em().max(1))
}

pub unsafe extern "C" fn CTFontCreateUIFontForLanguage(
    kind: u32,
    size: f64,
    _language: CFTypeRef,
) -> CFTypeRef {
    // `kCTFontUIFontUserFixedPitch` is 1; every other kind is proportional.
    let family = fonts::default_family(kind == 1).unwrap_or_else(|| "Sans".to_string());
    let name = string(&family);
    let descriptor = new_dictionary(&[(key("CTFontFamilyNameAttribute"), name)]);
    let font = make_font(descriptor, if size > 0.0 { size } else { 13.0 });
    release(descriptor);
    release(name);
    font
}

pub unsafe extern "C" fn CTFontCreateWithName(
    name: CFTypeRef,
    size: f64,
    _matrix: *const c_void,
) -> CFTypeRef {
    if name.is_null() {
        return std::ptr::null();
    }
    let descriptor = new_dictionary(&[
        (key("CTFontNameAttribute"), name),
        (key("CTFontFamilyNameAttribute"), name),
    ]);
    let font = make_font(descriptor, size);
    release(descriptor);
    font
}

pub unsafe extern "C" fn CTFontCopyFontDescriptor(font: CFTypeRef) -> CFTypeRef {
    if font.is_null() {
        return std::ptr::null();
    }
    unsafe { corefoundation::api::CFRetain(font) }
}

/// A font serves as its own graphics font.
pub unsafe extern "C" fn CTFontCopyGraphicsFont(
    font: CFTypeRef,
    attributes: *mut CFTypeRef,
) -> CFTypeRef {
    if font.is_null() {
        return std::ptr::null();
    }
    if !attributes.is_null() {
        // SAFETY: the caller supplied the documented optional output pointer.
        unsafe { *attributes = corefoundation::api::CFRetain(font) };
    }
    unsafe { corefoundation::api::CFRetain(font) }
}

fn copy_string_attribute(font: CFTypeRef, name: &'static str) -> CFTypeRef {
    let value = get(font, name);
    match value.is_null() {
        true => std::ptr::null(),
        // SAFETY: a live attribute value.
        false => unsafe { corefoundation::api::CFRetain(value) },
    }
}

pub unsafe extern "C" fn CTFontCopyName(font: CFTypeRef, name_key: CFTypeRef) -> CFTypeRef {
    let wanted = [
        ("CTFontFamilyNameKey", "CTFontFamilyNameAttribute"),
        ("CTFontSubFamilyNameKey", "CTFontStyleNameAttribute"),
        ("CTFontStyleNameKey", "CTFontStyleNameAttribute"),
        ("CTFontPostScriptNameKey", "CTFontNameAttribute"),
        ("CTFontUniqueNameKey", "CTFontNameAttribute"),
        ("CTFontFullNameKey", "CTFontNameAttribute"),
    ];
    for (name, attribute) in wanted {
        // SAFETY: both are strings, or `name_key` is null.
        if !name_key.is_null() && unsafe { corefoundation::api::CFEqual(name_key, key(name)) } != 0
        {
            return copy_string_attribute(font, attribute);
        }
    }
    std::ptr::null()
}

pub unsafe extern "C" fn CTFontCopyFamilyName(font: CFTypeRef) -> CFTypeRef {
    copy_string_attribute(font, "CTFontFamilyNameAttribute")
}

pub unsafe extern "C" fn CTFontCopyPostScriptName(font: CFTypeRef) -> CFTypeRef {
    copy_string_attribute(font, "CTFontNameAttribute")
}

pub unsafe extern "C" fn CTFontGetSymbolicTraits(font: CFTypeRef) -> u32 {
    let traits = get(font, "CTFontTraitsAttribute");
    get_number(traits, "CTFontSymbolicTrait").unwrap_or(0.0) as u32
}

pub unsafe extern "C" fn CTFontGetUnitsPerEm(font: CFTypeRef) -> u32 {
    loaded(font).map_or(1000, |face| u32::from(face.face().units_per_em()))
}

pub unsafe extern "C" fn CTFontGetSize(font: CFTypeRef) -> f64 {
    font_size(font)
}

fn metric(
    font: CFTypeRef,
    read: impl Fn(&owned_ttf_parser::Face<'_>) -> f64,
    fallback: f64,
) -> f64 {
    match loaded(font) {
        Some(face) => read(face.face()) * scale(font, &face),
        None => font_size(font) * fallback,
    }
}

pub unsafe extern "C" fn CTFontGetAscent(font: CFTypeRef) -> f64 {
    metric(font, |face| f64::from(face.ascender()), 0.8)
}

pub unsafe extern "C" fn CTFontGetDescent(font: CFTypeRef) -> f64 {
    metric(font, |face| -f64::from(face.descender()), 0.2)
}

pub unsafe extern "C" fn CTFontGetLeading(font: CFTypeRef) -> f64 {
    metric(font, |face| f64::from(face.line_gap()), 0.0)
}

pub unsafe extern "C" fn CTFontGetCapHeight(font: CFTypeRef) -> f64 {
    metric(
        font,
        |face| f64::from(face.capital_height().unwrap_or(face.ascender() * 7 / 10)),
        0.7,
    )
}

pub unsafe extern "C" fn CTFontGetXHeight(font: CFTypeRef) -> f64 {
    metric(
        font,
        |face| f64::from(face.x_height().unwrap_or(face.ascender() / 2)),
        0.5,
    )
}

pub unsafe extern "C" fn CTFontGetSlantAngle(font: CFTypeRef) -> f64 {
    loaded(font).map_or(0.0, |face| f64::from(face.face().italic_angle()))
}

pub unsafe extern "C" fn CTFontGetUnderlinePosition(font: CFTypeRef) -> f64 {
    metric(
        font,
        |face| f64::from(face.underline_metrics().map_or(-100, |m| m.position)),
        -0.1,
    )
}

pub unsafe extern "C" fn CTFontGetUnderlineThickness(font: CFTypeRef) -> f64 {
    metric(
        font,
        |face| f64::from(face.underline_metrics().map_or(50, |m| m.thickness)),
        0.05,
    )
}

fn rect(left: f64, bottom: f64, right: f64, top: f64) -> NSRect {
    NSRect {
        origin: NSPoint { x: left, y: bottom },
        size: NSSize {
            width: right - left,
            height: top - bottom,
        },
    }
}

pub unsafe extern "C" fn CTFontGetBoundingBox(font: CFTypeRef) -> NSRect {
    match loaded(font) {
        Some(face) => {
            let s = scale(font, &face);
            let b = face.face().global_bounding_box();
            rect(
                f64::from(b.x_min) * s,
                f64::from(b.y_min) * s,
                f64::from(b.x_max) * s,
                f64::from(b.y_max) * s,
            )
        }
        None => {
            let size = font_size(font);
            rect(0.0, size * -0.2, size * 0.6, size * 0.8)
        }
    }
}

pub unsafe extern "C" fn CTFontGetBoundingRectsForGlyphs(
    font: CFTypeRef,
    _orientation: u32,
    glyphs: *const u16,
    bounds: *mut NSRect,
    count: isize,
) -> NSRect {
    let count = count.max(0) as usize;
    let face = loaded(font);
    let mut union: Option<NSRect> = None;
    for at in 0..count {
        let glyph_rect = match (&face, glyphs.is_null()) {
            (Some(face), false) => {
                let s = scale(font, face);
                // SAFETY: the caller supplies `count` glyphs.
                let glyph = unsafe { *glyphs.add(at) };
                face.face()
                    .glyph_bounding_box(owned_ttf_parser::GlyphId(glyph))
                    .map_or(NSRect::default(), |b| {
                        rect(
                            f64::from(b.x_min) * s,
                            f64::from(b.y_min) * s,
                            f64::from(b.x_max) * s,
                            f64::from(b.y_max) * s,
                        )
                    })
            }
            _ => unsafe { CTFontGetBoundingBox(font) },
        };
        if !bounds.is_null() {
            // SAFETY: the caller supplies `count` writable rectangles.
            unsafe { bounds.add(at).write(glyph_rect) };
        }
        union = Some(match union {
            None => glyph_rect,
            Some(u) => rect(
                u.origin.x.min(glyph_rect.origin.x),
                u.origin.y.min(glyph_rect.origin.y),
                (u.origin.x + u.size.width).max(glyph_rect.origin.x + glyph_rect.size.width),
                (u.origin.y + u.size.height).max(glyph_rect.origin.y + glyph_rect.size.height),
            ),
        });
    }
    union.unwrap_or_default()
}

pub unsafe extern "C" fn CTFontGetAdvancesForGlyphs(
    font: CFTypeRef,
    _orientation: u32,
    glyphs: *const u16,
    advances: *mut NSSize,
    count: isize,
) -> f64 {
    let count = count.max(0) as usize;
    let face = loaded(font);
    let mut total = 0.0;
    for at in 0..count {
        let width = match (&face, glyphs.is_null()) {
            (Some(face), false) => {
                // SAFETY: the caller supplies `count` glyphs.
                let glyph = unsafe { *glyphs.add(at) };
                let units = face
                    .face()
                    .glyph_hor_advance(owned_ttf_parser::GlyphId(glyph))
                    .unwrap_or(0);
                f64::from(units) * scale(font, face)
            }
            _ => font_size(font) * 0.6,
        };
        if !advances.is_null() {
            // SAFETY: the caller supplies `count` writable advances.
            unsafe { advances.add(at).write(NSSize { width, height: 0.0 }) };
        }
        total += width;
    }
    total
}

/// Glyphs for UTF-16 characters: a surrogate pair's glyph goes at its first
/// unit and zero at its second, and the answer is whether every character
/// had one.
pub unsafe extern "C" fn CTFontGetGlyphsForCharacters(
    font: CFTypeRef,
    characters: *const u16,
    glyphs: *mut u16,
    count: isize,
) -> u8 {
    let Ok(count) = usize::try_from(count) else {
        return 0;
    };
    if count == 0 {
        return 1;
    }
    if characters.is_null() || glyphs.is_null() {
        return 0;
    }
    // SAFETY: the caller supplies `count` readable characters and writable glyphs.
    let (units, out) = unsafe {
        (
            std::slice::from_raw_parts(characters, count),
            std::slice::from_raw_parts_mut(glyphs, count),
        )
    };
    let Some(face) = loaded(font) else {
        out.copy_from_slice(units);
        return 1;
    };
    let mut complete = true;
    let mut at = 0;
    while at < count {
        let unit = units[at];
        let (character, width) = match (unit, units.get(at + 1)) {
            (0xd800..=0xdbff, Some(&low @ 0xdc00..=0xdfff)) => (
                char::from_u32(
                    0x10000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00),
                ),
                2,
            ),
            _ => (char::from_u32(u32::from(unit)), 1),
        };
        let glyph = character
            .and_then(|character| face.face().glyph_index(character))
            .map_or(0, |glyph| glyph.0);
        complete &= glyph != 0;
        out[at] = glyph;
        if width == 2 {
            out[at + 1] = 0;
        }
        at += width;
    }
    u8::from(complete)
}

/// Rasterize glyphs into a bitmap context with its fill color.
///
/// A glyph's outline is scaled to the font's size, placed at its position,
/// and mapped through the context's text matrix and CTM, which is the whole
/// of CoreGraphics' text rendering model for a fill.
pub unsafe extern "C" fn CTFontDrawGlyphs(
    font: CFTypeRef,
    glyphs: *const u16,
    positions: *const NSPoint,
    count: usize,
    context: CFTypeRef,
) {
    let Some(face) = loaded(font) else {
        return;
    };
    if glyphs.is_null() || positions.is_null() || count == 0 {
        return;
    }
    // SAFETY: the caller supplies `count` glyphs and positions.
    let (glyphs, positions) = unsafe {
        (
            std::slice::from_raw_parts(glyphs, count),
            std::slice::from_raw_parts(positions, count),
        )
    };
    let positions: Vec<(f64, f64)> = positions.iter().map(|p| (p.x, p.y)).collect();
    crate::quartz::draw_glyphs(context, &face, font_size(font), glyphs, &positions);
}

pub unsafe extern "C" fn CTFontCreatePathForGlyph(
    font: CFTypeRef,
    glyph: u16,
    transform: *const crate::cgcontext::CGAffineTransform,
) -> CFTypeRef {
    let Some(face) = loaded(font) else {
        return std::ptr::null();
    };
    let s = scale(font, &face);
    let to_font = crate::cgcontext::CGAffineTransform {
        a: s,
        d: s,
        ..crate::cgcontext::IDENTITY
    };
    let m = match transform.is_null() {
        true => to_font,
        // SAFETY: the caller's transform.
        false => to_font.then(unsafe { *transform }),
    };
    let mut outline = crate::cgpath::Elements::default();
    crate::cgpath::glyph_outline(face.face(), glyph, m, &mut outline);
    crate::cgpath::path_from(outline)
}

pub unsafe extern "C" fn CTFontDescriptorCopyAttribute(
    descriptor: CFTypeRef,
    attribute: CFTypeRef,
) -> CFTypeRef {
    if descriptor.is_null() || attribute.is_null() {
        return std::ptr::null();
    }
    // SAFETY: null, or a dictionary a guest received from this runtime.
    if unsafe { collections::dictionary(descriptor) }.is_none() {
        return std::ptr::null();
    }
    let value = unsafe { corefoundation::api::CFDictionaryGetValue(descriptor, attribute) };
    if value.is_null() {
        value
    } else {
        unsafe { corefoundation::api::CFRetain(value) }
    }
}

pub unsafe extern "C" fn CTFontCopyAttribute(font: CFTypeRef, attribute: CFTypeRef) -> CFTypeRef {
    unsafe { CTFontDescriptorCopyAttribute(font, attribute) }
}

pub unsafe extern "C" fn CTFontCopyTraits(font: CFTypeRef) -> CFTypeRef {
    let traits = get(font, "CTFontTraitsAttribute");
    match traits.is_null() {
        true => new_dictionary(&[]),
        // SAFETY: a live attribute value.
        false => unsafe { corefoundation::api::CFRetain(traits) },
    }
}

/// An unresolved descriptor is its attributes.
pub unsafe extern "C" fn CTFontDescriptorCreateWithAttributes(attributes: CFTypeRef) -> CFTypeRef {
    if attributes.is_null() {
        return std::ptr::null();
    }
    unsafe { corefoundation::api::CFRetain(attributes) }
}

pub unsafe extern "C" fn CTFontDescriptorCreateMatchingFontDescriptor(
    descriptor: CFTypeRef,
    _mandatory_attributes: CFTypeRef,
) -> CFTypeRef {
    if descriptor.is_null() {
        return std::ptr::null();
    }
    let info = resolve(descriptor);
    match info {
        Some(info) => {
            let size = get_number(descriptor, "CTFontSizeAttribute").unwrap_or(0.0);
            let resolved = face_attributes(&info, size);
            let merged = merged_attributes(descriptor, resolved);
            release(resolved);
            merged
        }
        None => unsafe { corefoundation::api::CFRetain(descriptor) },
    }
}

/// Every face of the descriptor's family, which is how Qt populates a family;
/// null for a family no font has.
pub unsafe extern "C" fn CTFontDescriptorCreateMatchingFontDescriptors(
    descriptor: CFTypeRef,
    _mandatory_attributes: CFTypeRef,
) -> CFTypeRef {
    if descriptor.is_null() {
        return std::ptr::null();
    }
    if get(descriptor, PATH_KEY).is_null() {
        if let Some(family) = get_string(descriptor, "CTFontFamilyNameAttribute") {
            let style = get_string(descriptor, "CTFontStyleNameAttribute");
            let faces: Vec<CFTypeRef> = fonts::all_faces()
                .iter()
                .filter(|face| face.family.eq_ignore_ascii_case(&family))
                .filter(|face| {
                    style
                        .as_ref()
                        .is_none_or(|style| face.style.eq_ignore_ascii_case(style))
                })
                .map(|face| face_attributes(face, 0.0))
                .collect();
            if faces.is_empty() {
                return std::ptr::null();
            }
            let array = collections::new_array(&faces, type_policy(), false);
            faces.into_iter().for_each(release);
            return array;
        }
    }
    let matching =
        unsafe { CTFontDescriptorCreateMatchingFontDescriptor(descriptor, std::ptr::null()) };
    let array = collections::new_array(&[matching], type_policy(), false);
    release(matching);
    array
}

pub unsafe extern "C" fn CTFontDescriptorCreateCopyWithAttributes(
    original: CFTypeRef,
    attributes: CFTypeRef,
) -> CFTypeRef {
    if original.is_null() {
        return std::ptr::null();
    }
    merged_attributes(original, attributes)
}

pub unsafe extern "C" fn CTFontDescriptorCreateWithNameAndSize(
    name: CFTypeRef,
    size: f64,
) -> CFTypeRef {
    if name.is_null() {
        return std::ptr::null();
    }
    let size = collections::new_number(Number::Float(size));
    let descriptor = new_dictionary(&[
        (key("CTFontNameAttribute"), name),
        (key("CTFontSizeAttribute"), size),
    ]);
    release(size);
    descriptor
}

pub unsafe extern "C" fn CTFontCreateWithFontDescriptor(
    descriptor: CFTypeRef,
    size: f64,
    _matrix: *const c_void,
) -> CFTypeRef {
    if descriptor.is_null() {
        return std::ptr::null();
    }
    make_font(descriptor, size)
}

/// `CTFontCreateCopyWithSymbolicTraits(CTFontRef, CGFloat size, const
/// CGAffineTransform *, CTFontSymbolicTraits value, CTFontSymbolicTraits
/// mask)`: the face of the same family with the masked traits changed, or
/// null when the family has none, as CoreText answers.
pub unsafe extern "C" fn CTFontCreateCopyWithSymbolicTraits(
    font: CFTypeRef,
    size: f64,
    _matrix: *const c_void,
    value: u32,
    mask: u32,
) -> CFTypeRef {
    if font.is_null() {
        return std::ptr::null();
    }
    let traits = get(font, "CTFontTraitsAttribute");
    let old = get_number(traits, "CTFontSymbolicTrait").unwrap_or(0.0) as u32;
    let wanted = (old & !mask) | (value & mask);
    let family = get_string(font, "CTFontFamilyNameAttribute").unwrap_or_default();
    let family = string(&family);
    let symbolic = collections::new_number(Number::Integer(i64::from(wanted)));
    let traits = new_dictionary(&[(key("CTFontSymbolicTrait"), symbolic)]);
    let descriptor = new_dictionary(&[
        (key("CTFontFamilyNameAttribute"), family),
        (key("CTFontTraitsAttribute"), traits),
    ]);
    let size = match size > 0.0 {
        true => size,
        false => font_size(font),
    };
    let found = resolve(descriptor).filter(|face| {
        (wanted & BOLD_TRAIT == 0 || face.weight >= 600)
            && (wanted & ITALIC_TRAIT == 0 || face.italic)
    });
    let made = match found {
        Some(_) => make_font(descriptor, size),
        None => std::ptr::null(),
    };
    for object in [descriptor, traits, symbolic, family] {
        release(object);
    }
    made
}

pub unsafe extern "C" fn CTFontCreateCopyWithAttributes(
    font: CFTypeRef,
    size: f64,
    _matrix: *const c_void,
    attributes: CFTypeRef,
) -> CFTypeRef {
    if font.is_null() {
        return std::ptr::null();
    }
    let merged = merged_attributes(font, attributes);
    let size = match size > 0.0 {
        true => size,
        false => font_size(font),
    };
    let result = make_font(merged, size);
    release(merged);
    result
}

/// A graphics font is the same attributes dictionary as its CTFont.
pub unsafe extern "C" fn CTFontCreateWithGraphicsFont(
    graphics_font: CFTypeRef,
    size: f64,
    _matrix: *const c_void,
    attributes: CFTypeRef,
) -> CFTypeRef {
    unsafe { CTFontCreateCopyWithAttributes(graphics_font, size, std::ptr::null(), attributes) }
}

/// kCTVersionNumber10_15.
pub unsafe extern "C" fn CTGetCoreTextVersion() -> u32 {
    0x000C_0000
}

/// The raw bytes of one of the face's tables, which Qt's HarfBuzz shapes with.
pub unsafe extern "C" fn CTFontCopyTable(font: CFTypeRef, table: u32, _options: u32) -> CFTypeRef {
    let Some(face) = loaded(font) else {
        return std::ptr::null();
    };
    let tag = owned_ttf_parser::Tag::from_bytes(&table.to_be_bytes());
    match face.face().raw_face().table(tag) {
        Some(bytes) => collections::new_data(bytes.to_vec()),
        None => std::ptr::null(),
    }
}

/// Fallback families after the default one, for characters it lacks.
pub unsafe extern "C" fn CTFontCopyDefaultCascadeListForLanguages(
    _font: CFTypeRef,
    _languages: CFTypeRef,
) -> CFTypeRef {
    let mut families: Vec<String> = fonts::default_family(false).into_iter().collect();
    for wanted in [
        "Noto Sans CJK SC",
        "Noto Sans",
        "DejaVu Sans",
        "Noto Color Emoji",
    ] {
        if fonts::all_faces().iter().any(|face| face.family == wanted)
            && !families.iter().any(|family| family == wanted)
        {
            families.push(wanted.to_string());
        }
    }
    let descriptors: Vec<CFTypeRef> = families
        .iter()
        .map(|family| {
            let name = string(family);
            let descriptor = new_dictionary(&[(key("CTFontFamilyNameAttribute"), name)]);
            release(name);
            descriptor
        })
        .collect();
    let array = collections::new_array(&descriptors, type_policy(), false);
    descriptors.into_iter().for_each(release);
    array
}

pub unsafe extern "C" fn CTFontManagerCopyAvailableFontFamilyNames() -> CFTypeRef {
    let mut families: Vec<String> = fonts::all_faces()
        .into_iter()
        .map(|face| face.family)
        .collect();
    families.sort();
    families.dedup();
    let names: Vec<CFTypeRef> = families.iter().map(|family| string(family)).collect();
    let array = collections::new_array(&names, type_policy(), false);
    names.into_iter().for_each(release);
    array
}

fn descriptors_for(faces: &[FaceInfo]) -> CFTypeRef {
    if faces.is_empty() {
        return std::ptr::null();
    }
    let descriptors: Vec<CFTypeRef> = faces
        .iter()
        .map(|face| face_attributes(face, 0.0))
        .collect();
    let array = collections::new_array(&descriptors, type_policy(), false);
    descriptors.into_iter().for_each(release);
    array
}

/// Register the faces in a font file and describe them.
pub unsafe extern "C" fn CTFontManagerCreateFontDescriptorsFromURL(url: CFTypeRef) -> CFTypeRef {
    let Some(path) = (unsafe { corefoundation::url::as_url(url) }).and_then(|url| url.file_path())
    else {
        return std::ptr::null();
    };
    // Translated once, here: `register_file` keeps the path in each `FaceInfo`
    // and `fonts::load` reads it again later.
    let path = prefix::ns::host_path(&path);
    descriptors_for(&fonts::register_file(&path.to_string_lossy()))
}

/// Write a font held in memory where the face database can read it, named by
/// its contents so the same font is written once. The path, or `None` when it
/// cannot be written.
pub(crate) fn spill_font(bytes: &[u8]) -> Option<String> {
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
pub(crate) fn face_location(font: CFTypeRef) -> Option<(String, u32)> {
    let path = get_string(font, PATH_KEY)?;
    let index = get_number(font, INDEX_KEY).unwrap_or(0.0) as u32;
    Some((path, index))
}

/// As [`CTFontManagerCreateFontDescriptorsFromURL`], for font bytes in memory,
/// which are written to a file so that they load like every other face.
pub unsafe extern "C" fn CTFontManagerCreateFontDescriptorFromData(data: CFTypeRef) -> CFTypeRef {
    // SAFETY: null or an object this runtime allocated.
    let Some(bytes) = (unsafe { collections::data(data) }) else {
        return std::ptr::null();
    };
    let Some(path) = spill_font(bytes) else {
        return std::ptr::null();
    };
    let faces = fonts::register_file(&path);
    let Some(first) = faces.first() else {
        return std::ptr::null();
    };
    face_attributes(first, 0.0)
}

exports::exports! {
    functions {
        "CTFontCreateUIFontForLanguage" => CTFontCreateUIFontForLanguage,
        "CTFontCreateWithName" => CTFontCreateWithName,
        "CTFontCopyFontDescriptor" => CTFontCopyFontDescriptor,
        "CTFontCopyGraphicsFont" => CTFontCopyGraphicsFont,
        "CTFontCopyName" => CTFontCopyName,
        "CTFontCopyFamilyName" => CTFontCopyFamilyName,
        "CTFontCopyPostScriptName" => CTFontCopyPostScriptName,
        "CTFontGetSymbolicTraits" => CTFontGetSymbolicTraits,
        "CTFontGetUnitsPerEm" => CTFontGetUnitsPerEm,
        "CTFontGetSize" => CTFontGetSize,
        "CTFontGetAscent" => CTFontGetAscent,
        "CTFontGetDescent" => CTFontGetDescent,
        "CTFontGetLeading" => CTFontGetLeading,
        "CTFontGetCapHeight" => CTFontGetCapHeight,
        "CTFontGetXHeight" => CTFontGetXHeight,
        "CTFontGetSlantAngle" => CTFontGetSlantAngle,
        "CTFontGetUnderlinePosition" => CTFontGetUnderlinePosition,
        "CTFontGetUnderlineThickness" => CTFontGetUnderlineThickness,
        "CTFontGetBoundingBox" => CTFontGetBoundingBox,
        "CTFontGetBoundingRectsForGlyphs" => CTFontGetBoundingRectsForGlyphs,
        "CTFontGetAdvancesForGlyphs" => CTFontGetAdvancesForGlyphs,
        "CTFontGetGlyphsForCharacters" => CTFontGetGlyphsForCharacters,
        "CTFontDrawGlyphs" => CTFontDrawGlyphs,
        "CTFontCreatePathForGlyph" => CTFontCreatePathForGlyph,
        "CTFontDescriptorCopyAttribute" => CTFontDescriptorCopyAttribute,
        "CTFontCopyAttribute" => CTFontCopyAttribute,
        "CTFontCopyTraits" => CTFontCopyTraits,
        "CTFontDescriptorCreateWithAttributes" => CTFontDescriptorCreateWithAttributes,
        "CTFontDescriptorCreateMatchingFontDescriptor" => CTFontDescriptorCreateMatchingFontDescriptor,
        "CTFontCreateWithFontDescriptor" => CTFontCreateWithFontDescriptor,
        "CTFontCopyTable" => CTFontCopyTable,
        "CTFontCopyDefaultCascadeListForLanguages" => CTFontCopyDefaultCascadeListForLanguages,
        "CTFontManagerCopyAvailableFontFamilyNames" => CTFontManagerCopyAvailableFontFamilyNames,
        "CTFontManagerCreateFontDescriptorsFromURL" => CTFontManagerCreateFontDescriptorsFromURL,
        "CTFontManagerCreateFontDescriptorFromData" => CTFontManagerCreateFontDescriptorFromData,
        "CTFontDescriptorCreateCopyWithAttributes" => CTFontDescriptorCreateCopyWithAttributes,
        "CTFontDescriptorCreateWithNameAndSize" => CTFontDescriptorCreateWithNameAndSize,
        "CTFontDescriptorCreateMatchingFontDescriptors" => CTFontDescriptorCreateMatchingFontDescriptors,
        "CTFontCreateCopyWithAttributes" => CTFontCreateCopyWithAttributes,
        "CTFontCreateCopyWithSymbolicTraits" => CTFontCreateCopyWithSymbolicTraits,
        "CTFontCreateWithGraphicsFont" => CTFontCreateWithGraphicsFont,
        "CTGetCoreTextVersion" => CTGetCoreTextVersion,
    }
}

pub fn data_symbols() -> Vec<(String, u64)> {
    corefoundation::base::string_globals(ATTRIBUTE_GLOBALS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host's default sans-serif font, or `None` on a host with no
    /// fontconfig, where these tests have nothing to read.
    fn default_font(size: f64) -> Option<CFTypeRef> {
        crate::bootstrap();
        fonts::default_family(false)?;
        // SAFETY: a null language is documented.
        let font = unsafe { CTFontCreateUIFontForLanguage(0, size, std::ptr::null()) };
        loaded(font).is_some().then_some(font)
    }

    #[test]
    fn weight_traits_round_trip_through_weight_classes() {
        for class in [100, 300, 400, 500, 600, 700, 900] {
            assert_eq!(weight_class(weight_trait(class)), class);
        }
    }

    #[test]
    fn a_font_reads_its_metrics_from_its_face() {
        let Some(font) = default_font(20.0) else {
            return;
        };
        // SAFETY: a live font.
        unsafe {
            assert_eq!(CTFontGetSize(font), 20.0);
            let ascent = CTFontGetAscent(font);
            let descent = CTFontGetDescent(font);
            assert!(ascent > 10.0 && ascent < 30.0, "ascent {ascent}");
            assert!(descent > 0.0 && descent < 10.0, "descent {descent}");
            let head = CTFontCopyTable(font, u32::from_be_bytes(*b"head"), 0);
            assert!(!head.is_null());
            release(head);
            release(font);
        }
    }

    #[test]
    fn glyphs_for_characters_come_from_the_cmap_and_draw_into_a_context() {
        let Some(font) = default_font(24.0) else {
            return;
        };
        let characters = [b'H' as u16];
        let mut glyph = [0u16];
        let mut pixels = vec![0u8; 32 * 32];
        // SAFETY: the arrays hold the counts passed; the context wraps `pixels`.
        unsafe {
            assert_eq!(
                CTFontGetGlyphsForCharacters(font, characters.as_ptr(), glyph.as_mut_ptr(), 1),
                1
            );
            assert_ne!(glyph[0], u16::from(b'H'));
            let mut advance = NSSize::default();
            CTFontGetAdvancesForGlyphs(font, 0, glyph.as_ptr(), &mut advance, 1);
            assert!(advance.width > 5.0);
            let context = crate::cgcontext::CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                32,
                32,
                8,
                32,
                std::ptr::null(),
                7,
            );
            crate::cgcontext::CGContextSetGrayFillColor(context, 1.0, 1.0);
            let origin = NSPoint { x: 4.0, y: 6.0 };
            CTFontDrawGlyphs(font, glyph.as_ptr(), &origin, 1, context);
            release(context);
            release(font);
        }
        let covered = pixels.iter().filter(|&&value| value > 128).count();
        assert!(covered > 20, "only {covered} pixels covered");
    }

    #[test]
    fn descriptor_copy_overrides_original_attributes() {
        crate::bootstrap();
        let name = string("Fira Mono");
        // SAFETY: all arguments are live Core Foundation objects.
        unsafe {
            let original = CTFontDescriptorCreateWithNameAndSize(name, 12.0);
            let size = collections::new_number(Number::Float(20.0));
            let overrides = new_dictionary(&[(key("CTFontSizeAttribute"), size)]);
            let copy = CTFontDescriptorCreateCopyWithAttributes(original, overrides);
            assert_eq!(CTFontGetSize(copy), 20.0);
            assert_eq!(CTFontGetSize(original), 12.0);
            for object in [copy, overrides, size, original, name] {
                release(object);
            }
        }
    }
}
