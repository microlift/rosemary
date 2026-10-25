//! `CGFont`: a font as the attribute dictionary CoreText also uses, and the
//! ATS references a `CGFont` can be made from.

#![allow(non_snake_case)]

use std::sync::Mutex;

use corefoundation::base::CFTypeRef;
use corefoundation::collections;

use crate::cgdataprovider::provider_bytes;
use crate::cgpath::{Elements, glyph_outline, path_from};
use crate::font_attributes::{
    face_attributes, face_location, key, loaded, make_font, new_dictionary, release, spill_font,
};
use crate::fonts::{self, FaceInfo};
use crate::{CGAffineTransformIdentity, CGPoint, CGRect, CGSize};

/// `kInvalidFont`.
pub const INVALID_FONT: u32 = 0;

/// The faces references have been handed out for, by `ATSFontRef - 1`.
static REFERENCES: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());

/// Bumped whenever the set of faces changes, for `ATSGetGeneration`.
static GENERATION: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// The reference for the face at `path` and `index`, made on first use.
pub fn reference(path: &str, index: u32) -> u32 {
    let mut table = REFERENCES.lock().unwrap_or_else(|e| e.into_inner());
    let at = match table.iter().position(|(p, i)| p == path && *i == index) {
        Some(at) => at,
        None => {
            table.push((path.to_string(), index));
            table.len() - 1
        }
    };
    at as u32 + 1
}

/// The face a reference names.
pub fn face_for(reference: u32) -> Option<FaceInfo> {
    let (path, index) = REFERENCES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(reference.checked_sub(1)? as usize)
        .cloned()?;
    fonts::all_faces()
        .into_iter()
        .find(|face| face.path == path && face.index == index)
}

/// The face a `CGFont` or `CTFont` draws with, as a reference.
pub fn reference_of(font: CFTypeRef) -> u32 {
    face_location(font).map_or(INVALID_FONT, |(path, index)| reference(&path, index))
}

