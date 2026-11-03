//! `CGFont` and the ATS font calls that predate Core Text.
//!
//! A `CGFont` here is the same attribute dictionary a `CTFont` is, as
//! `CTFontCopyGraphicsFont` already answers: the face it names is what both
//! draw with. An `ATSFontRef` is a small number this module hands out for a
//! face, stable for the life of the process, so a program that converts a font
//! to one and back gets the same face.

#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::Mutex;

use corefoundation::base::CFTypeRef;
use foundation::geometry::{NSPoint, NSRect, NSSize};

use crate::fonts::{self, FaceInfo};

type OSStatus = i32;

const NO_ERR: OSStatus = 0;
/// `paramErr`.
const PARAM_ERR: OSStatus = -50;
/// `kATSInvalidFontAccess`, for a reference that names no face.
const INVALID_FONT_ACCESS: OSStatus = -985;
/// `kATSInvalidFontTableAccess`, for a table the face does not have.
const INVALID_FONT_TABLE_ACCESS: OSStatus = -986;

/// `kInvalidFont`.
const INVALID_FONT: u32 = 0;

/// The faces references have been handed out for, by `ATSFontRef - 1`.
static REFERENCES: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());

/// Memory fonts activated through `ATSFontActivateFromMemory`, by container.
static CONTAINERS: Mutex<Vec<Vec<u32>>> = Mutex::new(Vec::new());

/// Bumped whenever the set of faces changes, for `ATSGetGeneration`.
static GENERATION: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// The reference for the face at `path` and `index`, made on first use.
fn reference(path: &str, index: u32) -> u32 {
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
fn face_for(reference: u32) -> Option<FaceInfo> {
    let (path, index) = REFERENCES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(reference.checked_sub(1)? as usize)
        .cloned()?;
    fonts::all_faces()
        .into_iter()
        .find(|face| face.path == path && face.index == index)
}

/// The text of a `CFString`, or `None` for null.
fn text(string: CFTypeRef) -> Option<String> {
    if string.is_null() {
        return None;
    }
    // SAFETY: a string, as every caller's parameter is declared.
    Some(String::from_utf8_lossy(&unsafe { foundation::string::bytes(string as _) }).into_owned())
}

/// `OSStatus ATSFontNotificationSubscribe(ATSNotificationCallback,
/// ATSFontNotifyOption, void *refcon, ATSFontNotificationRef *)`
///
/// Accepted and never called back: the faces fontconfig found do not change
/// while the program runs, and a memory font is announced by the generation
/// rather than by a notification.
pub unsafe extern "C" fn ATSFontNotificationSubscribe(
    _callback: *const c_void,
    _options: u32,
    _refcon: *mut c_void,
    out: *mut *mut c_void,
) -> OSStatus {
    static SUBSCRIPTION: u8 = 0;
    if !out.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { out.write((&raw const SUBSCRIPTION).cast_mut().cast()) };
    }
    NO_ERR
}

