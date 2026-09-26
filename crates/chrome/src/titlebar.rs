use crate::geometry::{Point, Rect};
use crate::paint::{Canvas, Color};
use crate::text::Font;
use crate::theme::Theme;
use crate::view::View;

/// Something in a bar a click can land on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Close,
    Minimize,
    Zoom,
    /// A menu title, by its index in [`Bar::menus`].
    Menu(usize),
    /// The chevron standing in for the menus that did not fit, the first of
    /// which is the index given.
    Overflow(usize),
}

impl Part {
    pub fn is_light(self) -> bool {
        matches!(self, Part::Close | Part::Minimize | Part::Zoom)
    }
}

/// Which of the three window buttons the window has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lights {
    pub closable: bool,
    pub minimizable: bool,
    pub zoomable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuTitle {
    pub title: String,
    pub enabled: bool,
}

/// A titlebar, a menu strip, or a titlebar with the menus in it: the same
/// row with different parts switched on.
///
/// The first menu is the application menu and is drawn bold, as a Mac's
/// menu bar draws it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bar {
    pub title: String,
    pub show_title: bool,
    /// The window buttons, or `None` for a bar without them.
    pub lights: Option<Lights>,
    pub menus: Vec<MenuTitle>,
    /// Whether the window is the key window: an inactive one is drawn grey.
    pub active: bool,
    pub hover: Option<Part>,
    pub pressed: Option<Part>,
    /// The menu whose panel is open.
    pub open: Option<usize>,
    /// A hairline along the bottom edge, between the bar and the content.
    pub separator: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BarLayout {
    pub bounds: Rect,
    pub parts: Vec<(Part, Rect)>,
    /// Where the title's baseline starts, and the title as it fits.
    pub title: Option<(Point, String)>,
}

impl BarLayout {
    pub fn rect_of(&self, part: Part) -> Option<Rect> {
        self.parts
            .iter()
            .find(|(candidate, _)| *candidate == part)
            .map(|(_, rect)| *rect)
    }
}

/// Space kept free after the menus and around the title.
const GAP: f32 = 12.0;
/// The chevron's width.
const OVERFLOW_WIDTH: f32 = 22.0;

fn fonts() -> (Option<std::sync::Arc<Font>>, Option<std::sync::Arc<Font>>) {
    (Font::system(400), Font::system(700))
}

impl Bar {
    fn light_parts(&self, bounds: Rect, theme: &Theme) -> Vec<(Part, Rect)> {
        if self.lights.is_none() {
            return Vec::new();
        }
        let radius = theme.light_diameter / 2.0;
        // A little larger than the circle, so a click just off the edge
        // still lands - but never overlapping the next one.
        let reach = (radius + 3.0).min(theme.light_pitch / 2.0);
        [Part::Close, Part::Minimize, Part::Zoom]
            .into_iter()
            .enumerate()
            .map(|(at, part)| {
                let center = Point::new(
                    bounds.x + theme.light_inset + at as f32 * theme.light_pitch,
                    bounds.center().y,
                );
                (
                    part,
                    Rect::new(center.x - reach, center.y - reach, reach * 2.0, reach * 2.0),
                )
            })
            .collect()
    }

    fn light_enabled(&self, part: Part) -> bool {
        let Some(lights) = self.lights else {
            return false;
        };
        match part {
            Part::Close => lights.closable,
            Part::Minimize => lights.minimizable,
            Part::Zoom => lights.zoomable,
            _ => false,
        }
    }

    fn paint_light(&self, canvas: &mut Canvas, part: Part, rect: Rect, theme: &Theme) {
        let center = rect.center();
        let radius = theme.light_diameter / 2.0;
        let enabled = self.light_enabled(part);
        let group_hovered = self.hover.is_some_and(Part::is_light);
        let colored = enabled && (self.active || group_hovered);
        let mut fill = match (colored, part) {
            (false, _) => theme.light_inactive,
            (true, Part::Close) => theme.close,
            (true, Part::Minimize) => theme.minimize,
            (true, _) => theme.zoom,
        };
        if enabled && self.pressed == Some(part) {
            fill = fill.mix(Color::rgb(0, 0, 0), 0.2);
        }
        canvas.fill_circle(center, radius, fill);
        let ring = Rect::new(
            center.x - radius,
            center.y - radius,
            radius * 2.0,
            radius * 2.0,
        );
        canvas.stroke_rounded_rect(ring, radius, 0.5, theme.light_border);
        if !(enabled && group_hovered) {
            return;
        }
        let glyph = theme.light_glyph;
        let arm = radius * 0.45;
        let width = 1.1;
        match part {
            Part::Close => {
                canvas.stroke_line(
                    Point::new(center.x - arm, center.y - arm),
                    Point::new(center.x + arm, center.y + arm),
                    width,
                    glyph,
                );
                canvas.stroke_line(
                    Point::new(center.x - arm, center.y + arm),
                    Point::new(center.x + arm, center.y - arm),
                    width,
                    glyph,
                );
            }
            Part::Minimize => canvas.stroke_line(
                Point::new(center.x - arm * 1.2, center.y),
                Point::new(center.x + arm * 1.2, center.y),
                width,
                glyph,
            ),
            _ => {
                let arm = arm * 1.2;
                canvas.stroke_line(
                    Point::new(center.x - arm, center.y),
                    Point::new(center.x + arm, center.y),
                    width,
                    glyph,
                );
                canvas.stroke_line(
                    Point::new(center.x, center.y - arm),
                    Point::new(center.x, center.y + arm),
                    width,
                    glyph,
                );
            }
        }
    }
}

