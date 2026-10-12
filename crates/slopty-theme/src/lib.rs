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

/// A colour at some strength.
///
/// A wash is laid over whatever it crosses (a selection over text, a changed word in a diff, a
/// quiet status ground); at full strength it is an opaque state or line that is the same on
/// every plane, as Zed's are.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Tint {
    /// What it is drawn in.
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
}

/// The dark ground: the work, every pane and the terminal's grid. Delta's `editor.background`,
/// One Dark pulled darker and cooler: a small blue chroma at hue about 215°, as Zed's planes carry
/// (`docs/decisions/ui.md`, "The design system is Zed's, with Delta's values").
#[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
const DARK_BG: Rgb = Rgb::hex(0x191c1f);
/// The light ground: One Light's `editor.background`, which Delta keeps.
#[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
const LIGHT_BG: Rgb = Rgb::hex(0xfafafa);

impl TerminalPalette {
    /// The default dark palette: One Dark's terminal, on Delta's ground.
    #[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
    pub const DARK: Self = Self {
        fg: Rgb::hex(0xabb2bf),
        bg: DARK_BG,
        // Zed's first player: the caret in the accent's blue.
        cursor: Rgb::hex(0x74ade8),
        // A block cursor cuts its cell out of the background, as ghostty draws it.
        cursor_text: DARK_BG,
        // The accent at Zed's `3d` over the ground, opaque.
        selection: Rgb::hex(0x2f3f4f),
        // Zed's `search.match_background`, the accent at `66` over the ground.
        search_match: Rgb::hex(0x3d566f),
        search_current: Rgb::hex(0x8c6a1f),
        // One Dark's, with its bright black lifted toward the text to read AA on the ground.
        ansi: [
            Rgb::hex(0x282c34),
            Rgb::hex(0xe06c75),
            Rgb::hex(0x98c379),
            Rgb::hex(0xe5c07b),
            Rgb::hex(0x61afef),
            Rgb::hex(0xc678dd),
            Rgb::hex(0x56b6c2),
            Rgb::hex(0xabb2bf),
            Rgb::hex(0x7b8497),
            Rgb::hex(0xea858b),
            Rgb::hex(0xaad581),
            Rgb::hex(0xffd885),
            Rgb::hex(0x85c1ff),
            Rgb::hex(0xd398eb),
            Rgb::hex(0x6ed5de),
            Rgb::hex(0xfafafa),
        ],
        minimum_contrast: 100,
    };
    /// The default light palette: One Light's text, ground and caret, and colours generated
    /// from the dark one's hues, since One Light's yellow and white read under 2:1 on paper.
    /// Each colour keeps its slot's hue (green moves to the brand's, since at One Dark's 133°
    /// and this lightness it turns olive), the normals sit at OKLCH L 0.525 and the brights at
    /// 0.465, stronger on paper as the dark brights are stronger on black, with as much chroma
    /// as sRGB allows up to a cap. The greys are true neutrals; 7 is the ordinary text and 8 the
    /// dim slot, as in dark. `light_ansi_is_generated_from_the_dark_hues` regenerates every
    /// slot.
    #[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
    pub const LIGHT: Self = Self {
        fg: Rgb::hex(0x2a2c33),
        bg: LIGHT_BG,
        // The accent's word tone: One Light's `5c78e2` cut out of paper reads under AA.
        cursor: Rgb::hex(0x465bac),
        cursor_text: LIGHT_BG,
        // The accent at Zed's `3d` and `66` over the ground, opaque.
        selection: Rgb::hex(0xd4dbf4),
        search_match: Rgb::hex(0xbbc6f0),
        search_current: Rgb::hex(0xf5b942),
        ansi: [
            Rgb::hex(0x1c1c1c),
            Rgb::hex(0xbf2239),
            Rgb::hex(0x068032),
            Rgb::hex(0x896301),
            Rgb::hex(0x0668c9),
            Rgb::hex(0x953aaf),
            Rgb::hex(0x007984),
            Rgb::hex(0x555555),
            Rgb::hex(0x6f6f6f),
            Rgb::hex(0xa70d2c),
            Rgb::hex(0x006d28),
            Rgb::hex(0x745300),
            Rgb::hex(0x0057ac),
            Rgb::hex(0x83259c),
            Rgb::hex(0x00666f),
            Rgb::hex(0x949494),
        ],
        minimum_contrast: 100,
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
    /// Monospace family for terminals and code; the first installed one wins, and the bundled
    /// face stands in when none is.
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
    /// Line height of the conversation's prose, read at length: Delta's, 15/21 (measured from
    /// the signed-in app, `.research/delta-signed-in-2026-10-12.md` §2.4).
    pub prose_line_height: f32,
    /// Line height of the composer's text: the prose's, since Delta's composer is the next
    /// turn's row, set as the turns are.
    pub composer_line_height: f32,
    /// The interface family: the chrome, the prose and the composer.
    pub ui_family: String,
    /// The chrome's label size: Zed's default label, 14 at its 16 rem. The smaller labels and
    /// the icons hang off it.
    pub ui_size: f32,
    /// What is read at length, in points: an agent's answers and the person's messages, and
    /// the composer they are written in. Its own setting, so reading can grow while the chrome
    /// keeps its size.
    pub prose_size: f32,
}

impl Typography {
    /// Markdown's strong words and a few titles: Zed's `SemiBold`. Zed bundles no medium weight,
    /// so a state is said by its tone, never by a heavier cut.
    pub const STRONG_WEIGHT: f32 = 600.0;

    /// The smallest chrome size (base − 4): Zed's extra-small label, 10, for HUD readouts,
    /// timestamps and counts.
    #[must_use]
    pub fn caption(&self) -> f32 {
        (self.ui_size - 4.0).max(6.0)
    }

    /// Secondary chrome (base − 2): Zed's small label, 12, for bar labels, folds, tool
    /// summaries, section labels, a row's second line, a bar's readouts and a status word.
    #[must_use]
    pub fn small(&self) -> f32 {
        (self.ui_size - 2.0).max(7.0)
    }

    /// Prose read at length: an assistant's answer and the prompt it answers, at the regular
    /// weight on [`Self::prose_line_height`]; [`Self::prose_size`], never under 6.
    #[must_use]
    pub const fn prose(&self) -> f32 {
        self.prose_size.max(6.0)
    }

    /// Every icon's slot (base + 2): a row's lead, a tile's kind, a status mark, a bar button,
    /// 16 at the default size.
    #[must_use]
    pub fn icon(&self) -> f32 {
        self.ui_size + 2.0
    }

    /// A glyph's drawing inside its slot (base): 14 at the default size, where Octicons' 1.5
    /// line on its 16 grid is the 1.31 px stem of the words beside it, with a point of air
    /// round it in the slot, as Zed sets its 12 px figure in a 16 slot.
    #[must_use]
    pub const fn glyph(&self) -> f32 {
        self.ui_size
    }

    /// A small glyph (base − 2): a thread's fold disclosure, Zed's `IconSize::Small`, 12 at the
    /// default size.
    #[must_use]
    pub fn icon_small(&self) -> f32 {
        self.ui_size - 2.0
    }
}

/// A type role: a size, the line it sits on, and its weight, in points at zoom 1.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TypeRole {
    /// The text's size.
    pub size: f32,
    /// The height of its line.
    pub line: f32,
    /// Its weight: [`Typography::REGULAR_WEIGHT`] or [`Typography::STRONG_WEIGHT`].
    pub weight: f32,
}

/// The type roles: what a piece of text is, each its size, line and weight, for a pointer or a
/// finger ([`Typography::roles`]).
///
/// A role says what the text is, and its numbers follow: the desktop's Zed's label and headline
/// ladder from the 14 pt chrome, the touch ones a step larger so a label has the presence its
/// 44 pt row gives it. Each follows the chrome size setting, and prose its own. Every role is the
/// regular weight but a panel's and a page's title: Zed sets nearly everything at 400.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TypeRoles {
    /// A timestamp or a dense diagnostic figure, which can be left out: Zed's extra-small
    /// label, 10/14. Never a reason or a scope the person must read.
    pub caption: TypeRole,
    /// Facts about a thing: where it is, when, its counts. Zed's small label, 12/16.
    pub metadata: TypeRole,
    /// A menu's, a picker's, a tab's, a page's and a dialog's words: Zed's default label, 14/20.
    pub chrome: TypeRole,
    /// The window's own rows: a navigator row and its section heads, a column's title in its
    /// header, the composer's foot, a palette row, a toast. Delta's small label, 12/16,
    /// measured from the signed-in app (`.research/delta-signed-in-2026-10-12.md` §3.1).
    pub row: TypeRole,
    /// A notice's words, which may run to a second line: the row's 12 on Delta's toast's 19,
    /// measured from the signed-in app (`.research/delta-signed-in-2026-10-12.md` §2.10).
    pub notice: TypeRole,
    /// The same, for what acts or is chosen: 14/20. Chosen is said by tone, not weight.
    pub action: TypeRole,
    /// What a piece of work is called where it is the thing to act on: a task, a request
    /// put to the person. 14/20, told by its place and tone.
    pub task_title: TypeRole,
    /// A section's heading in a list or a panel: 12/16, a tier under the words it heads in
    /// tone. It leads its group by its place and its tone, not by weight.
    pub section: TypeRole,
    /// A panel's or a dialog's title: Zed's large label, 16/22 at the strong weight.
    pub panel_title: TypeRole,
    /// Code where it is read (a thread's well, a diff, the review), in the code face: Delta's
    /// 15/21, measured from the signed-in app (`.research/delta-signed-in-2026-10-12.md` §3.4).
    pub code: TypeRole,
    /// What is read at length, at [`Typography::prose_size`]: Delta's 15/21.
    pub prose: TypeRole,
    /// What the person writes in the composer, at [`Typography::prose_size`]: 15/21.
    pub composer: TypeRole,
    /// A page's heading inside the window (a settings page, the empty workspace's question):
    /// Delta's settings title, 16/22 at the regular weight
    /// (`.research/delta-re-2026-10-12.md` §2.2).
    pub page_heading: TypeRole,
    /// The heading of a page that is the whole window, the first run: Zed's extra-large
    /// headline, 23/30 at the regular weight.
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
        let (regular, strong) = (Self::REGULAR_WEIGHT, Self::STRONG_WEIGHT);
        let prose = self.prose() + if touch { 2.0 } else { 0.0 };
        TypeRoles {
            caption: role((-4.0, 4.0), (-2.0, 4.0), regular),
            metadata: role((-2.0, 4.0), (-1.0, 5.0), regular),
            chrome: role((0.0, 6.0), (3.0, 5.0), regular),
            row: role((-2.0, 4.0), (3.0, 5.0), regular),
            notice: role((-2.0, 7.0), (3.0, 5.0), regular),
            action: role((0.0, 6.0), (3.0, 5.0), regular),
            task_title: role((0.0, 6.0), (3.0, 5.0), regular),
            section: role((-2.0, 4.0), (-1.0, 7.0), regular),
            panel_title: role((2.0, 6.0), (5.0, 5.0), strong),
            code: role((1.0, 6.0), (-1.0, 7.0), regular),
            prose: TypeRole {
                size: prose,
                line: (prose * self.prose_line_height).round(),
                weight: regular,
            },
            composer: TypeRole {
                size: prose,
                line: (prose * self.composer_line_height).round(),
                weight: regular,
            },
            page_heading: role((2.0, 6.0), (8.0, 6.0), regular),
            first_run: role((9.0, 7.0), (14.0, 6.0), regular),
        }
    }
}

