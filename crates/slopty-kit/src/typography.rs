// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Interface typography: the bundled Geist faces, the person's family and size choices, and the
//! families installed into [`crate::theme::Theme`].
//!
//! Interface text is written in [`ui_rems`] at the 16 px base, so the person's UI size (12 to
//! 20, default 16) scales the chrome through the window's rem size. Code and the terminal keep
//! absolute pixel sizes. Geist and Geist Mono v1.7.2 ship inside the binary (SIL OFL 1.1,
//! `assets/fonts/LICENSE-Geist.txt`).

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use gpui::{App, Global, Rems, SharedString, Window, px, rems};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::appearance::{self, Change};

/// The Geist family's name.
pub const GEIST: &str = "Geist";
/// The Geist Mono family's name.
pub const GEIST_MONO: &str = "Geist Mono";

/// A bundled, virtual or installed interface family.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum UiFontFamily {
    /// Geist, bundled; the default.
    #[default]
    Geist,
    /// Geist Mono, bundled.
    GeistMono,
    /// The system's interface face.
    System,
    /// A family installed on this device.
    Installed(String),
}

impl UiFontFamily {
    /// Its name in a picker.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Geist => GEIST,
            Self::GeistMono => GEIST_MONO,
            Self::System => "System UI",
            Self::Installed(name) => name,
        }
    }

    /// The family name the text system knows it by.
    #[must_use]
    pub fn family_name(&self) -> &str {
        match self {
            Self::Geist => GEIST,
            Self::GeistMono => GEIST_MONO,
            Self::System => ".SystemUIFont",
            Self::Installed(name) => name,
        }
    }
}

impl Serialize for UiFontFamily {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Geist => serializer.serialize_str("geist"),
            Self::GeistMono => serializer.serialize_str("geistMono"),
            Self::System => serializer.serialize_str("system"),
            Self::Installed(name) => serializer.serialize_str(&format!("installed:{name}")),
        }
    }
}

impl<'de> Deserialize<'de> for UiFontFamily {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "geist" => Self::Geist,
            "geistMono" => Self::GeistMono,
            "system" => Self::System,
            other => match other.strip_prefix("installed:") {
                Some(name) if !name.is_empty() => Self::Installed(name.to_owned()),
                _ => Self::Geist,
            },
        })
    }
}

/// The base size rem-based interface text scales from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UiFontSize(u8);

impl UiFontSize {
    /// Every size a person can pick.
    pub const ALL: [Self; 7] =
        [Self(12), Self(13), Self(14), Self(15), Self(16), Self(18), Self(20)];

    /// A size of `pixels`, snapped to the nearest choice.
    #[must_use]
    pub fn new(pixels: u8) -> Self {
        Self(pixels).normalized()
    }

    /// Its pixels.
    #[must_use]
    pub fn pixels(self) -> f32 {
        f32::from(self.0)
    }

    /// Its label in a picker.
    #[must_use]
    pub fn label(self) -> SharedString {
        format!("{} px", self.0).into()
    }

    /// The nearest choice to this size.
    #[must_use]
    pub fn normalized(self) -> Self {
        Self::ALL
            .into_iter()
            .min_by_key(|candidate| candidate.0.abs_diff(self.0))
            .unwrap_or_default()
    }
}

impl Default for UiFontSize {
    fn default() -> Self {
        Self(16)
    }
}

/// A size drawn at the 16 px base, as a rem that scales with the person's UI size. Code,
/// diffs and the terminal keep absolute pixels.
#[must_use]
pub const fn ui_rems(pixels_at_default: f32) -> Rems {
    rems(pixels_at_default / 16.0)
}

/// Code's default pixel size.
pub const CODE_FONT_SIZE_DEFAULT: f32 = 12.5;
/// The terminal's default pixel size.
pub const TERMINAL_FONT_SIZE_DEFAULT: f32 = 13.0;
/// The least code or terminal size.
pub const FONT_SIZE_MIN: f32 = 8.0;
/// The most code or terminal size.
pub const FONT_SIZE_MAX: f32 = 32.0;

