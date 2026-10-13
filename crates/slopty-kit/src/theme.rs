// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! The app theme: zeron's two appearances as GPUI colours, one token set.
//!
//! The values come from [`slopty_theme::model`], zeron's seeds and their derivation; this module
//! turns a [`ThemeVariant`] into [`Hsla`] and adds the material functions (glass, frost, the
//! popup tiers) and the context-free paint helpers ([`ink`], [`hairline`], [`wash`]).
//! **Numbers drive layout, colours are paint**: the measures live in [`slopty_theme::layout`].
//!
//! # Light is designed, not inverted
//!
//! In dark the content plane is the darkest and raised planes get lighter. In light the content
//! plane is white and the shell goes grey: chrome recedes by getting darker. Raised plates lift
//! with white, a border and a shadow; fill alphas carry over unchanged ([`INK_FILL_SCALE`]) and
//! only hairlines scale ([`INK_HAIRLINE_SCALE`]); accents drop from the 400 to the 600 step.
//!
//! Installed as a GPUI [`Global`] at boot; read with [`Theme::of`].

use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use gpui::{App, BoxShadow, Global, Hsla, SharedString, hsla, point, px};
pub use slopty_theme::model::{
    AccentPreset, AccentSelection, Appearance, Color as ModelColor, SurfacePreference,
    SurfaceTreatment, ThemeVariant,
};

use crate::syntax::HighlightKind;

/// The animated pixel mark's three rows, in the accent's family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphPalette {
    /// The top row.
    pub light: Hsla,
    /// The middle row.
    pub mid: Hsla,
    /// The bottom row.
    pub deep: Hsla,
}

impl GlyphPalette {
    /// Top to bottom.
    #[must_use]
    pub const fn rows(self) -> [Hsla; 3] {
        [self.light, self.mid, self.deep]
    }
}

/// The appearance a [`gpui::WindowAppearance`] paints in.
#[must_use]
pub const fn appearance_of_window(appearance: gpui::WindowAppearance) -> Appearance {
    match appearance {
        gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight => Appearance::Light,
        gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark => Appearance::Dark,
    }
}

/// The installed theme's appearance, mirrored for the paint helpers that have no `cx`.
///
/// Appearance is one setting for every window, so a single mirror is sound;
/// [`Theme::install`] is the only writer outside tests.
static CURRENT_APPEARANCE: AtomicU8 = AtomicU8::new(0);

/// Bumped whenever the resolved style changes, so caches that bake an [`Hsla`] (the Markdown
/// renderer's runs) drop what they hold.
static STYLE_GENERATION: AtomicU32 = AtomicU32::new(0);

/// The appearance the context-free paint helpers paint for.
#[must_use]
pub fn current_appearance() -> Appearance {
    match CURRENT_APPEARANCE.load(Ordering::Relaxed) {
        1 => Appearance::Light,
        _ => Appearance::Dark,
    }
}

/// The current resolved style's id (palette and interface type).
#[must_use]
pub fn style_generation() -> u32 {
    STYLE_GENERATION.load(Ordering::Relaxed)
}

/// Drop every cache that baked a resolved style.
pub fn bump_style_generation() {
    STYLE_GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// A model colour as GPUI's.
#[must_use]
pub fn model_color(color: ModelColor) -> Hsla {
    let channel = |c: u8| f32::from(c) / 255.0;
    let (h, s, l) = rgb_to_hsl(channel(color.r), channel(color.g), channel(color.b));
    hsla(h, s, l, channel(color.a))
}

/// Serialises the tests that flip the process-wide appearance.
#[cfg(test)]
pub(crate) fn lock_appearance() -> parking_lot::MutexGuard<'static, ()> {
    static APPEARANCE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    APPEARANCE_LOCK.lock()
}

/// Point the context-free paint helpers at `appearance`.
pub fn set_current_appearance(appearance: Appearance) {
    let encoded = match appearance {
        Appearance::Dark => 0,
        Appearance::Light => 1,
    };
    if CURRENT_APPEARANCE.swap(encoded, Ordering::Relaxed) != encoded {
        bump_style_generation();
    }
}

/// Light's multiplier for fills: 1, the same alpha in both appearances, only the tone flips.
pub const INK_FILL_SCALE: f32 = 1.0;

/// Light's multiplier for hairlines: a 1 px edge needs more ink on a bright field.
pub const INK_HAIRLINE_SCALE: f32 = 1.35;

/// Syntax colours for code, paint only.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(missing_docs, reason = "one field per highlight kind, named for it")]
pub struct SyntaxPalette {
    pub comment: Hsla,
    pub keyword: Hsla,
    pub string: Hsla,
    pub string_special: Hsla,
    pub escape: Hsla,
    pub number: Hsla,
    pub boolean: Hsla,
    pub type_name: Hsla,
    pub type_builtin: Hsla,
    pub constructor: Hsla,
    pub function: Hsla,
    pub function_builtin: Hsla,
    pub macro_name: Hsla,
    pub property: Hsla,
    pub constant: Hsla,
    pub variable: Hsla,
    pub variable_special: Hsla,
    pub parameter: Hsla,
    pub operator: Hsla,
    pub punctuation: Hsla,
    pub tag: Hsla,
    pub attribute: Hsla,
    pub label: Hsla,
    pub markup_heading: Hsla,
    pub markup_raw: Hsla,
    pub markup_link: Hsla,
    pub markup_reference: Hsla,
    pub markup_emphasis: Hsla,
    pub markup_strong: Hsla,
    pub invalid: Hsla,
}

