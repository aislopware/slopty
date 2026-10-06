//! Design tokens. Toolkit-agnostic: plain numbers and RGB so `slopty-ui` (GPUI) and any other
//! consumer read the same values.
//!
//! Visual direction: Warp-like. A neutral surface ladder derived from the terminal's
//! background, one accent, hairlines and one elevation for what floats, a 4/8 pt spacing scale,
//! status colour only where it carries meaning, the terminal mono for terminal surfaces and the
//! system sans for chrome. The rulings are in `docs/decisions/ui.md` ("Design tokens" and "The
//! chrome is derived from the content"); chrome draws from these tokens and nothing else.

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

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

    /// The colour in OKLCH (Björn Ottosson's matrices), its hue in degrees from 0 to 360.
    #[must_use]
    pub fn oklch(self) -> Oklch {
        let linear = |c: u8| {
            let c = f32::from(c) / 255.0;
            if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        let (red, green, blue) = (linear(self.r), linear(self.g), linear(self.b));
        let long =
            0.051_445_99_f32.mul_add(blue, 0.412_221_46_f32.mul_add(red, 0.536_332_55 * green));
        let medium =
            0.107_396_96_f32.mul_add(blue, 0.211_903_5_f32.mul_add(red, 0.680_699_5 * green));
        let short =
            0.629_978_7_f32.mul_add(blue, 0.088_302_46_f32.mul_add(red, 0.281_718_84 * green));
        let (long, medium, short) = (long.cbrt(), medium.cbrt(), short.cbrt());
        let lightness = (-0.004_072_047_f32)
            .mul_add(short, 0.210_454_26_f32.mul_add(long, 0.793_617_8 * medium));
        let green_red =
            0.450_593_7_f32.mul_add(short, 1.977_998_5_f32.mul_add(long, -2.428_592_2 * medium));
        let blue_yellow = (-0.808_675_77_f32)
            .mul_add(short, 0.025_904_037_f32.mul_add(long, 0.782_771_77 * medium));
        Oklch {
            l: lightness,
            c: green_red.hypot(blue_yellow),
            h: blue_yellow.atan2(green_red).to_degrees().rem_euclid(360.0),
        }
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

    /// APCA lightness contrast (Lc) of `self` as text on `bg`, by APCA-W3 0.0.98G: about 106
    /// for black on white, −108 for white on black (light text on dark is negative), 0 below
    /// the least difference it counts.
    ///
    /// WCAG's ratio overstates dark pairs: it calls a dark tier the equal of its light twin
    /// where APCA reads it 20 to 30 Lc weaker, so the chrome's text clears both.
    #[must_use]
    pub fn apca(self, bg: Self) -> f32 {
        const BLACK_THRESHOLD: f32 = 0.022;
        const BLACK_CLAMP: f32 = 1.414;
        const SCALE: f32 = 1.14;
        const OFFSET: f32 = 0.027;
        const CLIP: f32 = 0.1;
        let y = |c: Self| {
            let lin = |v: u8| (f32::from(v) / 255.0).powf(2.4);
            let y = 0.072_175_f32
                .mul_add(lin(c.b), 0.715_152_2_f32.mul_add(lin(c.g), 0.212_672_9 * lin(c.r)));
            if y > BLACK_THRESHOLD { y } else { y + (BLACK_THRESHOLD - y).powf(BLACK_CLAMP) }
        };
        let (text, ground) = (y(self), y(bg));
        if (ground - text).abs() < 0.000_5 {
            return 0.0;
        }
        let lc = if ground > text {
            let sapc = (ground.powf(0.56) - text.powf(0.57)) * SCALE;
            if sapc < CLIP { 0.0 } else { sapc - OFFSET }
        } else {
            let sapc = (ground.powf(0.65) - text.powf(0.62)) * SCALE;
            if sapc > -CLIP { 0.0 } else { sapc + OFFSET }
        };
        lc * 100.0
    }
}

/// A colour in OKLCH: perceived lightness (0 to 1), chroma, and hue in degrees.
///
/// The brand is set in it (`docs/decisions/brand.md`), so the roles that carry the brand's
/// meaning are derived in it too: a lighter or darker green keeps the brand's hue and only
/// gives up chroma where sRGB runs out.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Oklch {
    /// Perceived lightness, 0 (black) to 1 (white).
    pub l: f32,
    /// Chroma: 0 is grey; sRGB greens reach about 0.3.
    pub c: f32,
    /// Hue, in degrees.
    pub h: f32,
}

impl Oklch {
    /// The colour in sRGB. Past sRGB's gamut the chroma is cut, never the lightness or the
    /// hue, until it fits.
    #[must_use]
    pub fn rgb(self) -> Rgb {
        let fits =
            |c: f32| linear_srgb(self.l, c, self.h).iter().all(|v| (-1e-4..=1.0001).contains(v));
        let chroma = if fits(self.c) {
            self.c
        } else {
            // Inside the gamut at no chroma, outside at the asked one: bisect for the edge.
            let (mut lo, mut hi) = (0.0_f32, self.c);
            for _ in 0..20 {
                let mid = f32::midpoint(lo, hi);
                if fits(mid) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lo
        };
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0 to 255")]
        let encode = |v: f32| {
            let v = v.clamp(0.0, 1.0);
            let e = if v <= 0.003_130_8 {
                12.92 * v
            } else {
                1.055_f32.mul_add(v.powf(1.0 / 2.4), -0.055)
            };
            (e * 255.0).round() as u8
        };
        let [r, g, b] = linear_srgb(self.l, chroma, self.h);
        Rgb { r: encode(r), g: encode(g), b: encode(b) }
    }
}

/// OKLCH to linear sRGB (Björn Ottosson's matrices), channels unclamped.
#[expect(clippy::many_single_char_names, reason = "Ottosson's own names: L, C, h, a and b")]
fn linear_srgb(l: f32, c: f32, h: f32) -> [f32; 3] {
    let (sin, cos) = h.to_radians().sin_cos();
    let (a, b) = (c * cos, c * sin);
    let l_ = 0.215_803_76_f32.mul_add(b, 0.396_337_78_f32.mul_add(a, l));
    let m_ = (-0.063_854_17_f32).mul_add(b, (-0.105_561_35_f32).mul_add(a, l));
    let s_ = (-1.291_485_5_f32).mul_add(b, (-0.089_484_18_f32).mul_add(a, l));
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        0.230_969_93_f32.mul_add(s3, 4.076_741_7_f32.mul_add(l3, -3.307_711_6 * m3)),
        (-0.341_319_4_f32).mul_add(s3, (-1.268_438_f32).mul_add(l3, 2.609_757_4 * m3)),
        1.707_614_7_f32.mul_add(s3, (-0.004_196_086_f32).mul_add(l3, -0.703_418_6 * m3)),
    ]
}

/// The chrome's text at a few hundredths of its strength: a hairline, or a state's wash (hover,
/// selected, pressed).
///
/// Laid over whatever it crosses, it sits the same step off every surface. A grey mixed for
/// the content sat a different step off each of the others, so a row's hover vanished on a
/// float that sat near it. `MonoCode`'s and Zed's borders, Linear's hover (the plane plus a
/// step) and Raycast's (white at 5 %) are drawn the same way.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Tint {
    /// What it is drawn in: the chrome's text.
    pub ink: Rgb,
    /// How strongly, 0 to 255, as a channel is.
    pub alpha: u8,
}

impl Tint {
    /// A hairline in `ink` at `share` (0 to 1) of its strength.
    #[must_use]
    pub fn of(ink: Rgb, share: f32) -> Self {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0 to 255")]
        let alpha = (share.clamp(0.0, 1.0) * 255.0).round() as u8;
        Self { ink, alpha }
    }

    /// Its opacity, 0 to 1.
    #[must_use]
    pub fn opacity(self) -> f32 {
        f32::from(self.alpha) / 255.0
    }

    /// The colour it shows over `surface`.
    #[must_use]
    pub fn over(self, surface: Rgb) -> Rgb {
        surface.mix(self.ink, self.opacity())
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

/// The dark terminal background: the content step of the chrome's surface order, which tile
/// headers and bodies share. A charcoal of the one neutral ([`NEUTRAL_HUE`]) at a chroma no eye
/// reads as hued, so the accent and the status colours are the only colour on screen; the
/// blue-grey `#16181d` it replaced blended with the blue accent (`docs/decisions/ui.md`, "The
/// dark theme is neutral", and "One warm neutral for both modes").
#[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
const DARK_BG: Rgb = Rgb::hex(0x181716);
/// The light terminal background: a hair off white in the one neutral, so what floats and what
/// is raised (white) rises from it by tone as well as by its ring and shadow.
#[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
const LIGHT_BG: Rgb = Rgb::hex(0xfdfcfb);

/// The hue of every neutral in both modes, in OKLCH degrees: a warm paper grey.
///
/// Barely tinted (chroma 0.002 to 0.004), as Linear's and Notion's neutrals are. Warm greys keep
/// the code hues (blue, cyan) reading as colour, and the brand's green sits on it as on paper; a
/// green tint would dull the one colour that means live.
pub const NEUTRAL_HUE: f32 = 85.0;

impl TerminalPalette {
    /// The default dark palette.
    #[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
    pub const DARK: Self = Self {
        fg: Rgb::hex(0xe6e6e6),
        bg: DARK_BG,
        // The text's own colour, as Ghostty's and Warp's are: blue left the controls, and a
        // cursor is the one control a terminal draws.
        cursor: Rgb::hex(0xe6e6e6),
        // A block cursor cuts its cell out of the background, as ghostty draws it.
        cursor_text: DARK_BG,
        // A neutral wash: colour is kept for meaning, and a selection means nothing but "this".
        selection: Rgb::hex(0x3a3a3a),
        search_match: Rgb::hex(0x4a4020),
        search_current: Rgb::hex(0x8c6a1f),
        ansi: [
            Rgb::hex(0x1c1c1c),
            Rgb::hex(0xf06c75),
            Rgb::hex(0x98c379),
            Rgb::hex(0xe5c07b),
            Rgb::hex(0x61afef),
            Rgb::hex(0xc678dd),
            Rgb::hex(0x56b6c2),
            Rgb::hex(0xcbcbcb),
            Rgb::hex(0x838383),
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
    /// The default light palette, generated from the dark one's hues: each colour keeps its
    /// slot's hue (green moves to the brand's, since at One Dark's 133° and this lightness it
    /// turns olive), the normals sit at OKLCH L 0.525 and the brights at 0.465, stronger on
    /// paper as the dark brights are stronger on black, with as much chroma as sRGB allows up to
    /// a cap. The greys are the one neutral; 7 is the ordinary text and 8 the dim slot, as in
    /// dark. `light_ansi_is_generated_from_the_dark_hues` regenerates every slot.
    #[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
    pub const LIGHT: Self = Self {
        fg: Rgb::hex(0x1c1c1a),
        bg: LIGHT_BG,
        cursor: Rgb::hex(0x1c1c1a),
        cursor_text: LIGHT_BG,
        selection: Rgb::hex(0xdbd9d5),
        search_match: Rgb::hex(0xffe9a8),
        search_current: Rgb::hex(0xf5b942),
        ansi: [
            Rgb::hex(0x1d1c1a),
            Rgb::hex(0xbf2239),
            Rgb::hex(0x068032),
            Rgb::hex(0x896301),
            Rgb::hex(0x0668c9),
            Rgb::hex(0x953aaf),
            Rgb::hex(0x007984),
            Rgb::hex(0x565553),
            Rgb::hex(0x747371),
            Rgb::hex(0xa70d2c),
            Rgb::hex(0x006d28),
            Rgb::hex(0x745300),
            Rgb::hex(0x0057ac),
            Rgb::hex(0x83259c),
            Rgb::hex(0x00666f),
            Rgb::hex(0x949392),
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
/// The chrome scale hangs off `ui_size` ([`Self::caption`], [`Self::small`], [`Self::roles`]),
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
    /// Line height of Markdown (a note, a code block, a plan), as a multiple of the font size.
    pub markdown_line_height: f32,
    /// Line height of the conversation's prose, read at length: looser than Markdown's, as
    /// the conversations of `MonoCode` and T3 Code are.
    pub prose_line_height: f32,
    /// UI family.
    pub ui_family: String,
    /// UI base size.
    pub ui_size: f32,
    /// What is read at length, in points: an agent's answers and the person's messages, and
    /// the composer they are written in. Its own setting, so reading can grow while the chrome
    /// keeps its size.
    pub prose_size: f32,
}

impl Typography {
    /// "This one": the selected row, the focused title, the active tab, a button's words, an
    /// approval's statement. It says which without shouting, where the strong weight is kept
    /// for titles.
    pub const MEDIUM_WEIGHT: f32 = 500.0;
    /// Titles: a dialog's or a panel's, the first run's heading, Markdown headings. Chrome has
    /// no bold.
    pub const STRONG_WEIGHT: f32 = 600.0;

    /// The smallest chrome size: HUD readouts, timestamps, counts, chevrons (base − 2). At 10 it
    /// blurred under the dark theme's glyph thickening and read as a footnote.
    #[must_use]
    pub fn caption(&self) -> f32 {
        (self.ui_size - 2.0).max(6.0)
    }

    /// Secondary chrome (base − 1): bar labels, pills, folds, tool summaries, section labels,
    /// a row's second line, a bar's readouts, a status word. `MonoCode`'s most used size: 12 is
    /// the chrome's workhorse, and a row's facts are told from its title by their tone, not a
    /// smaller size.
    #[must_use]
    pub fn small(&self) -> f32 {
        (self.ui_size - 1.0).max(7.0)
    }

    /// Prose read at length: an assistant's answer and the prompt it answers, at the regular
    /// weight on [`Self::prose_line_height`]; [`Self::prose_size`], never under 6.
    #[must_use]
    pub const fn prose(&self) -> f32 {
        self.prose_size.max(6.0)
    }

    /// An icon beside chrome text: a tile's kind, a status mark, a bar button (base + 1).
    #[must_use]
    pub fn icon(&self) -> f32 {
        self.ui_size + 1.0
    }

    /// An icon that stands alone: an empty state, a title-bar button (base + 3).
    #[must_use]
    pub fn icon_large(&self) -> f32 {
        self.ui_size + 3.0
    }
}

/// A type role: a size, the line it sits on, and its weight, in points at zoom 1.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TypeRole {
    /// The text's size.
    pub size: f32,
    /// The height of its line.
    pub line: f32,
    /// Its weight: 400, [`Typography::MEDIUM_WEIGHT`] or [`Typography::STRONG_WEIGHT`].
    pub weight: f32,
}

/// The type roles: what a piece of text is, each its size, line and weight, for a pointer or a
/// finger ([`Typography::roles`]).
///
/// One scale for the chrome made the work itself read as settings: a task's name and a
/// request's question sat at the size of the facts round them. A role says what the text is,
/// and its numbers follow: the desktop's from the 13 pt chrome, the touch ones a step larger
/// so a label has the presence its 44 pt row gives it. Each follows the chrome size setting,
/// and prose its own.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TypeRoles {
    /// A timestamp or a dense diagnostic figure, which can be left out: 11/16. Never a reason
    /// or a scope the person must read.
    pub caption: TypeRole,
    /// Facts about a thing: where it is, when, its counts. 12/18.
    pub metadata: TypeRole,
    /// A control's or a row's words: 13/19.
    pub chrome: TypeRole,
    /// The same, for what acts or is chosen: 13/19 at the medium weight.
    pub action: TypeRole,
    /// What a piece of work is called where it is the thing to act on: a task, a request
    /// put to the person. 14/20 at the medium weight.
    pub task_title: TypeRole,
    /// A section's heading in a list or a panel: 12/16 at the medium weight, drawn a tier under
    /// the words it heads, as a list's label is in t3code and zeron. It leads its group by its
    /// place and its tone, not by bold.
    pub section: TypeRole,
    /// A panel's or a dialog's title: 15/20 at the strong weight.
    pub panel_title: TypeRole,
    /// What is read at length, at [`Typography::prose_size`]: 14/22.
    pub prose: TypeRole,
    /// A page's heading inside the window: 20/26 at the strong weight.
    pub page_heading: TypeRole,
    /// The heading of a page that is the whole window, the first run: 26/32 at the medium
    /// weight, SF's Display cut. Without letter spacing the strong weight read loose.
    pub first_run: TypeRole,
}

impl Typography {
    /// The regular weight.
    pub const REGULAR_WEIGHT: f32 = 400.0;

    /// The type roles for a pointer (`touch` false) or a finger.
    #[must_use]
    pub fn roles(&self, touch: bool) -> TypeRoles {
        let base = self.ui_size;
        // (size over the chrome size, leading) for a pointer, then for a finger.
        let role = |desk: (f32, f32), finger: (f32, f32), weight: f32| {
            let (step, leading) = if touch { finger } else { desk };
            let size = (base + step).max(6.0);
            TypeRole { size, line: size + leading, weight }
        };
        let (regular, medium, strong) =
            (Self::REGULAR_WEIGHT, Self::MEDIUM_WEIGHT, Self::STRONG_WEIGHT);
        let prose = self.prose() + if touch { 2.0 } else { 0.0 };
        TypeRoles {
            caption: role((-2.0, 5.0), (-1.0, 5.0), regular),
            metadata: role((-1.0, 6.0), (0.0, 5.0), regular),
            chrome: role((0.0, 6.0), (4.0, 5.0), regular),
            action: role((0.0, 6.0), (4.0, 5.0), medium),
            task_title: role((1.0, 6.0), (4.0, 5.0), medium),
            section: role((-1.0, 4.0), (0.0, 5.0), medium),
            panel_title: role((2.0, 5.0), (6.0, 5.0), strong),
            prose: TypeRole {
                size: prose,
                line: (prose * self.prose_line_height).round(),
                weight: regular,
            },
            page_heading: role((7.0, 6.0), (9.0, 6.0), strong),
            first_run: role((13.0, 6.0), (15.0, 6.0), medium),
        }
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
            prose_line_height: 1.6,
            ui_family: ".SystemUIFont".to_owned(),
            ui_size: 13.0,
            prose_size: 14.0,
        }
    }
}

/// Corner radii, in points: `MonoCode`'s, 6 the most used, then 8, 12, full and 4.
///
/// Two families: 6 at rest and 12 for what floats. A floating shell is its rows' radius plus
/// the pad round them (6 + 6), so the corners nest ([`Radii::nested`]).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Radii {
    /// Key caps, chips, inline code.
    pub xs: f32,
    /// The default: buttons, fields, rows (their hover and selected fills), tabs, a file's card
    /// in a diff.
    pub sm: f32,
    /// Framed blocks inside content: a code block, a command's output.
    pub md: f32,
    /// Everything that floats: the palette, menus, dialogs, the inbox, a toast, the composer.
    pub lg: f32,
    /// A capsule: a dot, a send or stop disc, a scroll pill. Larger than any side it rounds,
    /// so the ends are half circles.
    pub full: f32,
}

impl Radii {
    /// The radius of what sits `inset` inside a `card` of radius `card` whose border is
    /// `border` wide, so the two corners share a centre and the gap between them is even: a
    /// menu's rows inside the menu, a field inside the composer. Never under zero.
    #[must_use]
    pub fn nested(card: f32, border: f32, inset: f32) -> f32 {
        (card - border - inset).max(0.0)
    }
}

impl Default for Radii {
    fn default() -> Self {
        Self { xs: 4.0, sm: 6.0, md: 8.0, lg: 12.0, full: 9_999.0 }
    }
}

/// The spacing scale, in points: the only paddings and gaps chrome uses.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Spacing {
    /// 2: pill vertical padding, hairline-adjacent gaps.
    pub xxs: f32,
    /// 4: tight gaps, inline button padding.
    pub xs: f32,
    /// 8: the base unit — row gaps, button padding.
    pub sm: f32,
    /// 12: panel horizontal padding, section gaps, a tile's inset.
    pub md: f32,
    /// 16: panel padding.
    pub lg: f32,
    /// 24: dialog padding.
    pub xl: f32,
    /// 32: a page's margins, an empty state's room.
    pub xxl: f32,
    /// 48: a page's top, a reading column's gutters.
    pub xxxl: f32,
}

impl Spacing {
    /// The one edge grid: how far in from its region's edge every leading edge starts. A
    /// tile's header, a terminal's grid, a note's and a file's text, the navigator's rows, the
    /// palette's, the inbox's and the status bar's all start here, so they share one edge,
    /// clear of the dividers between flush panes.
    #[must_use]
    pub const fn inset(&self) -> f32 {
        self.md
    }

