//! Cell styling: colours and attributes, exactly as the engine reports them.
//!
//! Colours are kept *unresolved* (palette index vs. RGB vs. default). Resolution against a theme
//! happens at paint time, because rules like "bold does not brighten" and "inverse swaps the
//! raw fg/bg" need the original bits.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

/// A colour as the terminal program specified it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
pub enum Color {
    /// The theme's default for this slot (foreground, background or underline).
    #[default]
    Default,
    /// One of the 256 palette entries (0–15 are the ANSI colours).
    Palette(u8),
    /// A true colour.
    Rgb(u8, u8, u8),
}

/// Underline style (SGR 4, 4:1–4:5, 21).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
pub enum Underline {
    /// No underline.
    #[default]
    None,
    /// Single line.
    Single,
    /// Double line.
    Double,
    /// Wavy line (spell-check style).
    Curly,
    /// Dotted line.
    Dotted,
    /// Dashed line.
    Dashed,
}

bitflags! {
    /// Boolean SGR attributes.
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct StyleFlags: u16 {
        /// SGR 1.
        const BOLD = 1 << 0;
        /// SGR 2.
        const FAINT = 1 << 1;
        /// SGR 3.
        const ITALIC = 1 << 2;
        /// SGR 5 / 6.
        const BLINK = 1 << 3;
        /// SGR 7.
        const INVERSE = 1 << 4;
        /// SGR 8.
        const INVISIBLE = 1 << 5;
        /// SGR 9.
        const STRIKETHROUGH = 1 << 6;
        /// SGR 53.
        const OVERLINE = 1 << 7;
    }
}

/// The complete style of one cell.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
pub struct Style {
    /// Foreground colour.
    pub fg: Color,
    /// Background colour.
    pub bg: Color,
    /// Underline colour (SGR 58); `Default` means "same as foreground".
    pub underline_color: Color,
    /// Underline style.
    pub underline: Underline,
    /// Boolean attributes.
    pub flags: StyleFlags,
}

impl Style {
    /// The style of an untouched cell.
    pub const DEFAULT: Self = Self {
        fg: Color::Default,
        bg: Color::Default,
        underline_color: Color::Default,
        underline: Underline::None,
        flags: StyleFlags::empty(),
    };

    /// True when every field is at its default, i.e. the cell carries no styling at all.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::DEFAULT
    }

    /// True when the style has an attribute affecting glyph selection (bold/italic), so the
    /// renderer must pick a different font face.
    #[must_use]
    pub const fn needs_face_variant(&self) -> bool {
        self.flags.intersects(StyleFlags::BOLD.union(StyleFlags::ITALIC))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_style_is_the_zero_value() {
        assert!(Style::default().is_default());
        assert_eq!(Style::default(), Style::DEFAULT);
    }

    #[test]
    fn flags_round_trip_through_serde() {
        // Human-readable formats get names (bitflags' serde impl); postcard on the wire gets the
        // raw bits, pinned by slopty-proto's golden snapshots.
        let f = StyleFlags::BOLD | StyleFlags::ITALIC;
        let json = serde_json::to_string(&f).unwrap();
        assert_eq!(json, "\"BOLD | ITALIC\"");
        assert_eq!(serde_json::from_str::<StyleFlags>(&json).unwrap(), f);
    }
}
