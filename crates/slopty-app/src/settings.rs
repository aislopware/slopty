//! Settings on the app side: the file's terminal behaviour on a [`Theme`], and a watcher that
//! reloads it. The look itself is the shell's (`crate::interface::wear`).
//!
//! The watcher looks at the file once every [`POLL`] on GPUI's executor, as every reader of
//! the file does ([`slopty_settings::follow`]).

use std::path::Path;

pub use slopty_settings::follow::{POLL, Seen};
use slopty_settings::{Loaded, OptionAsAlt, Settings, SettingsError, bounds};
use slopty_theme::{Theme, Variant};

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

/// `text` (a `settings.toml`) with the clipboard shared with `worker` or not, by its id in
/// `[clipboard] workers` however the file writes that map, every other line kept as it was.
///
/// # Errors
///
/// When `text` does not parse, or the edit does not read back as asked: editing it blind would
/// bury the mistake.
pub fn with_clipboard_shared(
    text: &str,
    worker: slopty_core::WorkerId,
    share: bool,
) -> Result<String, String> {
    if let Some(error) = Settings::parse(text).error {
        return Err(error.to_string());
    }
    let id = worker.to_string();
    let out =
        slopty_settings::edit::write_entry(text, "clipboard", "workers", &id, &share.to_string());
    match Settings::parse(&out) {
        Loaded { error: None, settings, .. } if settings.clipboard.shared_with(&id) == share => {
            Ok(out)
        }
        _ => Err(format!("could not set the clipboard for worker {id} in settings.toml")),
    }
}

/// The theme in the dark variant or the light one, with the terminal behaviour `settings` asks
/// for: its contrast floor, copy on select, Option as Alt and secure entry.
#[must_use]
pub fn theme_for(settings: &Settings, dark: bool) -> Theme {
    let mut theme = Theme::new(if dark { Variant::Dark } else { Variant::Light });
    theme.terminal.minimum_contrast = hundredths(settings.terminal.minimum_contrast);
    theme.behaviour.copy_on_select = settings.terminal.copy_on_select;
    theme.behaviour.option_as_alt = match settings.terminal.option_as_alt {
        OptionAsAlt::False => slopty_theme::OptionAsAlt::False,
        OptionAsAlt::True => slopty_theme::OptionAsAlt::True,
        OptionAsAlt::Left => slopty_theme::OptionAsAlt::Left,
        OptionAsAlt::Right => slopty_theme::OptionAsAlt::Right,
    };
    theme.behaviour.secure_entry = settings.terminal.secure_keyboard_entry;
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

fn sized(v: f32, range: &std::ops::RangeInclusive<f32>, default: f32) -> f32 {
    if v.is_finite() && range.contains(&v) { v } else { default }
}

/// Whether a terminal's bell, or an agent needing the human, should sound the alert and
/// bounce the Dock.
///
/// As the settings say: by default only while the human is elsewhere (no window of ours
/// active; in front, the terminal flashes, and the inbox and the badge say it).
#[must_use]
pub const fn alerts(settings: &Settings, window_active: bool) -> bool {
    settings.terminal.alert.sounds(window_active)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stopping the clipboard for one machine keeps every other line, writes it by its worker
    /// id, sets it again in place, and leaves the rest shared.
    #[test]
    fn the_clipboard_is_kept_off_for_one_machine_by_id() {
        let (studio, mini) = (slopty_core::WorkerId::new(), slopty_core::WorkerId::new());
        let (studio_id, mini_id) = (studio.to_string(), mini.to_string());
        let text = "# mine\n[clipboard]\nsync = true\n";
        let off = with_clipboard_shared(text, studio, false).expect("set");
        assert!(off.starts_with("# mine\n[clipboard]\nsync = true\n"), "{off}");
        let read = Settings::parse(&off).settings.clipboard;
        assert!(!read.shared_with(&studio_id) && read.shared_with(&mini_id), "{off}");
        let on = with_clipboard_shared(&off, studio, true).expect("set again");
        assert_eq!(on.matches(studio_id.as_str()).count(), 1, "in place: {on}");
        assert!(Settings::parse(&on).settings.clipboard.shared_with(&studio_id));
        assert!(with_clipboard_shared("[clipboard\n", studio, false).is_err(), "broken file");
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
        let refused = save(&path, "[terminal]\nminimum_contrast = \"big\"\n", &mut seen)
            .expect_err("a string");
        assert!(refused.contains("\"big\""), "{refused}");
        assert!(!refused.starts_with(':'), "no empty path in front: {refused}");
        assert!(!path.exists());
        let loaded = save(&path, "[terminal]\nminimum_contrast = 4\nkerning = true\n", &mut seen)
            .expect("parses");
        assert!((loaded.settings.terminal.minimum_contrast - 4.0).abs() < f32::EPSILON);
        assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
        assert_eq!(editable_text(&path), "[terminal]\nminimum_contrast = 4\nkerning = true\n");
    }

    /// A bell or an agent that needs the person sounds the alert only in the background by
    /// default, never or always as the file says.
    #[test]
    fn an_alert_sounds_only_in_the_background_unless_asked() {
        let mut s = Settings::default();
        assert!(alerts(&s, false));
        assert!(!alerts(&s, true), "in front of the window the terminal says it");
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
        assert!(t.behaviour.secure_entry, "at passwords by default");
        s.terminal.secure_keyboard_entry = false;
        assert!(!theme_for(&s, true).behaviour.secure_entry);
        s.terminal.option_as_alt = OptionAsAlt::Left;
        assert_eq!(theme_for(&s, true).behaviour.option_as_alt, slopty_theme::OptionAsAlt::Left);
        s.terminal.minimum_contrast = 0.0;
        assert_eq!(theme_for(&s, true).terminal.minimum_contrast, 300, "a typo: the default");
        s.terminal.minimum_contrast = f32::INFINITY;
        assert_eq!(theme_for(&s, true).terminal.minimum_contrast, 300);
        s.terminal.minimum_contrast = 1.0;
        assert_eq!(theme_for(&s, true).terminal.minimum_contrast, 100, "1.0 turns it off");
    }

    /// The app's own save is not a change for the watcher, so its warnings are not shown twice;
    /// an edit from elsewhere is, once.
    #[test]
    fn the_watcher_sees_edits_from_elsewhere_and_not_the_apps_own_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let mut seen = Seen::of(&path);
        assert!(!seen.changed(&path), "nothing yet");
        let loaded = save(&path, "[terminal]\nnot_a_key = 1\n", &mut seen).unwrap();
        assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
        assert!(!seen.changed(&path), "the app's own save");
        std::fs::write(&path, "[terminal]\nnot_a_key = 22\n").unwrap();
        assert!(seen.changed(&path), "an edit from elsewhere");
        assert!(!seen.changed(&path), "seen once");
        save(&path, "[terminal\n", &mut seen).unwrap_err();
        assert!(!seen.changed(&path), "a save that did not parse wrote nothing");
    }
}
