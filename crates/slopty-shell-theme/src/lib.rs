// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Slopty's source-neutral theme domain model.
//!
//! Runtime code consumes complete [`ThemeVariant`] values.

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]
// The built-in themes are the whole set: there is no theme import or theme library
// (`docs/decisions/ui.md`, "A curated few themes").

/// A built-in colour from a CSS hex literal, parsed when the crate compiles: a literal that is
/// not CSS hex fails the build, so no built-in colour is parsed (or can fail) at run time.
macro_rules! hex {
    ($value:literal) => {
        const {
            match $crate::Color::from_css_hex($value) {
                Some(color) => color,
                None => panic!(concat!($value, " is not a CSS hex colour")),
            }
        }
    };
}

mod builtins;

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::str::FromStr;

pub use builtins::builtin_registry;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Whether a theme is light or dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Appearance {
    /// Light text on a dark ground.
    #[default]
    Dark,
    /// Dark text on a light ground.
    Light,
}

impl Appearance {
    /// Both appearances, light first.
    pub const ALL: [Self; 2] = [Self::Light, Self::Dark];

    /// Whether this is [`Appearance::Dark`].
    pub const fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }
}

/// How a theme's surfaces are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SurfaceTreatment {
    /// Solid fills.
    #[default]
    Opaque,
    /// Translucent fills over the window's blur.
    Frosted,
}

/// The device-local policy applied to a theme variant's recommended surface
/// treatment. This is deliberately independent from theme and accent choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SurfacePreference {
    /// Whatever the theme recommends.
    #[default]
    ThemeDefault,
    /// Always [`SurfaceTreatment::Frosted`].
    Frosted,
    /// Always [`SurfaceTreatment::Opaque`].
    Opaque,
}

impl SurfacePreference {
    /// Every preference, in the order settings lists them.
    pub const ALL: [Self; 3] = [Self::ThemeDefault, Self::Frosted, Self::Opaque];

    /// The treatment to draw with, given the theme's `recommended` one.
    pub const fn resolve(self, recommended: SurfaceTreatment) -> SurfaceTreatment {
        match self {
            Self::ThemeDefault => recommended,
            Self::Frosted => SurfaceTreatment::Frosted,
            Self::Opaque => SurfaceTreatment::Opaque,
        }
    }
}

/// A color serialized as `#rrggbb` or `#rrggbbaa`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    /// Red, 0 to 255.
    pub r: u8,
    /// Green, 0 to 255.
    pub g: u8,
    /// Blue, 0 to 255.
    pub b: u8,
    /// Opacity, 0 (clear) to 255 (opaque).
    pub a: u8,
}