/// `ATSGeneration ATSGetGeneration(void)`
pub unsafe extern "C" fn ATSGetGeneration() -> u32 {
    GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// `ATSFontRef ATSFontFindFromPostScriptName(CFStringRef, ATSOptionFlags)`
pub unsafe extern "C" fn ATSFontFindFromPostScriptName(name: CFTypeRef, _options: u32) -> u32 {
    let Some(name) = text(name) else {
        return INVALID_FONT;
    };
    fonts::all_faces()
        .into_iter()
        .find(|face| face.postscript == name)
        .map_or(INVALID_FONT, |face| reference(&face.path, face.index))
}

/// `ATSFontRef ATSFontFindFromName(CFStringRef, ATSOptionFlags)`: a full name,
/// `family style`, or a family's regular face for a bare family name. The
/// PostScript name is accepted too, as Apple's does.
pub unsafe extern "C" fn ATSFontFindFromName(name: CFTypeRef, _options: u32) -> u32 {
    let Some(name) = text(name) else {
        return INVALID_FONT;
    };
    let faces = fonts::all_faces();
    let full = |face: &FaceInfo| match face.style.eq_ignore_ascii_case("regular") {
        true => face.family.clone(),
        false => format!("{} {}", face.family, face.style),
    };
    faces
        .iter()
        .find(|face| face.postscript == name || full(face).eq_ignore_ascii_case(&name))
        .map_or(INVALID_FONT, |face| reference(&face.path, face.index))
}

/// `OSStatus ATSFontGetPostScriptName(ATSFontRef, ATSOptionFlags, CFStringRef *)`
pub unsafe extern "C" fn ATSFontGetPostScriptName(
    font: u32,
    _options: u32,
    out: *mut CFTypeRef,
) -> OSStatus {
    if out.is_null() {
        return PARAM_ERR;
    }
    let Some(face) = face_for(font) else {
        return INVALID_FONT_ACCESS;
    };
    // SAFETY: writable, per the caller; the string follows the copy rule.
    unsafe { out.write(foundation::string::from_str(&face.postscript) as CFTypeRef) };
    NO_ERR
}

/// `OSStatus ATSFontGetTable(ATSFontRef, FourCharCode, ByteOffset, ByteCount,
/// void *, ByteCount *)`
///
/// With no buffer, the table's length; with one, up to `size` bytes from
/// `offset` and how many were copied.
pub unsafe extern "C" fn ATSFontGetTable(
    font: u32,
    tag: u32,
    offset: u32,
    size: usize,
    buffer: *mut u8,
    out_size: *mut usize,
) -> OSStatus {
    let Some(face) = face_for(font) else {
        return INVALID_FONT_ACCESS;
    };
    let Some(loaded) = fonts::load(&face.path, face.index) else {
        return INVALID_FONT_ACCESS;
    };
    let tag = owned_ttf_parser::Tag::from_bytes(&tag.to_be_bytes());
    let Some(table) = loaded.face().raw_face().table(tag) else {
        return INVALID_FONT_TABLE_ACCESS;
    };
    let start = (offset as usize).min(table.len());
    let available = &table[start..];
    let copied = match buffer.is_null() {
        true => available.len(),
        false => {
            let n = available.len().min(size);
            // SAFETY: the caller supplies `size` writable bytes.
            unsafe { std::ptr::copy_nonoverlapping(available.as_ptr(), buffer, n) };
            n
        }
    };
    if !out_size.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { out_size.write(copied) };
    }
    NO_ERR
}

/// `ATSFontMetrics`, every length a fraction of the em.
#[repr(C)]
pub struct Metrics {
    version: u32,
    ascent: f32,
    descent: f32,
    leading: f32,
    avg_advance_width: f32,
    max_advance_width: f32,
    min_left_side_bearing: f32,
    min_right_side_bearing: f32,
    stem_width: f32,
    stem_height: f32,
    cap_height: f32,
    x_height: f32,
    italic_angle: f32,
    underline_position: f32,
    underline_thickness: f32,
}

/// `OSStatus ATSFontGetHorizontalMetrics(ATSFontRef, ATSOptionFlags,
/// ATSFontMetrics *)`
pub unsafe extern "C" fn ATSFontGetHorizontalMetrics(
    font: u32,
    _options: u32,
    out: *mut Metrics,
) -> OSStatus {
    if out.is_null() {
        return PARAM_ERR;
    }
    let Some(info) = face_for(font) else {
        return INVALID_FONT_ACCESS;
    };
    let Some(loaded) = fonts::load(&info.path, info.index) else {
        return INVALID_FONT_ACCESS;
    };
    let face = loaded.face();
    let em = f32::from(face.units_per_em().max(1));
    let unit = |v: i16| f32::from(v) / em;
    // `hhea.advanceWidthMax` and `OS/2.xAvgCharWidth`, which the parser does
    // not expose, at their fixed offsets.
    let raw = |tag: &[u8; 4], at: usize| {
        face.raw_face()
            .table(owned_ttf_parser::Tag::from_bytes(tag))
            .and_then(|t| t.get(at..at + 2))
            .map_or(0, |b| u16::from_be_bytes([b[0], b[1]]))
    };
    let hhea_max_advance = raw(b"hhea", 10);
    let avg = raw(b"OS/2", 2) as i16;
    let underline = face.underline_metrics();
    let metrics = Metrics {
        version: 0,
        ascent: unit(face.ascender()),
        descent: unit(face.descender()),
        leading: unit(face.line_gap()),
        avg_advance_width: unit(avg),
        max_advance_width: f32::from(hhea_max_advance) / em,
        min_left_side_bearing: 0.0,
        min_right_side_bearing: 0.0,
        stem_width: 0.0,
        stem_height: 0.0,
        cap_height: unit(face.capital_height().unwrap_or(face.ascender())),
        x_height: unit(face.x_height().unwrap_or(face.ascender() / 2)),
        italic_angle: face.italic_angle(),
        underline_position: underline.map_or(-0.1, |m| unit(m.position)),
        underline_thickness: underline.map_or(0.05, |m| unit(m.thickness)),
    };
    // SAFETY: writable, per the caller.
    unsafe { out.write(metrics) };
    NO_ERR
}