impl Default for Typography {
    fn default() -> Self {
        Self {
            // Zed's `.ZedMono`, bundled, so nothing after it is ever reached.
            mono_families: vec!["Lilex".to_owned()],
            mono_size: 13.0,
            ligatures: true,
            mono_line_height: 1.0,
            markdown_line_height: 1.5,
            // Delta's 15/21, measured from the signed-in app, for the turns and the next
            // turn's row alike.
            prose_line_height: 1.4,
            composer_line_height: 1.4,
            // Zed's `.ZedSans`, bundled with its four weights.
            ui_family: "IBM Plex Sans".to_owned(),
            ui_size: 14.0,
            // Delta's prose default, a point over the chrome's labels.
            prose_size: 15.0,
        }
    }
}

/// Corner radii, in points: GPUI's ladder as Zed spends it, 2, 4, 6 and 8.
///
/// Structure is square, with no token: bars, tabs, panes, panels and every row of a list or a
/// tree meet along a line. Controls are [`Self::sm`], cards [`Self::md`], and everything that
/// floats [`Self::lg`].
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Radii {
    /// Tiny boxes: a checkbox, a key cap, a fold toggle, a small chip in a row.
    pub xs: f32,
    /// A control: a button, an icon button, a field, a chip, a row that opts into rounding.
    pub sm: f32,
    /// A card: a tool call's, a question's.
    pub md: f32,
    /// Everything that floats: menus, popovers, tooltips, the palette, pickers, toasts.
    pub lg: f32,
    /// A dialog, which is a float like the rest: the same as [`Self::lg`].
    pub xl: f32,
    /// A capsule, for a switch, a count badge, a dot, a scrollbar's thumb and a one-line
    /// message bubble only. Larger than any side it rounds, so the ends are half circles.
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
        Self { xs: 2.0, sm: 4.0, md: 6.0, lg: 8.0, xl: 8.0, full: 9_999.0 }
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
    /// The one edge grid: how far in from its region's edge every leading edge starts, Zed's
    /// rows' `px_2` (8). A tile's header, a terminal's grid, a note's and a file's text, the
    /// navigator's rows, the palette's, the inbox's and the status bar's all start here, so they
    /// share one edge, clear of the dividers between flush panes.
    #[must_use]
    pub const fn inset(&self) -> f32 {
        self.sm
    }

    /// How far a row's trailing edge sits in (6), Zed's list item's `px(Base06)`. What ends a
    /// row is usually an icon button, whose own square adds the rest.
    #[must_use]
    pub const fn inset_trailing(&self) -> f32 {
        self.md / 2.0
    }

    /// How far apart a row's parts stand (6), Zed's `gap_1p5`: its lead, its words and its
    /// end slot.
    #[must_use]
    pub const fn row_gap(&self) -> f32 {
        self.md / 2.0
    }

    /// A page's side inset (32), Zed's `px_8`: the settings page and the empty state.
    #[must_use]
    pub const fn page(&self) -> f32 {
        self.xxl
    }
}

impl Default for Spacing {
    fn default() -> Self {
        Self { xxs: 2.0, xs: 4.0, sm: 8.0, md: 12.0, lg: 16.0, xl: 24.0, xxl: 32.0, xxxl: 48.0 }
    }
}

/// Line widths, in points.
pub mod stroke {
    /// The line: a point, Zed's 1 px, for every border and divider (a pane's sash, a bar's
    /// edge, a control's or a card's ring, an unticked box).
    ///
    /// GPUI rounds a stroke to whole device pixels, so it is one on a 1x screen and two on a
    /// Retina one: the same weight in points at every scale, where half a point is one device
    /// pixel at both and so twice the weight at 1x
    /// (`tests::the_line_weighs_the_same_at_every_scale`). The line shares are set for it.
    pub const LINE: f32 = 1.0;
    /// The keyboard's focus border: Zed's 1 px in `border.focused`, all round.
    pub const FOCUS: f32 = 1.0;
    /// The right edge of the row that has the keyboard, inside its focus border: Zed's
    /// project panel's `border_r_2`.
    pub const KEYED_EDGE: f32 = 2.0;
    /// A line that marks one place in the terminal's grid: a prompt's mark, the find match's
    /// underline. No tab or pane carries a coloured edge; a split's focused pane leads its row
    /// with a dot.
    pub const MARK: f32 = 1.5;
    /// A terminal block's state down its left edge (a failed command's): Warp's 3 pt bar.
    pub const BAR: f32 = 3.0;
    /// A notice's tone down its left edge, a band under a header or a row in a list: 2 pt,
    /// the identity bar's weight (`.research/design-polish-2026-10-12.md` §1.19).
    pub const NOTICE: f32 = 2.0;
    /// An overlay scrollbar's thumb, across: Zed's 6 pt, whatever the grid's cell.
    pub const SCROLLBAR: f32 = 6.0;
}

/// Opacities for the few real washes over a surface.
///
/// A selection over text, a scrim, a quiet pill. Planes, lines and
/// states are opaque colours, as Zed's are ([`Surfaces`]).
pub mod alpha {
    /// Barely there: the faint fill of a quiet pill, a wash across the terminal grid (a block
    /// separator, the visual bell).
    pub const FAINT: f32 = 0.12;
    /// A step past [`FAINT`]: a pressed wash, given this far toward its plane.
    pub const DIM: f32 = 0.16;
    /// A tint that has to be seen: answer buttons, a selection.
    pub const TINT: f32 = 0.25;
    /// A tint under the pointer, a scrollbar thumb.
    pub const PRESSED: f32 = 0.4;
    /// The accent's underline under a path that opens its file, in a thread's call: Delta's
    /// 40 %.
    pub const LINK: f32 = 0.4;
    /// The window under a modal in dark: black at 40 %.
    pub const SCRIM: f32 = 0.40;
    /// The window under a modal in light: black at 24 %, since 40 % over paper reads as mud
    /// (`.research/design-polish-2026-10-12.md` §1.19).
    pub const SCRIM_LIGHT: f32 = 0.24;
    /// Present but set back: a read row in the inbox, a thread's fold disclosure under the
    /// pointer.
    pub const STRONG: f32 = 0.7;
    /// Barely drawn until wanted: a thread's fold disclosure at rest, Zed's 30 %; and how far
    /// the activity bar's ground moves toward the selection.
    pub const QUIET: f32 = 0.3;
    /// How strongly a thread's call card and a thought's rail draw the border: Zed's tool card
    /// border, 80 %.
    pub const CALL_BORDER: f32 = 0.8;
    /// How strongly a row wearing an identity colour is tinted in it, under its 2 pt bar:
    /// Delta's colour-coded threads (`.research/delta-re-2026-10-12.md` §2.9).
    pub const IDENTITY: f32 = 0.08;
    /// The same on the selected row of a coloured run, under its 3 pt bar: Delta's strong tint,
    /// measured from the signed-in app (`.research/delta-signed-in-2026-10-12.md` §2.3).
    pub const IDENTITY_SELECTED: f32 = 0.2;
}

/// Slopty's green: OKLCH 0.72 0.16 150 (`docs/decisions/brand.md`). The mark's and the icon's
/// colour; the chrome's accent is Zed's blue.
pub const BRAND: Rgb = Rgb::hex(0x004a_c06c);

/// WCAG AA for body text: the least contrast chrome text has on any surface it lands on.
const AA: f32 = 4.5;

/// WCAG's least contrast for what is seen but not read (1.4.11): the focus border, a mark.
const NON_TEXT: f32 = 3.0;

/// APCA's floor for secondary text on every ground it lands on: its 60 for content text that
/// is not body, less 5 for chrome set at 12 to 14 pt.
const SECONDARY_LC: f32 = 55.0;

/// APCA's floor for muted text: a level under [`SECONDARY_LC`], still past its 45 for large or
/// heavy text, which hints and ages read beside.
const MUTED_LC: f32 = 45.0;

/// How far apart two text levels stay: each reads at least a quarter again the contrast of the
/// level under it, on the surface where both read worst. Past it, muted and secondary text
/// would be two names for one grey.
const LEVEL: f32 = 1.25;

