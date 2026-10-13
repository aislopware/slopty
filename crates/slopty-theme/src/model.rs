// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Zeron's theme model.
//!
//! The colour type, the twenty seeds of each appearance and the variant derived from them
//! (`crates/theme/src/lib.rs` and `builtins.rs` of zeron, its own family only).
//! Toolkit-agnostic: `slopty-kit` turns a [`ThemeVariant`] into GPUI colours.
//!
//! The values are zeron's exactly; `.research/zeron-adopt-2026-10-13.md` §1.2 lists what each
//! resolves to, and the tests below hold them there.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Light or dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Appearance {
    /// Light text on a near-black plane.
    #[default]
    Dark,
    /// Dark text on white and grey planes.
    Light,
}

impl Appearance {
    /// Both, light first.
    pub const ALL: [Self; 2] = [Self::Light, Self::Dark];

    /// Whether this is the dark appearance.
    #[must_use]
    pub const fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }

    /// Whether this is the light appearance.
    #[must_use]
    pub const fn is_light(self) -> bool {
        matches!(self, Self::Light)
    }
}

/// How the chrome's planes are painted: over the blurred desktop and scene, or solid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SurfaceTreatment {
    /// Solid planes, a shadow under what floats.
    #[default]
    Opaque,
    /// Translucent planes over a blur.
    Frosted,
}

/// The person's choice of [`SurfaceTreatment`], apart from the theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SurfacePreference {
    /// What the theme recommends.
    #[default]
    ThemeDefault,
    /// Always frosted.
    Frosted,
    /// Always solid.
    Opaque,
}

impl SurfacePreference {
    /// Every choice, in the order a picker lists them.
    pub const ALL: [Self; 3] = [Self::ThemeDefault, Self::Frosted, Self::Opaque];

    /// The treatment this choice gives a theme that recommends `recommended`.
    #[must_use]
    pub const fn resolve(self, recommended: SurfaceTreatment) -> SurfaceTreatment {
        match self {
            Self::ThemeDefault => recommended,
            Self::Frosted => SurfaceTreatment::Frosted,
            Self::Opaque => SurfaceTreatment::Opaque,
        }
    }

    /// The choice's name in a picker.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ThemeDefault => "Theme default",
            Self::Frosted => "Frosted",
            Self::Opaque => "Opaque",
        }
    }
}

/// An sRGB colour with alpha, written `#rrggbb` or `#rrggbbaa`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Alpha, 255 opaque.
    pub a: u8,
}

/// A channel from 0.0–1.0 to a byte.
#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to 0–255")]
const fn byte(value: f32) -> u8 {
    value.clamp(0.0, 255.0).round() as u8
}

impl Color {
    /// Opaque black.
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    /// Opaque white.
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    /// An opaque colour.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// A colour with alpha.
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// From a `0xRRGGBB` literal, opaque.
    #[must_use]
    pub const fn hex(v: u32) -> Self {
        Self::rgb(((v >> 16) & 0xff) as u8, ((v >> 8) & 0xff) as u8, (v & 0xff) as u8)
    }

    /// The same colour at `alpha` (0 to 1).
    #[must_use]
    pub fn with_alpha(self, alpha: f32) -> Self {
        Self { a: byte(alpha.clamp(0.0, 1.0) * 255.0), ..self }
    }

    /// Its alpha, 0 to 1.
    #[must_use]
    pub fn alpha(self) -> f32 {
        f32::from(self.a) / 255.0
    }

    /// `self` composited over an opaque `background`.
    #[must_use]
    pub fn blend_over(self, background: Self) -> Self {
        let alpha = self.alpha();
        let blend = |front: u8, back: u8| {
            byte(f32::from(front).mul_add(alpha, f32::from(back) * (1.0 - alpha)))
        };
        Self::rgb(
            blend(self.r, background.r),
            blend(self.g, background.g),
            blend(self.b, background.b),
        )
    }

