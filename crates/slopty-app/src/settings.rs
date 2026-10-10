//! Settings on the app side: the file → a [`Theme`], and a watcher that reloads it.
//!
//! The watcher looks at the file once every [`POLL`] on GPUI's executor, as every reader of
//! the file does ([`slopty_settings::follow`]).

use std::path::Path;

use gpui::WindowAppearance;
use slopty_settings::{
    Appearance, Color, Loaded, OptionAsAlt, Palette, SecureEntry, Settings, SettingsError, bounds,
};
use slopty_theme::{Density, Rgb, TerminalPalette, Theme, Variant};

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
            /// Open `settings.toml` in the in-app editor.
            OpenSettings,
            /// Open the settings on the Keyboard page: every command and its keys.
            OpenKeyboardShortcuts,
            /// Open the settings on the About page: the version, the build and the links.
            OpenAbout,
        ]
    );
}

pub use slopty_settings::follow::{POLL, Seen};

/// The text the in-app editor opens on: the file, or the commented defaults when there is
/// none (or it cannot be read; saving then overwrites it, which is what fixing it means).
#[must_use]
pub fn editable_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|_| Settings::default_file())
}

/// The editor's Save: parse `text`, and only when it holds write it to `path` and hand back
/// what was loaded (its unknown-key warnings included).
///
/// The error is the message shown under the field. `seen` takes the written file's stamp, so
/// the watcher does not load the app's own write a second time.
pub fn save(path: &Path, text: &str, seen: &mut Seen) -> Result<Loaded, String> {
    let mut loaded = Settings::parse(text);
    if let Some(error) = loaded.error.take() {
        // A parse error names no path (the text came from the field, not a file).
        return Err(match error {
            SettingsError::Parse { message, .. } => message,
            other @ SettingsError::Read { .. } => other.to_string(),
        });
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    // Replaced whole: the daemons and the other clients' watchers read this file too.
    slopty_platform::fs::replace(path, text.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    seen.saw(path);
    Ok(loaded)
}

/// `text` (a `settings.toml`) with the clipboard shared with the machine called `name` or not,
/// in `[clipboard] workers` however the file writes that map, every other line kept as it was.
///
/// # Errors
///
/// When `text` does not parse, or the edit does not read back as asked: editing it blind would
/// bury the mistake.
pub fn with_clipboard_shared(text: &str, name: &str, share: bool) -> Result<String, String> {
    if let Some(error) = Settings::parse(text).error {
        return Err(error.to_string());
    }
    let out =
        slopty_settings::edit::write_entry(text, "clipboard", "workers", name, &share.to_string());
    match Settings::parse(&out) {
        Loaded { error: None, settings, .. } if settings.clipboard.shared_with(name) == share => {
            Ok(out)
        }
        _ => Err(format!("could not set the clipboard for {name} in settings.toml")),
    }
}

/// Whether a window appearance is one of the dark ones.
#[must_use]
pub const fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

/// The chrome's density: a finger's 44 pt targets, or a pointer's compact rows.
#[must_use]
pub const fn density(touch: bool) -> Density {
    if touch { Density::TOUCH } else { Density::COMPACT }
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
    theme.typography.mono_size =
        sized(settings.font.mono_size, &bounds::MONO_SIZE, defaults.mono_size);
    theme.typography.ui_size = sized(settings.font.ui_size, &bounds::UI_SIZE, defaults.ui_size);
    theme.typography.prose_size =
        sized(settings.font.prose_size, &bounds::PROSE_SIZE, defaults.prose_size);
    theme.typography.mono_line_height =
        sized(settings.font.mono_line_height, &bounds::LINE_HEIGHT, defaults.mono_line_height);
    theme.typography.ligatures = settings.font.ligatures;
    theme.terminal.minimum_contrast = hundredths(settings.terminal.minimum_contrast);
    colour_the_terminal(&mut theme.terminal, settings.colors.for_dark(variant == Variant::Dark));
    theme.density = density(crate::TOUCH);
    theme.behaviour.copy_on_select = settings.terminal.copy_on_select;
    theme.behaviour.option_as_alt = match settings.terminal.option_as_alt {
        OptionAsAlt::False => slopty_theme::OptionAsAlt::False,
        OptionAsAlt::True => slopty_theme::OptionAsAlt::True,
        OptionAsAlt::Left => slopty_theme::OptionAsAlt::Left,
        OptionAsAlt::Right => slopty_theme::OptionAsAlt::Right,
    };
    theme.behaviour.secure_entry = match settings.terminal.secure_keyboard_entry {
        SecureEntry::Passwords => slopty_theme::SecureEntry::Passwords,
        SecureEntry::Always => slopty_theme::SecureEntry::Always,
        SecureEntry::Never => slopty_theme::SecureEntry::Never,
    };
    theme
}

/// A contrast ratio as the theme carries it: hundredths, a typo reads as the default.
fn hundredths(ratio: f32) -> u16 {
    let default = slopty_settings::TerminalSettings::default().minimum_contrast;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the ratio is clamped to 1..=21 first, so ×100 fits a u16 and is positive"
    )]
    let hundredths = (sized(ratio, &bounds::CONTRAST, default) * 100.0).round() as u16;
    hundredths
}

