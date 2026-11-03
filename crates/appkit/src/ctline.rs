#![allow(non_snake_case)]

//! `CTLine`, `CTParagraphStyle` and `CTTextTab`: a line of attributed text
//! laid out left to right with the attributed fonts, measured, hit-tested and
//! drawn into a context.
//!
//! FIXME (ACCURACY): no shaping beyond one glyph per character - no
//! ligatures, kerning pairs, bidirectional reordering or font fallback for a
//! character the attributed font lacks, which draws as that font's missing
//! glyph.

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use foundation::geometry::{NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

/// One glyph of a run: the glyph, its pen position from the line's start,
/// its advance, and the UTF-16 index of the character it draws.
#[derive(Clone, Copy)]
struct Glyph {
    glyph: u16,
    x: f64,
    advance: f64,
    index: usize,
}

/// A stretch of the line drawn with one font and one colour.
struct Run {
    /// Retained.
    font: CFTypeRef,
    /// A `CGColor`, retained, or null to draw in the context's fill colour.
    color: CFTypeRef,
    underline: i64,
    glyphs: Vec<Glyph>,
}

impl Drop for Run {
    fn drop(&mut self) {
        for object in [self.font, self.color] {
            if !object.is_null() {
                // SAFETY: references this run took.
                unsafe { corefoundation::api::CFRelease(object) };
            }
        }
    }
}

pub struct Line {
    runs: Vec<Run>,
    width: f64,
    ascent: f64,
    descent: f64,
    leading: f64,
    /// UTF-16 units in the string the line was made from.
    length: usize,
}

#[repr(C)]
struct LineObject {
    isa: *const c_void,
    line: *mut Line,
}

/// A `CTParagraphStyle`: only the settings a line here acts on.
#[derive(Clone, Default)]
pub struct Paragraph {
    /// Tab stop locations, in points from the line's start.
    tabs: Vec<f64>,
    /// `kCTParagraphStyleSpecifierDefaultTabInterval`: zero for none.
    tab_interval: f64,
}

#[repr(C)]
struct ParagraphObject {
    isa: *const c_void,
    paragraph: *mut Paragraph,
}

#[repr(C)]
struct TabObject {
    isa: *const c_void,
    alignment: u8,
    location: f64,
}

fn send_id(receiver: Id, sel: &str) -> Id {
    // SAFETY: `id (id, SEL)`, as each selector this is used with is.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel))
    }
}

fn is_kind(object: CFTypeRef, name: &std::ffi::CStr) -> bool {
    let Some(class) = objc::class::lookup(name) else {
        return false;
    };
    // SAFETY: null or an object of this runtime's.
    !object.is_null() && unsafe { corefoundation::base::is_kind_of(object, class) }
}

fn attribute(attributes: CFTypeRef, name: &'static str) -> CFTypeRef {
    let key = foundation::notification::name_object(name) as CFTypeRef;
    // SAFETY: a dictionary of this runtime's, and a string key.
    unsafe { corefoundation::api::CFDictionaryGetValue(attributes, key) }
}

fn number(value: CFTypeRef) -> Option<f64> {
    if value.is_null() {
        return None;
    }
    // SAFETY: `-doubleValue`, which a number answers.
    Some(unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> f64 = std::mem::transmute(objc::msg::entry());
        f(value as Id, objc::sel::get("doubleValue"))
    })
}

/// The colour an attribute dictionary draws its text in, as a `CGColor`
/// this call owns, or null for the context's fill colour.
fn run_color(attributes: CFTypeRef) -> CFTypeRef {
    let from_context = attribute(attributes, "CTForegroundColorFromContext");
    if number(from_context).is_some_and(|v| v != 0.0) {
        return std::ptr::null();
    }
    let cg = attribute(attributes, "CTForegroundColor");
    let cg = match cg.is_null() {
        false => cg,
        // AppKit's spelling holds an `NSColor`, which knows its `CGColor`.
        true => {
            let ns = attribute(attributes, "NSColor");
            if ns.is_null() {
                return std::ptr::null();
            }
            send_id(ns as Id, "CGColor") as CFTypeRef
        }
    };
    if cg.is_null() {
        return cg;
    }
    // SAFETY: a colour the dictionary holds, kept for the run.
    unsafe { corefoundation::api::CFRetain(cg) }
}