/// A colour channel from 0.0..=255.0, rounded to the nearest step.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped into 0..=255 first; there is no checked f32 → u8"
)]
const fn channel(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

impl Color {
    /// Opaque black.
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    /// Opaque white.
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    /// An opaque colour.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// A colour with its opacity.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// The colour of a CSS hex colour (`#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`), or `None`
    /// when `value` is not one of those.
    pub const fn from_css_hex(value: &str) -> Option<Self> {
        const fn nibble(digit: u8) -> Option<u8> {
            match digit {
                b'0'..=b'9' => Some(digit.wrapping_sub(b'0')),
                b'a'..=b'f' => Some(digit.wrapping_sub(b'a').wrapping_add(10)),
                b'A'..=b'F' => Some(digit.wrapping_sub(b'A').wrapping_add(10)),
                _ => None,
            }
        }
        // One digit doubled (`#abc`) or two (`#aabbcc`).
        const fn short(digit: u8) -> Option<u8> {
            match nibble(digit) {
                Some(n) => Some((n << 4) | n),
                None => None,
            }
        }
        const fn pair(high: u8, low: u8) -> Option<u8> {
            match (nibble(high), nibble(low)) {
                (Some(high), Some(low)) => Some((high << 4) | low),
                _ => None,
            }
        }
        let channels = match *value.as_bytes() {
            [b'#', r, g, b] => (short(r), short(g), short(b), Some(255)),
            [b'#', r, g, b, a] => (short(r), short(g), short(b), short(a)),
            [b'#', r1, r2, g1, g2, b1, b2] => (pair(r1, r2), pair(g1, g2), pair(b1, b2), Some(255)),
            [b'#', r1, r2, g1, g2, b1, b2, a1, a2] => {
                (pair(r1, r2), pair(g1, g2), pair(b1, b2), pair(a1, a2))
            }
            _ => return None,
        };
        match channels {
            (Some(r), Some(g), Some(b), Some(a)) => Some(Self { r, g, b, a }),
            _ => None,
        }
    }

    /// This colour at opacity `alpha` (0.0 to 1.0).
    #[must_use]
    pub fn with_alpha(self, alpha: f32) -> Self {
        Self { a: channel(alpha.clamp(0.0, 1.0) * 255.0), ..self }
    }

    /// This colour composited over an opaque `background`, as an opaque colour.
    #[must_use]
    pub fn blend_over(self, background: Self) -> Self {
        let alpha = f32::from(self.a) / 255.0;
        let blend = |front: u8, back: u8| {
            channel(f32::from(front).mul_add(alpha, f32::from(back) * (1.0 - alpha)))
        };
        Self::rgb(
            blend(self.r, background.r),
            blend(self.g, background.g),
            blend(self.b, background.b),
        )
    }

    /// The colour `amount` (0.0 to 1.0) of the way from this one to `other`, opacity too.
    #[must_use]
    #[expect(
        clippy::suboptimal_flops,
        reason = "unfused: fused, 478 of the 1.8 M byte pairs at the themes' amounts round the \
                  other way, moving derived theme colours by a step"
    )]
    pub fn mix(self, other: Self, amount: f32) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| {
            let (a, b) = (f32::from(a), f32::from(b));
            channel(a + (b - a) * amount)
        };
        Self::rgba(
            mix(self.r, other.r),
            mix(self.g, other.g),
            mix(self.b, other.b),
            mix(self.a, other.a),
        )
    }

    /// The WCAG contrast ratio of this colour over `background`, 1.0 to 21.0.
    pub fn contrast(self, background: Self) -> f32 {
        let foreground = if self.a == 255 { self } else { self.blend_over(background) };
        let (a, b) = (foreground.luminance(), background.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// White or black, whichever reads better on this colour.
    #[must_use]
    pub fn best_on_color(self) -> Self {
        if Self::WHITE.contrast(self) >= Self::BLACK.contrast(self) {
            Self::WHITE
        } else {
            Self::BLACK
        }
    }

    /// Move toward black or white until the requested contrast is met.
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
        for step in 1..=20_u8 {
            let candidate = self.mix(target, f32::from(step) / 20.0);
            if candidate.contrast(background) >= minimum {
                return candidate;
            }
        }
        target
    }

    fn luminance(self) -> f32 {
        let linear = |channel: u8| {
            let value = f32::from(channel) / 255.0;
            if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
        };
        0.2126_f32
            .mul_add(linear(self.r), 0.7152_f32.mul_add(linear(self.g), 0.0722 * linear(self.b)))
    }
}

impl fmt::Debug for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
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

impl FromStr for Color {
    type Err = ColorParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_css_hex(value.trim()).ok_or(ColorParseError)
    }
}

impl Serialize for Color {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?.parse().map_err(serde::de::Error::custom)
    }
}

/// A colour that is not CSS hex.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("expected a CSS hex color (#rgb, #rgba, #rrggbb, or #rrggbbaa)")]
pub struct ColorParseError;

/// An accent the person picks over the theme's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccentPreset {
    /// The house violet.
    #[default]
    Slopty,
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
    /// Every preset, in the order settings lists them.
    pub const ALL: [Self; 7] =
        [Self::Slopty, Self::Orange, Self::Amber, Self::Green, Self::Cyan, Self::Blue, Self::Pink];

    /// The preset's name as settings shows it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Slopty => "Slopty",
            Self::Orange => "Orange",
            Self::Amber => "Amber",
            Self::Green => "Green",
            Self::Cyan => "Cyan",
            Self::Blue => "Blue",
            Self::Pink => "Pink",
        }
    }

    /// The preset's colour under `appearance`, before [`AccentRoles::derive`] fits it to a
    /// background.
    pub const fn color(self, appearance: Appearance) -> Color {
        let (dark, light) = match self {
            Self::Slopty => (hex!("#8b7cf6"), hex!("#5b43e8")),
            Self::Orange => (hex!("#fb923c"), hex!("#c2410c")),
            Self::Amber => (hex!("#fbbf24"), hex!("#a16207")),
            Self::Green => (hex!("#4ade80"), hex!("#15803d")),
            Self::Cyan => (hex!("#22d3ee"), hex!("#0e7490")),
            Self::Blue => (hex!("#60a5fa"), hex!("#2563eb")),
            Self::Pink => (hex!("#f472b6"), hex!("#be185d")),
        };
        if appearance.is_dark() { dark } else { light }
    }
}

