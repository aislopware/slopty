//! User settings: `<data dir>/settings.toml`.
//!
//! Every field has a default and the file may be absent. Unknown keys are reported as
//! warnings, never errors. A file that does not parse yields the defaults plus the error, so
//! the app never fails to start over a typo; the caller shows the error and the next save
//! heals it. Watching the file for changes is the app's job (`slopty-app`).

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name inside the data directory.
pub const FILE_NAME: &str = "settings.toml";

/// `$SLOPTY_DATA_DIR`, else `~/Library/Application Support/Slopty`. The same directory the
/// client identity and the host daemon use.
#[must_use]
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SLOPTY_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    home.join("Library").join("Application Support").join("Slopty")
}

/// `<data dir>/settings.toml`.
#[must_use]
pub fn path() -> PathBuf {
    path_in(&data_dir())
}

/// `settings.toml` inside `data_dir`.
#[must_use]
pub fn path_in(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// Which theme variant to use.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Always dark.
    Dark,
    /// Always light.
    Light,
    /// Follow the window's appearance (System Settings ▸ Appearance).
    #[default]
    System,
}

/// `[font]`.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Font {
    /// Monospace family for terminals. The bundled face is `JetBrains Mono`; any installed
    /// family works, with `SF Mono` and `Menlo` as fallbacks when it is missing.
    pub mono_family: String,
    /// Terminal font size in points.
    pub mono_size: f32,
    /// Chrome (top bar, pills, picker) font size in points.
    pub ui_size: f32,
}

impl Default for Font {
    fn default() -> Self {
        Self { mono_family: "JetBrains Mono".to_owned(), mono_size: 13.0, ui_size: 13.0 }
    }
}

/// `[theme]`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeSettings {
    /// `dark`, `light` or `system`.
    pub appearance: Appearance,
}

/// `[terminal]`.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    /// The least WCAG contrast ratio (1–21) between a cell's text and its background; text
    /// under it is painted black or white instead, whichever reads. `1.0` leaves every
    /// colour as the program set it (ghostty's `minimum-contrast`).
    pub minimum_contrast: f32,
    /// Copy a selection to the clipboard as soon as it is made (ghostty's
    /// `copy-on-select = clipboard`, iTerm2's default).
    pub copy_on_select: bool,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self { minimum_contrast: 1.0, copy_on_select: false }
    }
}

/// The whole file.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Fonts.
    pub font: Font,
    /// Theme.
    pub theme: ThemeSettings,
    /// Terminal behaviour.
    pub terminal: TerminalSettings,
}

/// Why a file could not be used.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// The file exists but could not be read.
    #[error("read {path}: {source}")]
    Read {
        /// The file.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid TOML for this schema.
    #[error("{path}: {message}")]
    Parse {
        /// The file.
        path: PathBuf,
        /// The parser's message, first line only (TOML errors carry a source excerpt).
        message: String,
    },
}

/// The outcome of loading: always usable settings, plus what went wrong on the way.
#[derive(Debug)]
pub struct Loaded {
    /// The settings to use (defaults when `error` is set).
    pub settings: Settings,
    /// Unknown keys, one message each.
    pub warnings: Vec<String>,
    /// Why the file was ignored, if it was.
    pub error: Option<SettingsError>,
}

