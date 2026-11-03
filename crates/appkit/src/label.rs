//! How an `NSTextField` looks - its font, colours and alignment - and the
//! drawing and size that follow from them.
//!
//! One line of text, drawn by [`chrome`]'s renderer: what a label is. An
//! editable field shows its editor's text the same way while it is not being
//! edited.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use foundation::geometry::{NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;
use crate::view;

/// What a field was configured with. Colours and the font are retained.
#[derive(Clone, Copy)]
struct Look {
    font: usize,
    text_color: usize,
    background: usize,
    draws_background: bool,
    bordered: bool,
    alignment: isize,
    /// `NSLineBreakMode`; only the two wrapping modes draw more than a line.
    line_break: isize,
    /// `-maximumNumberOfLines`, 0 for as many as fit.
    max_lines: isize,
    single_line: bool,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            font: 0,
            text_color: 0,
            background: 0,
            draws_background: false,
            bordered: false,
            alignment: NATURAL,
            line_break: TRUNCATE_TAIL,
            max_lines: 0,
            single_line: false,
        }
    }
}

impl Look {
    fn wraps(&self) -> bool {
        !self.single_line && self.max_lines != 1 && matches!(self.line_break, WORD_WRAP | CHAR_WRAP)
    }
}

/// `NSTextAlignment` on macOS, where right is 1 and centre 2.
const RIGHT: isize = 1;
const CENTER: isize = 2;
const NATURAL: isize = 4;

/// `NSLineBreakMode`.
const WORD_WRAP: isize = 0;
const CHAR_WRAP: isize = 1;
const CLIP: isize = 2;
const TRUNCATE_TAIL: isize = 4;

/// The room `NSTextFieldCell` leaves either side of its text.
const PADDING: f64 = 2.0;

/// AppKit's system font size, for a field given no font.
const SYSTEM_SIZE: f64 = 13.0;

fn looks() -> &'static Mutex<HashMap<usize, Look>> {
    static LOOKS: OnceLock<Mutex<HashMap<usize, Look>>> = OnceLock::new();
    LOOKS.get_or_init(Default::default)
}

fn look(receiver: Id) -> Look {
    looks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(receiver as usize))
        .copied()
        .unwrap_or_default()
}

fn update(receiver: Id, with: impl FnOnce(&mut Look)) {
    with(
        looks()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(receiver as usize)
            .or_default(),
    );
    changed(receiver);
}

/// Redraw a field whose text or look changed, and let the stacks and
/// constraints it sits in size it again.
pub(crate) fn changed(receiver: Id) {
    crate::backing::needs_display(receiver);
    crate::layout::solve();
}

/// Replace a retained object in a field's look.
fn store(receiver: Id, value: Id, slot: impl FnOnce(&mut Look) -> &mut usize) {
    let retained = objc::arc::retain(value) as usize;
    let mut old = 0;
    update(receiver, |look| {
        old = std::mem::replace(slot(look), retained)
    });
    objc::arc::release(old as Id);
}

pub(crate) fn forget(receiver: Id) {
    let gone = looks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(receiver as usize));
    if let Some(look) = gone {
        for object in [look.font, look.text_color, look.background] {
            objc::arc::release(object as Id);
        }
    }
}

/// The font a field draws with, and its size in points.
fn font_of(look: &Look) -> Option<(Arc<chrome::Font>, f64)> {
    chrome_font(look.font as Id)
}

/// An `NSFont` as the face and point size chrome draws with; the system font
/// for nil or a font with no face here.
pub(crate) fn chrome_font(font: Id) -> Option<(Arc<chrome::Font>, f64)> {
    if let Some((face, size)) = coretext::face_of(font as *const _)
        && let Some(font) = chrome::Font::from_faces_with_fallback(vec![face])
    {
        return Some((Arc::new(font), if size > 0.0 { size } else { SYSTEM_SIZE }));
    }
    Some((chrome::Font::system(400)?, SYSTEM_SIZE))
}

