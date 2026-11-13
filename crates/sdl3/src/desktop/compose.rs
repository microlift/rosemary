//! Drawing the desktop: every window's picture over the background, back to
//! front, into the one surface the host shows.
//!
//! Pixels are four bytes - blue, green, red, alpha - premultiplied, top row
//! first, which is what SDL's window surface holds and so what every writer
//! of a window's picture in this tree already produces.

use crate::Rect;

const BYTES_PER_PIXEL: usize = 4;
const OPAQUE: u32 = 255;

/// What shows where no window is: opaque mid grey, in blue, green, red, alpha.
pub(crate) const BACKGROUND: [u8; BYTES_PER_PIXEL] = [0x80, 0x80, 0x80, 0xff];

/// One window's picture.
#[derive(Default)]
pub(crate) struct Buffer {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u8>,
}

impl Buffer {
    pub(crate) fn pitch(&self) -> usize {
        self.width.max(0) as usize * BYTES_PER_PIXEL
    }

    /// Take a new size, keeping the picture's top-left: a window resized
    /// before it redraws shows what it had rather than nothing.
    pub(crate) fn resize(&mut self, width: i32, height: i32) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.width, self.height) {
            return;
        }
        let mut next = Buffer {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * BYTES_PER_PIXEL],
        };
        let kept = self.width.min(width).max(0) as usize * BYTES_PER_PIXEL;
        for row in 0..self.height.min(height).max(0) as usize {
            let (from, to) = (row * self.pitch(), row * next.pitch());
            next.pixels[to..to + kept].copy_from_slice(&self.pixels[from..from + kept]);
        }
        *self = next;
    }
}

/// A window's picture and where it goes, in the target's pixels.
pub(crate) struct Layer<'a> {
    pub buffer: &'a Buffer,
    pub x: i32,
    pub y: i32,
    /// Whether the picture's alpha is meant: a window not made transparent
    /// is opaque whatever its fourth byte says.
    pub alpha: bool,
    pub opacity: f32,
}

/// The surface the desktop is drawn into.
pub(crate) struct Target<'a> {
    pub pixels: &'a mut [u8],
    pub pitch: usize,
    pub width: i32,
    pub height: i32,
}

/// `a * b`, both fractions of 255.
fn scaled(a: u32, b: u32) -> u32 {
    (a * b + OPAQUE / 2) / OPAQUE
}