impl SyntaxPalette {
    /// The colour of `kind`.
    #[must_use]
    pub const fn color(&self, kind: HighlightKind) -> Hsla {
        match kind {
            HighlightKind::Comment => self.comment,
            HighlightKind::Keyword => self.keyword,
            HighlightKind::String => self.string,
            HighlightKind::StringSpecial => self.string_special,
            HighlightKind::Escape => self.escape,
            HighlightKind::Number => self.number,
            HighlightKind::Boolean => self.boolean,
            HighlightKind::Type => self.type_name,
            HighlightKind::TypeBuiltin => self.type_builtin,
            HighlightKind::Constructor => self.constructor,
            HighlightKind::Function => self.function,
            HighlightKind::FunctionBuiltin => self.function_builtin,
            HighlightKind::Macro => self.macro_name,
            HighlightKind::Property => self.property,
            HighlightKind::Constant => self.constant,
            HighlightKind::Variable => self.variable,
            HighlightKind::VariableSpecial => self.variable_special,
            HighlightKind::Parameter => self.parameter,
            HighlightKind::Operator => self.operator,
            HighlightKind::Punctuation | HighlightKind::Embedded => self.punctuation,
            HighlightKind::Tag => self.tag,
            HighlightKind::Attribute => self.attribute,
            HighlightKind::Label => self.label,
            HighlightKind::MarkupHeading => self.markup_heading,
            HighlightKind::MarkupRaw => self.markup_raw,
            HighlightKind::MarkupLink => self.markup_link,
            HighlightKind::MarkupReference => self.markup_reference,
            HighlightKind::MarkupEmphasis => self.markup_emphasis,
            HighlightKind::MarkupStrong => self.markup_strong,
            HighlightKind::Invalid => self.invalid,
        }
    }

    fn from_variant(variant: &ThemeVariant, fallback: &Self) -> Self {
        let color = |key: &str, fallback: Hsla| {
            variant.syntax.get(key).copied().map_or(fallback, model_color)
        };
        Self {
            comment: color("comment", fallback.comment),
            keyword: color("keyword", fallback.keyword),
            string: color("string", fallback.string),
            string_special: color("stringSpecial", fallback.string_special),
            escape: color("escape", fallback.escape),
            number: color("number", fallback.number),
            boolean: color("boolean", fallback.boolean),
            type_name: color("type", fallback.type_name),
            type_builtin: color("typeBuiltin", fallback.type_builtin),
            constructor: color("constructor", fallback.constructor),
            function: color("function", fallback.function),
            function_builtin: color("functionBuiltin", fallback.function_builtin),
            macro_name: color("macro", fallback.macro_name),
            property: color("property", fallback.property),
            constant: color("constant", fallback.constant),
            variable: color("variable", fallback.variable),
            variable_special: color("variableSpecial", fallback.variable_special),
            parameter: color("parameter", fallback.parameter),
            operator: color("operator", fallback.operator),
            punctuation: color("punctuation", fallback.punctuation),
            tag: color("tag", fallback.tag),
            attribute: color("attribute", fallback.attribute),
            label: color("label", fallback.label),
            markup_heading: color("markupHeading", fallback.markup_heading),
            markup_raw: color("markupRaw", fallback.markup_raw),
            markup_link: color("markupLink", fallback.markup_link),
            markup_reference: color("markupReference", fallback.markup_reference),
            markup_emphasis: color("markupEmphasis", fallback.markup_emphasis),
            markup_strong: color("markupStrong", fallback.markup_strong),
            invalid: color("invalid", fallback.invalid),
        }
    }

    /// The graph-toned palette the variant's keys are laid over; only the `markup*` keys survive
    /// a variant, which carries no colour for them.
    fn for_appearance(appearance: Appearance) -> Self {
        let (text, comment, danger, [indigo, pink, emerald, amber]) = match appearance {
            Appearance::Dark => (
                neutral(0.922),
                neutral(0.60),
                oklch(0.704, 0.191, 22.216),
                [
                    oklch(0.673, 0.182, 276.935),
                    oklch(0.718, 0.202, 349.761),
                    oklch(0.765, 0.177, 163.223),
                    oklch(0.828, 0.189, 84.429),
                ],
            ),
            Appearance::Light => (
                neutral(0.25),
                neutral(0.48),
                oklch(0.505, 0.213, 27.518),
                [
                    oklch(0.47, 0.20, 276.966),
                    oklch(0.47, 0.17, 0.584),
                    oklch(0.46, 0.11, 163.225),
                    oklch(0.47, 0.12, 48.998),
                ],
            ),
        };
        let [indigo, pink, emerald, amber, red] =
            [indigo, pink, emerald, amber, danger].map(git_graph_tone);
        Self {
            comment,
            keyword: indigo,
            string: emerald,
            string_special: pink,
            escape: pink,
            number: amber,
            boolean: amber,
            type_name: amber,
            type_builtin: emerald,
            constructor: amber,
            function: indigo,
            function_builtin: pink,
            macro_name: pink,
            property: amber,
            constant: emerald,
            variable: text,
            variable_special: pink,
            parameter: text,
            operator: text,
            punctuation: text,
            tag: pink,
            attribute: amber,
            label: amber,
            markup_heading: indigo,
            markup_raw: emerald,
            markup_link: pink,
            markup_reference: amber,
            markup_emphasis: pink,
            markup_strong: indigo,
            invalid: red,
        }
    }
}