pub(crate) fn color_of(color: usize, default: chrome::Color) -> chrome::Color {
    if color == 0 {
        return default;
    }
    // SAFETY: a colour the guest set, retained here.
    let (r, g, b, a) = unsafe { crate::color::components(color as Id) };
    chrome::Color {
        r: r as f32,
        g: g as f32,
        b: b as f32,
        a: a as f32,
    }
}

fn is_text_field(object: Id) -> bool {
    // SAFETY: null or an object this runtime allocated.
    unsafe { objc::object::class_of(object) }.is_some_and(|c| {
        objc::class::lookup(c"NSTextField").is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    })
}

/// The size a field's text needs, which is what AppKit's
/// `-intrinsicContentSize` says and what a stack sizes a label to.
pub(crate) fn intrinsic_size(receiver: Id) -> Option<NSSize> {
    if !is_text_field(receiver) {
        return None;
    }
    let look = look(receiver);
    let (font, size) = font_of(&look)?;
    let text = crate::text::field_text(receiver);
    let (ascent, descent) = font.metrics(size as f32);
    Some(NSSize {
        width: (f64::from(font.measure(&text, size as f32)) + 2.0 * PADDING).ceil(),
        height: f64::from(ascent + descent).ceil(),
    })
}

/// `-[NSTextFieldCell cellSizeForBounds:]`: the intrinsic size, except that a
/// wrapping field breaks its text to the width of `bounds` and is as tall as
/// its lines.
pub(crate) fn cell_size(receiver: Id, bounds: NSRect) -> Option<NSSize> {
    let natural = intrinsic_size(receiver)?;
    let look = look(receiver);
    if !look.wraps() || natural.width <= bounds.size.width {
        return Some(natural);
    }
    let (font, size) = font_of(&look)?;
    let room = (bounds.size.width - 2.0 * PADDING).max(0.0) as f32;
    let text = crate::text::field_text(receiver);
    let mut lines = match look.line_break {
        CHAR_WRAP => char_wrap(&font, &text, size as f32, room),
        _ => font.wrap(&text, size as f32, room),
    };
    if look.max_lines > 0 {
        lines.truncate(look.max_lines as usize);
    }
    let widest = lines
        .iter()
        .map(|line| font.measure(line, size as f32))
        .fold(0.0f32, f32::max);
    let (ascent, descent) = font.metrics(size as f32);
    Some(NSSize {
        width: (f64::from(widest) + 2.0 * PADDING).ceil(),
        height: f64::from((ascent + descent).ceil()) * lines.len().max(1) as f64,
    })
}

/// `-setWraps:`, which AppKit spells as a line break mode: word wrapping, or
/// clipping.
unsafe extern "C" fn set_wraps(receiver: Id, _sel: Sel, wraps: u8) {
    update(receiver, |look| {
        look.line_break = match wraps {
            0 => CLIP,
            _ => WORD_WRAP,
        }
    });
}

unsafe extern "C" fn wraps(receiver: Id, _sel: Sel) -> u8 {
    u8::from(matches!(look(receiver).line_break, WORD_WRAP | CHAR_WRAP))
}

unsafe extern "C" fn intrinsic_content_size(receiver: Id, _sel: Sel) -> NSSize {
    intrinsic_size(receiver).unwrap_or(NSSize {
        // `NSViewNoIntrinsicMetric`.
        width: -1.0,
        height: -1.0,
    })
}