/// An absolute code or terminal size, clamped to the supported range.
#[must_use]
pub const fn clamp_font_size(size: f32) -> f32 {
    size.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX)
}

/// Which families registered in this process, and which installed ones are fixed width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontAvailability {
    geist: bool,
    geist_mono: bool,
    choices: Vec<UiFontFamily>,
    fixed_width: Vec<UiFontFamily>,
}

impl FontAvailability {
    /// Every family a person can pick for the interface or code.
    #[must_use]
    pub fn choices(&self) -> &[UiFontFamily] {
        &self.choices
    }

    /// The families whose advances fit the terminal's cell grid.
    #[must_use]
    pub fn fixed_width_choices(&self) -> &[UiFontFamily] {
        &self.fixed_width
    }

    /// Whether `family` can be used.
    #[must_use]
    pub fn is_available(&self, family: &UiFontFamily) -> bool {
        match family {
            UiFontFamily::Geist => self.geist,
            UiFontFamily::GeistMono => self.geist_mono,
            UiFontFamily::System => true,
            UiFontFamily::Installed(_) => self.choices.contains(family),
        }
    }

    /// Whether `family` can be used in the terminal's grid.
    #[must_use]
    pub fn is_fixed_width_available(&self, family: &UiFontFamily) -> bool {
        self.is_available(family) && self.fixed_width.contains(family)
    }

    const fn fallback(&self) -> UiFontFamily {
        if self.geist { UiFontFamily::Geist } else { UiFontFamily::System }
    }

    /// Code and the terminal fall back to the bundled mono, never to the proportional sans.
    const fn fallback_mono(&self) -> UiFontFamily {
        if self.geist_mono { UiFontFamily::GeistMono } else { self.fallback() }
    }

    /// Every family as available, for tests that need no text system.
    #[must_use]
    pub fn bundled() -> Self {
        Self {
            geist: true,
            geist_mono: true,
            choices: vec![UiFontFamily::Geist, UiFontFamily::GeistMono, UiFontFamily::System],
            fixed_width: vec![UiFontFamily::GeistMono],
        }
    }
}

impl Default for FontAvailability {
    fn default() -> Self {
        Self {
            geist: false,
            geist_mono: false,
            choices: vec![UiFontFamily::System],
            fixed_width: Vec::new(),
        }
    }
}

/// The requested and effective typography of the process.
///
/// The interface and code pick from the whole catalogue; the terminal paints into a cell grid
/// sized by the `m` advance, so it picks from the fixed-width subset only.
#[derive(Debug, Clone)]
pub struct TypographyState {
    /// The interface family asked for.
    pub requested: UiFontFamily,
    /// The interface family in use.
    pub effective: UiFontFamily,
    /// The interface base size.
    pub size: UiFontSize,
    /// The terminal family asked for.
    pub terminal_requested: UiFontFamily,
    /// The terminal family in use.
    pub terminal_effective: UiFontFamily,
    /// The terminal's pixel size.
    pub terminal_font_size: f32,
    /// The code family asked for.
    pub code_requested: UiFontFamily,
    /// The code family in use.
    pub code_effective: UiFontFamily,
    /// Code's pixel size.
    pub code_font_size: f32,
    /// What is installed.
    pub availability: FontAvailability,
    /// Bumped when anything that changes text measurement changes.
    pub generation: u32,
}

impl Global for TypographyState {}

const GEIST_FACES: [&[u8]; 8] = [
    include_bytes!("../assets/fonts/Geist.ttf"),
    include_bytes!("../assets/fonts/Geist-Italic.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../assets/fonts/Geist-MediumItalic.ttf"),
    include_bytes!("../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Geist-SemiBoldItalic.ttf"),
    include_bytes!("../assets/fonts/Geist-Bold.ttf"),
    include_bytes!("../assets/fonts/Geist-BoldItalic.ttf"),
];