/// The git graph's lanes, and syntax with them, run at 72% saturation.
const fn git_graph_tone(mut color: Hsla) -> Hsla {
    color.s *= 0.72;
    color
}

/// The theme's terminal colours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalColors {
    /// Default background.
    pub background: Hsla,
    /// Default text.
    pub foreground: Hsla,
    /// Selection.
    pub selection: Hsla,
    /// ANSI 0–15.
    pub ansi: [Hsla; 16],
}

impl TerminalColors {
    fn from_variant(variant: &ThemeVariant) -> Self {
        Self {
            background: model_color(variant.terminal.background),
            foreground: model_color(variant.terminal.foreground),
            selection: model_color(variant.terminal.selection),
            ansi: variant.terminal.ansi.map(model_color),
        }
    }
}

/// The families and sizes a theme is installed with; [`crate::typography`] owns the choice.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeFonts {
    /// The interface family.
    pub sans: SharedString,
    /// Code, diffs and file editors.
    pub mono: SharedString,
    /// Terminal output.
    pub terminal: SharedString,
    /// Code's pixel size.
    pub code_size: f32,
    /// The terminal's pixel size.
    pub terminal_size: f32,
}

impl Default for ThemeFonts {
    fn default() -> Self {
        Self {
            sans: crate::typography::GEIST.into(),
            mono: crate::typography::GEIST_MONO.into(),
            terminal: crate::typography::GEIST_MONO.into(),
            code_size: crate::typography::CODE_FONT_SIZE_DEFAULT,
            terminal_size: crate::typography::TERMINAL_FONT_SIZE_DEFAULT,
        }
    }
}

/// The app theme: every token of one appearance.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Which appearance these tokens were built for.
    pub appearance: Appearance,
    /// Whether the theme or a preset owns the accent.
    pub accent_selection: AccentSelection,
    /// The person's material choice.
    pub surface_preference: SurfacePreference,
    /// The material after that choice: frosted or opaque.
    pub surface_treatment: SurfaceTreatment,

    /// The content plane: right pane, editor. `#060606` dark, white light.
    pub bg: Hsla,
    /// The window plane: shell, conversation, sidebar base.
    pub surface: Hsla,
    /// Raised opaque pills and chips.
    pub surface_raised: Hsla,
    /// A card resting on the content plane.
    pub surface_card: Hsla,
    /// A modal dialog over a [`Theme::scrim`].
    pub surface_dialog: Hsla,
    /// Popovers, menus, the palette: the highest plane.
    pub surface_overlay: Hsla,
    /// Hover wash for rows and buttons.
    pub element_hover: Hsla,
    /// Active and selected wash.
    pub element_active: Hsla,
    /// Hairline.
    pub border: Hsla,
    /// A stronger hairline for focused or raised edges.
    pub border_strong: Hsla,

    /// Primary text.
    pub text: Hsla,
    /// Muted text: timestamps, secondary labels.
    pub text_muted: Hsla,
    /// Faint text: placeholders, disabled.
    pub text_faint: Hsla,
    /// The diff path's tone, one notch below muted.
    pub text_dim: Hsla,

    /// The primary button's plate: near-white on dark, near-black on light.
    pub solid: Hsla,
    /// Text on [`Self::solid`].
    pub on_solid: Hsla,

    /// The accent.
    pub accent: Hsla,
    /// The accent as a plate carrying [`Self::on_accent`].
    pub accent_strong: Hsla,
    /// A low wash of the accent.
    pub accent_wash: Hsla,
    /// Text on [`Self::accent_strong`].
    pub on_accent: Hsla,
    /// Errors, stop.
    pub danger: Hsla,
    /// Softer error copy.
    pub danger_muted: Hsla,
    /// Warnings.
    pub warning: Hsla,
    /// Softer warning copy.
    pub warning_muted: Hsla,
    /// Success, online.
    pub success: Hsla,
    /// Softer success copy.
    pub success_muted: Hsla,
    /// Working and streaming.
    pub busy: Hsla,
    /// The pixel mark's rows.
    pub glyph: GlyphPalette,

    /// An opaque raised pill on hover.
    pub surface_raised_hover: Hsla,
    /// The band behind a picker's header or footer strip.
    pub band: Hsla,
    /// The composer's and inputs' plate.
    pub input_bg: Hsla,
    /// Text selection.
    pub selection: Hsla,
    /// The terminal's block cursor.
    pub cursor: Hsla,
    /// The text caret.
    pub caret: Hsla,
    /// A destructive button's plate.
    pub danger_strong: Hsla,

    /// Inline code's text.
    pub code_text: Hsla,
    /// Inline code's wash.
    pub code_wash: Hsla,
    /// Syntax colours.
    pub syntax: SyntaxPalette,
    /// An added line.
    pub diff_add: Hsla,
    /// A deleted line.
    pub diff_del: Hsla,
    /// A hunk header's wash.
    pub diff_hunk_bg: Hsla,

    /// The terminal's colours.
    pub terminal: TerminalColors,

    /// The interface family.
    pub font_sans: SharedString,
    /// Geist, fixed, for chrome beside code and for recovery controls.
    pub font_sans_fixed: SharedString,
    /// Code, diffs and file editors.
    pub font_mono: SharedString,
    /// Terminal output.
    pub font_terminal: SharedString,
    /// Code's pixel size.
    pub code_font_size: f32,
    /// The terminal's pixel size.
    pub terminal_font_size: f32,
    /// The system sans, when a family is missing.
    pub font_sans_fallback: SharedString,
    /// The system mono, when a family is missing.
    pub font_mono_fallback: SharedString,
}