/// The font for a run's attributes, retained: the attributed one, or
/// Helvetica at 12 points as CoreText defaults to.
fn run_font(attributes: CFTypeRef) -> CFTypeRef {
    let font = attribute(attributes, "NSFont");
    if !font.is_null() {
        // SAFETY: a font the dictionary holds, kept for the run.
        return unsafe { corefoundation::api::CFRetain(font) };
    }
    let name = foundation::notification::name_object("Helvetica") as CFTypeRef;
    // SAFETY: a name and a size; no matrix.
    unsafe { coretext::CTFontCreateWithName(name, 12.0, std::ptr::null()) }
}

fn paragraph_of(attributes: CFTypeRef) -> Paragraph {
    let style = attribute(attributes, "NSParagraphStyle");
    if !is_kind(style, c"CTParagraphStyle") {
        return Paragraph::default();
    }
    // SAFETY: a paragraph style made below.
    unsafe { (*(style as *const ParagraphObject)).paragraph.as_ref() }
        .cloned()
        .unwrap_or_default()
}

/// Where a tab at `x` takes the pen: the next stop past it, then every
/// interval, then AppKit's default of every 28 points.
fn next_tab(paragraph: &Paragraph, x: f64) -> f64 {
    if let Some(stop) = paragraph.tabs.iter().copied().find(|t| *t > x + 0.001) {
        return stop;
    }
    let interval = match paragraph.tab_interval > 0.0 {
        true => paragraph.tab_interval,
        false => 28.0,
    };
    ((x / interval).floor() + 1.0) * interval
}

/// Lay out an attributed string.
/// `kCGFontIndexInvalid`: the glyph of a character with nothing to draw.
const INVISIBLE: u16 = 0xFFFF;

fn layout(string: Id) -> Line {
    let length = send_length(string);
    let text = send_id(string, "string");
    // SAFETY: an NSString.
    let units: Vec<u16> = {
        let bytes = unsafe { foundation::string::bytes(text) };
        String::from_utf8_lossy(&bytes).encode_utf16().collect()
    };
    let mut runs = Vec::new();
    let mut x = 0.0;
    let (mut ascent, mut descent, mut leading) = (0.0f64, 0.0f64, 0.0f64);
    let mut at = 0;
    while at < length.min(units.len()) {
        let mut effective = foundation::geometry::NSRange::default();
        // SAFETY: `-attributesAtIndex:effectiveRange:`.
        let attributes = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, usize, *mut foundation::geometry::NSRange) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                string,
                objc::sel::get("attributesAtIndex:effectiveRange:"),
                at,
                &mut effective,
            )
        } as CFTypeRef;
        let end = (effective.location + effective.length).clamp(at + 1, units.len());
        let font = run_font(attributes);
        let color = run_color(attributes);
        let paragraph = paragraph_of(attributes);
        let kern = number(attribute(attributes, "NSKern")).unwrap_or(0.0);
        let underline = number(attribute(attributes, "NSUnderline")).unwrap_or(0.0) as i64;
        // SAFETY: a font this run holds.
        unsafe {
            ascent = ascent.max(coretext::CTFontGetAscent(font));
            descent = descent.max(coretext::CTFontGetDescent(font));
            leading = leading.max(coretext::CTFontGetLeading(font));
        }
        let mut glyphs = Vec::new();
        let mut i = at;
        while i < end {
            let width = match (units[i], units.get(i + 1)) {
                (0xD800..=0xDBFF, Some(0xDC00..=0xDFFF)) if i + 1 < end => 2,
                _ => 1,
            };
            // A surrogate pair maps to its glyph in the first slot.
            let mut out = [0u16; 2];
            // SAFETY: `width` readable units and as many writable glyphs.
            unsafe {
                coretext::CTFontGetGlyphsForCharacters(
                    font,
                    units[i..].as_ptr(),
                    out.as_mut_ptr(),
                    width as isize,
                );
            }
            // A control character - a tab, a line end - is laid out but has
            // nothing to draw: `kCGFontIndexInvalid`, which drawing skips.
            let glyph = match units[i] {
                0x00..=0x1F | 0x7F => INVISIBLE,
                _ => out[0],
            };
            let advance = match units[i] {
                0x09 => next_tab(&paragraph, x) - x,
                // Line ends take no room.
                0x0A | 0x0D => 0.0,
                _ => {
                    let mut size = NSSize::default();
                    // SAFETY: one glyph and one advance.
                    unsafe { coretext::CTFontGetAdvancesForGlyphs(font, 0, &glyph, &mut size, 1) };
                    size.width + kern
                }
            };
            glyphs.push(Glyph {
                glyph,
                x,
                advance,
                index: i,
            });
            x += advance;
            i += width;
        }
        runs.push(Run {
            font,
            color,
            underline,
            glyphs,
        });
        at = end;
    }
    Line {
        runs,
        width: x,
        ascent,
        descent,
        leading,
        length: units.len(),
    }
}

