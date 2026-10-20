//! The warm and cool light beneath the new-thread glyph field, after Amp's hero backdrop
//! (`.research/amp-study-2026-10-19.md`, item 17): an ember low on the left and the accent high
//! on the right, each a wide soft glow that only adds colour, never a vignette that darkens.
//!
//! The glows are smooth enough to live in a small image that the GPU stretches over the pane,
//! so the backdrop is made once a theme and costs one textured quad a frame. Its alpha steps are
//! a few levels of 8 bits, so each texel is dithered to keep the stretch free of bands.

/// The image's size, without the border: the GPU's bilinear stretch over a pane of any size
/// keeps the glows smooth at this resolution.
const WIDTH: u32 = 256;
const HEIGHT: u32 = 192;

/// One texel around the image, repeating its edge: the atlas packs images without a gap, so the
/// pane is painted from the inner texels and its edge never samples a neighbour's.
pub(crate) const BORDER: u32 = 1;

/// A glow: its centre and radii as fractions of the pane, and its weight.
struct Glow {
    centre: (f32, f32),
    radii: (f32, f32),
    weight: f32,
}

/// Amp's ember sits at 12 % / 100 %, its cool light high on the other side.
const WARM: Glow = Glow { centre: (0.12, 1.0), radii: (0.55, 0.6), weight: 1.0 };
const COOL: Glow = Glow { centre: (0.85, 0.0), radii: (0.5, 0.55), weight: 0.8 };

impl Glow {
    fn at(&self, u: f32, v: f32) -> f32 {
        let dx = (u - self.centre.0) / self.radii.0;
        let dy = (v - self.centre.1) / self.radii.1;
        self.weight * (-(dx * dx + dy * dy)).exp()
    }
}

/// A colour as straight `[r, g, b]` in 0–255.
pub(crate) type Rgb = [f32; 3];

/// The backdrop as BGRA texels with straight alpha, [`BORDER`] included: `warm` and `cool` are
/// the glows' colours, `strength` the alpha at a glow's heart.
pub(crate) fn backdrop(warm: Rgb, cool: Rgb, strength: f32) -> image::RgbaImage {
    let (width, height) = (WIDTH + 2 * BORDER, HEIGHT + 2 * BORDER);
    image::RgbaImage::from_fn(width, height, |x, y| {
        // The border repeats the edge texel it touches.
        let inner_x = x.saturating_sub(BORDER).min(WIDTH - 1);
        let inner_y = y.saturating_sub(BORDER).min(HEIGHT - 1);
        let u = (inner_x as f32 + 0.5) / WIDTH as f32;
        let v = (inner_y as f32 + 0.5) / HEIGHT as f32;
        let (warm_weight, cool_weight) = (WARM.at(u, v), COOL.at(u, v));
        let total = warm_weight + cool_weight;
        // The colour is the glows' blend wherever either reaches, so the stretch never fades
        // through a third hue.
        let share = if total > 0.0 { warm_weight / total } else { 0.5 };
        let dither = dither(inner_x, inner_y);
        let channel = |i: usize| {
            let value = cool[i] + (warm[i] - cool[i]) * share;
            (value + dither).round().clamp(0.0, 255.0) as u8
        };
        let alpha = (total.min(1.0) * strength * 255.0 + dither).round().clamp(0.0, 255.0) as u8;
        image::Rgba([channel(2), channel(1), channel(0), alpha])
    })
}

/// A fixed offset in ±0.5 of a level a texel, so neighbours round different ways.
fn dither(x: u32, y: u32) -> f32 {
    let hash = (x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663)).wrapping_mul(2_654_435_761);
    (hash >> 8) as f32 / 16_777_216.0 - 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texel(image: &image::RgbaImage, u: f32, v: f32) -> image::Rgba<u8> {
        let x = BORDER + (u * (WIDTH - 1) as f32) as u32;
        let y = BORDER + (v * (HEIGHT - 1) as f32) as u32;
        *image.get_pixel(x, y)
    }

    #[test]
    fn the_ember_glows_low_left_and_the_accent_high_right() {
        let warm = [248.0, 140.0, 70.0];
        let cool = [124.0, 134.0, 255.0];
        let image = backdrop(warm, cool, 0.1);
        let [b, _, r, low_left] = texel(&image, 0.1, 1.0).0;
        assert!(r > b, "the lower left is warm: r {r} b {b}");
        let [b, _, r, high_right] = texel(&image, 0.85, 0.0).0;
        assert!(b > r, "the upper right is cool: r {r} b {b}");
        let [_, _, _, middle] = texel(&image, 0.5, 0.5).0;
        assert!(middle < low_left && middle < high_right, "the middle stays quiet: {middle}");
        let strongest = image.pixels().map(|pixel| pixel.0[3]).max().unwrap_or(0);
        assert!(strongest <= 26, "no texel passes the strength: {strongest}");
    }

    #[test]
    fn the_border_repeats_the_edge() {
        let image = backdrop([248.0, 140.0, 70.0], [124.0, 134.0, 255.0], 0.1);
        let (width, height) = image.dimensions();
        assert_eq!((width, height), (WIDTH + 2 * BORDER, HEIGHT + 2 * BORDER));
        for y in 0..height {
            assert_eq!(image.get_pixel(0, y), image.get_pixel(BORDER, y));
            assert_eq!(image.get_pixel(width - 1, y), image.get_pixel(width - 1 - BORDER, y));
        }
    }
}