/// Surface colours for chrome (not the terminal grid): Zed's token model with Delta's values
/// (`docs/decisions/ui.md`, "The design system is Zed's, with Delta's values").
///
/// Three cool planes: the work darkest ([`Self::ground`], Zed's `editor.background`), the
/// navigator a step up ([`Self::sidebar`], `panel.background`), and what floats a step more
/// ([`Self::elevated`]). Lines are opaque in two weights, [`Self::stroke`] between regions and
/// [`Self::border`] round controls and cards. The states are opaque too, one hover and one
/// selection, the same on every plane, as Zed's ghost elements are. Colour is spent on state
/// and on the accent, Zed's blue; green is success alone.
///
/// The values are Delta's, as its app draws them; only the text tones move, lifted toward
/// black or white as far as it takes to clear WCAG AA and APCA on every surface text lands on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Surfaces {
    /// The work: every pane, the terminal's grid, the active tab. Zed's `editor.background`.
    pub ground: Rgb,
    /// The navigator, the rail, the settings' sections, a column's title row and the status
    /// bar: Zed's `panel.background`, a step up from the work in dark.
    pub sidebar: Rgb,
    /// What floats: menus, popovers, the palette, dialogs, toasts. Zed's
    /// `elevated_surface.background`, standing off what it covers by its line and its shadow.
    pub elevated: Rgb,
    /// A pane's tab bar and its inactive tabs: the header row's plane, between the work and the
    /// sidebar, as the signed-in Delta draws it (`.research/delta-signed-in-2026-10-12.md`
    /// §2.1); the active tab takes the work's ground.
    pub tab_bar: Rgb,
    /// A column's header row: the tab bar's plane, so the window's top line is one band.
    pub header: Rgb,
    /// A code, diff or command well: the work sunk, 12 % toward black in dark and 2 % in light.
    pub well: Rgb,
    /// A call card's header: Zed's element background with a breath of the text.
    pub card_head: Rgb,
    /// The pointer over a row or a button: Zed's `ghost_element.hover`, opaque.
    pub hover: Tint,
    /// The chosen one of a list (a selected row, a toggle that is on, a menu that is open):
    /// Zed's `ghost_element.selected`, opaque.
    pub selected: Tint,
    /// The selected row of a list that has the keyboard: the selection, told apart by its
    /// [`Self::focus`] border and its [`stroke::KEYED_EDGE`], as Zed's project panel does.
    pub keyed: Tint,
    /// What is held down: Zed's `ghost_element.active`, the selection's tone.
    pub pressed: Tint,
    /// The line between regions, at 1 pt: the sash between panes, the navigator's edge, a bar's
    /// foot, a divider inside a pane. Zed's `border.variant`.
    pub stroke: Tint,
    /// The line round a control or a card, a tab's sides, a split: Zed's `border`.
    pub border: Tint,
    /// The line round a thread's call card and down a thought's side: Zed's tool card border,
    /// [`Self::border`] at 80 %, so what an agent did sits a step quieter than a control.
    pub call_border: Tint,
    /// The keyboard's border: Zed's `border.focused`, drawn at [`stroke::FOCUS`], moved toward
    /// the accent as far as it takes to stand 3:1 off every plane and row it rings (WCAG
    /// 2.4.13), where Zed's own reads 2.4 on a selected row.
    pub focus: Rgb,
    /// The line round what is chosen: Zed's `border.selected`.
    pub selected_border: Rgb,
    /// A tree's indent guide: Delta's `panel.indent_guide`, a wash in light.
    pub indent_guide: Tint,
    /// A filled button's or a field's ground: Zed's `element.background`.
    pub element: Rgb,
    /// Where a dragged thing would land: Zed's `drop_target.background`.
    pub drop_target: Tint,
    /// Primary text.
    pub text: Rgb,
    /// Labels, tool summaries, counts: Zed's `text.muted`.
    pub text_secondary: Rgb,
    /// Hints, timestamps, placeholders: Zed's `text.placeholder`, lifted to read AA.
    pub text_muted: Rgb,
    /// A glyph at full strength: Delta's `icon`.
    pub icon: Rgb,
    /// A glyph set back: Delta's `icon.muted`.
    pub icon_muted: Rgb,
    /// The accent as text and lines: a link, a mark that says "live" or "chosen". Zed's
    /// `text.accent`, blue.
    pub accent: Rgb,
    /// Done, connected, lines added: as text. Zed's `success`, green.
    pub success: Rgb,
    /// Agent waiting, "N need you", reconnecting: as text. Zed's `warning`.
    pub warn: Rgb,
    /// A failed result or command: as text. Zed's `error`.
    pub error: Rgb,
    /// The accent as a mark: an unread dot, a busy bar, a progress line, a switch that is on.
    /// Zed's `icon.accent`.
    pub accent_fill: Rgb,
    /// Success as a mark: a dot, a bar, a badge. Zed's `created`.
    pub success_fill: Rgb,
    /// Warn as a mark: the attention bar, the bell's badge. Zed's `modified`.
    pub warn_fill: Rgb,
    /// Error as a mark: a failed glyph, a badge. Zed's `deleted`.
    pub error_fill: Rgb,
    /// Working as text: the accent's blue, Zed's `info`.
    pub working: Rgb,
    /// Working as a mark: the accent's blue.
    pub working_fill: Rgb,
    /// Merged as text: Zed's violet player.
    pub merged: Rgb,
    /// Merged as a mark: a merged task's check, a merged pull request's glyph.
    pub merged_fill: Rgb,
    /// The accent's quiet ground: a tinted button's, an info notice's. Zed's `info.background`.
    pub info_background: Tint,
    /// The line round it: Zed's `info.border`.
    pub info_border: Rgb,
    /// Success's quiet ground: Zed's `success.background`.
    pub success_background: Tint,
    /// The line round it: Zed's `success.border`.
    pub success_border: Rgb,
    /// Warning's quiet ground: Zed's `warning.background`.
    pub warning_background: Tint,
    /// The line round it: Zed's `warning.border`.
    pub warning_border: Rgb,
    /// Error's quiet ground, the destructive button's under the [`Self::error`] word: Zed's
    /// `error.background`.
    pub error_background: Tint,
    /// The line round it: Zed's `error.border`.
    pub error_border: Rgb,
    /// An added line's band in a diff: Delta's `version_control.added`.
    pub vc_added_line: Rgb,
    /// A removed line's band in a diff: Delta's `version_control.deleted`.
    pub vc_deleted_line: Rgb,
    /// The words added inside a changed line: Zed's `version_control.word_added`.
    pub vc_word_added: Tint,
    /// The words removed inside a changed line: Zed's `version_control.word_deleted`.
    pub vc_word_deleted: Tint,
    /// A line number: Zed's `editor.line_number`.
    pub line_number: Rgb,
    /// The caret's line number: Zed's `editor.active_line_number`.
    pub line_number_active: Rgb,
    /// A machine's or a project's own colour, on its glyph and its row's bar, picked by a hash
    /// of its id (`slopty_ui::identity::identity_ink`). Eight hues at one lightness and a low
    /// chroma, each kept 20 degrees or more from every state's hue: a tint that tells machines
    /// apart side by side and never reads as a state.
    pub identity: [Rgb; IDENTITY_HUES],
    /// Text on the success, warn and error fills: a badge's count.
    pub fill_fg: Rgb,
    /// Text on the accent fill: a badge's count on the blue.
    pub accent_ink: Rgb,
    /// What a remote window or display sits on where its picture does not reach: a neutral
    /// near-black in both variants, as every remote-desktop client letterboxes. A remote screen
    /// is media, and dark bars keep its edge read as a screen's (`docs/decisions/video.md`,
    /// "Remote pictures sit on a dark stage").
    pub stage: Rgb,
}

/// [`Surfaces::stage`] in the standard look.
pub const STAGE: Rgb = Rgb::hex(0x000a_0a0a);

/// One appearance's values before the text is lifted: Delta's where it sets one, else One's.
#[derive(Clone, Copy, Debug)]
struct Tones {
    ground: Rgb,
    sidebar: Rgb,
    elevated: Rgb,
    tab_bar: Rgb,
    header: Rgb,
    well: Rgb,
    card_head: Rgb,
    hover: Rgb,
    selected: Rgb,
    stroke: Rgb,
    border: Rgb,
    focus: Rgb,
    selected_border: Rgb,
    indent_guide: Tint,
    element: Rgb,
    drop_target: Tint,
    /// Where text moves when it has to read better: the pole away from the surfaces.
    pole: Rgb,
    text: Rgb,
    text_secondary: Rgb,
    text_muted: Rgb,
    icon: Rgb,
    icon_muted: Rgb,
    accent: Rgb,
    accent_fill: Rgb,
    success: Rgb,
    success_fill: Rgb,
    warn: Rgb,
    warn_fill: Rgb,
    error: Rgb,
    error_fill: Rgb,
    merged: Rgb,
    merged_fill: Rgb,
    /// The quiet grounds and their lines: info, success, warning, error.
    backgrounds: [Tint; 4],
    borders: [Rgb; 4],
    vc_added_line: Rgb,
    vc_deleted_line: Rgb,
    vc_word_added: Tint,
    vc_word_deleted: Tint,
    line_number: Rgb,
    line_number_active: Rgb,
    /// The identity hues, at one lightness.
    identity: [Oklch; IDENTITY_HUES],
    fill_fg: Rgb,
    accent_ink: Rgb,
}

/// Slopty's green in OKLCH (`docs/decisions/brand.md`): [`BRAND`] is its sRGB.
pub const BRAND_OKLCH: Oklch = Oklch { l: 0.72, c: 0.16, h: 150.0 };

/// How many identity colours there are ([`Surfaces::identity`]).
pub const IDENTITY_HUES: usize = 8;

/// The identity hues, in degrees, with their chroma as a share of the mode's: an orange, four
/// steps from green-teal to azure, a violet between the accent and merged, a pink, and a sand
/// at a third of it. Each sits 20 degrees or more from every state's hue in both modes (green
/// at 130 to 145, amber at 85 to 94, red at 17 to 30, the accent at 250 to 270, merged at 315
/// to 324), the sand excepted, since at its chroma it reads as no state.
const IDENTITY: [(f32, f32); IDENTITY_HUES] = [
    (57.0, 1.0),
    (165.0, 1.0),
    (185.5, 1.0),
    (207.0, 1.0),
    (228.5, 1.0),
    (292.5, 1.0),
    (350.0, 1.0),
    (85.0, 0.05 / 0.13),
];

/// The identity hues at lightness `l` and chroma `c`.
const fn identity(light: f32, chroma: f32) -> [Oklch; IDENTITY_HUES] {
    const fn one(light: f32, chroma: f32, (hue, share): (f32, f32)) -> Oklch {
        Oklch { l: light, c: chroma * share, h: hue }
    }
    let [orange, green, teal, cyan, azure, violet, pink, sand] = IDENTITY;
    [
        one(light, chroma, orange),
        one(light, chroma, green),
        one(light, chroma, teal),
        one(light, chroma, cyan),
        one(light, chroma, azure),
        one(light, chroma, violet),
        one(light, chroma, pink),
        one(light, chroma, sand),
    ]
}

/// A wash of `ink` at `alpha` (0 to 255), as Zed writes `#RRGGBBAA`.
const fn wash(ink: u32, alpha: u8) -> Tint {
    Tint { ink: Rgb::hex(ink), alpha }
}

/// An opaque colour as a [`Tint`]: a state or a line, the same on every plane.
const fn opaque(ink: u32) -> Tint {
    wash(ink, u8::MAX)
}