const GEIST_MONO_FACES: [&[u8]; 8] = [
    include_bytes!("../assets/fonts/GeistMono.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Italic.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Medium.ttf"),
    include_bytes!("../assets/fonts/GeistMono-MediumItalic.ttf"),
    include_bytes!("../assets/fonts/GeistMono-SemiBold.ttf"),
    include_bytes!("../assets/fonts/GeistMono-SemiBoldItalic.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Bold.ttf"),
    include_bytes!("../assets/fonts/GeistMono-BoldItalic.ttf"),
];

fn register_family(cx: &App, family: &UiFontFamily, faces: &'static [&'static [u8]]) -> bool {
    let fonts = faces.iter().map(|face| Cow::Borrowed(*face)).collect();
    match cx.text_system().add_fonts(fonts) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(font_family = family.label(), %error, "a bundled family did not register");
            false
        }
    }
}

/// How far a face's sampled advances may spread and still count as fixed width.
const FIXED_WIDTH_TOLERANCE: f32 = 0.01;

/// Whether every Latin cell of `font` is one `m` wide, measured rather than read from flags a
/// font can set wrongly.
fn face_is_fixed_width(font: &ttf_parser::Face<'_>) -> bool {
    let Some(advances) = ['i', 'm', 'W', '0']
        .iter()
        .map(|ch| font.glyph_index(*ch).and_then(|id| font.glyph_hor_advance(id)))
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    let min = advances.iter().copied().min().unwrap_or(0);
    let max = advances.iter().copied().max().unwrap_or(0);
    min > 0 && f32::from(max.saturating_sub(min)) <= FIXED_WIDTH_TOLERANCE * f32::from(min)
}

/// Installed families with the Latin `m` GPUI measures by, each mapped to whether all its faces
/// are fixed width.
fn installed_families_with_latin_metrics() -> BTreeMap<String, bool> {
    let mut database = fontdb::Database::new();
    database.load_system_fonts();
    let mut families = BTreeMap::new();
    for face in database.faces() {
        let Some(Some(fixed_width)) = database.with_face_data(face.id, |data, index| {
            ttf_parser::Face::parse(data, index)
                .ok()
                .filter(|font| font.glyph_index('m').is_some())
                .map(|font| face_is_fixed_width(&font))
        }) else {
            continue;
        };
        for (name, _) in &face.families {
            families
                .entry(name.clone())
                .and_modify(|all_fixed| *all_fixed &= fixed_width)
                .or_insert(fixed_width);
        }
    }
    families
}

/// Register the bundled families, each on its own so one bad face hides nothing else, and list
/// what the device has installed. Call once at boot, before the first theme.
pub fn register_fonts(cx: &App) -> FontAvailability {
    let families_with_latin_metrics = installed_families_with_latin_metrics();
    let system_names: BTreeSet<_> = cx
        .text_system()
        .all_font_names()
        .into_iter()
        .filter(|name| !name.starts_with('.'))
        .filter(|name| name != GEIST && name != GEIST_MONO)
        .filter(|name| families_with_latin_metrics.contains_key(name))
        .collect();
    let geist = register_family(cx, &UiFontFamily::Geist, &GEIST_FACES);
    let geist_mono = register_family(cx, &UiFontFamily::GeistMono, &GEIST_MONO_FACES);
    let mut choices = vec![UiFontFamily::Geist, UiFontFamily::GeistMono, UiFontFamily::System];
    let mut fixed_width = vec![UiFontFamily::GeistMono];
    for name in system_names {
        if families_with_latin_metrics.get(&name) == Some(&true) {
            fixed_width.push(UiFontFamily::Installed(name.clone()));
        }
        choices.push(UiFontFamily::Installed(name));
    }
    FontAvailability { geist, geist_mono, choices, fixed_width }
}

fn resolve_effective(requested: &UiFontFamily, availability: &FontAvailability) -> UiFontFamily {
    if availability.is_available(requested) { requested.clone() } else { availability.fallback() }
}

/// `fixed_width_only` guards the terminal's grid: a proportional pick falls back rather than
/// draw cells its glyphs do not fill.
fn resolve_effective_mono(
    requested: &UiFontFamily,
    availability: &FontAvailability,
    fixed_width_only: bool,
) -> UiFontFamily {
    let usable = if fixed_width_only {
        availability.is_fixed_width_available(requested)
    } else {
        availability.is_available(requested)
    };
    if usable { requested.clone() } else { availability.fallback_mono() }
}