unsafe extern "C" fn draw_rect(receiver: Id, _sel: Sel, _dirty: NSRect) {
    let look = look(receiver);
    let bounds = view::bounds_of(receiver);
    let density = crate::backing::focus_density();
    let width = (bounds.size.width * density).ceil() as usize;
    let height = (bounds.size.height * density).ceil() as usize;
    if width == 0 || height == 0 {
        return;
    }
    let mut pixels = vec![0u8; width * height * 4];
    let mut canvas = chrome::Canvas::new(&mut pixels, width, height, width * 4, density as f32);
    let (w, h) = (bounds.size.width as f32, bounds.size.height as f32);
    if look.draws_background && look.background != 0 {
        let fill = color_of(look.background, chrome::Color::rgba(0, 0, 0, 0.0));
        canvas.fill_rect(chrome::Rect::new(0.0, 0.0, w, h), fill);
    }
    if look.bordered {
        let edge = chrome::Color::rgba(0, 0, 0, 0.25);
        canvas.stroke_rounded_rect(
            chrome::Rect::new(0.5, 0.5, w - 1.0, h - 1.0),
            0.0,
            1.0,
            edge,
        );
    }
    let text = crate::text::field_text(receiver);
    if let Some((font, size)) = font_of(&look)
        && !text.is_empty()
    {
        let size = size as f32;
        let room = (w - 2.0 * PADDING as f32).max(0.0);
        let color = color_of(look.text_color, chrome::Color::rgba(0, 0, 0, 0.85));
        let x_for = |advance: f32| match look.alignment {
            RIGHT => w - PADDING as f32 - advance,
            CENTER => (w - advance) / 2.0,
            _ => PADDING as f32,
        };
        match look.wraps() {
            // A wrapping field sets its lines from the top, as a Mac's does.
            true => {
                let (ascent, descent) = font.metrics(size);
                let mut lines = match look.line_break {
                    CHAR_WRAP => char_wrap(&font, &text, size, room),
                    _ => font.wrap(&text, size, room),
                };
                if look.max_lines > 0 {
                    lines.truncate(look.max_lines as usize);
                }
                let leading = (ascent + descent).ceil();
                for (i, line) in lines.iter().enumerate() {
                    let baseline = ascent.round() + i as f32 * leading;
                    let advance = font.measure(line, size);
                    canvas.draw_text(
                        line,
                        &font,
                        size,
                        chrome::Point::new(x_for(advance), baseline),
                        color,
                    );
                }
            }
            false => {
                let line = font.truncate(&text, size, room);
                let advance = font.measure(&line, size);
                let baseline = font.centered_baseline(size, 0.0, h);
                canvas.draw_text(
                    &line,
                    &font,
                    size,
                    chrome::Point::new(x_for(advance), baseline),
                    color,
                );
            }
        }
    }
    crate::backing::draw_image(
        &pixels,
        width,
        height,
        bounds.size,
        bounds,
        NSRect {
            origin: NSPoint::default(),
            size: bounds.size,
        },
        // Source-over.
        2,
        1.0,
        true,
    );
}

/// `text` broken into lines no wider than `width` at any character, which is
/// `NSLineBreakByCharWrapping`. Each `\n` starts a line.
fn char_wrap(font: &chrome::Font, text: &str, size: f32, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for c in paragraph.chars() {
            line.push(c);
            if line.chars().nth(1).is_some() && font.measure(&line, size) > width {
                line.pop();
                lines.push(std::mem::take(&mut line));
                line.push(c);
            }
        }
        lines.push(line);
    }
    lines
}

unsafe extern "C" fn set_line_break_mode(receiver: Id, _sel: Sel, mode: isize) {
    update(receiver, |look| look.line_break = mode);
}

unsafe extern "C" fn line_break_mode(receiver: Id, _sel: Sel) -> isize {
    look(receiver).line_break
}

unsafe extern "C" fn set_maximum_number_of_lines(receiver: Id, _sel: Sel, lines: isize) {
    update(receiver, |look| look.max_lines = lines);
}

unsafe extern "C" fn maximum_number_of_lines(receiver: Id, _sel: Sel) -> isize {
    look(receiver).max_lines
}

unsafe extern "C" fn set_uses_single_line_mode(receiver: Id, _sel: Sel, single: u8) {
    update(receiver, |look| look.single_line = single != 0);
}

unsafe extern "C" fn uses_single_line_mode(receiver: Id, _sel: Sel) -> u8 {
    u8::from(look(receiver).single_line)
}

unsafe extern "C" fn set_font(receiver: Id, _sel: Sel, font: Id) {
    store(receiver, font, |look| &mut look.font);
}

unsafe extern "C" fn font(receiver: Id, _sel: Sel) -> Id {
    look(receiver).font as Id
}

unsafe extern "C" fn set_text_color(receiver: Id, _sel: Sel, color: Id) {
    store(receiver, color, |look| &mut look.text_color);
}