/// Dark: Delta's dark theme, One Dark pulled darker and cooler.
#[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
const DARK_TONES: Tones = Tones {
    ground: DARK_BG,
    sidebar: Rgb::hex(0x202327),
    elevated: Rgb::hex(0x22252a),
    // Measured from the signed-in app: the header row and the tab bar are one plane between
    // the work and the sidebar (`.research/delta-signed-in-2026-10-12.md` §2.1).
    tab_bar: Rgb::hex(0x1d2023),
    header: Rgb::hex(0x1d2023),
    well: Rgb::hex(0x16191b),
    card_head: Rgb::hex(0x25282e),
    hover: Rgb::hex(0x272a2f),
    selected: Rgb::hex(0x2c2f35),
    stroke: Rgb::hex(0x292c32),
    border: Rgb::hex(0x32363e),
    focus: Rgb::hex(0x47679e),
    selected_border: Rgb::hex(0x293b5b),
    indent_guide: opaque(0x2d3137),
    element: Rgb::hex(0x22252a),
    drop_target: wash(0x838994, 0x80),
    pole: Rgb::hex(0xffffff),
    text: Rgb::hex(0xdce0e5),
    text_secondary: Rgb::hex(0xa9afbc),
    text_muted: Rgb::hex(0x878a98),
    icon: Rgb::hex(0xd3d8de),
    // Lifted from Zed's 0x7e879a so a glyph at rest carries most of its words' tone
    // (`icon_muted_carries_three_quarters_of_its_words`).
    icon_muted: Rgb::hex(0x9aa2b1),
    accent: Rgb::hex(0x74ade8),
    accent_fill: Rgb::hex(0x74ade8),
    // Delta's green word (`.research/delta-re-2026-10-12.md` §2.1).
    success: Rgb::hex(0x7bd979),
    success_fill: Rgb::hex(0xa1c181),
    warn: Rgb::hex(0xdec184),
    warn_fill: Rgb::hex(0xdec184),
    // Delta's `ff928a` 0.04 down in Oklab lightness: at its own a deuteranope saw it 0.031
    // from the green word (`vision`); here 0.067.
    error: Rgb::hex(0xf1857e),
    error_fill: Rgb::hex(0xd07277),
    merged: Rgb::hex(0xb477cf),
    merged_fill: Rgb::hex(0xb477cf),
    backgrounds: [
        wash(0x74ade8, 0x1a),
        wash(0xa1c181, 0x1a),
        wash(0xdec184, 0x1a),
        wash(0xd07277, 0x1a),
    ],
    borders: [Rgb::hex(0x293b5b), Rgb::hex(0x38482f), Rgb::hex(0x5d4c2f), Rgb::hex(0x4c2b2c)],
    vc_added_line: Rgb::hex(0x274736),
    vc_deleted_line: Rgb::hex(0x44292e),
    vc_word_added: wash(0x2ea048, 0x59),
    vc_word_deleted: wash(0x78081b, 0xcc),
    line_number: Rgb::hex(0x4e5a5f),
    line_number_active: Rgb::hex(0xd0d4da),
    identity: identity(0.72, 0.06),
    fill_fg: Rgb::hex(0x0a0b0e),
    accent_ink: Rgb::hex(0x0a0b0e),
};

/// Light: One Light, which Delta keeps but for four values.
#[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
const LIGHT_TONES: Tones = Tones {
    ground: LIGHT_BG,
    sidebar: Rgb::hex(0xebebec),
    elevated: Rgb::hex(0xebebec),
    tab_bar: Rgb::hex(0xf4f4f4),
    header: Rgb::hex(0xf4f4f4),
    well: Rgb::hex(0xf5f5f5),
    card_head: Rgb::hex(0xe6e6e7),
    hover: Rgb::hex(0xdfdfe0),
    selected: Rgb::hex(0xdadbdd),
    stroke: Rgb::hex(0xdfdfe0),
    border: Rgb::hex(0xc9c9ca),
    focus: Rgb::hex(0x7d82e8),
    selected_border: Rgb::hex(0xcbcdf6),
    indent_guide: wash(0x1f1800, 0x21),
    element: Rgb::hex(0xebebec),
    drop_target: wash(0x7e8087, 0x80),
    pole: Rgb::hex(0x000000),
    text: Rgb::hex(0x242529),
    text_secondary: Rgb::hex(0x58585a),
    text_muted: Rgb::hex(0x7e8086),
    icon: Rgb::hex(0x242529),
    icon_muted: Rgb::hex(0x58585a),
    accent: Rgb::hex(0x5c78e2),
    accent_fill: Rgb::hex(0x5c78e2),
    // Delta's green word; the lift to AA on every plane and row darkens it to `007016`.
    success: Rgb::hex(0x008019),
    success_fill: Rgb::hex(0x58884c),
    warn: Rgb::hex(0xa48819),
    warn_fill: Rgb::hex(0x937a16),
    // Delta's red at its hue, taken to Oklab L 0.40 C 0.14: lifted to AA, Delta's `d03a42`
    // landed at `b13138`, which a deuteranope saw 0.048 from the green word and 0.033 from the
    // amber (`vision`); here 0.066 and 0.089.
    error: Rgb::hex(0x831921),
    error_fill: Rgb::hex(0xc75b4c),
    merged: Rgb::hex(0xa349ab),
    merged_fill: Rgb::hex(0xa349ab),
    backgrounds: [opaque(0xeeeefc), opaque(0xdfeadb), opaque(0xfaf2e6), opaque(0xfbdfd9)],
    borders: [Rgb::hex(0xcbcdf6), Rgb::hex(0xc8dcc1), Rgb::hex(0xf4e7d1), Rgb::hex(0xf6c6bd)],
    vc_added_line: Rgb::hex(0xdaf1e2),
    vc_deleted_line: Rgb::hex(0xfdded8),
    // Delta's light pair, opaque (`.research/delta-re-2026-10-12.md` §2.1).
    vc_word_added: opaque(0x85d6a3),
    vc_word_deleted: opaque(0xefa4b2),
    line_number: Rgb::hex(0xb4b4bb),
    line_number_active: Rgb::hex(0x44454b),
    identity: identity(0.60, 0.07),
    fill_fg: Rgb::hex(0x0a0b0e),
    // White reads 4.0:1 on this blue; the near-black 4.9.
    accent_ink: Rgb::hex(0x0a0b0e),
};

/// The least contrast `fg` has on any of `surfaces`.
fn worst(fg: Rgb, surfaces: &[Rgb]) -> f32 {
    surfaces.iter().map(|&bg| fg.contrast(bg)).fold(f32::INFINITY, f32::min)
}

/// The least APCA lightness contrast, as a magnitude, `fg` has on any of `surfaces`.
fn worst_lc(fg: Rgb, surfaces: &[Rgb]) -> f32 {
    surfaces.iter().map(|&bg| fg.apca(bg).abs()).fold(f32::INFINITY, f32::min)
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

/// `fg`, moved toward `pole` only as far as it takes to clear `floor` on every one of
/// `surfaces`, so its hue survives: the pole itself when even that is not enough.
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

impl Surfaces {
    /// The chrome of `variant`.
    ///
    /// The planes, lines, states and marks are the variant's values as they are. Text tones
    /// then move toward white or black until each clears WCAG AA and its APCA floor on every
    /// surface text lands on (each plane, and a hovered or selected row), and each text level
    /// keeps a quarter again the contrast of the one under it. A status word clears the muted
    /// floor; the error's word also reads on its quiet ground, the destructive button's.
    ///
    /// The chrome is light and dark only, with no third look for the system's Increase
    /// Contrast (`docs/decisions/ui.md`, "Light and dark only").
    #[must_use]
    pub fn new(variant: Variant) -> Self {
        let t = match variant {
            Variant::Dark => DARK_TONES,
            Variant::Light => LIGHT_TONES,
        };
        let planes = [t.ground, t.sidebar, t.elevated, t.tab_bar, t.header, t.well, t.card_head];
        let under: Vec<Rgb> = planes.into_iter().chain([t.hover, t.selected]).collect();
        let tier = |fg: Rgb, ratio: f32, lc: f32| lift_to(fg, &under, t.pole, Floor { ratio, lc });
        let text_muted = tier(t.text_muted, AA, MUTED_LC);
        let text_secondary =
            tier(t.text_secondary, AA.max(worst(text_muted, &under) * LEVEL), SECONDARY_LC);
        let text = tier(t.text, AA.max(worst(text_secondary, &under) * LEVEL), 0.0);
        let word = |fg: Rgb| tier(fg, AA, MUTED_LC);
        // The error's word reads on its quiet ground too, the destructive button's, over every
        // plane and row.
        let tinted: Vec<Rgb> = under
            .iter()
            .map(|&plane| t.backgrounds[3].over(plane))
            .chain(under.iter().copied())
            .collect();
        let accent = word(t.accent);
        let [info_background, success_background, warning_background, error_background] =
            t.backgrounds;
        let [info_border, success_border, warning_border, error_border] = t.borders;
        Self {
            ground: t.ground,
            sidebar: t.sidebar,
            elevated: t.elevated,
            tab_bar: t.tab_bar,
            header: t.header,
            well: t.well,
            card_head: t.card_head,
            hover: Tint { ink: t.hover, alpha: u8::MAX },
            selected: Tint { ink: t.selected, alpha: u8::MAX },
            keyed: Tint { ink: t.selected, alpha: u8::MAX },
            pressed: Tint { ink: t.selected, alpha: u8::MAX },
            stroke: Tint { ink: t.stroke, alpha: u8::MAX },
            border: Tint { ink: t.border, alpha: u8::MAX },
            call_border: Tint::of(t.border, alpha::CALL_BORDER),
            focus: lift_to(t.focus, &under, accent, Floor { ratio: NON_TEXT, lc: 0.0 }),
            selected_border: t.selected_border,
            indent_guide: t.indent_guide,
            element: t.element,
            drop_target: t.drop_target,
            text,
            text_secondary,
            text_muted,
            icon: t.icon,
            icon_muted: t.icon_muted,
            accent,
            success: word(t.success),
            warn: word(t.warn),
            error: lift_to(t.error, &tinted, t.pole, Floor { ratio: AA, lc: MUTED_LC }),
            accent_fill: t.accent_fill,
            success_fill: t.success_fill,
            warn_fill: t.warn_fill,
            error_fill: t.error_fill,
            working: accent,
            working_fill: t.accent_fill,
            merged: word(t.merged),
            merged_fill: t.merged_fill,
            info_background,
            info_border,
            success_background,
            success_border,
            warning_background,
            warning_border,
            error_background,
            error_border,
            vc_added_line: t.vc_added_line,
            vc_deleted_line: t.vc_deleted_line,
            vc_word_added: t.vc_word_added,
            vc_word_deleted: t.vc_word_deleted,
            line_number: t.line_number,
            line_number_active: t.line_number_active,
            identity: t.identity.map(Oklch::rgb),
            fill_fg: t.fill_fg,
            accent_ink: t.accent_ink,
            stage: STAGE,
        }
    }
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

/// The finish of what is sunk: a field, a segmented control's track, a meter. Shade is held
/// inside its top edge, where light does not reach the floor of a hollow.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Sunk {
    /// The black shade's opacity inside the top edge.
    pub shade: f32,
    /// A code well's shade inside it, soft and a point down: Delta's `0 1 4` inset.
    pub well: Shadow,
}

/// What floats and what dims the window under a modal, in both variants alike.
///
/// Nothing at rest wears a shadow, a rim or a highlight: a card is its ring on its plane, as
/// Zed's surfaces are. Only what floats has a shadow, Zed's crisp layers in black: menus, popovers,
/// the palette and toasts its elevated surface's two, a dialog its modal surface's four. Both are
/// drawn only by the kit.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Elevation {
    /// The colour of the shadow and of the scrim: black.
    pub shade: Rgb,
    /// How much the scrim under a modal dims the window.
    pub scrim: f32,
    /// The shadow under a menu, a popover, the palette or a toast, tightest first: Zed's
    /// elevated surface.
    pub float: [Shadow; 2],
    /// The shadow under a dialog, tightest first: Zed's modal surface.
    pub dialog: [Shadow; 4],
    /// What is sunk.
    pub sunk: Sunk,
}