/// The person's typography choices, as settings keep them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TypographyChoice {
    /// The interface family.
    pub family: UiFontFamily,
    /// The interface size.
    pub size: UiFontSize,
    /// The terminal family.
    pub terminal_family: Option<UiFontFamily>,
    /// The terminal size, in pixels.
    pub terminal_size: Option<f32>,
    /// The code family.
    pub code_family: Option<UiFontFamily>,
    /// The code size, in pixels.
    pub code_size: Option<f32>,
}

/// Install the typography state before [`crate::appearance::init`] builds the first theme.
pub fn init(choice: TypographyChoice, availability: FontAvailability, cx: &mut App) {
    let terminal_requested = choice.terminal_family.unwrap_or(UiFontFamily::GeistMono);
    let code_requested = choice.code_family.unwrap_or(UiFontFamily::GeistMono);
    cx.set_global(TypographyState {
        effective: resolve_effective(&choice.family, &availability),
        requested: choice.family,
        size: choice.size.normalized(),
        terminal_effective: resolve_effective_mono(&terminal_requested, &availability, true),
        terminal_requested,
        terminal_font_size: clamp_font_size(
            choice.terminal_size.unwrap_or(TERMINAL_FONT_SIZE_DEFAULT),
        ),
        code_effective: resolve_effective_mono(&code_requested, &availability, false),
        code_requested,
        code_font_size: clamp_font_size(choice.code_size.unwrap_or(CODE_FONT_SIZE_DEFAULT)),
        availability,
        generation: 0,
    });
}

fn state(cx: &App) -> Option<&TypographyState> {
    cx.try_global::<TypographyState>()
}

/// The interface family asked for.
#[must_use]
pub fn requested(cx: &App) -> UiFontFamily {
    state(cx).map(|state| state.requested.clone()).unwrap_or_default()
}

/// The interface family in use.
#[must_use]
pub fn effective(cx: &App) -> UiFontFamily {
    state(cx).map(|state| state.effective.clone()).unwrap_or_default()
}

/// The interface family's name.
#[must_use]
pub fn effective_family_name(cx: &App) -> SharedString {
    SharedString::from(effective(cx).family_name().to_owned())
}

/// The terminal family asked for.
#[must_use]
pub fn terminal_requested(cx: &App) -> UiFontFamily {
    state(cx).map_or(UiFontFamily::GeistMono, |state| state.terminal_requested.clone())
}

/// The terminal family in use.
#[must_use]
pub fn terminal_effective(cx: &App) -> UiFontFamily {
    state(cx).map_or(UiFontFamily::GeistMono, |state| state.terminal_effective.clone())
}

/// The terminal family's name.
#[must_use]
pub fn terminal_effective_family_name(cx: &App) -> SharedString {
    SharedString::from(terminal_effective(cx).family_name().to_owned())
}

/// The terminal's pixel size.
#[must_use]
pub fn terminal_font_size(cx: &App) -> f32 {
    state(cx).map_or(TERMINAL_FONT_SIZE_DEFAULT, |state| state.terminal_font_size)
}

/// The code family asked for.
#[must_use]
pub fn code_requested(cx: &App) -> UiFontFamily {
    state(cx).map_or(UiFontFamily::GeistMono, |state| state.code_requested.clone())
}

/// The code family in use.
#[must_use]
pub fn code_effective(cx: &App) -> UiFontFamily {
    state(cx).map_or(UiFontFamily::GeistMono, |state| state.code_effective.clone())
}

/// The code family's name.
#[must_use]
pub fn code_effective_family_name(cx: &App) -> SharedString {
    SharedString::from(code_effective(cx).family_name().to_owned())
}

/// Code's pixel size.
#[must_use]
pub fn code_font_size(cx: &App) -> f32 {
    state(cx).map_or(CODE_FONT_SIZE_DEFAULT, |state| state.code_font_size)
}

