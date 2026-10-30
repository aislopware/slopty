// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Light/dark switching: what the person asked for, the application's appearance,
//! and the plumbing that turns a change in either into a repaint.
//!
//! 1. [`AppearanceMode`] — the persisted choice: follow the system, or pin one.
//! 2. [`AppearanceState`] — a gpui global holding that choice and the theme's.
//! 3. The application's appearance (`App::window_appearance`): Slopty pins it to a
//!    pinned mode and clears it to follow the system, so it is the appearance the
//!    theme takes; `App::on_window_appearance_change` tells when it moves, the
//!    system turning dark at sunset included, and [`apply`] follows.
//!
//! The theme follows the application, never a window: a window wears the
//! appearance of its terminal's ground ([`crate::window_appearance`]), so a dark
//! program in a light app darkens its window and leaves the theme light. Ghostty
//! reads `NSApp.effectiveAppearance` for its colour scheme for the same reason.
//!
//! # Why `refresh_windows` and not `notify`
//!
//! Colors are read *imperatively* (`Theme::of(cx).text`) at paint time, not
//! through a reactive binding, so no view knows its colors went stale — a
//! `notify()` on some entity would repaint that entity and nothing else.
//! [`App::refresh_windows`] marks every window dirty *and* disables gpui's
//! per-view prepaint cache for the frame, which is the only thing that forces
//! already-laid-out elements to re-run their paint with the new palette.

use gpui::{App, Global};
use serde::{Deserialize, Serialize};
use slopty_shell_theme::{AccentSelection, SurfacePreference, ThemeSelection};

use crate::settings::{self, SavePolicy};
use crate::theme::{Appearance, Theme};

/// The user's appearance preference. Persisted in `ui-settings.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppearanceMode {
    /// Follow the OS. The default — matches every other native app on the
    /// machine, including when the user has macOS set to switch at sunset.
    #[default]
    System,
    Light,
    Dark,
}

impl AppearanceMode {
    /// Menu/label text.
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// Shared glyph for appearance controls throughout the app.
    pub fn icon(self) -> &'static str {
        match self {
            Self::System => crate::icons::MONITOR,
            Self::Light => crate::icons::SUN,
            Self::Dark => crate::icons::MOON,
        }
    }

    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];
}

/// Global state behind the current theme: what the person chose, and the
/// application's appearance as the theme last took it. Kept separate from
/// [`Theme`] itself, which is what the two resolve to.
pub struct AppearanceState {
    pub mode: AppearanceMode,
    pub app: Appearance,
    pub themes: ThemeSelection,
    pub accent: AccentSelection,
    pub surface: SurfacePreference,
}

impl Global for AppearanceState {}

/// Combine the person's choice with the system's appearance.
pub fn resolve(mode: AppearanceMode, system: Appearance) -> Appearance {
    match mode {
        AppearanceMode::System => system,
        AppearanceMode::Light => Appearance::Light,
        AppearanceMode::Dark => Appearance::Dark,
    }
}

/// Install the appearance globals and the matching theme. Call once at boot,
/// before any window opens, so the first frame is already the right palette
/// (installing later produces a visible dark-to-light flash).
pub fn init(
    mode: AppearanceMode,
    themes: ThemeSelection,
    accent: AccentSelection,
    surface: SurfacePreference,
    cx: &mut App,
) {
    cx.set_window_appearance(app_appearance(mode));
    let app = Appearance::from_window(cx.window_appearance());
    cx.set_global(AppearanceState {
        mode,
        app,
        themes: themes.clone(),
        accent,
        surface,
    });
    // For as long as the app runs.
    cx.on_window_appearance_change(follow_app).detach();
    let appearance = resolve(mode, app);
    tracing::debug!(?mode, ?app, ?appearance, "appearance: initial");
    Theme::install_selection(
        appearance,
        themes.variant_id(model_appearance(appearance)),
        accent,
        surface,
        cx,
    );
}

/// The application's appearance as the theme last took it: the pinned mode's
/// where the platform pins it (macOS), else the system's. A window's own
/// appearance is measured against it ([`crate::window_appearance`]).
pub fn app(cx: &App) -> Appearance {
    cx.try_global::<AppearanceState>()
        .map_or_else(|| Appearance::from_window(cx.window_appearance()), |state| state.app)
}

/// The mode currently in effect (defaults to `System` before [`init`]).
pub fn mode(cx: &App) -> AppearanceMode {
    cx.try_global::<AppearanceState>()
        .map(|s| s.mode)
        .unwrap_or_default()
}