    /// How far a row's trailing edge sits in (6): half the leading inset, as `MonoCode`'s rows
    /// are padded 12 and 6. What ends a row is usually an icon button, whose own square adds
    /// the rest, so its glyph lands near the leading inset from the edge.
    #[must_use]
    pub const fn inset_trailing(&self) -> f32 {
        self.md / 2.0
    }
}

impl Default for Spacing {
    fn default() -> Self {
        Self { xxs: 2.0, xs: 4.0, sm: 8.0, md: 12.0, lg: 16.0, xl: 24.0, xxl: 32.0, xxxl: 48.0 }
    }
}

/// Line widths, in points.
pub mod stroke {
    /// A hairline: half a point, one device pixel at every scale.
    ///
    /// GPUI rounds a stroke to whole device pixels, never under one, so it is one pixel on a
    /// Retina screen, on a 1x one and on a 3x phone alike. Linear draws its borders this way; at a
    /// full point every rule on a Retina screen was two pixels and read as a ruled form. The
    /// hairline shares are set for this width.
    pub const HAIR: f32 = 0.5;
    /// A point: a line that is a thing's own edge rather than a divider.
    ///
    /// Seen whole: an unticked box's outline, a drop target's ring, the cut round a badge.
    pub const EDGE: f32 = 1.0;
    /// A line that marks one thing among its peers.
    ///
    /// The focused tile's header, the shown tab of the focused column. At 2 pt in the text's tone
    /// it was the loudest stroke on screen; every reference's "current" marker is the quietest
    /// that still reads.
    pub const MARK: f32 = 1.5;
}

/// How much of the focus tone a focused field's edge starts from ([`Surfaces::field_focus`]):
/// a mid tone, so the field says it has the keyboard without the weight of the text colour.
pub const FIELD_FOCUS: f32 = 0.45;

/// Opacities for tints and washes over a surface: one ladder, used everywhere, so the chrome
/// reads as one surface rather than a collection of one-off transparencies.
pub mod alpha {
    /// An edge that catches the light: the white line along the top of a dark floating
    /// surface.
    pub const EDGE: f32 = 0.06;
    /// The rim of what rests, a step under [`EDGE`]: light along a dark card's top, a shade
    /// along a light one's bottom.
    pub const RIM: f32 = 0.04;
    /// Barely there: a selected row, the faint fill of a quiet pill, the hover wash over a
    /// bare button, a wash across the terminal grid (a block separator, the visual bell), a
    /// changed line's wash in a diff.
    pub const FAINT: f32 = 0.12;
    /// The words that changed inside a changed line of a diff, over the line's own
    /// [`FAINT`] wash: Zed's Delta measures about 0.34, git-delta's emphasis sits about 2.5
    /// times its line's luminance off it.
    pub const EMPH: f32 = 0.34;
    /// A step past [`FAINT`]: the neutral solid pressed, given this far toward its plane.
    pub const DIM: f32 = 0.16;
    /// [`FAINT`] on white: a quiet pill's fill in light, where a saturated tone at 0.12 reads
    /// heavier than it does on near-black.
    pub const FAINT_ON_PAPER: f32 = 0.10;
    /// A tint that has to be seen: answer buttons, a selection.
    pub const TINT: f32 = 0.25;
    /// A tint under the pointer, a scrollbar thumb.
    pub const PRESSED: f32 = 0.4;
    /// A neutral ring that marks a choice without meaning anything.
    ///
    /// The overview's active workspace, in the text's tone, where the accent would say
    /// "done". At 0.5 round a
    /// point and a half it read as the old heavy outline of a chosen card; Geist and Radix
    /// draw theirs at about 15 to 30 %.
    pub const RING: f32 = 0.3;
    /// The window under a dark modal or sheet: Linear dims under 0.4, shadcn far less; at 0.6
    /// the settings blacked out the work behind them.
    pub const SCRIM: f32 = 0.45;
    /// The window under a light modal or sheet, dimmed in the warm ink.
    ///
    /// About as far as Notion's light overlay (0.24 of a near-black), so the sheet leads and the
    /// work behind it keeps its own warm material rather than turning a flat grey.
    pub const SCRIM_ON_PAPER: f32 = 0.20;
    /// The work under a dark floating sidebar (a phone's drawer): half a modal's.
    ///
    /// The sidebar is a way through the work, not a question over it, as iOS dims only lightly
    /// under the sidebar it floats over a compact window.
    pub const SCRIM_ASIDE: f32 = 0.22;
    /// The work under a light floating sidebar, in the warm ink.
    pub const SCRIM_ASIDE_ON_PAPER: f32 = 0.10;
    /// Half way: the pointer over a light canvas row, half way up to the raised selection.
    pub const HALF: f32 = 0.5;
    /// Present but set back: a read row in the inbox.
    pub const STRONG: f32 = 0.7;
    /// The docked navigator's canvas over the system's sidebar material (macOS).
    ///
    /// The lowest share, to 0.02, at which the chrome's text keeps its floors over any wallpaper,
    /// white or black ([`crate::Surfaces::on_glass`] lifting it): t3code's glass is 0.80. At nine
    /// tenths the material barely showed.
    pub const GLASS: f32 = 0.82;
    /// An unlit dot of the mark on a dark surface (the brand's ink plate).
    pub const UNLIT: f32 = 0.2;
    /// An unlit dot of the mark on a light surface, where 0.2 fades into the paper.
    pub const UNLIT_ON_PAPER: f32 = 0.3;
}

/// Slopty's green: OKLCH 0.72 0.16 150 (`docs/decisions/brand.md`).
pub const BRAND: Rgb = Rgb::hex(0x004a_c06c);

/// WCAG AA for body text: the least contrast chrome text has on any surface it lands on.
const AA: f32 = 4.5;

/// WCAG's least contrast for what is seen but not read (1.4.11): a control's outline always
/// reaches it.
const NON_TEXT: f32 = 3.0;

/// APCA's floor for secondary text on every ground it lands on: its 60 for content text that
/// is not body, less 5 for chrome set at 12 to 13 pt in the medium weight.
const SECONDARY_LC: f32 = 55.0;

/// APCA's floor for muted text: a level under [`SECONDARY_LC`], still past its 45 for large or
/// heavy text, which hints and ages read beside.
const MUTED_LC: f32 = 45.0;

/// How far apart two text levels stay: each reads at least a quarter again the contrast of the
/// level under it, on the surface where both read worst. Past it, muted and secondary text
/// would be two names for one grey.
const LEVEL: f32 = 1.25;

/// Surface colours for chrome (not the terminal grid), derived from the content they frame.
///
/// The window reads in steps of elevation, lighter as they rise in both variants: the bars on
/// `canvas`, the navigator on `panel`, tile headers and bodies on the content step
/// ([`Theme::content`], the terminal's own background), and what floats over them (the
/// palette, menus, dialogs, popovers, hints) on `elevated`, a clear step (+5 OKLCH L in dark)
/// above the content. The states ride on whichever plane they land on: `hover`, `selected` and
/// `pressed` are washes of the chrome's text, each a step past the last, so a menu row, a card
/// and a navigator row answer the pointer alike.
///
/// Nothing here is picked by hand: [`Surfaces::derive`] computes the fills and hairlines from
/// the content and the chrome's text at fixed steps, and lifts each text tone until it clears
/// WCAG AA on every surface it can land on. A terminal background set in the settings moves
/// the whole chrome with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Surfaces {
    /// The lowest step: the window, the title bar and the status bar.
    pub canvas: Rgb,
    /// One step up: the navigator, unfocused tile headers, the composer.
    pub panel: Rgb,
    /// Above the content: the palette, menus, dialogs, popovers and hints, anything that
    /// floats. It wears [`Elevation::shadow`] and the `border` hairline.
    pub elevated: Rgb,
    /// The pointer over a row or a button, and a resting well (a secondary button, a code
    /// block, a chip, a field's ground): the ink's first wash, over whatever plane it lands on.
    pub hover: Tint,
    /// The chosen one of a list (a selected row, a toggle that is on, a menu that is open, a
    /// key cap's plate): a step past `hover`.
    pub selected: Tint,
    /// What is held down: a step past `selected`, so a press reads apart from a hover.
    pub pressed: Tint,
    /// The chosen row of a list that lies on the canvas (the docked navigator, the settings
    /// sheet's sections). In light it rises to the raised surface, white on the dimmer canvas,
    /// as t3code's sidebar lifts its selection; a grey wash on a grey plane read dull. In dark
    /// it is [`Self::selected`]: a lift on near-black is a wash.
    pub chosen_on_canvas: Tint,
    /// The pointer over a row on the canvas: half way up to [`Self::chosen_on_canvas`] in
    /// light, a step up and not down; [`Self::hover`] in dark.
    pub hover_on_canvas: Tint,
    /// A terminal block's head band, the rows its command was typed on: the content with a few
    /// hundredths of the ink, as Zed's active line is, in both variants. It is its own step so
    /// a band under an unfocused header (on `panel`) does not read as a second header, and so it
    /// shows on white, where `panel` sat 1.5 L* off the content and only its rule was seen.
    pub band: Rgb,
    /// The hairlines that divide regions: between panes, under a bar, round a popover.
    pub border: Tint,
    /// The quieter hairline inside one region: between rows or groups of a list, under a
    /// tab row, between a panel's sections.
    pub border_subtle: Tint,
    /// The outline of a control that is nothing without it, an unticked box: it reads 3:1 on
    /// every surface it can sit on, WCAG's least for a control's edge (1.4.11), where a
    /// dividing hairline is only seen.
    pub control: Tint,
    /// Primary text.
    pub text: Rgb,
    /// Labels, tool summaries, counts.
    pub text_secondary: Rgb,
    /// Hints, timestamps, folds, inactive titles, second lines.
    pub text_muted: Rgb,
    /// The interaction accent, the brand's green as text and lines: a link in prose, a mark
    /// that says "live" or "chosen". Spent sparingly: the primary action is [`Self::solid`],
    /// and the keyboard's ring is [`Self::focus`], not this.
    pub accent: Rgb,
    /// Connected, agent done, lines added: as text. Its own seed, the brand's green as the
    /// accent's is for now, so a done state and a live mark can part without touching every
    /// use of either.
    pub success: Rgb,
    /// The keyboard's ring and any outline that says "this has the keyboard": the chrome's
    /// text, neutral, so green keeps meaning live and done. Drawn at `alpha::STRONG`, it clears
    /// 3:1 on every surface it can ring (WCAG 2.4.13).
    pub focus: Rgb,
    /// Agent waiting, "N need you", muted, reconnecting: as text.
    pub warn: Rgb,
    /// A failed result or command, a pairing error: as text.
    pub error: Rgb,
    /// The accent as a mark: an unseen dot, a busy bar, a progress line, a drop wash. Never a
    /// control's fill: a control that is on or primary takes [`Self::solid`].
    pub accent_fill: Rgb,
    /// Success as a mark: a dot, a bar, a badge, an added line's wash.
    pub success_fill: Rgb,
    /// Warn as a mark: the attention bar, the bell's badge, a dot, a wash. Amber in both
    /// variants, where the `warn` text tone is a dark ochre in the light one.
    pub warn_fill: Rgb,
    /// Error as a mark: a failed block's wash, a failed glyph, a badge.
    pub error_fill: Rgb,
    /// Working as text, for the rare word that has to say it in its hue. Blue.
    pub working: Rgb,
    /// Working as a mark: the working cell's dots in lists, headers, the board and the rollups,
    /// so the most common state reads apart from idle at a glance. Blue, never the accent, a
    /// link or the focus.
    pub working_fill: Rgb,
    /// Merged as text, for the rare word that has to say it in its hue. Violet.
    pub merged: Rgb,
    /// Merged as a mark: a merged task's check, a merged pull request's glyph. Violet, as
    /// GitHub's merged is.
    pub merged_fill: Rgb,
    /// A machine's or a project's own colour, on its glyph alone, picked by a hash of its id
    /// (`slopty_ui::kit::identity_ink`). Eight hues near the status fills' lightness at under
    /// half their chroma (0.06 dark, 0.07 light), each kept clear of every status hue: a tint that
    /// tells machines apart side by side and never competes with a state.
    pub identity: [Rgb; IDENTITY_HUES],
    /// Text on the success, warn and error fills: a badge's count.
    pub fill_fg: Rgb,
    /// Text on the accent fill: a badge's count on the green. A near-black green in both
    /// variants, since white does not read on a green this light.
    pub accent_ink: Rgb,
    /// The neutral solid: the one primary action of a surface, the send and stop disc, a ticked
    /// box, a switch that is on, a key that is armed. The chrome's text as a fill, white on
    /// dark and near-black on light, as `MonoCode`'s stop and Commit buttons are.
    pub solid: Rgb,
    /// Text and glyphs on [`Self::solid`]: the darkest chrome step in dark, white in light.
    pub solid_ink: Rgb,
    /// Slopty's green, the mark's lit dots: the same in both variants, as a brand colour is
    /// (`docs/decisions/brand.md`, OKLCH 0.72 0.16 150).
    pub brand: Rgb,
    /// What a remote window or display sits on where its picture does not reach: a neutral
    /// near-black in both variants, as every remote-desktop client letterboxes. A remote screen
    /// is media, and dark bars keep its edge read as a screen's (`docs/decisions/video.md`,
    /// "Remote pictures sit on a dark stage").
    pub stage: Rgb,
}