/// The interface base size.
#[must_use]
pub fn font_size(cx: &App) -> UiFontSize {
    state(cx).map(|state| state.size).unwrap_or_default()
}

/// What is installed.
#[must_use]
pub fn availability(cx: &App) -> FontAvailability {
    state(cx).map(|state| state.availability.clone()).unwrap_or_default()
}

/// The current typography's id: layout caches compare it to drop measurements.
#[must_use]
pub fn generation(cx: &App) -> u32 {
    state(cx).map(|state| state.generation).unwrap_or_default()
}

/// The families and sizes the theme is built with.
#[must_use]
pub fn theme_fonts(cx: &App) -> crate::theme::ThemeFonts {
    crate::theme::ThemeFonts {
        sans: effective_family_name(cx),
        mono: code_effective_family_name(cx),
        terminal: terminal_effective_family_name(cx),
        code_size: code_font_size(cx),
        terminal_size: terminal_font_size(cx),
    }
}

/// Apply and keep the interface family; whether the family in use changed.
pub fn set_family(family: UiFontFamily, cx: &mut App) -> bool {
    let Some(state) = state(cx) else { return false };
    if !state.availability.is_available(&family) {
        return false;
    }
    let effective = resolve_effective(&family, &state.availability);
    if state.requested == family && state.effective == effective {
        return false;
    }
    let effective_changed = state.effective != effective;
    let state = cx.global_mut::<TypographyState>();
    state.requested = family.clone();
    state.effective = effective;
    if effective_changed {
        state.generation = state.generation.wrapping_add(1);
        appearance::reinstall(cx);
    }
    appearance::persist(Change::UiFamily(family), cx);
    effective_changed
}

/// Apply and keep the terminal family; whether anything changed.
pub fn set_terminal_family(family: UiFontFamily, cx: &mut App) -> bool {
    set_category_family(family, Category::Terminal, cx)
}

/// Apply and keep the code family; whether anything changed.
pub fn set_code_family(family: UiFontFamily, cx: &mut App) -> bool {
    set_category_family(family, Category::Code, cx)
}

#[derive(Clone, Copy)]
enum Category {
    Terminal,
    Code,
}

fn set_category_family(family: UiFontFamily, category: Category, cx: &mut App) -> bool {
    let Some(state) = state(cx) else { return false };
    let fixed_width_only = matches!(category, Category::Terminal);
    let selectable = if fixed_width_only {
        state.availability.is_fixed_width_available(&family)
    } else {
        state.availability.is_available(&family)
    };
    if !selectable {
        return false;
    }
    let effective = resolve_effective_mono(&family, &state.availability, fixed_width_only);
    let (requested, current) = match category {
        Category::Terminal => (&state.terminal_requested, &state.terminal_effective),
        Category::Code => (&state.code_requested, &state.code_effective),
    };
    if *requested == family && *current == effective {
        return false;
    }
    let state = cx.global_mut::<TypographyState>();
    state.generation = state.generation.wrapping_add(1);
    match category {
        Category::Terminal => {
            state.terminal_requested = family.clone();
            state.terminal_effective = effective;
        }
        Category::Code => {
            state.code_requested = family.clone();
            state.code_effective = effective;
        }
    }
    appearance::reinstall(cx);
    appearance::persist(
        match category {
            Category::Terminal => Change::TerminalFamily(family),
            Category::Code => Change::CodeFamily(family),
        },
        cx,
    );
    true
}

/// Apply and keep the terminal's pixel size; whether it changed.
pub fn set_terminal_font_size(size: f32, cx: &mut App) -> bool {
    set_category_font_size(size, Category::Terminal, cx)
}

/// Apply and keep code's pixel size; whether it changed.
pub fn set_code_font_size(size: f32, cx: &mut App) -> bool {
    set_category_font_size(size, Category::Code, cx)
}