fn send_length(string: Id) -> usize {
    // SAFETY: `-length`, which an attributed string answers.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());
        f(string, objc::sel::get("length"))
    }
}

fn line_of<'a>(line: CFTypeRef) -> Option<&'a Line> {
    if !is_kind(line, c"CTLine") {
        return None;
    }
    // SAFETY: a line object made by `CTLineCreateWithAttributedString`.
    unsafe { (*(line as *const LineObject)).line.as_ref() }
}

/// `CTLineRef CTLineCreateWithAttributedString(CFAttributedStringRef)`.
pub unsafe extern "C" fn CTLineCreateWithAttributedString(string: CFTypeRef) -> CFTypeRef {
    if string.is_null() {
        return std::ptr::null();
    }
    let line = layout(string as Id);
    let class = objc::class::lookup(c"CTLine").expect("installed during bootstrap");
    // SAFETY: `LineObject` is the registered layout.
    let object = unsafe { corefoundation::base::allocate::<LineObject>(class) };
    // SAFETY: a fresh allocation.
    unsafe { (*object).line = Box::into_raw(Box::new(line)) };
    object as CFTypeRef
}

/// `double CTLineGetTypographicBounds(CTLineRef, CGFloat *ascent, CGFloat
/// *descent, CGFloat *leading)`: the width, and the three metrics.
pub unsafe extern "C" fn CTLineGetTypographicBounds(
    line: CFTypeRef,
    ascent: *mut f64,
    descent: *mut f64,
    leading: *mut f64,
) -> f64 {
    let Some(line) = line_of(line) else {
        return 0.0;
    };
    for (out, value) in [
        (ascent, line.ascent),
        (descent, line.descent),
        (leading, line.leading),
    ] {
        // SAFETY: null or writable, by the caller.
        if let Some(out) = unsafe { out.as_mut() } {
            *out = value;
        }
    }
    line.width
}

/// `CGFloat CTLineGetTrailingWhitespaceWidth(CTLineRef)`.
pub unsafe extern "C" fn CTLineGetTrailingWhitespaceWidth(line: CFTypeRef) -> f64 {
    let Some(line) = line_of(line) else {
        return 0.0;
    };
    let mut width = 0.0;
    for run in line.runs.iter().rev() {
        for glyph in run.glyphs.iter().rev() {
            // A glyph with no outline and an advance is white space; it is
            // the closest test that needs no copy of the string.
            if glyph.advance <= 0.0 {
                continue;
            }
            let Some((face, _)) = coretext::face_of(run.font) else {
                return width;
            };
            let blank = face
                .face()
                .glyph_bounding_box(owned_ttf_parser::GlyphId(glyph.glyph))
                .is_none();
            if !blank {
                return width;
            }
            width += glyph.advance;
        }
    }
    width
}

/// `CFIndex CTLineGetGlyphCount(CTLineRef)`.
pub unsafe extern "C" fn CTLineGetGlyphCount(line: CFTypeRef) -> isize {
    line_of(line).map_or(0, |l| {
        l.runs.iter().map(|r| r.glyphs.len()).sum::<usize>() as isize
    })
}

/// `CFRange CTLineGetStringRange(CTLineRef)`.
pub unsafe extern "C" fn CTLineGetStringRange(line: CFTypeRef) -> foundation::geometry::NSRange {
    foundation::geometry::NSRange {
        location: 0,
        length: line_of(line).map_or(0, |l| l.length),
    }
}

/// `CGFloat CTLineGetOffsetForStringIndex(CTLineRef, CFIndex, CGFloat
/// *secondaryOffset)`: where the caret before `index` stands.
pub unsafe extern "C" fn CTLineGetOffsetForStringIndex(
    line: CFTypeRef,
    index: isize,
    secondary: *mut f64,
) -> f64 {
    let offset = line_of(line).map_or(0.0, |line| {
        let index = index.max(0) as usize;
        line.runs
            .iter()
            .flat_map(|r| r.glyphs.iter())
            .find(|g| g.index >= index)
            .map_or(line.width, |g| g.x)
    });
    // SAFETY: null or writable. Left to right only, so one caret.
    if let Some(secondary) = unsafe { secondary.as_mut() } {
        *secondary = offset;
    }
    offset
}