impl Elevation {
    /// Dark.
    pub const DARK: Self = Self {
        shade: Rgb::hex(0),
        scrim: alpha::SCRIM,
        float: [Self::layer(1.0, 0.0, 0.06), Self::layer(2.0, 3.0, 0.12)],
        dialog: [
            Self::layer(1.0, 0.0, 0.12),
            Self::layer(2.0, 3.0, 0.12),
            Self::layer(3.0, 6.0, 0.08),
            Self::layer(6.0, 12.0, 0.04),
        ],
        sunk: Sunk { shade: 0.18, well: Self::layer(1.0, 4.0, 0.20) },
    };
    /// Light.
    pub const LIGHT: Self = Self {
        shade: Rgb::hex(0),
        scrim: alpha::SCRIM_LIGHT,
        float: [Self::layer(1.0, 0.0, 0.03), Self::layer(2.0, 3.0, 0.12)],
        dialog: [
            Self::layer(1.0, 0.0, 0.04),
            Self::layer(2.0, 3.0, 0.06),
            Self::layer(3.0, 6.0, 0.06),
            Self::layer(6.0, 12.0, 0.04),
        ],
        sunk: Sunk { shade: 0.05, well: Self::layer(1.0, 4.0, 0.032) },
    };

    /// One of Zed's layers: `y` down, blurred over `blur`, black at `alpha`, with no spread.
    const fn layer(y: f32, blur: f32, alpha: f32) -> Shadow {
        Shadow { y, blur, spread: 0.0, alpha }
    }

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
    /// A tab opening or closing: nearly linear, so the strip's other tabs slide with the slot
    /// as it grows or folds.
    pub const TAB: Self = Self { p1: (0.3333, 0.6667), p2: (0.6667, 1.0) };

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

/// How chrome moves: durations and curves, one set for every animation, Zed's.
///
/// Two speeds, fast 150 ms and slow 300, on an ease-out quint (`docs/decisions/ui.md`, "The design
/// system is Zed's, with Delta's values"). A hover or a press is instant, as Zed's are. Menus,
/// popovers and the palette open on their first frame: latency comes first, and an overlay that
/// takes longer to arrive than a key takes to type reads as waiting, where words an agent is
/// writing are read as they come and lift with its pace. What moves is opacity and a small
/// translate. Under Reduce Motion all of it lands at once (`slopty_kit::kit::motion`), but for what
/// says work is live: the working mark breathes in opacity over [`Self::breath`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Motion {
    /// A hover or a press arriving or leaving: nothing, since Zed's land at once.
    pub feedback: std::time::Duration,
    /// A small fade: a hint, a word, a mark appearing.
    pub fade: std::time::Duration,
    /// A toast, a notice or a card arriving, [`Self::toast_travel`] in from its edge: fast.
    pub toast: std::time::Duration,
    /// Overlays, menus and hints leaving: shorter than their entrance, since what is dismissed
    /// is no longer looked at (Radix, `HeroUI`: 100 ms out against 150 to 200 in).
    pub exit: std::time::Duration,
    /// A selected row's fill moving, a tab resizing or sliding to its new place as tabs are
    /// reordered, a fold opening: fast.
    pub settle: std::time::Duration,
    /// A phone's palette sheet, the iPad's drawer, the composer turning into an approval, a
    /// tab closing: slow.
    pub sheet: std::time::Duration,
    /// A new pane sliding in from the edge it opened at, on [`Self::ease_out`]: slow.
    pub pane: std::time::Duration,
    /// A prompt the person sent rising into its place, [`Self::prompt_rise`] up from below,
    /// on [`Self::ease_out`]: slow.
    pub reveal: std::time::Duration,
    /// One breath of a live mark under Reduce Motion: its opacity rises and falls over this,
    /// with no travel or turn, since a mark that freezes reads as hung: Zed's pulse, 2 s (the
    /// platform keeps its activity indicators alive under Reduce Motion).
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
    /// The curve of everything but a sheet: Zed's ease-out quint, fast out of the gate and a
    /// long soft landing.
    pub ease_out: Curve,
    /// How far a toast travels in, in points.
    pub toast_travel: f32,
    /// How far a sent prompt rises into its place, in points.
    pub prompt_rise: f32,
    /// A sheet's curve: a drawer's, which follows a finger's flick.
    pub drawer: Curve,
}

impl Motion {
    /// The one set.
    pub const DEFAULT: Self = Self {
        feedback: std::time::Duration::ZERO,
        fade: std::time::Duration::from_millis(150),
        toast: std::time::Duration::from_millis(150),
        exit: std::time::Duration::from_millis(100),
        settle: std::time::Duration::from_millis(150),
        sheet: std::time::Duration::from_millis(300),
        pane: std::time::Duration::from_millis(300),
        reveal: std::time::Duration::from_millis(300),
        breath: std::time::Duration::from_millis(2_000),
        blink: std::time::Duration::from_millis(600),
        stream: std::time::Duration::from_millis(400),
        stream_stagger: std::time::Duration::from_millis(10),
        ease_out: Curve { p1: (0.22, 1.0), p2: (0.36, 1.0) },
        toast_travel: 8.0,
        prompt_rise: 10.0,
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
/// Two sets, one per input: a pointer's is Zed's density, a finger needs the 44 pt Apple's HIG
/// asks for. Visual sizes (icons, type) are the same in both; only the hit area and the rows
/// round it grow.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Density {
    /// A one-line row: a worker's header, a palette or menu row, a row of *Needs you*.
    pub row: f32,
    /// A row of two lines: a title over its meta line.
    pub row_two_line: f32,
    /// The navigator's header row, and the top line it shares with the top panes' tab bars:
    /// Zed's title bar's 34 on the Mac.
    pub title: f32,
    /// A pane's tab bar, its header: 34 on the Mac, so the top panes' bars meet the
    /// navigator's header on one line.
    pub header: f32,
    /// A pane's own toolbar, apart from its tab bar: 32 on the Mac.
    pub bar: f32,
    /// A column's foot row (a thread's, a review's): Delta's 36, measured from the signed-in
    /// app (`.research/delta-signed-in-2026-10-12.md` §2.2). The window has no status bar.
    pub foot: f32,
    /// A tab, a cell of its bar: 34 on the Mac, the bar's whole height.
    pub tab: f32,
    /// A navigator or settings-list row: Delta's 28 on the Mac
    /// (`.research/delta-signed-in-2026-10-12.md` §3.5).
    pub item: f32,
    /// A menu's row (a thing's own menu, the bar's menus, the effort menu): Delta's 23.
    pub menu: f32,
    /// A row of a list typed at (the palette, a picker): Delta's palette's 27.
    pub list: f32,
    /// A row of the composer's slash commands or its files: Delta's 24.
    pub slash: f32,
    /// A row of the composer's model picker: Delta's 31.
    pub picker: f32,
    /// An icon button's side: Zed's default button, 22 on the Mac.
    pub button: f32,
    /// A status bar chip's height: 22 on the Mac.
    pub chip: f32,
    /// A decision's head band and the buttons of its foot, what waits on the person in the
    /// thread: Zed's permission rows' 28 on the Mac.
    pub decision: f32,
    /// A text button's, a dropdown's or a field's height, in a page, a dialog or a foot:
    /// Delta's 28 on the Mac, a row's, so a button in a row fills it. Icon buttons stay
    /// [`Self::button`].
    pub control: f32,
    /// The least side of anything tapped or clicked: an icon button, a key cap, a close box.
    pub hit: f32,
    /// A search field heading a list (the settings'): Delta's 32 on the Mac, a step over a
    /// control (`.research/delta-re-2026-10-12.md` §2.5).
    pub search: f32,
    /// How far across a sash's hit area reaches, centred on its 1 pt line: Zed's 4 pt on a
    /// pointer, a finger's half target on touch.
    pub sash: f32,
    /// How much larger a glyph and its slot are than the chrome's ([`Theme::glyph`],
    /// [`Theme::icon`]): none on a pointer; a finger's 4, a glyph 18 in a slot of 20
    /// (`.research/design-polish-2026-10-12.md` §4.12).
    pub glyph_step: f32,
}

impl Density {
    /// A pointer: the Mac.
    pub const COMPACT: Self = Self {
        row: 28.0,
        row_two_line: 44.0,
        title: 34.0,
        header: 34.0,
        bar: 32.0,
        foot: 36.0,
        tab: 34.0,
        item: 28.0,
        menu: 23.0,
        list: 27.0,
        slash: 24.0,
        picker: 31.0,
        button: 22.0,
        chip: 22.0,
        decision: 28.0,
        control: 28.0,
        hit: 22.0,
        search: 32.0,
        sash: 4.0,
        glyph_step: 0.0,
    };
    /// A finger: the iPhone and the iPad.
    pub const TOUCH: Self = Self {
        row: 44.0,
        row_two_line: 56.0,
        title: 44.0,
        header: 44.0,
        bar: 44.0,
        foot: 44.0,
        tab: 44.0,
        item: 44.0,
        menu: 44.0,
        list: 44.0,
        slash: 44.0,
        picker: 44.0,
        button: 44.0,
        chip: 32.0,
        decision: 44.0,
        control: 44.0,
        hit: 44.0,
        search: 44.0,
        sash: 24.0,
        glyph_step: 4.0,
    };
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
    /// ⌥ as Alt: sent with every key, since the encoder is the worker's.
    pub option_as_alt: OptionAsAlt,
    /// Whether typing is kept from other programs on this Mac (secure event input) while the
    /// focused tile takes a password.
    pub secure_entry: bool,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self { copy_on_select: false, option_as_alt: OptionAsAlt::False, secure_entry: true }
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

/// The whole theme.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Theme {
    /// Terminal colours.
    pub terminal: TerminalPalette,
    /// Terminal behaviour.
    pub behaviour: Behaviour,
    /// Chrome colours, the variant's; a `[colors]` background changes the terminal alone.
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

