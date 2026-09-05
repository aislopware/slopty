//! Design tokens. Toolkit-agnostic: plain numbers and RGB so `slopty-ui` (GPUI) and any other
//! consumer read the same values.
//!
//! Visual direction: Warp-like. Near-black neutral surfaces, one accent, restrained contrast,
//! generous spacing, a monospace face with real weight variation.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use slopty_grid::Color;

/// An sRGB colour.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Rgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

impl Rgb {
    /// From a `0xRRGGBB` literal.
    #[must_use]
    pub const fn hex(v: u32) -> Self {
        Self { r: ((v >> 16) & 0xff) as u8, g: ((v >> 8) & 0xff) as u8, b: (v & 0xff) as u8 }
    }
}

/// Terminal colours.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TerminalPalette {
    /// Default text.
    pub fg: Rgb,
    /// Default background.
    pub bg: Rgb,
    /// Cursor block.
    pub cursor: Rgb,
    /// Text under a block cursor.
    pub cursor_text: Rgb,
    /// Selection background.
    pub selection: Rgb,
    /// Background of a search hit.
    pub search_match: Rgb,
    /// Background of the search hit the user is on.
    pub search_current: Rgb,
    /// ANSI 0–15.
    pub ansi: [Rgb; 16],
}

impl TerminalPalette {
    /// The default dark palette.
    #[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
    pub const DARK: Self = Self {
        fg: Rgb::hex(0xE6E6E6),
        bg: Rgb::hex(0x0E0F12),
        cursor: Rgb::hex(0x8AB4F8),
        cursor_text: Rgb::hex(0x0E0F12),
        selection: Rgb::hex(0x2B3A55),
        search_match: Rgb::hex(0x4A4020),
        search_current: Rgb::hex(0x8C6A1F),
        ansi: [
            Rgb::hex(0x1A1B1F),
            Rgb::hex(0xF06C75),
            Rgb::hex(0x98C379),
            Rgb::hex(0xE5C07B),
            Rgb::hex(0x61AFEF),
            Rgb::hex(0xC678DD),
            Rgb::hex(0x56B6C2),
            Rgb::hex(0xC8CCD4),
            Rgb::hex(0x5C6370),
            Rgb::hex(0xFF7B86),
            Rgb::hex(0xA6D68A),
            Rgb::hex(0xF0CC8C),
            Rgb::hex(0x74BEFF),
            Rgb::hex(0xD68AEE),
            Rgb::hex(0x66C7D4),
            Rgb::hex(0xFFFFFF),
        ],
    };
    /// The default light palette (GitHub-light hues: legible on white without glare).
    #[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
    pub const LIGHT: Self = Self {
        fg: Rgb::hex(0x1F2328),
        bg: Rgb::hex(0xFFFFFF),
        cursor: Rgb::hex(0x2F6FDB),
        cursor_text: Rgb::hex(0xFFFFFF),
        selection: Rgb::hex(0xC9DDFB),
        search_match: Rgb::hex(0xFFE9A8),
        search_current: Rgb::hex(0xF5B942),
        ansi: [
            Rgb::hex(0x24292F),
            Rgb::hex(0xCF222E),
            Rgb::hex(0x116329),
            Rgb::hex(0x9A6700),
            Rgb::hex(0x0969DA),
            Rgb::hex(0x8250DF),
            Rgb::hex(0x1B7C83),
            Rgb::hex(0x6E7781),
            Rgb::hex(0x57606A),
            Rgb::hex(0xA40E26),
            Rgb::hex(0x1A7F37),
            Rgb::hex(0xBF8700),
            Rgb::hex(0x218BFF),
            Rgb::hex(0xA475F9),
            Rgb::hex(0x3192AA),
            Rgb::hex(0x8C959F),
        ],
    };

    /// Resolve a grid colour to RGB. `Default` uses `fg` or `bg` depending on `slot_is_bg`.
    #[must_use]
    pub fn resolve(&self, color: Color, slot_is_bg: bool) -> Rgb {
        match color {
            Color::Default => {
                if slot_is_bg {
                    self.bg
                } else {
                    self.fg
                }
            }
            Color::Palette(n) => self.palette(n),
            Color::Rgb(r, g, b) => Rgb { r, g, b },
        }
    }

    /// The 256-colour palette: 0–15 from the theme, 16–231 the 6×6×6 cube, 232–255 greys.
    #[must_use]
    pub fn palette(&self, index: u8) -> Rgb {
        if let Some(named) = self.ansi.get(usize::from(index)) {
            return *named;
        }
        if index < 232 {
            let cube = index.saturating_sub(16);
            let level = |v: u8| if v == 0 { 0 } else { 55_u8.saturating_add(v.saturating_mul(40)) };
            Rgb { r: level(cube / 36), g: level((cube / 6) % 6), b: level(cube % 6) }
        } else {
            let grey = 8_u8.saturating_add(index.saturating_sub(232).saturating_mul(10));
            Rgb { r: grey, g: grey, b: grey }
        }
    }
}

