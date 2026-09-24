//! Design tokens. Toolkit-agnostic: plain numbers and RGB so `slopty-ui` (GPUI) and any other
//! consumer read the same values.
//!
//! Visual direction: Warp-like. A neutral surface ladder, one accent, hairlines rather than
//! shadows, a 4/8 pt spacing scale, status colour only where it carries meaning, the terminal
//! mono for terminal surfaces and the system sans for chrome. The table and the rules are the
//! "Design tokens" ruling in `docs/DECISIONS.md`; chrome draws from these tokens and nothing
//! else.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use slopty_grid::Color;
use slopty_proto::terminal::{ColorOverrides, TermColors};

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

    /// Whether the colour reads as light: BT.601 luma past the middle.
    #[must_use]
    pub const fn is_light(self) -> bool {
        let luma = (self.r as u32)
            .wrapping_mul(299)
            .wrapping_add((self.g as u32).wrapping_mul(587))
            .wrapping_add((self.b as u32).wrapping_mul(114));
        luma > 128_000
    }

    /// WCAG relative luminance: sRGB linearised, 0.0 black to 1.0 white.
    #[must_use]
    pub fn luminance(self) -> f32 {
        let linear = |c: u8| {
            let c = f32::from(c) / 255.0;
            if c <= 0.039_28 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.0722_f32
            .mul_add(linear(self.b), 0.7152_f32.mul_add(linear(self.g), 0.2126 * linear(self.r)))
    }

    /// `self` moved `t` (0 to 1) of the way to `other`, channel by channel.
    #[must_use]
    pub fn mix(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0 to 255")]
        let channel =
            |a: u8, b: u8| (f32::from(b) - f32::from(a)).mul_add(t, f32::from(a)).round() as u8;
        Self {
            r: channel(self.r, other.r),
            g: channel(self.g, other.g),
            b: channel(self.b, other.b),
        }
    }

    /// WCAG contrast ratio with `other`: 1.0 (the same) to 21.0 (black on white).
    #[must_use]
    pub fn contrast(self, other: Self) -> f32 {
        let (a, b) = (self.luminance() + 0.05, other.luminance() + 0.05);
        if a > b { a / b } else { b / a }
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
    /// The least contrast ratio between a cell's text and its background, in hundredths
    /// (`100` = 1.0, off; `300` = 3.0). Text under it is painted black or white, whichever
    /// contrasts more with the background: ghostty's `minimum-contrast`.
    pub minimum_contrast: u16,
    /// Bold text in ANSI 0–7 is painted in ANSI 8–15 (xterm's `boldColors`, ghostty's
    /// `bold-is-bright`), for the many schemes that make the bright half a lighter tint.
    pub bold_is_bright: bool,
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
            Rgb::hex(0x747d8d),
            Rgb::hex(0xff7b86),
            Rgb::hex(0xa6d68a),
            Rgb::hex(0xf0cc8c),
            Rgb::hex(0x74beff),
            Rgb::hex(0xd68aee),
            Rgb::hex(0x66c7d4),
            Rgb::hex(0xffffff),
        ],
        minimum_contrast: 100,
        bold_is_bright: false,
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
            Rgb::hex(0x996c00),
            Rgb::hex(0x0070ea),
            Rgb::hex(0x8a4ef7),
            Rgb::hex(0x2a7e92),
            Rgb::hex(0x8c959f),
        ],
        minimum_contrast: 100,
        bold_is_bright: false,
    };

    /// The palette as the worker hears it: what colour queries answer while this client drives.
    #[must_use]
    pub fn wire(&self) -> TermColors {
        let rgb = |c: Rgb| [c.r, c.g, c.b];
        TermColors {
            fg: rgb(self.fg),
            bg: rgb(self.bg),
            cursor: rgb(self.cursor),
            ansi: self.ansi.map(rgb),
        }
    }

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

/// The theme's terminal colours under a program's changes (OSC 4, 10, 11, 12): what the cells
/// are painted with.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Colors {
    /// The theme with the program's default text, background, cursor and ANSI 0–15 on it.
    pub theme: TerminalPalette,
    /// The program's changes to palette entries 16–255 (0–15 are on `theme`).
    pub cube: BTreeMap<u8, Rgb>,
}