    /// Every icon's slot at this theme's density: [`Typography::icon`], 16 at the default
    /// size, and 20 for a finger ([`Density::glyph_step`]).
    #[must_use]
    pub fn icon(&self) -> f32 {
        self.typography.icon() + self.density.glyph_step
    }

    /// A glyph's drawing inside its slot at this theme's density: [`Typography::glyph`], 14 at
    /// the default size, and 18 for a finger.
    #[must_use]
    pub const fn glyph(&self) -> f32 {
        self.typography.glyph() + self.density.glyph_step
    }

    /// A small glyph's drawing (a chevron, the small cross) at this theme's density:
    /// [`Typography::small`], 12 at the default size, and 14 for a finger.
    #[must_use]
    pub fn glyph_small(&self) -> f32 {
        self.density.glyph_step.mul_add(0.5, self.typography.small())
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
            surfaces: Surfaces::new(variant),
            elevation: Elevation::of(terminal.bg),
            density: Density::default(),
            typography: Typography::default(),
            radii: Radii::default(),
            spacing: Spacing::default(),
        }
    }

    /// The content step of the surface order: what tile headers and bodies sit on, the ground.
    /// A shell's grid is the same surface unless a `[colors]` background repaints the grid alone.
    #[must_use]
    pub const fn content(&self) -> Rgb {
        self.surfaces.ground
    }

    /// Which variant the chrome is: light when its ground reads as light.
    #[must_use]
    pub const fn variant(&self) -> Variant {
        if self.surfaces.ground.is_light() { Variant::Light } else { Variant::Dark }
    }
}

#[cfg(test)]
mod vision;

#[cfg(test)]
mod tests {
    use super::*;

    /// Both appearances.
    const VARIANTS: [Variant; 2] = [Variant::Dark, Variant::Light];

    /// The most chroma a neutral holds: none to speak of.
    const NEUTRAL_CHROMA: f32 = 0.002;

