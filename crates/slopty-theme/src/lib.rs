//! Design tokens. Toolkit-agnostic: plain numbers and RGB so `slopty-ui` (GPUI) and any other
//! consumer read the same values.
//!
//! Visual direction: Warp-like. A neutral surface ladder, one accent, hairlines rather than
//! shadows, a 4/8 pt spacing scale, status colour only where it carries meaning, the terminal
//! mono for terminal surfaces and the system sans for chrome. The table and the rules are the
//! "Design tokens" ruling in `docs/DECISIONS.md`; chrome draws from these tokens and nothing
//! else.

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
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
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
        fg: Rgb::hex(0xe6e6e6),
        bg: Rgb::hex(0x0e0f12),
        cursor: Rgb::hex(0x8ab4f8),
        cursor_text: Rgb::hex(0x0e0f12),
        selection: Rgb::hex(0x2b3a55),
        search_match: Rgb::hex(0x4a4020),
        search_current: Rgb::hex(0x8c6a1f),
        ansi: [
            Rgb::hex(0x1a1b1f),
            Rgb::hex(0xf06c75),
            Rgb::hex(0x98c379),
            Rgb::hex(0xe5c07b),
            Rgb::hex(0x61afef),
            Rgb::hex(0xc678dd),
            Rgb::hex(0x56b6c2),
            Rgb::hex(0xc8ccd4),
            Rgb::hex(0x5c6370),
            Rgb::hex(0xff7b86),
            Rgb::hex(0xa6d68a),
            Rgb::hex(0xf0cc8c),
            Rgb::hex(0x74beff),
            Rgb::hex(0xd68aee),
            Rgb::hex(0x66c7d4),
            Rgb::hex(0xffffff),
        ],
    };
    /// The default light palette (GitHub-light hues: legible on white without glare).
    #[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
    pub const LIGHT: Self = Self {
        fg: Rgb::hex(0x1f2328),
        bg: Rgb::hex(0xffffff),
        cursor: Rgb::hex(0x2f6fdb),
        cursor_text: Rgb::hex(0xffffff),
        selection: Rgb::hex(0xc9ddfb),
        search_match: Rgb::hex(0xffe9a8),
        search_current: Rgb::hex(0xf5b942),
        ansi: [
            Rgb::hex(0x24292f),
            Rgb::hex(0xcf222e),
            Rgb::hex(0x116329),
            Rgb::hex(0x9a6700),
            Rgb::hex(0x0969da),
            Rgb::hex(0x8250df),
            Rgb::hex(0x1b7c83),
            Rgb::hex(0x6e7781),
            Rgb::hex(0x57606a),
            Rgb::hex(0xa40e26),
            Rgb::hex(0x1a7f37),
            Rgb::hex(0xbf8700),
            Rgb::hex(0x218bff),
            Rgb::hex(0xa475f9),
            Rgb::hex(0x3192aa),
            Rgb::hex(0x8c959f),
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
///
/// The chrome scale hangs off `ui_size` ([`Self::caption`], [`Self::small`], [`Self::title`]),
/// so a settings change moves every label together.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Typography {
    /// Monospace family for terminals; the first installed one wins.
    pub mono_families: Vec<String>,
    /// Terminal font size in points.
    pub mono_size: f32,
    /// Terminal line height as a multiple of the one the font asks for (ghostty's
    /// `adjust-cell-height`). `1.0` is the font's own, which is what a terminal wants.
    pub mono_line_height: f32,
    /// Line height of Markdown prose (assistant turns), as a multiple of the font size.
    pub markdown_line_height: f32,
    /// UI family.
    pub ui_family: String,
    /// UI base size.
    pub ui_size: f32,
}

impl Typography {
    /// The smallest chrome size: HUD readouts, timestamps, chevrons (base − 3).
    #[must_use]
    pub fn caption(&self) -> f32 {
        (self.ui_size - 3.0).max(6.0)
    }

    /// Secondary chrome: bar labels, pills, folds, tool summaries (base − 1).
    #[must_use]
    pub fn small(&self) -> f32 {
        (self.ui_size - 1.0).max(7.0)
    }

    /// Titles of panels and dialogs (base + 2).
    #[must_use]
    pub fn title(&self) -> f32 {
        self.ui_size + 2.0
    }
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
            mono_line_height: 1.0,
            markdown_line_height: 1.5,
            ui_family: ".SystemUIFont".to_owned(),
            ui_size: 13.0,
        }
    }
}