    /// `self` moved `amount` (0 to 1) of the way to `other`, alpha included.
    #[must_use]
    pub fn mix(self, other: Self, amount: f32) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| byte((f32::from(b) - f32::from(a)).mul_add(amount, f32::from(a)));
        Self::rgba(
            mix(self.r, other.r),
            mix(self.g, other.g),
            mix(self.b, other.b),
            mix(self.a, other.a),
        )
    }

    /// WCAG contrast ratio of `self`, composited when translucent, against `background`.
    #[must_use]
    pub fn contrast(self, background: Self) -> f32 {
        let foreground = if self.a == 255 { self } else { self.blend_over(background) };
        let (a, b) = (foreground.luminance(), background.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// Black or white, whichever reads better on `self`.
    #[must_use]
    pub fn best_on_color(self) -> Self {
        if Self::WHITE.contrast(self) >= Self::BLACK.contrast(self) {
            Self::WHITE
        } else {
            Self::BLACK
        }
    }

    /// Moved toward black or white until it reaches `minimum` contrast on `background`.
    #[must_use]
    pub fn ensure_contrast(self, background: Self, minimum: f32) -> Self {
        if self.contrast(background) >= minimum {
            return self;
        }
        let target = if Self::BLACK.contrast(background) >= Self::WHITE.contrast(background) {
            Self::BLACK
        } else {
            Self::WHITE
        };
        (1..=20_u8)
            .map(|step| self.mix(target, f32::from(step) / 20.0))
            .find(|candidate| candidate.contrast(background) >= minimum)
            .unwrap_or(target)
    }

    /// WCAG relative luminance, 0 black to 1 white.
    #[must_use]
    pub fn luminance(self) -> f32 {
        let linear = |channel: u8| {
            let value = f32::from(channel) / 255.0;
            if value <= 0.040_45 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
        };
        0.0722_f32
            .mul_add(linear(self.b), 0.2126_f32.mul_add(linear(self.r), 0.7152 * linear(self.g)))
    }
}

impl fmt::Debug for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.a == 255 {
            write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            write!(f, "#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }
}

/// A string that is not a CSS hex colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorParseError;

impl fmt::Display for ColorParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected a CSS hex color (#rgb, #rgba, #rrggbb, or #rrggbbaa)")
    }
}

impl std::error::Error for ColorParseError {}

impl FromStr for Color {
    type Err = ColorParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let digits = value.trim().strip_prefix('#').ok_or(ColorParseError)?;
        if !digits.is_ascii() {
            return Err(ColorParseError);
        }
        let short = |at: usize| {
            let c = digits.get(at..=at).ok_or(ColorParseError)?;
            u8::from_str_radix(&format!("{c}{c}"), 16).ok().ok_or(ColorParseError)
        };
        let pair = |at: usize| {
            let c = digits.get(at..at.saturating_add(2)).ok_or(ColorParseError)?;
            u8::from_str_radix(c, 16).ok().ok_or(ColorParseError)
        };
        match digits.len() {
            3 => Ok(Self::rgb(short(0)?, short(1)?, short(2)?)),
            4 => Ok(Self::rgba(short(0)?, short(1)?, short(2)?, short(3)?)),
            6 => Ok(Self::rgb(pair(0)?, pair(2)?, pair(4)?)),
            8 => Ok(Self::rgba(pair(0)?, pair(2)?, pair(4)?, pair(6)?)),
            _ => Err(ColorParseError),
        }
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?.parse().map_err(serde::de::Error::custom)
    }
}

/// The accent presets a person can pick in place of the theme's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccentPreset {
    /// Zeron's violet, the default.
    #[default]
    Zeron,
    /// Orange.
    Orange,
    /// Amber.
    Amber,
    /// Green.
    Green,
    /// Cyan.
    Cyan,
    /// Blue.
    Blue,
    /// Pink.
    Pink,
}

impl AccentPreset {
    /// Every preset, in the picker's order.
    pub const ALL: [Self; 7] =
        [Self::Zeron, Self::Orange, Self::Amber, Self::Green, Self::Cyan, Self::Blue, Self::Pink];

    /// Its name in a picker.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Zeron => "Violet",
            Self::Orange => "Orange",
            Self::Amber => "Amber",
            Self::Green => "Green",
            Self::Cyan => "Cyan",
            Self::Blue => "Blue",
            Self::Pink => "Pink",
        }
    }

    /// The preset's primary in `appearance`: the 400 step on dark, the 600 or 700 on light.
    #[must_use]
    pub const fn color(self, appearance: Appearance) -> Color {
        let (dark, light) = match self {
            Self::Zeron => (0x8b_7cf6, 0x5b_43e8),
            Self::Orange => (0xfb_923c, 0xc2_410c),
            Self::Amber => (0xfb_bf24, 0xa1_6207),
            Self::Green => (0x4a_de80, 0x15_803d),
            Self::Cyan => (0x22_d3ee, 0x0e_7490),
            Self::Blue => (0x60_a5fa, 0x25_63eb),
            Self::Pink => (0xf4_72b6, 0xbe_185d),
        };
        Color::hex(if appearance.is_dark() { dark } else { light })
    }
}

