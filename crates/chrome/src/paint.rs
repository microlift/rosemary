use crate::geometry::{Point, Rect, Size};
use crate::text::Font;

/// A colour with straight (not premultiplied) alpha.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color::rgba(r, g, b, 1.0)
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Self {
        Color {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a,
        }
    }

    pub const fn with_alpha(self, a: f32) -> Self {
        Color { a, ..self }
    }

    /// Straight-line blend towards `other`, for gradients.
    pub fn mix(self, other: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let lerp = |a: f32, b: f32| a + (b - a) * t;
        Color {
            r: lerp(self.r, other.r),
            g: lerp(self.g, other.g),
            b: lerp(self.b, other.b),
            a: lerp(self.a, other.a),
        }
    }
}

/// An image with straight R, G, B, A pixels, top row first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

/// An outline in points, filled with the non-zero rule.
#[derive(Clone, Debug, Default)]
pub struct Path {
    segments: Vec<Segment>,
    start: Point,
    at: Point,
}

#[derive(Clone, Copy, Debug)]
enum Segment {
    Line(Point, Point),
    Quad(Point, Point, Point),
    Cubic(Point, Point, Point, Point),
}

/// A circle quadrant as one cubic is within 0.03% of the arc.
const KAPPA: f32 = 0.552_284_8;

impl Path {
    pub fn new() -> Self {
        Path::default()
    }

    pub fn move_to(&mut self, point: Point) {
        self.close();
        self.start = point;
        self.at = point;
    }

    pub fn line_to(&mut self, point: Point) {
        self.segments.push(Segment::Line(self.at, point));
        self.at = point;
    }

    pub fn quad_to(&mut self, control: Point, point: Point) {
        self.segments.push(Segment::Quad(self.at, control, point));
        self.at = point;
    }

    pub fn cubic_to(&mut self, a: Point, b: Point, point: Point) {
        self.segments.push(Segment::Cubic(self.at, a, b, point));
        self.at = point;
    }

    pub fn close(&mut self) {
        if self.at != self.start {
            self.segments.push(Segment::Line(self.at, self.start));
        }
        self.at = self.start;
    }

    /// A rounded rectangle, clockwise; `reverse` winds it the other way so
    /// that it cuts a hole out of one wound clockwise.
    pub fn rounded_rect(rect: Rect, radius: f32, reverse: bool) -> Path {
        let r = radius.clamp(0.0, rect.width.min(rect.height) / 2.0);
        let k = r * (1.0 - KAPPA);
        let (l, t, rt, b) = (rect.x, rect.y, rect.right(), rect.bottom());
        let mut path = Path::new();
        let p = Point::new;
        path.move_to(p(l + r, t));
        path.line_to(p(rt - r, t));
        path.cubic_to(p(rt - k, t), p(rt, t + k), p(rt, t + r));
        path.line_to(p(rt, b - r));
        path.cubic_to(p(rt, b - k), p(rt - k, b), p(rt - r, b));
        path.line_to(p(l + r, b));
        path.cubic_to(p(l + k, b), p(l, b - k), p(l, b - r));
        path.line_to(p(l, t + r));
        path.cubic_to(p(l, t + k), p(l + k, t), p(l + r, t));
        path.close();
        if reverse {
            path.reverse();
        }
        path
    }

    pub fn circle(center: Point, radius: f32) -> Path {
        let rect = Rect::new(
            center.x - radius,
            center.y - radius,
            radius * 2.0,
            radius * 2.0,
        );
        Path::rounded_rect(rect, radius, false)
    }

    /// A line of `width` from `a` to `b`, with square-cut ends.
    pub fn line(a: Point, b: Point, width: f32) -> Path {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let length = (dx * dx + dy * dy).sqrt();
        let mut path = Path::new();
        if length <= f32::EPSILON {
            return path;
        }
        let (nx, ny) = (-dy / length * width / 2.0, dx / length * width / 2.0);
        path.move_to(Point::new(a.x + nx, a.y + ny));
        path.line_to(Point::new(b.x + nx, b.y + ny));
        path.line_to(Point::new(b.x - nx, b.y - ny));
        path.line_to(Point::new(a.x - nx, a.y - ny));
        path.close();
        path
    }

    /// Append `other`, so that one fill covers both.
    pub fn extend(&mut self, other: &Path) {
        self.close();
        self.segments.extend_from_slice(&other.segments);
    }