/// Corner radii, in points.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Radii {
    /// Pills and inline buttons.
    pub xs: f32,
    /// Buttons, inputs, key caps.
    pub sm: f32,
    /// Panels, canvas items, popovers.
    pub md: f32,
}

impl Default for Radii {
    fn default() -> Self {
        Self { xs: 4.0, sm: 6.0, md: 8.0 }
    }
}

/// The spacing scale, in points: the only paddings and gaps chrome uses.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Spacing {
    /// 2: pill vertical padding, hairline-adjacent gaps.
    pub xxs: f32,
    /// 4: tight gaps, inline button padding.
    pub xs: f32,
    /// 8: the base unit — row gaps, button padding, grid inset.
    pub sm: f32,
    /// 12: panel horizontal padding, section gaps.
    pub md: f32,
    /// 16: panel padding.
    pub lg: f32,
    /// 24: dialog padding.
    pub xl: f32,
}

impl Default for Spacing {
    fn default() -> Self {
        Self { xxs: 2.0, xs: 4.0, sm: 8.0, md: 12.0, lg: 16.0, xl: 24.0 }
    }
}

/// Opacities for tints and washes over a surface.
pub mod alpha {
    /// A selected row, the faint fill of a quiet pill.
    pub const TINT_FAINT: f32 = 0.08;
    /// Pill fills, the hover of a row.
    pub const TINT: f32 = 0.12;
    /// Answer buttons, the human's bubble.
    pub const TINT_STRONG: f32 = 0.25;
    /// A strong tint under the pointer.
    pub const TINT_PRESSED: f32 = 0.4;
    /// The hover wash of `text` over a bare button.
    pub const HOVER: f32 = 0.08;
    /// A modal backdrop.
    pub const SCRIM: f32 = 0.6;
    /// The command-block separator (the terminal foreground).
    pub const SEPARATOR: f32 = 0.18;
    /// The separator after a failed command (the error tone).
    pub const SEPARATOR_ERROR: f32 = 0.7;
    /// The translucent panel behind a HUD readout over video.
    pub const HUD: f32 = 0.85;
    /// The minimap's panel over the canvas.
    pub const MINIMAP: f32 = 0.92;
    /// Minimap item blocks.
    pub const MINIMAP_ITEM: f32 = 0.7;
    /// Another client's viewport outline on the minimap.
    pub const MINIMAP_LOOKER: f32 = 0.8;
    /// The name tag on another client's outline; opaque while that client is followed.
    pub const LOOKER_TAG: f32 = 0.9;
}

/// Surface colours for chrome (not the terminal grid): a four-step ladder, three text
/// levels, the accent and what sits on it, and three status tones.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Surfaces {
    /// Step 0: window, canvas, bars.
    pub canvas: Rgb,
    /// Step 1: title bars, panels, popovers, the composer.
    pub panel: Rgb,
    /// Step 2: key caps, inputs, hovered rows.
    pub raised: Rgb,
    /// Step 3: pressed rows, pill fills, the HUD.
    pub overlay: Rgb,
    /// Every hairline.
    pub border: Rgb,
    /// Primary text.
    pub text: Rgb,
    /// Labels, tool summaries, counts.
    pub text_secondary: Rgb,
    /// Hints, timestamps, folds, inactive titles.
    pub text_muted: Rgb,
    /// Focus ring, active border, the primary action, links.
    pub accent: Rgb,
    /// Text on an accent fill.
    pub accent_fg: Rgb,
    /// Connected, agent done.
    pub success: Rgb,
    /// Agent waiting, "N need you", muted, reconnecting.
    pub warn: Rgb,
    /// A failed result or command, a pairing error.
    pub error: Rgb,
}