/// [`Surfaces::stage`] in the standard look.
pub const STAGE: Rgb = Rgb::hex(0x000a_0a0a);

/// What a ladder step is mixed toward from the content.
#[derive(Clone, Copy, Debug)]
enum Toward {
    /// The chrome's text.
    Ink,
    /// Black.
    Black,
    /// White.
    White,
}

/// A step of the ladder: `share` (0 to 1) of the way from the content toward something.
#[derive(Clone, Copy, Debug)]
struct Step {
    toward: Toward,
    share: f32,
}

const fn ink(share: f32) -> Step {
    Step { toward: Toward::Ink, share }
}

/// The inputs of one variant: its steps, its tones before they are lifted, and its fills.
#[derive(Clone, Copy, Debug)]
struct Tones {
    canvas: Step,
    panel: Step,
    elevated: Step,
    /// The state washes: shares of the text laid over whatever plane they land on.
    hover: f32,
    selected: f32,
    pressed: f32,
    band: Step,
    border: Step,
    border_subtle: Step,
    /// Where text moves when it has to read better: the pole away from the surfaces.
    pole: Rgb,
    text: Rgb,
    /// The text tiers as shares of the text over the content (`MonoCode`'s `content/70`),
    /// before they are lifted.
    text_secondary: f32,
    text_muted: f32,
    /// The brand's green as text, and as a mark: the accent.
    accent: Oklch,
    accent_fill: Oklch,
    /// Done and good as text, and as a mark: its own seed, the accent's green for now.
    success: Oklch,
    success_fill: Oklch,
    /// Text on the green mark.
    accent_ink: Oklch,
    warn: Rgb,
    error: Rgb,
    warn_fill: Rgb,
    error_fill: Rgb,
    /// Working as text and as a mark.
    working: Oklch,
    working_fill: Oklch,
    /// Merged as text and as a mark.
    merged: Oklch,
    merged_fill: Oklch,
    /// The identity hues, at the fills' lightness.
    identity: [Oklch; IDENTITY_HUES],
    fill_fg: Rgb,
    /// The solid's ink: the far end of the ladder from the text.
    solid_ink: Step,
}

/// Slopty's green in OKLCH (`docs/decisions/brand.md`): [`BRAND`] is its sRGB.
pub const BRAND_OKLCH: Oklch = Oklch { l: 0.72, c: 0.16, h: 150.0 };

/// A near-black of the brand's hue: words on a green mark, in both variants.
const ON_GREEN: Oklch = Oklch { l: 0.22, c: 0.04, h: BRAND_OKLCH.h };

/// How many identity colours there are ([`Surfaces::identity`]).
pub const IDENTITY_HUES: usize = 8;

/// The identity hues, in degrees, with their chroma as a share of the mode's: orange, lime,
/// teal, sky, indigo, magenta, pink, and a sand at a third of it. Red, amber, green, blue and
/// violet are the states' and are left out; each hue sits 20 degrees or more from every
/// status hue, the sand excepted, since at its chroma it reads as no state. So indigo sits at
/// 272, not 270, where a channel's rounding could bring it under 20 from working's 250, and
/// the purple is a magenta at 325, since at 310 it sat 10 from merged's violet.
const IDENTITY: [(f32, f32); IDENTITY_HUES] = [
    (50.0, 1.0),
    (125.0, 1.0),
    (185.0, 1.0),
    (225.0, 1.0),
    (272.0, 1.0),
    (325.0, 1.0),
    (355.0, 1.0),
    (85.0, 0.05 / 0.13),
];

/// The identity hues at lightness `l` and chroma `c`.
const fn identity(light: f32, chroma: f32) -> [Oklch; IDENTITY_HUES] {
    const fn one(light: f32, chroma: f32, (hue, share): (f32, f32)) -> Oklch {
        Oklch { l: light, c: chroma * share, h: hue }
    }
    let [orange, lime, teal, sky, indigo, magenta, pink, sand] = IDENTITY;
    [
        one(light, chroma, orange),
        one(light, chroma, lime),
        one(light, chroma, teal),
        one(light, chroma, sky),
        one(light, chroma, indigo),
        one(light, chroma, magenta),
        one(light, chroma, pink),
        one(light, chroma, sand),
    ]
}

/// Working's hue, a blue (OKLCH), and merged's, a violet.
const WORKING_HUE: f32 = 250.0;
const MERGED_HUE: f32 = 300.0;

/// Dark: the bars and the navigator sink toward black, everything above the content climbs
/// toward the text.
#[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
const DARK_TONES: Tones = Tones {
    // One notch under the content, not three: at 0.66 the bars framed the window in black.
    // The panel sits two L* under the content, as the light one does, so an unfocused header
    // still reads as one; at 0.12 it was 1.5. On the neutral content 0.28 left the panel
    // 0.82 L* over the bars, under the one unit that shows; 0.30 clears it.
    canvas: Step { toward: Toward::Black, share: 0.30 },
    panel: Step { toward: Toward::Black, share: 0.16 },
    // What floats sits +5 OKLCH L over the content, near Radix's and shadcn's step: with the
    // states as washes over their plane, a menu row's hover still shows on it (+4 L). When the
    // states were solid steps the float had to sit under the hover step, at +3, and read as
    // barely lifted.
    elevated: ink(0.055),
    // `MonoCode`'s ladder: hover at 5 % of the text, selection at 8.5 %, and pressed a like
    // step past it, so a press reads apart from a hover.
    hover: 0.05,
    selected: 0.085,
    pressed: 0.12,
    // The hairlines at half a point (one device pixel), their shares a half again the 7 % and
    // 4.5 % they had at a full point, as Linear sets its thin borders, so they weigh about what
    // they did at half the stroke.
    border_subtle: ink(0.065),
    band: ink(0.035),
    border: ink(0.10),
    pole: Rgb::hex(0xffffff),
    text: Rgb::hex(0xecebea),
    // `content/74.5` and `content/65.1`: the least shares at which secondary text reads APCA
    // |Lc| 55 and muted 45 on the selected wash over a float (the lightest ground text lands
    // on), so the lift has nothing to do. WCAG AA alone put them at `/71.5` and `/62.5`, which
    // APCA reads at Lc 53 and 43 there. `MonoCode`'s `/70` and `/55` sat on solid fills under
    // +3 floats.
    text_secondary: 0.745,
    text_muted: 0.651,
    accent: BRAND_OKLCH,
    accent_fill: BRAND_OKLCH,
    success: BRAND_OKLCH,
    success_fill: BRAND_OKLCH,
    accent_ink: ON_GREEN,
    warn: Rgb::hex(0xe5c07b),
    // A light coral, near Radix's dark red 11 (`ff9592`): at One Dark's lightness (`f27d84`)
    // the red sat 0.02 Oklab from the green for a deuteranope, one just-noticeable step, so
    // lines added and removed read alike; a step lighter keeps them apart for every
    // dichromacy (`vision`), and reads well past APCA |Lc| 45 on a float's selected wash.
    error: Rgb::hex(0xff9095),
    warn_fill: Rgb::hex(0xf5b83d),
    error_fill: Rgb::hex(0xf0555f),
    // The new hues at the green's lightness as a mark (0.72) and Radix's step 11 as text (0.80).
    // Chroma goes by urgency: working and merged are calm states the person need not act on, so
    // their marks sit under the amber and red at full chroma (0.11 and 0.09), every hue kept.
    working: Oklch { l: 0.80, c: 0.11, h: WORKING_HUE },
    working_fill: Oklch { l: 0.72, c: 0.11, h: WORKING_HUE },
    merged: Oklch { l: 0.80, c: 0.11, h: MERGED_HUE },
    merged_fill: Oklch { l: 0.72, c: 0.09, h: MERGED_HUE },
    // A tint that tells machines apart side by side, not a state's chroma.
    identity: identity(0.72, 0.06),
    fill_fg: Rgb::hex(0x0a0b0e),
    solid_ink: Step { toward: Toward::Black, share: 0.30 },
};

/// Light: every step below the content darkens toward the text; what floats goes to white.
#[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
const LIGHT_TONES: Tones = Tones {
    // Near-white chrome and a zinc-200 hairline: at 0.08 and 0.21 the bars were a grey slab
    // under a darker rule than any reference draws.
    canvas: ink(0.04),
    panel: ink(0.025),
    // White, a step over the content: what floats and what is raised rises by tone as well as
    // by its ring and shadow. At 0.6 over a white content it was white on white.
    elevated: Step { toward: Toward::White, share: 1.0 },
    hover: 0.05,
    selected: 0.08,
    pressed: 0.12,
    // The dark ladder's hairlines a third again as strong, as `MonoCode`'s light hairlines
    // are (on white the same share reads fainter than on near-black), and a half again for
    // the half-point stroke.
    border_subtle: ink(0.085),
    band: ink(0.045),
    border: ink(0.13),
    pole: Rgb::hex(0x000000),
    text: Rgb::hex(0x1c1c1a),
    // Where the lift would put them on paper: secondary at the level gap under the text, muted
    // AA on the selected wash over the bars. Light's tiers are pinned there, so its hierarchy
    // comes from weight and placement too.
    text_secondary: 0.745,
    text_muted: 0.68,
    // The brand's hue at the lightness that reads AA on paper as text, with as much chroma as
    // sRGB holds there, and 3:1 as a mark on the paper: a word is darkened only to its floor,
    // never greyed.
    accent: Oklch { l: 0.49, c: 0.135, h: BRAND_OKLCH.h },
    accent_fill: Oklch { l: 0.64, c: 0.16, h: BRAND_OKLCH.h },
    success: Oklch { l: 0.49, c: 0.135, h: BRAND_OKLCH.h },
    success_fill: Oklch { l: 0.64, c: 0.16, h: BRAND_OKLCH.h },
    accent_ink: ON_GREEN,
    // A touch more amber than brown, so waiting reads as amber and never as a brown.
    warn: Rgb::hex(0x8a5600),
    // A crimson: at the green's and the amber's lightness (`c7212c`) a deuteranope saw the
    // amber and the red as one brown (0.03 Oklab apart); this lightness keeps failed apart
    // from waiting and from added for every dichromacy (`vision`), and the chroma keeps it red
    // rather than the oxblood `8f1d1d` it replaced. The colour-blind test set its lightness,
    // `oklch(0.41 0.165 25)`: with the green word at full chroma, the lighter crimsons
    // (`940018`, `940c19`) met a deuteranope's floor against it only to the third place.
    error: Rgb::hex(0x8f0214),
    warn_fill: Rgb::hex(0xf0a000),
    error_fill: Rgb::hex(0xef4b52),
    // A mark at 0.58, 3:1 and more on paper with the most chroma it holds there, and a word
    // at 0.50, AA on paper.
    working: Oklch { l: 0.50, c: 0.15, h: WORKING_HUE },
    // Calm states a step under the urgent ones' chroma, as in dark.
    working_fill: Oklch { l: 0.58, c: 0.12, h: WORKING_HUE },
    merged: Oklch { l: 0.50, c: 0.15, h: MERGED_HUE },
    merged_fill: Oklch { l: 0.58, c: 0.10, h: MERGED_HUE },
    identity: identity(0.60, 0.07),
    fill_fg: Rgb::hex(0x0a0b0e),
    solid_ink: Step { toward: Toward::White, share: 1.0 },
};

/// The least contrast `fg` has on any of `surfaces`.
fn worst(fg: Rgb, surfaces: &[Rgb]) -> f32 {
    surfaces.iter().map(|&bg| fg.contrast(bg)).fold(f32::INFINITY, f32::min)
}

/// The least APCA lightness contrast, as a magnitude, `fg` has on any of `surfaces`.
fn worst_lc(fg: Rgb, surfaces: &[Rgb]) -> f32 {
    surfaces.iter().map(|&bg| fg.apca(bg).abs()).fold(f32::INFINITY, f32::min)
}

/// `fg`, moved toward `pole` only as far as it takes to read `least` on every one of
/// `surfaces`, so its hue survives: the pole itself when even that is not enough.
fn lift(fg: Rgb, surfaces: &[Rgb], pole: Rgb, least: f32) -> Rgb {
    lift_to(fg, surfaces, pole, Floor { ratio: least, lc: 0.0 })
}

/// What a text tone must clear on every ground: a WCAG ratio and an APCA |Lc|.
#[derive(Clone, Copy, Debug)]
struct Floor {
    ratio: f32,
    lc: f32,
}

impl Floor {
    fn met(self, fg: Rgb, surfaces: &[Rgb]) -> bool {
        worst(fg, surfaces) >= self.ratio && worst_lc(fg, surfaces) >= self.lc
    }
}