impl View for Bar {
    type Part = Part;
    type Layout = BarLayout;

    fn layout(&self, bounds: Rect, theme: &Theme) -> BarLayout {
        let (regular, bold) = fonts();
        let mut parts = self.light_parts(bounds, theme);
        let mut x = match self.lights {
            Some(_) => {
                bounds.x + theme.light_inset + 2.0 * theme.light_pitch + theme.light_diameter
            }
            None => bounds.x + 4.0,
        };
        if let (Some(regular), Some(bold)) = (&regular, &bold) {
            let right = bounds.right() - GAP;
            let widths: Vec<f32> = self
                .menus
                .iter()
                .enumerate()
                .map(|(at, menu)| {
                    let font = if at == 0 { bold } else { regular };
                    font.measure(&menu.title, theme.menu_title_size)
                        + 2.0 * theme.menu_title_padding
                })
                .collect();
            for (at, width) in widths.iter().enumerate() {
                let rest: f32 = widths[at..].iter().sum();
                let fits_with_rest = x + rest <= right;
                // Once the rest cannot all fit, keep room for the chevron.
                let limit = match fits_with_rest {
                    true => right,
                    false => right - OVERFLOW_WIDTH,
                };
                if x + width > limit {
                    parts.push((
                        Part::Overflow(at),
                        Rect::new(x, bounds.y, OVERFLOW_WIDTH, bounds.height),
                    ));
                    x += OVERFLOW_WIDTH;
                    break;
                }
                parts.push((
                    Part::Menu(at),
                    Rect::new(x, bounds.y, *width, bounds.height),
                ));
                x += width;
            }
        }
        let title = match (&regular, self.show_title && !self.title.is_empty()) {
            (Some(font), true) => {
                let size = theme.title_size;
                let left = match self.menus.is_empty() && self.lights.is_none() {
                    true => bounds.x + GAP,
                    false => x + GAP,
                };
                let right = bounds.right() - GAP;
                let width = font.measure(&self.title, size);
                // Centred on the window when that clears everything to its
                // left, as a Mac centres it; pushed right when it does not.
                let centered = bounds.center().x - width / 2.0;
                let start = centered.max(left);
                let shown = font.truncate(&self.title, size, right - start);
                (!shown.is_empty()).then(|| {
                    let baseline = font.centered_baseline(size, bounds.y, bounds.height);
                    (Point::new(start.round(), baseline), shown)
                })
            }
            _ => None,
        };
        BarLayout {
            bounds,
            parts,
            title,
        }
    }

    fn paint(&self, canvas: &mut Canvas, layout: &BarLayout, theme: &Theme) {
        let bounds = layout.bounds;
        match self.active {
            true => {
                canvas.fill_vertical_gradient(bounds, theme.titlebar_top, theme.titlebar_bottom)
            }
            false => canvas.fill_rect(bounds, theme.titlebar_inactive),
        }
        if self.separator {
            let hairline = 1.0 / canvas.scale();
            canvas.fill_rect(
                Rect::new(bounds.x, bounds.bottom() - hairline, bounds.width, hairline),
                theme.separator,
            );
        }
        let (regular, bold) = fonts();
        for (part, rect) in &layout.parts {
            match *part {
                Part::Close | Part::Minimize | Part::Zoom => {
                    self.paint_light(canvas, *part, *rect, theme)
                }
                Part::Menu(at) => {
                    let Some(menu) = self.menus.get(at) else {
                        continue;
                    };
                    let font = match at {
                        0 => &bold,
                        _ => &regular,
                    };
                    let Some(font) = font else {
                        continue;
                    };
                    let lit = self.open == Some(at) || self.pressed == Some(*part);
                    if lit || (self.hover == Some(*part) && self.open.is_some()) {
                        let pill = rect.inset(0.0, 4.0);
                        canvas.fill_rounded_rect(pill, 4.0, theme.menu_title_highlight);
                    }
                    let color = match menu.enabled && self.active {
                        true => theme.menu_title,
                        false => theme.menu_title_disabled,
                    };
                    let size = theme.menu_title_size;
                    let baseline = font.centered_baseline(size, rect.y, rect.height);
                    canvas.draw_text(
                        &menu.title,
                        font,
                        size,
                        Point::new(rect.x + theme.menu_title_padding, baseline),
                        color,
                    );
                }
                Part::Overflow(_) => {
                    if self.pressed == Some(*part) {
                        canvas.fill_rounded_rect(
                            rect.inset(0.0, 4.0),
                            4.0,
                            theme.menu_title_highlight,
                        );
                    }
                    let c = rect.center();
                    for dx in [-3.0, 2.0] {
                        let x = c.x + dx;
                        canvas.stroke_line(
                            Point::new(x - 2.0, c.y - 3.5),
                            Point::new(x + 1.5, c.y),
                            1.2,
                            theme.menu_title,
                        );
                        canvas.stroke_line(
                            Point::new(x + 1.5, c.y),
                            Point::new(x - 2.0, c.y + 3.5),
                            1.2,
                            theme.menu_title,
                        );
                    }
                }
            }
        }
        if let (Some((origin, title)), Some(font)) = (&layout.title, &regular) {
            let color = match self.active {
                true => theme.title,
                false => theme.title_inactive,
            };
            canvas.draw_text(title, font, theme.title_size, *origin, color);
        }
    }

