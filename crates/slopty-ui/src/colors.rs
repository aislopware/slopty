//! Theme tokens → GPUI colours.

use gpui::{Hsla, Rgba};
use slopty_theme::Rgb;

/// An opaque colour.
#[must_use]
pub fn hsla(c: Rgb) -> Hsla {
    Rgba { r: f32::from(c.r) / 255.0, g: f32::from(c.g) / 255.0, b: f32::from(c.b) / 255.0, a: 1.0 }
        .into()
}

/// A colour with alpha.
#[must_use]
pub fn hsla_alpha(c: Rgb, a: f32) -> Hsla {
    Rgba { r: f32::from(c.r) / 255.0, g: f32::from(c.g) / 255.0, b: f32::from(c.b) / 255.0, a }
        .into()
}