/// [`lift`] to the stricter of a WCAG ratio and an APCA floor.
fn lift_to(fg: Rgb, surfaces: &[Rgb], pole: Rgb, floor: Floor) -> Rgb {
    if floor.met(fg, surfaces) {
        return fg;
    }
    if !floor.met(pole, surfaces) {
        return pole;
    }
    // Both grow with the mix toward the pole: bisect for the least mix that reads.
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    for _ in 0..14 {
        let mid = f32::midpoint(lo, hi);
        if floor.met(fg.mix(pole, mid), surfaces) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    fg.mix(pole, hi)
}

/// `line` laid on only as much thicker as it takes to read `least` over every one of
/// `surfaces`: the ink whole when even that is not enough.
fn thicken(line: Tint, surfaces: &[Rgb], least: f32) -> Tint {
    thicken_until(line, |line| {
        surfaces.iter().map(|&bg| line.over(bg).contrast(bg)).fold(f32::INFINITY, f32::min) >= least
    })
}

/// `line` laid on only as much thicker as it takes for `reads` to hold: the ink whole when even
/// that is not enough. `reads` grows with the share.
fn thicken_until(line: Tint, reads: impl Fn(Tint) -> bool) -> Tint {
    let reads = |share: f32| reads(Tint::of(line.ink, share));
    let from = line.opacity();
    if reads(from) {
        return line;
    }
    // Contrast grows with the share: bisect for the least share that reads.
    let (mut lo, mut hi) = (from, 1.0_f32);
    for _ in 0..14 {
        let mid = f32::midpoint(lo, hi);
        if reads(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Tint::of(line.ink, hi)
}

impl Surfaces {
    /// The edge of a text field that has the keyboard (a composer, a message being written):
    /// the focus tone set back toward the field's [`Self::elevated`] ground, so focus is said
    /// quietly. A near-black ring round a whole card was the loudest thing on the screen. It
    /// keeps the least of [`FIELD_FOCUS`] of the focus tone that still clears 3:1 against the
    /// ground (WCAG 1.4.11, non-text contrast).
    #[must_use]
    pub fn field_focus(&self) -> Rgb {
        let mut share = FIELD_FOCUS;
        loop {
            let edge = self.elevated.mix(self.focus, share);
            if edge.contrast(self.elevated) >= NON_TEXT || share >= 1.0 {
                return edge;
            }
            share = (share + 0.05).min(1.0);
        }
    }

    /// The chrome for `content`, the terminal's background: dark tones on a dark one, light on
    /// a light one.
    ///
    /// The fills and hairlines are `content` mixed toward the chrome's text (or toward black
    /// or white) at the fixed steps of the variant, so they keep the content's tint. Text
    /// tones then move toward black or white until each clears WCAG AA on all six surfaces
    /// text lands on, and each text level keeps a quarter again the contrast of the one under
    /// it. For the default backgrounds that moves a tone three steps of a channel at most.
    ///
    /// A background from black up to a relative luminance of 0.05, or from 0.6 up to white,
    /// gets chrome that clears AA with three distinct text levels. A mid grey between them
    /// cannot: no text colour reads 4.5:1 on both it and a step above it. There the tones
    /// go as far as black or white do.
    ///
    /// The chrome is light and dark only, with no third look for the system's Increase
    /// Contrast (`docs/decisions/ui.md`, "Light and dark only").
    #[must_use]
    pub fn derive(content: Rgb) -> Self {
        let t = if content.is_light() { LIGHT_TONES } else { DARK_TONES };
        let at = |step: Step| {
            let toward = match step.toward {
                Toward::Ink => t.text,
                Toward::Black => Rgb::hex(0),
                Toward::White => Rgb::hex(0xff_ffff),
            };
            content.mix(toward, step.share)
        };
        let (canvas, panel, elevated, band) =
            (at(t.canvas), at(t.panel), at(t.elevated), at(t.band));
        let (hover, selected, pressed) =
            (Tint::of(t.text, t.hover), Tint::of(t.text, t.selected), Tint::of(t.text, t.pressed));
        let planes = [canvas, panel, content, elevated, band];
        // Text lands on a plane or on a resting state over one: the selected wash, the
        // deepest that stays (a press lasts as long as a click). The hover between them is
        // nearer the plane than the selection is.
        let grounds = |wash: Tint| -> Vec<Rgb> {
            planes.into_iter().chain(planes.map(|plane| wash.over(plane))).collect()
        };
        let under = grounds(selected);
        // A hairline is the ink laid over the surface, so over the content it is the step. It
        // crosses planes and the wells on them, never a selected fill.
        let crossed = grounds(hover);
        let floor = AA;
        let tier = |share: f32| content.mix(t.text, share);
        let text_muted =
            lift_to(tier(t.text_muted), &under, t.pole, Floor { ratio: floor, lc: MUTED_LC });
        let text_secondary = lift_to(
            tier(t.text_secondary),
            &under,
            t.pole,
            Floor { ratio: floor.max(worst(text_muted, &under) * LEVEL), lc: SECONDARY_LC },
        );
        let text = lift(t.text, &under, t.pole, floor.max(worst(text_secondary, &under) * LEVEL));
        // A status word (failed, waiting, done) is read as muted text is, and clears its floor.
        let word = Floor { ratio: floor, lc: MUTED_LC };
        let green = lift_to(t.accent.rgb(), &under, t.pole, word);
        let green_fill = t.accent_fill.rgb();
        let done = lift_to(t.success.rgb(), &under, t.pole, word);
        let (working, merged) = (
            lift_to(t.working.rgb(), &under, t.pole, word),
            lift_to(t.merged.rgb(), &under, t.pole, word),
        );
        let light = content.is_light();
        let (chosen_on_canvas, hover_on_canvas) = if light {
            (Tint::of(elevated, 1.0), Tint::of(elevated, alpha::HALF))
        } else {
            (selected, hover)
        };
        Self {
            canvas,
            panel,
            elevated,
            hover,
            selected,
            pressed,
            chosen_on_canvas,
            hover_on_canvas,
            band,
            border: Tint::of(t.text, t.border.share),
            border_subtle: Tint::of(t.text, t.border_subtle.share),
            control: thicken(Tint::of(t.text, t.border.share), &crossed, NON_TEXT),
            text,
            text_secondary,
            text_muted,
            accent: green,
            success: done,
            focus: text,
            warn: lift_to(t.warn, &under, t.pole, word),
            error: lift_to(t.error, &under, t.pole, word),
            accent_fill: green_fill,
            success_fill: t.success_fill.rgb(),
            warn_fill: t.warn_fill,
            error_fill: t.error_fill,
            working,
            working_fill: t.working_fill.rgb(),
            merged,
            merged_fill: t.merged_fill.rgb(),
            identity: t.identity.map(Oklch::rgb),
            fill_fg: t.fill_fg,
            accent_ink: t.accent_ink.rgb(),
            solid: text,
            solid_ink: at(t.solid_ink),
            brand: BRAND,
            stage: STAGE,
        }
    }
}

impl Surfaces {
    /// The chrome as it stands on glass: the docked navigator's, whose ground is the canvas laid
    /// at [`alpha::GLASS`] over the system's sidebar material (macOS).
    ///
    /// The material shows what lies behind the window, so the ground is known only to lie
    /// between the canvas over black and the canvas over white. Text there keeps the floors it
    /// keeps on every opaque surface (WCAG AA, APCA's secondary and muted levels, each text
    /// level a quarter again past the one under it) on both ends and on the hover and selected
    /// washes over them: each text tone moves toward black or white only as far as that takes,
    /// as Apple's sidebars set their labels deeper on vibrancy, and no further than black or
    /// white themselves. Every other colour stays.
    #[must_use]
    pub fn on_glass(self) -> Self {
        let pole = if self.canvas.is_light() { LIGHT_TONES.pole } else { DARK_TONES.pole };
        let under: Vec<Rgb> = glass_grounds(&self).collect();
        let text_muted = lift_to(self.text_muted, &under, pole, Floor { ratio: AA, lc: MUTED_LC });
        let text_secondary = lift_to(
            self.text_secondary,
            &under,
            pole,
            Floor { ratio: AA.max(worst(text_muted, &under) * LEVEL), lc: SECONDARY_LC },
        );
        let text = lift(self.text, &under, pole, AA.max(worst(text_secondary, &under) * LEVEL));
        let word = Floor { ratio: AA, lc: MUTED_LC };
        Self {
            text,
            text_secondary,
            text_muted,
            accent: lift_to(self.accent, &under, pole, word),
            success: lift_to(self.success, &under, pole, word),
            warn: lift_to(self.warn, &under, pole, word),
            error: lift_to(self.error, &under, pole, word),
            working: lift_to(self.working, &under, pole, word),
            merged: lift_to(self.merged, &under, pole, word),
            ..self
        }
    }
}

/// The grounds text lands on over glass ([`Surfaces::on_glass`]): the canvas at
/// [`alpha::GLASS`] over black and over white, the two ends of anything the material can show,
/// and the hover and selected washes over each. Contrast follows luminance alone, which a
/// channel's mix moves one way, so every wallpaper's ground lies between these.
fn glass_grounds(s: &Surfaces) -> impl Iterator<Item = Rgb> {
    let glass = Tint::of(s.canvas, alpha::GLASS);
    let (hover, selected) = (s.hover, s.selected);
    [Rgb::hex(0), Rgb::hex(0x00ff_ffff)].into_iter().flat_map(move |wall| {
        let ground = glass.over(wall);
        [ground, hover.over(ground), selected.over(ground)]
    })
}

/// One layer of a shadow, in points: the elevation's shade at `alpha`, `y` down, blurred over
/// `blur`, its shape grown by `spread` (drawn in by a negative one).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Shadow {
    /// How far down it falls.
    pub y: f32,
    /// How far it blurs.
    pub blur: f32,
    /// How far its shape grows past the surface's, before the blur; negative draws it in, so
    /// a soft layer falls below a float rather than haloing round it.
    pub spread: f32,
    /// Its opacity.
    pub alpha: f32,
}

impl Shadow {
    /// No layer: a stack with fewer layers than its slots fills the rest with it, and nothing
    /// is drawn for it.
    pub const NONE: Self = Self { y: 0.0, blur: 0.0, spread: 0.0, alpha: 0.0 };

    /// Whether it draws anything.
    #[must_use]
    pub fn shows(self) -> bool {
        self.alpha > 0.0
    }
}

/// The finish of what is sunk: a field, a segmented control's track, a meter.
///
/// Shade is held inside its top edge, and in dark a lip of light inside its bottom, where
/// light reaches the far side of a hollow. The rim's counterpart: what is raised catches light
/// at its top edge, what is sunk holds shade there (Frame's fields and tracks, tty7's key
/// caps).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Sunk {
    /// The black shade's opacity inside the top edge.
    pub shade: f32,
    /// The white lip's opacity inside the bottom edge, or none.
    pub lip: Option<f32>,
}

/// The rim of what is raised: a point inside one edge that catches the light, the one rule
/// for everything that stands off its plane.
///
/// In dark it is light along the top edge: a black shadow on a near-black window cannot show
/// where a sheet ends, an edge that catches the light can. In light it is a shade along the
/// bottom edge, as coss draws its surfaces, since white on white shows no light.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Rim {
    /// The lit edge is the top one; else the bottom, a shade.
    pub top: bool,
    /// What it is drawn in: white for light, black for a shade.
    pub ink: Rgb,
    /// Its opacity on what rests (a card, a secondary button, the composer, a segmented
    /// control's thumb), or none where the ring and the contact already end it.
    pub rest: Option<f32>,
    /// Its opacity on what floats, or none where the drop shadow already says where it ends.
    pub float: Option<f32>,
}

/// The finish of the neutral solid, the one primary of a surface: a point inside one edge, a
/// contact under it in light, and a shade inside its top while it is held, so it presses in.
///
/// coss lights its primary along the top and shades it in when pressed; Linear gives its
/// primary a low shadow. On the near-black solid of light the point is light along the top; on
/// the white solid of dark it is a shade along the bottom, where light would not show.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Finish {
    /// The point is along the top edge; else along the bottom.
    pub top: bool,
    /// What the point is drawn in.
    pub ink: Rgb,
    /// Its opacity at rest.
    pub alpha: f32,
    /// The contact under the solid, or none.
    pub contact: Option<Shadow>,
    /// The black shade inside the top edge while held; the point is off then.
    pub pressed: f32,
}

/// The two elevations, resting and floating, and what dims the window under a modal.
///
/// What rests (a card, a secondary button, the composer) stands off its plane by its rim and,
/// in light, a contact shadow; what floats (a sheet, a menu, a hint) by the soft shadow too.
/// Every reference has this pair: Geist's base and menu, Radix's shadow 2 and 5, `HeroUI`'s
/// surface and overlay, coss's rim and its large shadow. Both are drawn only by the kit.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Elevation {
    /// The colour of the shadow and of the scrim: black in dark, the warm ink in light, so a
    /// shadow on paper is a deeper paper rather than a grey (Radix's tinted greys).
    pub shade: Rgb,
    /// How much the scrim under a modal dims the window.
    pub scrim: f32,
    /// How much the scrim under a floating sidebar dims the window: lighter than a modal's.
    pub aside: f32,
    /// The shadow under an `elevated` surface, tightest first: a contact layer, then softer
    /// ones; [`Shadow::NONE`] fills the slots a mode does not use.
    pub shadow: [Shadow; 3],
    /// The contact under what rests, its layers tightest first, or none: in dark a shadow on
    /// near-black says nothing, and the rim does the work.
    pub rest: Option<[Shadow; 2]>,
    /// The rim of what is raised, resting or floating.
    pub rim: Rim,
    /// The primary solid's finish.
    pub finish: Finish,
    /// What is sunk.
    pub sunk: Sunk,
}

impl Elevation {
    /// Dark: a quiet shadow, a lit top edge, and a deep scrim. On near-black a shadow cannot
    /// say where a sheet ends; the lit edge and the hairline say it, so the shadow only gives
    /// the sheet weight. At 0.4 and 0.5 it was four times Zed's and pooled round every menu.
    pub const DARK: Self = Self {
        shade: Rgb::hex(0),
        scrim: alpha::SCRIM,
        aside: alpha::SCRIM_ASIDE,
        shadow: [
            Shadow { y: 1.0, blur: 2.0, spread: 0.0, alpha: 0.1 },
            // Drawn in by 10, so it falls below the sheet (6 to the sides, none above) where
            // `0 12 32` reached 16 sideways and 4 above; a third heavier to keep its weight.
            Shadow { y: 12.0, blur: 32.0, spread: -10.0, alpha: 0.16 },
            Shadow::NONE,
        ],
        rest: None,
        // coss draws its dark rim at 6 %; what floats keeps that, what rests a step quieter.
        rim: Rim {
            top: true,
            ink: Rgb::hex(0x00ff_ffff),
            rest: Some(alpha::RIM),
            float: Some(alpha::EDGE),
        },
        finish: Finish { top: false, ink: Rgb::hex(0), alpha: 0.10, contact: None, pressed: 0.08 },
        sunk: Sunk { shade: 0.18, lip: Some(alpha::RIM) },
    };
    /// Light: shadows in the warm ink (`oklch(0.24 0.012 85)`), three layers under what floats
    /// and two faint ones under what rests, and a scrim of the same ink.
    #[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
    pub const LIGHT: Self = Self {
        shade: Rgb::hex(0x221f19),
        scrim: alpha::SCRIM_ON_PAPER,
        aside: alpha::SCRIM_ASIDE_ON_PAPER,
        // Geist's menu and modal shape, a notch firmer for the half-point ring: a contact, a
        // near layer, then a soft one drawn in so it falls below the sheet, not round it.
        shadow: [
            Shadow { y: 1.0, blur: 1.0, spread: 0.0, alpha: 0.05 },
            Shadow { y: 4.0, blur: 8.0, spread: -4.0, alpha: 0.06 },
            Shadow { y: 16.0, blur: 32.0, spread: -8.0, alpha: 0.11 },
        ],
        // Primer's resting small: a card on the content is held by its ring and a contact
        // this faint, not a pool.
        rest: Some([
            Shadow { y: 1.0, blur: 1.0, spread: 0.0, alpha: 0.04 },
            Shadow { y: 1.0, blur: 2.0, spread: 0.0, alpha: 0.03 },
        ]),
        // No rim on what rests: its ring and contact already end it, and a third edge along
        // its bottom read as a box drawn twice.
        rim: Rim { top: false, ink: Rgb::hex(0), rest: None, float: None },
        finish: Finish {
            top: true,
            ink: Rgb::hex(0x00ff_ffff),
            alpha: 0.14,
            contact: Some(Shadow { y: 1.0, blur: 2.0, spread: 0.0, alpha: 0.08 }),
            pressed: 0.08,
        },
        sunk: Sunk { shade: 0.05, lip: None },
    };

    /// The elevation for chrome over `content`.
    #[must_use]
    pub const fn of(content: Rgb) -> Self {
        if content.is_light() { Self::LIGHT } else { Self::DARK }
    }
}

/// A timing curve: CSS's `cubic-bezier(x1, y1, x2, y2)`, from (0, 0) to (1, 1).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Curve {
    /// The first control point.
    pub p1: (f32, f32),
    /// The second control point.
    pub p2: (f32, f32),
}

impl Curve {
    /// How far along the motion is at `t` (0 to 1) of its time.
    ///
    /// Solves the curve's x for `t` by Newton's method, falling back to bisection where the
    /// slope is too flat to step on, then reads y there.
    #[must_use]
    pub fn at(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        // One axis of the curve and its slope at parameter `s`, from its two control values.
        let axis = |a: f32, b: f32, s: f32| {
            let u = 1.0 - s;
            (3.0 * u * u * s).mul_add(a, (3.0 * u * s * s).mul_add(b, s * s * s))
        };
        let slope = |a: f32, b: f32, s: f32| {
            let u = 1.0 - s;
            (3.0 * u * u).mul_add(a, (6.0 * u * s).mul_add(b - a, 3.0 * s * s * (1.0 - b)))
        };
        let (x1, x2) = (self.p1.0, self.p2.0);
        let mut s = t;
        for _ in 0..8 {
            let dx = slope(x1, x2, s);
            if dx.abs() < 1e-6 {
                break;
            }
            s = (s - (axis(x1, x2, s) - t) / dx).clamp(0.0, 1.0);
        }
        if (axis(x1, x2, s) - t).abs() > 1e-4 {
            let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
            for _ in 0..24 {
                s = f32::midpoint(lo, hi);
                if axis(x1, x2, s) < t {
                    lo = s;
                } else {
                    hi = s;
                }
            }
        }
        axis(self.p1.1, self.p2.1, s)
    }
}

