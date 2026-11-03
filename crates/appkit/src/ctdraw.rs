//! The two Core Text calls that draw or outline glyphs through Core Graphics,
//! which is why they are here rather than in the `coretext` crate.

#![allow(non_snake_case)]

use corefoundation::base::CFTypeRef;
use foundation::geometry::NSPoint;

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
    let Some(face) = coretext::loaded(font) else {
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
    crate::quartz::draw_glyphs(
        context,
        &face,
        coretext::font_size(font),
        glyphs,
        &positions,
    );
}

pub unsafe extern "C" fn CTFontCreatePathForGlyph(
    font: CFTypeRef,
    glyph: u16,
    transform: *const crate::cgcontext::CGAffineTransform,
) -> CFTypeRef {
    let Some(face) = coretext::loaded(font) else {
        return std::ptr::null();
    };
    let s = coretext::scale(font, &face);
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

exports::exports! {
    functions {
        "CTFontDrawGlyphs" => CTFontDrawGlyphs,
        "CTFontCreatePathForGlyph" => CTFontCreatePathForGlyph,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundation::geometry::NSSize;

    /// The host's default sans-serif font, or `None` on a host with no
    /// fontconfig, where this test has nothing to read.
    fn default_font(size: f64) -> Option<CFTypeRef> {
        crate::bootstrap();
        chrome::fonts::default_family(false)?;
        // SAFETY: a null language is documented.
        let font = unsafe { coretext::CTFontCreateUIFontForLanguage(0, size, std::ptr::null()) };
        coretext::loaded(font).is_some().then_some(font)
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
                coretext::CTFontGetGlyphsForCharacters(
                    font,
                    characters.as_ptr(),
                    glyph.as_mut_ptr(),
                    1
                ),
                1
            );
            assert_ne!(glyph[0], u16::from(b'H'));
            let mut advance = NSSize::default();
            coretext::CTFontGetAdvancesForGlyphs(font, 0, glyph.as_ptr(), &mut advance, 1);
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
            super::CTFontDrawGlyphs(font, glyph.as_ptr(), &origin, 1, context);
            corefoundation::api::CFRelease(context);
            corefoundation::api::CFRelease(font);
        }
        let covered = pixels.iter().filter(|&&value| value > 128).count();
        assert!(covered > 20, "only {covered} pixels covered");
    }
}