impl Theme {
    /// The window glass's tint over the blurred desktop (macOS vibrancy).
    pub const GLASS_ALPHA: f32 = 0.80;
    /// Light's glass tint.
    pub const GLASS_ALPHA_LIGHT: f32 = 0.80;

    /// The theme for `appearance` with `accent` and `surface`, before any font choice.
    #[must_use]
    pub fn new(
        appearance: Appearance,
        accent: AccentSelection,
        surface: SurfacePreference,
        fonts: &ThemeFonts,
    ) -> Self {
        Self::from_variant(&ThemeVariant::zeron(appearance), accent, surface, fonts)
    }

    /// Zeron dark with its own accent, frosted.
    #[must_use]
    pub fn dark() -> Self {
        Self::new(
            Appearance::Dark,
            AccentSelection::ThemeDefault,
            SurfacePreference::ThemeDefault,
            &ThemeFonts::default(),
        )
    }

    /// Zeron light with its own accent, frosted.
    #[must_use]
    pub fn light() -> Self {
        Self::new(
            Appearance::Light,
            AccentSelection::ThemeDefault,
            SurfacePreference::ThemeDefault,
            &ThemeFonts::default(),
        )
    }

    /// Every token of `variant` under the person's accent and material choice.
    #[must_use]
    pub fn from_variant(
        variant: &ThemeVariant,
        accent_selection: AccentSelection,
        surface_preference: SurfacePreference,
        fonts: &ThemeFonts,
    ) -> Self {
        let appearance = variant.appearance;
        let colors = &variant.colors;
        let accent = variant.accent_for(accent_selection);
        let text_muted = model_color(colors.text_muted);
        Self {
            appearance,
            accent_selection,
            surface_preference,
            surface_treatment: surface_preference.resolve(variant.recommended_surface_treatment),
            bg: model_color(colors.background),
            surface: model_color(colors.shell),
            surface_raised: model_color(colors.raised),
            surface_card: model_color(colors.card),
            surface_dialog: model_color(colors.dialog),
            surface_overlay: model_color(colors.overlay),
            element_hover: model_color(colors.hover),
            element_active: model_color(colors.active),
            border: model_color(colors.border),
            border_strong: model_color(colors.border_strong),
            text: model_color(colors.text),
            text_muted,
            text_faint: model_color(colors.text_faint),
            text_dim: text_muted,
            solid: model_color(colors.solid),
            on_solid: model_color(colors.on_solid),
            accent: model_color(accent.primary),
            accent_strong: model_color(accent.strong),
            accent_wash: model_color(accent.wash),
            on_accent: model_color(accent.on),
            danger: model_color(colors.danger),
            danger_muted: model_color(colors.danger_muted),
            warning: model_color(colors.warning),
            warning_muted: model_color(colors.warning_muted),
            success: model_color(colors.success),
            success_muted: model_color(colors.success_muted),
            busy: model_color(accent.activity),
            glyph: GlyphPalette {
                light: model_color(accent.glyph[0]),
                mid: model_color(accent.glyph[1]),
                deep: model_color(accent.glyph[2]),
            },
            surface_raised_hover: model_color(colors.raised),
            band: model_color(colors.hover),
            input_bg: model_color(colors.input),
            selection: model_color(accent.selection),
            cursor: model_color(colors.cursor),
            caret: model_color(accent.caret),
            danger_strong: model_color(colors.danger),
            code_text: model_color(accent.primary),
            code_wash: model_color(accent.wash),
            syntax: SyntaxPalette::from_variant(
                variant,
                &SyntaxPalette::for_appearance(appearance),
            ),
            diff_add: model_color(colors.diff_add),
            diff_del: model_color(colors.diff_delete),
            diff_hunk_bg: model_color(colors.diff_hunk),
            terminal: TerminalColors::from_variant(variant),
            font_sans: fonts.sans.clone(),
            font_sans_fixed: crate::typography::GEIST.into(),
            font_mono: fonts.mono.clone(),
            font_terminal: fonts.terminal.clone(),
            code_font_size: fonts.code_size,
            terminal_font_size: fonts.terminal_size,
            font_sans_fallback: SYSTEM_SANS.into(),
            font_mono_fallback: SYSTEM_MONO.into(),
        }
    }

    /// The shell's tint over the blurred window background, or the shell itself when opaque.
    #[must_use]
    pub fn glass(&self) -> Hsla {
        if self.surface_treatment == SurfaceTreatment::Opaque {
            return self.surface;
        }
        let base = match self.appearance {
            Appearance::Dark => Self::GLASS_ALPHA,
            Appearance::Light => Self::GLASS_ALPHA_LIGHT,
        };
        self.surface.opacity(self.contrast_checked_glass_alpha(base))
    }

    fn contrast_checked_glass_alpha(&self, base: f32) -> f32 {
        self.contrast_checked_tint_alpha(self.surface, base, self.adverse_backdrop())
    }

    /// A tint's coverage raised only as far as primary text needs 4.5:1 and muted text 3:1
    /// over the worst backdrop.
    fn contrast_checked_tint_alpha(&self, tint: Hsla, base: f32, backdrop: Hsla) -> f32 {
        (0..=20_u8)
            .map(|step| (1.0 - base).mul_add(f32::from(step) / 20.0, base))
            .find(|alpha| {
                let composite = flatten(tint.opacity(*alpha), backdrop);
                painted_contrast(self.text, composite) >= 4.5
                    && painted_contrast(self.text_muted, composite) >= 3.0
            })
            .unwrap_or(1.0)
    }

