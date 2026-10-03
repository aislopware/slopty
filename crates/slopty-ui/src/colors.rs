//! Theme tokens → GPUI colours.

use gpui::{Hsla, Rgba};
use slopty_theme::{Rgb, Tint};

/// A theme colour as GPUI draws it: an opaque one, or a hairline at its own opacity.
pub trait Tone: Copy {
    /// Its channels, 0 to 1, and its opacity.
    fn rgba(self) -> Rgba;
}

impl Tone for Rgb {
    fn rgba(self) -> Rgba {
        Rgba {
            r: f32::from(self.r) / 255.0,
            g: f32::from(self.g) / 255.0,
            b: f32::from(self.b) / 255.0,
            a: 1.0,
        }
    }
}

impl Tone for Tint {
    fn rgba(self) -> Rgba {
        Rgba { a: self.opacity(), ..self.ink.rgba() }
    }
}

/// A theme colour as it is: opaque, or a hairline at its own opacity.
#[must_use]
pub fn hsla(c: impl Tone) -> Hsla {
    c.rgba().into()
}

/// A colour at `a` of its own opacity.
#[must_use]
pub fn hsla_alpha(c: impl Tone, a: f32) -> Hsla {
    let rgba = c.rgba();
    Rgba { a: rgba.a * a, ..rgba }.into()
}