impl Settings {
    /// Load `path`. Absent → defaults, no error. Unreadable or unparsable → defaults + error.
    #[must_use]
    pub fn load(path: &Path) -> Loaded {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Loaded { settings: Self::default(), warnings: Vec::new(), error: None };
            }
            Err(source) => {
                return Loaded {
                    settings: Self::default(),
                    warnings: Vec::new(),
                    error: Some(SettingsError::Read { path: path.to_path_buf(), source }),
                };
            }
        };
        let mut loaded = Self::parse(&text);
        if let Some(SettingsError::Parse { path: p, .. }) = &mut loaded.error {
            *p = path.to_path_buf();
        }
        loaded
    }

    /// Parse the file's text. The error's path is empty; [`Settings::load`] fills it in.
    #[must_use]
    pub fn parse(text: &str) -> Loaded {
        let table: toml::Table = match toml::from_str(text) {
            Ok(table) => table,
            Err(e) => return Loaded::broken(first_line(&e.to_string())),
        };
        let mut warnings = Vec::new();
        if let Ok(known) = toml::Table::try_from(Self::default()) {
            unknown_keys(&table, &known, "", &mut warnings);
        }
        match table.try_into::<Self>() {
            Ok(settings) => Loaded { settings, warnings, error: None },
            Err(e) => Loaded::broken(first_line(&e.to_string())),
        }
    }

    /// The default file: every key, its default value, one comment each. Parses back to
    /// [`Settings::default`].
    #[must_use]
    pub fn default_file() -> String {
        let d = Self::default();
        format!(
            "\
# Slopty settings. Saved changes apply while the app runs.
# Unknown keys are ignored; a file that does not parse is skipped
# and the defaults apply until the next save.

[font]
# Terminal family. \"JetBrains Mono\" ships with the app; any installed
# family works (SF Mono and Menlo fill in when it is missing).
mono_family = {mono_family}
# Terminal size in points.
mono_size = {mono_size}
# Top bar, pills and picker size in points.
ui_size = {ui_size}

[theme]
# \"dark\", \"light\" or \"system\" (follow the macOS appearance).
appearance = {appearance}

[terminal]
# Least contrast ratio (1.0 to 21.0) between text and its background;
# text under it turns black or white. 1.0 keeps every colour as set.
minimum_contrast = {minimum_contrast}
# Copy a selection to the clipboard as soon as it is made.
copy_on_select = {copy_on_select}
",
            mono_family = toml_string(&d.font.mono_family),
            mono_size = toml_float(d.font.mono_size),
            ui_size = toml_float(d.font.ui_size),
            appearance = toml_string(appearance_name(d.theme.appearance)),
            minimum_contrast = toml_float(d.terminal.minimum_contrast),
            copy_on_select = d.terminal.copy_on_select,
        )
    }

    /// Write [`Settings::default_file`] to `path` unless a file is already there. Returns
    /// whether a file was written.
    pub fn init(path: &Path) -> std::io::Result<bool> {
        if path.exists() {
            return Ok(false);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, Self::default_file())?;
        Ok(true)
    }
}

impl Loaded {
    fn broken(message: String) -> Self {
        Self {
            settings: Settings::default(),
            warnings: Vec::new(),
            error: Some(SettingsError::Parse { path: PathBuf::new(), message }),
        }
    }
}

/// Keys in `given` with no counterpart in `known`, recursively through tables.
fn unknown_keys(given: &toml::Table, known: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (key, value) in given {
        let full = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
        match known.get(key) {
            None => out.push(format!("unknown key `{full}`")),
            Some(toml::Value::Table(known_inner)) => {
                if let toml::Value::Table(inner) = value {
                    unknown_keys(inner, known_inner, &full, out);
                }
            }
            Some(_) => {}
        }
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or_default().trim().to_owned()
}

const fn appearance_name(a: Appearance) -> &'static str {
    match a {
        Appearance::Dark => "dark",
        Appearance::Light => "light",
        Appearance::System => "system",
    }
}

fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

fn toml_float(v: f32) -> String {
    // `13.0`, not `13`: a bare integer parses as an integer and fails the float field.
    let s = format!("{v}");
    if s.contains('.') { s } else { format!("{s}.0") }
}