    fn adverse_backdrop(&self) -> Hsla {
        match self.appearance {
            Appearance::Dark => grey(0xff),
            Appearance::Light => grey(0),
        }
    }

    /// Whether the window paints translucent chrome over the blurred desktop.
    #[must_use]
    pub fn is_glass(&self) -> bool {
        self.glass().a < 1.0
    }

    /// The editor host's and the files column's background.
    #[must_use]
    pub fn panel_bg(&self) -> Hsla {
        if self.is_glass() { self.bg.opacity(0.4) } else { self.bg }
    }

    /// Whether floating surfaces (popovers, the composer) blur what is under them: frost on a
    /// desktop platform.
    #[must_use]
    pub fn is_frost(&self) -> bool {
        self.surface_treatment == SurfaceTreatment::Frosted
            && cfg!(any(target_os = "macos", target_os = "linux", target_os = "windows"))
    }

    /// The hover wash for chrome on glass: sidebar rows, tabs, titlebar buttons.
    #[must_use]
    pub const fn glass_hover(&self) -> Hsla {
        self.element_hover
    }

    /// The theme for a floating surface: on frost, the text tiers are kept legible over the
    /// blur.
    #[must_use]
    pub fn for_popup(&self) -> Self {
        let mut popup = self.clone();
        if !self.is_frost() {
            return popup;
        }
        if self.appearance.is_dark() {
            popup.text_muted = self.text.opacity(0.64);
            popup.text_faint = self.text.opacity(0.48);
            return popup;
        }
        let mut primary_surface = self.clone();
        primary_surface.text_muted = self.text;
        let background = flatten(
            primary_surface.glass_overlay(),
            flatten(self.glass(), self.adverse_backdrop()),
        );
        if painted_contrast(popup.text, background) < 4.5 {
            popup.text = (1..=100_u8)
                .map(|step| mix(self.text, grey(0), f32::from(step) / 100.0))
                .find(|text| painted_contrast(*text, background) >= 4.5)
                .unwrap_or_else(|| grey(0));
        }
        let text = popup.text;
        for color in [&mut popup.text_muted, &mut popup.text_dim, &mut popup.text_faint] {
            let original = *color;
            if painted_contrast(original, background) >= 4.5 {
                continue;
            }
            *color = (1..=100_u8)
                .map(|step| mix(original, text, f32::from(step) / 100.0))
                .find(|tone| painted_contrast(*tone, background) >= 4.5)
                .unwrap_or(text);
        }
        popup
    }

    /// Settings pages: dark keeps its authored tiers, light reads as a popup.
    #[must_use]
    pub fn for_settings_surface(&self) -> Self {
        if self.appearance.is_dark() { self.clone() } else { self.for_popup() }
    }

    /// The tint floating cards paint over their blur ([`crate::frost::frosted`]).
    #[must_use]
    pub fn glass_overlay(&self) -> Hsla {
        if !self.is_frost() {
            return self.surface_overlay;
        }
        if self.appearance.is_light() {
            return self.surface_overlay.opacity(0.45);
        }
        self.surface_overlay.opacity(self.contrast_checked_tint_alpha(
            self.surface_overlay,
            0.50,
            self.adverse_backdrop(),
        ))
    }

    /// The composer pill's fill: dark frost's tint, else the input glass.
    #[must_use]
    pub fn composer_surface_bg(&self) -> Hsla {
        if self.is_frost() && self.appearance.is_dark() {
            self.composer_sidebar_tint()
        } else {
            self.input_glass_bg()
        }
    }

    /// The composer pill's edge: a cool silver on frost, the border otherwise.
    #[must_use]
    pub fn composer_surface_border(&self) -> Hsla {
        composer_surface_border(self)
    }

    /// Dark frost's 15% tint for the composer and floating cards.
    #[must_use]
    #[expect(clippy::many_single_char_names, reason = "the colour models' own letters")]
    pub fn composer_sidebar_tint(&self) -> Hsla {
        let target = if self.is_glass() {
            flatten(self.bg.opacity(0.4), flatten(self.glass(), self.bg))
        } else {
            self.bg
        };
        let canvas = flatten(self.glass(), self.bg);
        let canvas = hsl_to_rgb(canvas.h, canvas.s, canvas.l);
        let target = hsl_to_rgb(target.h, target.s, target.l);
        let alpha = canvas.into_iter().zip(target).fold(0.60_f32, |alpha, (base, desired)| {
            let needed = if desired > base {
                (desired - base) / (1.0 - base).max(f32::EPSILON)
            } else {
                (base - desired) / base.max(f32::EPSILON)
            };
            alpha.max(needed)
        });
        let [r, g, b] = [0, 1, 2].map(|i: usize| {
            let (desired, base) =
                (target.get(i).copied().unwrap_or(0.0), canvas.get(i).copied().unwrap_or(0.0));
            (base.mul_add(-(1.0 - alpha), desired) / alpha).clamp(0.0, 1.0)
        });
        let (h, s, l) = rgb_to_hsl(r, g, b);
        hsla(h, s, l, 0.15)
    }

    /// The composer's, queue tray's and input panels' fill.
    #[must_use]
    pub fn input_glass_bg(&self) -> Hsla {
        let tint = flatten(self.input_bg, self.bg);
        if !self.is_frost() {
            return tint;
        }
        if self.appearance.is_light() {
            return tint.opacity(0.35);
        }
        let window = flatten(self.glass(), self.adverse_backdrop());
        self.input_bg.opacity(self.contrast_checked_tint_alpha(
            self.input_bg,
            self.input_bg.a,
            window,
        ))
    }