    fn reverse(&mut self) {
        self.segments.reverse();
        for segment in &mut self.segments {
            *segment = match *segment {
                Segment::Line(a, b) => Segment::Line(b, a),
                Segment::Quad(a, c, b) => Segment::Quad(b, c, a),
                Segment::Cubic(a, c, d, b) => Segment::Cubic(b, d, c, a),
            };
        }
    }
}

/// A pixel rectangle, top-left origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Area {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Area {
    fn intersection(self, other: Area) -> Area {
        Area {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        }
    }

    fn is_empty(self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }
}

/// Paths wider or taller than this are not rasterised: the rasteriser
/// allocates the whole bounding box, and no chrome shape comes close.
const MAX_PATH_EXTENT: f32 = 8192.0;

/// Draws into premultiplied B, G, R, A pixels, top row first, which is the
/// layout AppKit's backing stores and SDL surfaces share.
///
/// Every coordinate a caller passes is in points; `scale` is pixels per
/// point.
pub struct Canvas<'a> {
    pixels: &'a mut [u8],
    width: usize,
    height: usize,
    stride: usize,
    scale: f32,
    clips: Vec<Area>,
}

impl<'a> Canvas<'a> {
    /// `stride` is in bytes. Panics when the pixels are too short for it.
    pub fn new(
        pixels: &'a mut [u8],
        width: usize,
        height: usize,
        stride: usize,
        scale: f32,
    ) -> Self {
        assert!(stride >= width * 4, "a row is at least its pixels");
        assert!(
            height == 0 || pixels.len() >= stride * (height - 1) + width * 4,
            "the pixels cover every row"
        );
        let whole = Area {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        Canvas {
            pixels,
            width,
            height,
            stride,
            scale: if scale > 0.0 { scale } else { 1.0 },
            clips: vec![whole],
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// The canvas's extent in points.
    pub fn size(&self) -> Size {
        Size::new(
            self.width as f32 / self.scale,
            self.height as f32 / self.scale,
        )
    }

    /// Restrict drawing to `rect` as well as to the current clip, until the
    /// matching [`Canvas::pop_clip`].
    pub fn push_clip(&mut self, rect: Rect) {
        let s = self.scale;
        let area = Area {
            left: (rect.x * s).floor() as i32,
            top: (rect.y * s).floor() as i32,
            right: (rect.right() * s).ceil() as i32,
            bottom: (rect.bottom() * s).ceil() as i32,
        };
        let clip = self.clip().intersection(area);
        self.clips.push(clip);
    }

    pub fn pop_clip(&mut self) {
        if self.clips.len() > 1 {
            self.clips.pop();
        }
    }

    fn clip(&self) -> Area {
        *self
            .clips
            .last()
            .expect("the whole canvas is always a clip")
    }

    /// Replace every pixel in the clip, rather than blending over it.
    pub fn clear(&mut self, color: Color) {
        let texel = premultiply(color, 1.0);
        let clip = self.clip();
        for y in clip.top..clip.bottom {
            let row = y as usize * self.stride;
            for x in clip.left..clip.right {
                let at = row + x as usize * 4;
                self.pixels[at..at + 4].copy_from_slice(&texel);
            }
        }
    }

    /// Fill an axis-aligned rectangle, antialiasing its edges.
    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        self.fill_rect_with(rect, |_| color);
    }

    /// Fill `rect` from `top` at its top edge to `bottom` at its bottom edge.
    pub fn fill_vertical_gradient(&mut self, rect: Rect, top: Color, bottom: Color) {
        let s = self.scale;
        let (start, extent) = (rect.y * s, (rect.height * s).max(1.0));
        self.fill_rect_with(rect, |y| top.mix(bottom, (y as f32 + 0.5 - start) / extent));
    }

    fn fill_rect_with(&mut self, rect: Rect, color_of_row: impl Fn(i32) -> Color) {
        let s = self.scale;
        let (left, top, right, bottom) =
            (rect.x * s, rect.y * s, rect.right() * s, rect.bottom() * s);
        let area = Area {
            left: left.floor() as i32,
            top: top.floor() as i32,
            right: right.ceil() as i32,
            bottom: bottom.ceil() as i32,
        }
        .intersection(self.clip());
        if area.is_empty() {
            return;
        }
        let overlap = |lo: f32, hi: f32, at: i32| {
            let (a, b) = (at as f32, at as f32 + 1.0);
            (hi.min(b) - lo.max(a)).clamp(0.0, 1.0)
        };
        for y in area.top..area.bottom {
            let cover_y = overlap(top, bottom, y);
            let color = color_of_row(y);
            for x in area.left..area.right {
                let cover = cover_y * overlap(left, right, x);
                self.blend(x, y, color, cover);
            }
        }
    }

    pub fn fill_rounded_rect(&mut self, rect: Rect, radius: f32, color: Color) {
        self.fill_path(&Path::rounded_rect(rect, radius, false), color);
    }

    /// A ring `width` wide just inside `rect`'s edge.
    pub fn stroke_rounded_rect(&mut self, rect: Rect, radius: f32, width: f32, color: Color) {
        let mut path = Path::rounded_rect(rect, radius, false);
        let inner = rect.inset(width, width);
        if !inner.is_empty() {
            path.extend(&Path::rounded_rect(inner, (radius - width).max(0.0), true));
        }
        self.fill_path(&path, color);
    }

    pub fn fill_circle(&mut self, center: Point, radius: f32, color: Color) {
        self.fill_path(&Path::circle(center, radius), color);
    }

    pub fn stroke_line(&mut self, a: Point, b: Point, width: f32, color: Color) {
        self.fill_path(&Path::line(a, b, width), color);
    }

    pub fn fill_path(&mut self, path: &Path, color: Color) {
        let s = self.scale;
        let scaled = |p: Point| Point::new(p.x * s, p.y * s);
        let device: Vec<Segment> = path
            .segments
            .iter()
            .map(|segment| match *segment {
                Segment::Line(a, b) => Segment::Line(scaled(a), scaled(b)),
                Segment::Quad(a, b, c) => Segment::Quad(scaled(a), scaled(b), scaled(c)),
                Segment::Cubic(a, b, c, d) => {
                    Segment::Cubic(scaled(a), scaled(b), scaled(c), scaled(d))
                }
            })
            .collect();
        self.fill_device(&device, color);
    }

    /// Fill segments already in device pixels.
    fn fill_device(&mut self, segments: &[Segment], color: Color) {
        let mut bounds = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        let mut grow = |p: &Point| {
            bounds = (
                bounds.0.min(p.x),
                bounds.1.min(p.y),
                bounds.2.max(p.x),
                bounds.3.max(p.y),
            );
        };
        for segment in segments {
            match segment {
                Segment::Line(a, b) => [a, b].into_iter().for_each(&mut grow),
                Segment::Quad(a, b, c) => [a, b, c].into_iter().for_each(&mut grow),
                Segment::Cubic(a, b, c, d) => [a, b, c, d].into_iter().for_each(&mut grow),
            }
        }
        let (min_x, min_y, max_x, max_y) = bounds;
        if segments.is_empty()
            || !(min_x.is_finite() && max_y.is_finite())
            || max_x - min_x > MAX_PATH_EXTENT
            || max_y - min_y > MAX_PATH_EXTENT
        {
            return;
        }
        let path_area = Area {
            left: min_x.floor() as i32,
            top: min_y.floor() as i32,
            right: max_x.ceil() as i32,
            bottom: max_y.ceil() as i32,
        };
        if path_area.intersection(self.clip()).is_empty() {
            return;
        }
        // The rasteriser spills a line's coverage one pixel to the right of
        // it, so the right edge needs room to land in.
        let width = (path_area.right - path_area.left).max(1) as usize + 2;
        let height = (path_area.bottom - path_area.top).max(1) as usize;
        let mut rasterizer = ab_glyph_rasterizer::Rasterizer::new(width, height);
        let (dx, dy) = (path_area.left as f32, path_area.top as f32);
        let local = |p: Point| ab_glyph_rasterizer::point(p.x - dx, p.y - dy);
        for segment in segments {
            match *segment {
                Segment::Line(a, b) => rasterizer.draw_line(local(a), local(b)),
                Segment::Quad(a, b, c) => rasterizer.draw_quad(local(a), local(b), local(c)),
                Segment::Cubic(a, b, c, d) => {
                    rasterizer.draw_cubic(local(a), local(b), local(c), local(d))
                }
            }
        }
        let clip = self.clip();
        rasterizer.for_each_pixel_2d(|x, y, coverage| {
            let (x, y) = (x as i32 + path_area.left, y as i32 + path_area.top);
            if x >= clip.left && x < clip.right && y >= clip.top && y < clip.bottom {
                self.blend(x, y, color, coverage);
            }
        });
    }

    /// Draw one line of text with its baseline's left end at `origin`, and
    /// answer how far it advanced.
    pub fn draw_text(
        &mut self,
        text: &str,
        font: &Font,
        size: f32,
        origin: Point,
        color: Color,
    ) -> f32 {
        let s = self.scale;
        let mut pen = origin.x;
        for glyph in font.glyphs(text, size) {
            let x0 = pen;
            let segments: Vec<Segment> = glyph
                .outline
                .iter()
                .map(|segment| {
                    // Font units are y-up; the canvas is y-down.
                    let map = |(x, y): (f32, f32)| {
                        Point::new((x0 + x * glyph.scale) * s, (origin.y - y * glyph.scale) * s)
                    };
                    match *segment {
                        crate::fonts::Segment::Line(a, b) => Segment::Line(map(a), map(b)),
                        crate::fonts::Segment::Quad(a, b, c) => {
                            Segment::Quad(map(a), map(b), map(c))
                        }
                        crate::fonts::Segment::Cubic(a, b, c, d) => {
                            Segment::Cubic(map(a), map(b), map(c), map(d))
                        }
                    }
                })
                .collect();
            self.fill_device(&segments, color);
            pen += glyph.advance;
        }
        pen - origin.x
    }

    /// Draw `image` scaled into `rect`, averaging the source pixels under
    /// each device pixel: an icon is usually far larger than it is shown.
    pub fn draw_image(&mut self, image: &Image, rect: Rect) {
        if image.width == 0
            || image.height == 0
            || image.pixels.len() < image.width * image.height * 4
        {
            return;
        }
        let s = self.scale;
        let area = Area {
            left: (rect.x * s).round() as i32,
            top: (rect.y * s).round() as i32,
            right: (rect.right() * s).round() as i32,
            bottom: (rect.bottom() * s).round() as i32,
        };
        let clip = area.intersection(self.clip());
        if area.is_empty() || clip.is_empty() {
            return;
        }
        let across = image.width as f32 / (area.right - area.left) as f32;
        let down = image.height as f32 / (area.bottom - area.top) as f32;
        let span = |start: f32, step: f32, limit: usize| {
            let first = (start.floor() as usize).min(limit - 1);
            let last = ((start + step).ceil() as usize).clamp(first + 1, limit);
            first..last
        };
        for y in clip.top..clip.bottom {
            let rows = span((y - area.top) as f32 * down, down, image.height);
            for x in clip.left..clip.right {
                let columns = span((x - area.left) as f32 * across, across, image.width);
                // Averaged premultiplied, so a transparent pixel's colour
                // does not bleed into its neighbours.
                let mut sum = [0.0f32; 4];
                let mut count = 0.0;
                for row in rows.clone() {
                    for column in columns.clone() {
                        let at = (row * image.width + column) * 4;
                        let texel = &image.pixels[at..at + 4];
                        let alpha = f32::from(texel[3]) / 255.0;
                        for channel in 0..3 {
                            sum[channel] += f32::from(texel[channel]) / 255.0 * alpha;
                        }
                        sum[3] += alpha;
                        count += 1.0;
                    }
                }
                let alpha = sum[3] / count;
                if alpha <= 0.0 {
                    continue;
                }
                let straight = |v: f32| v / count / alpha;
                let color = Color {
                    r: straight(sum[0]),
                    g: straight(sum[1]),
                    b: straight(sum[2]),
                    a: alpha,
                };
                self.blend(x, y, color, 1.0);
            }
        }
    }

    fn blend(&mut self, x: i32, y: i32, color: Color, coverage: f32) {
        let alpha = (color.a * coverage).clamp(0.0, 1.0);
        if alpha <= 0.0 || x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return;
        }
        let at = y as usize * self.stride + x as usize * 4;
        let texel = &mut self.pixels[at..at + 4];
        let source = premultiply(color, coverage);
        if alpha >= 1.0 {
            texel.copy_from_slice(&source);
            return;
        }
        let keep = 1.0 - alpha;
        for channel in 0..4 {
            let blended = f32::from(source[channel]) + f32::from(texel[channel]) * keep;
            texel[channel] = blended.round().min(255.0) as u8;
        }
    }
}

fn premultiply(color: Color, coverage: f32) -> [u8; 4] {
    let a = (color.a * coverage).clamp(0.0, 1.0);
    let channel = |v: f32| (v.clamp(0.0, 1.0) * a * 255.0).round() as u8;
    [
        channel(color.b),
        channel(color.g),
        channel(color.r),
        (a * 255.0).round() as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_is_averaged_down_into_its_rectangle() {
        // Two opaque red pixels and two transparent ones, into one pixel.
        let image = Image {
            width: 2,
            height: 2,
            pixels: vec![255, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 255, 0, 0, 0, 0],
        };
        let mut pixels = vec![0u8; 4];
        let mut canvas = Canvas::new(&mut pixels, 1, 1, 4, 1.0);
        canvas.draw_image(&image, Rect::new(0.0, 0.0, 1.0, 1.0));
        // Half-covered pure red, premultiplied B, G, R, A.
        assert_eq!(pixels, [0, 0, 128, 128]);
    }

    fn canvas_pixels(width: usize, height: usize) -> Vec<u8> {
        vec![0; width * height * 4]
    }

    fn alpha_at(pixels: &[u8], width: usize, x: usize, y: usize) -> u8 {
        pixels[(y * width + x) * 4 + 3]
    }

    #[test]
    fn fills_a_rect_in_premultiplied_bgra() {
        let mut pixels = canvas_pixels(4, 4);
        let mut canvas = Canvas::new(&mut pixels, 4, 4, 16, 1.0);
        canvas.fill_rect(Rect::new(1.0, 1.0, 2.0, 2.0), Color::rgba(255, 0, 0, 0.5));
        let at = (4 + 1) * 4;
        assert_eq!(&pixels[at..at + 4], &[0, 0, 128, 128]);
        assert_eq!(alpha_at(&pixels, 4, 0, 0), 0);
    }

    #[test]
    fn a_rect_on_half_pixels_is_antialiased() {
        let mut pixels = canvas_pixels(4, 1);
        let mut canvas = Canvas::new(&mut pixels, 4, 1, 16, 1.0);
        canvas.fill_rect(Rect::new(0.5, 0.0, 2.0, 1.0), Color::rgb(0, 0, 0));
        assert_eq!(alpha_at(&pixels, 4, 0, 0), 128);
        assert_eq!(alpha_at(&pixels, 4, 1, 0), 255);
        assert_eq!(alpha_at(&pixels, 4, 2, 0), 128);
        assert_eq!(alpha_at(&pixels, 4, 3, 0), 0);
    }

    #[test]
    fn a_circle_is_solid_inside_and_soft_at_its_edge() {
        let mut pixels = canvas_pixels(20, 20);
        let mut canvas = Canvas::new(&mut pixels, 20, 20, 80, 1.0);
        canvas.fill_circle(Point::new(10.0, 10.0), 6.0, Color::rgb(255, 255, 255));
        assert_eq!(alpha_at(&pixels, 20, 10, 10), 255);
        assert_eq!(alpha_at(&pixels, 20, 1, 1), 0);
        let edge = alpha_at(&pixels, 20, 15, 6);
        assert!(edge > 0 && edge < 255, "edge coverage {edge}");
    }

    #[test]
    fn a_stroked_rect_leaves_its_middle_alone() {
        let mut pixels = canvas_pixels(20, 20);
        let mut canvas = Canvas::new(&mut pixels, 20, 20, 80, 1.0);
        canvas.stroke_rounded_rect(
            Rect::new(2.0, 2.0, 16.0, 16.0),
            3.0,
            2.0,
            Color::rgb(0, 0, 0),
        );
        assert_eq!(alpha_at(&pixels, 20, 10, 10), 0);
        assert_eq!(alpha_at(&pixels, 20, 10, 2), 255);
    }

    #[test]
    fn drawing_follows_the_scale_and_the_clip() {
        let mut pixels = canvas_pixels(8, 8);
        let mut canvas = Canvas::new(&mut pixels, 8, 8, 32, 2.0);
        canvas.push_clip(Rect::new(0.0, 0.0, 2.0, 4.0));
        canvas.fill_rect(Rect::new(0.0, 0.0, 4.0, 4.0), Color::rgb(0, 0, 0));
        canvas.pop_clip();
        assert_eq!(alpha_at(&pixels, 8, 3, 7), 255);
        assert_eq!(alpha_at(&pixels, 8, 4, 0), 0);
    }

    #[test]
    fn a_gradient_runs_from_top_to_bottom() {
        let mut pixels = canvas_pixels(1, 4);
        let mut canvas = Canvas::new(&mut pixels, 1, 4, 4, 1.0);
        canvas.fill_vertical_gradient(
            Rect::new(0.0, 0.0, 1.0, 4.0),
            Color::rgb(0, 0, 0),
            Color::rgb(0, 0, 255),
        );
        assert!(pixels[0] < pixels[12], "blue grows downwards");
    }
}