/// `CFIndex CTLineGetStringIndexForPosition(CTLineRef, CGPoint)`: the
/// character boundary nearest `position`.
pub unsafe extern "C" fn CTLineGetStringIndexForPosition(
    line: CFTypeRef,
    position: NSPoint,
) -> isize {
    let Some(line) = line_of(line) else {
        return -1;
    };
    for glyph in line.runs.iter().flat_map(|r| r.glyphs.iter()) {
        if position.x < glyph.x + glyph.advance / 2.0 {
            return glyph.index as isize;
        }
    }
    line.length as isize
}

/// `CGRect CTLineGetImageBounds(CTLineRef, CGContextRef)`: the typographic
/// box at the text position, which covers what the glyphs draw.
pub unsafe extern "C" fn CTLineGetImageBounds(line: CFTypeRef, context: CFTypeRef) -> NSRect {
    let Some(line) = line_of(line) else {
        return NSRect::default();
    };
    // SAFETY: a context or null.
    let at = unsafe { crate::cgcontext::CGContextGetTextPosition(context) };
    NSRect {
        origin: NSPoint {
            x: at.x,
            y: at.y - line.descent,
        },
        size: NSSize {
            width: line.width,
            height: line.ascent + line.descent,
        },
    }
}

/// `void CTLineDraw(CTLineRef, CGContextRef)`: at the context's text
/// position, each run in its colour, underlines after.
pub unsafe extern "C" fn CTLineDraw(line: CFTypeRef, context: CFTypeRef) {
    let Some(line) = line_of(line) else {
        return;
    };
    // SAFETY: a context.
    let origin = unsafe { crate::cgcontext::CGContextGetTextPosition(context) };
    for run in &line.runs {
        if run.glyphs.is_empty() {
            continue;
        }
        // SAFETY: a context, saved and restored around the run's colour.
        unsafe {
            crate::cgcontext::CGContextSaveGState(context);
            if !run.color.is_null() {
                crate::cgcontext::CGContextSetFillColorWithColor(context, run.color);
            }
        }
        let shown = || run.glyphs.iter().filter(|g| g.glyph != INVISIBLE);
        let glyphs: Vec<u16> = shown().map(|g| g.glyph).collect();
        let positions: Vec<NSPoint> = shown().map(|g| NSPoint { x: g.x, y: 0.0 }).collect();
        // SAFETY: as many glyphs as positions.
        unsafe {
            crate::ctdraw::CTFontDrawGlyphs(
                run.font,
                glyphs.as_ptr(),
                positions.as_ptr(),
                glyphs.len(),
                context,
            );
        }
        if run.underline != 0 {
            let first = run.glyphs[0].x;
            let last = run.glyphs.last().map_or(first, |g| g.x + g.advance);
            // SAFETY: the run's font, and a rectangle in the context.
            unsafe {
                let position = coretext::CTFontGetUnderlinePosition(run.font);
                let thickness = coretext::CTFontGetUnderlineThickness(run.font).max(1.0);
                crate::cgcontext::CGContextFillRect(
                    context,
                    NSRect {
                        origin: NSPoint {
                            x: origin.x + first,
                            y: origin.y + position - thickness / 2.0,
                        },
                        size: NSSize {
                            width: last - first,
                            height: thickness,
                        },
                    },
                );
            }
        }
        // SAFETY: as above.
        unsafe { crate::cgcontext::CGContextRestoreGState(context) };
    }
}

/// `CTParagraphStyleSetting`.
#[repr(C)]
pub struct Setting {
    spec: u32,
    size: usize,
    value: *const c_void,
}

/// `kCTParagraphStyleSpecifierTabStops` and `...DefaultTabInterval`.
const TAB_STOPS: u32 = 4;
const DEFAULT_TAB_INTERVAL: u32 = 5;

