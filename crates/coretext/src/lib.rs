#![allow(
    non_snake_case,
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref
)]

use std::ffi::c_void;
use std::sync::Arc;

use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections::{self, Number};
use foundation::geometry::{NSPoint, NSRect, NSSize};
use objc::object::Id;

use chrome::fonts::{self, FaceInfo, LoadedFace};

const ATTRIBUTE_GLOBALS: &[(&str, &str)] = &[
    ("_kCTFontCopyrightNameKey", "CTFontCopyrightNameKey"),
    // The attributed-string keys are shared with AppKit's where both
    // exist, which is how one attributed string serves both frameworks.
    ("_kCTFontAttributeName", "NSFont"),
    ("_kCTFontCascadeListAttribute", "CTFontCascadeListAttribute"),
    ("_kCTFontFamilyNameAttribute", "CTFontFamilyNameAttribute"),
    ("_kCTFontFamilyNameKey", "CTFontFamilyNameKey"),
    ("_kCTFontFormatAttribute", "CTFontFormatAttribute"),
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
    ("_kCTFontStyleNameKey", "CTFontStyleNameKey"),
    ("_kCTFontSubFamilyNameKey", "CTFontSubFamilyNameKey"),
    ("_kCTFontSymbolicTrait", "CTFontSymbolicTrait"),
    ("_kCTFontTraitsAttribute", "CTFontTraitsAttribute"),
    ("_kCTFontURLAttribute", "CTFontURLAttribute"),
    ("_kCTFontVariationAttribute", "NSCTFontVariationAttribute"),
    (
        "_kCTFontVariationAxisIdentifierKey",
        VARIATION_AXIS_IDENTIFIER,
    ),
    (
        "_kCTFontVariationAxisMinimumValueKey",
        VARIATION_AXIS_MINIMUM,
    ),
    (
        "_kCTFontVariationAxisMaximumValueKey",
        VARIATION_AXIS_MAXIMUM,
    ),
    (
        "_kCTFontVariationAxisDefaultValueKey",
        VARIATION_AXIS_DEFAULT,
    ),
    ("_kCTFontVariationAxisNameKey", VARIATION_AXIS_NAME),
    ("_kCTFontVariationAxisHiddenKey", VARIATION_AXIS_HIDDEN),
    ("_kCTFontUniqueNameKey", "CTFontUniqueNameKey"),
    ("_kCTFontWeightTrait", "CTFontWeightTrait"),
    ("_kCTFontWidthTrait", "CTFontWidthTrait"),
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
const VARIATION_AXIS_IDENTIFIER: &str = "NSCTVariationAxisIdentifier";
const VARIATION_AXIS_MINIMUM: &str = "NSCTVariationAxisMinimumValue";
const VARIATION_AXIS_MAXIMUM: &str = "NSCTVariationAxisMaximumValue";
const VARIATION_AXIS_DEFAULT: &str = "NSCTVariationAxisDefaultValue";
const VARIATION_AXIS_NAME: &str = "NSCTVariationAxisName";
const VARIATION_AXIS_HIDDEN: &str = "NSCTVariationAxisHidden";

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

pub fn get(dictionary: CFTypeRef, name: &'static str) -> CFTypeRef {
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

/// `kCTFontWidthTrait`: OS/2 `usWidthClass` (1-9, 5 normal), linearly mapped
/// to CoreText's -1..1 trait.
///
/// FIXME (ACCURACY): Apple does not document its own mapping as linear; this
/// keeps a condensed or expanded face on the correct side of zero without
/// claiming to reproduce whatever curve it actually uses.
fn width_trait(class: u16) -> f64 {
    ((f64::from(class) - 5.0) / 4.0).clamp(-1.0, 1.0)
}

/// `kCTFontFormatAttribute`: every face here is sfnt-wrapped, so the only
/// question is which outline table it carries - `kCTFontFormatOpenTypePostScript`
/// for CFF outlines, `kCTFontFormatOpenTypeTrueType` otherwise.
fn font_format(face: &owned_ttf_parser::Face<'_>) -> i64 {
    const OPEN_TYPE_POSTSCRIPT: i64 = 1;
    const OPEN_TYPE_TRUE_TYPE: i64 = 2;
    match face.tables().cff.is_some() {
        true => OPEN_TYPE_POSTSCRIPT,
        false => OPEN_TYPE_TRUE_TYPE,
    }
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
    // Loaded once: both the languages list and the width/format attributes
    // below read it.
    let loaded_face = fonts::load(&info.path, info.index);
    let width = own(collections::new_number(Number::Float(
        loaded_face
            .as_ref()
            .map_or(0.0, |face| width_trait(face.face().width().to_number())),
    )));
    let traits = own(new_dictionary(&[
        (key("CTFontWeightTrait"), weight),
        (key("CTFontSlantTrait"), slant),
        (key("CTFontWidthTrait"), width),
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
    let language_list: Vec<CFTypeRef> = loaded_face
        .as_ref()
        .map(|face| languages(face).into_iter().map(string).collect())
        .unwrap_or_default();
    let languages = own(collections::new_array(&language_list, type_policy(), false));
    for language in language_list {
        release(language);
    }
    let format = own(collections::new_number(Number::Integer(
        loaded_face
            .as_ref()
            .map_or(2, |face| font_format(face.face())),
    )));
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
        (key("CTFontFormatAttribute"), format),
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
    if let Some(style) = style
        && let Some(face) = fonts::all_faces().into_iter().find(|face| {
            face.family.eq_ignore_ascii_case(&family) && face.style.eq_ignore_ascii_case(&style)
        })
    {
        return Some(face);
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
            pairs.extend(contents.entries().iter().copied());
        }
    }
    new_dictionary(&pairs)
}

/// `NSFontDescriptorSystemDesign` values. Self-spelled, like every other
/// private constant this crate publishes - see `appkit::KEYS`.
pub const DESIGN_DEFAULT: &str = "NSFontDescriptorSystemDesignDefault";
pub const DESIGN_SERIF: &str = "NSFontDescriptorSystemDesignSerif";
pub const DESIGN_MONOSPACED: &str = "NSFontDescriptorSystemDesignMonospaced";
pub const DESIGN_ROUNDED: &str = "NSFontDescriptorSystemDesignRounded";

/// A face whose family reads as serif, for [`DESIGN_SERIF`].
///
/// FIXME (ACCURACY): fontconfig's fallback defaults have a generic monospace
/// family (used by [`fonts::default_family`]) but no generic serif one, so
/// this guesses from common serif family names instead of asking the host's
/// configuration. A host with only odd serif family names installed answers
/// nothing, like `Default`.
fn serif_family() -> Option<String> {
    fonts::all_faces()
        .into_iter()
        .map(|face| face.family)
        .find(|family| family.to_ascii_lowercase().contains("serif"))
}

/// `-[NSFontDescriptor fontDescriptorWithDesign:]`.
///
/// A copy of `descriptor` whose family is swapped for one matching `design`,
/// clearing the resolved file a `-fontDescriptor` already carries so the new
/// family is what resolves next time, not the old one. Every other
/// attribute - size, weight, slant - passes through untouched.
///
/// `Default`, and any design this font backend has no substitute family for,
/// answer the receiver unchanged: Apple documents that for `Default`, and a
/// descriptor is a more usable answer than nil for the rest.
pub fn font_descriptor_with_design(descriptor: CFTypeRef, design: &str) -> CFTypeRef {
    if descriptor.is_null() {
        return std::ptr::null();
    }
    let family = match design {
        DESIGN_MONOSPACED => fonts::default_family(true),
        DESIGN_SERIF => serif_family(),
        DESIGN_DEFAULT => None,
        // FIXME (ACCURACY): no face here is marked as a rounded design, so
        // this cannot pick one out from its family's regular sibling; it
        // degrades to the receiver's own design.
        DESIGN_ROUNDED => None,
        // Not a design this type defines.
        _ => return std::ptr::null(),
    };
    let Some(family) = family else {
        return unsafe { corefoundation::api::CFRetain(descriptor) };
    };
    let value = string(&family);
    let overrides = new_dictionary(&[
        (key("CTFontFamilyNameAttribute"), value),
        (key(PATH_KEY), std::ptr::null()),
        (key(INDEX_KEY), std::ptr::null()),
    ]);
    release(value);
    let merged = merged_attributes(descriptor, overrides);
    release(overrides);
    merged
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

/// The attribute `CTFontCopyName` and `CTFontCopyLocalizedName` read for each
/// name key they accept.
const NAME_KEYS: &[(&str, &str)] = &[
    ("CTFontFamilyNameKey", "CTFontFamilyNameAttribute"),
    ("CTFontSubFamilyNameKey", "CTFontStyleNameAttribute"),
    ("CTFontStyleNameKey", "CTFontStyleNameAttribute"),
    ("CTFontPostScriptNameKey", "CTFontNameAttribute"),
    ("CTFontUniqueNameKey", "CTFontNameAttribute"),
    ("CTFontFullNameKey", "CTFontNameAttribute"),
];

fn name_for_key(font: CFTypeRef, name_key: CFTypeRef) -> CFTypeRef {
    if name_key.is_null() {
        return std::ptr::null();
    }
    for (name, attribute) in NAME_KEYS {
        // SAFETY: both are strings.
        if unsafe { corefoundation::api::CFEqual(name_key, key(name)) } != 0 {
            return copy_string_attribute(font, attribute);
        }
    }
    std::ptr::null()
}

pub unsafe extern "C" fn CTFontCopyName(font: CFTypeRef, name_key: CFTypeRef) -> CFTypeRef {
    name_for_key(font, name_key)
}

/// This runtime has one language, so the lookup never actually varies by it.
pub unsafe extern "C" fn CTFontCopyLocalizedName(
    font: CFTypeRef,
    name_key: CFTypeRef,
    actual_language: *mut CFTypeRef,
) -> CFTypeRef {
    if !actual_language.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { actual_language.write(string("en")) };
    }
    name_for_key(font, name_key)
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
        && let Some(family) = get_string(descriptor, "CTFontFamilyNameAttribute")
    {
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

/// One dictionary per axis of the face's `fvar` table, or null for a face that
/// is not a variable font.
pub unsafe extern "C" fn CTFontCopyVariationAxes(font: CFTypeRef) -> CFTypeRef {
    let Some(face) = loaded(font) else {
        return std::ptr::null();
    };
    let face = face.face();
    let axes: Vec<CFTypeRef> = face
        .variation_axes()
        .into_iter()
        .map(|axis| {
            let name = face
                .names()
                .into_iter()
                .filter(|name| name.name_id == axis.name_id && name.is_unicode())
                .find_map(|name| name.to_string())
                .unwrap_or_default();
            let values = [
                (
                    VARIATION_AXIS_IDENTIFIER,
                    collections::new_number(Number::Integer(i64::from(axis.tag.0))),
                ),
                (
                    VARIATION_AXIS_MINIMUM,
                    collections::new_number(Number::Float(f64::from(axis.min_value))),
                ),
                (
                    VARIATION_AXIS_MAXIMUM,
                    collections::new_number(Number::Float(f64::from(axis.max_value))),
                ),
                (
                    VARIATION_AXIS_DEFAULT,
                    collections::new_number(Number::Float(f64::from(axis.def_value))),
                ),
                (VARIATION_AXIS_NAME, string(&name)),
                // Retained so the one release below covers every value.
                (VARIATION_AXIS_HIDDEN, unsafe {
                    corefoundation::api::CFRetain(collections::boolean(axis.hidden))
                }),
            ];
            let pairs: Vec<_> = values
                .iter()
                .map(|&(name, value)| (key(name), value))
                .collect();
            let axis = new_dictionary(&pairs);
            values.into_iter().for_each(|(_, value)| release(value));
            axis
        })
        .collect();
    if axes.is_empty() {
        return std::ptr::null();
    }
    let array = collections::new_array(&axes, type_policy(), false);
    axes.into_iter().for_each(release);
    array
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

/// `CFArrayRef CTFontCopyAvailableTables(CTFontRef, CTFontTableOptions)`: every
/// sfnt table tag the face's directory lists. `options`
/// (`kCTFontTableOptionExcludeSynthetic`) names nothing here, since this
/// runtime never synthesizes a table that is not already in the file.
pub unsafe extern "C" fn CTFontCopyAvailableTables(font: CFTypeRef, _options: u32) -> CFTypeRef {
    let Some(face) = loaded(font) else {
        return std::ptr::null();
    };
    let tags: Vec<CFTypeRef> = face
        .face()
        .raw_face()
        .table_records
        .into_iter()
        .map(|record| collections::new_number(Number::Integer(i64::from(record.tag.0))))
        .collect();
    if tags.is_empty() {
        return std::ptr::null();
    }
    let array = collections::new_array(&tags, type_policy(), false);
    tags.into_iter().for_each(release);
    array
}

/// `CFCharacterSetRef CTFontCopyCharacterSet(CTFontRef)`: every code point a
/// Unicode `cmap` subtable of the face maps, collapsed into ranges.
pub unsafe extern "C" fn CTFontCopyCharacterSet(font: CFTypeRef) -> CFTypeRef {
    let mut points = Vec::new();
    if let Some(face) = loaded(font)
        && let Some(cmap) = face.face().tables().cmap
    {
        for subtable in cmap.subtables {
            if subtable.is_unicode() {
                subtable.codepoints(|point| points.push(point));
            }
        }
    }
    points.sort_unstable();
    points.dedup();
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for point in points {
        match ranges.last_mut() {
            Some((_, high)) if point == *high + 1 => *high = point,
            _ => ranges.push((point, point)),
        }
    }
    foundation::characterset::create_with_ranges(ranges) as CFTypeRef
}

/// `CFStringRef CTFontCopyFullName(CTFontRef)`: `family style`, or the bare
/// family for a regular face - as [`crate::cgfont`]'s ATS name lookup already
/// formats one.
pub unsafe extern "C" fn CTFontCopyFullName(font: CFTypeRef) -> CFTypeRef {
    let Some(family) = get_string(font, "CTFontFamilyNameAttribute") else {
        return std::ptr::null();
    };
    let style = get_string(font, "CTFontStyleNameAttribute").unwrap_or_default();
    let full = match style.is_empty() || style.eq_ignore_ascii_case("regular") {
        true => family,
        false => format!("{family} {style}"),
    };
    string(&full)
}

/// `CFDictionaryRef CTFontCopyVariation(CTFontRef)`: every variation axis'
/// current value, keyed by its tag number as Core Text's own dictionary is.
pub unsafe extern "C" fn CTFontCopyVariation(font: CFTypeRef) -> CFTypeRef {
    let Some((path, index)) = face_location(font) else {
        return std::ptr::null();
    };
    let coordinates = fonts::current_variation(&path, index);
    if coordinates.is_empty() {
        return std::ptr::null();
    }
    let mut owned = Vec::new();
    let pairs: Vec<(CFTypeRef, CFTypeRef)> = coordinates
        .into_iter()
        .map(|(tag, value)| {
            let tag = collections::new_number(Number::Integer(i64::from(tag)));
            let value = collections::new_number(Number::Float(f64::from(value)));
            owned.push(tag);
            owned.push(value);
            (tag, value)
        })
        .collect();
    let dict =
        collections::new_dictionary(&pairs, Policy::from_key_callbacks(), type_policy(), false);
    owned.into_iter().for_each(release);
    dict
}

/// `CTFontRef CTFontCreateCopyWithSymbolicTraits(CTFontRef, CGFloat, const
/// CGAffineTransform *, CTFontSymbolicTraits, CTFontSymbolicTraits)`: a face
/// of the same family whose bold, italic and monospace traits exactly match
/// `(current & !mask) | (value & mask)`, or null when the family has none.
pub unsafe extern "C" fn CTFontCreateCopyWithSymbolicTraits(
    font: CFTypeRef,
    size: f64,
    _matrix: *const c_void,
    sym_trait_value: u32,
    sym_trait_mask: u32,
) -> CFTypeRef {
    if font.is_null() {
        return std::ptr::null();
    }
    let Some(family) = get_string(font, "CTFontFamilyNameAttribute") else {
        return std::ptr::null();
    };
    let current = unsafe { CTFontGetSymbolicTraits(font) };
    let wanted = (current & !sym_trait_mask) | (sym_trait_value & sym_trait_mask);
    let (italic, bold, monospace) = (
        wanted & ITALIC_TRAIT != 0,
        wanted & BOLD_TRAIT != 0,
        wanted & MONOSPACE_TRAIT != 0,
    );
    let candidate = fonts::all_faces()
        .into_iter()
        .filter(|face| face.family.eq_ignore_ascii_case(&family))
        .min_by_key(|face| {
            let face_bold = face.weight >= 600;
            u32::from(face.italic != italic) * 1000
                + u32::from(face_bold != bold) * 100
                + u32::from(face.monospace != monospace) * 10
                + face.weight.abs_diff(if bold { 700 } else { 400 }) as u32
        });
    let Some(info) = candidate else {
        return std::ptr::null();
    };
    // A "closest" face elsewhere in the family is not what the caller asked
    // for - only an exact match on the traits that were actually requested.
    if info.italic != italic || (info.weight >= 600) != bold {
        return std::ptr::null();
    }
    let size = if size > 0.0 { size } else { font_size(font) };
    let resolved = face_attributes(&info, size);
    let result = merged_attributes(font, resolved);
    release(resolved);
    result
}

/// `CTFontRef CTFontCreateForString(CTFontRef, CFStringRef, CFRange)`: `font`
/// itself when its face already covers every character in the range, else
/// the first family of its default cascade list whose face covers them all.
///
/// FIXME (ACCURACY): this is coverage-only cascading, not Core Text's
/// per-character script and coverage ranking.
pub unsafe extern "C" fn CTFontCreateForString(
    font: CFTypeRef,
    string: CFTypeRef,
    range_location: i64,
    range_length: i64,
) -> CFTypeRef {
    if font.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a live string, or null.
    let Some(all_units) = (unsafe { corefoundation::string::as_string(string) })
        .map(corefoundation::string::CFString::units)
    else {
        return unsafe { corefoundation::api::CFRetain(font) };
    };
    let Ok(start) = usize::try_from(range_location) else {
        return unsafe { corefoundation::api::CFRetain(font) };
    };
    let end = if range_length == 0 {
        all_units.len()
    } else {
        match usize::try_from(range_length)
            .ok()
            .and_then(|n| start.checked_add(n))
        {
            Some(end) if end <= all_units.len() => end,
            _ => return unsafe { corefoundation::api::CFRetain(font) },
        }
    };
    let Some(slice) = all_units.get(start..end) else {
        return unsafe { corefoundation::api::CFRetain(font) };
    };
    let covers = |face: &LoadedFace| {
        char::decode_utf16(slice.iter().copied())
            .all(|c| c.is_ok_and(|c| face.face().glyph_index(c).is_some()))
    };
    if slice.is_empty() || loaded(font).is_some_and(|face| covers(&face)) {
        return unsafe { corefoundation::api::CFRetain(font) };
    }
    let size = font_size(font);
    let cascade = unsafe { CTFontCopyDefaultCascadeListForLanguages(font, std::ptr::null()) };
    // SAFETY: null or an array this runtime built just above.
    let descriptors = unsafe { collections::array(cascade) }
        .map(|array| array.values.clone())
        .unwrap_or_default();
    release(cascade);
    for descriptor in descriptors {
        if let Some(info) = resolve(descriptor)
            && fonts::load(&info.path, info.index).is_some_and(|face| covers(&face))
        {
            return make_font(descriptor, size);
        }
    }
    unsafe { corefoundation::api::CFRetain(font) }
}

/// `CTFontCollectionRef CTFontCollectionCreateFromAvailableFonts(CFDictionaryRef)`.
///
/// FIXME (ACCURACY): `options` (`kCTFontCollectionRemoveDuplicatesOption` and
/// friends) is ignored; every registered face is always in the collection.
///
/// The collection this hands back *is* the descriptor array -
/// `CTFontCollectionCreateMatchingFontDescriptors` just retains and returns
/// it, the same "the object answers for itself" shortcut
/// `CTFontCopyGraphicsFont` uses for a font's graphics font.
pub unsafe extern "C" fn CTFontCollectionCreateFromAvailableFonts(
    _options: CFTypeRef,
) -> CFTypeRef {
    descriptors_for(&fonts::all_faces())
}

pub unsafe extern "C" fn CTFontCollectionCreateMatchingFontDescriptors(
    collection: CFTypeRef,
) -> CFTypeRef {
    if collection.is_null() {
        return std::ptr::null();
    }
    unsafe { corefoundation::api::CFRetain(collection) }
}

/// `CFArrayRef CTFontManagerCopyAvailablePostScriptNames(void)`.
pub unsafe extern "C" fn CTFontManagerCopyAvailablePostScriptNames() -> CFTypeRef {
    let mut names: Vec<String> = fonts::all_faces()
        .into_iter()
        .map(|face| match face.postscript.is_empty() {
            true => format!(
                "{}-{}",
                face.family.replace(' ', ""),
                face.style.replace(' ', "")
            ),
            false => face.postscript,
        })
        .collect();
    names.sort();
    names.dedup();
    let strings: Vec<CFTypeRef> = names.iter().map(|name| string(name)).collect();
    let array = collections::new_array(&strings, type_policy(), false);
    strings.into_iter().for_each(release);
    array
}

/// `Boolean CTFontManagerRegisterFontsForURL(CFURLRef, CTFontManagerScope,
/// CFErrorRef *)`: as [`CTFontManagerCreateFontDescriptorsFromURL`], answering
/// whether the file had a face to register. `scope` names nothing here, since
/// every registration already lasts the process.
pub unsafe extern "C" fn CTFontManagerRegisterFontsForURL(
    font_url: CFTypeRef,
    _scope: u32,
    error: *mut CFTypeRef,
) -> u8 {
    if !error.is_null() {
        // SAFETY: writable, per the caller; this runtime never fails to parse
        // a font file it can read.
        unsafe { error.write(std::ptr::null()) };
    }
    let Some(path) =
        (unsafe { corefoundation::url::as_url(font_url) }).and_then(|url| url.file_path())
    else {
        return 0;
    };
    let path = prefix::ns::host_path(&path);
    u8::from(!fonts::register_file(&path.to_string_lossy()).is_empty())
}

/// `Boolean CTFontManagerUnregisterFontsForURL(CFURLRef, CTFontManagerScope,
/// CFErrorRef *)`.
///
/// FIXME (ACCURACY): as `ATSFontDeactivate` already notes, the face database
/// has no way to forget a file, so a registered font stays discoverable.
/// Accepted rather than refused, so a caller's unregister call does not fail
/// for a reason that has nothing to do with its own arguments.
pub unsafe extern "C" fn CTFontManagerUnregisterFontsForURL(
    font_url: CFTypeRef,
    _scope: u32,
    error: *mut CFTypeRef,
) -> u8 {
    if !error.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { error.write(std::ptr::null()) };
    }
    u8::from(!font_url.is_null())
}

// Shaping: `CTRun`, `CTLine` and `CTTypesetter`.
//
// None of the three is toll-free bridged to anything this runtime already
// represents as a dictionary - a run also carries parallel glyph, position
// and advance arrays no dictionary shape fits - so each is its own
// Objective-C class, registered in `install`, the way `CGPath` and
// `CGContext` already are in `crate::cgpath` and `crate::cgcontext`.
//
// FIXME (ACCURACY): shaping is one glyph per UTF-16 code unit straight out of
// the face's `cmap`, in the order the text already has. There is no
// ligature, kerning, bidi or script-specific shaping, and no justification.

#[repr(C)]
struct Typesetter {
    isa: *const c_void,
    data: *mut TypesetterData,
}

/// A typesetter: the text, and each run's length and attributes dictionary
/// (retained out of the attributed string's own, which may be edited or
/// freed after this is made).
struct TypesetterData {
    units: Vec<u16>,
    runs: Vec<(usize, CFTypeRef)>,
}

#[repr(C)]
struct Line {
    isa: *const c_void,
    data: *mut LineData,
}

struct LineData {
    /// A retained array of retained `__CTRun`s - what `CTLineGetGlyphRuns`
    /// hands back directly, as a "Get" function must.
    runs: CFTypeRef,
}

#[repr(C)]
struct Run {
    isa: *const c_void,
    data: *mut RunData,
}

struct RunData {
    glyphs: Vec<u16>,
    positions: Vec<NSPoint>,
    advances: Vec<NSSize>,
    string_indices: Vec<isize>,
    range: (isize, isize),
    /// Retained; released on dealloc.
    attributes: CFTypeRef,
}

fn new_typesetter(data: TypesetterData) -> CFTypeRef {
    let class = objc::class::lookup(c"__CTTypesetter").expect("installed during bootstrap");
    // SAFETY: `Typesetter` is the registered class layout.
    let object: *mut Typesetter = unsafe { corefoundation::base::allocate(class) };
    // SAFETY: a fresh allocation.
    unsafe { (*object).data = Box::into_raw(Box::new(data)) };
    object as CFTypeRef
}

fn typesetter_data<'a>(typesetter: CFTypeRef) -> Option<&'a TypesetterData> {
    let class = objc::class::lookup(c"__CTTypesetter")?;
    if typesetter.is_null() || !unsafe { corefoundation::base::is_kind_of(typesetter, class) } {
        return None;
    }
    // SAFETY: a `__CTTypesetter`, created with data.
    unsafe { (*(typesetter as *mut Typesetter)).data.as_ref() }
}

unsafe extern "C" fn typesetter_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__CTTypesetter`, whose data came from
    // `Box::into_raw` and whose runs' attributes this object retained.
    unsafe {
        let object = receiver as *mut Typesetter;
        if !(*object).data.is_null() {
            let data = Box::from_raw((*object).data);
            for (_, attributes) in data.runs {
                release(attributes);
            }
            (*object).data = std::ptr::null_mut();
        }
        objc::arc::finalize(receiver);
    }
}

fn new_line(data: LineData) -> CFTypeRef {
    let class = objc::class::lookup(c"__CTLine").expect("installed during bootstrap");
    // SAFETY: `Line` is the registered class layout.
    let object: *mut Line = unsafe { corefoundation::base::allocate(class) };
    // SAFETY: a fresh allocation.
    unsafe { (*object).data = Box::into_raw(Box::new(data)) };
    object as CFTypeRef
}

fn line_data<'a>(line: CFTypeRef) -> Option<&'a LineData> {
    let class = objc::class::lookup(c"__CTLine")?;
    if line.is_null() || !unsafe { corefoundation::base::is_kind_of(line, class) } {
        return None;
    }
    // SAFETY: a `__CTLine`, created with data.
    unsafe { (*(line as *mut Line)).data.as_ref() }
}

unsafe extern "C" fn line_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__CTLine`, whose data came from `Box::into_raw`.
    unsafe {
        let object = receiver as *mut Line;
        if !(*object).data.is_null() {
            let data = Box::from_raw((*object).data);
            release(data.runs);
            (*object).data = std::ptr::null_mut();
        }
        objc::arc::finalize(receiver);
    }
}

fn new_run(data: RunData) -> CFTypeRef {
    let class = objc::class::lookup(c"__CTRun").expect("installed during bootstrap");
    // SAFETY: `Run` is the registered class layout.
    let object: *mut Run = unsafe { corefoundation::base::allocate(class) };
    // SAFETY: a fresh allocation.
    unsafe { (*object).data = Box::into_raw(Box::new(data)) };
    object as CFTypeRef
}

fn run_data<'a>(run: CFTypeRef) -> Option<&'a RunData> {
    let class = objc::class::lookup(c"__CTRun")?;
    if run.is_null() || !unsafe { corefoundation::base::is_kind_of(run, class) } {
        return None;
    }
    // SAFETY: a `__CTRun`, created with data.
    unsafe { (*(run as *mut Run)).data.as_ref() }
}

unsafe extern "C" fn run_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__CTRun`, whose data came from `Box::into_raw`
    // and whose attributes this object retained.
    unsafe {
        let object = receiver as *mut Run;
        if !(*object).data.is_null() {
            let data = Box::from_raw((*object).data);
            release(data.attributes);
            (*object).data = std::ptr::null_mut();
        }
        objc::arc::finalize(receiver);
    }
}

/// The attributes dictionary of the run starting at character `index`, or
/// null past the end.
fn attrs_at(data: &TypesetterData, index: usize) -> CFTypeRef {
    let mut at = 0;
    for &(length, attributes) in &data.runs {
        if index < at + length {
            return attributes;
        }
        at += length;
    }
    std::ptr::null()
}

/// Each attribute run's slice of `units[start..end]`, as a UTF-16 offset
/// range paired with that run's attributes dictionary.
fn attr_ranges_in(
    data: &TypesetterData,
    start: usize,
    end: usize,
) -> Vec<(std::ops::Range<usize>, CFTypeRef)> {
    let mut out = Vec::new();
    let mut at = 0;
    for &(length, attributes) in &data.runs {
        let (run_start, run_end) = (at, at + length);
        at = run_end;
        let (lo, hi) = (run_start.max(start), run_end.min(end));
        if lo < hi {
            out.push((lo..hi, attributes));
        }
    }
    out
}

/// The font a run's attributes name under `kCTFontAttributeName`, retained -
/// or Core Text's own default UI font when the run has none.
fn font_for_attributes(attributes: CFTypeRef) -> CFTypeRef {
    let value = get(attributes, "CTFontAttributeName");
    if !value.is_null() {
        // SAFETY: a live attribute value.
        return unsafe { corefoundation::api::CFRetain(value) };
    }
    // SAFETY: a null language is documented.
    unsafe { CTFontCreateUIFontForLanguage(2, DEFAULT_SIZE, std::ptr::null()) }
}

/// Shapes `units` (absolute offset `base`) into one `__CTRun`, advancing
/// `pen` by the run's total width.
fn shape_run(units: &[u16], base: usize, attributes: CFTypeRef, pen: &mut f64) -> CFTypeRef {
    let font = font_for_attributes(attributes);
    let count = units.len();
    let mut glyphs = vec![0u16; count];
    // SAFETY: `glyphs` has `count` writable slots for `count` characters.
    unsafe {
        CTFontGetGlyphsForCharacters(font, units.as_ptr(), glyphs.as_mut_ptr(), count as isize)
    };
    let mut advances = vec![NSSize::default(); count];
    // SAFETY: as above.
    unsafe {
        CTFontGetAdvancesForGlyphs(
            font,
            0,
            glyphs.as_ptr(),
            advances.as_mut_ptr(),
            count as isize,
        )
    };
    let mut positions = Vec::with_capacity(count);
    let mut string_indices = Vec::with_capacity(count);
    for (at, advance) in advances.iter().enumerate() {
        positions.push(NSPoint { x: *pen, y: 0.0 });
        string_indices.push((base + at) as isize);
        *pen += advance.width;
    }
    release(font);
    let attributes = match attributes.is_null() {
        true => new_dictionary(&[]),
        // SAFETY: a live attributes dictionary borrowed from the typesetter.
        false => unsafe { corefoundation::api::CFRetain(attributes) },
    };
    new_run(RunData {
        glyphs,
        positions,
        advances,
        string_indices,
        range: (base as isize, count as isize),
        attributes,
    })
}

/// `CTTypesetterRef CTTypesetterCreateWithAttributedString(CFAttributedStringRef)`.
pub unsafe extern "C" fn CTTypesetterCreateWithAttributedString(string: CFTypeRef) -> CFTypeRef {
    install();
    if string.is_null() {
        return std::ptr::null();
    }
    // SAFETY: an NSAttributedString, or null, which the caller supplies - Core
    // Foundation's `CFAttributedString` is toll-free bridged to it.
    let Some((units, runs)) = (unsafe { foundation::attributed_string::runs(string as Id) }) else {
        return std::ptr::null();
    };
    let runs = runs
        .into_iter()
        .map(|(length, attributes)| {
            // SAFETY: a live attributes dictionary borrowed from the string.
            (length, unsafe {
                corefoundation::api::CFRetain(attributes as CFTypeRef)
            })
        })
        .collect();
    new_typesetter(TypesetterData { units, runs })
}

/// `CTLineRef CTTypesetterCreateLine(CTTypesetterRef, CFRange)`.
pub unsafe extern "C" fn CTTypesetterCreateLine(
    typesetter: CFTypeRef,
    range_location: i64,
    range_length: i64,
) -> CFTypeRef {
    let Some(data) = typesetter_data(typesetter) else {
        return std::ptr::null();
    };
    let total = data.units.len();
    let Ok(start) = usize::try_from(range_location) else {
        return std::ptr::null();
    };
    if start > total {
        return std::ptr::null();
    }
    let end = if range_length == 0 {
        total
    } else {
        match usize::try_from(range_length)
            .ok()
            .and_then(|n| start.checked_add(n))
        {
            Some(end) if end <= total => end,
            _ => return std::ptr::null(),
        }
    };
    let mut pen = 0.0;
    let runs: Vec<CFTypeRef> = attr_ranges_in(data, start, end)
        .into_iter()
        .map(|(range, attributes)| {
            shape_run(
                &data.units[range.clone()],
                range.start,
                attributes,
                &mut pen,
            )
        })
        .collect();
    let array = collections::new_array(&runs, type_policy(), false);
    runs.into_iter().for_each(release);
    new_line(LineData { runs: array })
}

/// `CFIndex CTTypesetterSuggestLineBreak(CTTypesetterRef, CFIndex, double)`: a
/// character count from `startIndex`, breaking at the last space or tab
/// before the requested width would be exceeded, or hard-breaking after the
/// first character that alone exceeds it - the line Core Text builds always
/// gains at least one character.
///
/// FIXME (ACCURACY): break opportunities are ASCII space and tab, and hard
/// breaks are `\n`, `\r`, U+2028 and U+2029 - not the Unicode line-breaking
/// algorithm (UAX #14) or hyphenation Core Text actually consults.
pub unsafe extern "C" fn CTTypesetterSuggestLineBreak(
    typesetter: CFTypeRef,
    start_index: isize,
    width: f64,
) -> isize {
    let Some(data) = typesetter_data(typesetter) else {
        return 0;
    };
    let total = data.units.len();
    let Ok(start) = usize::try_from(start_index) else {
        return 0;
    };
    if start >= total {
        return 0;
    }
    let mut pen = 0.0;
    let mut last_break: Option<usize> = None;
    let mut at = start;
    while at < total {
        let unit = data.units[at];
        if matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029) {
            return (at + 1 - start) as isize;
        }
        let font = font_for_attributes(attrs_at(data, at));
        let mut glyph = [0u16];
        // SAFETY: one character, one glyph slot.
        unsafe { CTFontGetGlyphsForCharacters(font, &unit, glyph.as_mut_ptr(), 1) };
        let mut advance = NSSize::default();
        // SAFETY: as above.
        unsafe { CTFontGetAdvancesForGlyphs(font, 0, glyph.as_ptr(), &mut advance, 1) };
        release(font);
        if pen > 0.0 && pen + advance.width > width {
            return (last_break.unwrap_or(at).max(start + 1) - start) as isize;
        }
        pen += advance.width;
        if matches!(unit, 0x20 | 0x09) {
            last_break = Some(at + 1);
        }
        at += 1;
    }
    (total - start) as isize
}

/// `CFArrayRef CTLineGetGlyphRuns(CTLineRef)`.
pub unsafe extern "C" fn CTLineGetGlyphRuns(line: CFTypeRef) -> CFTypeRef {
    line_data(line).map_or(std::ptr::null(), |data| data.runs)
}

pub unsafe extern "C" fn CTRunGetGlyphCount(run: CFTypeRef) -> isize {
    run_data(run).map_or(0, |data| data.glyphs.len() as isize)
}

/// Copies `data[location..location+length]` into `out`, or the rest of
/// `data` from `location` when `length` is zero, as every `CTRunGet*`
/// accessor's `CFRange` is documented.
fn copy_range<T: Copy>(data: &[T], location: isize, length: isize, out: *mut T) {
    let Ok(start) = usize::try_from(location) else {
        return;
    };
    if start > data.len() || out.is_null() {
        return;
    }
    let end = if length == 0 {
        data.len()
    } else {
        match usize::try_from(length)
            .ok()
            .and_then(|n| start.checked_add(n))
        {
            Some(end) if end <= data.len() => end,
            _ => return,
        }
    };
    // SAFETY: the caller supplies a buffer of at least `end - start` elements.
    unsafe { std::ptr::copy_nonoverlapping(data[start..end].as_ptr(), out, end - start) };
}

pub unsafe extern "C" fn CTRunGetGlyphs(
    run: CFTypeRef,
    range_location: isize,
    range_length: isize,
    buffer: *mut u16,
) {
    if let Some(data) = run_data(run) {
        copy_range(&data.glyphs, range_location, range_length, buffer);
    }
}

pub unsafe extern "C" fn CTRunGetPositions(
    run: CFTypeRef,
    range_location: isize,
    range_length: isize,
    buffer: *mut NSPoint,
) {
    if let Some(data) = run_data(run) {
        copy_range(&data.positions, range_location, range_length, buffer);
    }
}

pub unsafe extern "C" fn CTRunGetAdvances(
    run: CFTypeRef,
    range_location: isize,
    range_length: isize,
    buffer: *mut NSSize,
) {
    if let Some(data) = run_data(run) {
        copy_range(&data.advances, range_location, range_length, buffer);
    }
}

pub unsafe extern "C" fn CTRunGetStringIndices(
    run: CFTypeRef,
    range_location: isize,
    range_length: isize,
    buffer: *mut isize,
) {
    if let Some(data) = run_data(run) {
        copy_range(&data.string_indices, range_location, range_length, buffer);
    }
}

/// `CFDictionaryRef CTRunGetAttributes(CTRunRef)`.
pub unsafe extern "C" fn CTRunGetAttributes(run: CFTypeRef) -> CFTypeRef {
    run_data(run).map_or(std::ptr::null(), |data| data.attributes)
}

/// `CFRange CTRunGetStringRange(CTRunRef)`: both fields in an integer
/// register pair under SysV AMD64 - see `AGGREGATE` in
/// `crate::abi::table::REFUSED`, which is why this has no entry in
/// `SIGNATURES`. Called directly by the native backend, with no marshaller in
/// the way.
#[repr(C)]
pub struct StringRange {
    pub location: isize,
    pub length: isize,
}

pub unsafe extern "C" fn CTRunGetStringRange(run: CFTypeRef) -> StringRange {
    run_data(run).map_or(
        StringRange {
            location: 0,
            length: 0,
        },
        |data| StringRange {
            location: data.range.0,
            length: data.range.1,
        },
    )
}

pub fn install() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let run = corefoundation::base::register(c"__CTRun", None, size_of::<Run>(), true);
        run.replace_method(
            objc::sel::get("dealloc"),
            run_dealloc as *const c_void,
            c"v16@0:8",
        );
        let line = corefoundation::base::register(c"__CTLine", None, size_of::<Line>(), true);
        line.replace_method(
            objc::sel::get("dealloc"),
            line_dealloc as *const c_void,
            c"v16@0:8",
        );
        let typesetter =
            corefoundation::base::register(c"__CTTypesetter", None, size_of::<Typesetter>(), true);
        typesetter.replace_method(
            objc::sel::get("dealloc"),
            typesetter_dealloc as *const c_void,
            c"v16@0:8",
        );
    });
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
        "CTFontDescriptorCopyAttribute" => CTFontDescriptorCopyAttribute,
        "CTFontCopyAttribute" => CTFontCopyAttribute,
        "CTFontCopyTraits" => CTFontCopyTraits,
        "CTFontDescriptorCreateWithAttributes" => CTFontDescriptorCreateWithAttributes,
        "CTFontDescriptorCreateMatchingFontDescriptor" => CTFontDescriptorCreateMatchingFontDescriptor,
        "CTFontCreateWithFontDescriptor" => CTFontCreateWithFontDescriptor,
        "CTFontCopyTable" => CTFontCopyTable,
        "CTFontCopyVariationAxes" => CTFontCopyVariationAxes,
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
        "CTFontCopyAvailableTables" => CTFontCopyAvailableTables,
        "CTFontCopyCharacterSet" => CTFontCopyCharacterSet,
        "CTFontCopyFullName" => CTFontCopyFullName,
        "CTFontCopyLocalizedName" => CTFontCopyLocalizedName,
        "CTFontCopyVariation" => CTFontCopyVariation,
        "CTFontCreateForString" => CTFontCreateForString,
        "CTFontCollectionCreateFromAvailableFonts" => CTFontCollectionCreateFromAvailableFonts,
        "CTFontCollectionCreateMatchingFontDescriptors" => CTFontCollectionCreateMatchingFontDescriptors,
        "CTFontManagerCopyAvailablePostScriptNames" => CTFontManagerCopyAvailablePostScriptNames,
        "CTFontManagerRegisterFontsForURL" => CTFontManagerRegisterFontsForURL,
        "CTFontManagerUnregisterFontsForURL" => CTFontManagerUnregisterFontsForURL,
        "CTTypesetterCreateWithAttributedString" => CTTypesetterCreateWithAttributedString,
        "CTTypesetterCreateLine" => CTTypesetterCreateLine,
        "CTTypesetterSuggestLineBreak" => CTTypesetterSuggestLineBreak,
        "CTLineGetGlyphRuns" => CTLineGetGlyphRuns,
        "CTRunGetGlyphCount" => CTRunGetGlyphCount,
        "CTRunGetGlyphs" => CTRunGetGlyphs,
        "CTRunGetPositions" => CTRunGetPositions,
        "CTRunGetAdvances" => CTRunGetAdvances,
        "CTRunGetStringIndices" => CTRunGetStringIndices,
        "CTRunGetAttributes" => CTRunGetAttributes,
        "CTRunGetStringRange" => CTRunGetStringRange,
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
        foundation::bootstrap();
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
    fn descriptor_copy_overrides_original_attributes() {
        foundation::bootstrap();
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

    #[test]
    fn width_trait_is_zero_at_the_normal_width_class() {
        assert_eq!(width_trait(5), 0.0);
        assert!(width_trait(1) < 0.0);
        assert!(width_trait(9) > 0.0);
    }

    #[test]
    fn full_name_starts_with_the_family_name() {
        let Some(font) = default_font(12.0) else {
            return;
        };
        // SAFETY: a live font.
        unsafe {
            let family = get_string(font, "CTFontFamilyNameAttribute").expect("a family");
            let full = CTFontCopyFullName(font);
            assert!(!full.is_null());
            let text = String::from_utf8_lossy(&foundation::string::bytes(full as _)).into_owned();
            assert!(
                text.starts_with(&family),
                "{text:?} does not start with {family:?}"
            );
            release(full);
            release(font);
        }
    }

    #[test]
    fn character_set_contains_the_fonts_known_glyphs() {
        let Some(font) = default_font(12.0) else {
            return;
        };
        // SAFETY: a live font.
        unsafe {
            let set = CTFontCopyCharacterSet(font);
            assert!(!set.is_null());
            assert_eq!(
                foundation::characterset::CFCharacterSetIsCharacterMember(
                    set as Id,
                    u16::from(b'a')
                ),
                1
            );
            release(set);
            release(font);
        }
    }

    #[test]
    fn copy_with_symbolic_traits_keeps_the_same_face_when_traits_are_unchanged() {
        let Some(font) = default_font(16.0) else {
            return;
        };
        // SAFETY: a live font.
        unsafe {
            let traits = CTFontGetSymbolicTraits(font);
            // A zero mask asks to change nothing, so the family's own face
            // must still match.
            let copy = CTFontCreateCopyWithSymbolicTraits(font, 0.0, std::ptr::null(), traits, 0);
            assert!(!copy.is_null());
            assert_eq!(CTFontGetSymbolicTraits(copy), traits);
            release(copy);
            release(font);
        }
    }

    #[test]
    fn create_for_string_returns_the_same_font_when_it_already_covers_the_text() {
        let Some(font) = default_font(16.0) else {
            return;
        };
        // SAFETY: a live font and string.
        unsafe {
            let text = string("Hi");
            let result = CTFontCreateForString(font, text, 0, 0);
            assert_eq!(result, font, "a covering font should not be replaced");
            release(text);
            release(result);
            release(font);
        }
    }

    /// Builds a plain `NSAttributedString` (one run, no attributes) for
    /// `text`, by sending `initWithString:` directly to a fresh instance -
    /// what `crate::boxed::fill` needs to make `text` readable at all.
    fn attributed_string(text: &str) -> CFTypeRef {
        foundation::bootstrap();
        let class =
            objc::class::lookup(c"NSAttributedString").expect("NSAttributedString installed");
        let instance = objc::object::create_instance(class, 0);
        let selector = objc::sel::get("initWithString:");
        let method = class
            .method(selector)
            .expect("NSAttributedString implements -initWithString:");
        // SAFETY: the registered method's own type encoding.
        let init: unsafe extern "C" fn(Id, objc::sel::Sel, Id) -> Id =
            unsafe { std::mem::transmute(method.implementation()) };
        let value = foundation::string::from_str(text) as Id;
        // SAFETY: a freshly allocated instance and a live string.
        let object = unsafe { init(instance, selector, value) };
        release(value as CFTypeRef);
        object as CFTypeRef
    }

    #[test]
    fn typesetter_shapes_a_line_into_one_run_of_its_glyphs() {
        let Some(default_family) = fonts::default_family(false) else {
            eprintln!("skipped: no fonts on this host");
            return;
        };
        let _ = default_family;
        let string = attributed_string("Hi");
        // SAFETY: a live attributed string, and every result below is this
        // runtime's own object.
        unsafe {
            let typesetter = CTTypesetterCreateWithAttributedString(string);
            assert!(!typesetter.is_null());
            let line = CTTypesetterCreateLine(typesetter, 0, 0);
            assert!(!line.is_null());
            let runs = CTLineGetGlyphRuns(line);
            assert!(!runs.is_null());
            let run = corefoundation::api::CFArrayGetValueAtIndex(runs, 0);
            assert!(!run.is_null());
            assert_eq!(CTRunGetGlyphCount(run), 2);
            let range = CTRunGetStringRange(run);
            assert_eq!((range.location, range.length), (0, 2));
            let mut glyphs = [0u16; 2];
            CTRunGetGlyphs(run, 0, 0, glyphs.as_mut_ptr());
            let mut positions = [NSPoint::default(); 2];
            CTRunGetPositions(run, 0, 0, positions.as_mut_ptr());
            let mut advances = [NSSize::default(); 2];
            CTRunGetAdvances(run, 0, 0, advances.as_mut_ptr());
            assert!(advances[0].width > 0.0, "a letter should have some width");
            assert_eq!(positions[1].x, positions[0].x + advances[0].width);
            let mut indices = [0isize; 2];
            CTRunGetStringIndices(run, 0, 0, indices.as_mut_ptr());
            assert_eq!(indices, [0, 1]);
            let attributes = CTRunGetAttributes(run);
            assert!(!attributes.is_null());
            release(line);
            release(typesetter);
            release(string);
        }
    }

    #[test]
    fn default_design_answers_the_receiver_unchanged() {
        foundation::bootstrap();
        let name = string("Fira Mono");
        // SAFETY: a descriptor this call just made.
        let descriptor = unsafe { CTFontDescriptorCreateWithNameAndSize(name, 12.0) };
        let answer = font_descriptor_with_design(descriptor, DESIGN_DEFAULT);
        assert_eq!(answer, descriptor, "Default keeps the same attributes");
        for object in [descriptor, answer, name] {
            release(object);
        }
    }

    #[test]
    fn suggest_line_break_prefers_the_space_over_splitting_a_word() {
        let Some(default_family) = fonts::default_family(false) else {
            eprintln!("skipped: no fonts on this host");
            return;
        };
        let _ = default_family;
        let text = "ab cd";
        let string = attributed_string(text);
        // SAFETY: a live attributed string.
        unsafe {
            let typesetter = CTTypesetterCreateWithAttributedString(string);
            // The same default font `CTTypesetterSuggestLineBreak` shapes
            // with, measured directly so the test's width sits exactly
            // between "ab " and "ab c" regardless of the host's own metrics.
            let font = font_for_attributes(std::ptr::null());
            let units: Vec<u16> = text.encode_utf16().collect();
            let mut glyphs = vec![0u16; units.len()];
            CTFontGetGlyphsForCharacters(
                font,
                units.as_ptr(),
                glyphs.as_mut_ptr(),
                units.len() as isize,
            );
            let mut advances = vec![NSSize::default(); units.len()];
            CTFontGetAdvancesForGlyphs(
                font,
                0,
                glyphs.as_ptr(),
                advances.as_mut_ptr(),
                units.len() as isize,
            );
            release(font);
            let cumulative: Vec<f64> = advances
                .iter()
                .scan(0.0, |total, advance| {
                    *total += advance.width;
                    Some(*total)
                })
                .collect();
            // Between "ab " (index 2, inclusive) and "ab c" (index 3): the
            // break must land right after the space, not mid-word.
            let width = (cumulative[2] + cumulative[3]) / 2.0;
            let broken = CTTypesetterSuggestLineBreak(typesetter, 0, width);
            assert_eq!(broken, 3, "should break after the space, at \"ab \"");
            release(typesetter);
            release(string);
        }
    }

    #[test]
    fn variation_axes_and_variation_cover_the_same_tags() {
        foundation::bootstrap();
        let Some(face) = fonts::all_faces().into_iter().find(|face| {
            fonts::load(&face.path, face.index)
                .is_some_and(|loaded| !loaded.face().variation_axes().is_empty())
        }) else {
            eprintln!("skipped: no variable font on this host");
            return;
        };
        // SAFETY: a font this runtime built from a real face.
        unsafe {
            let font = face_attributes(&face, 12.0);
            let axes = CTFontCopyVariationAxes(font);
            assert!(!axes.is_null());
            let variation = CTFontCopyVariation(font);
            assert!(!variation.is_null());
            let axis_count = collections::array(axes).map(|a| a.values.len());
            let variation_count = collections::dictionary(variation).map(|d| d.entries().len());
            assert_eq!(axis_count, variation_count);
            release(axes);
            release(variation);
            release(font);
        }
    }

    #[test]
    fn an_unknown_design_answers_nil() {
        foundation::bootstrap();
        let name = string("Fira Mono");
        // SAFETY: a descriptor this call just made.
        let descriptor = unsafe { CTFontDescriptorCreateWithNameAndSize(name, 12.0) };
        let answer = font_descriptor_with_design(descriptor, "NSFontDescriptorSystemDesignNope");
        assert!(answer.is_null());
        release(descriptor);
        release(name);
    }

    #[test]
    fn monospaced_design_swaps_the_family_and_drops_the_old_resolution() {
        foundation::bootstrap();
        let Some(monospace) = fonts::default_family(true) else {
            return;
        };
        let name = string("Fira Sans");
        // SAFETY: a descriptor this call just made.
        let descriptor = unsafe { CTFontDescriptorCreateWithNameAndSize(name, 12.0) };
        let answer = font_descriptor_with_design(descriptor, DESIGN_MONOSPACED);
        assert_ne!(answer, descriptor);
        assert_eq!(
            get_string(answer, "CTFontFamilyNameAttribute").as_deref(),
            Some(monospace.as_str())
        );
        assert!(get_string(answer, PATH_KEY).is_none(), "resolution cleared");
        for object in [descriptor, answer, name] {
            release(object);
        }
    }
}