/// The theme's accent or a preset in its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccentSelection {
    /// The theme's own.
    #[default]
    ThemeDefault,
    /// A preset.
    Preset(AccentPreset),
}

impl AccentSelection {
    /// Its name in a picker.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ThemeDefault => "Theme default",
            Self::Preset(preset) => preset.label(),
        }
    }
}

/// The roles one accent plays, derived from its primary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccentRoles {
    /// The accent, at 3:1 or better on the background.
    pub primary: Color,
    /// The accent as a plate that carries [`Self::on`] at 4.5:1.
    pub strong: Color,
    /// A low wash of it: inline code, tinted identity.
    pub wash: Color,
    /// Text on [`Self::strong`].
    pub on: Color,
    /// Text selection.
    pub selection: Color,
    /// The text caret.
    pub caret: Color,
    /// Working and streaming indicators.
    pub activity: Color,
    /// The pixel mark's three rows: light, mid, deep.
    pub glyph: [Color; 3],
}

impl AccentRoles {
    /// The roles of `primary` on `background` in `appearance`.
    #[must_use]
    pub fn derive(primary: Color, appearance: Appearance, background: Color) -> Self {
        let primary = primary.ensure_contrast(background, 3.0);
        let on = primary.best_on_color();
        let strong =
            if on.contrast(primary) < 4.5 { primary.ensure_contrast(on, 4.5) } else { primary };
        let dark = appearance.is_dark();
        let light = primary
            .mix(if dark { Color::WHITE } else { background }, if dark { 0.28 } else { 0.18 });
        let deep = primary.mix(Color::BLACK, if dark { 0.18 } else { 0.26 });
        Self {
            primary,
            strong,
            wash: primary.with_alpha(if dark { 0.22 } else { 0.12 }),
            on,
            selection: primary.with_alpha(if dark { 0.35 } else { 0.24 }),
            caret: primary,
            activity: primary,
            glyph: [light, primary, deep],
        }
    }
}

/// Every surface, text and state colour of a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeColors {
    /// The content plane: the right pane, the editor.
    pub background: Color,
    /// The window plane: the conversation and the sidebar's base.
    pub shell: Color,
    /// A raised plate.
    pub raised: Color,
    /// A card on the content plane.
    pub card: Color,
    /// A modal dialog.
    pub dialog: Color,
    /// Menus, popovers and the palette.
    pub overlay: Color,
    /// The hover wash.
    pub hover: Color,
    /// The active wash.
    pub active: Color,
    /// A hairline.
    pub border: Color,
    /// A stronger hairline.
    pub border_strong: Color,
    /// Primary text.
    pub text: Color,
    /// Secondary text, 4.5:1 on the background.
    pub text_muted: Color,
    /// Placeholders and hints.
    pub text_faint: Color,
    /// The primary button's plate.
    pub solid: Color,
    /// Text on [`Self::solid`].
    pub on_solid: Color,
    /// Errors and stop.
    pub danger: Color,
    /// Softer error copy.
    pub danger_muted: Color,
    /// Warnings.
    pub warning: Color,
    /// Softer warning copy.
    pub warning_muted: Color,
    /// Success and online.
    pub success: Color,
    /// Softer success copy.
    pub success_muted: Color,
    /// An input's plate.
    pub input: Color,
    /// The terminal's block cursor.
    pub cursor: Color,
    /// An added line.
    pub diff_add: Color,
    /// A deleted line.
    pub diff_delete: Color,
    /// A hunk header's wash.
    pub diff_hunk: Color,
}

/// A variant's terminal colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalColors {
    /// Default background.
    pub background: Color,
    /// Default text.
    pub foreground: Color,
    /// Selection.
    pub selection: Color,
    /// ANSI 0–15.
    pub ansi: [Color; 16],
}

