//! A window's rounded corners, as coverage over pixels it already has.

/// How much of the pixel `column` across and `row` down from a corner lies
/// inside a rounding of `radius` pixels, from 0 to 1.
pub fn coverage(radius: f32, column: usize, row: usize) -> f32 {
    let (x, y) = (column as f32 + 0.5, row as f32 + 0.5);
    if x >= radius || y >= radius {
        return 1.0;
    }
    let distance = (radius - x).hypot(radius - y);
    (radius - distance + 0.5).clamp(0.0, 1.0)
}

/// For each row from a corner's edge, how many pixels in from the side are
/// mostly outside the rounding: what a path that cannot blend clears.
pub fn outside(radius: f32) -> Vec<usize> {
    let rows = radius.ceil().max(0.0) as usize;
    (0..rows)
        .map(|row| {
            (0..rows)
                .take_while(|&column| coverage(radius, column, row) < 0.5)
                .count()
        })
        .take_while(|&count| count > 0)
        .collect()
}

/// Scale the premultiplied B, G, R, A pixels at the chosen corners of a
/// `width` by `height` image by their coverage.
pub fn clip(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    stride: usize,
    radius: f32,
    top: bool,
    bottom: bool,
) {
    let size = (radius.ceil().max(0.0) as usize).min(width / 2).min(height);
    let mut scale = |x: usize, y: usize, amount: f32| {
        let at = y * stride + x * 4;
        for byte in &mut pixels[at..at + 4] {
            *byte = (*byte as f32 * amount).round() as u8;
        }
    };
    for row in 0..size {
        for column in 0..size {
            let amount = coverage(radius, column, row);
            if amount >= 1.0 {
                continue;
            }
            let right = width - 1 - column;
            if top {
                scale(column, row, amount);
                scale(right, row, amount);
            }
            if bottom && height > row {
                let y = height - 1 - row;
                scale(column, y, amount);
                scale(right, y, amount);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_corner_pixel_is_outside_and_the_edge_midpoint_inside() {
        assert_eq!(coverage(10.0, 0, 0), 0.0);
        assert_eq!(coverage(10.0, 10, 0), 1.0);
        assert_eq!(coverage(10.0, 9, 9), 1.0);
    }

    #[test]
    fn outside_runs_narrow_towards_the_straight_edge() {
        let spans = outside(10.0);
        assert!(spans[0] >= 3);
        assert!(spans.windows(2).all(|pair| pair[0] >= pair[1]));
        assert!(spans.len() < 10);
    }

    #[test]
    fn clipping_clears_the_corners_and_keeps_the_middle() {
        let (width, height) = (40, 30);
        let mut pixels = vec![255u8; width * height * 4];
        clip(&mut pixels, width, height, width * 4, 8.0, true, false);
        let at = |x: usize, y: usize| pixels[(y * width + x) * 4 + 3];
        assert_eq!(at(0, 0), 0);
        assert_eq!(at(width - 1, 0), 0);
        assert_eq!(at(0, height - 1), 255);
        assert_eq!(at(width / 2, 0), 255);
    }
}
