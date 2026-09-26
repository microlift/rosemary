use crate::geometry::{Point, Rect, Size};
use crate::paint::{Canvas, Color};
use crate::text::Font;
use crate::theme::Theme;
use crate::view::View;

/// `NSControlStateValue`, as a menu item shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Off,
    On,
    Mixed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Item {
    pub title: String,
    /// The key equivalent as it is shown, e.g. from [`shortcut_label`].
    pub shortcut: String,
    pub enabled: bool,
    pub state: State,
    pub submenu: bool,
    pub separator: bool,
    pub indent: u8,
}

impl Item {
    pub fn separator() -> Item {
        Item {
            separator: true,
            ..Item::default()
        }
    }
}

/// The panel a menu opens as.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Panel {
    pub items: Vec<Item>,
    /// The row under the pointer or the keyboard.
    pub highlighted: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PanelLayout {
    /// The whole panel; its origin is where it was laid out.
    pub bounds: Rect,
    /// One per item, in order.
    pub rows: Vec<Rect>,
}

/// Between a title and its shortcut.
const SHORTCUT_GAP: f32 = 28.0;
/// Room for a submenu's arrow.
const ARROW_WIDTH: f32 = 14.0;
const INDENT: f32 = 12.0;

impl Panel {
    /// The panel's size, to make a window of before laying it out.
    pub fn size(&self, theme: &Theme) -> Size {
        self.layout(Rect::new(0.0, 0.0, 0.0, 0.0), theme)
            .bounds
            .size()
    }

    /// Whether a click on row `at` would choose it.
    pub fn selectable(&self, at: usize) -> bool {
        self.items
            .get(at)
            .is_some_and(|item| !item.separator && item.enabled)
    }
}

impl View for Panel {
    type Part = usize;
    type Layout = PanelLayout;

    /// Only `bounds`' origin is used: a panel is as large as its items.
    fn layout(&self, bounds: Rect, theme: &Theme) -> PanelLayout {
        let size = theme.panel_text_size;
        let content = match Font::system(400) {
            Some(font) => self
                .items
                .iter()
                .filter(|item| !item.separator)
                .map(|item| {
                    let title = font.measure(&item.title, size) + f32::from(item.indent) * INDENT;
                    let extra = match (item.submenu, item.shortcut.is_empty()) {
                        (true, _) => SHORTCUT_GAP / 2.0 + ARROW_WIDTH,
                        (false, true) => 0.0,
                        (false, false) => SHORTCUT_GAP + font.measure(&item.shortcut, size),
                    };
                    title + extra
                })
                .fold(0.0f32, f32::max),
            None => 0.0,
        };
        let width = (theme.panel_leading + content + theme.panel_trailing)
            .max(theme.panel_min_width)
            .ceil();
        let mut y = bounds.y + theme.panel_padding;
        let rows = self
            .items
            .iter()
            .map(|item| {
                let height = match item.separator {
                    true => theme.panel_separator_height,
                    false => theme.panel_item_height,
                };
                let row = Rect::new(bounds.x, y, width, height);
                y += height;
                row
            })
            .collect();
        PanelLayout {
            bounds: Rect::new(
                bounds.x,
                bounds.y,
                width,
                y + theme.panel_padding - bounds.y,
            ),
            rows,
        }
    }

    fn paint(&self, canvas: &mut Canvas, layout: &PanelLayout, theme: &Theme) {
        let bounds = layout.bounds;
        canvas.fill_rect(bounds, theme.panel_background);
        canvas.stroke_rounded_rect(bounds, 0.0, 1.0 / canvas.scale(), theme.panel_border);
        let Some(font) = Font::system(400) else {
            return;
        };
        let size = theme.panel_text_size;
        for (at, (item, row)) in self.items.iter().zip(&layout.rows).enumerate() {
            if item.separator {
                let hairline = 1.0 / canvas.scale();
                canvas.fill_rect(
                    Rect::new(
                        row.x + 10.0,
                        (row.center().y - hairline / 2.0).round(),
                        row.width - 20.0,
                        hairline,
                    ),
                    theme.panel_separator,
                );
                continue;
            }
            let lit = self.highlighted == Some(at) && (item.enabled || item.submenu);
            if lit {
                canvas.fill_rounded_rect(row.inset(5.0, 0.0), 4.0, theme.panel_selection);
            }
            let color = match (lit, item.enabled) {
                (true, _) => theme.panel_selection_text,
                (false, true) => theme.panel_text,
                (false, false) => theme.panel_text_disabled,
            };
            let baseline = font.centered_baseline(size, row.y, row.height);
            let mark = Point::new(row.x + theme.panel_leading / 2.0 + 1.0, row.center().y);
            match item.state {
                State::Off => {}
                State::On => paint_check(canvas, mark, color),
                State::Mixed => canvas.stroke_line(
                    Point::new(mark.x - 3.5, mark.y),
                    Point::new(mark.x + 3.5, mark.y),
                    1.5,
                    color,
                ),
            }
            let left = row.x + theme.panel_leading + f32::from(item.indent) * INDENT;
            canvas.draw_text(&item.title, &font, size, Point::new(left, baseline), color);
            let right = row.right() - theme.panel_trailing;
            if item.submenu {
                let c = Point::new(right - 3.0, row.center().y);
                canvas.stroke_line(
                    Point::new(c.x - 2.5, c.y - 4.0),
                    Point::new(c.x + 1.5, c.y),
                    1.4,
                    color,
                );
                canvas.stroke_line(
                    Point::new(c.x + 1.5, c.y),
                    Point::new(c.x - 2.5, c.y + 4.0),
                    1.4,
                    color,
                );
            } else if !item.shortcut.is_empty() {
                let width = font.measure(&item.shortcut, size);
                let shortcut_color = match lit {
                    true => color,
                    false => color.with_alpha(color.a * 0.6),
                };
                canvas.draw_text(
                    &item.shortcut,
                    &font,
                    size,
                    Point::new(right - width, baseline),
                    shortcut_color,
                );
            }
        }
    }