/// The accent the person picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccentSelection {
    /// The theme's own accent.
    #[default]
    ThemeDefault,
    /// A preset over the theme's accent.
    Preset(AccentPreset),
}

impl AccentSelection {
    /// The selection's name as settings shows it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::ThemeDefault => "Theme default",
            Self::Preset(preset) => preset.label(),
        }
    }
}

/// The accent in each role it plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccentRoles {
    /// The accent itself, at 3:1 or more over the background.
    pub primary: Color,
    /// The accent as a fill that [`on`](Self::on) reads on at 4.5:1.
    pub strong: Color,
    /// A faint wash of the accent behind a highlighted row.
    pub wash: Color,
    /// Text and glyphs drawn on [`strong`](Self::strong).
    pub on: Color,
    /// Selected text's highlight.
    pub selection: Color,
    /// The text caret.
    pub caret: Color,
    /// What marks activity in progress.
    pub activity: Color,
    /// The accent's light, base and deep tones, for a glyph drawn in three.
    pub glyph: [Color; 3],
}

impl AccentRoles {
    /// The roles of accent `primary` under `appearance`, fitted to `background`.
    pub fn derive(primary: Color, appearance: Appearance, background: Color) -> Self {
        let primary = primary.ensure_contrast(background, 3.0);
        let on = primary.best_on_color();
        let mut strong = primary;
        if on.contrast(strong) < 4.5 {
            strong = strong.ensure_contrast(on, 4.5);
        }
        let light = primary.mix(
            if appearance.is_dark() { Color::WHITE } else { background },
            if appearance.is_dark() { 0.28 } else { 0.18 },
        );
        let deep = primary.mix(Color::BLACK, if appearance.is_dark() { 0.18 } else { 0.26 });
        Self {
            primary,
            strong,
            wash: primary.with_alpha(if appearance.is_dark() { 0.22 } else { 0.12 }),
            on,
            selection: primary.with_alpha(if appearance.is_dark() { 0.35 } else { 0.24 }),
            caret: primary,
            activity: primary,
            glyph: [light, primary, deep],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// The person's theme for each appearance, by variant id.
pub struct ThemeSelection {
    /// The light variant's id.
    pub light: String,
    /// The dark variant's id.
    pub dark: String,
}

impl Default for ThemeSelection {
    fn default() -> Self {
        Self { light: "slopty-light".into(), dark: "slopty-dark".into() }
    }
}

impl ThemeSelection {
    /// The variant id chosen for `appearance`.
    pub fn variant_id(&self, appearance: Appearance) -> &str {
        if appearance.is_dark() { &self.dark } else { &self.light }
    }

    /// Choose variant `id` for `appearance`.
    pub fn set_variant(&mut self, appearance: Appearance, id: impl Into<String>) {
        if appearance.is_dark() {
            self.dark = id.into();
        } else {
            self.light = id.into();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Where a theme comes from.
pub struct ThemeSource {
    /// The upstream theme's format (`native` for Slopty's own).
    pub format: String,
    /// The upstream theme's repository.
    pub url: String,
    /// The upstream revision it was ported from.
    pub revision: String,
    /// The upstream licence.
    pub license: String,
    /// `sha256:` of the resolved variant as checked in, so a curation edit changes it too.
    pub asset_hash: String,
}

/// A theme's interface colours, by role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeColors {
    /// The window's ground.
    pub background: Color,
    /// The chrome around the content.
    pub shell: Color,
    /// A surface raised over the ground: controls, chips.
    pub raised: Color,
    /// A card's fill.
    pub card: Color,
    /// A dialog's fill.
    pub dialog: Color,
    /// A popover's or menu's fill.
    pub overlay: Color,
    /// The wash under a hovered control.
    pub hover: Color,
    /// The wash under a pressed or current control.
    pub active: Color,
    /// Hairlines and dividers.
    pub border: Color,
    /// A border that must read: a focused field, a separator that carries meaning.
    pub border_strong: Color,
    /// Body text.
    pub text: Color,
    /// Secondary text, at 4.5:1 or more over the ground.
    pub text_muted: Color,
    /// Tertiary text: hints, placeholders.
    pub text_faint: Color,
    /// A solid neutral fill: the primary button.
    pub solid: Color,
    /// Text on [`solid`](Self::solid).
    pub on_solid: Color,
    /// Errors and destructive actions.
    pub danger: Color,
    /// [`danger`](Self::danger) toned toward the text, for long runs of it.
    pub danger_muted: Color,
    /// Warnings.
    pub warning: Color,
    /// [`warning`](Self::warning) toned toward the text.
    pub warning_muted: Color,
    /// Success.
    pub success: Color,
    /// [`success`](Self::success) toned toward the text.
    pub success_muted: Color,
    /// A text field's fill.
    pub input: Color,
    /// The text cursor.
    pub cursor: Color,
    /// An added line in a diff.
    pub diff_add: Color,
    /// A deleted line in a diff.
    pub diff_delete: Color,
}

/// A theme's terminal colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalPalette {
    /// The terminal's ground.
    pub background: Color,
    /// The terminal's default text.
    pub foreground: Color,
    /// Selected cells' highlight.
    pub selection: Color,
    /// The 16 ANSI colours: the 8 normal, then the 8 bright.
    pub ansi: [Color; 16],
}

/// One theme in one appearance, complete: everything the interface draws with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeVariant {
    /// The variant's stable id, as settings names it (`slopty-dark`).
    pub id: String,
    /// The family it belongs to.
    pub family_id: String,
    /// Its name as settings shows it.
    pub name: String,
    /// One short line of the theme's voice, under its name in Appearance (Amp's).
    #[serde(default)]
    pub description: String,
    /// Light or dark.
    pub appearance: Appearance,
    /// The author's recommended treatment. Runtime applies the user's
    /// [`SurfacePreference`] before components consume the variant.
    #[serde(alias = "surfaceTreatment")]
    pub recommended_surface_treatment: SurfaceTreatment,
    /// The interface colours.
    pub colors: ThemeColors,
    /// The theme's own accent.
    pub accent: AccentRoles,
    /// Code colours, by highlight kind in camel case (`stringSpecial`).
    pub syntax: BTreeMap<String, Color>,
    /// The terminal colours.
    pub terminal: TerminalPalette,
    /// Where the theme comes from.
    pub source: ThemeSource,
}

impl ThemeVariant {
    /// The accent roles for `selection`: the theme's own, or a preset fitted to this theme.
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// A theme and its variants.
pub struct ThemeFamily {
    /// The family's stable id.
    pub id: String,
    /// Its name as settings shows it.
    pub name: String,
    /// Its variants, one or more per appearance.
    pub variants: Vec<ThemeVariant>,
}

/// The themes on offer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeRegistry {
    /// Every family, in the order settings lists them.
    pub families: Vec<ThemeFamily>,
}

impl ThemeRegistry {
    /// The built-in themes, the whole set.
    pub fn builtin() -> &'static Self {
        builtin_registry()
    }

    /// The variant with id `id`.
    pub fn variant(&self, id: &str) -> Option<&ThemeVariant> {
        self.families.iter().flat_map(|family| &family.variants).find(|variant| variant.id == id)
    }

    /// Every variant of `appearance`.
    pub fn variants_for(&self, appearance: Appearance) -> impl Iterator<Item = &ThemeVariant> {
        self.families
            .iter()
            .flat_map(|family| &family.variants)
            .filter(move |variant| variant.appearance == appearance)
    }

    /// The variant `selection` chooses for `appearance`, or the house's own when this registry
    /// has no variant of that id (a theme no longer built in).
    pub fn resolve(&self, selection: &ThemeSelection, appearance: Appearance) -> &ThemeVariant {
        self.variant(selection.variant_id(appearance))
            .or_else(|| self.variant(builtins::house_id(appearance)))
            .unwrap_or_else(|| builtins::house(appearance))
    }

    /// Every problem with this registry's variants: duplicate ids, contrast under the WCAG
    /// minimums, incomplete provenance.
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        let mut ids = HashSet::new();
        for family in &self.families {
            for variant in &family.variants {
                if variant.id.trim().is_empty() || !ids.insert(&variant.id) {
                    issues.push(ValidationIssue::structural_error(
                        &variant.id,
                        "variant id must be unique",
                    ));
                }
                validate_contrast(
                    &mut issues,
                    variant,
                    "text",
                    variant.colors.text,
                    variant.colors.background,
                    4.5,
                );
                validate_contrast(
                    &mut issues,
                    variant,
                    "muted text",
                    variant.colors.text_muted,
                    variant.colors.background,
                    4.5,
                );
                validate_contrast(
                    &mut issues,
                    variant,
                    "accent",
                    variant.accent.primary,
                    variant.colors.background,
                    3.0,
                );
                validate_contrast(
                    &mut issues,
                    variant,
                    "on-accent",
                    variant.accent.on,
                    variant.accent.strong,
                    4.5,
                );
                validate_contrast(
                    &mut issues,
                    variant,
                    "terminal foreground",
                    variant.terminal.foreground,
                    variant.terminal.background,
                    4.5,
                );
                if variant.source.url.is_empty()
                    || variant.source.revision.is_empty()
                    || variant.source.license.is_empty()
                    || variant.source.asset_hash.is_empty()
                {
                    issues.push(ValidationIssue::structural_error(
                        &variant.id,
                        "source provenance is incomplete",
                    ));
                }
                for (index, color) in variant.terminal.ansi.iter().enumerate() {
                    // Slots 0 and 8 are structural black/dim colors. Every
                    // chromatic normal and bright slot is expected to remain
                    // distinguishable as terminal text.
                    if index % 8 == 0 {
                        continue;
                    }
                    if color.contrast(variant.terminal.background) < 3.0 {
                        issues.push(ValidationIssue::contrast_warning(
                            &variant.id,
                            format!("terminal ANSI slot {index} is below 3:1"),
                        ));
                    }
                }
            }
        }
        issues
    }
}

fn validate_contrast(
    issues: &mut Vec<ValidationIssue>,
    variant: &ThemeVariant,
    role: &str,
    foreground: Color,
    background: Color,
    minimum: f32,
) {
    let actual = foreground.contrast(background);
    if actual < minimum {
        issues.push(ValidationIssue::contrast_error(
            &variant.id,
            format!("{role} contrast is {actual:.2}:1; expected {minimum:.1}:1"),
        ));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// How bad a validation issue is.
pub enum ValidationSeverity {
    /// Worth fixing; the theme works.
    Warning,
    /// The theme falls short.
    Error,
}

/// Whether a validation issue makes a theme structurally unsafe to install or
/// describes a visual quality problem that the compiler/runtime can harden.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValidationCategory {
    /// The theme is malformed: a duplicate id, missing provenance.
    Structural,
    /// A colour pair reads below its contrast minimum.
    #[default]
    Contrast,
}

/// One problem [`ThemeRegistry::validate`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    /// The variant it is about.
    pub variant_id: String,
    /// What kind of problem it is.
    #[serde(default)]
    pub category: ValidationCategory,
    /// How bad it is.
    pub severity: ValidationSeverity,
    /// The problem, in words.
    pub message: String,
}

impl ValidationIssue {
    fn structural_error(variant_id: &str, message: impl Into<String>) -> Self {
        Self {
            variant_id: variant_id.into(),
            category: ValidationCategory::Structural,
            severity: ValidationSeverity::Error,
            message: message.into(),
        }
    }