/// `OSStatus ATSFontActivateFromMemory(LogicalAddress, ByteCount,
/// ATSFontContext, ATSFontFormat, void *, ATSOptionFlags, ATSFontContainerRef *)`
///
/// A downloaded font: written out where the face database can read it, and
/// its faces registered under a container.
pub unsafe extern "C" fn ATSFontActivateFromMemory(
    data: *const u8,
    length: usize,
    _context: u32,
    _format: u32,
    _reserved: *mut c_void,
    _options: u32,
    out: *mut u32,
) -> OSStatus {
    if data.is_null() || out.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller supplies `length` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(data, length) };
    let Some(path) = coretext::spill_font(bytes) else {
        return PARAM_ERR;
    };
    let faces = fonts::register_file(&path);
    if faces.is_empty() {
        return PARAM_ERR;
    }
    let refs = faces
        .iter()
        .map(|face| reference(&face.path, face.index))
        .collect();
    let mut containers = CONTAINERS.lock().unwrap_or_else(|e| e.into_inner());
    containers.push(refs);
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // SAFETY: writable, per the caller.
    unsafe { out.write(containers.len() as u32) };
    NO_ERR
}

/// `OSStatus ATSFontFindFromContainer(ATSFontContainerRef, ATSOptionFlags,
/// ItemCount, ATSFontRef *, ItemCount *)`
pub unsafe extern "C" fn ATSFontFindFromContainer(
    container: u32,
    _options: u32,
    count: usize,
    fonts_out: *mut u32,
    out_count: *mut usize,
) -> OSStatus {
    let containers = CONTAINERS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(refs) = container
        .checked_sub(1)
        .and_then(|at| containers.get(at as usize))
    else {
        return PARAM_ERR;
    };
    if !fonts_out.is_null() {
        for (at, font) in refs.iter().take(count).enumerate() {
            // SAFETY: the caller supplies `count` writable references.
            unsafe { fonts_out.add(at).write(*font) };
        }
    }
    if !out_count.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { out_count.write(refs.len()) };
    }
    NO_ERR
}

/// `OSStatus ATSFontDeactivate(ATSFontContainerRef, void *, ATSOptionFlags)`
///
/// FIXME (ACCURACY): the faces stay findable by name; the face database has no
/// way to forget a file. A page that unloads a web font and loads another of
/// the same name gets whichever was registered first.
pub unsafe extern "C" fn ATSFontDeactivate(
    container: u32,
    _refcon: *mut c_void,
    _options: u32,
) -> OSStatus {
    let containers = CONTAINERS.lock().unwrap_or_else(|e| e.into_inner());
    match container
        .checked_sub(1)
        .and_then(|at| containers.get(at as usize))
    {
        Some(_) => NO_ERR,
        None => PARAM_ERR,
    }
}

/// `ATSFontFamilyRef ATSFontFamilyFindFromQuickDrawName(ConstStr255Param)`
///
/// `kInvalidFontFamily`: QuickDraw family numbers have no meaning for a face
/// fontconfig found.
pub unsafe extern "C" fn ATSFontFamilyFindFromQuickDrawName(_name: *const u8) -> u32 {
    u32::MAX
}

/// `OSStatus ATSFontFamilyGetName(ATSFontFamilyRef, ATSOptionFlags, CFStringRef *)`
pub unsafe extern "C" fn ATSFontFamilyGetName(
    _family: u32,
    _options: u32,
    _out: *mut CFTypeRef,
) -> OSStatus {
    INVALID_FONT_ACCESS
}

/// The face a `CGFont` or `CTFont` draws with, as a reference.
fn reference_of(font: CFTypeRef) -> u32 {
    coretext::face_location(font).map_or(INVALID_FONT, |(path, index)| reference(&path, index))
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
    coretext::face_attributes(&face, 0.0)
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
    coretext::face_attributes(&face, size)
}

