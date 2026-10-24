//! CoreText: fonts and descriptors.

#![allow(
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals
)]

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use corefoundation::collections::{self, Number};
use coregraphics::cgfont::{face_for, reference_of};
use coregraphics::font_attributes::{
    PATH_KEY, face_attributes, font_size, get, get_number, get_string, key, loaded, make_font,
    merged_attributes, new_dictionary, release, resolve, scale, spill_font, string, type_policy,
};
use coregraphics::fonts::{self, FaceInfo};
use coregraphics::{CGPoint, CGRect, CGSize};

const ATTRIBUTE_GLOBALS: &[(&str, &str)] = &[
    ("_kCTFontCopyrightNameKey", "CTFontCopyrightNameKey"),
    ("_kCTFontAttributeName", "CTFontAttributeName"),
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
    (
        "_kCTForegroundColorAttributeName",
        "CTForegroundColorAttributeName",
    ),
    ("_kCTKernAttributeName", "CTKernAttributeName"),
    ("_kCTLanguageAttributeName", "CTLanguageAttributeName"),
    (
        "_kCTTypesetterOptionForcedEmbeddingLevel",
        "CTTypesetterOptionForcedEmbeddingLevel",
    ),
    ("_kCTVerticalFormsAttributeName", "CTVerticalForms"),
];

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

/// `CTFontRef CTFontCreateWithPlatformFont(ATSFontRef, CGFloat, const
/// CGAffineTransform *, CTFontDescriptorRef)`
pub unsafe extern "C" fn CTFontCreateWithPlatformFont(
    font: u32,
    size: f64,
    _matrix: *const c_void,
    _attributes: CFTypeRef,
) -> CFTypeRef {
    let Some(face) = face_for(font) else {
        return std::ptr::null();
    };
    face_attributes(&face, size)
}

/// `ATSFontRef CTFontGetPlatformFont(CTFontRef, CTFontDescriptorRef *)`
pub unsafe extern "C" fn CTFontGetPlatformFont(font: CFTypeRef, attributes: *mut CFTypeRef) -> u32 {
    if !attributes.is_null() {
        // SAFETY: writable, per the caller. The descriptor is optional.
        unsafe { attributes.write(std::ptr::null()) };
    }
    reference_of(font)
}

/// `CFIndex CTFontGetGlyphCount(CTFontRef)`
pub unsafe extern "C" fn CTFontGetGlyphCount(font: CFTypeRef) -> isize {
    loaded(font).map_or(0, |face| face.face().number_of_glyphs() as isize)
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

fn rect(left: f64, bottom: f64, right: f64, top: f64) -> CGRect {
    CGRect {
        origin: CGPoint { x: left, y: bottom },
        size: CGSize {
            width: right - left,
            height: top - bottom,
        },
    }
}

pub unsafe extern "C" fn CTFontGetBoundingBox(font: CFTypeRef) -> CGRect {
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
    bounds: *mut CGRect,
    count: isize,
) -> CGRect {
    let count = count.max(0) as usize;
    let face = loaded(font);
    let mut union: Option<CGRect> = None;
    for at in 0..count {
        let glyph_rect = match (&face, glyphs.is_null()) {
            (Some(face), false) => {
                let s = scale(font, face);
                // SAFETY: the caller supplies `count` glyphs.
                let glyph = unsafe { *glyphs.add(at) };
                face.face()
                    .glyph_bounding_box(owned_ttf_parser::GlyphId(glyph))
                    .map_or(CGRect::default(), |b| {
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
    advances: *mut CGSize,
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
            unsafe { advances.add(at).write(CGSize { width, height: 0.0 }) };
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
    positions: *const CGPoint,
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
    coregraphics::quartz::draw_glyphs(context, &face, font_size(font), glyphs, &positions);
}

pub unsafe extern "C" fn CTFontCreatePathForGlyph(
    font: CFTypeRef,
    glyph: u16,
    transform: *const coregraphics::cgcontext::CGAffineTransform,
) -> CFTypeRef {
    let Some(face) = loaded(font) else {
        return std::ptr::null();
    };
    let s = scale(font, &face);
    let to_font = coregraphics::cgcontext::CGAffineTransform {
        a: s,
        d: s,
        ..coregraphics::CGAffineTransformIdentity
    };
    let m = match transform.is_null() {
        true => to_font,
        // SAFETY: the caller's transform.
        false => to_font.then(unsafe { *transform }),
    };
    let mut outline = coregraphics::cgpath::Elements::default();
    coregraphics::cgpath::glyph_outline(face.face(), glyph, m, &mut outline);
    coregraphics::cgpath::path_from(outline).cast()
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
    if get(descriptor, PATH_KEY).is_null()
        && let Some(family) = get_string(descriptor, "CTFontFamilyNameAttribute") {
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
        "CTFontCreateWithPlatformFont" => CTFontCreateWithPlatformFont,
        "CTFontGetPlatformFont" => CTFontGetPlatformFont,
        "CTFontGetGlyphCount" => CTFontGetGlyphCount,
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
    use coregraphics::font_attributes::{weight_class, weight_trait};

    /// The host's default sans-serif font, or `None` on a host with no
    /// fontconfig, where these tests have nothing to read.
    fn default_font(size: f64) -> Option<CFTypeRef> {
        coregraphics::bootstrap();
        fonts::default_family(false)?;
        // SAFETY: a null language is documented.
        let font = unsafe { CTFontCreateUIFontForLanguage(0, size, std::ptr::null()) };
        loaded(font).is_some().then_some(font)
    }

    #[test]
    fn the_framework_publishes_its_font_calls() {
        let names = names();
        for name in [
            "CTFontCreateWithName",
            "CTFontDrawGlyphs",
            "CTFontCreatePathForGlyph",
        ] {
            assert!(names.contains(&name), "{name}");
        }
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
            let mut advance = CGSize::default();
            CTFontGetAdvancesForGlyphs(font, 0, glyph.as_ptr(), &mut advance, 1);
            assert!(advance.width > 5.0);
            let context = coregraphics::cgbitmap::CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                32,
                32,
                8,
                32,
                std::ptr::null(),
                7,
            );
            coregraphics::cgcontext::CGContextSetGrayFillColor(context, 1.0, 1.0);
            let origin = CGPoint { x: 4.0, y: 6.0 };
            CTFontDrawGlyphs(font, glyph.as_ptr(), &origin, 1, context);
            release(context);
            release(font);
        }
        let covered = pixels.iter().filter(|&&value| value > 128).count();
        assert!(covered > 20, "only {covered} pixels covered");
    }

    #[test]
    fn descriptor_copy_overrides_original_attributes() {
        coregraphics::bootstrap();
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