pub fn themes(cx: &App) -> ThemeSelection {
    cx.try_global::<AppearanceState>()
        .map(|state| state.themes.clone())
        .unwrap_or_default()
}

pub fn accent(cx: &App) -> AccentSelection {
    cx.try_global::<AppearanceState>()
        .map(|state| state.accent)
        .unwrap_or_default()
}

pub fn surface(cx: &App) -> SurfacePreference {
    cx.try_global::<AppearanceState>()
        .map(|state| state.surface)
        .unwrap_or_default()
}

/// Change the user's preference, repaint if that changed the palette, and write
/// the choice to disk.
pub fn set_mode(mode: AppearanceMode, cx: &mut App) {
    if !cx.has_global::<AppearanceState>() {
        return;
    }
    let state = cx.global_mut::<AppearanceState>();
    if state.mode == mode {
        return;
    }
    state.mode = mode;
    cx.set_window_appearance(app_appearance(mode));
    follow_app(cx);
    settings::update(SavePolicy::Immediate, cx, |settings| {
        settings.appearance = mode;
    });
}

/// Change the interactive accent without changing any semantic theme roles.
pub fn set_theme(appearance: Appearance, variant_id: impl Into<String>, cx: &mut App) {
    if !cx.has_global::<AppearanceState>() {
        return;
    }
    let variant_id = variant_id.into();
    let state = cx.global_mut::<AppearanceState>();
    if state.themes.variant_id(model_appearance(appearance)) == variant_id {
        return;
    }
    state
        .themes
        .set_variant(model_appearance(appearance), variant_id);
    let themes = state.themes.clone();
    apply(cx);
    settings::update(SavePolicy::Immediate, cx, |settings| {
        settings.theme_selection = themes;
    });
}

pub fn set_accent(accent: AccentSelection, cx: &mut App) {
    if !cx.has_global::<AppearanceState>() {
        return;
    }
    let state = cx.global_mut::<AppearanceState>();
    if state.accent == accent {
        return;
    }
    state.accent = accent;
    apply(cx);
    settings::update(SavePolicy::Immediate, cx, |settings| {
        settings.accent = accent;
    });
}

/// Change glass independently from appearance, theme, and accent selections.
pub fn set_surface(surface: SurfacePreference, cx: &mut App) {
    if !cx.has_global::<AppearanceState>() {
        return;
    }
    let state = cx.global_mut::<AppearanceState>();
    if state.surface == surface {
        return;
    }
    state.surface = surface;
    apply(cx);
    settings::update(SavePolicy::Immediate, cx, |settings| {
        settings.surface = surface;
    });
}

/// The application's appearance for `mode`: a pinned mode's, so what AppKit draws
/// (the traffic lights above all) matches the palette Slopty paints, or none, so
/// AppKit follows the system and the theme with it.
fn app_appearance(mode: AppearanceMode) -> Option<gpui::WindowAppearance> {
    match mode {
        AppearanceMode::System => None,
        AppearanceMode::Light => Some(gpui::WindowAppearance::Light),
        AppearanceMode::Dark => Some(gpui::WindowAppearance::Dark),
    }
}

/// Records `app` as the application's appearance, and resolves the appearance
/// the theme takes: the pinned one, else the system's, which the application's
/// is while Slopty pins none (on iOS, which pins none, it is the system's in
/// every mode, and [`resolve`] ignores it for a pinned one).
fn take_app(app: Appearance, cx: &mut App) -> Option<Appearance> {
    let state = cx.try_global::<AppearanceState>()?;
    let mode = state.mode;
    if state.app != app {
        cx.global_mut::<AppearanceState>().app = app;
    }
    Some(resolve(mode, app))
}

/// Re-resolve the palette and, if it moved, swap the theme and force a full
/// repaint. A no-op when the resolved appearance is unchanged.
pub fn apply(cx: &mut App) {
    if let Some(app) = cx.try_global::<AppearanceState>().map(|state| state.app) {
        apply_from(app, cx);
    }
}

/// [`apply`] with the application's appearance read again, as it turned: the
/// system's light and dark, or the person's mode pinning or clearing it. A
/// no-op when that leaves the resolved appearance as it was, so the system
/// turning under a pinned mode repaints nothing.
fn follow_app(cx: &mut App) {
    apply_from(Appearance::from_window(cx.window_appearance()), cx);
}

