use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use corefoundation::collections::{self, Number};
use coregraphics::cgcontext::*;
use coregraphics::font_attributes::{key, loaded, new_dictionary, release};
use coregraphics::{CGPoint, CGRect, CGSize};
use coretext::*;
use objc::object::Id;
use objc::sel::Sel;

fn line(text: &str, attributes: CFTypeRef) -> CFTypeRef {
    coretext::bootstrap();
    let string = foundation::string::from_str(text);
    let class = objc::class::lookup(c"NSAttributedString").unwrap();
    let object = objc::object::create_instance(class, 0);
    // SAFETY: the initializer accepts a string and an optional attributes dictionary.
    unsafe {
        let init: unsafe extern "C" fn(Id, Sel, Id, CFTypeRef) -> Id =
            std::mem::transmute(objc::msg::entry());
        let object = init(
            object,
            objc::sel::get("initWithString:attributes:"),
            string,
            attributes,
        );
        let result = CTLineCreateWithAttributedString(object.cast());
        release(object.cast());
        release(string.cast());
        result
    }
}

fn font() -> Option<CFTypeRef> {
    coretext::bootstrap();
    // SAFETY: the default UI font, with no language preference.
    let font = unsafe { CTFontCreateUIFontForLanguage(0, 16.0, std::ptr::null()) };
    if loaded(font).is_some() {
        Some(font)
    } else {
        release(font);
        eprintln!("skipping font-dependent CoreText test: no usable host font");
        None
    }
}

#[test]
fn an_empty_line_has_zero_metrics_and_an_empty_range() {
    let line = line("", std::ptr::null());
    let (mut ascent, mut descent, mut leading) = (-1.0, -1.0, -1.0);
    // SAFETY: a live line and writable metric outputs.
    unsafe {
        assert_eq!(
            CTLineGetTypographicBounds(line, &mut ascent, &mut descent, &mut leading),
            0.0
        );
        assert_eq!((ascent, descent, leading), (0.0, 0.0, 0.0));
        assert_eq!(CTLineGetGlyphCount(line), 0);
        let range = CTLineGetStringRange(line);
        assert_eq!((range.location, range.length), (0, 0));
        assert_eq!(CTLineGetStringIndexForPosition(line, CGPoint::default()), 0);
    }
    release(line);
}

#[test]
fn control_characters_advance_without_drawing_missing_glyphs() {
    let line = line("\t\r\n", std::ptr::null());
    let mut pixels = [0u8; 64 * 32 * 4];
    // SAFETY: a live line and a context wrapping the supplied pixel buffer.
    unsafe {
        let context = coregraphics::cgbitmap::CGBitmapContextCreate(
            pixels.as_mut_ptr().cast(), 64, 32, 8, 64 * 4, std::ptr::null(), 1,
        );
        CGContextSetRGBFillColor(context, 1.0, 1.0, 1.0, 1.0);
        CGContextSetTextPosition(context, 0.0, 8.0);
        assert_eq!(CTLineGetGlyphCount(line), 3);
        assert_eq!(
            CTLineGetTypographicBounds(line, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()),
            28.0
        );
        CTLineDraw(line, context);
        CGContextRelease(context);
    }
    assert!(pixels.iter().all(|byte| *byte == 0));
    release(line);
}

#[test]
fn paragraph_tabs_use_sorted_explicit_stops_then_the_default_interval() {
    coretext::bootstrap();
    // SAFETY: documented null options, live settings and correctly sized values.
    unsafe {
        let later = CTTextTabCreate(0, 40.0, std::ptr::null());
        let earlier = CTTextTabCreate(0, 10.0, std::ptr::null());
        let tabs = collections::new_array(
            &[later, earlier], corefoundation::callbacks::Policy::from_type_callbacks(), false,
        );
        let interval = 12.0f64;
        let settings = [
            CTParagraphStyleSetting { spec: 4, size: size_of::<CFTypeRef>(), value: (&tabs as *const CFTypeRef).cast() },
            CTParagraphStyleSetting { spec: 5, size: size_of::<f64>(), value: (&interval as *const f64).cast() },
        ];
        let style = CTParagraphStyleCreate(settings.as_ptr(), settings.len());
        let attributes = new_dictionary(&[(key("NSParagraphStyle"), style)]);
        let line = line("\t\t\t", attributes);
        for object in [attributes, style, tabs, later, earlier] {
            release(object);
        }
        for (index, expected) in [(0, 0.0), (1, 10.0), (2, 40.0), (3, 48.0)] {
            assert_eq!(CTLineGetOffsetForStringIndex(line, index, std::ptr::null_mut()), expected);
        }
        release(line);
    }
}