/// One appearance of a theme, every colour resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeVariant {
    /// Its stable id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Light or dark.
    pub appearance: Appearance,
    /// The author's treatment, before the person's [`SurfacePreference`].
    pub recommended_surface_treatment: SurfaceTreatment,
    /// Surfaces, text and states.
    pub colors: ThemeColors,
    /// The theme's accent.
    pub accent: AccentRoles,
    /// Syntax colours by highlight key (`keyword`, `stringSpecial`, …).
    pub syntax: BTreeMap<String, Color>,
    /// The terminal.
    pub terminal: TerminalColors,
}

impl ThemeVariant {
    /// The accent roles for `selection`: the theme's, or a preset's derived on this
    /// background.
    #[must_use]
    pub fn accent_for(&self, selection: AccentSelection) -> AccentRoles {
        match selection {
            AccentSelection::ThemeDefault => self.accent,
            AccentSelection::Preset(preset) => AccentRoles::derive(
                preset.color(self.appearance),
                self.appearance,
                self.colors.background,
            ),
        }
    }

    /// Zeron's own variant for `appearance`.
    #[must_use]
    pub fn zeron(appearance: Appearance) -> Self {
        match appearance {
            Appearance::Dark => zeron_dark(),
            Appearance::Light => zeron_light(),
        }
    }
}

/// The twenty seeds a variant is derived from.
struct Seeds {
    id: &'static str,
    name: &'static str,
    appearance: Appearance,
    treatment: SurfaceTreatment,
    background: u32,
    shell: u32,
    raised: u32,
    card: u32,
    text: u32,
    muted: u32,
    faint: u32,
    accent: u32,
    danger: u32,
    warning: u32,
    success: u32,
    terminal_background: u32,
    ansi: [u32; 16],
    /// Comment, keyword, string, number, type, function, property, variable, punctuation, tag,
    /// attribute, invalid.
    syntax: [u32; 12],
}

fn variant(seed: &Seeds) -> ThemeVariant {
    let background = Color::hex(seed.background);
    let raised = Color::hex(seed.raised);
    let card = Color::hex(seed.card);
    let text = Color::hex(seed.text);
    let danger = Color::hex(seed.danger);
    let warning = Color::hex(seed.warning);
    let success = Color::hex(seed.success);
    let accent = AccentRoles::derive(Color::hex(seed.accent), seed.appearance, background);
    let dark = seed.appearance.is_dark();
    let border_tone = if dark { Color::WHITE } else { Color::BLACK };
    let solid = if dark { Color::rgb(235, 235, 239) } else { Color::rgb(35, 35, 40) };
    let colors = ThemeColors {
        background,
        shell: Color::hex(seed.shell),
        raised,
        card,
        dialog: card.mix(raised, if dark { 0.18 } else { 0.04 }),
        overlay: card.mix(raised, if dark { 0.34 } else { 0.02 }),
        hover: border_tone.with_alpha(if dark { 0.11 } else { 0.06 }),
        active: accent.primary.with_alpha(if dark { 0.18 } else { 0.10 }),
        border: border_tone.with_alpha(if dark { 0.10 } else { 0.12 }),
        border_strong: border_tone.with_alpha(if dark { 0.18 } else { 0.22 }),
        text,
        text_muted: Color::hex(seed.muted).ensure_contrast(background, 4.5),
        text_faint: Color::hex(seed.faint),
        solid,
        on_solid: solid.best_on_color(),
        danger,
        danger_muted: danger.mix(text, 0.28),
        warning,
        warning_muted: warning.mix(text, 0.25),
        success,
        success_muted: success.mix(text, 0.25),
        input: if dark { raised.with_alpha(0.72) } else { card },
        cursor: text.with_alpha(if dark { 0.40 } else { 0.55 }),
        diff_add: success,
        diff_delete: danger,
        diff_hunk: accent.primary.with_alpha(if dark { 0.08 } else { 0.07 }),
    };
    let terminal_background = Color::hex(seed.terminal_background);
    ThemeVariant {
        id: seed.id.to_owned(),
        name: seed.name.to_owned(),
        appearance: seed.appearance,
        recommended_surface_treatment: seed.treatment,
        colors,
        accent,
        syntax: syntax(seed.syntax),
        terminal: TerminalColors {
            background: terminal_background,
            foreground: text.ensure_contrast(terminal_background, 4.5),
            selection: border_tone.with_alpha(if dark { 0.22 } else { 0.16 }),
            ansi: seed.ansi.map(Color::hex),
        },
    }
}