    fn hit(&self, layout: &PanelLayout, point: Point) -> Option<usize> {
        layout
            .rows
            .iter()
            .position(|row| row.contains(point))
            .filter(|at| !self.items[*at].separator)
    }
}

fn paint_check(canvas: &mut Canvas, at: Point, color: Color) {
    canvas.stroke_line(
        Point::new(at.x - 4.0, at.y),
        Point::new(at.x - 1.0, at.y + 3.5),
        1.6,
        color,
    );
    canvas.stroke_line(
        Point::new(at.x - 1.5, at.y + 3.5),
        Point::new(at.x + 4.5, at.y - 4.0),
        1.6,
        color,
    );
}

/// Modifier flags a key equivalent can carry, as [`shortcut_label`] reads
/// them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub command: bool,
    pub shift: bool,
    pub option: bool,
    pub control: bool,
}

/// How a Mac menu shows a key equivalent: the modifiers in AppKit's order
/// (control, option, shift, command), then the key.
///
/// An upper-case letter implies shift, as it does in `-keyEquivalent`.
pub fn shortcut_label(key: &str, modifiers: Modifiers) -> String {
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return String::new();
    };
    let shift = modifiers.shift || (c.is_alphabetic() && c.is_uppercase());
    let name = match c {
        '\r' | '\n' | '\u{3}' => "\u{21A9}".to_string(),
        '\u{1b}' => "\u{238B}".to_string(),
        '\t' => "\u{21E5}".to_string(),
        '\u{8}' | '\u{7f}' => "\u{232B}".to_string(),
        ' ' => "Space".to_string(),
        '\u{F700}' => "\u{2191}".to_string(),
        '\u{F701}' => "\u{2193}".to_string(),
        '\u{F702}' => "\u{2190}".to_string(),
        '\u{F703}' => "\u{2192}".to_string(),
        '\u{F728}' => "\u{2326}".to_string(),
        '\u{F729}' => "\u{2196}".to_string(),
        '\u{F72B}' => "\u{2198}".to_string(),
        '\u{F72C}' => "\u{21DE}".to_string(),
        '\u{F72D}' => "\u{21DF}".to_string(),
        // NSF1FunctionKey onwards.
        '\u{F704}'..='\u{F726}' => format!("F{}", c as u32 - 0xF704 + 1),
        other => other.to_uppercase().collect(),
    };
    let mut out = String::new();
    for (on, glyph) in [
        (modifiers.control, '\u{2303}'),
        (modifiers.option, '\u{2325}'),
        (shift, '\u{21E7}'),
        (modifiers.command, '\u{2318}'),
    ] {
        if on {
            out.push(glyph);
        }
    }
    out + &name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str) -> Item {
        Item {
            title: title.to_string(),
            enabled: true,
            ..Item::default()
        }
    }

    #[test]
    fn rows_stack_and_separators_are_not_hit() {
        let theme = Theme::light();
        let panel = Panel {
            items: vec![item("Open"), Item::separator(), item("Close")],
            highlighted: None,
        };
        let layout = panel.layout(Rect::new(0.0, 0.0, 0.0, 0.0), &theme);
        assert_eq!(layout.rows.len(), 3);
        assert_eq!(layout.rows[1].y, layout.rows[0].bottom());
        assert!(layout.bounds.width >= theme.panel_min_width);
        let middle = |row: &Rect| Point::new(row.center().x, row.center().y);
        assert_eq!(panel.hit(&layout, middle(&layout.rows[0])), Some(0));
        assert_eq!(panel.hit(&layout, middle(&layout.rows[1])), None);
        assert_eq!(panel.hit(&layout, middle(&layout.rows[2])), Some(2));
    }

    #[test]
    fn a_disabled_item_is_not_selectable() {
        let panel = Panel {
            items: vec![
                Item {
                    enabled: false,
                    ..item("Undo")
                },
                item("Copy"),
            ],
            highlighted: None,
        };
        assert!(!panel.selectable(0));
        assert!(panel.selectable(1));
        assert!(!panel.selectable(5));
    }

    #[test]
    fn shortcuts_read_as_a_mac_shows_them() {
        let command = Modifiers {
            command: true,
            ..Modifiers::default()
        };
        assert_eq!(shortcut_label("s", command), "\u{2318}S");
        assert_eq!(shortcut_label("S", command), "\u{21E7}\u{2318}S");
        let all = Modifiers {
            command: true,
            shift: true,
            option: true,
            control: true,
        };
        assert_eq!(
            shortcut_label("f", all),
            "\u{2303}\u{2325}\u{21E7}\u{2318}F"
        );
        assert_eq!(shortcut_label("\u{F705}", command), "\u{2318}F2");
        assert_eq!(shortcut_label("", command), "");
    }

    #[test]
    fn painting_leaves_no_hole_in_the_panel() {
        let theme = Theme::light();
        let panel = Panel {
            items: vec![item("New"), item("Open")],
            highlighted: Some(1),
        };
        let layout = panel.layout(Rect::new(0.0, 0.0, 0.0, 0.0), &theme);
        let (width, height) = (layout.bounds.width as usize, layout.bounds.height as usize);
        let mut pixels = vec![0u8; width * height * 4];
        let mut canvas = Canvas::new(&mut pixels, width, height, width * 4, 1.0);
        panel.paint(&mut canvas, &layout, &theme);
        assert!(pixels.chunks(4).all(|texel| texel[3] == 255));
    }
}