#[test]
fn measurement_and_hit_testing_use_utf16_string_indices() {
    let Some(font) = font() else { return };
    let attributes = new_dictionary(&[(key("NSFont"), font)]);
    let line = line("A\u{1f600}B  ", attributes);
    release(attributes);
    release(font);
    let (mut ascent, mut descent) = (0.0, 0.0);
    // SAFETY: the line owns its fonts after the attributed string is released.
    unsafe {
        let width = CTLineGetTypographicBounds(line, &mut ascent, &mut descent, std::ptr::null_mut());
        assert!(width > 0.0 && ascent > 0.0 && descent >= 0.0);
        assert_eq!(CTLineGetGlyphCount(line), 5);
        let range = CTLineGetStringRange(line);
        assert_eq!((range.location, range.length), (0, 6));
        let mut secondary = -1.0;
        let offset = CTLineGetOffsetForStringIndex(line, 3, &mut secondary);
        assert_eq!(secondary, offset);
        assert!(offset > 0.0 && offset < width);
        assert_eq!(CTLineGetStringIndexForPosition(line, CGPoint { x: offset, y: 0.0 }), 3);
        assert_eq!(CTLineGetOffsetForStringIndex(line, 6, std::ptr::null_mut()), width);
        let whitespace_start = CTLineGetOffsetForStringIndex(line, 4, std::ptr::null_mut());
        assert!((CTLineGetTrailingWhitespaceWidth(line) - (width - whitespace_start)).abs() < 1e-9);
    }
    release(line);
}

#[test]
fn drawing_uses_run_colors_and_underlines_without_leaking_graphics_state() {
    let Some(font) = font() else { return };
    // SAFETY: live fonts and colors, and a context wrapping the pixel buffer.
    unsafe {
        let green = coregraphics::cgcolor::CGColorCreateGenericRGB(0.0, 1.0, 0.0, 1.0);
        let underline = collections::new_number(Number::Integer(1));
        let attributes = new_dictionary(&[
            (key("NSFont"), font), (key("CTForegroundColor"), green), (key("NSUnderline"), underline),
        ]);
        let line = line("Hi", attributes);
        for object in [attributes, font, green, underline] { release(object); }
        let mut pixels = [0u8; 64 * 32 * 4];
        let context = coregraphics::cgbitmap::CGBitmapContextCreate(
            pixels.as_mut_ptr().cast(), 64, 32, 8, 64 * 4, std::ptr::null(), 1,
        );
        CGContextSetTextPosition(context, 4.0, 8.0);
        CGContextSetRGBFillColor(context, 1.0, 0.0, 0.0, 1.0);
        let ctm = CGContextGetCTM(context);
        let position = CGContextGetTextPosition(context);
        CTLineDraw(line, context);
        assert_eq!(CGContextGetCTM(context), ctm);
        assert_eq!(CGContextGetTextPosition(context), position);
        let bounds = CTLineGetImageBounds(line, context);
        assert_eq!(bounds.origin.x, 4.0);
        assert!(bounds.size.width > 0.0 && bounds.size.height > 0.0);
        CGContextFillRect(context, CGRect { origin: CGPoint::default(), size: CGSize { width: 2.0, height: 2.0 } });
        CGContextRelease(context);
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[0] == 0 && pixel[1] > 128 && pixel[2] == 0));
        assert_eq!(&pixels[(31 * 64) * 4..(31 * 64) * 4 + 4], &[255, 0, 0, 255]);
        release(line);
    }
}

#[test]
fn null_line_queries_and_drawing_are_safe() {
    coretext::bootstrap();
    let null = std::ptr::null::<c_void>();
    // SAFETY: null handles are supported by these entry points.
    unsafe {
        assert!(CTLineCreateWithAttributedString(null).is_null());
        assert_eq!(CTLineGetGlyphCount(null), 0);
        assert_eq!(CTLineGetTrailingWhitespaceWidth(null), 0.0);
        assert_eq!(CTLineGetStringIndexForPosition(null, CGPoint::default()), -1);
        assert_eq!(CTLineGetImageBounds(null, null), CGRect::default());
        CTLineDraw(null, null);
    }
}
