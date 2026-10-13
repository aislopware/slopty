// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Light and dark: what the person asked for, what the OS reports, and the repaint a change in
//! either makes.
//!
//! * [`AppearanceMode`]: the person's choice, follow the OS or pin one.
//! * [`AppearanceState`]: that choice beside the OS's last word, with the accent and material.
//! * [`observe_window`]: the platform's appearance notification, re-applied.
//!
//! Colours are read at paint time (`Theme::of(cx).text`), so a swap repaints every window with
//! [`App::refresh_windows`] rather than notifying a view. A confirmed choice is kept by the
//! hook the app installs ([`on_change`]): the kit knows nothing of the settings file.

use std::rc::Rc;

use gpui::{App, Global, Subscription, Window};
use serde::{Deserialize, Serialize};

use crate::theme::{AccentSelection, Appearance, SurfacePreference, Theme, appearance_of_window};
use crate::typography::{UiFontFamily, UiFontSize};

/// The person's appearance choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppearanceMode {
    /// Follow the OS.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

impl AppearanceMode {
    /// Every choice, in the picker's order.
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    /// Its label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// Its glyph, shared by every appearance control.
    #[must_use]
    pub const fn icon(self) -> &'static str {
        match self {
            Self::System => crate::solar::MONITOR,
            Self::Light => crate::solar::SUN,
            Self::Dark => crate::solar::MOON,
        }
    }
}

/// The state behind the theme: what the person chose and what the OS last said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppearanceState {
    /// The person's mode.
    pub mode: AppearanceMode,
    /// The OS's appearance.
    pub system: Appearance,
    /// The accent.
    pub accent: AccentSelection,
    /// The material.
    pub surface: SurfacePreference,
}

impl Global for AppearanceState {}

/// A confirmed change, for the app to keep.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// The appearance mode.
    Mode(AppearanceMode),
    /// The accent.
    Accent(AccentSelection),
    /// The material.
    Surface(SurfacePreference),
    /// The interface family.
    UiFamily(UiFontFamily),
    /// The interface size.
    UiSize(UiFontSize),
    /// The terminal family.
    TerminalFamily(UiFontFamily),
    /// The terminal's size.
    TerminalSize(f32),
    /// The code family.
    CodeFamily(UiFontFamily),
    /// Code's size.
    CodeSize(f32),
}

/// Where a confirmed change goes: the app keeps it in its settings.
#[derive(Clone)]
struct Keeper(Rc<Keep>);

/// The app's keeper of confirmed changes.
type Keep = dyn Fn(Change, &mut App);

impl Global for Keeper {}

/// Install where confirmed changes go.
pub fn on_change(keep: impl Fn(Change, &mut App) + 'static, cx: &mut App) {
    cx.set_global(Keeper(Rc::new(keep)));
}

/// Hand a confirmed change to the app.
pub fn persist(change: Change, cx: &mut App) {
    if let Some(Keeper(keep)) = cx.try_global::<Keeper>().cloned() {
        keep(change, cx);
    }
}

/// The person's choice combined with the OS's.
#[must_use]
pub const fn resolve(mode: AppearanceMode, system: Appearance) -> Appearance {
    match mode {
        AppearanceMode::System => system,
        AppearanceMode::Light => Appearance::Light,
        AppearanceMode::Dark => Appearance::Dark,
    }
}

/// Install the appearance state and the matching theme. Call once at boot, after
/// [`crate::typography::init`] and before any window opens, so the first frame is right.
pub fn init(
    mode: AppearanceMode,
    accent: AccentSelection,
    surface: SurfacePreference,
    cx: &mut App,
) {
    let system = appearance_of_window(cx.window_appearance());
    cx.set_global(AppearanceState { mode, system, accent, surface });
    install(cx);
}

/// The mode in effect.
#[must_use]
pub fn mode(cx: &App) -> AppearanceMode {
    cx.try_global::<AppearanceState>().map(|state| state.mode).unwrap_or_default()
}

/// The accent in effect.
#[must_use]
pub fn accent(cx: &App) -> AccentSelection {
    cx.try_global::<AppearanceState>().map(|state| state.accent).unwrap_or_default()
}

/// The material in effect.
#[must_use]
pub fn surface(cx: &App) -> SurfacePreference {
    cx.try_global::<AppearanceState>().map(|state| state.surface).unwrap_or_default()
}

fn update(cx: &mut App, change: impl FnOnce(&mut AppearanceState) -> Option<Change>) {
    let Some(state) = cx.try_global::<AppearanceState>().copied() else { return };
    let mut next = state;
    let Some(confirmed) = change(&mut next) else { return };
    if next == state {
        return;
    }
    cx.set_global(next);
    apply(cx);
    persist(confirmed, cx);
}

