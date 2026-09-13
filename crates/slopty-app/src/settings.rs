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
use slopty_settings::{Appearance, Color, ColorSettings, CursorBlink, Settings};
use slopty_theme::{Rgb, TerminalPalette, Theme, Variant};

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
/// Half the font's line height packs rows past reading; twice it is a list, not a grid.
const LINE_HEIGHT: std::ops::RangeInclusive<f32> = 0.5..=2.0;
/// WCAG ratios run from 1 (the same colour) to 21 (black on white).
const CONTRAST: std::ops::RangeInclusive<f32> = 1.0..=21.0;
/// Below 15 the stream is a slideshow; above 120 no display here refreshes.
const FPS: std::ops::RangeInclusive<u16> = 15..=120;
/// Under a megabit nothing decodes; 200 Mbit/s is past what one stream ever grows to.
const MBPS: std::ops::RangeInclusive<u16> = 1..=200;

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
    theme.typography.mono_line_height =
        sized(settings.font.mono_line_height, &LINE_HEIGHT, defaults.mono_line_height);
    theme.terminal.minimum_contrast = hundredths(settings.terminal.minimum_contrast);
    colour_the_terminal(&mut theme.terminal, &settings.colors);
    theme.behaviour.copy_on_select = settings.terminal.copy_on_select;
    theme.behaviour.paste_protection = settings.terminal.paste_protection;
    theme.behaviour.cursor_blink = match settings.terminal.cursor_blink {
        CursorBlink::Program => slopty_theme::CursorBlink::Program,
        CursorBlink::Always => slopty_theme::CursorBlink::Always,
        CursorBlink::Never => slopty_theme::CursorBlink::Never,
    };
    let remote = &settings.remote;
    let defaults = slopty_theme::StreamPrefs::default();
    theme.behaviour.stream = slopty_theme::StreamPrefs {
        fps: if FPS.contains(&remote.fps) { remote.fps } else { defaults.fps },
        max_bitrate_bps: if MBPS.contains(&remote.max_bitrate_mbps) {
            u32::from(remote.max_bitrate_mbps).saturating_mul(1_000_000)
        } else {
            defaults.max_bitrate_bps
        },
        hdr: remote.hdr,
    };
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

/// Lay the `[colors]` overrides over the theme's palette. Text under the cursor follows a
/// custom cursor (black or white, whichever reads) unless set itself.
fn colour_the_terminal(palette: &mut TerminalPalette, colors: &ColorSettings) {
    let rgb = |c: Color| c.0.map(|[r, g, b]| Rgb { r, g, b });
    if let Some(fg) = rgb(colors.foreground) {
        palette.fg = fg;
    }
    if let Some(bg) = rgb(colors.background) {
        palette.bg = bg;
    }
    if let Some(cursor) = rgb(colors.cursor) {
        palette.cursor = cursor;
        palette.cursor_text = if cursor.is_light() { Rgb::hex(0) } else { Rgb::hex(0xff_ffff) };
    }
    if let Some(cursor_text) = rgb(colors.cursor_text) {
        palette.cursor_text = cursor_text;
    }
    if let Some(selection) = rgb(colors.selection) {
        palette.selection = selection;
    }
    for (slot, color) in palette.ansi.iter_mut().zip(&colors.ansi) {
        if let Some(color) = rgb(*color) {
            *slot = color;
        }
    }
}

fn sized(v: f32, range: &std::ops::RangeInclusive<f32>, default: f32) -> f32 {
    if v.is_finite() && range.contains(&v) { v } else { default }
}

/// Whether a bell should sound the alert and bounce the Dock: only while the human is
/// elsewhere (no window of ours active), and only when the settings say so.
#[must_use]
pub const fn bell_alerts(settings: &Settings, window_active: bool) -> bool {
    settings.terminal.bell_alert && !window_active
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
        s.font.mono_line_height = 1.2;
        assert_eq!(theme_for(&s, true).typography.mono_line_height, 1.2);
        s.font.mono_line_height = 3.0;
        assert_eq!(theme_for(&s, true).typography.mono_line_height, 1.0, "a typo: the font's");
    }

    #[test]
    fn custom_colours_lay_over_the_theme() {
        let mut s = Settings::default();
        let dark = theme_for(&s, true).terminal;
        s.colors.foreground = Color(Some([0xc0, 0xca, 0xf5]));
        s.colors.cursor = Color(Some([0xff, 0xff, 0xff]));
        s.colors.ansi = vec![Color(None), Color(Some([0xf7, 0x76, 0x8e]))];
        let t = theme_for(&s, true).terminal;
        assert_eq!((t.fg, t.bg), (Rgb::hex(0x00c0_caf5), dark.bg), "set and unset");
        assert_eq!(
            (t.cursor, t.cursor_text),
            (Rgb::hex(0x00ff_ffff), Rgb::hex(0)),
            "black on a light cursor"
        );
        assert_eq!(
            (t.ansi[0], t.ansi[1], t.ansi[2]),
            (dark.ansi[0], Rgb::hex(0x00f7_768e), dark.ansi[2])
        );
        s.colors.cursor_text = Color(Some([1, 2, 3]));
        assert_eq!(
            theme_for(&s, true).terminal.cursor_text,
            Rgb { r: 1, g: 2, b: 3 },
            "set: as said"
        );
        let light = theme_for(&s, false).terminal;
        assert_eq!(light.fg, Rgb::hex(0x00c0_caf5), "both appearances");
    }

    #[test]
    fn a_bell_alerts_only_in_the_background_and_when_asked() {
        let mut s = Settings::default();
        assert!(bell_alerts(&s, false));
        assert!(!bell_alerts(&s, true), "in front of the window the flash is enough");
        s.terminal.bell_alert = false;
        assert!(!bell_alerts(&s, false));
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
        assert!(t.behaviour.paste_protection);
        s.terminal.paste_protection = false;
        assert!(!theme_for(&s, true).behaviour.paste_protection);
        s.terminal.cursor_blink = CursorBlink::Never;
        assert_eq!(theme_for(&s, true).behaviour.cursor_blink, slopty_theme::CursorBlink::Never);
        s.terminal.minimum_contrast = 0.0;
        assert_eq!(theme_for(&s, true).terminal.minimum_contrast, 100, "a typo reads as off");
        s.terminal.minimum_contrast = f32::INFINITY;
        assert_eq!(theme_for(&s, true).terminal.minimum_contrast, 100);
    }

    #[test]
    fn remote_settings_ride_on_the_theme() {
        let mut s = Settings::default();
        let stream = theme_for(&s, true).behaviour.stream;
        assert_eq!((stream.fps, stream.max_bitrate_bps, stream.hdr), (60, 30_000_000, false));
        s.remote.fps = 30;
        s.remote.max_bitrate_mbps = 8;
        s.remote.hdr = true;
        let stream = theme_for(&s, true).behaviour.stream;
        assert_eq!((stream.fps, stream.max_bitrate_bps, stream.hdr), (30, 8_000_000, true));
        s.remote.fps = 0;
        s.remote.max_bitrate_mbps = 500;
        let stream = theme_for(&s, true).behaviour.stream;
        assert_eq!((stream.fps, stream.max_bitrate_bps), (60, 30_000_000), "typos read as default");
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