/// Type tokens.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Typography {
    /// Monospace family for terminals; the first installed one wins.
    pub mono_families: Vec<String>,
    /// Terminal font size in points.
    pub mono_size: f32,
    /// Terminal line height as a multiple of the font size.
    pub mono_line_height: f32,
    /// UI family.
    pub ui_family: String,
    /// UI base size.
    pub ui_size: f32,
}

impl Default for Typography {
    fn default() -> Self {
        Self {
            mono_families: vec![
                "JetBrains Mono".to_owned(),
                "SF Mono".to_owned(),
                "Menlo".to_owned(),
            ],
            mono_size: 13.0,
            mono_line_height: 1.35,
            ui_family: ".SystemUIFont".to_owned(),
            ui_size: 13.0,
        }
    }
}

/// Surface colours for chrome (not the terminal grid).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Surfaces {
    /// Window background.
    pub canvas: Rgb,
    /// Panels, cards.
    pub panel: Rgb,
    /// Hairlines.
    pub border: Rgb,
    /// Primary text.
    pub text: Rgb,
    /// Secondary text.
    pub text_muted: Rgb,
    /// Accent.
    pub accent: Rgb,
}

impl Surfaces {
    /// Dark.
    #[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
    pub const DARK: Self = Self {
        canvas: Rgb::hex(0x0A0B0E),
        panel: Rgb::hex(0x14161B),
        border: Rgb::hex(0x24272E),
        text: Rgb::hex(0xE6E6E6),
        text_muted: Rgb::hex(0x8B919C),
        accent: Rgb::hex(0x8AB4F8),
    };
    /// Light.
    #[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
    pub const LIGHT: Self = Self {
        canvas: Rgb::hex(0xF4F5F7),
        panel: Rgb::hex(0xFFFFFF),
        border: Rgb::hex(0xD8DBE1),
        text: Rgb::hex(0x1D1D1F),
        text_muted: Rgb::hex(0x6E6E73),
        accent: Rgb::hex(0x2F6FDB),
    };
}

/// Dark or light.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub enum Variant {
    /// Near-black surfaces, light text.
    #[default]
    Dark,
    /// White surfaces, dark text.
    Light,
}

/// The whole theme.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Theme {
    /// Terminal colours.
    pub terminal: TerminalPalette,
    /// Chrome colours.
    pub surfaces: Surfaces,
    /// Type.
    pub typography: Typography,
    /// Corner radius for panels, in points.
    pub radius: f32,
    /// Base spacing unit, in points.
    pub space: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self::new(Variant::Dark)
    }
}

impl Theme {
    /// The theme for `variant` with default typography.
    #[must_use]
    pub fn new(variant: Variant) -> Self {
        let (terminal, surfaces) = match variant {
            Variant::Dark => (TerminalPalette::DARK, Surfaces::DARK),
            Variant::Light => (TerminalPalette::LIGHT, Surfaces::LIGHT),
        };
        Self { terminal, surfaces, typography: Typography::default(), radius: 8.0, space: 8.0 }
    }

    /// Which variant the colours are (by the terminal background).
    #[must_use]
    pub fn variant(&self) -> Variant {
        if self.terminal.bg == TerminalPalette::LIGHT.bg { Variant::Light } else { Variant::Dark }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_cube_and_greys() {
        let p = TerminalPalette::DARK;
        assert_eq!(p.palette(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(p.palette(231), Rgb { r: 255, g: 255, b: 255 });
        assert_eq!(p.palette(196), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(p.palette(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(p.palette(255), Rgb { r: 238, g: 238, b: 238 });
        assert_eq!(p.palette(1), p.ansi[1]);
        assert_eq!(p.resolve(Color::Default, true), p.bg);
        assert_eq!(p.resolve(Color::Rgb(1, 2, 3), false), Rgb { r: 1, g: 2, b: 3 });
    }

    #[test]
    fn variants() {
        assert_eq!(Theme::default().variant(), Variant::Dark);
        let light = Theme::new(Variant::Light);
        assert_eq!(light.variant(), Variant::Light);
        assert_eq!(light.surfaces, Surfaces::LIGHT);
        assert_eq!(light.typography, Typography::default());
        assert_ne!(light.terminal.palette(0), light.terminal.bg, "ANSI black is visible on white");
    }
}
