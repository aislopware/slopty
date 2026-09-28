//! Design tokens. Toolkit-agnostic: plain numbers and RGB so `slopty-ui` (GPUI) and any other
//! consumer read the same values.
//!
//! Visual direction: Warp-like. A neutral surface ladder derived from the terminal's
//! background, one accent, hairlines and one elevation for what floats, a 4/8 pt spacing scale,
//! status colour only where it carries meaning, the terminal mono for terminal surfaces and the
//! system sans for chrome. The rulings are in `docs/decisions/ui.md` ("Design tokens" and "The
//! chrome is derived from the content"); chrome draws from these tokens and nothing else.

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

/// The dark terminal background: the content step of the chrome's surface order, which tile
/// headers and bodies share.
#[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
const DARK_BG: Rgb = Rgb::hex(0x16181d);
/// The light terminal background.
#[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
const LIGHT_BG: Rgb = Rgb::hex(0xffffff);

impl TerminalPalette {
    /// The default dark palette.
    #[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
    pub const DARK: Self = Self {
        fg: Rgb::hex(0xe6e6e6),
        bg: DARK_BG,
        cursor: Rgb::hex(0x8ab4f8),
        // A block cursor cuts its cell out of the background, as ghostty draws it.
        cursor_text: DARK_BG,
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
            Rgb::hex(0x7a8393),
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
        bg: LIGHT_BG,
        cursor: Rgb::hex(0x2f6fdb),
        cursor_text: LIGHT_BG,
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
    /// "This one": the selected row, the focused title, the active tab, a button's words, an
    /// approval's statement. It says which without shouting, where the strong weight is kept
    /// for titles.
    pub const MEDIUM_WEIGHT: f32 = 500.0;
    /// Titles: a dialog's or a panel's, the first run's heading, Markdown headings. Chrome has
    /// no bold.
    pub const STRONG_WEIGHT: f32 = 600.0;

    /// The smallest chrome size: HUD readouts, timestamps, chevrons (base − 3).
    #[must_use]
    pub fn caption(&self) -> f32 {
        (self.ui_size - 3.0).max(6.0)
    }

    /// Meta text: a row's second line, a bar's readouts, a status word (base − 2). It sits
    /// between `small()` and `caption()` so a two-line row reads as a title over its facts.
    #[must_use]
    pub fn meta(&self) -> f32 {
        (self.ui_size - 2.0).max(6.0)
    }

    /// Secondary chrome: bar labels, pills, folds, tool summaries, section labels (base − 1).
    #[must_use]
    pub fn small(&self) -> f32 {
        (self.ui_size - 1.0).max(7.0)
    }

    /// Prose read at length: an assistant's answer and the prompt it answers (base + 1).
    #[must_use]
    pub fn prose(&self) -> f32 {
        self.ui_size + 1.0
    }

    /// Titles of panels and dialogs (base + 2).
    #[must_use]
    pub fn title(&self) -> f32 {
        self.ui_size + 2.0
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

    /// The heading of a page that is the whole window, the first run (base + 9). At 22 the
    /// system face switches to its Display cut on its own.
    #[must_use]
    pub fn display(&self) -> f32 {
        self.ui_size + 9.0
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
///
/// Two families: 6 at rest and 12 for what floats. A floating shell is its rows' radius plus
/// the pad round them (6 + 6), so the corners nest.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Radii {
    /// Key caps, chips, inline code.
    pub xs: f32,
    /// Buttons, fields, rows (their hover and selected fills), tabs.
    pub sm: f32,
    /// Framed blocks inside content: a diff, a code block.
    pub md: f32,
    /// Everything that floats: the palette, menus, dialogs, the inbox, a toast, the composer.
    pub lg: f32,
}

impl Default for Radii {
    fn default() -> Self {
        Self { xs: 4.0, sm: 6.0, md: 8.0, lg: 12.0 }
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
}

impl Default for Spacing {
    fn default() -> Self {
        Self { xxs: 2.0, xs: 4.0, sm: 8.0, md: 12.0, lg: 16.0, xl: 24.0 }
    }
}

/// Opacities for tints and washes over a surface: one ladder, used everywhere, so the chrome
/// reads as one surface rather than a collection of one-off transparencies.
pub mod alpha {
    /// An edge that catches the light: the white line along the top of a dark floating
    /// surface.
    pub const EDGE: f32 = 0.06;
    /// Barely there: a selected row, the faint fill of a quiet pill, the hover wash over a
    /// bare button, a wash across the terminal grid (a block separator, the visual bell).
    pub const FAINT: f32 = 0.12;
    /// The window under a light modal or sheet: dimmed about as far as an iOS sheet dims it,
    /// so the sheet leads without the whole screen turning grey.
    pub const DIM: f32 = 0.16;
    /// A tint that has to be seen: answer buttons, a selection.
    pub const TINT: f32 = 0.25;
    /// A tint under the pointer, a scrollbar thumb.
    pub const PRESSED: f32 = 0.4;
    /// The window under a dark modal or sheet.
    pub const SCRIM: f32 = 0.6;
    /// Present but set back: a read row in the inbox.
    pub const STRONG: f32 = 0.7;
    /// A panel laid over content and read through only barely: the stream HUD.
    pub const VEIL: f32 = 0.9;
}

/// WCAG AA for body text: the least contrast chrome text has on any surface it lands on.
const AA: f32 = 4.5;

/// How far apart two text levels stay: each reads at least a quarter again the contrast of the
/// level under it, on the surface where both read worst. Past it, muted and secondary text
/// would be two names for one grey.
const LEVEL: f32 = 1.25;

/// Surface colours for chrome (not the terminal grid), derived from the content they frame.
///
/// The window reads in steps of elevation, lighter as they rise in both variants: the bars on
/// `canvas`, the navigator on `panel`, tile headers and bodies on the content step
/// ([`Theme::content`], the terminal's own background), and what floats over them (the
/// palette, menus, dialogs, popovers, hints) on `elevated`. `raised` and `overlay` are the
/// hover and the selected or pressed fills above whichever of them they sit on.
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
    /// Hovered rows and buttons, key caps, inputs.
    pub raised: Rgb,
    /// Selected and pressed rows, pill fills, the HUD.
    pub overlay: Rgb,
    /// A terminal block's head band, the rows its command was typed on: the content with a few
    /// hundredths of the ink, as Zed's active line is, in both variants. It is its own step so
    /// a band under an unfocused header (on `panel`) does not read as a second header, and so it
    /// shows on white, where `panel` sat 1.5 L* off the content and only its rule was seen.
    pub band: Rgb,
    /// The hairlines that divide regions: between panes, under a bar, round a popover.
    pub border: Rgb,
    /// The quieter hairline inside one region: between rows or groups of a list, under a
    /// tab row, between a panel's sections.
    pub border_subtle: Rgb,
    /// Primary text.
    pub text: Rgb,
    /// Labels, tool summaries, counts.
    pub text_secondary: Rgb,
    /// Hints, timestamps, folds, inactive titles, second lines.
    pub text_muted: Rgb,
    /// Focus ring, active border, links: the accent as text and hairlines. A primary action
    /// is a fill, so it takes [`Self::accent_fill`].
    pub accent: Rgb,
    /// Connected, agent done: as text.
    pub success: Rgb,
    /// Agent waiting, "N need you", muted, reconnecting: as text.
    pub warn: Rgb,
    /// A failed result or command, a pairing error: as text.
    pub error: Rgb,
    /// The accent as a mark or a fill: an unseen dot, a busy bar, a drop wash, the primary
    /// action, a ticked box, a key that is on.
    pub accent_fill: Rgb,
    /// Success as a mark: a dot, a bar, a badge, a wash.
    pub success_fill: Rgb,
    /// Warn as a mark: the attention bar, the bell's badge, a dot, a wash. Amber in both
    /// variants, where the `warn` text tone is a dark ochre in the light one.
    pub warn_fill: Rgb,
    /// Error as a mark: a failed block's wash, a dot, a badge.
    pub error_fill: Rgb,
    /// Text on the success, warn and error fills: a badge's count.
    pub fill_fg: Rgb,
    /// Text on the accent fill: a primary button's label, a tick, a key that is on. White on a
    /// saturated blue in both variants, as a native primary button is.
    pub accent_ink: Rgb,
}

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
    raised: Step,
    overlay: Step,
    band: Step,
    border: Step,
    border_subtle: Step,
    /// Where text moves when it has to read better: the pole away from the surfaces.
    pole: Rgb,
    text: Rgb,
    text_secondary: Rgb,
    text_muted: Rgb,
    accent: Rgb,
    success: Rgb,
    warn: Rgb,
    error: Rgb,
    accent_fill: Rgb,
    success_fill: Rgb,
    warn_fill: Rgb,
    error_fill: Rgb,
    fill_fg: Rgb,
    accent_ink: Rgb,
}

/// Dark: the bars and the navigator sink toward black, everything above the content climbs
/// toward the text.
#[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
const DARK_TONES: Tones = Tones {
    // One notch under the content, not three: at 0.66 the bars framed the window in black.
    // The panel sits two L* under the content, as the light one does, so an unfocused header
    // still reads as one; at 0.12 it was 1.5.
    canvas: Step { toward: Toward::Black, share: 0.28 },
    panel: Step { toward: Toward::Black, share: 0.16 },
    elevated: ink(0.045),
    raised: ink(0.065),
    border_subtle: ink(0.06),
    overlay: ink(0.085),
    band: ink(0.035),
    border: ink(0.105),
    pole: Rgb::hex(0xffffff),
    text: Rgb::hex(0xe6e6e6),
    text_secondary: Rgb::hex(0xb4b9c3),
    text_muted: Rgb::hex(0x8b919c),
    accent: Rgb::hex(0x8ab4f8),
    success: Rgb::hex(0x98c379),
    warn: Rgb::hex(0xe5c07b),
    error: Rgb::hex(0xf06c75),
    accent_fill: Rgb::hex(0x346bf1),
    success_fill: Rgb::hex(0x34c759),
    warn_fill: Rgb::hex(0xf5b83d),
    error_fill: Rgb::hex(0xf0555f),
    fill_fg: Rgb::hex(0x0a0b0e),
    accent_ink: Rgb::hex(0xffffff),
};

/// Light: every step below the content darkens toward the text; what floats goes to white.
#[expect(clippy::unreadable_literal, reason = "colours read as RRGGBB")]
const LIGHT_TONES: Tones = Tones {
    // Near-white chrome and a zinc-200 hairline: at 0.08 and 0.21 the bars were a grey slab
    // under a darker rule than any reference draws.
    canvas: ink(0.04),
    panel: ink(0.025),
    elevated: Step { toward: Toward::White, share: 0.6 },
    raised: ink(0.055),
    border_subtle: ink(0.065),
    overlay: ink(0.085),
    band: ink(0.045),
    border: ink(0.115),
    pole: Rgb::hex(0x000000),
    text: Rgb::hex(0x1d1d1f),
    text_secondary: Rgb::hex(0x4b4f58),
    text_muted: Rgb::hex(0x66666b),
    accent: Rgb::hex(0x2a63c4),
    success: Rgb::hex(0x187633),
    warn: Rgb::hex(0x8b5d00),
    error: Rgb::hex(0xc7212c),
    accent_fill: Rgb::hex(0x1b4ed8),
    success_fill: Rgb::hex(0x2da44e),
    warn_fill: Rgb::hex(0xf0a000),
    error_fill: Rgb::hex(0xef4b52),
    fill_fg: Rgb::hex(0x0a0b0e),
    accent_ink: Rgb::hex(0xffffff),
};

/// The least contrast `fg` has on any of `surfaces`.
fn worst(fg: Rgb, surfaces: &[Rgb]) -> f32 {
    surfaces.iter().map(|&bg| fg.contrast(bg)).fold(f32::INFINITY, f32::min)
}

/// `fg`, moved toward `pole` only as far as it takes to read `least` on every one of
/// `surfaces`, so its hue survives: the pole itself when even that is not enough.
fn lift(fg: Rgb, surfaces: &[Rgb], pole: Rgb, least: f32) -> Rgb {
    if worst(fg, surfaces) >= least {
        return fg;
    }
    if worst(pole, surfaces) <= least {
        return pole;
    }
    // Contrast grows with the mix toward the pole: bisect for the least mix that reads.
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    for _ in 0..14 {
        let mid = f32::midpoint(lo, hi);
        if worst(fg.mix(pole, mid), surfaces) >= least {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    fg.mix(pole, hi)
}

impl Surfaces {
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
        let (canvas, panel, elevated) = (at(t.canvas), at(t.panel), at(t.elevated));
        let (raised, overlay) = (at(t.raised), at(t.overlay));
        let under = [canvas, panel, content, elevated, raised, overlay];
        let text_muted = lift(t.text_muted, &under, t.pole, AA);
        let text_secondary =
            lift(t.text_secondary, &under, t.pole, AA.max(worst(text_muted, &under) * LEVEL));
        let text = lift(t.text, &under, t.pole, AA.max(worst(text_secondary, &under) * LEVEL));
        Self {
            canvas,
            panel,
            elevated,
            raised,
            overlay,
            band: at(t.band),
            border: at(t.border),
            border_subtle: at(t.border_subtle),
            text,
            text_secondary,
            text_muted,
            accent: lift(t.accent, &under, t.pole, AA),
            success: lift(t.success, &under, t.pole, AA),
            warn: lift(t.warn, &under, t.pole, AA),
            error: lift(t.error, &under, t.pole, AA),
            accent_fill: t.accent_fill,
            success_fill: t.success_fill,
            warn_fill: t.warn_fill,
            error_fill: t.error_fill,
            fill_fg: t.fill_fg,
            accent_ink: t.accent_ink,
        }
    }
}

/// One layer of a shadow, in points: black at `alpha`, `y` down, blurred over `blur`.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Shadow {
    /// How far down it falls.
    pub y: f32,
    /// How far it blurs.
    pub blur: f32,
    /// Its opacity.
    pub alpha: f32,
}

/// The one elevation: what lifts a floating surface off the chrome, and what dims the window
/// under a modal.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Elevation {
    /// The colour of the shadow and of the scrim.
    pub shade: Rgb,
    /// How much the scrim under a modal dims the window.
    pub scrim: f32,
    /// The shadow under an `elevated` surface: a tight contact layer, then a soft one.
    pub shadow: [Shadow; 2],
    /// The opacity of a 1 pt white line inside the top edge of an `elevated` surface, in dark
    /// only. A black shadow on a near-black window cannot show where a sheet ends; an edge
    /// that catches the light can.
    pub highlight: Option<f32>,
}

impl Elevation {
    /// Dark: a shadow deep enough to read on near-black, a lit top edge, and a deep scrim.
    pub const DARK: Self = Self {
        shade: Rgb::hex(0),
        scrim: alpha::SCRIM,
        shadow: [
            Shadow { y: 1.0, blur: 2.0, alpha: 0.4 },
            Shadow { y: 12.0, blur: 32.0, alpha: 0.5 },
        ],
        highlight: Some(alpha::EDGE),
    };
    /// Light: a faint shadow and a light scrim, since white panels read on their own. At a
    /// quarter the scrim flattened the screen to a mid grey behind a drawer.
    pub const LIGHT: Self = Self {
        shade: Rgb::hex(0),
        scrim: alpha::DIM,
        shadow: [
            Shadow { y: 1.0, blur: 2.0, alpha: 0.06 },
            Shadow { y: 12.0, blur: 32.0, alpha: 0.10 },
        ],
        highlight: None,
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
/// Every duration stays at or under 160 ms but a sheet's: an overlay that takes longer to arrive
/// than a key takes to type reads as waiting. What moves is opacity and a small translate, never
/// the scale of text. Under Reduce Motion all of it lands at once (`slopty_ui::kit::motion`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Motion {
    /// Hover fills and cursor changes: instant.
    pub hover: std::time::Duration,
    /// Overlays, menus and hints appearing.
    pub fade: std::time::Duration,
    /// A selected row's fill moving, a tab resizing, a fold opening.
    pub settle: std::time::Duration,
    /// A phone's palette sheet, the iPad's drawer, the composer turning into an approval.
    pub sheet: std::time::Duration,
    /// The curve of everything but a sheet: fast out of the gate, a long soft landing.
    pub ease_out: Curve,
    /// A sheet's curve: a drawer's, which follows a finger's flick.
    pub drawer: Curve,
}

impl Motion {
    /// The one set.
    pub const DEFAULT: Self = Self {
        hover: std::time::Duration::ZERO,
        fade: std::time::Duration::from_millis(120),
        settle: std::time::Duration::from_millis(160),
        sheet: std::time::Duration::from_millis(240),
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
    /// A tile's header.
    pub header: f32,
    /// The least side of anything tapped or clicked: an icon button, a key cap, a close box.
    pub hit: f32,
}

impl Density {
    /// A pointer: the Mac.
    pub const COMPACT: Self = Self { row: 28.0, row_two_line: 40.0, header: 28.0, hit: 24.0 };
    /// A finger: the iPhone and the iPad.
    pub const TOUCH: Self = Self { row: 44.0, row_two_line: 56.0, header: 44.0, hit: 44.0 };
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

/// The quality a remote stream is opened at (the scale follows the width the tile is drawn at,
/// not this).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StreamPrefs {
    /// Frames per second.
    pub fps: u16,
    /// The bitrate ceiling, bits per second.
    pub max_bitrate_bps: u32,
    /// A stream opens with its audio silenced on this client (the title-bar pill still
    /// toggles it).
    pub muted: bool,
}

impl Default for StreamPrefs {
    fn default() -> Self {
        Self { fps: 60, max_bitrate_bps: 30_000_000, muted: false }
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

    /// Derive the chrome again from the terminal's background, after something changed it
    /// (a `[colors]` background in the settings).
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

    /// Which variant the colours are: light when the terminal's background reads as light,
    /// whatever the settings made it.
    #[must_use]
    pub const fn variant(&self) -> Variant {
        if self.terminal.bg.is_light() { Variant::Light } else { Variant::Dark }
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
        assert!(r.xs < r.sm && r.sm < r.md && r.md < r.lg);
        assert!((r.sm + r.sm - r.lg).abs() < f32::EPSILON, "a sheet is its rows' radius and pad");

        let mut t = Typography::default();
        assert_eq!(
            (t.caption(), t.meta(), t.small(), t.prose(), t.title(), t.display()),
            (10.0, 11.0, 12.0, 14.0, 15.0, 22.0)
        );
        const {
            assert!(Typography::MEDIUM_WEIGHT > 400.0);
            assert!(Typography::MEDIUM_WEIGHT < Typography::STRONG_WEIGHT);
        };
        t.ui_size = 8.0;
        assert_eq!(
            (t.caption(), t.meta(), t.small(), t.title()),
            (6.0, 6.0, 7.0, 10.0),
            "clamped at the floor"
        );
        let s = Spacing::default();
        assert!(s.xxs < s.xs && s.xs < s.sm && s.sm < s.md && s.md < s.lg && s.lg < s.xl);
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
                let (loud, quiet) = (s.border.contrast(surface), s.border_subtle.contrast(surface));
                assert!(quiet < loud, "{variant:?}: the subtle hairline is quieter on {name}");
            }
            let subtle = s.border_subtle.contrast(content);
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

    /// Terminal backgrounds the chrome is derived for, with a name for the messages: the two
    /// defaults, popular schemes, and the ends of the supported range (a relative luminance of
    /// 0.05 at most in dark, 0.6 at least in light).
    const BACKGROUNDS: [(&str, u32); 13] = [
        ("black", 0x00_0000),
        ("default dark", 0x16_181d),
        ("catppuccin mocha", 0x1e_1e2e),
        ("dracula", 0x28_2a36),
        ("solarized dark", 0x00_2b36),
        ("nord", 0x2e_3440),
        ("dark end", 0x3f_3f3f),
        ("default light", 0xff_ffff),
        ("one light", 0xfa_fafa),
        ("solarized light", 0xfd_f6e3),
        ("gruvbox light", 0xfb_f1c7),
        ("catppuccin latte", 0xef_f1f5),
        ("light end", 0xcc_cccc),
    ];

    /// The surfaces chrome text can land on.
    fn under_text(s: &Surfaces, content: Rgb) -> [(&'static str, Rgb); 6] {
        [
            ("canvas", s.canvas),
            ("panel", s.panel),
            ("content", content),
            ("elevated", s.elevated),
            ("raised", s.raised),
            ("overlay", s.overlay),
        ]
    }

    /// The chrome text colours, with their names.
    fn inks(s: &Surfaces) -> [(&'static str, Rgb); 7] {
        [
            ("text", s.text),
            ("text_secondary", s.text_secondary),
            ("text_muted", s.text_muted),
            ("success", s.success),
            ("warn", s.warn),
            ("error", s.error),
            ("accent", s.accent),
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
    /// Derived at the steps without the lift, dark `text_muted` read 4.48 on `overlay` and
    /// light `accent`, `error`, `success` and `text_muted` about 4.40.
    #[test]
    fn chrome_text_clears_wcag_aa() {
        for (name, bg) in BACKGROUNDS {
            let content = Rgb::hex(bg);
            let s = Surfaces::derive(content);
            let under = under_text(&s, content);
            for (ink, fg) in inks(&s) {
                for (surface, bg) in under {
                    let ratio = fg.contrast(bg);
                    assert!(ratio >= AA, "{name}: {ink} on {surface} is {ratio:.2}, under {AA}");
                }
            }
            let surfaces = under.map(|(_, c)| c);
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

    /// The default themes keep the tones they were designed with: the lift is a guard for
    /// other backgrounds, not a second design. At most one step of rounding moves.
    #[test]
    fn the_default_tones_are_lifted_by_a_rounding_step_at_most() {
        for (content, tones) in
            [(TerminalPalette::DARK.bg, DARK_TONES), (TerminalPalette::LIGHT.bg, LIGHT_TONES)]
        {
            let s = Surfaces::derive(content);
            for (name, derived, designed) in [
                ("text", s.text, tones.text),
                ("text_secondary", s.text_secondary, tones.text_secondary),
                ("text_muted", s.text_muted, tones.text_muted),
                ("accent", s.accent, tones.accent),
                ("success", s.success, tones.success),
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
        let surfaces = under_text(&s, content).map(|(_, c)| c);
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
            let c = l(content);
            assert!(
                l(s.canvas) <= l(s.panel) && l(s.panel) <= c,
                "{name}: bars, navigator, content"
            );
            assert!(l(s.elevated) >= c, "{name}: what floats is not below the content");
            if content.is_light() {
                assert!(l(s.raised) < l(s.canvas), "{name}: hover shows on the bars");
                assert!(l(s.overlay) < l(s.raised), "{name}: selected past hover");
                assert!(l(s.border) < l(s.border_subtle), "{name}: hairlines");
            } else {
                assert!(c < l(s.elevated), "{name}: what floats is above the content");
                assert!(l(s.elevated) < l(s.raised), "{name}: hover shows on what floats");
                assert!(l(s.raised) < l(s.overlay), "{name}: selected past hover");
                assert!(l(s.border_subtle) < l(s.border), "{name}: hairlines");
            }
            // A hairline is never drawn across a selected fill, so `overlay` is left out.
            for (surface, under) in under_text(&s, content).into_iter().take(5) {
                let (loud, quiet) = (s.border.contrast(under), s.border_subtle.contrast(under));
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
                assert!(sat >= 0.5, "{variant:?}: {name} is greyed ({sat:.2})");
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
    #[test]
    fn elevation_and_density() {
        let dark = Theme::new(Variant::Dark);
        let step = dark.surfaces.elevated.contrast(dark.content());
        assert!(step >= 1.05, "dark: elevated is {step:.3} over the content");
        let light = Theme::new(Variant::Light);
        assert_eq!(light.surfaces.elevated, Rgb::hex(0xff_ffff), "light: what floats is white");
        for (theme, least) in [(&dark, 0.4), (&light, 0.1)] {
            let [contact, soft] = theme.elevation.shadow;
            assert!(contact.blur < soft.blur && contact.y < soft.y, "a tight layer, then a soft");
            assert!(soft.alpha >= least, "{:?}: the shadow shows", theme.variant());
        }
        assert_eq!(dark.elevation.highlight, Some(alpha::EDGE), "a dark sheet's lit edge");
        assert_eq!(light.elevation.highlight, None, "white sheets read on their own");
        const { assert!(alpha::EDGE < alpha::FAINT, "the edge is the ladder's quietest step") };
        const { assert!(Elevation::DARK.scrim > Elevation::LIGHT.scrim, "dark dims deeper") };
        const {
            assert!(alpha::FAINT < alpha::DIM && alpha::DIM < alpha::TINT, "one ladder, in order");
            assert!(Elevation::LIGHT.scrim < alpha::TINT, "light dims a sheet's worth, not grey");
        };
        let (compact, touch) = (Density::COMPACT, Density::TOUCH);
        assert!(touch.hit >= 44.0 && touch.row >= 44.0 && touch.header >= 44.0);
        assert!(compact.row < touch.row && compact.row_two_line < touch.row_two_line);
        assert!(compact.row < compact.row_two_line && touch.row < touch.row_two_line);
        assert_eq!(Theme::default().density, compact, "the Mac is the default");
    }

    /// A primary button is white words on a saturated blue in both variants, as a native one
    /// is: dark's `346BF1` and light's `1B4ED8` both carry white at AA. Dark's lighter
    /// `6AA1FF` with dark words read as a disabled or foreign control.
    #[test]
    fn the_primary_fill_carries_white() {
        for variant in [Variant::Dark, Variant::Light] {
            let s = Theme::new(variant).surfaces;
            assert_eq!(s.accent_ink, Rgb::hex(0xff_ffff), "{variant:?}: white words");
            let ink = s.accent_ink.contrast(s.accent_fill);
            assert!(ink >= AA, "{variant:?}: white on {:?} is {ink:.2}", s.accent_fill);
        }
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
        assert!(motion.hover.is_zero(), "hover is instant");
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