impl Surfaces {
    /// Dark.
    #[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
    pub const DARK: Self = Self {
        canvas: Rgb::hex(0x0a0b0e),
        panel: Rgb::hex(0x14161b),
        raised: Rgb::hex(0x1b1e25),
        overlay: Rgb::hex(0x23272f),
        border: Rgb::hex(0x24272e),
        text: Rgb::hex(0xe6e6e6),
        text_secondary: Rgb::hex(0xb4b9c3),
        text_muted: Rgb::hex(0x8b919c),
        accent: Rgb::hex(0x8ab4f8),
        accent_fg: Rgb::hex(0x0a0b0e),
        success: Rgb::hex(0x98c379),
        warn: Rgb::hex(0xe5c07b),
        error: Rgb::hex(0xf06c75),
    };
    /// Light.
    #[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
    pub const LIGHT: Self = Self {
        canvas: Rgb::hex(0xf4f5f7),
        panel: Rgb::hex(0xffffff),
        raised: Rgb::hex(0xeef0f3),
        overlay: Rgb::hex(0xe4e7ec),
        border: Rgb::hex(0xd8dbe1),
        text: Rgb::hex(0x1d1d1f),
        text_secondary: Rgb::hex(0x4b4f58),
        text_muted: Rgb::hex(0x6e6e73),
        accent: Rgb::hex(0x2f6fdb),
        accent_fg: Rgb::hex(0xffffff),
        success: Rgb::hex(0x1a7f37),
        warn: Rgb::hex(0x9a6700),
        error: Rgb::hex(0xcf222e),
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
    /// Corner radii.
    pub radii: Radii,
    /// The spacing scale.
    pub spacing: Spacing,
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
        Self {
            terminal,
            surfaces,
            typography: Typography::default(),
            radii: Radii::default(),
            spacing: Spacing::default(),
        }
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

    #[test]
    fn the_ladder_climbs_and_the_scale_follows_the_base() {
        // Dark: each step lighter than the last; light: each step darker.
        let luma = |c: Rgb| u32::from(c.r) + u32::from(c.g) + u32::from(c.b);
        let dark = Surfaces::DARK;
        assert!(luma(dark.canvas) < luma(dark.panel) && luma(dark.panel) < luma(dark.raised));
        assert!(luma(dark.raised) < luma(dark.overlay));
        assert!(
            luma(dark.text_muted) < luma(dark.text_secondary)
                && luma(dark.text_secondary) < luma(dark.text)
        );
        let light = Surfaces::LIGHT;
        assert!(
            luma(light.canvas) > luma(light.raised) && luma(light.raised) > luma(light.overlay)
        );
        assert!(
            luma(light.text_muted) > luma(light.text_secondary)
                && luma(light.text_secondary) > luma(light.text)
        );
        assert_ne!(dark.accent_fg, light.accent_fg, "text on the accent flips with the variant");

        let mut t = Typography::default();
        assert_eq!((t.caption(), t.small(), t.title()), (10.0, 12.0, 15.0));
        t.ui_size = 8.0;
        assert_eq!((t.caption(), t.small(), t.title()), (6.0, 7.0, 10.0), "clamped at the floor");
        let s = Spacing::default();
        assert!(s.xxs < s.xs && s.xs < s.sm && s.sm < s.md && s.md < s.lg && s.lg < s.xl);
        let r = Radii::default();
        assert!(r.xs < r.sm && r.sm < r.md);
    }

    /// Each byte of a `0xRRGGBB` literal lands in its own channel, and the cube's first
    /// axis is the red one.
    #[test]
    fn a_hex_literal_splits_into_channels() {
        assert_eq!(Rgb::hex(0x12_34_56), Rgb { r: 0x12, g: 0x34, b: 0x56 });
        assert_eq!(Rgb::hex(0xff_00_00), Rgb { r: 255, g: 0, b: 0 });
        let p = Theme::new(Variant::Dark).terminal;
        assert_eq!(p.palette(52), Rgb { r: 95, g: 0, b: 0 }, "16 + 36: one step of red");
        assert_eq!(p.palette(22), Rgb { r: 0, g: 95, b: 0 }, "16 + 6: one step of green");
    }
}