fn set_category_font_size(size: f32, category: Category, cx: &mut App) -> bool {
    let size = clamp_font_size(size);
    let Some(state) = state(cx) else { return false };
    let current = match category {
        Category::Terminal => state.terminal_font_size,
        Category::Code => state.code_font_size,
    };
    if (current - size).abs() < f32::EPSILON {
        return false;
    }
    let state = cx.global_mut::<TypographyState>();
    state.generation = state.generation.wrapping_add(1);
    match category {
        Category::Terminal => state.terminal_font_size = size,
        Category::Code => state.code_font_size = size,
    }
    appearance::reinstall(cx);
    appearance::persist(
        match category {
            Category::Terminal => Change::TerminalSize(size),
            Category::Code => Change::CodeSize(size),
        },
        cx,
    );
    true
}

/// Apply a UI size to `window` and keep it; whether it changed.
pub fn set_font_size(size: UiFontSize, window: &mut Window, cx: &mut App) -> bool {
    let size = size.normalized();
    let Some(state) = state(cx) else { return false };
    if state.size == size {
        return false;
    }
    let state = cx.global_mut::<TypographyState>();
    state.size = size;
    state.generation = state.generation.wrapping_add(1);
    window.set_rem_size(px(size.pixels()));
    crate::theme::bump_style_generation();
    cx.refresh_windows();
    appearance::persist(Change::UiSize(size), cx);
    true
}

/// Give `window` the person's rem size: call once a window opens.
pub fn apply_rem_size(window: &mut Window, cx: &App) {
    window.set_rem_size(px(font_size(cx).pixels()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_snaps_to_the_nearest_choice() {
        assert_eq!(UiFontSize::new(17).pixels(), 16.0, "17 snaps down");
        assert_eq!(UiFontSize::new(19).pixels(), 18.0, "19 snaps to 18");
        assert_eq!(UiFontSize::new(40).pixels(), 20.0, "past the largest");
    }

    #[test]
    fn a_family_round_trips_through_settings() {
        for family in [
            UiFontFamily::Geist,
            UiFontFamily::GeistMono,
            UiFontFamily::System,
            UiFontFamily::Installed("Menlo".to_owned()),
        ] {
            let json = serde_json::to_string(&family).expect("writes");
            assert_eq!(
                serde_json::from_str::<UiFontFamily>(&json).expect("reads"),
                family,
                "{json}"
            );
        }
        assert_eq!(
            serde_json::from_str::<UiFontFamily>("\"installed:\"").expect("reads"),
            UiFontFamily::Geist,
            "an empty installed name falls back"
        );
    }

    #[test]
    fn the_terminal_falls_back_to_the_bundled_mono() {
        let availability = FontAvailability::bundled();
        let proportional = UiFontFamily::Installed("Helvetica".to_owned());
        assert_eq!(
            resolve_effective_mono(&proportional, &availability, true),
            UiFontFamily::GeistMono,
            "a proportional face never reaches the grid"
        );
        assert_eq!(resolve_effective(&proportional, &availability), UiFontFamily::Geist, "ui");
    }

    /// Every Geist and Geist Mono face registers with the platform's text system and resolves
    /// by family, weight and slant.
    #[test]
    fn every_geist_face_resolves() {
        use gpui::{Font, FontFeatures, FontStyle, FontWeight};
        let system = gpui_platform::text_system();
        system
            .add_fonts(
                GEIST_FACES.iter().chain(&GEIST_MONO_FACES).map(|f| Cow::Borrowed(*f)).collect(),
            )
            .unwrap_or_else(|e| panic!("{e}"));
        for family in [GEIST, GEIST_MONO] {
            for weight in
                [FontWeight::NORMAL, FontWeight::MEDIUM, FontWeight::SEMIBOLD, FontWeight::BOLD]
            {
                for style in [FontStyle::Normal, FontStyle::Italic] {
                    let font = Font {
                        family: family.into(),
                        features: FontFeatures::default(),
                        fallbacks: None,
                        weight,
                        style,
                    };
                    let id = system
                        .font_id(&font)
                        .unwrap_or_else(|e| panic!("{family} {weight:?} {style:?}: {e}"));
                    assert_eq!(
                        system.font_metrics(id).units_per_em,
                        1000,
                        "{family} {weight:?} {style:?}"
                    );
                }
            }
        }
    }
}