impl Colors {
    /// `theme` with `set` painted over it.
    #[must_use]
    pub fn new(theme: &TerminalPalette, set: &ColorOverrides) -> Self {
        let rgb = |[r, g, b]: [u8; 3]| Rgb { r, g, b };
        let mut theme = *theme;
        if let Some(fg) = set.fg {
            theme.fg = rgb(fg);
        }
        if let Some(bg) = set.bg {
            theme.bg = rgb(bg);
        }
        if let Some(cursor) = set.cursor {
            // The theme's text-under-cursor colour was chosen against the theme's cursor;
            // against the program's it is whichever of black and white reads.
            theme.cursor = rgb(cursor);
            theme.cursor_text =
                if theme.cursor.is_light() { Rgb::hex(0) } else { Rgb::hex(0xff_ffff) };
        }
        let mut cube = BTreeMap::new();
        for &(index, color) in &set.palette {
            if let Some(named) = theme.ansi.get_mut(usize::from(index)) {
                *named = rgb(color);
            } else {
                cube.insert(index, rgb(color));
            }
        }
        Self { theme, cube }
    }

    /// Resolve a grid colour to RGB. `Default` uses `fg` or `bg` depending on `slot_is_bg`.
    #[must_use]
    pub fn resolve(&self, color: Color, slot_is_bg: bool) -> Rgb {
        match color {
            Color::Palette(n) => {
                self.cube.get(&n).copied().unwrap_or_else(|| self.theme.palette(n))
            }
            other => self.theme.resolve(other, slot_is_bg),
        }
    }

    /// The slot bold text takes: ANSI 0–7 becomes 8–15 when the theme says bold is bright.
    #[must_use]
    pub const fn bold_slot(&self, color: Color, bold: bool) -> Color {
        match color {
            Color::Palette(n) if bold && self.theme.bold_is_bright && n < 8 => {
                Color::Palette(n.saturating_add(8))
            }
            other => other,
        }
    }