fn syntax(colors: [u32; 12]) -> BTreeMap<String, Color> {
    let [
        comment,
        keyword,
        string,
        number,
        type_name,
        function,
        property,
        variable,
        punctuation,
        tag,
        attribute,
        invalid,
    ] = colors.map(Color::hex);
    [
        ("comment", comment),
        ("keyword", keyword),
        ("string", string),
        ("stringSpecial", attribute),
        ("escape", attribute),
        ("number", number),
        ("boolean", number),
        ("type", type_name),
        ("typeBuiltin", type_name),
        ("constructor", type_name),
        ("function", function),
        ("functionBuiltin", function),
        ("macro", keyword),
        ("property", property),
        ("constant", number),
        ("variable", variable),
        ("variableSpecial", keyword),
        ("parameter", variable),
        ("operator", keyword),
        ("punctuation", punctuation),
        ("tag", tag),
        ("attribute", attribute),
        ("label", function),
        ("embedded", punctuation),
        ("invalid", invalid),
    ]
    .into_iter()
    .map(|(key, color)| (key.to_owned(), color))
    .collect()
}

const ANSI_DARK: [u32; 16] = [
    0x24_2424, 0xf8_7171, 0x4a_de80, 0xfa_cc15, 0x60_a5fa, 0xc0_84fc, 0x22_d3ee, 0xd4_d4d8,
    0x52_525b, 0xfc_a5a5, 0x86_efac, 0xfd_e047, 0x93_c5fd, 0xd8_b4fe, 0x67_e8f9, 0xfa_fafa,
];

const ANSI_LIGHT: [u32; 16] = [
    0x1f_1f1f, 0xdc_2626, 0x16_a34a, 0xb4_5309, 0x25_63eb, 0x93_33ea, 0x0e_7490, 0x3f_3f46,
    0x71_717a, 0xb9_1c1c, 0x15_803d, 0x92_400e, 0x1d_4ed8, 0x7e_22ce, 0x15_5e75, 0x18_181b,
];

fn zeron_dark() -> ThemeVariant {
    variant(&Seeds {
        id: "zeron-dark",
        name: "Zeron Dark",
        appearance: Appearance::Dark,
        treatment: SurfaceTreatment::Frosted,
        background: 0x06_0606,
        shell: 0x0d_0d0d,
        raised: 0x34_3438,
        card: 0x0e_0e0e,
        text: 0xe8_e8ea,
        muted: 0xa9_a9ae,
        faint: 0x85_858a,
        accent: 0x8b_7cf6,
        danger: 0xf8_7171,
        warning: 0xfa_cc15,
        success: 0x34_d399,
        terminal_background: 0x09_0909,
        ansi: ANSI_DARK,
        syntax: [
            0x92_929a, 0x8b_7cf6, 0x34_d399, 0xfa_cc15, 0xc0_84fc, 0x60_a5fa, 0xf4_72b6, 0xe8_e8ea,
            0xa1_a1aa, 0xf4_72b6, 0x22_d3ee, 0xf8_7171,
        ],
    })
}

