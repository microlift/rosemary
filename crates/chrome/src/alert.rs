//! A modal alert, after a current Mac's: the icon, the message in bold and the
//! informative text under it, all centred, then the buttons.

use crate::geometry::{Point, Rect};
use crate::paint::{Canvas, Image};
use crate::text::Font;
use crate::theme::Theme;
use crate::view::View;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Alert {
    pub message: String,
    pub informative: String,
    /// The first is the default, answered by Return.
    pub buttons: Vec<String>,
    pub icon: Option<Image>,
    /// The button being pressed, drawn darker.
    pub pressed: Option<usize>,
    /// Whether the window has room around the corners for them to be round.
    pub rounded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Button(usize),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line {
    pub text: String,
    /// The left end of the baseline.
    pub origin: Point,
    pub bold: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AlertLayout {
    pub bounds: Rect,
    pub icon: Option<Rect>,
    pub lines: Vec<Line>,
    /// One per button, in the order they were given.
    pub buttons: Vec<Rect>,
}

const TOP: f32 = 20.0;
const SIDE: f32 = 16.0;
const BOTTOM: f32 = 16.0;
const AFTER_ICON: f32 = 12.0;
const BEFORE_INFORMATIVE: f32 = 6.0;
const BEFORE_BUTTONS: f32 = 20.0;
const BUTTON_GAP: f32 = 8.0;
const BUTTON_RADIUS: f32 = 6.0;
/// Room a button's title needs on each side before the buttons stack.
const TITLE_PADDING: f32 = 12.0;
/// Beyond this the rest of the text is cut, so an alert still fits on the
/// screen with its buttons.
const MAX_LINES: usize = 40;

impl Alert {
    /// The button Escape answers: the one titled Cancel, as in `NSAlert`.
    pub fn cancel_button(&self) -> Option<usize> {
        self.buttons
            .iter()
            .position(|title| title.eq_ignore_ascii_case("cancel"))
    }
}

/// Up to `limit` of `lines`, the last marked with an ellipsis when some
/// were cut.
fn cap(mut lines: Vec<String>, limit: usize) -> Vec<String> {
    if lines.len() > limit {
        lines.truncate(limit);
        if let Some(last) = lines.last_mut() {
            last.push('\u{2026}');
        }
    }
    lines
}

impl View for Alert {
    type Part = Part;
    type Layout = AlertLayout;

    /// Only `bounds`' origin is used: an alert is as tall as what it says.
    fn layout(&self, bounds: Rect, theme: &Theme) -> AlertLayout {
        let width = theme.alert_width;
        let text_width = width - 2.0 * SIDE;
        let (x, mut y) = (bounds.x, bounds.y + TOP);

        let icon = self.icon.as_ref().map(|_| {
            let size = theme.alert_icon_size;
            let rect = Rect::new(x + (width - size) / 2.0, y, size, size);
            y += size + AFTER_ICON;
            rect
        });

        let mut lines = Vec::new();
        let mut text =
            |font: Option<std::sync::Arc<Font>>, body: &str, size: f32, bold, y: &mut f32| {
                let Some(font) = font else {
                    return;
                };
                let height = (size * 1.25).round();
                for line in cap(font.wrap(body, size, text_width), MAX_LINES) {
                    let line = line.trim().to_string();
                    let advance = font.measure(&line, size);
                    let baseline = font.centered_baseline(size, *y, height);
                    lines.push(Line {
                        origin: Point::new((x + (width - advance) / 2.0).round(), baseline),
                        text: line,
                        bold,
                    });
                    *y += height;
                }
            };
        if !self.message.is_empty() {
            let size = theme.alert_message_size;
            text(Font::system(700), &self.message, size, true, &mut y);
        }
        if !self.informative.is_empty() {
            if !self.message.is_empty() {
                y += BEFORE_INFORMATIVE;
            }
            let size = theme.alert_informative_size;
            text(Font::system(400), &self.informative, size, false, &mut y);
        }
        y += BEFORE_BUTTONS;

        // Two buttons share a row, the default on the right, when both titles
        // fit; otherwise each has a row, the default on top.
        let half = (text_width - BUTTON_GAP) / 2.0;
        let fits = Font::system(400).is_some_and(|font| {
            self.buttons.iter().all(|title| {
                font.measure(title, theme.alert_message_size) + 2.0 * TITLE_PADDING <= half
            })
        });
        let height = theme.alert_button_height;
        let buttons = match (self.buttons.len(), fits) {
            (0, _) => Vec::new(),
            (1, _) => vec![Rect::new(x + SIDE, y, text_width, height)],
            (2, true) => vec![
                Rect::new(x + SIDE + half + BUTTON_GAP, y, half, height),
                Rect::new(x + SIDE, y, half, height),
            ],
            _ => (0..self.buttons.len())
                .map(|at| {
                    let top = y + at as f32 * (height + BUTTON_GAP);
                    Rect::new(x + SIDE, top, text_width, height)
                })
                .collect(),
        };
        let bottom = buttons
            .iter()
            .map(Rect::bottom)
            .fold(y - BEFORE_BUTTONS, f32::max);

        AlertLayout {
            bounds: Rect::new(x, bounds.y, width, (bottom + BOTTOM - bounds.y).ceil()),
            icon,
            lines,
            buttons,
        }
    }

    fn paint(&self, canvas: &mut Canvas, layout: &AlertLayout, theme: &Theme) {
        let radius = match self.rounded {
            true => theme.alert_corner_radius,
            false => 0.0,
        };
        let bounds = layout.bounds;
        canvas.fill_rounded_rect(bounds, radius, theme.alert_background);
        canvas.stroke_rounded_rect(bounds, radius, 1.0 / canvas.scale(), theme.alert_border);

        if let (Some(image), Some(rect)) = (&self.icon, layout.icon) {
            canvas.draw_image(image, rect);
        }

        for line in &layout.lines {
            let (weight, size) = match line.bold {
                true => (700, theme.alert_message_size),
                false => (400, theme.alert_informative_size),
            };
            if let Some(font) = Font::system(weight) {
                canvas.draw_text(&line.text, &font, size, line.origin, theme.alert_text);
            }
        }

        let Some(font) = Font::system(400) else {
            return;
        };
        let size = theme.alert_message_size;
        for (at, (title, rect)) in self.buttons.iter().zip(&layout.buttons).enumerate() {
            let pressed = self.pressed == Some(at);
            let (fill, text) = match (at == 0, pressed) {
                (true, false) => (theme.alert_default, theme.alert_default_text),
                (true, true) => (theme.alert_default_pressed, theme.alert_default_text),
                (false, false) => (theme.alert_button, theme.alert_button_text),
                (false, true) => (theme.alert_button_pressed, theme.alert_button_text),
            };
            canvas.fill_rounded_rect(*rect, BUTTON_RADIUS, fill);
            let title = font.truncate(title, size, rect.width - 2.0 * TITLE_PADDING);
            let advance = font.measure(&title, size);
            let origin = Point::new(
                (rect.center().x - advance / 2.0).round(),
                font.centered_baseline(size, rect.y, rect.height),
            );
            canvas.draw_text(&title, &font, size, origin, text);
        }
    }

    fn hit(&self, layout: &AlertLayout, point: Point) -> Option<Part> {
        layout
            .buttons
            .iter()
            .position(|rect| rect.contains(point))
            .map(Part::Button)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(buttons: &[&str]) -> Alert {
        Alert {
            message: "Delete the file?".into(),
            informative: "This cannot be undone.".into(),
            buttons: buttons.iter().map(|title| title.to_string()).collect(),
            ..Alert::default()
        }
    }

    fn laid_out(alert: &Alert) -> AlertLayout {
        alert.layout(Rect::default(), &Theme::light())
    }

    #[test]
    fn two_short_buttons_share_a_row_with_the_default_on_the_right() {
        if Font::system(400).is_none() {
            return;
        }
        let layout = laid_out(&alert(&["Delete", "Cancel"]));
        let [default, other] = layout.buttons[..] else {
            panic!("two buttons");
        };
        assert_eq!(default.y, other.y);
        assert!(default.x > other.x);
    }

    #[test]
    fn three_buttons_stack_with_the_default_on_top() {
        let layout = laid_out(&alert(&["Save", "Don't Save", "Cancel"]));
        assert_eq!(layout.buttons.len(), 3);
        assert!(
            layout
                .buttons
                .windows(2)
                .all(|pair| pair[0].bottom() < pair[1].y)
        );
        assert!(layout.bounds.bottom() > layout.buttons[2].bottom());
    }

    #[test]
    fn a_button_is_hit_and_the_rest_is_not() {
        let alert = alert(&["OK"]);
        let layout = laid_out(&alert);
        let button = layout.buttons[0];
        assert_eq!(alert.hit(&layout, button.center()), Some(Part::Button(0)));
        assert_eq!(alert.hit(&layout, Point::new(1.0, 1.0)), None);
    }

    #[test]
    fn escape_answers_the_button_titled_cancel() {
        assert_eq!(alert(&["OK", "Cancel"]).cancel_button(), Some(1));
        assert_eq!(alert(&["OK"]).cancel_button(), None);
    }

    #[test]
    fn an_icon_pushes_the_text_down() {
        let plain = laid_out(&alert(&["OK"]));
        let with_icon = laid_out(&Alert {
            icon: Some(Image {
                width: 1,
                height: 1,
                pixels: vec![0; 4],
            }),
            ..alert(&["OK"])
        });
        assert!(with_icon.icon.is_some());
        assert!(with_icon.bounds.height > plain.bounds.height);
    }

    #[test]
    fn painting_covers_the_whole_square_alert() {
        let alert = Alert {
            pressed: Some(0),
            ..alert(&["OK", "Cancel"])
        };
        let theme = Theme::dark();
        let layout = alert.layout(Rect::default(), &theme);
        let (width, height) = (layout.bounds.width as usize, layout.bounds.height as usize);
        let mut pixels = vec![0u8; width * height * 4];
        let mut canvas = Canvas::new(&mut pixels, width, height, width * 4, 1.0);
        alert.paint(&mut canvas, &layout, &theme);
        assert!(pixels.chunks(4).all(|texel| texel[3] == 255));
    }
}