unsafe extern "C" fn text_color(receiver: Id, _sel: Sel) -> Id {
    look(receiver).text_color as Id
}

unsafe extern "C" fn set_background_color(receiver: Id, _sel: Sel, color: Id) {
    store(receiver, color, |look| &mut look.background);
}

unsafe extern "C" fn background_color(receiver: Id, _sel: Sel) -> Id {
    look(receiver).background as Id
}

unsafe extern "C" fn set_draws_background(receiver: Id, _sel: Sel, flag: u8) {
    update(receiver, |look| look.draws_background = flag != 0);
}

unsafe extern "C" fn draws_background(receiver: Id, _sel: Sel) -> u8 {
    u8::from(look(receiver).draws_background)
}

unsafe extern "C" fn set_bordered(receiver: Id, _sel: Sel, flag: u8) {
    update(receiver, |look| look.bordered = flag != 0);
}

unsafe extern "C" fn is_bordered(receiver: Id, _sel: Sel) -> u8 {
    u8::from(look(receiver).bordered)
}

unsafe extern "C" fn set_alignment(receiver: Id, _sel: Sel, alignment: isize) {
    update(receiver, |look| look.alignment = alignment);
}

unsafe extern "C" fn alignment(receiver: Id, _sel: Sel) -> isize {
    look(receiver).alignment
}

pub(crate) fn install() {
    let field = objc::class::lookup(c"NSTextField").expect("declared before its methods");
    methods!(field, {
        "setFont:" => set_font, c"v24@0:8@16";
        "font" => font, c"@16@0:8";
        "setTextColor:" => set_text_color, c"v24@0:8@16";
        "textColor" => text_color, c"@16@0:8";
        "setBackgroundColor:" => set_background_color, c"v24@0:8@16";
        "backgroundColor" => background_color, c"@16@0:8";
        "setDrawsBackground:" => set_draws_background, c"v20@0:8c16";
        "drawsBackground" => draws_background, c"c16@0:8";
        "setBordered:" => set_bordered, c"v20@0:8c16";
        "isBordered" => is_bordered, c"c16@0:8";
        // FIXME (ACCURACY): kept, and not drawn: a field here draws its border
        // the same way bezeled or not.
        "isBezeled" => crate::props::get_bool, c"c16@0:8";
        "setBezeled:" => crate::props::set_bool, c"v20@0:8c16";
        "bezelStyle" => crate::props::get_integer, c"q16@0:8";
        "setBezelStyle:" => crate::props::set_integer, c"v24@0:8q16";
        "setAlignment:" => set_alignment, c"v24@0:8q16";
        "alignment" => alignment, c"q16@0:8";
        "setLineBreakMode:" => set_line_break_mode, c"v24@0:8q16";
        "lineBreakMode" => line_break_mode, c"q16@0:8";
        "setMaximumNumberOfLines:" => set_maximum_number_of_lines, c"v24@0:8q16";
        "maximumNumberOfLines" => maximum_number_of_lines, c"q16@0:8";
        "setUsesSingleLineMode:" => set_uses_single_line_mode, c"v20@0:8c16";
        "usesSingleLineMode" => uses_single_line_mode, c"c16@0:8";
        "setWraps:" => set_wraps, c"v20@0:8c16";
        "wraps" => wraps, c"c16@0:8";
        "intrinsicContentSize" => intrinsic_content_size, c"{CGSize=dd}16@0:8";
        "drawRect:" => draw_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_is_as_wide_as_its_text_and_centres_it() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSTextField").expect("NSTextField");
        let label = objc::object::create_instance(class, 0);
        let empty = intrinsic_size(label).expect("a text field has a size");
        let text = foundation::string::from_str("Messages");
        // SAFETY: each method is sent with the prototype it is installed with.
        unsafe {
            crate::text::set_field_text(label, text);
            set_alignment(label, objc::sel::get("setAlignment:"), CENTER);
        }
        objc::arc::release(text);
        let sized = intrinsic_size(label).expect("a text field has a size");
        assert!(sized.width > empty.width);
        assert_eq!(sized.height, empty.height);
        assert_eq!(look(label).alignment, CENTER);
        forget(label);
    }
}