    /// The colour text `fg` is painted in over `bg`: itself, unless the theme's minimum
    /// contrast says it would not read. Then it is moved toward black or white, whichever
    /// contrasts more with `bg`, only as far as the minimum needs, so its hue survives: a
    /// pale mint prompt on white turns teal, where ghostty's snap would turn it black.
    #[must_use]
    pub fn text_over(&self, fg: Rgb, bg: Rgb) -> Rgb {
        let least = f32::from(self.theme.minimum_contrast) / 100.0;
        if least <= 1.0 || fg.contrast(bg) >= least {
            return fg;
        }
        let (black, white) = (Rgb::hex(0), Rgb::hex(0xff_ffff));
        let pole = if bg.contrast(white) >= bg.contrast(black) { white } else { black };
        if pole.contrast(bg) <= least {
            return pole;
        }
        // Contrast grows with the mix toward the pole: bisect for the least mix that reads.
        let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
        for _ in 0..12 {
            let mid = f32::midpoint(lo, hi);
            if fg.mix(pole, mid).contrast(bg) >= least {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        fg.mix(pole, hi)
    }
}

impl From<&TerminalPalette> for Colors {
    /// The theme as it is: nothing changed.
    fn from(theme: &TerminalPalette) -> Self {
        Self { theme: *theme, cube: BTreeMap::new() }
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
    /// Whether the terminal font's ligatures (`calt`) are shaped; off, `=>` stays two glyphs.
    pub ligatures: bool,
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
            ligatures: true,
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

/// Opacities for tints and washes over a surface: one ladder, used everywhere, so the chrome
/// reads as one surface rather than a collection of one-off transparencies.
pub mod alpha {
    /// Barely there: a selected row, the faint fill of a quiet pill, the hover wash over a
    /// bare button, a wash across the terminal grid (a block separator, the visual bell).
    pub const FAINT: f32 = 0.12;
    /// A tint that has to be seen: answer buttons, a selection.
    pub const TINT: f32 = 0.25;
    /// A tint under the pointer, a scrollbar thumb.
    pub const PRESSED: f32 = 0.4;
    /// A modal backdrop.
    pub const SCRIM: f32 = 0.6;
    /// A mark that must read over whatever it covers: the separator after a failed command,
    /// the minimap's item blocks.
    pub const STRONG: f32 = 0.7;
    /// A panel or tag laid over content and read through only barely: the stream HUD, the
    /// minimap, another client's viewport outline and its name tag.
    pub const VEIL: f32 = 0.9;
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
        // Darkened 2026-09-15 so every one of these clears WCAG AA on the darkest chrome
        // surface, not just on white: at their previous values `accent` read 3.83 and `warn`
        // 3.93 against `overlay`, and all five are text (`chrome_text_clears_wcag_aa`).
        text_muted: Rgb::hex(0x66666b),
        accent: Rgb::hex(0x2a63c4),
        accent_fg: Rgb::hex(0xffffff),
        success: Rgb::hex(0x187633),
        warn: Rgb::hex(0x8b5d00),
        error: Rgb::hex(0xc7212c),
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

/// How the terminal behaves: settings that are neither colours nor type but ride with them,
/// so one `set_theme` reaches every view when the file changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Behaviour {
    /// A selection goes to the clipboard as soon as it is made.
    pub copy_on_select: bool,
    /// Whether the cursor blinks: the program's choice (DECSCUSR), or overridden either way.
    pub cursor_blink: CursorBlink,
    /// The cursor's shape: the program's, or one fixed.
    pub cursor_style: CursorStyle,
    /// A paste that could run commands (a newline outside bracketed paste, the bracket's end
    /// sequence inside it) waits for a confirmation.
    pub paste_protection: bool,
    /// The pointer hides while typing into a terminal, until it moves (Terminal.app's way).
    pub hide_pointer_while_typing: bool,
    /// What a wheel or trackpad line is worth in grid lines, in hundredths (`100` = one).
    pub scroll_multiplier: u16,
    /// ⌥ as Alt: sent with every key, since the encoder is the worker's.
    pub option_as_alt: OptionAsAlt,
    /// Closing a terminal whose command is still running asks first.
    pub confirm_close: bool,
    /// ⌘← ⌘→ ⌘⌫ ⌥← ⌥→ ⌥⌫ edit the shell's line as the Mac's text fields do.
    pub natural_editing: bool,
    /// What a remote window or display stream asks the worker for.
    pub stream: StreamPrefs,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            copy_on_select: false,
            cursor_blink: CursorBlink::Program,
            cursor_style: CursorStyle::Program,
            paste_protection: true,
            hide_pointer_while_typing: true,
            scroll_multiplier: 100,
            option_as_alt: OptionAsAlt::False,
            confirm_close: true,
            natural_editing: true,
            stream: StreamPrefs::default(),
        }
    }
}

/// Whether ⌥ is Alt (ghostty's `macos-option-as-alt`): a modifier that sends an escape
/// prefix, or the layout's key that types the symbol.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum OptionAsAlt {
    /// The layout's key (`⌥b` types `∫`).
    #[default]
    False,
    /// Alt on both sides.
    True,
    /// Only the left key is Alt.
    Left,
    /// Only the right key is Alt.
    Right,
}

impl OptionAsAlt {
    /// Whether the ⌥ held (`right` says which) is Alt.
    #[must_use]
    pub const fn applies(self, right: bool) -> bool {
        match self {
            Self::False => false,
            Self::True => true,
            Self::Left => !right,
            Self::Right => right,
        }
    }
}

/// Whether the cursor blinks (ghostty's `cursor-style-blink`: unset, true, false).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum CursorBlink {
    /// The program decides (DECSCUSR); shells default to steady, editors often ask to blink.
    #[default]
    Program,
    /// Always blinks.
    Always,
    /// Never blinks.
    Never,
}

/// The cursor's shape (ghostty's `cursor-style`): the program's (DECSCUSR), or one of the
/// three fixed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum CursorStyle {
    /// The program decides.
    #[default]
    Program,
    /// A filled block.
    Block,
    /// A bar at the left edge.
    Bar,
    /// An underline.
    Underline,
}

impl CursorBlink {
    /// Whether the cursor blinks, given what the program asked for.
    #[must_use]
    pub const fn blinks(self, program: bool) -> bool {
        match self {
            Self::Program => program,
            Self::Always => true,
            Self::Never => false,
        }
    }
}

/// The quality a remote stream is opened at (the scale follows the canvas zoom, not this).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StreamPrefs {
    /// Frames per second.
    pub fps: u16,
    /// The bitrate ceiling, bits per second.
    pub max_bitrate_bps: u32,
    /// 10-bit HEVC.
    pub hdr: bool,
    /// A stream opens with its audio silenced on this client (the title-bar pill still
    /// toggles it).
    pub muted: bool,
}

impl Default for StreamPrefs {
    fn default() -> Self {
        Self { fps: 60, max_bitrate_bps: 30_000_000, hdr: false, muted: false }
    }
}