/// [`follow_app`], with the application's appearance read from `window` as it opens.
/// A window that just opened has no appearance of its own yet, so it wears the
/// application's, and it has it even early in launch, when
/// `NSApp.effectiveAppearance` may not be populated: read from there, the app
/// booted dark and turned light at the next change.
pub fn reconcile(window: &gpui::Window, cx: &mut App) {
    apply_from(Appearance::from_window(window.appearance()), cx);
}

fn apply_from(app: Appearance, cx: &mut App) {
    let Some(wanted) = take_app(app, cx) else {
        return;
    };
    let state = cx.global::<AppearanceState>();
    let accent = state.accent;
    let surface = state.surface;
    let variant_id = state.themes.variant_id(model_appearance(wanted)).to_owned();
    let changed = !cx.try_global::<Theme>().is_some_and(|theme| {
        theme.appearance == wanted
            && theme.variant_id.as_ref() == variant_id
            && theme.accent_selection == accent
            && theme.surface_preference == surface
    });
    if changed {
        tracing::debug!(?wanted, %variant_id, "appearance: installing palette");
        Theme::install_selection(wanted, &variant_id, accent, surface, cx);
        cx.refresh_windows();
    }
    // Unconditional, even when the palette did not move: this is the only thing
    // that keeps macOS vibrancy alive. gpui's macOS backend removes the
    // `NSVisualEffectView` from the window the moment the background appearance
    // is anything but `Blurred`, and nothing puts it back on its own — so a
    // single missed re-apply leaves the sidebar and tab strip permanently
    // opaque, which is exactly how the frost died. zed runs the same loop on
    // every settings change (`crates/zed/src/main.rs`).
    reapply_window_background(cx);
}

/// Rebuild the current palette after the installed custom registry changes.
/// Unlike [`apply`], this deliberately reinstalls even when the selected id is
/// unchanged because a linked theme may have recompiled under that same id.
pub fn apply_registry_change(cx: &mut App) {
    let Some(state) = cx.try_global::<AppearanceState>() else {
        return;
    };
    let wanted = resolve(state.mode, state.app);
    let accent = state.accent;
    let surface = state.surface;
    let variant_id = state.themes.variant_id(model_appearance(wanted)).to_owned();
    Theme::reinstall_selection(wanted, &variant_id, accent, surface, cx);
    cx.refresh_windows();
    reapply_window_background(cx);
}

fn model_appearance(appearance: Appearance) -> slopty_shell_theme::Appearance {
    match appearance {
        Appearance::Dark => slopty_shell_theme::Appearance::Dark,
        Appearance::Light => slopty_shell_theme::Appearance::Light,
    }
}

/// Push the theme's window background appearance onto every open window.
pub fn reapply_window_background(cx: &mut App) {
    // The window handling a settings click is temporarily taken out of App,
    // so updating it synchronously fails — silently, leaving its native
    // backdrop (macOS blur view, Windows Acrylic) on the old surface until a
    // restart. Defer until every window is back, on every platform.
    cx.defer(apply_window_background);
}

fn apply_window_background(cx: &mut App) {
    let Some(wanted) = cx
        .try_global::<Theme>()
        .map(|theme| theme.window_background_appearance())
    else {
        return;
    };
    for window in cx.windows() {
        if let Err(error) = window.update(cx, |_, window, _| {
            window.set_background_appearance(wanted);
        }) {
            tracing::warn!(%error, "appearance: window background not applied");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_mode_follows_the_os() {
        assert_eq!(
            resolve(AppearanceMode::System, Appearance::Light),
            Appearance::Light
        );
        assert_eq!(
            resolve(AppearanceMode::System, Appearance::Dark),
            Appearance::Dark
        );
    }

    #[test]
    fn pinned_modes_ignore_the_os() {
        for system in [Appearance::Light, Appearance::Dark] {
            assert_eq!(resolve(AppearanceMode::Light, system), Appearance::Light);
            assert_eq!(resolve(AppearanceMode::Dark, system), Appearance::Dark);
        }
    }

    #[test]
    fn default_mode_is_system() {
        assert_eq!(AppearanceMode::default(), AppearanceMode::System);
    }

    /// The setting round-trips through the settings file as a lowercase string.
    #[test]
    fn mode_serialises_stably() {
        for (mode, json) in [
            (AppearanceMode::System, "\"system\""),
            (AppearanceMode::Light, "\"light\""),
            (AppearanceMode::Dark, "\"dark\""),
        ] {
            assert_eq!(serde_json::to_string(&mode).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<AppearanceMode>(json).unwrap(),
                mode,
                "{json} should parse back"
            );
        }
    }
}