/// Change the mode, repaint if the palette moved, and keep it.
pub fn set_mode(mode: AppearanceMode, cx: &mut App) {
    update(cx, |state| {
        state.mode = mode;
        Some(Change::Mode(mode))
    });
}

/// Change the accent and keep it.
pub fn set_accent(accent: AccentSelection, cx: &mut App) {
    update(cx, |state| {
        state.accent = accent;
        Some(Change::Accent(accent))
    });
}

/// Change the material and keep it.
pub fn set_surface(surface: SurfacePreference, cx: &mut App) {
    update(cx, |state| {
        state.surface = surface;
        Some(Change::Surface(surface))
    });
}

/// Follow a window's appearance notifications; the subscription must outlive the window.
///
/// The window is asked first: before any window exists the platform's word on the appearance
/// is not reliable this early in launch.
pub fn observe_window(window: &Window, cx: &mut App) -> Subscription {
    sync(appearance_of_window(window.appearance()), cx);
    window.observe_window_appearance(|window, cx| {
        sync(appearance_of_window(window.appearance()), cx);
    })
}

fn sync(system: Appearance, cx: &mut App) {
    let Some(state) = cx.try_global::<AppearanceState>().copied() else { return };
    if state.system == system {
        return;
    }
    cx.set_global(AppearanceState { system, ..state });
    apply(cx);
}

fn wanted(cx: &App) -> Option<Theme> {
    let state = cx.try_global::<AppearanceState>()?;
    Some(Theme::new(
        resolve(state.mode, state.system),
        state.accent,
        state.surface,
        &crate::typography::theme_fonts(cx),
    ))
}

fn install(cx: &mut App) {
    if let Some(theme) = wanted(cx) {
        Theme::install(theme, cx);
    }
}

/// Re-resolve the palette and, if it moved, swap the theme and repaint every window; then
/// re-apply the window background, which keeps macOS vibrancy alive across swaps.
pub fn apply(cx: &mut App) {
    let Some(next) = wanted(cx) else { return };
    let changed = !cx.try_global::<Theme>().is_some_and(|theme| {
        theme.appearance == next.appearance
            && theme.accent_selection == next.accent_selection
            && theme.surface_preference == next.surface_preference
    });
    if changed {
        Theme::install(next, cx);
        cx.refresh_windows();
    }
    reapply_window_background(cx);
}

/// Build the theme again with the current fonts and repaint: a family or size changed.
pub fn reinstall(cx: &mut App) {
    crate::theme::bump_style_generation();
    install(cx);
    cx.refresh_windows();
}

/// Push the theme's window background onto every window, once each is back in the app.
pub fn reapply_window_background(cx: &mut App) {
    cx.defer(apply_window_background);
}

fn apply_window_background(cx: &mut App) {
    let Some(wanted) = cx.try_global::<Theme>().map(Theme::window_background_appearance) else {
        return;
    };
    for window in cx.windows() {
        if let Err(error) = window.update(cx, |_, window, _| {
            window.set_background_appearance(wanted);
        }) {
            tracing::warn!(%error, "the window background was not applied");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pinned_mode_overrides_the_system() {
        assert_eq!(
            resolve(AppearanceMode::System, Appearance::Light),
            Appearance::Light,
            "follows"
        );
        assert_eq!(resolve(AppearanceMode::Dark, Appearance::Light), Appearance::Dark, "pinned");
        assert_eq!(resolve(AppearanceMode::Light, Appearance::Dark), Appearance::Light, "pinned");
    }

    /// A change of mode installs the other palette and reaches the keeper once.
    #[gpui::test]
    fn a_change_repaints_and_is_kept(cx: &gpui::TestAppContext) {
        let _guard = crate::theme::lock_appearance();
        let kept = Rc::new(std::cell::RefCell::new(Vec::new()));
        cx.update(|cx| {
            let sink = Rc::clone(&kept);
            on_change(move |change, _| sink.borrow_mut().push(change), cx);
            init(
                AppearanceMode::Dark,
                AccentSelection::ThemeDefault,
                SurfacePreference::Opaque,
                cx,
            );
            assert_eq!(Theme::of(cx).appearance, Appearance::Dark, "dark first");
            set_mode(AppearanceMode::Light, cx);
            assert_eq!(Theme::of(cx).appearance, Appearance::Light, "then light");
            set_mode(AppearanceMode::Light, cx);
        });
        assert_eq!(*kept.borrow(), [Change::Mode(AppearanceMode::Light)], "kept once");
        crate::theme::set_current_appearance(Appearance::Dark);
    }
}