/// Lay one appearance's `[colors.light]` or `[colors.dark]` over the theme's palette. Text
/// under the cursor follows a custom cursor (black or white, whichever reads) unless set
/// itself.
fn colour_the_terminal(palette: &mut TerminalPalette, colors: &Palette) {
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

/// Whether a terminal's bell, or an agent needing the human, should sound the alert and
/// bounce the Dock.
///
/// As the settings say: by default only while the human is elsewhere (no window of ours
/// active; in front, the tile flashes, and the inbox and the badge say it).
#[must_use]
pub const fn alerts(settings: &Settings, window_active: bool) -> bool {
    settings.terminal.alert.sounds(window_active)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stopping the clipboard for one machine keeps every other line, sets it again in place,
    /// and leaves the rest shared; a name a bare key cannot hold is quoted, and set in place.
    #[test]
    fn the_clipboard_is_kept_off_for_one_machine_by_name() {
        let text = "# mine\n[clipboard]\nsync = true\n";
        let off = with_clipboard_shared(text, "studio", false).expect("set");
        assert!(off.starts_with("# mine\n[clipboard]\nsync = true\n"), "{off}");
        let read = Settings::parse(&off).settings.clipboard;
        assert!(!read.shared_with("studio") && read.shared_with("mini"), "{off}");
        let on = with_clipboard_shared(&off, "studio", true).expect("set again");
        assert_eq!(on.matches("studio").count(), 1, "in place: {on}");
        assert!(Settings::parse(&on).settings.clipboard.shared_with("studio"));
        let spaced = with_clipboard_shared(text, "Cong's Mac", false).expect("quoted");
        assert!(!Settings::parse(&spaced).settings.clipboard.shared_with("Cong's Mac"), "{spaced}");
        let again = with_clipboard_shared(&spaced, "Cong's Mac", true).expect("set again");
        assert_eq!(again.matches("Cong's Mac").count(), 1, "a quoted name in place too: {again}");
        assert!(with_clipboard_shared("[clipboard\n", "studio", false).is_err(), "broken file");
    }

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
        s.colors.dark.foreground = Color(Some([0xc0, 0xca, 0xf5]));
        s.colors.dark.cursor = Color(Some([0xff, 0xff, 0xff]));
        s.colors.dark.ansi = vec![Color(None), Color(Some([0xf7, 0x76, 0x8e]))];
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
        s.colors.dark.cursor_text = Color(Some([1, 2, 3]));
        assert_eq!(
            theme_for(&s, true).terminal.cursor_text,
            Rgb { r: 1, g: 2, b: 3 },
            "set: as said"
        );
        let light = theme_for(&s, false).terminal;
        let own = TerminalPalette::LIGHT;
        assert_eq!(
            (light.fg, light.cursor, light.ansi),
            (own.fg, own.cursor, own.ansi),
            "the dark palette leaves light alone"
        );
        s.colors.light.foreground = Color(Some([1, 2, 3]));
        let light = theme_for(&s, false).terminal;
        assert_eq!(light.fg, Rgb { r: 1, g: 2, b: 3 }, "light's own");
        assert_eq!(theme_for(&s, true).terminal.fg, Rgb::hex(0x00c0_caf5));
    }

    /// A background set in `[colors.dark]` repaints the terminal alone: the chrome and the
    /// variant stay the appearance's, even under a light background.
    #[test]
    fn a_custom_background_is_the_terminal_s_alone() {
        let mut s = Settings::default();
        let stock = theme_for(&s, true);
        s.colors.dark.background = Color(Some([0xfd, 0xf6, 0xe3]));
        let t = theme_for(&s, true);
        assert_eq!(t.terminal.bg, Rgb::hex(0x00fd_f6e3));
        assert_eq!((t.surfaces, t.elevation), (stock.surfaces, stock.elevation));
        assert_eq!(t.variant(), Variant::Dark, "the appearance decides");
    }

    /// The editor opens on the file, or the commented defaults; a text that does not parse
    /// is refused with the parser's line and leaves the file alone; one that does is written
    /// and comes back loaded, warnings and all.
    #[test]
    fn the_editor_saves_only_what_parses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("app").join("settings.toml");
        assert_eq!(editable_text(&path), Settings::default_file());
        let mut seen = Seen::of(&path);
        let refused =
            save(&path, "[font]\nmono_size = \"big\"\n", &mut seen).expect_err("a string size");
        assert!(refused.contains("\"big\""), "{refused}");
        assert!(!refused.starts_with(':'), "no empty path in front: {refused}");
        assert!(!path.exists());
        let loaded =
            save(&path, "[font]\nmono_size = 20\nkerning = true\n", &mut seen).expect("parses");
        assert_eq!(loaded.settings.font.mono_size, 20.0);
        assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
        assert_eq!(editable_text(&path), "[font]\nmono_size = 20\nkerning = true\n");
    }

    /// A bell or an agent that needs the person sounds the alert only in the background by
    /// default, never or always as the file says.
    #[test]
    fn an_alert_sounds_only_in_the_background_unless_asked() {
        let mut s = Settings::default();
        assert!(alerts(&s, false));
        assert!(!alerts(&s, true), "in front of the window the tile says it");
        s.terminal.alert = slopty_settings::Alert::Never;
        assert!(!alerts(&s, false), "never: the flash, the banner and the inbox only");
        s.terminal.alert = slopty_settings::Alert::Always;
        assert!(alerts(&s, true), "always: in front too");
    }

    #[test]
    fn terminal_settings_ride_on_the_theme() {
        let mut s = Settings::default();
        let t = theme_for(&s, true);
        assert_eq!(t.terminal.minimum_contrast, 300, "on by default");
        assert!(!t.behaviour.copy_on_select);
        s.terminal.minimum_contrast = 4.5;
        s.terminal.copy_on_select = true;
        let t = theme_for(&s, true);
        assert_eq!(t.terminal.minimum_contrast, 450);
        assert!(t.behaviour.copy_on_select);
        assert_eq!(t.behaviour.secure_entry, slopty_theme::SecureEntry::Passwords);
        s.terminal.secure_keyboard_entry = SecureEntry::Always;
        let secure = theme_for(&s, true).behaviour.secure_entry;
        assert_eq!(secure, slopty_theme::SecureEntry::Always);
        assert!(t.typography.ligatures);
        s.font.ligatures = false;
        assert!(!theme_for(&s, true).typography.ligatures);
        s.terminal.option_as_alt = OptionAsAlt::Left;
        assert_eq!(theme_for(&s, true).behaviour.option_as_alt, slopty_theme::OptionAsAlt::Left);
        s.terminal.minimum_contrast = 0.0;
        assert_eq!(theme_for(&s, true).terminal.minimum_contrast, 300, "a typo: the default");
        s.terminal.minimum_contrast = f32::INFINITY;
        assert_eq!(theme_for(&s, true).terminal.minimum_contrast, 300);
        s.terminal.minimum_contrast = 1.0;
        assert_eq!(theme_for(&s, true).terminal.minimum_contrast, 100, "1.0 turns it off");
    }

    /// Reading has its own size on the theme, and a size out of range reads as the default.
    #[test]
    fn the_reading_size_rides_on_the_theme() {
        let mut s = Settings::default();
        s.font.prose_size = 20.0;
        let typography = theme_for(&s, true).typography;
        assert_eq!((typography.prose(), typography.ui_size), (20.0, 13.0), "the chrome stays");
        s.font.prose_size = 2.0;
        assert_eq!(theme_for(&s, true).typography.prose(), 14.0);
    }

    /// The app's own save is not a change for the watcher, so its warnings are not shown twice;
    /// an edit from elsewhere is, once.
    #[test]
    fn the_watcher_sees_edits_from_elsewhere_and_not_the_apps_own_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let mut seen = Seen::of(&path);
        assert!(!seen.changed(&path), "nothing yet");
        let loaded = save(&path, "[font]\nnot_a_key = 1\n", &mut seen).unwrap();
        assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
        assert!(!seen.changed(&path), "the app's own save");
        std::fs::write(&path, "[font]\nnot_a_key = 22\n").unwrap();
        assert!(seen.changed(&path), "an edit from elsewhere");
        assert!(!seen.changed(&path), "seen once");
        save(&path, "[font\n", &mut seen).unwrap_err();
        assert!(!seen.changed(&path), "a save that did not parse wrote nothing");
    }

    /// A finger gets 44 pt targets and a pointer the compact rows; the Mac is always compact,
    /// and the touch build only once the chrome follows the density.
    #[test]
    fn a_finger_gets_the_touch_density() {
        assert_eq!(density(true), Density::TOUCH);
        assert_eq!(density(false), Density::COMPACT);
        assert_eq!(theme_for(&Settings::default(), true).density, density(crate::TOUCH));
        assert_eq!(theme_for(&Settings::default(), false).density, density(crate::TOUCH));
    }
}