    /// A section card's fill: the surface, thinned on frost.
    #[must_use]
    pub fn card_glass_bg(&self) -> Hsla {
        if !self.is_frost() {
            return self.surface;
        }
        let window = flatten(self.glass(), self.adverse_backdrop());
        self.surface.opacity(self.contrast_checked_tint_alpha(self.surface, 0.40, window))
    }

    /// The standard modal backdrop.
    #[must_use]
    pub const fn scrim(&self) -> Hsla {
        scrim_for(self.appearance, SCRIM_ALPHA_DARK)
    }

    /// How the platform composites the window behind the paint: blurred under glass.
    #[must_use]
    pub fn window_background_appearance(&self) -> gpui::WindowBackgroundAppearance {
        if self.is_glass() {
            gpui::WindowBackgroundAppearance::Blurred
        } else {
            gpui::WindowBackgroundAppearance::Opaque
        }
    }

    /// Install `theme` as the GPUI global and point the paint helpers at its appearance. The
    /// only way the appearance changes.
    pub fn install(theme: Self, cx: &mut App) {
        let changed = cx.try_global::<Self>().is_some_and(|old| {
            old.accent_selection != theme.accent_selection
                || old.surface_preference != theme.surface_preference
                || old.appearance != theme.appearance
        });
        set_current_appearance(theme.appearance);
        sync_kit_scrollbar(&theme, cx);
        cx.set_global(theme);
        if changed {
            bump_style_generation();
        }
    }