/// How chrome moves: durations and curves, one set for every animation.
///
/// Every duration stays at or under 160 ms but a sheet's and streamed text's: an overlay that
/// takes longer to arrive than a key takes to type reads as waiting, where words an agent is
/// writing are read as they come and lift with its pace. What moves is opacity and a small
/// translate, never the scale of text. Under Reduce Motion all of it lands at once
/// (`slopty_ui::kit::motion`), but for what says work is live: the working mark breathes in opacity
/// over [`Self::breath`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Motion {
    /// A hover or press arriving, and a cursor changing: instant, so the pointer never waits.
    pub hover: std::time::Duration,
    /// A hover or press leaving: the fill lets go over this, so a pointer swept across a list
    /// leaves a short wake rather than a flicker (Linear and Raycast: in at once, out in 150).
    pub unhover: std::time::Duration,
    /// Overlays, menus and hints appearing.
    pub fade: std::time::Duration,
    /// Overlays, menus and hints leaving: shorter than their entrance, since what is dismissed
    /// is no longer looked at (Radix, `HeroUI`: 100 ms out against 150 to 200 in).
    pub exit: std::time::Duration,
    /// A selected row's fill moving, a tab resizing, a fold opening.
    pub settle: std::time::Duration,
    /// A phone's palette sheet, the iPad's drawer, the composer turning into an approval, a
    /// tab closing: `MonoCode`'s 200 ms.
    pub sheet: std::time::Duration,
    /// One breath of a live mark under Reduce Motion: its opacity rises and falls over this,
    /// with no travel or turn, since a mark that freezes reads as hung (Zeron's activity pulse;
    /// the platform keeps its activity indicators alive under Reduce Motion).
    pub breath: std::time::Duration,
    /// Half a caret blink: shown this long, then hidden as long (Ghostty's cadence). The pace of
    /// a blink, not a transition: nothing eases.
    pub blink: std::time::Duration,
    /// The longest the words an agent streams take to lift to full colour. The lift follows the
    /// stream, three of its gaps long, from `fade` up to this, so a quick stream lifts briskly
    /// and a slow one's newest words stay veiled about until the next arrive. An answer's first
    /// words come after a pause, so they lift over the whole of it.
    pub stream: std::time::Duration,
    /// How much later each word of one streamed chunk starts lifting than the word before it:
    /// a six-word chunk lights up in 60 ms, one gesture with a gradient inside it, well under
    /// the gap to the next chunk.
    pub stream_stagger: std::time::Duration,
    /// The curve of everything but a sheet: fast out of the gate, a long soft landing.
    pub ease_out: Curve,
    /// A sheet's curve: a drawer's, which follows a finger's flick.
    pub drawer: Curve,
}

impl Motion {
    /// The one set.
    pub const DEFAULT: Self = Self {
        hover: std::time::Duration::ZERO,
        unhover: std::time::Duration::from_millis(150),
        fade: std::time::Duration::from_millis(120),
        exit: std::time::Duration::from_millis(100),
        settle: std::time::Duration::from_millis(160),
        sheet: std::time::Duration::from_millis(200),
        breath: std::time::Duration::from_millis(2_400),
        blink: std::time::Duration::from_millis(600),
        stream: std::time::Duration::from_millis(400),
        stream_stagger: std::time::Duration::from_millis(10),
        ease_out: Curve { p1: (0.22, 1.0), p2: (0.36, 1.0) },
        drawer: Curve { p1: (0.32, 0.72), p2: (0.0, 1.0) },
    };
}

impl Default for Motion {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// How roomy the chrome is: row and header heights and the least side of anything tapped.
///
/// Two sets, one per input: a pointer is precise enough for a 24 pt target, a finger needs
/// the 44 pt Apple's HIG asks for. Visual sizes (icons, type) are the same in both; only the
/// hit area and the rows round it grow.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Density {
    /// A one-line row: a worker's header, a palette or menu row, a row of *Needs you*.
    pub row: f32,
    /// A row of two lines: a title over its meta line.
    pub row_two_line: f32,
    /// A tile's header: 40 on the Mac, `MonoCode`'s, so a header holds its title, its state
    /// and its buttons with air above and below rather than as a strip.
    pub header: f32,
    /// A button's or a field's height: a row's, so a button in a row fills it.
    pub control: f32,
    /// The least side of anything tapped or clicked: an icon button, a key cap, a close box.
    pub hit: f32,
}

impl Density {
    /// A pointer: the Mac.
    pub const COMPACT: Self =
        Self { row: 28.0, row_two_line: 40.0, header: 40.0, control: 28.0, hit: 24.0 };
    /// A finger: the iPhone and the iPad.
    pub const TOUCH: Self =
        Self { row: 44.0, row_two_line: 56.0, header: 44.0, control: 44.0, hit: 44.0 };
}

impl Default for Density {
    fn default() -> Self {
        Self::COMPACT
    }
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
    /// When typing is kept from other programs on this Mac (secure event input).
    pub secure_entry: SecureEntry,
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
            secure_entry: SecureEntry::Passwords,
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

/// When typing into the app is kept from other programs on this Mac (macOS secure event
/// input, Terminal's "Secure Keyboard Entry").
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum SecureEntry {
    /// While a terminal waits for a password or a remote password field has the keyboard.
    #[default]
    Passwords,
    /// Whenever an app window is in front.
    Always,
    /// Never.
    Never,
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

/// The quality a remote stream is opened at (the scale follows the width the tile is drawn at,
/// not this).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StreamPrefs {
    /// The bitrate ceiling, bits per second.
    pub max_bitrate_bps: u32,
}

impl Default for StreamPrefs {
    fn default() -> Self {
        Self { max_bitrate_bps: 30_000_000 }
    }
}

/// The whole theme.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Theme {
    /// Terminal colours.
    pub terminal: TerminalPalette,
    /// Terminal behaviour.
    pub behaviour: Behaviour,
    /// Chrome colours, derived from the terminal's background.
    pub surfaces: Surfaces,
    /// The shadow of what floats and the scrim under a modal.
    pub elevation: Elevation,
    /// How roomy the chrome is for the input at hand.
    pub density: Density,
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
    /// The type roles at this theme's density: a finger's when its targets are a finger's.
    #[must_use]
    pub fn roles(&self) -> TypeRoles {
        self.typography.roles(self.density.hit > Density::COMPACT.hit)
    }

    /// The theme for `variant` with default typography.
    #[must_use]
    pub fn new(variant: Variant) -> Self {
        let terminal = match variant {
            Variant::Dark => TerminalPalette::DARK,
            Variant::Light => TerminalPalette::LIGHT,
        };
        Self {
            terminal,
            behaviour: Behaviour::default(),
            surfaces: Surfaces::derive(terminal.bg),
            elevation: Elevation::of(terminal.bg),
            density: Density::default(),
            typography: Typography::default(),
            radii: Radii::default(),
            spacing: Spacing::default(),
        }
    }

    /// Derive the chrome again from the terminal's background, after something changed it (a
    /// `[colors]` background in the settings).
    pub fn derive_chrome(&mut self) {
        self.surfaces = Surfaces::derive(self.terminal.bg);
        self.elevation = Elevation::of(self.terminal.bg);
    }

    /// The content step of the surface order: what tile headers and bodies sit on. It is the
    /// terminal's background, whatever the settings made it, so a shell's grid, the body
    /// round it and its focused header are one surface.
    #[must_use]
    pub const fn content(&self) -> Rgb {
        self.terminal.bg
    }

    /// The edge of a text field that has the keyboard ([`Surfaces::field_focus`]).
    #[must_use]
    pub fn field_focus(&self) -> Rgb {
        self.surfaces.field_focus()
    }

    /// Which variant the colours are: light when the terminal's background reads as light,
    /// whatever the settings made it.
    #[must_use]
    pub const fn variant(&self) -> Variant {
        if self.terminal.bg.is_light() { Variant::Light } else { Variant::Dark }
    }

    /// How lit an unlit dot of the mark is, over this theme's content.
    #[must_use]
    pub const fn brand_unlit(&self) -> f32 {
        match self.variant() {
            Variant::Light => alpha::UNLIT_ON_PAPER,
            Variant::Dark => alpha::UNLIT,
        }
    }
}

