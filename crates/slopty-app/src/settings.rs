//! Settings on the app side: the file → a [`Theme`], and a watcher that reloads it.
//!
//! The watcher polls the file's modification time and size once a second on GPUI's
//! executor instead of using `notify`. Editors save atomically (write a temp file, rename it
//! over the old one), which invalidates a per-file FSEvents/kqueue watch and forces watching
//! the directory and filtering; a 1 s `stat` handles create, replace and delete alike, costs
//! nothing, and adds no dependency to the iOS build.

use std::path::Path;
use std::time::{Duration, SystemTime};

use gpui::WindowAppearance;
use slopty_settings::{Appearance, Settings};
use slopty_theme::{Theme, Variant};

/// GPUI actions.
pub mod actions {
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    use gpui::actions;

    actions!(
        slopty,
        [
            /// Open `settings.toml` in the default editor (creating it first).
            OpenSettings,
        ]
    );
}

/// Poll period of the file watcher.
pub const POLL: Duration = Duration::from_secs(1);
/// Sizes outside this range are typos; the default applies instead.
const MONO_SIZE: std::ops::RangeInclusive<f32> = 6.0..=72.0;
const UI_SIZE: std::ops::RangeInclusive<f32> = 8.0..=32.0;
/// WCAG ratios run from 1 (the same colour) to 21 (black on white).
const CONTRAST: std::ops::RangeInclusive<f32> = 1.0..=21.0;

/// Whether a window appearance is one of the dark ones.
#[must_use]
pub const fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

/// The theme `settings` asks for, given whether the window is dark (used by `system`).
#[must_use]
pub fn theme_for(settings: &Settings, window_dark: bool) -> Theme {
    let variant = match settings.theme.appearance {
        Appearance::Dark => Variant::Dark,
        Appearance::System if window_dark => Variant::Dark,
        Appearance::Light | Appearance::System => Variant::Light,
    };
    let mut theme = Theme::new(variant);
    let defaults = Theme::default().typography;
    let family = settings.font.mono_family.trim();
    if !family.is_empty() {
        let mut families = vec![family.to_owned()];
        families.extend(defaults.mono_families.iter().filter(|f| *f != family).cloned());
        theme.typography.mono_families = families;
    }
    theme.typography.mono_size = sized(settings.font.mono_size, &MONO_SIZE, defaults.mono_size);
    theme.typography.ui_size = sized(settings.font.ui_size, &UI_SIZE, defaults.ui_size);
    theme.terminal.minimum_contrast = hundredths(settings.terminal.minimum_contrast);
    theme.behaviour.copy_on_select = settings.terminal.copy_on_select;
    theme
}

/// A contrast ratio as the theme carries it: hundredths, a typo reads as off.
fn hundredths(ratio: f32) -> u16 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the ratio is clamped to 1..=21 first, so ×100 fits a u16 and is positive"
    )]
    let hundredths = (sized(ratio, &CONTRAST, 1.0) * 100.0).round() as u16;
    hundredths
}

fn sized(v: f32, range: &std::ops::RangeInclusive<f32>, default: f32) -> f32 {
    if v.is_finite() && range.contains(&v) { v } else { default }
}

/// What the watcher compares between polls.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
    exists: bool,
}

impl Stamp {
    /// The file's current stamp (a missing file has one too, so deletion is a change).
    #[must_use]
    pub fn of(path: &Path) -> Self {
        std::fs::metadata(path).map_or_else(
            |_| Self::default(),
            |m| Self { modified: m.modified().ok(), len: m.len(), exists: true },
        )
    }
}

#[cfg(test)]
#[expect(clippy::float_cmp, reason = "the values are literals passed through, not arithmetic")]
mod tests {
    use super::*;

    #[test]
    fn family_leads_the_fallback_list() {
        let mut s = Settings::default();
        "Fira Code".clone_into(&mut s.font.mono_family);
        let t = theme_for(&s, true);
        assert_eq!(t.typography.mono_families, ["Fira Code", "JetBrains Mono", "SF Mono", "Menlo"]);
        let t = theme_for(&Settings::default(), true);
        assert_eq!(t.typography.mono_families, ["JetBrains Mono", "SF Mono", "Menlo"]);
    }

    #[test]
    fn appearance_resolution() {
        let mut s = Settings::default();
        assert_eq!(theme_for(&s, true).variant(), Variant::Dark);
        assert_eq!(theme_for(&s, false).variant(), Variant::Light);
        s.theme.appearance = Appearance::Dark;
        assert_eq!(theme_for(&s, false).variant(), Variant::Dark);
        s.theme.appearance = Appearance::Light;
        assert_eq!(theme_for(&s, true).variant(), Variant::Light);
    }

    #[test]
    fn absurd_sizes_fall_back() {
        let mut s = Settings::default();
        s.font.mono_size = 400.0;
        s.font.ui_size = f32::NAN;
        let t = theme_for(&s, true);
        assert_eq!(t.typography.mono_size, 13.0);
        assert_eq!(t.typography.ui_size, 13.0);
        s.font.mono_size = 16.0;
        assert_eq!(theme_for(&s, true).typography.mono_size, 16.0);
    }

    #[test]
    fn terminal_settings_ride_on_the_theme() {
        let mut s = Settings::default();
        let t = theme_for(&s, true);
        assert_eq!(t.terminal.minimum_contrast, 100, "off");
        assert!(!t.behaviour.copy_on_select);
        s.terminal.minimum_contrast = 4.5;
        s.terminal.copy_on_select = true;
        let t = theme_for(&s, true);
        assert_eq!(t.terminal.minimum_contrast, 450);
        assert!(t.behaviour.copy_on_select);
        s.terminal.minimum_contrast = 0.0;
        assert_eq!(theme_for(&s, true).terminal.minimum_contrast, 100, "a typo reads as off");
        s.terminal.minimum_contrast = f32::INFINITY;
        assert_eq!(theme_for(&s, true).terminal.minimum_contrast, 100);
    }

    #[test]
    fn stamp_tracks_writes_and_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let missing = Stamp::of(&path);
        std::fs::write(&path, "a").unwrap();
        let one = Stamp::of(&path);
        assert_ne!(missing, one);
        std::fs::write(&path, "ab").unwrap();
        assert_ne!(one, Stamp::of(&path));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(Stamp::of(&path), missing);
    }
}