    /// The installed theme.
    #[must_use]
    pub fn of(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    /// Fill ink at `alpha`; see [`ink`].
    #[must_use]
    pub const fn ink(&self, alpha: f32) -> Hsla {
        ink_for(self.appearance, alpha)
    }

    /// Hairline ink at `alpha`; see [`hairline`].
    #[must_use]
    pub fn hairline(&self, alpha: f32) -> Hsla {
        hairline_for(self.appearance, alpha)
    }

    /// A state wash at `alpha`; see [`wash`].
    #[must_use]
    pub const fn wash(&self, alpha: f32) -> Hsla {
        wash_for(self.appearance, alpha)
    }
}

/// The scrollbar thumb at rest, hovered and held.
#[must_use]
pub fn scrollbar_thumb_colors(theme: &Theme) -> (Hsla, Hsla, Hsla) {
    (theme.text.opacity(0.30), theme.text.opacity(0.42), theme.text.opacity(0.55))
}

/// gpui-kit's scrollbars (its editor and inputs) in the theme's thumb tones.
fn sync_kit_scrollbar(theme: &Theme, cx: &mut App) {
    if cx.try_global::<gpui_kit::base::Theme>().is_none() {
        return;
    }
    let (normal, hover, active) = scrollbar_thumb_colors(theme);
    let styles = gpui_kit::base::ScrollbarStyles::default()
        .thumb(|style| style.bg(normal))
        .thumb_hover(|style| style.bg(hover))
        .thumb_active(|style| style.bg(active));
    let kit = gpui_kit::base::Theme::global_mut(cx);
    kit.scrollbar = std::mem::take(&mut kit.scrollbar).with_styles(styles);
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Global for Theme {}

/// The system sans on Apple platforms.
const SYSTEM_SANS: &str = "Helvetica";
/// The system mono on Apple platforms.
const SYSTEM_MONO: &str = "Menlo";

/// A neutral oklch tone (chroma 0) as [`Hsla`].
#[must_use]
pub fn neutral(lightness: f32) -> Hsla {
    let [v, _, _] = oklch_to_srgb(lightness, 0.0, 0.0);
    hsla(0.0, 0.0, v, 1.0)
}

/// Translucent fill ink for states and chip plates: white on dark, black on light. Alphas are
/// quoted in dark's terms at every call site; hover fades rest on `ink(0.0)`.
#[must_use]
pub fn ink(alpha: f32) -> Hsla {
    ink_for(current_appearance(), alpha)
}

const fn ink_for(appearance: Appearance, alpha: f32) -> Hsla {
    match appearance {
        Appearance::Dark => hsla(0.0, 0.0, 1.0, alpha),
        Appearance::Light => hsla(0.0, 0.0, 0.0, alpha * INK_FILL_SCALE),
    }
}

/// Translucent hairline ink for borders, dividers and rings: white on dark, black on light at
/// [`INK_HAIRLINE_SCALE`] of the alpha.
#[must_use]
pub fn hairline(alpha: f32) -> Hsla {
    hairline_for(current_appearance(), alpha)
}

fn hairline_for(appearance: Appearance, alpha: f32) -> Hsla {
    match appearance {
        Appearance::Dark => hsla(0.0, 0.0, 1.0, alpha),
        Appearance::Light => hsla(0.0, 0.0, 0.0, (alpha * INK_HAIRLINE_SCALE).min(0.5)),
    }
}

/// A state wash: a softened [`ink`] that stops short of pure black or white.
#[must_use]
pub fn wash(alpha: f32) -> Hsla {
    wash_for(current_appearance(), alpha)
}

const fn wash_for(appearance: Appearance, alpha: f32) -> Hsla {
    match appearance {
        Appearance::Dark => hsla(0.0, 0.0, 0.92, alpha),
        Appearance::Light => hsla(0.0, 0.0, 0.10, alpha * INK_FILL_SCALE),
    }
}

/// The standard modal backdrop's alpha on dark.
pub const SCRIM_ALPHA_DARK: f32 = 0.60;

/// A modal backdrop at `alpha_dark`, quoted in dark's terms; light runs about half.
#[must_use]
pub fn scrim(alpha_dark: f32) -> Hsla {
    scrim_for(current_appearance(), alpha_dark)
}

const fn scrim_for(appearance: Appearance, alpha_dark: f32) -> Hsla {
    match appearance {
        Appearance::Dark => hsla(0.0, 0.0, 0.0, alpha_dark),
        Appearance::Light => hsla(0.0, 0.0, 0.0, 0.32 * (alpha_dark / SCRIM_ALPHA_DARK)),
    }
}

/// The recessed band behind a palette's or picker's header or footer strip.
#[must_use]
pub fn band() -> Hsla {
    match current_appearance() {
        Appearance::Dark => hsla(0.0, 0.0, 0.0, 0.16),
        Appearance::Light => hsla(0.0, 0.0, 0.0, 0.045),
    }
}

/// A selected tab's, session's or space's wash on glass; the ring tells it from hover.
#[must_use]
pub fn glass_selected_bg() -> Hsla {
    match current_appearance() {
        Appearance::Dark => wash(0.11),
        Appearance::Light => wash(0.06),
    }
}

/// The user bubble's plate, a step softer than the selection.
#[must_use]
pub fn user_bubble_bg() -> Hsla {
    match current_appearance() {
        Appearance::Dark => wash(0.08),
        Appearance::Light => wash(0.04),
    }
}

/// A selected or keyed row inside a floating card.
#[must_use]
pub fn card_selected_bg() -> Hsla {
    match current_appearance() {
        Appearance::Dark => wash(0.11),
        Appearance::Light => wash(0.06),
    }
}

/// The selected chip's ring on glass: the inset ring alone.
#[must_use]
pub fn glass_selected_shadows() -> Vec<BoxShadow> {
    card_selected_shadows()
}

/// Selection's ring inside a floating card: an inset 1 px ring, never a drop shadow.
#[must_use]
pub fn card_selected_shadows() -> Vec<BoxShadow> {
    let color = match current_appearance() {
        Appearance::Dark => hairline(0.09),
        Appearance::Light => hsla(0.0, 0.0, 0.0, 0.07),
    };
    vec![BoxShadow {
        color,
        offset: point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: px(1.0),
        inset: true,
    }]
}

/// An achromatic tone from an 8-bit channel: `grey(13)` is `#0d0d0d`.
#[must_use]
pub fn grey(value: u8) -> Hsla {
    hsla(0.0, 0.0, f32::from(value) / 255.0, 1.0)
}

/// An oklch colour (L 0–1, C, H in degrees) as [`Hsla`].
#[must_use]
#[expect(clippy::many_single_char_names, reason = "the colour models' own letters")]
pub fn oklch(l: f32, c: f32, h_deg: f32) -> Hsla {
    let [r, g, b] = oklch_to_srgb(l, c, h_deg);
    let (h, s, l) = rgb_to_hsl(r, g, b);
    hsla(h, s, l, 1.0)
}

/// oklch to sRGB, each channel 0–1, clipped (Björn Ottosson's matrices, as CSS Color 4).
#[must_use]
#[expect(clippy::many_single_char_names, reason = "the colour models' own letters")]
pub fn oklch_to_srgb(l: f32, c: f32, h_deg: f32) -> [f32; 3] {
    let h = h_deg.to_radians();
    let (a, b) = (c * h.cos(), c * h.sin());
    let l_ = 0.215_803_76_f32.mul_add(b, 0.396_337_78_f32.mul_add(a, l));
    let m_ = (-0.063_854_17_f32).mul_add(b, (-0.105_561_346_f32).mul_add(a, l));
    let s_ = (-1.291_485_5_f32).mul_add(b, (-0.089_484_18_f32).mul_add(a, l));
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 0.230_969_93_f32.mul_add(s3, 4.076_741_7_f32.mul_add(l3, -3.307_711_6 * m3));
    let g = (-0.341_319_4_f32).mul_add(s3, (-1.268_438_f32).mul_add(l3, 2.609_757_4 * m3));
    let b = 1.707_614_7_f32.mul_add(s3, (-0.004_196_086_3_f32).mul_add(l3, -0.703_418_6 * m3));
    [gamma_encode(r), gamma_encode(g), gamma_encode(b)]
}

fn gamma_encode(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.003_130_8 { 12.92 * x } else { 1.055_f32.mul_add(x.powf(1.0 / 2.4), -0.055) }
}

/// sRGB (0–1) to HSL, every component 0–1 as GPUI's [`Hsla`] has it.
#[must_use]
#[expect(clippy::many_single_char_names, reason = "the colour models' own letters")]
pub fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = f32::midpoint(max, min);
    let delta = max - min;
    if delta < f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 { delta / (2.0 - max - min) } else { delta / (max + min) };
    let h = if (max - r).abs() < f32::EPSILON {
        ((g - b) / delta).rem_euclid(6.0)
    } else if (max - g).abs() < f32::EPSILON {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    } / 6.0;
    (h, s, l)
}

/// HSL (GPUI's convention, 0–1) to sRGB components 0–1.
#[must_use]
#[expect(clippy::many_single_char_names, reason = "the colour models' own letters")]
pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    if s <= f32::EPSILON {
        return [l, l, l];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l.mul_add(-s, l + s) };
    let p = 2.0_f32.mul_add(l, -q);
    let hue = |t: f32| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            ((q - p) * 6.0).mul_add(t, p)
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            ((q - p) * (2.0 / 3.0 - t)).mul_add(6.0, p)
        } else {
            p
        }
    };
    [hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0)]
}