/// `CTParagraphStyleRef CTParagraphStyleCreate(const CTParagraphStyleSetting
/// *settings, size_t count)`. Settings a line here does not act on are kept
/// out rather than refused.
pub unsafe extern "C" fn CTParagraphStyleCreate(
    settings: *const Setting,
    count: usize,
) -> CFTypeRef {
    let mut paragraph = Paragraph::default();
    if !settings.is_null() {
        // SAFETY: `count` settings, by the caller.
        for setting in unsafe { std::slice::from_raw_parts(settings, count) } {
            match setting.spec {
                // A `CFArrayRef` of `CTTextTab`s, passed by address.
                TAB_STOPS if !setting.value.is_null() => {
                    // SAFETY: a pointer to an array reference.
                    let array = unsafe { *(setting.value as *const CFTypeRef) };
                    // SAFETY: null or an array of this runtime's.
                    if let Some(array) = unsafe { corefoundation::collections::array(array) } {
                        for tab in &array.values {
                            if is_kind(*tab, c"CTTextTab") {
                                // SAFETY: a tab made below.
                                paragraph
                                    .tabs
                                    .push(unsafe { (*(*tab as *const TabObject)).location });
                            }
                        }
                    }
                    paragraph.tabs.sort_by(f64::total_cmp);
                }
                // A `CGFloat`, by address.
                DEFAULT_TAB_INTERVAL if !setting.value.is_null() => {
                    // SAFETY: a pointer to a double.
                    paragraph.tab_interval = unsafe { *(setting.value as *const f64) };
                }
                _ => {}
            }
        }
    }
    let class = objc::class::lookup(c"CTParagraphStyle").expect("installed during bootstrap");
    // SAFETY: `ParagraphObject` is the registered layout.
    let object = unsafe { corefoundation::base::allocate::<ParagraphObject>(class) };
    // SAFETY: a fresh allocation.
    unsafe { (*object).paragraph = Box::into_raw(Box::new(paragraph)) };
    object as CFTypeRef
}

/// `CTTextTabRef CTTextTabCreate(CTTextAlignment, double location,
/// CFDictionaryRef options)`.
///
/// FIXME (ACCURACY): every tab is a left tab.
pub unsafe extern "C" fn CTTextTabCreate(
    alignment: u8,
    location: f64,
    _options: CFTypeRef,
) -> CFTypeRef {
    let class = objc::class::lookup(c"CTTextTab").expect("installed during bootstrap");
    // SAFETY: `TabObject` is the registered layout.
    let object = unsafe { corefoundation::base::allocate::<TabObject>(class) };
    // SAFETY: a fresh allocation.
    unsafe {
        (*object).alignment = alignment;
        (*object).location = location;
    }
    object as CFTypeRef
}

unsafe extern "C" fn line_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a line object whose payload came from `Box::into_raw`.
    unsafe {
        let object = &mut *(receiver as *mut LineObject);
        if !object.line.is_null() {
            drop(Box::from_raw(object.line));
            object.line = std::ptr::null_mut();
        }
    }
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn paragraph_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: as `line_dealloc`.
    unsafe {
        let object = &mut *(receiver as *mut ParagraphObject);
        if !object.paragraph.is_null() {
            drop(Box::from_raw(object.paragraph));
            object.paragraph = std::ptr::null_mut();
        }
    }
    objc::arc::finalize(receiver);
}

pub(crate) fn install() {
    let line = corefoundation::base::register(c"CTLine", None, size_of::<LineObject>(), true);
    line.replace_method(
        objc::sel::get("dealloc"),
        line_dealloc as *const c_void,
        c"v16@0:8",
    );
    let paragraph = corefoundation::base::register(
        c"CTParagraphStyle",
        None,
        size_of::<ParagraphObject>(),
        true,
    );
    paragraph.replace_method(
        objc::sel::get("dealloc"),
        paragraph_dealloc as *const c_void,
        c"v16@0:8",
    );
    corefoundation::base::register(c"CTTextTab", None, size_of::<TabObject>(), true);
}

exports::exports! {
    functions {
        "CTLineCreateWithAttributedString" => CTLineCreateWithAttributedString,
        "CTLineGetTypographicBounds" => CTLineGetTypographicBounds,
        "CTLineGetTrailingWhitespaceWidth" => CTLineGetTrailingWhitespaceWidth,
        "CTLineGetGlyphCount" => CTLineGetGlyphCount,
        "CTLineGetStringRange" => CTLineGetStringRange,
        "CTLineGetOffsetForStringIndex" => CTLineGetOffsetForStringIndex,
        "CTLineGetStringIndexForPosition" => CTLineGetStringIndexForPosition,
        "CTLineGetImageBounds" => CTLineGetImageBounds,
        "CTLineDraw" => CTLineDraw,
        "CTParagraphStyleCreate" => CTParagraphStyleCreate,
        "CTTextTabCreate" => CTTextTabCreate,
    }
}