/// Redraw `damage` of `target`: the background, then `layers` back to front.
pub(crate) fn compose(target: &mut Target, layers: &[Layer], damage: Rect) {
    let damage = damage.intersection(Rect {
        x: 0,
        y: 0,
        w: target.width,
        h: target.height,
    });
    if damage.is_empty() {
        return;
    }
    for row in damage.y..damage.y + damage.h {
        let start = row as usize * target.pitch + damage.x as usize * BYTES_PER_PIXEL;
        let end = start + damage.w as usize * BYTES_PER_PIXEL;
        for pixel in target.pixels[start..end]
            .as_chunks_mut::<BYTES_PER_PIXEL>()
            .0
        {
            *pixel = BACKGROUND;
        }
    }
    for layer in layers {
        let opacity = (layer.opacity.clamp(0.0, 1.0) * OPAQUE as f32).round() as u32;
        let shown = damage.intersection(Rect {
            x: layer.x,
            y: layer.y,
            w: layer.buffer.width,
            h: layer.buffer.height,
        });
        if shown.is_empty() || opacity == 0 {
            continue;
        }
        let bytes = shown.w as usize * BYTES_PER_PIXEL;
        for row in shown.y..shown.y + shown.h {
            let from = (row - layer.y) as usize * layer.buffer.pitch()
                + (shown.x - layer.x) as usize * BYTES_PER_PIXEL;
            let to = row as usize * target.pitch + shown.x as usize * BYTES_PER_PIXEL;
            let source = &layer.buffer.pixels[from..from + bytes];
            let destination = &mut target.pixels[to..to + bytes];
            if !layer.alpha && opacity == OPAQUE {
                destination.copy_from_slice(source);
                continue;
            }
            let pixels = source
                .as_chunks::<BYTES_PER_PIXEL>()
                .0
                .iter()
                .zip(destination.as_chunks_mut::<BYTES_PER_PIXEL>().0);
            for (source, destination) in pixels {
                let coverage = match layer.alpha {
                    true => scaled(u32::from(source[3]), opacity),
                    false => opacity,
                };
                for channel in 0..BYTES_PER_PIXEL {
                    let over = scaled(u32::from(source[channel]), opacity);
                    let under = scaled(u32::from(destination[channel]), OPAQUE - coverage);
                    destination[channel] = (over + under).min(OPAQUE) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [u8; 4] = [0, 0, 0xff, 0xff];
    const BLUE: [u8; 4] = [0xff, 0, 0, 0xff];

    fn filled(width: i32, height: i32, pixel: [u8; 4]) -> Buffer {
        Buffer {
            width,
            height,
            pixels: pixel.repeat((width * height) as usize),
        }
    }

    fn draw(layers: &[Layer], damage: Rect) -> Vec<[u8; 4]> {
        // A pitch wider than the row, as SDL's may be.
        let (width, height, pitch) = (4, 2, 20);
        let mut pixels = vec![0; pitch * height as usize];
        compose(
            &mut Target {
                pixels: &mut pixels,
                pitch,
                width,
                height,
            },
            layers,
            damage,
        );
        pixels
            .chunks(pitch)
            .flat_map(|row| row[..width as usize * 4].as_chunks::<4>().0.to_vec())
            .collect()
    }

    const WHOLE: Rect = Rect {
        x: 0,
        y: 0,
        w: 4,
        h: 2,
    };

    #[test]
    fn later_layers_cover_earlier_ones_and_the_background_shows_between() {
        let (red, blue) = (filled(2, 2, RED), filled(2, 1, BLUE));
        let layer = |buffer, x, y| Layer {
            buffer,
            x,
            y,
            alpha: false,
            opacity: 1.0,
        };
        assert_eq!(
            draw(&[layer(&red, 0, 0), layer(&blue, 1, 0)], WHOLE),
            [
                RED, BLUE, BLUE, BACKGROUND, //
                RED, RED, BACKGROUND, BACKGROUND,
            ]
        );
    }

    #[test]
    fn a_layer_partly_off_the_surface_is_clipped() {
        let red = filled(3, 3, RED);
        let layer = Layer {
            buffer: &red,
            x: -2,
            y: -2,
            alpha: false,
            opacity: 1.0,
        };
        assert_eq!(draw(&[layer], WHOLE)[..2], [RED, BACKGROUND]);
    }

    #[test]
    fn only_the_damage_is_redrawn() {
        let red = filled(4, 2, RED);
        let layer = Layer {
            buffer: &red,
            x: 0,
            y: 0,
            alpha: false,
            opacity: 1.0,
        };
        let damage = Rect {
            x: 1,
            y: 0,
            w: 1,
            h: 1,
        };
        let drawn = draw(&[layer], damage);
        assert_eq!(drawn[1], RED);
        assert_eq!(drawn[0], [0; 4]);
    }

    #[test]
    fn alpha_counts_only_for_a_transparent_window() {
        let clear = filled(1, 1, [0; 4]);
        let layer = |alpha| Layer {
            buffer: &clear,
            x: 0,
            y: 0,
            alpha,
            opacity: 1.0,
        };
        assert_eq!(draw(&[layer(true)], WHOLE)[0], BACKGROUND);
        assert_eq!(draw(&[layer(false)], WHOLE)[0], [0; 4]);
    }

    #[test]
    fn opacity_fades_a_window_into_what_is_under_it() {
        let white = filled(1, 1, [0xff; 4]);
        let layer = Layer {
            buffer: &white,
            x: 0,
            y: 0,
            alpha: false,
            opacity: 0.5,
        };
        // Halfway between the background's grey and white.
        assert_eq!(draw(&[layer], WHOLE)[0], [0xc0, 0xc0, 0xc0, 0xff]);
    }

    #[test]
    fn a_resized_buffer_keeps_its_top_left() {
        let mut buffer = filled(2, 2, RED);
        buffer.resize(3, 1);
        assert_eq!(buffer.pixels.as_chunks::<4>().0, [RED, RED, [0; 4]]);
    }
}