    fn contrast_error(variant_id: &str, message: impl Into<String>) -> Self {
        Self {
            variant_id: variant_id.into(),
            category: ValidationCategory::Contrast,
            severity: ValidationSeverity::Error,
            message: message.into(),
        }
    }

    fn contrast_warning(variant_id: &str, message: impl Into<String>) -> Self {
        Self {
            variant_id: variant_id.into(),
            category: ValidationCategory::Contrast,
            severity: ValidationSeverity::Warning,
            message: message.into(),
        }
    }

    /// Whether the theme must not be installed: a structural error.
    pub fn is_blocking(&self) -> bool {
        self.category == ValidationCategory::Structural
            && self.severity == ValidationSeverity::Error
    }
}

/// Stable visual QA scenes every bundled variant must be reviewed against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VisualFixture {
    /// The session list.
    Sidebar,
    /// A transcript's prose.
    TranscriptMarkdown,
    /// A transcript's code blocks.
    TranscriptCode,
    /// The composer.
    Composer,
    /// A picker's popover.
    PickerPopover,
    /// The appearance settings.
    AppearanceSettings,
    /// A diff.
    Diff,
    /// A terminal.
    Terminal,
    /// An empty state.
    EmptyState,
    /// A dialog.
    Dialog,
}

impl VisualFixture {
    /// Every scene.
    pub const ALL: [Self; 10] = [
        Self::Sidebar,
        Self::TranscriptMarkdown,
        Self::TranscriptCode,
        Self::Composer,
        Self::PickerPopover,
        Self::AppearanceSettings,
        Self::Diff,
        Self::Terminal,
        Self::EmptyState,
        Self::Dialog,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_round_trip_all_supported_css_hex_lengths() {
        for source in ["#abc", "#abcd", "#102030", "#10203040"] {
            let color: Color = source.parse().unwrap();
            let json = serde_json::to_string(&color).unwrap();
            assert_eq!(serde_json::from_str::<Color>(&json).unwrap(), color);
        }
    }

    #[test]
    fn accent_overlay_meets_interaction_contrast() {
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
    fn surface_preference_resolves_independently_from_theme_recommendation() {
        assert_eq!(
            SurfacePreference::ThemeDefault.resolve(SurfaceTreatment::Frosted),
            SurfaceTreatment::Frosted
        );
        assert_eq!(
            SurfacePreference::ThemeDefault.resolve(SurfaceTreatment::Opaque),
            SurfaceTreatment::Opaque
        );
        for recommended in [SurfaceTreatment::Frosted, SurfaceTreatment::Opaque] {
            assert_eq!(SurfacePreference::Frosted.resolve(recommended), SurfaceTreatment::Frosted);
            assert_eq!(SurfacePreference::Opaque.resolve(recommended), SurfaceTreatment::Opaque);
        }
    }

    #[test]
    fn builtins_have_complete_provenance_and_no_validation_errors() {
        let registry = ThemeRegistry::builtin();
        assert_eq!(registry.families.len(), 6);
        assert!(registry.variant("slopty-light").is_some());
        assert!(registry.variant("slopty-dark").is_some());
        let errors: Vec<_> = registry
            .validate()
            .into_iter()
            .filter(|issue| issue.severity == ValidationSeverity::Error)
            .collect();
        assert!(errors.is_empty(), "{errors:#?}");
    }

    #[test]
    fn visual_fixture_matrix_covers_every_variant_and_scene() {
        let registry = ThemeRegistry::builtin();
        let variants = registry.families.iter().map(|family| family.variants.len()).sum::<usize>();
        assert_eq!(variants, 9);
        assert_eq!(variants * VisualFixture::ALL.len(), 90);
    }

    // Each curated theme carries one short line in sentence case, and both appearances
    // keep a high-contrast choice and a choice past the house pair.
    #[test]
    fn every_builtin_says_one_short_line() {
        let registry = ThemeRegistry::builtin();
        for variant in registry.families.iter().flat_map(|family| &family.variants) {
            let line = &variant.description;
            assert!(!line.is_empty() && line.len() <= 60, "{}: {line:?}", variant.id);
            assert!(line.starts_with(char::is_uppercase) && line.ends_with('.'), "{line:?}");
            assert!(!line.contains('\n'), "one line: {line:?}");
        }
        for appearance in Appearance::ALL {
            assert!(registry.variants_for(appearance).count() >= 3, "{appearance:?}");
        }
        assert!(registry.variant("high-contrast-dark").is_some());
        assert!(registry.variant("high-contrast-light").is_some());
        let black = registry.variant("slopty-black").unwrap();
        assert_eq!(black.colors.background, Color::rgb(0, 0, 0), "true black");
    }

    // A theme that is not built in resolves to the house's own, so a settings file naming one still
    // opens.
    #[test]
    fn a_theme_no_longer_built_in_resolves_to_the_houses() {
        let registry = ThemeRegistry::builtin();
        let gone = ThemeSelection { light: "gruvbox-light".into(), dark: "nord".into() };
        assert_eq!(registry.resolve(&gone, Appearance::Light).id, "slopty-light");
        assert_eq!(registry.resolve(&gone, Appearance::Dark).id, "slopty-dark");
    }
}