/// WCAG 2.1 relative luminance of an opaque colour.
#[must_use]
pub fn relative_luminance(color: Hsla) -> f32 {
    let lin = |c: f32| if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    let [r, g, b] = hsl_to_rgb(color.h, color.s, color.l);
    0.0722_f32.mul_add(lin(b), 0.2126_f32.mul_add(lin(r), 0.7152 * lin(g)))
}

/// WCAG 2.1 contrast ratio of two opaque colours, 1 to 21.
#[must_use]
pub fn contrast_ratio(a: Hsla, b: Hsla) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn painted_contrast(foreground: Hsla, background: Hsla) -> f32 {
    contrast_ratio(flatten(foreground, background), background)
}

/// `fg`, maybe translucent, composited over an opaque `bg`: the colour the eye receives.
#[must_use]
pub fn flatten(fg: Hsla, bg: Hsla) -> Hsla {
    let a = fg.a.clamp(0.0, 1.0);
    let [fr, fg_, fb] = hsl_to_rgb(fg.h, fg.s, fg.l);
    let [br, bg_, bb] = hsl_to_rgb(bg.h, bg.s, bg.l);
    let over = |f: f32, b: f32| f.mul_add(a, b * (1.0 - a));
    let (h, s, l) = rgb_to_hsl(over(fr, br), over(fg_, bg_), over(fb, bb));
    hsla(h, s, l, 1.0)
}

/// The composer's and its companions' edge: a cool silver on frost, the border otherwise.
#[must_use]
pub fn composer_surface_border(theme: &Theme) -> Hsla {
    if theme.is_frost() {
        match theme.appearance {
            Appearance::Dark => hsla(210.0 / 360.0, 0.18, 0.78, 0.09),
            Appearance::Light => hsla(210.0 / 360.0, 0.18, 0.32, 0.10),
        }
    } else {
        theme.border
    }
}

/// A component-wise mix of two colours, through hue naively.
#[must_use]
pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: f32, y: f32| (y - x).mul_add(t, x);
    hsla(lerp(a.h, b.h), lerp(a.s, b.s), lerp(a.l, b.l), lerp(a.a, b.a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0 to 255")]
    fn srgb_u8(c: [f32; 3]) -> [u8; 3] {
        c.map(|v| (v * 255.0).round() as u8)
    }

    fn hex(c: Hsla) -> String {
        let [r, g, b] = srgb_u8(hsl_to_rgb(c.h, c.s, c.l));
        format!("#{r:02x}{g:02x}{b:02x}")
    }

    #[test]
    fn neutral_950_is_0a0a0a() {
        assert_eq!(srgb_u8(oklch_to_srgb(0.145, 0.0, 0.0)), [10, 10, 10], "neutral-950");
    }

    /// The opaque planes are zeron's (the blueprint's §1.3): window, sidebar, content.
    #[test]
    fn the_opaque_planes_are_zerons() {
        let _guard = lock_appearance();
        for (appearance, window, sidebar, content) in [
            (Appearance::Dark, "#0d0d0d", "#181818", "#060606"),
            (Appearance::Light, "#f3f3f5", "#e8e8ea", "#ffffff"),
        ] {
            let theme = Theme::new(
                appearance,
                AccentSelection::ThemeDefault,
                SurfacePreference::Opaque,
                &ThemeFonts::default(),
            );
            set_current_appearance(appearance);
            assert_eq!(hex(theme.glass()), window, "{appearance:?} window");
            assert_eq!(hex(flatten(wash(0.05), theme.glass())), sidebar, "{appearance:?} sidebar");
            assert_eq!(hex(theme.panel_bg()), content, "{appearance:?} content");
            assert!(!theme.is_glass(), "opaque is solid");
        }
        set_current_appearance(Appearance::Dark);
    }

    #[test]
    fn the_scrollbar_thumbs_follow_the_appearance() {
        let (dark, light) = (Theme::dark(), Theme::light());
        let (d, l) = (scrollbar_thumb_colors(&dark), scrollbar_thumb_colors(&light));
        assert!(d.0.l > dark.bg.l, "light thumb on dark");
        assert!(l.0.l < light.bg.l, "dark thumb on light");
    }

    #[test]
    fn hsl_round_trips_through_rgb() {
        for (r, g, b) in [(0.1, 0.5, 0.9), (0.9, 0.2, 0.3), (0.5, 0.5, 0.5)] {
            let (h, s, l) = rgb_to_hsl(r, g, b);
            let [r2, g2, b2] = hsl_to_rgb(h, s, l);
            assert!(
                (r - r2).abs() < 1e-4 && (g - g2).abs() < 1e-4 && (b - b2).abs() < 1e-4,
                "{r} {g} {b}"
            );
        }
    }

    #[test]
    fn contrast_ratio_hits_known_anchors() {
        let ratio = contrast_ratio(grey(0), grey(255));
        assert!((ratio - 21.0).abs() < 0.01, "black on white is 21:1, got {ratio}");
    }

    #[test]
    fn the_default_accent_is_zerons_violet() {
        assert_eq!(hex(Theme::dark().accent), "#8b7cf6", "dark");
        assert_eq!(hex(Theme::light().accent), "#5b43e8", "light");
    }
}