    /// A glyph at rest carries most of its words' tone mass: the glyph's ink density times its
    /// lightness contrast with the plane, against the words' density times theirs, at least
    /// 0.70 on the sidebar and the ground in both appearances. The densities are Octicons' at
    /// 14 and IBM Plex Sans' at 14, measured on our own renders
    /// (`.research/design-polish-2026-10-12.md` §3.1, §3.2). Zed's dark `icon_muted` carried
    /// 0.65, which read as grey specks beside white words.
    #[test]
    fn icon_muted_carries_three_quarters_of_its_words() {
        const GLYPH_DENSITY: f32 = 0.331;
        const WORDS_DENSITY: f32 = 0.287;
        // CIE L*, the lightness the eye reads a tone mass by.
        let lightness = |c: Rgb| {
            let y = c.luminance();
            if y > 0.008_856 { 116.0_f32.mul_add(y.cbrt(), -16.0) } else { 903.3 * y }
        };
        for variant in VARIANTS {
            let s = Theme::new(variant).surfaces;
            for (name, plane) in [("ground", s.ground), ("sidebar", s.sidebar)] {
                let contrast = |ink: Rgb| (lightness(ink) - lightness(plane)).abs();
                let mass =
                    (GLYPH_DENSITY * contrast(s.icon_muted)) / (WORDS_DENSITY * contrast(s.text));
                assert!(mass >= 0.70, "{variant:?} on {name}: {mass:.3} of its words");
            }
        }
    }

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
        assert_eq!(light.surfaces, Surfaces::new(Variant::Light));
        assert_eq!(light.elevation, Elevation::LIGHT);
        assert_eq!(light.typography, Typography::default());
        assert_ne!(light.terminal.palette(0), light.terminal.bg, "ANSI black is visible on white");
    }

    /// The planes, lines and states are Delta's values as its app draws them (the header's and
    /// the tab bar's measured from the signed-in app), and One Light's where Delta keeps them:
    /// nothing is mixed or lifted but the text.
    #[test]
    fn the_planes_lines_and_states_are_deltas() {
        let hex = |s: &Surfaces| {
            [
                s.ground,
                s.sidebar,
                s.elevated,
                s.tab_bar,
                s.header,
                s.well,
                s.card_head,
                s.hover.ink,
                s.selected.ink,
                s.stroke.ink,
                s.border.ink,
                s.element,
            ]
        };
        let dark = [
            0x19_1c1f, 0x20_2327, 0x22_252a, 0x1d_2023, 0x1d_2023, 0x16_191b, 0x25_282e, 0x27_2a2f,
            0x2c_2f35, 0x29_2c32, 0x32_363e, 0x22_252a,
        ];
        let light = [
            0xfa_fafa, 0xeb_ebec, 0xeb_ebec, 0xf4_f4f4, 0xf4_f4f4, 0xf5_f5f5, 0xe6_e6e7, 0xdf_dfe0,
            0xda_dbdd, 0xdf_dfe0, 0xc9_c9ca, 0xeb_ebec,
        ];
        for (variant, want) in [(Variant::Dark, dark), (Variant::Light, light)] {
            let s = Theme::new(variant).surfaces;
            assert_eq!(hex(&s), want.map(Rgb::hex), "{variant:?}");
            for state in [s.hover, s.selected, s.keyed, s.pressed, s.stroke, s.border] {
                assert_eq!(state.alpha, u8::MAX, "{variant:?}: opaque, as Zed's are");
            }
            assert_eq!(
                (s.keyed, s.pressed),
                (s.selected, s.selected),
                "{variant:?}: one selection"
            );
        }
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
        assert_eq!(
            (r.xs, r.sm, r.md, r.lg, r.xl),
            (2.0, 4.0, 6.0, 8.0, 8.0),
            "GPUI's, as Zed spends it"
        );
        assert!(r.lg < r.full, "a capsule past every corner");
        assert!((Radii::nested(r.lg, 0.0, 2.0) - r.md).abs() < f32::EPSILON, "a float's rows nest");
        assert!((Radii::nested(r.lg, 1.0, r.xs) - 5.0).abs() < f32::EPSILON, "inside a border");
        assert!(Radii::nested(r.xs, 1.0, r.md).abs() < f32::EPSILON, "never under zero");

        // Zed's labels, 14, 12 and 10, and its headlines by their roles.
        let mut t = Typography::default();
        let roles = t.roles(false);
        assert_eq!(
            (
                t.caption(),
                t.small(),
                t.ui_size,
                t.icon(),
                roles.panel_title.size,
                roles.page_heading.size,
                roles.first_run.size
            ),
            (10.0, 12.0, 14.0, 16.0, 16.0, 16.0, 23.0),
            "one name per size; a page's heading is a panel's title at the regular weight"
        );
        const { assert!(Typography::STRONG_WEIGHT <= 600.0, "semibold at the most") };
        t.ui_size = 8.0;
        let title = t.roles(false).panel_title.size;
        assert_eq!((t.caption(), t.small(), title), (6.0, 7.0, 10.0), "clamped at the floor");
        let s = Spacing::default();
        assert!(s.xxs < s.xs && s.xs < s.sm && s.sm < s.md && s.md < s.lg && s.lg < s.xl);
        assert!(s.xl < s.xxl && s.xxl < s.xxxl, "the page steps follow");
        assert_eq!((s.inset(), s.inset_trailing()), (8.0, 6.0), "Zed's rows: 8 and 6");
    }

    /// Three planes in dark, the work darkest, the navigator a step up and what floats a step
    /// more (3 to 5 points of `OKLab` lightness over the work); in light the navigator and the
    /// floats share One Light's grey under the white work. The region line is quieter than a
    /// control's on every plane and still seen on each.
    #[test]
    fn the_planes_climb_from_the_work_and_the_lines_are_seen() {
        let dark = Theme::new(Variant::Dark).surfaces;
        let (work, nav, float) =
            (oklab_l(dark.ground), oklab_l(dark.sidebar), oklab_l(dark.elevated));
        assert!(work < nav && nav < float, "dark: {work:.1} < {nav:.1} < {float:.1}");
        assert!((3.0..=5.0).contains(&(float - work)), "+{:.2} L", float - work);
        let light = Theme::new(Variant::Light).surfaces;
        assert_eq!(light.sidebar, light.elevated, "light: One Light's one grey");
        assert!(oklab_l(light.sidebar) < oklab_l(light.ground), "light: under the white work");
        for variant in VARIANTS {
            let s = Theme::new(variant).surfaces;
            for (name, plane) in
                [("ground", s.ground), ("sidebar", s.sidebar), ("elevated", s.elevated)]
            {
                let (quiet, loud) =
                    (s.stroke.over(plane).contrast(plane), s.border.over(plane).contrast(plane));
                assert!(quiet < loud, "{variant:?} on {name}: the region line under a control's");
                assert!(quiet >= 1.09, "{variant:?}: the line shows on {name}: {quiet:.3}");
            }
        }
    }

    /// A hover and a selection show on every plane a list lies on, the selection past the
    /// hover, both one opaque tone whatever the plane.
    #[test]
    fn the_states_show_on_every_plane() {
        for variant in VARIANTS {
            let s = Theme::new(variant).surfaces;
            for (name, plane) in [
                ("ground", s.ground),
                ("sidebar", s.sidebar),
                ("elevated", s.elevated),
                ("tab bar", s.tab_bar),
            ] {
                let off = |state: Tint| state.over(plane).contrast(plane);
                let (hover, selected) = (off(s.hover), off(s.selected));
                assert!(hover >= 1.06, "{variant:?}: the hover is {hover:.3} off the {name}");
                assert!(
                    selected > hover,
                    "{variant:?}: the selection leads the hover on the {name}"
                );
            }
        }
    }

    /// The keyboard's border is Zed's focused blue, 1 pt all round with 2 on a row's right
    /// edge, moved only as far as it takes to stand 3:1 off every plane and row it rings
    /// (WCAG 2.4.13 and 1.4.11).
    #[test]
    fn the_focus_border_is_the_blue_and_seen_everywhere() {
        assert!(
            (stroke::FOCUS - 1.0).abs() < f32::EPSILON
                && (stroke::KEYED_EDGE - 2.0).abs() < f32::EPSILON
        );
        for variant in VARIANTS {
            let s = Theme::new(variant).surfaces;
            let h = s.focus.oklch().h;
            assert!((245.0..=285.0).contains(&h), "{variant:?}: blue, at {h:.1}°");
            for (ground, under) in under_text(&s) {
                let ratio = s.focus.contrast(under);
                assert!(ratio >= NON_TEXT, "{variant:?} on {ground}: {ratio:.2}");
            }
        }
    }

    /// The type roles: Zed's labels and headlines from the 14 pt chrome, the touch ones a step
    /// larger, each following the chrome size, prose its own; a touch theme picks the touch ones.
    /// Every role is the regular weight but a panel's and a page's title.
    #[test]
    fn the_type_roles_are_zeds_labels_and_headlines() {
        let t = Typography::default();
        let at = |r: TypeRole| (r.size, r.line, r.weight);
        let desk = t.roles(false);
        assert_eq!(at(desk.caption), (10.0, 14.0, 400.0));
        assert_eq!(at(desk.metadata), (12.0, 16.0, 400.0));
        assert_eq!(at(desk.chrome), (14.0, 20.0, 400.0));
        assert_eq!(at(desk.row), (12.0, 16.0, 400.0), "the window's rows, Delta's 12");
        assert_eq!(at(desk.action), (14.0, 20.0, 400.0));
        assert_eq!(at(desk.task_title), (14.0, 20.0, 400.0));
        assert_eq!(at(desk.section), (12.0, 16.0, 400.0));
        assert_eq!(at(desk.panel_title), (16.0, 22.0, 600.0));
        assert_eq!(at(desk.code), (15.0, 21.0, 400.0), "Delta's code");
        assert_eq!(at(desk.prose), (15.0, 21.0, 400.0), "Delta's prose");
        assert_eq!(at(desk.composer), (15.0, 21.0, 400.0));
        assert_eq!(at(desk.page_heading), (16.0, 22.0, 400.0), "Delta's settings title");
        assert_eq!(at(desk.first_run), (23.0, 30.0, 400.0));
        let touch = t.roles(true);
        assert_eq!(at(touch.metadata), (13.0, 18.0, 400.0));
        assert_eq!(at(touch.chrome), (17.0, 22.0, 400.0));
        assert_eq!(at(touch.row), (17.0, 22.0, 400.0), "a finger's row keeps the body size");
        assert_eq!(at(desk.notice), (12.0, 19.0, 400.0), "Delta's toast");
        assert_eq!(at(touch.notice), at(touch.row));
        assert_eq!(at(touch.panel_title), (19.0, 24.0, 600.0));
        assert_eq!(at(touch.prose), (17.0, 24.0, 400.0));
        assert_eq!(at(touch.first_run), (28.0, 34.0, 400.0));
        let larger = Typography { ui_size: 16.0, ..Typography::default() }.roles(false);
        assert_eq!(at(larger.chrome), (16.0, 22.0, 400.0), "it follows the chrome size");
        assert_eq!(larger.prose, desk.prose, "prose keeps its own size");
        for roles in [desk, touch] {
            let strong = |r: TypeRole| (r.weight - Typography::STRONG_WEIGHT).abs() < f32::EPSILON;
            let regular = [
                roles.caption,
                roles.metadata,
                roles.chrome,
                roles.row,
                roles.notice,
                roles.action,
                roles.task_title,
                roles.section,
                roles.prose,
                roles.composer,
                roles.page_heading,
                roles.first_run,
            ];
            assert!(
                regular
                    .iter()
                    .all(|r| (r.weight - Typography::REGULAR_WEIGHT).abs() < f32::EPSILON)
            );
            assert!(strong(roles.panel_title), "a panel's title is strong");
        }
        let finger = Theme { density: Density::TOUCH, ..Theme::default() };
        assert_eq!(finger.roles(), touch);
        assert_eq!(Theme::default().roles(), desk);
    }

    /// The planes chrome text sits on: the work, the navigator, what floats, a tab bar, a
    /// column's header, a well and a card's head.
    fn planes(s: &Surfaces) -> [(&'static str, Rgb); 7] {
        [
            ("ground", s.ground),
            ("sidebar", s.sidebar),
            ("elevated", s.elevated),
            ("tab bar", s.tab_bar),
            ("header", s.header),
            ("well", s.well),
            ("card head", s.card_head),
        ]
    }

    /// The grounds chrome text can land on: the planes, and a hovered or selected row.
    fn under_text(s: &Surfaces) -> Vec<(String, Rgb)> {
        planes(s)
            .map(|(name, c)| (name.to_owned(), c))
            .into_iter()
            .chain([("hover".to_owned(), s.hover.ink), ("selected".to_owned(), s.selected.ink)])
            .collect()
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

    /// Every chrome colour drawn as text reads AA on every surface it can land on, in both
    /// appearances, and the three text levels stay apart. Delta's placeholder grey read 3.9 on a
    /// selected row, and One Light's words 2.3 to 3.8 on paper: the lift moves them.
    #[test]
    fn chrome_text_clears_wcag_aa() {
        for variant in VARIANTS {
            let s = Theme::new(variant).surfaces;
            let under = under_text(&s);
            for (ink, fg) in inks(&s) {
                for (surface, bg) in &under {
                    let ratio = fg.contrast(*bg);
                    assert!(
                        ratio >= AA,
                        "{variant:?}: {ink} on {surface} is {ratio:.2}, under {AA}"
                    );
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
                "{variant:?}: secondary {secondary:.2}, muted {muted:.2}"
            );
            assert!(
                text >= secondary * LEVEL,
                "{variant:?}: text {text:.2}, secondary {secondary:.2}"
            );
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
    /// ground they land on, in both appearances.
    #[test]
    fn chrome_text_clears_apca() {
        for variant in VARIANTS {
            let s = Theme::new(variant).surfaces;
            let surfaces: Vec<Rgb> = under_text(&s).into_iter().map(|(_, c)| c).collect();
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
                assert!(lc >= least, "{variant:?}: {ink} Lc {lc:.1}");
            }
        }
    }

    /// The lift is a guard, not a second design: Delta's dark text, its secondary text and its
    /// marks' words keep their tones within a rounding step, as does One Light's text.
    #[test]
    fn deltas_tones_are_lifted_by_a_rounding_step_at_most() {
        let dark = Surfaces::new(Variant::Dark);
        let light = Surfaces::new(Variant::Light);
        for (name, made, designed) in [
            ("dark text", dark.text, DARK_TONES.text),
            ("dark text_secondary", dark.text_secondary, DARK_TONES.text_secondary),
            ("dark accent", dark.accent, DARK_TONES.accent),
            ("dark success", dark.success, DARK_TONES.success),
            ("dark warn", dark.warn, DARK_TONES.warn),
            ("light text", light.text, LIGHT_TONES.text),
        ] {
            let moved = [
                made.r.abs_diff(designed.r),
                made.g.abs_diff(designed.g),
                made.b.abs_diff(designed.b),
            ];
            assert!(moved.iter().all(|&d| d <= 4), "{name}: {designed:?} became {made:?}");
        }
    }

    /// A remote picture's stage is one near-black in both appearances.
    #[test]
    fn the_stage_is_the_same_near_black_in_both_variants() {
        for variant in VARIANTS {
            assert_eq!(Surfaces::new(variant).stage, STAGE, "{variant:?}");
        }
        assert!(STAGE.contrast(Rgb::hex(0)) < 1.1, "near-black");
    }

    /// The chrome is the appearance's: a light terminal background set in the settings repaints
    /// the grid alone, and the chrome, its ground and its variant stay dark.
    #[test]
    fn a_terminal_background_leaves_the_chrome_alone() {
        let dark = Theme::new(Variant::Dark);
        let mut theme = dark.clone();
        theme.terminal.bg = Rgb::hex(0xfd_f6e3);
        assert_eq!(theme.variant(), Variant::Dark, "the variant is the chrome's");
        assert_eq!(theme.surfaces, dark.surfaces);
        assert_eq!(theme.content(), dark.terminal.bg, "the content is the ground");
    }

    /// One hue per meaning across the modes, as words and as marks: the accent blue, done
    /// green, waiting amber, failed red and merged violet, each in its band in both, whatever
    /// lightness each mode needs.
    #[test]
    fn one_hue_per_meaning_across_modes() {
        for variant in VARIANTS {
            let s = Theme::new(variant).surfaces;
            for (name, colour, band) in [
                ("accent", s.accent, 245.0..=275.0),
                ("accent_fill", s.accent_fill, 245.0..=275.0),
                ("working", s.working, 245.0..=275.0),
                ("success", s.success, 125.0..=145.0),
                ("success_fill", s.success_fill, 125.0..=145.0),
                ("warn", s.warn, 80.0..=100.0),
                ("warn_fill", s.warn_fill, 80.0..=100.0),
                ("error", s.error, 15.0..=32.0),
                ("error_fill", s.error_fill, 15.0..=32.0),
                ("merged", s.merged, 310.0..=330.0),
                ("merged_fill", s.merged_fill, 310.0..=330.0),
            ] {
                let h = colour.oklch().h;
                assert!(band.contains(&h), "{variant:?}: {name} {colour:?} at {h:.1}°");
            }
        }
    }

    /// The accent is Zed's blue and green says done alone; the brand's green stays the mark's
    /// (`docs/decisions/brand.md`). The caret is the accent.
    #[test]
    fn the_accent_is_zeds_blue_and_green_is_success() {
        let green = |c: Rgb| c.g > c.r.saturating_add(40) && c.g > c.b.saturating_add(40);
        assert!(green(BRAND), "the brand is green");
        for variant in VARIANTS {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            assert_ne!(s.accent, s.success, "{variant:?}: two meanings, two hues");
            assert_eq!(
                (s.working, s.working_fill),
                (s.accent, s.accent_fill),
                "{variant:?}: info blue"
            );
            let caret = theme.terminal.cursor.oklch().h;
            assert!((245.0..=275.0).contains(&caret), "{variant:?}: the caret at {caret:.1}°");
        }
    }

    /// Each mark reads as a mark: 3:1 and more on the work and the navigator in both modes, and
    /// on every ground text lands on in dark (WCAG's non-text contrast).
    #[test]
    fn a_status_fill_reads_as_a_mark() {
        for variant in VARIANTS {
            let s = Theme::new(variant).surfaces;
            let grounds: Vec<(String, Rgb)> = match variant {
                Variant::Dark => under_text(&s),
                Variant::Light => {
                    vec![("ground".to_owned(), s.ground), ("sidebar".to_owned(), s.sidebar)]
                }
            };
            for (name, fill) in [
                ("accent_fill", s.accent_fill),
                ("success_fill", s.success_fill),
                ("warn_fill", s.warn_fill),
                ("error_fill", s.error_fill),
                ("merged_fill", s.merged_fill),
            ] {
                for (ground, under) in &grounds {
                    let seen = fill.contrast(*under);
                    assert!(seen >= NON_TEXT, "{variant:?}: {name} on {ground} is {seen:.2}");
                }
            }
        }
    }

    /// The identity colours share one lightness in each mode (0.72 on the dark work, 0.60 on
    /// paper), and each sits 20 degrees or more from every state's hue, the sand excepted, and
    /// from each other, so a machine's glyph never reads as a state.
    #[test]
    fn identity_keeps_clear_of_the_states() {
        let apart = |a: f32, b: f32| (a - b).abs().min(360.0 - (a - b).abs());
        for variant in VARIANTS {
            let s = Theme::new(variant).surfaces;
            let states = [
                s.accent,
                s.accent_fill,
                s.success,
                s.success_fill,
                s.warn,
                s.warn_fill,
                s.error,
                s.error_fill,
                s.merged,
                s.merged_fill,
            ];
            for (i, colour) in s.identity.iter().enumerate() {
                let l = colour.oklch().l;
                let at = match variant {
                    Variant::Dark => 0.72,
                    Variant::Light => 0.60,
                };
                assert!((l - at).abs() <= 0.01, "{variant:?}: identity {i} at L {l:.3}");
            }
            let sand = s.identity.len() - 1;
            for (i, colour) in s.identity.iter().enumerate().take(sand) {
                let h = colour.oklch().h;
                for state in states {
                    let off = apart(h, state.oklch().h);
                    assert!(
                        off >= 20.0,
                        "{variant:?}: identity {i} at {h:.1}° is {off:.1}° off {state:?}"
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

    /// Every word on a coloured ground reads: a badge's count AA on every fill, the primary's
    /// words AA on the accent's quiet ground and the destructive button's red word AA on its
    /// own, over every plane,
    /// each status word AA on its quiet ground over the work, and the text AA on a diff's
    /// added and removed bands.
    #[test]
    fn the_words_on_coloured_grounds_read() {
        for variant in VARIANTS {
            let s = Theme::new(variant).surfaces;
            for (fill, on) in [
                ("accent_fill", s.accent_fill, s.accent_ink),
                ("success_fill", s.success_fill, s.fill_fg),
                ("warn_fill", s.warn_fill, s.fill_fg),
                ("error_fill", s.error_fill, s.fill_fg),
            ]
            .map(|(n, f, o)| (n, o.contrast(f)))
            {
                assert!(on >= AA, "{variant:?}: words on {fill} {on:.2}");
            }
            for (plane, under) in planes(&s) {
                let red = s.error.contrast(s.error_background.over(under));
                assert!(red >= AA, "{variant:?}: the destructive word on {plane}: {red:.2}");
                let primary = s.text.contrast(s.info_background.over(under));
                assert!(primary >= AA, "{variant:?}: the primary's words on {plane}: {primary:.2}");
            }
            for (name, word, quiet) in [
                ("info", s.accent, s.info_background),
                ("success", s.success, s.success_background),
                ("warning", s.warn, s.warning_background),
                ("error", s.error, s.error_background),
            ] {
                let ratio = word.contrast(quiet.over(s.ground));
                assert!(ratio >= AA, "{variant:?}: {name} on its ground {ratio:.2}");
            }
            for (name, band) in [("added", s.vc_added_line), ("removed", s.vc_deleted_line)] {
                let ratio = s.text.contrast(band);
                assert!(ratio >= AA, "{variant:?}: text on a {name} line {ratio:.2}");
            }
        }
    }

    /// Only what floats wears a shadow, in black: menus, popovers, the palette and toasts Zed's
    /// two crisp layers, tightest first, and a dialog its modal four, none blurred past 12 pt.
    /// The scrim under a modal is black at 0.40 in dark and 0.24 in light. A finger gets
    /// Apple's 44 pt; a pointer Zed's density: one 34 pt top line (the navigator's header and
    /// the top tab bars, whose tabs fill them), 28 pt rows, 23 pt menu rows, 22 pt buttons and
    /// a 36 pt column foot.
    #[test]
    fn elevation_and_density() {
        let (dark, light) = (Theme::new(Variant::Dark), Theme::new(Variant::Light));
        for theme in [&dark, &light] {
            let e = theme.elevation;
            let v = theme.variant();
            assert_eq!(e.shade, Rgb::hex(0), "{v:?}: black");
            let scrim = match v {
                Variant::Dark => alpha::SCRIM,
                Variant::Light => alpha::SCRIM_LIGHT,
            };
            assert!((e.scrim - scrim).abs() < f32::EPSILON, "{v:?}: the scrim");
            let [near, far] = e.float;
            assert!(near.blur < far.blur && near.y < far.y, "{v:?}: tightest first");
            let deepest = e.dialog.iter().map(|l| l.blur).fold(0.0, f32::max);
            assert!(deepest > far.blur, "{v:?}: a dialog's reaches further");
            assert!(deepest <= 12.0, "{v:?}: crisp, never a soft blur");
            assert!(
                e.float.iter().chain(&e.dialog).all(|l| l.y > 0.0 && l.spread == 0.0),
                "{v:?}: below the top edge, unspread"
            );
            assert!(e.dialog.windows(2).all(|w| w[0].y < w[1].y), "{v:?}: tightest first");
        }
        let (compact, touch) = (Density::COMPACT, Density::TOUCH);
        assert!(touch.hit >= 44.0 && touch.row >= 44.0 && touch.header >= 44.0);
        assert!(touch.control >= 44.0 && touch.bar >= 44.0, "a finger's button and bar");
        assert_eq!(
            (compact.title, compact.header, compact.bar, compact.foot),
            (34.0, 34.0, 32.0, 36.0),
            "one top line, Zed's bars, Delta's 36 pt foot"
        );
        assert_eq!(
            (compact.menu, compact.list, compact.slash, compact.picker),
            (23.0, 27.0, 24.0, 31.0),
            "Delta's menu, palette, slash and model rows"
        );
        assert!(
            [touch.menu, touch.list, touch.slash, touch.picker].iter().all(|h| *h >= 44.0),
            "a finger's menu rows"
        );
        assert_eq!(compact.header, compact.title, "the tab bars meet the navigator's header");
        assert_eq!(
            (compact.row, compact.item, compact.button, compact.chip, compact.control, compact.hit),
            (28.0, 28.0, 22.0, 22.0, 28.0, 22.0),
            "Delta's rows and controls, Zed's icon buttons"
        );
        assert_eq!(compact.row_two_line, 44.0, "Delta's two-line row");
        assert_eq!(compact.search, 32.0, "Delta's search field");
        assert!(touch.search >= 44.0);
        assert_eq!(compact.tab, compact.header, "a tab is a cell of its bar");
        assert_eq!(compact.decision, 28.0, "Zed's permission rows");
        assert!(touch.decision >= 44.0, "a finger's answer");
        assert_eq!(compact.sash, 4.0, "Zed's split hit area");
        assert!(touch.tab >= 36.0 && touch.item >= 44.0 && touch.button >= 44.0);
        assert!(touch.sash >= touch.hit / 2.0, "a finger finds the sash");
        assert!(compact.row < touch.row && compact.row_two_line < touch.row_two_line);
        assert!(compact.row < compact.row_two_line && touch.row < touch.row_two_line);
        assert_eq!(Theme::default().density, compact, "the Mac is the default");
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
    /// the diagonal is that line. The durations are Zed's two speeds, and a hover is instant.
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
        let ms = |d: std::time::Duration| d.as_millis();
        assert_eq!(ms(motion.feedback), 0, "a hover is instant");
        let fast = [motion.fade, motion.toast, motion.settle].map(ms);
        let slow = [motion.sheet, motion.pane, motion.reveal].map(ms);
        assert_eq!((fast, slow), ([150; 3], [300; 3]), "Zed's fast and slow");
        assert_eq!(ms(motion.breath), 2_000, "Zed's pulse");
        assert!((motion.toast_travel - 8.0).abs() < f32::EPSILON, "a toast travels 8 pt");
        assert!((motion.prompt_rise - 10.0).abs() < f32::EPSILON, "a sent prompt rises 10 pt");
        assert!(motion.exit < motion.fade, "what leaves goes quicker than it came");
        assert!(motion.fade < motion.stream, "a stream's lift has room to follow its pace");
        assert!(motion.stream_stagger * 6 < motion.fade, "a chunk lights up as one gesture");
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
    /// capped per hue where sRGB would clip it unevenly, and the greys true neutrals, the dim
    /// slot at 0.54 so it reads AA on the ground.
    const LIGHT_ANSI: [Oklch; 16] = {
        const fn at(l: f32, c: f32, h: f32) -> Oklch {
            Oklch { l, c, h }
        }
        const GREEN: f32 = 148.0;
        [
            at(0.226, 0.0, 0.0),
            at(0.525, 0.190, 20.0),
            at(0.525, 0.150, GREEN),
            at(0.525, 0.108, 82.0),
            at(0.525, 0.170, 255.0),
            at(0.525, 0.190, 318.0),
            at(0.525, 0.090, 206.0),
            at(0.450, 0.0, 0.0),
            at(0.540, 0.0, 0.0),
            at(0.465, 0.180, 20.0),
            at(0.465, 0.135, GREEN),
            at(0.465, 0.096, 82.0),
            at(0.465, 0.153, 255.0),
            at(0.465, 0.190, 318.0),
            at(0.465, 0.080, 206.0),
            at(0.665, 0.0, 0.0),
        ]
    };

    /// The light palette is what its generator makes, and the generator's hues are the dark
    /// palette's: a colour is one hue in both modes (within 10°, green within 20° since it
    /// moves to the brand's), and the greys are true neutrals.
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
            // Green within 20°: light's moves to the brand's 148°, and One Dark's bright green
            // sits at 131°.
            let within = if ix % 8 == 2 { 20.0 } else { 10.0 };
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