    fn hit(&self, layout: &BarLayout, point: Point) -> Option<Part> {
        layout
            .parts
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(part, _)| *part)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(names: &[&str]) -> Vec<MenuTitle> {
        names
            .iter()
            .map(|title| MenuTitle {
                title: title.to_string(),
                enabled: true,
            })
            .collect()
    }

    fn bar() -> Bar {
        Bar {
            title: "Untitled".into(),
            show_title: true,
            lights: Some(Lights {
                closable: true,
                minimizable: true,
                zoomable: true,
            }),
            active: true,
            ..Bar::default()
        }
    }

    #[test]
    fn the_lights_are_hit_where_they_are_drawn() {
        let theme = Theme::light();
        let bar = bar();
        let layout = bar.layout(Rect::new(0.0, 0.0, 600.0, 28.0), &theme);
        let at = |x| bar.hit(&layout, Point::new(x, 14.0));
        assert_eq!(at(theme.light_inset), Some(Part::Close));
        assert_eq!(
            at(theme.light_inset + theme.light_pitch),
            Some(Part::Minimize)
        );
        assert_eq!(
            at(theme.light_inset + 2.0 * theme.light_pitch),
            Some(Part::Zoom)
        );
        assert_eq!(at(300.0), None, "the title is somewhere to drag from");
    }

    #[test]
    fn menus_follow_the_lights_and_do_not_overlap() {
        if Font::system(400).is_none() {
            return;
        }
        let theme = Theme::light();
        let bar = Bar {
            menus: titles(&["App", "File", "Edit", "View"]),
            ..bar()
        };
        let layout = bar.layout(Rect::new(0.0, 0.0, 800.0, 28.0), &theme);
        let zoom = layout.rect_of(Part::Zoom).expect("a zoom button");
        let rects: Vec<Rect> = (0..4)
            .map(|at| layout.rect_of(Part::Menu(at)).expect("every menu fits"))
            .collect();
        assert!(rects[0].x >= zoom.right());
        for pair in rects.windows(2) {
            assert!(pair[1].x >= pair[0].right());
        }
        let (origin, _) = layout.title.as_ref().expect("room for the title");
        assert!(
            origin.x >= rects[3].right(),
            "the title yields to the menus"
        );
    }

    #[test]
    fn menus_that_do_not_fit_collapse_into_a_chevron() {
        if Font::system(400).is_none() {
            return;
        }
        let theme = Theme::light();
        let bar = Bar {
            menus: titles(&["Application", "File", "Edit", "Format", "View", "Window"]),
            ..bar()
        };
        let layout = bar.layout(Rect::new(0.0, 0.0, 260.0, 28.0), &theme);
        let overflow = layout
            .parts
            .iter()
            .find_map(|(part, rect)| match part {
                Part::Overflow(first) => Some((*first, *rect)),
                _ => None,
            })
            .expect("a chevron");
        assert!(overflow.1.right() <= 260.0);
        assert!(layout.rect_of(Part::Menu(overflow.0)).is_none());
    }

    #[test]
    fn a_strip_has_no_lights_and_no_title() {
        let theme = Theme::light();
        let strip = Bar {
            menus: titles(&["App", "File"]),
            ..Bar::default()
        };
        let layout = strip.layout(Rect::new(0.0, 0.0, 400.0, 24.0), &theme);
        assert!(layout.parts.iter().all(|(part, _)| !part.is_light()));
        assert!(layout.title.is_none());
    }

    #[test]
    fn painting_fills_the_whole_bar() {
        let theme = Theme::light();
        let bar = bar();
        let (width, height) = (200, 28);
        let mut pixels = vec![0u8; width * height * 4];
        let mut canvas = Canvas::new(&mut pixels, width, height, width * 4, 1.0);
        let layout = bar.layout(Rect::new(0.0, 0.0, 200.0, 28.0), &theme);
        bar.paint(&mut canvas, &layout, &theme);
        assert!(pixels.chunks(4).all(|texel| texel[3] == 255));
    }
}