/// The whole theme.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Theme {
    /// Terminal colours.
    pub terminal: TerminalPalette,
    /// Terminal behaviour.
    pub behaviour: Behaviour,
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
            behaviour: Behaviour::default(),
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
    #[test]
    fn a_programs_colours_paint_over_the_themes() {
        let theme = TerminalPalette::DARK;
        let rgb = |r, g, b| Rgb { r, g, b };
        let set = ColorOverrides {
            fg: Some([1, 2, 3]),
            bg: None,
            cursor: Some([9, 9, 9]),
            palette: vec![(1, [4, 5, 6]), (17, [7, 8, 9])],
        };
        let colors = Colors::new(&theme, &set);
        assert_eq!(colors.resolve(Color::Default, false), rgb(1, 2, 3));
        assert_eq!(colors.resolve(Color::Default, true), theme.bg, "not set: the theme's");
        assert_eq!(colors.theme.cursor, rgb(9, 9, 9));
        assert_eq!(colors.theme.cursor_text, rgb(255, 255, 255), "text reads on a dark cursor");
        let white = ColorOverrides { cursor: Some([255; 3]), ..ColorOverrides::default() };
        assert_eq!(Colors::new(&theme, &white).theme.cursor_text, rgb(0, 0, 0));
        assert!(Rgb::hex(0xff_ffff).is_light() && !Rgb::hex(0x80_8080).is_light());
        assert_eq!(colors.resolve(Color::Palette(1), false), rgb(4, 5, 6));
        assert_eq!(colors.resolve(Color::Palette(17), false), rgb(7, 8, 9), "cube entries too");
        assert_eq!(colors.resolve(Color::Palette(18), false), theme.palette(18));
        assert_eq!(colors.resolve(Color::Rgb(7, 7, 7), false), rgb(7, 7, 7));
        let plain = Colors::from(&theme);
        assert_eq!(plain.resolve(Color::Palette(17), false), theme.palette(17));
        assert_ne!(plain, colors, "the shaped-word cache keys on the colours");
    }

    #[test]
    fn bold_is_bright_lifts_only_the_named_eight() {
        let mut theme = TerminalPalette::DARK;
        let colors = Colors::from(&theme);
        assert_eq!(colors.bold_slot(Color::Palette(1), true), Color::Palette(1), "off");
        theme.bold_is_bright = true;
        let colors = Colors::from(&theme);
        assert_eq!(colors.bold_slot(Color::Palette(1), true), Color::Palette(9));
        assert_eq!(colors.bold_slot(Color::Palette(1), false), Color::Palette(1), "not bold");
        assert_eq!(colors.bold_slot(Color::Palette(9), true), Color::Palette(9), "already bright");
        assert_eq!(colors.bold_slot(Color::Palette(196), true), Color::Palette(196), "the cube");
        assert_eq!(colors.bold_slot(Color::Default, true), Color::Default);
    }

    #[test]
    fn option_as_alt_applies_to_the_side_held() {
        for (setting, left, right) in [
            (OptionAsAlt::False, false, false),
            (OptionAsAlt::True, true, true),
            (OptionAsAlt::Left, true, false),
            (OptionAsAlt::Right, false, true),
        ] {
            assert_eq!(
                (setting.applies(false), setting.applies(true)),
                (left, right),
                "{setting:?}"
            );
        }
    }

    #[test]
    fn the_cursor_blink_override_beats_the_program() {
        assert_eq!(
            (CursorBlink::Program.blinks(true), CursorBlink::Program.blinks(false)),
            (true, false)
        );
        assert_eq!(
            (CursorBlink::Always.blinks(true), CursorBlink::Always.blinks(false)),
            (true, true)
        );
        assert_eq!(
            (CursorBlink::Never.blinks(true), CursorBlink::Never.blinks(false)),
            (false, false)
        );
    }

    /// Every chrome colour that is drawn as text reads on every chrome surface it can land on.
    ///
    /// The terminal grid has `minimum_contrast` to lift a cell whose colours the program chose;
    /// chrome has nothing of the kind, because these colours are ours and the fix is to pick
    /// better ones. WCAG AA for body text is 4.5:1, and the pairs are checked against all four
    /// surfaces rather than against the one they usually sit on: a status label follows its card,
    /// and the card can be on any of them.
    ///
    /// `warn` and `accent` on `overlay` read 3.93 and 3.83 in the light variant before this test
    /// existed. The dark variant already passed.
    #[test]
    fn chrome_text_clears_wcag_aa() {
        const AA: f32 = 4.5;
        for (name, s) in [("dark", Surfaces::DARK), ("light", Surfaces::LIGHT)] {
            let surfaces = [
                ("canvas", s.canvas),
                ("panel", s.panel),
                ("raised", s.raised),
                ("overlay", s.overlay),
            ];
            let inks = [
                ("text", s.text),
                ("text_secondary", s.text_secondary),
                ("text_muted", s.text_muted),
                ("success", s.success),
                ("warn", s.warn),
                ("error", s.error),
                ("accent", s.accent),
            ];
            for (ink, fg) in inks {
                for (surface, bg) in surfaces {
                    let ratio = fg.contrast(bg);
                    assert!(ratio >= AA, "{name}: {ink} on {surface} is {ratio:.2}, under {AA}");
                }
            }
            // The accent is also a surface of its own, with its own foreground on it.
            let on_accent = s.accent_fg.contrast(s.accent);
            assert!(on_accent >= AA, "{name}: accent_fg on accent is {on_accent:.2}");
        }
    }

    #[test]
    fn text_under_the_minimum_contrast_moves_toward_black_or_white() {
        let rgb = |r, g, b| Rgb { r, g, b };
        let (black, white) = (rgb(0, 0, 0), rgb(255, 255, 255));
        assert!((black.contrast(white) - 21.0).abs() < 0.01, "{}", black.contrast(white));
        assert!((white.contrast(white) - 1.0).abs() < f32::EPSILON);
        let mut theme = TerminalPalette::DARK;
        let navy = rgb(0, 0, 95);
        let off = Colors::from(&theme);
        assert_eq!(off.text_over(navy, black), navy, "1.0 keeps every colour");
        theme.minimum_contrast = 300;
        let on = Colors::from(&theme);
        let lifted = on.text_over(navy, black);
        assert!(lifted.contrast(black) >= 3.0, "{lifted:?}");
        assert!(lifted.contrast(black) < 3.2, "no further than it needs: {lifted:?}");
        assert!(lifted.b > lifted.r, "still blue: {lifted:?}");
        assert_eq!(on.text_over(navy, white), navy, "navy on white reads");
        let mint = rgb(0x80, 0xff, 0xea);
        let teal = on.text_over(mint, white);
        assert!(teal.contrast(white) >= 3.0 && teal.g > teal.r, "mint on white: {teal:?}");
        assert_eq!(on.text_over(theme.fg, theme.bg), theme.fg, "the theme itself reads");
        // A minimum past what black or white can give is as far as they go.
        theme.minimum_contrast = 2100;
        let most = Colors::from(&theme);
        assert_eq!(most.text_over(rgb(200, 200, 200), rgb(128, 128, 128)), black);
        // The minimum rides on the theme, so a program's colours are held to it too.
        theme.minimum_contrast = 300;
        let set = ColorOverrides { fg: Some([0, 0, 95]), ..ColorOverrides::default() };
        let program = Colors::new(&theme, &set);
        let fg = program.text_over(program.theme.fg, program.theme.bg);
        assert!(fg.contrast(program.theme.bg) >= 3.0, "{fg:?}");
    }

    /// Terminal text in the theme's own colours reads without any minimum contrast: every ANSI
    /// colour clears WCAG AA against the terminal background in both variants, except the one
    /// that names the background itself (black in dark, bright white in light), which programs
    /// use as a fill. The light brights read 3.0–3.6 before this test (GitHub light's).
    #[test]
    fn ansi_text_clears_wcag_aa_on_the_terminal_background() {
        const AA: f32 = 4.5;
        for (name, palette, namesake) in
            [("dark", TerminalPalette::DARK, 0), ("light", TerminalPalette::LIGHT, 15)]
        {
            let fg = palette.fg.contrast(palette.bg);
            assert!(fg >= AA, "{name}: the default text is {fg:.2}");
            for (ix, ink) in palette.ansi.iter().enumerate() {
                if ix == namesake {
                    continue;
                }
                let ratio = ink.contrast(palette.bg);
                assert!(
                    ratio >= AA,
                    "{name}: ANSI {ix} is {ratio:.2} on the background, under {AA}"
                );
            }
        }
    }
}