#[cfg(test)]
mod vision;

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG AAA for body text: what the primary solid's ink reads at.
    const AAA: f32 = 7.0;

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

    /// The mark is Slopty's green in both variants; its unlit dots stay visible without
    /// competing: the brand's 0.2 on dark, 0.3 on paper. Its cursor blinks at the terminal's
    /// cadence.
    #[test]
    fn the_mark_is_slopty_green_with_its_unlit_dots_set_back() {
        let (dark, light) = (Theme::new(Variant::Dark), Theme::new(Variant::Light));
        assert_eq!(dark.surfaces.brand, Rgb { r: 0x4a, g: 0xc0, b: 0x6c });
        assert_eq!(light.surfaces.brand, dark.surfaces.brand);
        let unlit = |theme: &Theme| {
            let content = theme.content();
            content.mix(theme.surfaces.brand, theme.brand_unlit()).contrast(content)
        };
        for theme in [&dark, &light] {
            let set_back = unlit(theme);
            assert!((1.2..2.0).contains(&set_back), "{:?}: {set_back}", theme.variant());
        }
        assert_eq!(Motion::DEFAULT.blink, std::time::Duration::from_millis(600));
    }

    #[test]
    fn variants() {
        assert_eq!(Theme::default().variant(), Variant::Dark);
        let light = Theme::new(Variant::Light);
        assert_eq!(light.variant(), Variant::Light);
        assert_eq!(light.surfaces, Surfaces::derive(TerminalPalette::LIGHT.bg));
        assert_eq!(light.elevation, Elevation::LIGHT);
        assert_eq!(light.typography, Typography::default());
        assert_ne!(light.terminal.palette(0), light.terminal.bg, "ANSI black is visible on white");
    }

    #[test]
    fn the_text_levels_climb_and_the_scale_follows_the_base() {
        let luma = |c: Rgb| u32::from(c.r) + u32::from(c.g) + u32::from(c.b);
        let dark = Theme::new(Variant::Dark).surfaces;
        assert!(
            luma(dark.text_muted) < luma(dark.text_secondary)
                && luma(dark.text_secondary) < luma(dark.text)
        );
        let light = Theme::new(Variant::Light).surfaces;
        assert!(
            luma(light.text_muted) > luma(light.text_secondary)
                && luma(light.text_secondary) > luma(light.text)
        );

        let r = Radii::default();
        assert!(r.xs < r.sm && r.sm < r.md && r.md < r.lg && r.lg < r.full);
        assert!(
            (Radii::nested(r.lg, 0.0, r.sm) - r.sm).abs() < f32::EPSILON,
            "a sheet is its rows' radius and pad"
        );
        assert!((Radii::nested(r.lg, 1.0, r.xs) - 7.0).abs() < f32::EPSILON, "inside a border");
        assert!(Radii::nested(r.xs, 1.0, r.md).abs() < f32::EPSILON, "never under zero");

        // `MonoCode`'s chrome sizes: 13, 12 (the most used), 11; prose 14; then the titles and
        // page headings by their roles, 15, 20 and 26, weights no higher than semibold.
        let mut t = Typography::default();
        let roles = t.roles(false);
        assert_eq!(
            (
                t.caption(),
                t.small(),
                t.ui_size,
                roles.panel_title.size,
                roles.page_heading.size,
                roles.first_run.size
            ),
            (11.0, 12.0, 13.0, 15.0, 20.0, 26.0),
            "one name per size"
        );
        const {
            assert!(Typography::MEDIUM_WEIGHT > 400.0);
            assert!(Typography::MEDIUM_WEIGHT < Typography::STRONG_WEIGHT);
            assert!(Typography::STRONG_WEIGHT <= 600.0, "semibold at the most");
        };
        t.ui_size = 8.0;
        let title = t.roles(false).panel_title.size;
        assert_eq!((t.caption(), t.small(), title), (6.0, 7.0, 10.0), "clamped at the floor");
        let s = Spacing::default();
        assert!(s.xxs < s.xs && s.xs < s.sm && s.sm < s.md && s.md < s.lg && s.lg < s.xl);
        assert!(s.xl < s.xxl && s.xxl < s.xxxl, "the page steps follow");
        assert!(2.0_f32.mul_add(-s.inset_trailing(), s.inset()).abs() < f32::EPSILON, "12 and 6");
    }

    /// The window climbs in both variants (bars, then unfocused headers, then the content),
    /// and the in-panel hairline is quieter than the one between panes on every step while
    /// still showing on the content.
    ///
    /// Before the first ruling, the light variant's navigator, status bar, headers and bodies
    /// were all white, and the dark one's content sat between its bars and its navigator.
    #[test]
    fn the_surfaces_climb_bars_panel_content() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let content = theme.content();
            assert_eq!(content, theme.terminal.bg, "{variant:?}: the content is the grid's");
            let (bars, panel) = (s.canvas.luminance(), s.panel.luminance());
            assert!(bars < panel && panel < content.luminance(), "{variant:?} climbs");
            for (name, surface) in [("canvas", s.canvas), ("panel", s.panel), ("content", content)]
            {
                let (loud, quiet) = (
                    s.border.over(surface).contrast(surface),
                    s.border_subtle.over(surface).contrast(surface),
                );
                assert!(quiet < loud, "{variant:?}: the subtle hairline is quieter on {name}");
            }
            let subtle = s.border_subtle.over(content).contrast(content);
            assert!(subtle >= 1.05, "{variant:?}: the subtle hairline shows on content");
        }
    }

    /// The chrome sits one notch from the content, as Linear's navigation sits a few notches
    /// dimmer than its content and Ghostty's tab bar takes the terminal's own colour: the bars
    /// 2.5 to 4 CIE L* under it, the panel between, each step at least one unit, and the light
    /// variant's steps as far apart as the dark one's, give or take half a unit.
    ///
    /// At three notches (dark bars 66 % toward black, `07080A` round `16181D`) the window wore a
    /// black frame; in light a grey slab (`EDEDED`) under a `D0D0D0` rule. A contrast ratio
    /// cannot see this: on near-black every step is under 1.05.
    /// A colour's CIE L*: how light the eye reads it, in steps a viewer can compare.
    fn lightness(c: Rgb) -> f32 {
        let y = c.luminance();
        if y > 216.0 / 24_389.0 { 116.0_f32.mul_add(y.cbrt(), -16.0) } else { y * 24_389.0 / 27.0 }
    }

    /// A selection on the canvas rises, never darkens: in light the chosen row is the raised
    /// surface itself, lighter than the canvas it lies on, and the pointer's step is half way up
    /// to it; in dark both are the ink's washes, as everywhere else.
    #[test]
    fn a_selection_on_the_canvas_rises_in_light() {
        for (name, bg) in BACKGROUNDS {
            let s = Surfaces::derive(Rgb::hex(bg));
            let (chosen, hover) = (
                s.chosen_on_canvas.over(s.canvas).oklch().l,
                s.hover_on_canvas.over(s.canvas).oklch().l,
            );
            let canvas = s.canvas.oklch().l;
            if Rgb::hex(bg).is_light() {
                assert_eq!(
                    s.chosen_on_canvas.over(s.canvas),
                    s.elevated,
                    "{name}: the raised surface"
                );
                assert!(
                    chosen > hover && hover > canvas,
                    "{name}: {canvas:.3} < {hover:.3} < {chosen:.3}"
                );
            } else {
                assert_eq!(
                    (s.chosen_on_canvas, s.hover_on_canvas),
                    (s.selected, s.hover),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn the_chrome_sits_one_notch_from_the_content() {
        let steps = |variant| {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let (content, panel, canvas) =
                (lightness(theme.content()), lightness(s.panel), lightness(s.canvas));
            [(content - panel).abs(), (panel - canvas).abs(), (content - canvas).abs()]
        };
        let (dark, light) = (steps(Variant::Dark), steps(Variant::Light));
        for (name, d, l) in [
            ("content to panel", dark[0], light[0]),
            ("panel to bars", dark[1], light[1]),
            ("content to bars", dark[2], light[2]),
        ] {
            assert!((d - l).abs() <= 0.5, "{name}: dark {d:.2}, light {l:.2}");
            assert!(d.min(l) >= 1.0, "{name} shows: dark {d:.2}, light {l:.2}");
        }
        for (variant, notch) in [("dark", dark[2]), ("light", light[2])] {
            assert!((2.5..=4.0).contains(&notch), "{variant}: bars {notch:.2} L* under content");
        }
    }

    /// A terminal block's head band is seen on its own: as far off the content as the bars
    /// are (2.5 to 4 L*) in both variants, and at least a unit off `panel`, so under an
    /// unfocused header it is not a second header. On white it sat on `panel`, 1.5 L* off, and
    /// the eye saw only its rule.
    #[test]
    fn the_head_band_shows_and_is_not_a_header() {
        let notch = |variant| {
            let theme = Theme::new(variant);
            let band = lightness(theme.surfaces.band);
            let apart = (band - lightness(theme.surfaces.panel)).abs();
            assert!(apart >= 1.0, "{variant:?}: the band is {apart:.2} L* off the panel");
            (lightness(theme.content()) - band).abs()
        };
        let (dark, light) = (notch(Variant::Dark), notch(Variant::Light));
        for (variant, notch) in [("dark", dark), ("light", light)] {
            assert!((2.5..=4.0).contains(&notch), "{variant}: band {notch:.2} L* off content");
        }
        assert!((dark - light).abs() <= 0.5, "dark {dark:.2}, light {light:.2}");
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
        for theme in [TerminalPalette::DARK, TerminalPalette::LIGHT] {
            assert_eq!(theme.cursor_text, theme.bg, "a block cursor cuts out the background");
            assert!(theme.cursor_text.contrast(theme.cursor) >= 4.5, "and reads on it");
        }
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

    /// The keyboard's ring is neutral, the chrome's text and not the green, so green keeps
    /// meaning live and done; drawn at its strength it clears 3:1 on every ground it rings,
    /// on every background.
    #[test]
    fn the_focus_ring_is_neutral_and_seen_everywhere() {
        for (name, bg) in BACKGROUNDS {
            let content = Rgb::hex(bg);
            let s = Surfaces::derive(content);
            assert_eq!(s.focus, s.text, "{name}: the ring is the text's tone");
            assert_ne!(s.focus, s.accent, "{name}: not the green");
            for (ground, under) in under_text(&s, content) {
                let ring = under.mix(s.focus, alpha::STRONG);
                let ratio = ring.contrast(under);
                assert!(ratio >= NON_TEXT, "{name} on {ground}: {ratio:.2}");
            }
        }
    }

    /// Success has its own seed, apart from the accent's, though both are the brand's green
    /// for now: as text each reads on every ground, as the accent does.
    #[test]
    fn success_is_its_own_token() {
        for tones in [DARK_TONES, LIGHT_TONES] {
            assert_eq!(tones.success, tones.accent, "one green for now");
            assert_eq!(tones.success_fill, tones.accent_fill);
        }
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            for (ground, under) in under_text(&s, theme.content()) {
                assert!(s.success.contrast(under) >= 3.0, "{variant:?} on {ground}");
            }
        }
    }

    /// The type roles: the desktop's from the 13 pt chrome, the touch ones a step larger, each
    /// following the chrome size, prose its own; a touch theme picks the touch ones.
    #[test]
    fn the_type_roles_are_the_scale_the_critique_set() {
        let t = Typography::default();
        let at = |r: TypeRole| (r.size, r.line, r.weight);
        let desk = t.roles(false);
        assert_eq!(at(desk.caption), (11.0, 16.0, 400.0));
        assert_eq!(at(desk.metadata), (12.0, 18.0, 400.0));
        assert_eq!(at(desk.chrome), (13.0, 19.0, 400.0));
        assert_eq!(at(desk.action), (13.0, 19.0, 500.0));
        assert_eq!(at(desk.task_title), (14.0, 20.0, 500.0));
        assert_eq!(at(desk.section), (12.0, 16.0, 500.0));
        assert_eq!(at(desk.panel_title), (15.0, 20.0, 600.0));
        assert_eq!(at(desk.prose), (14.0, 22.0, 400.0));
        assert_eq!(at(desk.page_heading), (20.0, 26.0, 600.0));
        assert_eq!(at(desk.first_run), (26.0, 32.0, 500.0));
        let touch = t.roles(true);
        assert_eq!(at(touch.metadata), (13.0, 18.0, 400.0));
        assert_eq!(at(touch.chrome), (17.0, 22.0, 400.0));
        assert_eq!(at(touch.task_title), (17.0, 22.0, 500.0));
        assert_eq!(at(touch.section), (13.0, 18.0, 500.0));
        assert_eq!(at(touch.panel_title), (19.0, 24.0, 600.0));
        assert_eq!(at(touch.prose), (16.0, 26.0, 400.0));
        assert_eq!(at(touch.first_run), (28.0, 34.0, 500.0));
        let larger = Typography { ui_size: 15.0, ..Typography::default() }.roles(false);
        assert_eq!(at(larger.task_title), (16.0, 22.0, 500.0), "it follows the chrome size");
        assert_eq!(larger.prose, desk.prose, "prose keeps its own size");
        // The strong weight is a title's alone: a dialog's or a panel's, and a page's heading.
        // A section's head and the first run's hero are medium.
        for roles in [desk, touch] {
            let strong: Vec<f32> = [
                roles.caption,
                roles.metadata,
                roles.chrome,
                roles.action,
                roles.task_title,
                roles.section,
                roles.prose,
                roles.first_run,
            ]
            .iter()
            .map(|r| r.weight)
            .filter(|w| *w > Typography::MEDIUM_WEIGHT)
            .collect();
            assert!(strong.is_empty(), "only titles are strong: {strong:?}");
            let strong = |r: TypeRole| (r.weight - Typography::STRONG_WEIGHT).abs() < f32::EPSILON;
            assert!(strong(roles.panel_title) && strong(roles.page_heading), "titles are strong");
        }
        let finger = Theme { density: Density::TOUCH, ..Theme::default() };
        assert_eq!(finger.roles(), touch);
        assert_eq!(Theme::default().roles(), desk);
    }

    /// Terminal backgrounds the chrome is derived for, with a name for the messages: the two
    /// defaults, popular schemes, and the ends of the supported range (a relative luminance of
    /// 0.05 at most in dark, 0.6 at least in light).
    const BACKGROUNDS: [(&str, u32); 15] = [
        ("black", 0x00_0000),
        ("default dark", 0x18_1716),
        ("old default dark", 0x16_181d),
        ("catppuccin mocha", 0x1e_1e2e),
        ("dracula", 0x28_2a36),
        ("solarized dark", 0x00_2b36),
        ("nord", 0x2e_3440),
        ("dark end", 0x3f_3f3f),
        ("default light", 0xfd_fcfb),
        ("white", 0xff_ffff),
        ("one light", 0xfa_fafa),
        ("solarized light", 0xfd_f6e3),
        ("gruvbox light", 0xfb_f1c7),
        ("catppuccin latte", 0xef_f1f5),
        ("light end", 0xcc_cccc),
    ];

    /// The planes chrome sits on.
    fn planes(s: &Surfaces, content: Rgb) -> [(&'static str, Rgb); 5] {
        [
            ("canvas", s.canvas),
            ("panel", s.panel),
            ("content", content),
            ("elevated", s.elevated),
            ("band", s.band),
        ]
    }

    /// The grounds chrome text can land on: the planes first, then the selected wash over
    /// each.
    fn under_text(s: &Surfaces, content: Rgb) -> Vec<(String, Rgb)> {
        let planes = planes(s, content);
        let named = planes.map(|(name, c)| (name.to_owned(), c));
        let selected = planes.map(|(name, c)| (format!("selected on {name}"), s.selected.over(c)));
        named.into_iter().chain(selected).collect()
    }

    /// The chrome text colours, with their names.
    fn inks(s: &Surfaces) -> [(&'static str, Rgb); 9] {
        [
            ("text", s.text),
            ("text_secondary", s.text_secondary),
            ("text_muted", s.text_muted),
            ("success", s.success),
            ("warn", s.warn),
            ("error", s.error),
            ("accent", s.accent),
            ("working", s.working),
            ("merged", s.merged),
        ]
    }

    /// The ends of the range are where they are said to be.
    #[test]
    fn the_range_ends_sit_where_the_docs_put_them() {
        let dark_end = Rgb::hex(0x3f_3f3f).luminance();
        let light_end = Rgb::hex(0xcc_cccc).luminance();
        assert!((0.045..=0.05).contains(&dark_end), "{dark_end}");
        assert!((0.6..=0.61).contains(&light_end), "{light_end}");
    }

    /// Every chrome colour that is drawn as text reads on every chrome surface it can land on,
    /// for every background in the supported range, and the three text levels stay apart.
    ///
    /// The terminal grid has `minimum_contrast` to lift a cell whose colours the program chose;
    /// chrome has nothing of the kind, because these colours are ours and the fix is to pick
    /// better ones. WCAG AA for body text is 4.5:1, and the pairs are checked against all six
    /// surfaces rather than against the one they usually sit on: a status label follows its
    /// tile, and the tile can be on any of them.
    ///
    /// Derived at the steps without the lift, dark `text_muted` read 4.48 on `selected` over the
    /// content and light `accent`, `error`, `success` and `text_muted` about 4.40.
    #[test]
    fn chrome_text_clears_wcag_aa() {
        for (name, bg) in BACKGROUNDS {
            let content = Rgb::hex(bg);
            let s = Surfaces::derive(content);
            let under = under_text(&s, content);
            for (ink, fg) in inks(&s) {
                for (surface, bg) in &under {
                    let ratio = fg.contrast(*bg);
                    assert!(ratio >= AA, "{name}: {ink} on {surface} is {ratio:.2}, under {AA}");
                }
            }
            let surfaces: Vec<Rgb> = under.iter().map(|(_, c)| *c).collect();
            let (muted, secondary, text) = (
                worst(s.text_muted, &surfaces),
                worst(s.text_secondary, &surfaces),
                worst(s.text, &surfaces),
            );
            assert!(
                secondary >= muted * LEVEL,
                "{name}: secondary {secondary:.2}, muted {muted:.2}"
            );
            assert!(text >= secondary * LEVEL, "{name}: text {text:.2}, secondary {secondary:.2}");
        }
    }

    /// Wallpapers a window's glass can stand over: white and black, the ends, and busy ones
    /// between, dark and bright and saturated, which their blur leaves as a cast of their colour.
    const WALLPAPERS: [u32; 8] =
        [0xff_ffff, 0x00_0000, 0x1b_2a4a, 0x4a_1e2a, 0x2d_3b1f, 0xff_3b30, 0x34_c759, 0x80_8080];

    /// The navigator's text on glass keeps every floor it keeps on an opaque surface, over any
    /// wallpaper, for every background in the supported range: WCAG AA for every tone, APCA's
    /// |Lc| 55 for secondary text and 45 for muted text and the status words, and each text
    /// level a quarter again past the one under it (or as far as black or white go). Decided at
    /// [`alpha::GLASS`] on the ground the canvas makes over each wallpaper, and on the hover and
    /// selected rows over that.
    #[test]
    fn text_on_glass_keeps_its_floors_over_any_wallpaper() {
        for (name, bg) in BACKGROUNDS {
            let content = Rgb::hex(bg);
            let pole = if content.is_light() { LIGHT_TONES.pole } else { DARK_TONES.pole };
            let s = Surfaces::derive(content).on_glass();
            let glass = Tint::of(s.canvas, alpha::GLASS);
            let grounds: Vec<Rgb> = WALLPAPERS
                .into_iter()
                .flat_map(|wall| {
                    let ground = glass.over(Rgb::hex(wall));
                    [ground, s.hover.over(ground), s.selected.over(ground)]
                })
                .collect();
            for (ink, fg) in inks(&s) {
                let ratio = worst(fg, &grounds);
                assert!(ratio >= AA, "{name}: {ink} on glass is {ratio:.2}, under {AA}");
                let least = if ink == "text_secondary" { SECONDARY_LC } else { MUTED_LC };
                let lc = worst_lc(fg, &grounds);
                assert!(lc >= least || fg == pole, "{name}: {ink} on glass is Lc {lc:.1}");
            }
            let (muted, secondary, text) = (
                worst(s.text_muted, &grounds),
                worst(s.text_secondary, &grounds),
                worst(s.text, &grounds),
            );
            assert!(
                secondary >= muted * LEVEL,
                "{name}: secondary {secondary:.2}, muted {muted:.2}"
            );
            // At the light end of the range black itself is not a quarter past secondary text
            // that clears Lc 55 on the darkest ground: text goes as far as black goes.
            assert!(
                text >= secondary * LEVEL || s.text == pole,
                "{name}: text {text:.2}, secondary {secondary:.2}"
            );
        }
    }

    /// Glass reads as the chrome it is, in light as in dark: over the worst wallpaper its ground
    /// moves off the canvas by no more than the share of the wallpaper it lets through, its text
    /// tones go at most a tenth of L deeper than on the opaque chrome (a grey a shade
    /// darker, never a second hierarchy: the floors test keeps each tier a step apart), and
    /// nothing but text moves. At 0.90 the glass was too opaque to be seen; at 0.82 it shows.
    #[test]
    fn glass_reads_as_the_chrome_in_light_and_dark() {
        let default =
            |name: &str| BACKGROUNDS.iter().find(|(n, _)| *n == name).map(|&(_, bg)| Rgb::hex(bg));
        for content in [default("default dark"), default("default light")].into_iter().flatten() {
            let opaque = Surfaces::derive(content);
            let glass = opaque.on_glass();
            let canvas = opaque.canvas.oklch().l;
            for wall in WALLPAPERS {
                let ground = Tint::of(opaque.canvas, alpha::GLASS).over(Rgb::hex(wall));
                let off = (ground.oklch().l - canvas).abs();
                let through = 1.0 - alpha::GLASS;
                assert!(
                    off <= through,
                    "{content:?} over {wall:06x}: the ground is {off:.3} L off"
                );
            }
            for ((ink, was), (_, now)) in inks(&opaque).into_iter().zip(inks(&glass)) {
                let deeper = (now.oklch().l - was.oklch().l).abs();
                assert!(deeper <= 0.10, "{content:?}: {ink} moves {deeper:.3} L on glass");
            }
            let text = Surfaces {
                text: opaque.text,
                text_secondary: opaque.text_secondary,
                text_muted: opaque.text_muted,
                accent: opaque.accent,
                success: opaque.success,
                warn: opaque.warn,
                error: opaque.error,
                working: opaque.working,
                merged: opaque.merged,
                ..glass
            };
            assert_eq!(text, opaque, "{content:?}: only text moves");
        }
    }

    /// The APCA arithmetic gives the published values: `#888` on white Lc 63.06, white on
    /// black −107.88, black on white 106.04.
    #[test]
    fn apca_gives_the_published_values() {
        let (white, black) = (Rgb::hex(0xff_ffff), Rgb::hex(0));
        assert!((Rgb::hex(0x88_8888).apca(white) - 63.06).abs() < 0.05);
        assert!((white.apca(black) + 107.88).abs() < 0.05);
        assert!((black.apca(white) - 106.04).abs() < 0.05);
        assert!(white.apca(white).abs() < f32::EPSILON, "no difference, no contrast");
    }

    /// Secondary text clears APCA's |Lc| 55 and muted text and the status words 45 on every
    /// ground they land on, for every background in the supported range, or go as far as black
    /// or white go: WCAG alone let dark muted text and the dark red sit at Lc 43 on the
    /// selected wash over a float.
    #[test]
    fn chrome_text_clears_apca() {
        for (name, bg) in BACKGROUNDS {
            let content = Rgb::hex(bg);
            let pole = if content.is_light() { LIGHT_TONES.pole } else { DARK_TONES.pole };
            let s = Surfaces::derive(content);
            let surfaces: Vec<Rgb> = under_text(&s, content).into_iter().map(|(_, c)| c).collect();
            for (ink, fg, least) in [
                ("text_secondary", s.text_secondary, SECONDARY_LC),
                ("text_muted", s.text_muted, MUTED_LC),
                ("accent", s.accent, MUTED_LC),
                ("warn", s.warn, MUTED_LC),
                ("error", s.error, MUTED_LC),
                ("working", s.working, MUTED_LC),
                ("merged", s.merged, MUTED_LC),
            ] {
                let lc = worst_lc(fg, &surfaces);
                assert!(lc >= least || fg == pole, "{name}: {ink} Lc {lc:.1}");
            }
        }
    }

    /// A remote picture's stage is one near-black whatever the content.
    #[test]
    fn the_stage_is_the_same_near_black_in_both_variants() {
        for content in [Rgb::hex(0x0016_1616), Rgb::hex(0x00ff_ffff), Rgb::hex(0x0028_2c34)] {
            assert_eq!(Surfaces::derive(content).stage, STAGE, "{content:?}");
        }
        assert!(STAGE.contrast(Rgb::hex(0)) < 1.1, "near-black");
    }

    /// A focused field's edge clears 3:1 on its card, quieter than the keyboard's ring and
    /// louder than its hairline at rest.
    #[test]
    fn a_focused_field_says_so_quietly_and_still_clears_three_to_one() {
        for (name, bg) in BACKGROUNDS {
            let content = Rgb::hex(bg);
            let plain = Surfaces::derive(content);
            let edge = plain.field_focus();
            let (said, full) =
                (edge.contrast(plain.elevated), plain.focus.contrast(plain.elevated));
            assert!(said >= NON_TEXT, "{name}: the edge reads {said:.2} on its card");
            assert!(said < full || full < NON_TEXT, "{name}: quieter than the ring ({said:.2})");
            let rest = plain.border.over(plain.elevated);
            assert!(said > rest.contrast(plain.elevated), "{name}: stronger than at rest");
            println!("MEASURE field focus {name}: {said:.2}:1, the ring {full:.2}:1");
        }
    }

    /// An unticked box's outline reads 3:1 on every surface it can sit on, for every supported
    /// background. It sat at about 1.3 as a dividing hairline did.
    #[test]
    fn a_control_s_outline_reads_three_to_one_everywhere() {
        for (name, bg) in BACKGROUNDS {
            let content = Rgb::hex(bg);
            let s = Surfaces::derive(content);
            let crossed = planes(&s, content).map(|(_, c)| c);
            let reads = crossed
                .iter()
                .map(|&bg| s.control.over(bg).contrast(bg))
                .fold(f32::INFINITY, f32::min);
            assert!(reads >= NON_TEXT, "{name}: {reads:.2}");
            let border = crossed
                .iter()
                .map(|&bg| s.border.over(bg).contrast(bg))
                .fold(f32::INFINITY, f32::min);
            assert!(reads > border, "{name}: louder than a divider");
        }
    }

    /// The default themes keep the tones they were designed with: the lift is a guard for
    /// other backgrounds, not a second design. At most one step of rounding moves.
    #[test]
    fn the_default_tones_are_lifted_by_a_rounding_step_at_most() {
        for (content, tones) in
            [(TerminalPalette::DARK.bg, DARK_TONES), (TerminalPalette::LIGHT.bg, LIGHT_TONES)]
        {
            let s = Surfaces::derive(content);
            let tier = |share: f32| content.mix(tones.text, share);
            for (name, derived, designed) in [
                ("text", s.text, tones.text),
                ("text_secondary", s.text_secondary, tier(tones.text_secondary)),
                ("text_muted", s.text_muted, tier(tones.text_muted)),
                ("accent", s.accent, tones.accent.rgb()),
                ("warn", s.warn, tones.warn),
                ("error", s.error, tones.error),
            ] {
                let moved = [
                    derived.r.abs_diff(designed.r),
                    derived.g.abs_diff(designed.g),
                    derived.b.abs_diff(designed.b),
                ];
                assert!(moved.iter().all(|&d| d <= 4), "{name}: {designed:?} became {derived:?}");
            }
        }
    }

    /// A mid grey is outside the range, and this is why: the chrome's own text cannot clear AA
    /// on it and a step above it at once. The derivation still returns a theme, with the text
    /// as far out as white or black take it.
    #[test]
    fn a_mid_grey_background_cannot_clear_aa() {
        let content = Rgb::hex(0x77_7777);
        let s = Surfaces::derive(content);
        let surfaces: Vec<Rgb> = under_text(&s, content).into_iter().map(|(_, c)| c).collect();
        assert!(worst(s.text, &surfaces) < AA, "{:?}", s.text);
        assert_eq!(s.text, DARK_TONES.pole, "as far as it goes");
    }

    /// The ladder climbs in order for every supported background: in dark each fill above the
    /// content is lighter than the last, in light each step below the content darker, what
    /// floats is never below the content, and the loud hairline is louder than the quiet one
    /// on every surface a hairline is drawn on.
    /// On pure black the bars cannot sink below the content and stay level with it.
    #[test]
    fn the_ladder_is_monotonic() {
        for (name, bg) in BACKGROUNDS {
            let content = Rgb::hex(bg);
            let s = Surfaces::derive(content);
            let l = Rgb::luminance;
            let c_rgb = content;
            let c = l(content);
            assert!(
                l(s.canvas) <= l(s.panel) && l(s.panel) <= c,
                "{name}: bars, navigator, content"
            );
            assert!(l(s.elevated) >= c, "{name}: what floats is not below the content");
            if content.is_light() {
                assert!(
                    l(s.border.over(c_rgb)) < l(s.border_subtle.over(c_rgb)),
                    "{name}: hairlines"
                );
            } else {
                assert!(c < l(s.elevated), "{name}: what floats is above the content");
                assert!(
                    l(s.border_subtle.over(c_rgb)) < l(s.border.over(c_rgb)),
                    "{name}: hairlines"
                );
            }
            // Each state rides a step past the last on every plane, toward the text.
            for (plane, under) in planes(&s, content) {
                let steps =
                    [under, s.hover.over(under), s.selected.over(under), s.pressed.over(under)]
                        .map(|c| (lightness(c) - lightness(under)).abs());
                assert!(
                    steps.windows(2).all(|w| w[1] > w[0] + 0.5),
                    "{name}: the states climb on {plane}: {steps:?}"
                );
            }
            // A hairline is never drawn across a selected fill.
            for (surface, under) in planes(&s, content) {
                let (loud, quiet) = (
                    s.border.over(under).contrast(under),
                    s.border_subtle.over(under).contrast(under),
                );
                assert!(quiet < loud, "{name}: the subtle hairline is quieter on {surface}");
            }
        }
    }

    /// The chrome follows the terminal's background: a light scheme set in the settings
    /// makes light chrome in its own tint, whatever the appearance asked for.
    #[test]
    fn the_chrome_follows_the_terminals_background() {
        let mut theme = Theme::new(Variant::Dark);
        theme.terminal.bg = Rgb::hex(0xfd_f6e3);
        assert_eq!(theme.variant(), Variant::Light, "the variant is the background's");
        theme.derive_chrome();
        assert_eq!(theme.surfaces, Surfaces::derive(theme.content()));
        assert_eq!(theme.elevation, Elevation::LIGHT);
        let canvas = theme.surfaces.canvas;
        assert!(canvas.r > canvas.b, "the cream survives in the bars: {canvas:?}");
    }

    /// A fill is a mark, seen by its hue: saturated and mid-light, so the light warn reads
    /// amber, not the brown of its text tone. A badge's count reads on every fill, and in dark
    /// every fill stands 3:1 off every surface (WCAG's non-text contrast).
    #[test]
    fn status_fills_read_as_their_hue() {
        /// Saturation and lightness, HSL, 0 to 1.
        fn sl(colour: Rgb) -> (f32, f32) {
            let channels = [colour.r, colour.g, colour.b].map(|v| f32::from(v) / 255.0);
            let max = channels.into_iter().fold(0.0, f32::max);
            let min = channels.into_iter().fold(1.0, f32::min);
            let lightness = f32::midpoint(max, min);
            let chroma = max - min;
            let saturation = if chroma == 0.0 {
                0.0
            } else {
                chroma / (1.0 - 2.0_f32.mul_add(lightness, -1.0).abs())
            };
            (saturation, lightness)
        }
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let fills = [
                ("accent_fill", s.accent_fill),
                ("success_fill", s.success_fill),
                ("warn_fill", s.warn_fill),
                ("error_fill", s.error_fill),
            ];
            for (name, fill) in fills {
                let (sat, light) = sl(fill);
                // The brand's own green sits at 0.48, the least saturated of the fills.
                assert!(sat >= 0.45, "{variant:?}: {name} is greyed ({sat:.2})");
                assert!((0.4..=0.75).contains(&light), "{variant:?}: {name} lightness {light:.2}");
                let on = if name == "accent_fill" { s.accent_ink } else { s.fill_fg };
                let ink = on.contrast(fill);
                assert!(ink >= AA, "{variant:?}: text on {name} is {ink:.2}");
                if variant == Variant::Dark {
                    for (surface, under) in under_text(&s, theme.content()) {
                        let seen = fill.contrast(under);
                        assert!(seen >= 3.0, "{variant:?}: {name} on {surface} is {seen:.2}");
                    }
                }
            }
            let (_, fill) = sl(s.warn_fill);
            let (_, text) = sl(s.warn);
            if variant == Variant::Light {
                assert!(fill > text + 0.15, "the light warn fill is amber, its text ochre");
            }
        }
    }

    /// What floats reads above what it covers in both variants: a step up from the content in
    /// dark and white in light, with a shadow dark enough to see on near-black. A finger gets
    /// Apple's 44 pt, a pointer rows no taller than they were.
    /// The most chroma a neutral holds: a warm paper grey, never a colour.
    const NEUTRAL_CHROMA: f32 = 0.006;

    /// One neutral, two polarities (R1): every grey of the chrome in both modes is the one
    /// warm neutral, barely tinted. Its hue is held loosely: at a chroma of 0.002 an 8-bit
    /// channel's rounding swings it by tens of degrees, so a grey is held only to the warm
    /// side, within 45° of [`NEUTRAL_HUE`], and only where it has any chroma to speak of.
    #[test]
    fn light_and_dark_share_one_neutral_hue() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let content = theme.content();
            let greys = [
                ("content", content),
                ("canvas", s.canvas),
                ("panel", s.panel),
                ("elevated", s.elevated),
                ("band", s.band),
                ("hover", s.hover.over(content)),
                ("selected", s.selected.over(content)),
                ("pressed", s.pressed.over(content)),
                ("border", s.border.over(content)),
                ("border_subtle", s.border_subtle.over(content)),
                ("text", s.text),
                ("text_secondary", s.text_secondary),
                ("text_muted", s.text_muted),
                ("selection", theme.terminal.selection),
            ];
            for (name, grey) in greys {
                let Oklch { c, h, .. } = grey.oklch();
                assert!(c <= NEUTRAL_CHROMA, "{variant:?}: {name} {grey:?} is coloured ({c:.4})");
                let off = (h - NEUTRAL_HUE).abs().min(360.0 - (h - NEUTRAL_HUE).abs());
                assert!(c < 0.0015 || off <= 45.0, "{variant:?}: {name} {grey:?} at {h:.0}°");
            }
        }
    }

    /// Nearer is lighter in both modes (R2): what floats and what is raised sits over the
    /// content in L*, and in light by a step the eye takes, not only by its ring.
    #[test]
    fn what_is_nearer_is_lighter_in_both_modes() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let content = lightness(theme.content());
            let raised =
                if variant == Variant::Light { s.elevated } else { s.hover.over(theme.content()) };
            for (name, surface) in [("elevated", s.elevated), ("raised", raised)] {
                let step = lightness(surface) - content;
                assert!(step > 0.0, "{variant:?}: {name} sits {step:.2} L* off the content");
            }
            if variant == Variant::Light {
                let step = lightness(s.elevated) - content;
                assert!(step >= 0.75, "light: a float is only {step:.2} L* over the paper");
            }
        }
    }

    /// The dividing hairline is seen equally in both modes (R5): `border` over the content sits
    /// the same L* off it, within a unit, and reads APCA Lc 14 or more on paper.
    #[test]
    fn the_dividing_hairline_is_seen_equally() {
        let off = |variant| {
            let theme = Theme::new(variant);
            let content = theme.content();
            let rule = theme.surfaces.border.over(content);
            ((lightness(content) - lightness(rule)).abs(), rule.apca(content).abs())
        };
        let ((dark, _), (light, lc)) = (off(Variant::Dark), off(Variant::Light));
        assert!(
            (dark - light).abs() <= 1.0,
            "the rule sits {dark:.2} L* off in dark, {light:.2} in light"
        );
        assert!(lc >= 14.0, "light: the rule reads Lc {lc:.1}");
    }

    /// One hue per meaning across the modes (R6): the green on the brand's 150°, waiting amber
    /// and failed red each in its own band in both, whatever lightness each mode needs.
    #[test]
    fn one_hue_per_meaning_across_modes() {
        for variant in [Variant::Dark, Variant::Light] {
            let s = Theme::new(variant).surfaces;
            for (name, colour, band) in [
                ("accent", s.accent, 145.0..=155.0),
                ("accent_fill", s.accent_fill, 145.0..=155.0),
                ("warn", s.warn, 65.0..=85.0),
                ("error", s.error, 15.0..=30.0),
            ] {
                let h = colour.oklch().h;
                assert!(band.contains(&h), "{variant:?}: {name} {colour:?} at {h:.1}°");
            }
        }
    }

    /// One lightness per role (C1): the status and identity marks share one L in each mode, so
    /// no hue shouts over another, 0.72 on near-black and 0.58 to 0.64 on paper. Amber and red
    /// keep their own lightness: amber at 0.6 is olive, and red at the others' higher L turns
    /// pink, so it stays where the colour-blind pairs (`vision`) pinned it.
    #[test]
    fn status_fills_share_one_lightness() {
        for variant in [Variant::Dark, Variant::Light] {
            let s = Theme::new(variant).surfaces;
            let named = [
                ("success_fill", s.success_fill),
                ("working_fill", s.working_fill),
                ("merged_fill", s.merged_fill),
            ];
            let identity =
                s.identity.iter().enumerate().map(|(i, &c)| (format!("identity {i}"), c));
            for (name, fill) in named.map(|(n, c)| (n.to_owned(), c)).into_iter().chain(identity) {
                let l = fill.oklch().l;
                let fits = match variant {
                    Variant::Dark => (l - 0.72).abs() <= 0.01,
                    Variant::Light => (0.575..=0.645).contains(&l),
                };
                assert!(fits, "{variant:?}: {name} {fill:?} at L {l:.3}");
            }
        }
    }

    /// Each new hue has its band (C2), and an identity colour sits 20 degrees or more from every
    /// status mark's hue, the sand excepted, so a machine's glyph never reads as a state.
    #[test]
    fn identity_keeps_clear_of_the_states() {
        let apart = |a: f32, b: f32| (a - b).abs().min(360.0 - (a - b).abs());
        for variant in [Variant::Dark, Variant::Light] {
            let s = Theme::new(variant).surfaces;
            let states = [
                ("success_fill", s.success_fill, 145.0..=155.0),
                ("warn_fill", s.warn_fill, 70.0..=85.0),
                ("error_fill", s.error_fill, 18.0..=30.0),
                ("working", s.working, 245.0..=255.0),
                ("working_fill", s.working_fill, 245.0..=255.0),
                ("merged", s.merged, 295.0..=305.0),
                ("merged_fill", s.merged_fill, 295.0..=305.0),
            ];
            for (name, colour, band) in &states {
                let h = colour.oklch().h;
                assert!(band.contains(&h), "{variant:?}: {name} {colour:?} at {h:.1}°");
            }
            let sand = s.identity.len() - 1;
            for (i, colour) in s.identity.iter().enumerate().take(sand) {
                let h = colour.oklch().h;
                for (name, state, _) in &states {
                    let off = apart(h, state.oklch().h);
                    assert!(
                        off >= 20.0,
                        "{variant:?}: identity {i} at {h:.1}° is {off:.1}° off {name}"
                    );
                }
            }
            for (i, a) in s.identity.iter().enumerate() {
                for b in s.identity.iter().skip(i + 1) {
                    let off = apart(a.oklch().h, b.oklch().h);
                    assert!(off >= 20.0, "{variant:?}: {a:?} and {b:?} are {off:.1}° apart");
                }
            }
        }
    }

    /// Working's and merged's marks read as marks (C3): 3:1 and more on the content and on the
    /// panel in both modes.
    #[test]
    fn a_status_fill_reads_as_a_mark() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            for (name, fill) in [("working_fill", s.working_fill), ("merged_fill", s.merged_fill)] {
                for (ground, under) in [("content", theme.content()), ("panel", s.panel)] {
                    let seen = fill.contrast(under);
                    assert!(seen >= NON_TEXT, "{variant:?}: {name} on {ground} is {seen:.2}");
                }
            }
        }
    }

    #[test]
    fn elevation_and_density() {
        let dark = Theme::new(Variant::Dark);
        let step = dark.surfaces.elevated.contrast(dark.content());
        assert!(step >= 1.05, "dark: elevated is {step:.3} over the content");
        let light = Theme::new(Variant::Light);
        assert_eq!(light.surfaces.elevated, Rgb::hex(0xff_ffff), "light: what floats is white");
        for theme in [&dark, &light] {
            let layers: Vec<Shadow> =
                theme.elevation.shadow.into_iter().filter(|l| l.shows()).collect();
            let (contact, soft) = (layers[0], layers[layers.len() - 1]);
            assert!(contact.blur < soft.blur && contact.y < soft.y, "a tight layer, then a soft");
            assert!(
                layers.windows(2).all(|w| w[0].blur <= w[1].blur && w[0].y <= w[1].y),
                "{:?}: tightest first",
                theme.variant()
            );
            assert!(soft.alpha >= 0.1, "{:?}: the shadow shows", theme.variant());
            // Zed's quiet floor: a dark shadow about a third of the 0.5 it was, drawn in so it
            // falls below the sheet and shows nothing above it.
            assert!(soft.alpha <= 0.17, "{:?}: the shadow pools", theme.variant());
            let v = theme.variant();
            assert!(soft.y + soft.spread > 0.0, "{v:?}: its shape starts below the top edge");
            // Its reach to the sides is a third of its reach below at most: a sheet's weight
            // falls under it, and it never rings the sheet in grey.
            let (aside, below) =
                (soft.spread + soft.blur / 2.0, soft.y + soft.spread + soft.blur / 2.0);
            assert!(
                aside <= 8.0 && aside * 3.0 <= below,
                "{v:?}: a halo, {aside} aside, {below} below"
            );
        }
        let (dark_rim, light_rim) = (dark.elevation.rim, light.elevation.rim);
        assert_eq!(dark_rim.float, Some(alpha::EDGE), "a dark sheet's lit edge");
        assert_eq!(light_rim.float, None, "white sheets read on their shadow");
        assert!(dark_rim.top && !light_rim.top, "lit from above in dark, a shade below in light");
        assert!(dark_rim.ink.is_light() && !light_rim.ink.is_light());
        assert!(dark.elevation.rest.is_none(), "dark rests on its rim alone");
        assert!(dark_rim.rest.is_some(), "dark: what rests catches the light at its top");
        assert_eq!(light_rim.rest, None, "light: what rests wears one edge, its ring");
        let rest = light.elevation.rest.unwrap_or([Shadow::NONE; 2]);
        let float = light.elevation.shadow[0].alpha;
        assert!(rest.iter().all(|l| l.shows() && l.alpha < float), "a contact under a float's");
        // R8: shadows are black in dark and the warm ink in light.
        assert_eq!(dark.elevation.shade, Rgb::hex(0), "dark: black");
        let ink = light.elevation.shade.oklch();
        assert!((ink.h - NEUTRAL_HUE).abs() < 5.0 && ink.c > 0.006, "light: the warm ink {ink:?}");
        assert!(ink.l < 0.3, "light: an ink, not a grey: {ink:?}");
        const {
            assert!(alpha::RIM < alpha::EDGE, "what rests catches less light than what floats");
            assert!(alpha::EDGE < alpha::FAINT, "the edge is the ladder's quietest step");
        };
        const { assert!(Elevation::DARK.scrim > Elevation::LIGHT.scrim, "dark dims deeper") };
        const {
            assert!(alpha::FAINT < alpha::DIM && alpha::DIM < alpha::TINT, "one ladder, in order");
            assert!(Elevation::LIGHT.scrim < alpha::TINT, "light dims a sheet's worth, not grey");
        };
        let (compact, touch) = (Density::COMPACT, Density::TOUCH);
        assert!(touch.hit >= 44.0 && touch.row >= 44.0 && touch.header >= 44.0);
        assert!(touch.control >= 44.0, "a finger's button");
        assert_eq!(
            (compact.row, compact.control, compact.header),
            (28.0, 28.0, 40.0),
            "MonoCode's"
        );
        assert!(compact.row < touch.row && compact.row_two_line < touch.row_two_line);
        assert!(compact.row < compact.row_two_line && touch.row < touch.row_two_line);
        assert_eq!(Theme::default().density, compact, "the Mac is the default");
    }

    /// The primary action is the neutral solid, `MonoCode`'s: the chrome's text as a fill, so
    /// white on dark and near-black on light, with no hue, and its ink reads at AAA on it for
    /// every supported background. The blue fill it replaces was a second brand colour.
    #[test]
    fn the_primary_is_the_neutral_solid() {
        for (name, bg) in BACKGROUNDS {
            let content = Rgb::hex(bg);
            let s = Surfaces::derive(content);
            assert_eq!(s.solid, s.text, "{name}: the solid is the text as a fill");
            let ink = s.solid_ink.contrast(s.solid);
            assert!(ink >= AAA, "{name}: ink on the solid is {ink:.2}");
            assert_ne!(s.solid.is_light(), content.is_light(), "{name}: inverted");
        }
        for variant in [Variant::Dark, Variant::Light] {
            let s = Theme::new(variant).surfaces;
            let grey = |c: Rgb| c.r.abs_diff(c.g) <= 2 && c.g.abs_diff(c.b) <= 2;
            assert!(grey(s.solid) && grey(s.solid_ink), "{variant:?}: neutral");
        }
        assert_eq!(Theme::new(Variant::Light).surfaces.solid_ink, Rgb::hex(0xff_ffff));
    }

    /// Every control and every word on a coloured fill reads, for every supported background:
    /// the solid stands 3:1 off every surface it can sit on (WCAG 1.4.11), its ink reads AA on
    /// it, at rest and pressed (the solid given [`alpha::DIM`] toward the content, the
    /// most the kit gives it); a badge's count reads AA on every fill.
    #[test]
    fn controls_and_the_words_on_fills_read() {
        for (name, bg) in BACKGROUNDS {
            let content = Rgb::hex(bg);
            let s = Surfaces::derive(content);
            for (surface, under) in under_text(&s, content) {
                let seen = s.solid.contrast(under);
                assert!(seen >= NON_TEXT, "{name}: solid on {surface} {seen:.2}");
            }
            for (state, fill) in [("rest", s.solid), ("pressed", s.solid.mix(content, alpha::DIM))]
            {
                let ink = s.solid_ink.contrast(fill);
                assert!(ink >= AA, "{name}: ink on the {state} solid {ink:.2}");
            }
            for (fill, on) in [
                ("accent_fill", s.accent_fill, s.accent_ink),
                ("warn_fill", s.warn_fill, s.fill_fg),
                ("error_fill", s.error_fill, s.fill_fg),
            ]
            .map(|(n, f, o)| (n, o.contrast(f)))
            {
                assert!(on >= AA, "{name}: words on {fill} {on:.2}");
            }
        }
    }

    /// Slopty's green in OKLCH is the brand's sRGB, give or take a rounding step.
    #[test]
    fn the_brand_in_oklch_is_the_brand() {
        let made = BRAND_OKLCH.rgb();
        let off = [made.r.abs_diff(BRAND.r), made.g.abs_diff(BRAND.g), made.b.abs_diff(BRAND.b)];
        assert!(off.iter().all(|&d| d <= 2), "{made:?} against {BRAND:?}");
        assert_eq!(Oklch { l: 1.0, c: 0.0, h: 0.0 }.rgb(), Rgb::hex(0xff_ffff));
        assert_eq!(Oklch { l: 0.0, c: 0.0, h: 0.0 }.rgb(), Rgb::hex(0));
        // Past the gamut the chroma gives way and the lightness holds.
        let wild = Oklch { l: 0.72, c: 0.6, h: 150.0 }.rgb();
        assert!(wild.g > wild.r && wild.g > wild.b, "still green: {wild:?}");
    }

    /// The interaction accent and success are the brand's green in both variants, one
    /// colour for one meaning; blue is gone from the chrome. As text it reads AA on every
    /// surface; as a mark it stands 3:1 off the content, and its ink reads AA on it.
    #[test]
    fn the_accent_is_the_brand_green_and_blue_is_gone() {
        let green = |c: Rgb| c.g > c.r.saturating_add(40) && c.g > c.b.saturating_add(40);
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            assert_eq!((s.accent, s.accent_fill), (s.success, s.success_fill), "{variant:?}");
            for (name, c) in [("accent", s.accent), ("accent_fill", s.accent_fill)] {
                assert!(green(c), "{variant:?}: {name} is {c:?}");
            }
            let mark = s.accent_fill.contrast(theme.content());
            assert!(mark >= NON_TEXT, "{variant:?}: the mark is {mark:.2} off the content");
            let ink = s.accent_ink.contrast(s.accent_fill);
            assert!(ink >= AA, "{variant:?}: ink on the green is {ink:.2}");
        }
        assert_eq!(Theme::new(Variant::Dark).surfaces.accent, BRAND, "dark speaks the brand");
        let cursor = |p: TerminalPalette| p.cursor == p.fg;
        assert!(cursor(TerminalPalette::DARK) && cursor(TerminalPalette::LIGHT), "no blue caret");
        for p in [TerminalPalette::DARK, TerminalPalette::LIGHT] {
            let c = p.selection.oklch().c;
            assert!(c <= NEUTRAL_CHROMA, "a neutral selection: {:?} at chroma {c:.4}", p.selection);
        }
    }

    /// The hairlines are the text at `MonoCode`'s 7 % in dark made a half again for the
    /// half-point stroke, a third again on white where the same share reads fainter; the hover
    /// and selected washes sit at its 5 % and 8 to 10 %, and pressed a like step past them.
    #[test]
    fn the_hairlines_and_washes_are_monocode_s_shares() {
        let share = |l: Tint| l.opacity();
        let dark = Theme::new(Variant::Dark).surfaces;
        let light = Theme::new(Variant::Light).surfaces;
        assert!(0.07_f32.mul_add(-1.45, share(dark.border)).abs() < 0.005, "{:?}", dark.border);
        let ratio = share(light.border) / share(dark.border);
        assert!((1.25..=1.45).contains(&ratio), "light is a third again: {ratio:.2}");
        for s in [dark, light] {
            assert!((0.045..=0.06).contains(&share(s.hover)), "hover {:?}", s.hover);
            assert!((0.075..=0.10).contains(&share(s.selected)), "selection {:?}", s.selected);
            let (step, next) =
                (share(s.selected) - share(s.hover), share(s.pressed) - share(s.selected));
            assert!(next >= step - 0.005, "pressed is a step past selected: {step:.3} {next:.3}");
        }
    }

    /// A well on the chrome (the navigator's filter) and a row's hover stand off the bars in
    /// both variants: the washes ride on the plane under them. As solid steps mixed for the
    /// content, the light hover sat a hundredth from the bars' own tone and vanished there.
    #[test]
    fn the_washes_stand_off_the_chrome() {
        for variant in [Variant::Dark, Variant::Light] {
            let s = Theme::new(variant).surfaces;
            let well = s.selected.over(s.canvas).contrast(s.canvas);
            assert!(well >= 1.07, "{variant:?}: the well is {well:.3} off the chrome");
            let hover = s.hover.over(s.canvas).contrast(s.canvas);
            assert!(hover >= 1.04, "{variant:?}: the hover is {hover:.3} off the chrome");
        }
    }

    /// What floats sits a clear step over the content in dark (+4.5 to +5.5 OKLCH L, near
    /// Radix's and shadcn's), and a row's hover still shows on it.
    #[test]
    fn a_float_rises_and_its_rows_still_answer_the_pointer() {
        let theme = Theme::new(Variant::Dark);
        let s = theme.surfaces;
        let (content, float) = (oklab_l(theme.content()), oklab_l(s.elevated));
        assert!((4.5..=5.5).contains(&(float - content)), "+{:.2} L", float - content);
        let hover = oklab_l(s.hover.over(s.elevated)) - float;
        assert!(hover >= 3.5, "the hover on a float is +{hover:.2} L");
    }

    /// A colour's `OKLab` lightness, 0 to 100.
    #[expect(clippy::many_single_char_names, reason = "the OKLab paper's own names")]
    fn oklab_l(c: Rgb) -> f32 {
        let lin = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        let (r, g, b) = (lin(c.r), lin(c.g), lin(c.b));
        let l = 0.051_445_99_f32.mul_add(b, 0.412_221_47_f32.mul_add(r, 0.536_332_55 * g)).cbrt();
        let m = 0.107_396_96_f32.mul_add(b, 0.211_903_5_f32.mul_add(r, 0.680_699_5 * g)).cbrt();
        let s = 0.629_978_7_f32.mul_add(b, 0.088_302_46_f32.mul_add(r, 0.281_718_85 * g)).cbrt();
        100.0 * (-0.004_072_047_f32).mul_add(s, 0.210_454_26_f32.mul_add(l, 0.793_617_8 * m))
    }

    /// A curve starts at rest and lands, and CSS's named curves read as CSS draws them: the
    /// ease-out ahead of a straight line all the way, and a curve whose control points sit on
    /// the diagonal is that line.
    #[test]
    fn a_curve_runs_from_rest_to_landed() {
        let motion = Motion::DEFAULT;
        for curve in [motion.ease_out, motion.drawer] {
            assert!(curve.at(0.0).abs() < 1e-4 && (curve.at(1.0) - 1.0).abs() < 1e-4);
            let mut last = 0.0;
            for step in 1..=20_u8 {
                let y = curve.at(f32::from(step) / 20.0);
                assert!(y >= last - 1e-4, "{curve:?} turns back at {step}");
                last = y;
            }
        }
        for step in 1..10_u8 {
            let t = f32::from(step) / 10.0;
            assert!(motion.ease_out.at(t) > t, "ease-out leads at {t}");
        }
        let linear = Curve { p1: (0.25, 0.25), p2: (0.75, 0.75) };
        assert!((linear.at(0.3) - 0.3).abs() < 1e-3);
        assert!(motion.fade <= motion.settle && motion.settle < motion.sheet);
        assert!(motion.exit < motion.fade, "what leaves goes quicker than it came");
        assert!(motion.unhover > motion.hover, "and lets go softly");
        assert!(motion.hover.is_zero(), "hover is instant");
        assert!(motion.fade < motion.stream, "a stream's lift has room to follow its pace");
        assert!(motion.stream_stagger * 6 < motion.fade, "a chunk lights up as one gesture");
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

    /// The light ANSI colours as generated: each slot's OKLCH, its hue taken from the dark
    /// palette's (green on the brand's), the normals at L 0.525, the brights at 0.465, chroma
    /// capped per hue where sRGB would clip it unevenly, and the greys on the one neutral.
    const LIGHT_ANSI: [Oklch; 16] = {
        const fn at(l: f32, c: f32, h: f32) -> Oklch {
            Oklch { l, c, h }
        }
        const GREEN: f32 = 148.0;
        [
            at(0.226, 0.004, NEUTRAL_HUE),
            at(0.525, 0.190, 20.0),
            at(0.525, 0.150, GREEN),
            at(0.525, 0.108, 82.0),
            at(0.525, 0.170, 255.0),
            at(0.525, 0.190, 318.0),
            at(0.525, 0.090, 206.0),
            at(0.450, 0.003, NEUTRAL_HUE),
            at(0.556, 0.003, NEUTRAL_HUE),
            at(0.465, 0.180, 20.0),
            at(0.465, 0.135, GREEN),
            at(0.465, 0.096, 82.0),
            at(0.465, 0.153, 255.0),
            at(0.465, 0.190, 318.0),
            at(0.465, 0.080, 206.0),
            at(0.665, 0.003, NEUTRAL_HUE),
        ]
    };

    /// The light palette is what its generator makes, and the generator's hues are the dark
    /// palette's: a colour is one hue in both modes (within 10°, green within 15° since it
    /// moves to the brand's), and the greys share the one neutral.
    #[test]
    fn light_ansi_is_generated_from_the_dark_hues() {
        for (ix, (made, spec)) in TerminalPalette::LIGHT.ansi.iter().zip(LIGHT_ANSI).enumerate() {
            let want = spec.rgb();
            let off = [(made.r, want.r), (made.g, want.g), (made.b, want.b)]
                .iter()
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap_or(0);
            assert!(off <= 1, "ANSI {ix}: {made:?}, generated {want:?}");
        }
    }

    /// One hue per ANSI slot across the modes (R7): a red is the same red on black and on
    /// paper, so code and a program's output read alike in either.
    #[test]
    fn ansi_slots_keep_their_hue_across_modes() {
        let chromatic = (1..=6).chain(9..=14);
        for ix in chromatic {
            let (dark, light) =
                (TerminalPalette::DARK.ansi[ix].oklch(), TerminalPalette::LIGHT.ansi[ix].oklch());
            let apart = (dark.h - light.h).abs();
            let apart = apart.min(360.0 - apart);
            let within = if ix % 8 == 2 { 15.0 } else { 10.0 };
            assert!(apart <= within, "ANSI {ix}: {:.0}° dark, {:.0}° light", dark.h, light.h);
        }
        for ix in [0, 7, 8, 15] {
            let grey = TerminalPalette::LIGHT.ansi[ix].oklch();
            assert!(grey.c <= NEUTRAL_CHROMA, "ANSI {ix} is a neutral: {grey:?}");
        }
    }

    /// A bright is at least as strong as its normal against the ground, in both modes (R7):
    /// "bright" means "stronger", never "paler", and in light the normals sit within 8 Lc of
    /// each other. Dark keeps One Dark's uneven normals, which the person chose.
    #[test]
    fn ansi_brights_are_at_least_as_strong_as_their_normals() {
        for (name, palette) in [("dark", TerminalPalette::DARK), ("light", TerminalPalette::LIGHT)]
        {
            let strength = |ix: usize| palette.ansi[ix].apca(palette.bg).abs();
            for ix in 1..=6 {
                let (normal, bright) = (strength(ix), strength(ix + 8));
                assert!(
                    bright >= normal,
                    "{name}: ANSI {} at Lc {bright:.0}, {ix} at {normal:.0}",
                    ix + 8
                );
            }
        }
        let light: Vec<f32> =
            (1..=6).map(|ix| TerminalPalette::LIGHT.ansi[ix].apca(LIGHT_BG).abs()).collect();
        let (least, most) =
            light.iter().fold((f32::MAX, f32::MIN), |(lo, hi), lc| (lo.min(*lc), hi.max(*lc)));
        assert!(most - least <= 8.0, "light normals span Lc {least:.0} to {most:.0}");
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