/// `ATSFontRef CTFontGetPlatformFont(CTFontRef, CTFontDescriptorRef *)`
pub unsafe extern "C" fn CTFontGetPlatformFont(font: CFTypeRef, attributes: *mut CFTypeRef) -> u32 {
    if !attributes.is_null() {
        // SAFETY: writable, per the caller. The descriptor is optional.
        unsafe { attributes.write(std::ptr::null()) };
    }
    reference_of(font)
}

/// `CGFontRef CGFontCreateWithFontName(CFStringRef)`: by PostScript or full
/// name, as `CTFontCreateWithName` resolves one.
pub unsafe extern "C" fn CGFontCreateWithFontName(name: CFTypeRef) -> CFTypeRef {
    // SAFETY: a string or null.
    unsafe { coretext::CTFontCreateWithName(name, 0.0, std::ptr::null()) }
}

/// `CGFontRef CGFontCreateWithDataProvider(CGDataProviderRef)`: a font file's
/// bytes.
pub unsafe extern "C" fn CGFontCreateWithDataProvider(provider: CFTypeRef) -> CFTypeRef {
    let Some(bytes) = crate::graphics::provider_bytes(provider) else {
        return std::ptr::null();
    };
    let Some(path) = coretext::spill_font(bytes) else {
        return std::ptr::null();
    };
    match fonts::register_file(&path).first() {
        Some(face) => {
            GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            coretext::face_attributes(face, 0.0)
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
    // SAFETY: a font this runtime made, or null.
    unsafe { coretext::CTFontGetUnitsPerEm(font) as i32 }
}

/// `int CGFontGetXHeight(CGFontRef)`, in font units.
pub unsafe extern "C" fn CGFontGetXHeight(font: CFTypeRef) -> i32 {
    coretext::loaded(font).map_or(0, |face| {
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
    let Some(face) = coretext::loaded(font) else {
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
pub unsafe extern "C" fn CGFontCopyTableForTag(font: CFTypeRef, tag: u32) -> CFTypeRef {
    // SAFETY: a font this runtime made, or null.
    unsafe { coretext::CTFontCopyTable(font, tag, 0) }
}

/// `CFIndex CTFontGetGlyphCount(CTFontRef)`
pub unsafe extern "C" fn CTFontGetGlyphCount(font: CFTypeRef) -> isize {
    coretext::loaded(font).map_or(0, |face| face.face().number_of_glyphs() as isize)
}

/// A face's design-unit rectangle as a `CGRect`.
fn unit_rect(r: owned_ttf_parser::Rect) -> NSRect {
    NSRect {
        origin: NSPoint {
            x: f64::from(r.x_min),
            y: f64::from(r.y_min),
        },
        size: NSSize {
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
    boxes: *mut NSRect,
) -> bool {
    let Some(face) = coretext::loaded(font) else {
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
                .map_or(NSRect::default(), unit_rect);
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
    let Some(face) = coretext::loaded(font) else {
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
    let Some(face) = coretext::loaded(font) else {
        return std::ptr::null();
    };
    let m = match transform.is_null() {
        true => crate::cgcontext::IDENTITY,
        // SAFETY: the caller's transform.
        false => unsafe { *transform },
    };
    let mut outline = crate::cgpath::Elements::default();
    crate::cgpath::glyph_outline(face.face(), glyph, m, &mut outline);
    crate::cgpath::path_from(outline)
}

/// `CGRect CGFontGetFontBBox(CGFontRef)`, the `head` table's, in font units.
pub unsafe extern "C" fn CGFontGetFontBBox(font: CFTypeRef) -> NSRect {
    coretext::loaded(font).map_or(NSRect::default(), |face| {
        unit_rect(face.face().global_bounding_box())
    })
}

/// `int CGFontGetAscent(CGFontRef)` and its two neighbours, in font units.
pub unsafe extern "C" fn CGFontGetAscent(font: CFTypeRef) -> i32 {
    coretext::loaded(font).map_or(0, |face| i32::from(face.face().ascender()))
}

pub unsafe extern "C" fn CGFontGetDescent(font: CFTypeRef) -> i32 {
    coretext::loaded(font).map_or(0, |face| i32::from(face.face().descender()))
}

pub unsafe extern "C" fn CGFontGetLeading(font: CFTypeRef) -> i32 {
    coretext::loaded(font).map_or(0, |face| i32::from(face.face().line_gap()))
}

exports::exports! {
    functions {
        "ATSFontNotificationSubscribe" => ATSFontNotificationSubscribe,
        "ATSGetGeneration" => ATSGetGeneration,
        "ATSFontFindFromPostScriptName" => ATSFontFindFromPostScriptName,
        "ATSFontFindFromName" => ATSFontFindFromName,
        "ATSFontGetPostScriptName" => ATSFontGetPostScriptName,
        "ATSFontGetTable" => ATSFontGetTable,
        "ATSFontGetHorizontalMetrics" => ATSFontGetHorizontalMetrics,
        "ATSFontActivateFromMemory" => ATSFontActivateFromMemory,
        "ATSFontFindFromContainer" => ATSFontFindFromContainer,
        "ATSFontDeactivate" => ATSFontDeactivate,
        "ATSFontFamilyFindFromQuickDrawName" => ATSFontFamilyFindFromQuickDrawName,
        "ATSFontFamilyGetName" => ATSFontFamilyGetName,
        "CGFontCreateWithPlatformFont" => CGFontCreateWithPlatformFont,
        "CGFontCreateWithFontName" => CGFontCreateWithFontName,
        "CGFontCreateWithDataProvider" => CGFontCreateWithDataProvider,
        "CGFontRetain" => CGFontRetain,
        "CGFontRelease" => CGFontRelease,
        "CGFontGetUnitsPerEm" => CGFontGetUnitsPerEm,
        "CGFontGetXHeight" => CGFontGetXHeight,
        "CGFontGetGlyphAdvances" => CGFontGetGlyphAdvances,
        "CGFontCopyTableForTag" => CGFontCopyTableForTag,
        "CTFontCreateWithPlatformFont" => CTFontCreateWithPlatformFont,
        "CTFontGetPlatformFont" => CTFontGetPlatformFont,
        "CTFontGetGlyphCount" => CTFontGetGlyphCount,
        "CGFontGetGlyphBBoxes" => CGFontGetGlyphBBoxes,
        "CGFontGetGlyphsForUnichars" => CGFontGetGlyphsForUnichars,
        "CGFontGetGlyphPath" => CGFontGetGlyphPath,
        "CGFontGetFontBBox" => CGFontGetFontBBox,
        "CGFontGetAscent" => CGFontGetAscent,
        "CGFontGetDescent" => CGFontGetDescent,
        "CGFontGetLeading" => CGFontGetLeading,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reference converts back to the face it was made for, and the same face
    /// always gets the same reference.
    #[test]
    fn a_reference_round_trips_to_its_face() {
        let Some(face) = fonts::all_faces().into_iter().next() else {
            eprintln!("skipped: no fonts on this host");
            return;
        };
        let first = reference(&face.path, face.index);
        assert_ne!(first, INVALID_FONT);
        assert_eq!(reference(&face.path, face.index), first);
        let back = face_for(first).expect("the face");
        assert_eq!((back.path, back.index), (face.path, face.index));
    }

    /// The table length comes back without a buffer, and the bytes with one.
    #[test]
    fn a_table_is_read_by_length_then_bytes() {
        let Some(face) = fonts::all_faces().into_iter().next() else {
            eprintln!("skipped: no fonts on this host");
            return;
        };
        let font = reference(&face.path, face.index);
        let head = u32::from_be_bytes(*b"head");
        let mut length = 0usize;
        // SAFETY: no buffer, and a writable length.
        let status =
            unsafe { ATSFontGetTable(font, head, 0, 0, std::ptr::null_mut(), &mut length) };
        assert_eq!(status, NO_ERR);
        assert!(length >= 54, "a head table is at least 54 bytes");
        let mut bytes = vec![0u8; length];
        let mut copied = 0usize;
        // SAFETY: a buffer of `length` bytes.
        let status =
            unsafe { ATSFontGetTable(font, head, 0, length, bytes.as_mut_ptr(), &mut copied) };
        assert_eq!((status, copied), (NO_ERR, length));
        // `magicNumber`, twelve bytes in.
        assert_eq!(&bytes[12..16], &[0x5f, 0x0f, 0x3c, 0xf5]);
    }

    #[test]
    fn an_unknown_reference_is_refused() {
        let mut length = 0usize;
        // SAFETY: no buffer, and a writable length.
        let status =
            unsafe { ATSFontGetTable(u32::MAX, 0, 0, 0, std::ptr::null_mut(), &mut length) };
        assert_eq!(status, INVALID_FONT_ACCESS);
    }
}