#[cfg(test)]
#[expect(clippy::float_cmp, reason = "the values are literals parsed from text, not arithmetic")]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn absent_file_is_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = Settings::load(&path_in(dir.path()));
        assert_eq!(loaded.settings, Settings::default());
        assert!(loaded.error.is_none(), "no error for a missing file");
        assert!(loaded.warnings.is_empty(), "no warnings for a missing file");
    }

    #[test]
    fn defaults_match_the_brief() {
        let d = Settings::default();
        assert_eq!(d.font.mono_family, "JetBrains Mono");
        assert_eq!(d.font.mono_size, 13.0);
        assert_eq!(d.font.ui_size, 13.0);
        assert_eq!(d.theme.appearance, Appearance::System);
        assert_eq!(d.terminal.minimum_contrast, 1.0, "off, as ghostty");
        assert!(!d.terminal.copy_on_select, "\u{2318}C copies, as on the Mac");
    }

    #[test]
    fn terminal_keys() {
        let loaded = Settings::parse("[terminal]\nminimum_contrast = 3\ncopy_on_select = true\n");
        assert!(loaded.error.is_none(), "{:?}", loaded.error);
        assert_eq!(loaded.settings.terminal.minimum_contrast, 3.0);
        assert!(loaded.settings.terminal.copy_on_select);
        assert_eq!(loaded.settings.font, Font::default());
    }

    #[test]
    fn partial_file_fills_the_rest() {
        let loaded = Settings::parse("[font]\nmono_size = 15.5\n");
        assert!(loaded.error.is_none(), "{:?}", loaded.error);
        assert_eq!(loaded.settings.font.mono_size, 15.5);
        assert_eq!(loaded.settings.font.mono_family, "JetBrains Mono");
        assert_eq!(loaded.settings.theme.appearance, Appearance::System);
    }

    #[test]
    fn integer_sizes_are_accepted() {
        let loaded = Settings::parse("[font]\nmono_size = 14\nui_size = 12\n");
        assert!(loaded.error.is_none(), "{:?}", loaded.error);
        assert_eq!(loaded.settings.font.mono_size, 14.0);
        assert_eq!(loaded.settings.font.ui_size, 12.0);
    }

    #[test]
    fn appearance_values() {
        for (text, want) in [
            ("dark", Appearance::Dark),
            ("light", Appearance::Light),
            ("system", Appearance::System),
        ] {
            let loaded = Settings::parse(&format!("[theme]\nappearance = \"{text}\"\n"));
            assert_eq!(loaded.settings.theme.appearance, want, "{text}");
        }
        let bad = Settings::parse("[theme]\nappearance = \"sepia\"\n");
        assert!(bad.error.is_some(), "an unknown variant is an error");
        assert_eq!(bad.settings, Settings::default());
    }

    #[test]
    fn unknown_keys_warn_but_load() {
        let loaded = Settings::parse(
            "[font]\nmono_size = 11.0\nligatures = true\n[terminal]\nscrollback_lines = 1\n",
        );
        assert!(loaded.error.is_none(), "{:?}", loaded.error);
        assert_eq!(loaded.settings.font.mono_size, 11.0);
        assert_eq!(
            loaded.warnings,
            vec![
                "unknown key `font.ligatures`".to_owned(),
                "unknown key `terminal.scrollback_lines`".to_owned()
            ]
        );
    }

    #[test]
    fn broken_file_falls_back_with_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(dir.path());
        std::fs::write(&path, "[font\nmono_size = ").unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded.settings, Settings::default());
        let err = loaded.error.expect("a parse error");
        let text = err.to_string();
        assert!(text.starts_with(&path.display().to_string()), "{text}");
        assert_eq!(text.lines().count(), 1, "one line for the status bar: {text}");
    }

    #[test]
    fn wrong_type_falls_back_with_error() {
        let loaded = Settings::parse("[font]\nmono_size = \"big\"\n");
        assert!(loaded.error.is_some(), "a type error is an error");
        assert_eq!(loaded.settings, Settings::default());
    }

    #[test]
    fn default_file_round_trips() {
        let text = Settings::default_file();
        let loaded = Settings::parse(&text);
        assert!(loaded.error.is_none(), "{:?}", loaded.error);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(loaded.settings, Settings::default());
        assert!(text.contains("mono_size = 13.0"), "{text}");
        assert!(text.contains("appearance = \"system\""), "{text}");
        assert!(text.contains("minimum_contrast = 1.0"), "{text}");
        assert!(text.contains("copy_on_select = false"), "{text}");
    }

    #[test]
    fn init_writes_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(FILE_NAME);
        assert!(Settings::init(&path).unwrap(), "first init writes");
        std::fs::write(&path, "[font]\nmono_size = 9.0\n").unwrap();
        assert!(!Settings::init(&path).unwrap(), "second init keeps the file");
        assert_eq!(Settings::load(&path).settings.font.mono_size, 9.0);
    }

    #[test]
    fn data_dir_honours_override() {
        assert_eq!(path_in(Path::new("/tmp/x")), PathBuf::from("/tmp/x/settings.toml"));
    }
}