/// The generation of the face set, for `ATSGetGeneration`.
pub fn generation() -> u32 {
    GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Note that the face set changed.
pub fn bump_generation() {
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// `CGFontRef CGFontCreateWithPlatformFont(void *platformFontReference)`: the
/// reference is an `ATSFontRef *`.
pub unsafe extern "C" fn CGFontCreateWithPlatformFont(platform: *const u32) -> CFTypeRef {
    if platform.is_null() {
        return std::ptr::null();
    }
    // SAFETY: an `ATSFontRef *`, as the documentation defines it.
    let Some(face) = face_for(unsafe { *platform }) else {
        return std::ptr::null();
    };
    face_attributes(&face, 0.0)
}

/// `CGFontRef CGFontCreateWithFontName(CFStringRef)`: by PostScript or full
/// name, as `CTFontCreateWithName` resolves one.
pub unsafe extern "C" fn CGFontCreateWithFontName(name: CFTypeRef) -> CFTypeRef {
    if name.is_null() {
        return std::ptr::null();
    }
    let descriptor = new_dictionary(&[
        (key("CTFontNameAttribute"), name),
        (key("CTFontFamilyNameAttribute"), name),
    ]);
    let font = make_font(descriptor, 0.0);
    release(descriptor);
    font
}

/// `CGFontRef CGFontCreateWithDataProvider(CGDataProviderRef)`: a font file's
/// bytes.
pub unsafe extern "C" fn CGFontCreateWithDataProvider(provider: CFTypeRef) -> CFTypeRef {
    let Some(bytes) = provider_bytes(provider) else {
        return std::ptr::null();
    };
    let Some(path) = spill_font(bytes) else {
        return std::ptr::null();
    };
    match fonts::register_file(&path).first() {
        Some(face) => {
            bump_generation();
            face_attributes(face, 0.0)
        }
        None => std::ptr::null(),
    }
}

pub unsafe extern "C" fn CGFontRetain(font: CFTypeRef) -> CFTypeRef {
    if !font.is_null() {
        // SAFETY: a font this runtime made.
        unsafe { corefoundation::api::CFRetain(font) };
    }
    font
}

pub unsafe extern "C" fn CGFontRelease(font: CFTypeRef) {
    if !font.is_null() {
        // SAFETY: a reference the caller owns.
        unsafe { corefoundation::api::CFRelease(font) };
    }
}

/// `int CGFontGetUnitsPerEm(CGFontRef)`
pub unsafe extern "C" fn CGFontGetUnitsPerEm(font: CFTypeRef) -> i32 {
    loaded(font).map_or(1000, |face| i32::from(face.face().units_per_em()))
}

/// `int CGFontGetXHeight(CGFontRef)`, in font units.
pub unsafe extern "C" fn CGFontGetXHeight(font: CFTypeRef) -> i32 {
    loaded(font).map_or(0, |face| {
        let face = face.face();
        i32::from(face.x_height().unwrap_or(face.ascender() / 2))
    })
}

/// `bool CGFontGetGlyphAdvances(CGFontRef, const CGGlyph *, size_t, int *)`,
/// in font units.
pub unsafe extern "C" fn CGFontGetGlyphAdvances(
    font: CFTypeRef,
    glyphs: *const u16,
    count: usize,
    advances: *mut i32,
) -> bool {
    let Some(face) = loaded(font) else {
        return false;
    };
    if count > 0 && (glyphs.is_null() || advances.is_null()) {
        return false;
    }
    for at in 0..count {
        // SAFETY: the caller supplies `count` glyphs and advances.
        unsafe {
            let glyph = owned_ttf_parser::GlyphId(*glyphs.add(at));
            let width = face.face().glyph_hor_advance(glyph).unwrap_or(0);
            advances.add(at).write(i32::from(width));
        }
    }
    true
}

/// `CFDataRef CGFontCopyTableForTag(CGFontRef, uint32_t)`
pub unsafe extern "C" fn CGFontCopyTableForTag(font: CFTypeRef, table: u32) -> CFTypeRef {
    let Some(face) = loaded(font) else {
        return std::ptr::null();
    };
    let tag = owned_ttf_parser::Tag::from_bytes(&table.to_be_bytes());
    match face.face().raw_face().table(tag) {
        Some(bytes) => collections::new_data(bytes.to_vec()),
        None => std::ptr::null(),
    }
}

/// A face's design-unit rectangle as a `CGRect`.
fn unit_rect(r: owned_ttf_parser::Rect) -> CGRect {
    CGRect {
        origin: CGPoint {
            x: f64::from(r.x_min),
            y: f64::from(r.y_min),
        },
        size: CGSize {
            width: f64::from(r.x_max) - f64::from(r.x_min),
            height: f64::from(r.y_max) - f64::from(r.y_min),
        },
    }
}

/// `bool CGFontGetGlyphBBoxes(CGFontRef, const CGGlyph *, size_t, CGRect *)`,
/// in font units. A glyph with no outline - a space - has the empty box.
pub unsafe extern "C" fn CGFontGetGlyphBBoxes(
    font: CFTypeRef,
    glyphs: *const u16,
    count: usize,
    boxes: *mut CGRect,
) -> bool {
    let Some(face) = loaded(font) else {
        return false;
    };
    if count > 0 && (glyphs.is_null() || boxes.is_null()) {
        return false;
    }
    for at in 0..count {
        // SAFETY: the caller supplies `count` glyphs and boxes.
        unsafe {
            let glyph = owned_ttf_parser::GlyphId(*glyphs.add(at));
            let rect = face
                .face()
                .glyph_bounding_box(glyph)
                .map_or(CGRect::default(), unit_rect);
            boxes.add(at).write(rect);
        }
    }
    true
}

/// `void CGFontGetGlyphsForUnichars(CGFontRef, const UniChar *, CGGlyph *,
/// size_t)`, private but looked up by name by Cairo's Quartz backend.
///
/// One glyph per UTF-16 unit; a surrogate pair maps to its character's glyph
/// in the first slot and zero in the second, as Apple's does.
pub unsafe extern "C" fn CGFontGetGlyphsForUnichars(
    font: CFTypeRef,
    characters: *const u16,
    glyphs: *mut u16,
    count: usize,
) {
    if count == 0 || characters.is_null() || glyphs.is_null() {
        return;
    }
    // SAFETY: the caller supplies `count` characters and glyph slots.
    let (units, out) = unsafe {
        (
            std::slice::from_raw_parts(characters, count),
            std::slice::from_raw_parts_mut(glyphs, count),
        )
    };
    out.fill(0);
    let Some(face) = loaded(font) else {
        return;
    };
    let mut at = 0;
    for decoded in char::decode_utf16(units.iter().copied()) {
        let width = decoded.as_ref().map_or(1, |c| c.len_utf16());
        if let Ok(c) = decoded {
            out[at] = face.face().glyph_index(c).map_or(0, |g| g.0);
        }
        at += width;
    }
}

/// `CGPathRef CGFontGetGlyphPath(CGFontRef, const CGAffineTransform *, int,
/// CGGlyph)`, private and looked up by Cairo: the outline in font units under
/// `transform`, which the caller releases.
pub unsafe extern "C" fn CGFontGetGlyphPath(
    font: CFTypeRef,
    transform: *const crate::cgcontext::CGAffineTransform,
    _unknown: i32,
    glyph: u16,
) -> CFTypeRef {
    let Some(face) = loaded(font) else {
        return std::ptr::null();
    };
    let m = match transform.is_null() {
        true => CGAffineTransformIdentity,
        // SAFETY: the caller's transform.
        false => unsafe { *transform },
    };
    let mut outline = Elements::default();
    glyph_outline(face.face(), glyph, m, &mut outline);
    path_from(outline).cast()
}

/// `CGRect CGFontGetFontBBox(CGFontRef)`, the `head` table's, in font units.
pub unsafe extern "C" fn CGFontGetFontBBox(font: CFTypeRef) -> CGRect {
    loaded(font).map_or(CGRect::default(), |face| {
        unit_rect(face.face().global_bounding_box())
    })
}

/// `int CGFontGetAscent(CGFontRef)` and its two neighbours, in font units.
pub unsafe extern "C" fn CGFontGetAscent(font: CFTypeRef) -> i32 {
    loaded(font).map_or(0, |face| i32::from(face.face().ascender()))
}

pub unsafe extern "C" fn CGFontGetDescent(font: CFTypeRef) -> i32 {
    loaded(font).map_or(0, |face| i32::from(face.face().descender()))
}

pub unsafe extern "C" fn CGFontGetLeading(font: CFTypeRef) -> i32 {
    loaded(font).map_or(0, |face| i32::from(face.face().line_gap()))
}

exports::exports! {
    functions {
        "CGFontCreateWithPlatformFont" => CGFontCreateWithPlatformFont,
        "CGFontCreateWithFontName" => CGFontCreateWithFontName,
        "CGFontCreateWithDataProvider" => CGFontCreateWithDataProvider,
        "CGFontRetain" => CGFontRetain,
        "CGFontRelease" => CGFontRelease,
        "CGFontGetUnitsPerEm" => CGFontGetUnitsPerEm,
        "CGFontGetXHeight" => CGFontGetXHeight,
        "CGFontGetGlyphAdvances" => CGFontGetGlyphAdvances,
        "CGFontCopyTableForTag" => CGFontCopyTableForTag,
        "CGFontGetGlyphBBoxes" => CGFontGetGlyphBBoxes,
        "CGFontGetGlyphsForUnichars" => CGFontGetGlyphsForUnichars,
        "CGFontGetGlyphPath" => CGFontGetGlyphPath,
        "CGFontGetFontBBox" => CGFontGetFontBBox,
        "CGFontGetAscent" => CGFontGetAscent,
        "CGFontGetDescent" => CGFontGetDescent,
        "CGFontGetLeading" => CGFontGetLeading,
    }
}