fn zeron_light() -> ThemeVariant {
    variant(&Seeds {
        id: "zeron-light",
        name: "Zeron Light",
        appearance: Appearance::Light,
        treatment: SurfaceTreatment::Frosted,
        background: 0xff_ffff,
        shell: 0xf3_f3f5,
        raised: 0xed_edf0,
        card: 0xff_ffff,
        text: 0x30_3035,
        muted: 0x62_626a,
        faint: 0x79_7981,
        accent: 0x5b_43e8,
        danger: 0xdc_2626,
        warning: 0xa1_6207,
        success: 0x15_803d,
        terminal_background: 0xfa_fafa,
        ansi: ANSI_LIGHT,
        syntax: [
            0x6b_7280, 0x5b_43e8, 0x15_803d, 0xa1_6207, 0x7e_22ce, 0x25_63eb, 0xbe_185d, 0x30_3035,
            0x52_525b, 0xbe_185d, 0x0e_7490, 0xb9_1c1c,
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(c: Color) -> String {
        c.to_string()
    }

    /// The resolved tokens are zeron's dump, value for value (the blueprint's §1.2).
    #[test]
    fn the_variants_resolve_to_zerons_dump() {
        let dark = ThemeVariant::zeron(Appearance::Dark);
        let c = &dark.colors;
        let got = [
            c.background,
            c.shell,
            c.raised,
            c.card,
            c.dialog,
            c.overlay,
            c.hover,
            c.active,
            c.border,
            c.border_strong,
            c.text,
            c.text_muted,
            c.text_faint,
            c.solid,
            c.on_solid,
            c.danger_muted,
            c.warning_muted,
            c.success_muted,
            c.input,
            c.cursor,
            c.diff_hunk,
        ]
        .map(hex);
        let want = [
            "#060606",
            "#0d0d0d",
            "#343438",
            "#0e0e0e",
            "#151516",
            "#1b1b1c",
            "#ffffff1c",
            "#8b7cf62e",
            "#ffffff1a",
            "#ffffff2e",
            "#e8e8ea",
            "#a9a9ae",
            "#85858a",
            "#ebebef",
            "#000000",
            "#f49293",
            "#f6d34a",
            "#61d8ad",
            "#343438b8",
            "#e8e8ea66",
            "#8b7cf614",
        ];
        assert_eq!(got, want, "dark");
        assert_eq!(hex(dark.accent.wash), "#8b7cf638", "dark wash");
        assert_eq!(hex(dark.accent.selection), "#8b7cf659", "dark selection");
        assert_eq!(dark.accent.glyph.map(hex), ["#aba1f9", "#8b7cf6", "#7266ca"], "dark glyph");
        assert_eq!(hex(dark.terminal.selection), "#ffffff38", "dark terminal selection");

        let light = ThemeVariant::zeron(Appearance::Light);
        let c = &light.colors;
        let got = [
            c.background,
            c.shell,
            c.raised,
            c.card,
            c.dialog,
            c.overlay,
            c.hover,
            c.active,
            c.border,
            c.border_strong,
            c.text,
            c.text_muted,
            c.text_faint,
            c.solid,
            c.on_solid,
            c.danger_muted,
            c.warning_muted,
            c.success_muted,
            c.input,
            c.cursor,
            c.diff_hunk,
        ]
        .map(hex);
        let want = [
            "#ffffff",
            "#f3f3f5",
            "#ededf0",
            "#ffffff",
            "#fefefe",
            "#ffffff",
            "#0000000f",
            "#5b43e81a",
            "#0000001f",
            "#00000038",
            "#303035",
            "#62626a",
            "#797981",
            "#232328",
            "#ffffff",
            "#ac292a",
            "#855613",
            "#1c6c3b",
            "#ffffff",
            "#3030358c",
            "#5b43e812",
        ];
        assert_eq!(got, want, "light");
        assert_eq!(hex(light.accent.wash), "#5b43e81f", "light wash");
        assert_eq!(light.accent.glyph.map(hex), ["#7965ec", "#5b43e8", "#4332ac"], "light glyph");
    }

    #[test]
    fn colors_round_trip_every_css_hex_length() {
        for source in ["#abc", "#abcd", "#102030", "#10203040"] {
            let color: Color = source.parse().expect("parses");
            let json = serde_json::to_string(&color).expect("serializes");
            assert_eq!(serde_json::from_str::<Color>(&json).expect("reads"), color, "{source}");
        }
        assert!("#12".parse::<Color>().is_err(), "too short");
        assert!("#zzzzzz".parse::<Color>().is_err(), "not hex");
    }

    #[test]
    fn every_preset_keeps_its_contrast() {
        for appearance in Appearance::ALL {
            let background = if appearance.is_dark() { Color::rgb(6, 6, 6) } else { Color::WHITE };
            for preset in AccentPreset::ALL {
                let roles = AccentRoles::derive(preset.color(appearance), appearance, background);
                assert!(roles.primary.contrast(background) >= 3.0, "{appearance:?} {preset:?}");
                assert!(roles.on.contrast(roles.strong) >= 4.5, "{appearance:?} {preset:?}");
            }
        }
    }

    #[test]
    fn the_surface_preference_overrides_the_theme_only_when_set() {
        for recommended in [SurfaceTreatment::Frosted, SurfaceTreatment::Opaque] {
            assert_eq!(
                SurfacePreference::ThemeDefault.resolve(recommended),
                recommended,
                "default"
            );
            assert_eq!(
                SurfacePreference::Frosted.resolve(recommended),
                SurfaceTreatment::Frosted,
                "frosted"
            );
            assert_eq!(
                SurfacePreference::Opaque.resolve(recommended),
                SurfaceTreatment::Opaque,
                "opaque"
            );
        }
    }
}
