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

/// Whether the terminal cursor blinks.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorBlink {
    /// The program decides (DECSCUSR): shells are steady, editors often blink.
    #[default]
    Program,
    /// Always.
    Always,
    /// Never.
    Never,
}

/// The terminal cursor's shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    /// The program decides (DECSCUSR).
    #[default]
    Program,
    /// A filled block.
    Block,
    /// A bar at the left edge.
    Bar,
    /// An underline.
    Underline,
}

/// Whether ⌥ is Alt (ghostty's `macos-option-as-alt`): a modifier that sends an escape
/// prefix, or the layout's key that types the symbol.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OptionAsAlt {
    /// The layout's key (`⌥b` types `∫`).
    #[default]
    False,
    /// Alt on both sides.
    True,
    /// Only the left key is Alt.
    Left,
    /// Only the right key is Alt.
    Right,
}

/// A colour as `"#rrggbb"` (or `"rrggbb"`), or `""` for the theme's own.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Color(pub Option<[u8; 3]>);

impl Color {
    /// Parse `"#rrggbb"`, `"rrggbb"` or the empty string.
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let hex = text.strip_prefix('#').unwrap_or(text);
        if hex.is_empty() {
            return Some(Self(None));
        }
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let byte = |at: usize| u8::from_str_radix(hex.get(at..at.checked_add(2)?)?, 16).ok();
        Some(Self(Some([byte(0)?, byte(2)?, byte(4)?])))
    }
}

impl Serialize for Color {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Some([r, g, b]) => serializer.serialize_str(&format!("#{r:02x}{g:02x}{b:02x}")),
            None => serializer.serialize_str(""),
        }
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| {
            serde::de::Error::custom(format!("expected \"#rrggbb\" or \"\", got {text:?}"))
        })
    }
}

/// `[colors]`: the terminal palette, each entry the theme's own unless set. They apply
/// to both appearances.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorSettings {
    /// Default text.
    pub foreground: Color,
    /// Default background.
    pub background: Color,
    /// The cursor block.
    pub cursor: Color,
    /// Text under the cursor block (black or white against `cursor` when unset).
    pub cursor_text: Color,
    /// Selection background.
    pub selection: Color,
    /// ANSI 0–15 in order; fewer than 16 leave the rest to the theme.
    pub ansi: Vec<Color>,
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
    /// Terminal line height as a multiple of the font's own (ghostty's `adjust-cell-height`):
    /// `1.0` is the font's, `1.2` airier, `0.9` tighter.
    pub mono_line_height: f32,
    /// Whether the terminal font's ligatures are shaped (`=>`, `!=` as one glyph in fonts
    /// that have them).
    pub ligatures: bool,
    /// Chrome (top bar, pills, picker) font size in points.
    pub ui_size: f32,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            mono_family: "JetBrains Mono".to_owned(),
            mono_size: 13.0,
            mono_line_height: 1.0,
            ligatures: true,
            ui_size: 13.0,
        }
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
    /// A bell while the app is in the background plays the alert sound and bounces the Dock;
    /// off, it only flashes the card.
    pub bell_alert: bool,
    /// Whether the cursor blinks (ghostty's `cursor-style-blink`): the program's choice, or
    /// always, or never.
    pub cursor_blink: CursorBlink,
    /// The cursor's shape (ghostty's `cursor-style`): the program's choice, or block, bar
    /// or underline.
    pub cursor_style: CursorStyle,
    /// A paste that could run commands (a newline into a shell that did not ask for
    /// bracketed paste) waits for a confirmation (ghostty's `clipboard-paste-protection`).
    pub paste_protection: bool,
    /// Bold text in ANSI 0–7 is painted in ANSI 8–15 (ghostty's `bold-is-bright`).
    pub bold_is_bright: bool,
    /// The pointer hides while typing into a terminal, until it moves.
    pub hide_pointer_while_typing: bool,
    /// What a wheel or trackpad line scrolls, in grid lines (ghostty's
    /// `mouse-scroll-multiplier`): `1.0` one for one, `3.0` fast.
    pub scroll_multiplier: f32,
    /// ⌥ as Alt (ghostty's `macos-option-as-alt`): `false` types the layout's symbol,
    /// `true` sends an escape prefix for readline's ⌥b/⌥f, `left`/`right` one side each.
    pub option_as_alt: OptionAsAlt,
    /// Closing a terminal whose command is still running asks first (ghostty's
    /// `confirm-close-surface`).
    pub confirm_close: bool,
    /// The Mac's line-editing keys in a shell (ghostty's macOS "natural text editing"
    /// keybinds): ⌘← ⌘→ ⌘⌫ ⌥← ⌥→ ⌥⌫ sent as readline's bytes.
    pub natural_editing: bool,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            minimum_contrast: 1.0,
            copy_on_select: false,
            bell_alert: true,
            cursor_blink: CursorBlink::Program,
            cursor_style: CursorStyle::Program,
            paste_protection: true,
            bold_is_bright: false,
            hide_pointer_while_typing: true,
            scroll_multiplier: 1.0,
            option_as_alt: OptionAsAlt::False,
            confirm_close: true,
            natural_editing: true,
        }
    }
}

/// `[remote]`: what a remote window or display stream asks the host for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteSettings {
    /// Frames per second the host captures and encodes at (15–120).
    pub fps: u16,
    /// The most the host may send per stream, in megabits per second (1–200): the ceiling
    /// its bitrate controller grows towards, never the rate it starts at.
    pub max_bitrate_mbps: u16,
    /// Encode 10-bit HEVC (Main 10) so an HDR source keeps its range; off, everything is
    /// 8-bit.
    pub hdr: bool,
    /// A stream opens silenced on this client; the title-bar pill still toggles it.
    pub muted: bool,
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self { fps: 60, max_bitrate_mbps: 30, hdr: false, muted: false }
    }
}

/// `[host]`: what `slopty-hostd` reads from the same file when it starts.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HostSettings {
    /// Address ranges (`100.64.0.0/10`, `fd00::/8`, a bare address) whose peers may connect,
    /// replacing the default of the tailnet and private LANs. Loopback is always admitted.
    pub allow: Vec<String>,
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
    /// Remote window and display streams.
    pub remote: RemoteSettings,
    /// Terminal colours.
    pub colors: ColorSettings,
    /// The host daemon.
    pub host: HostSettings,
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
# Terminal line height as a multiple of the font's own (0.5 to 2.0).
mono_line_height = {mono_line_height}
# Shape the terminal font's ligatures (=> != as one glyph, in fonts that have them).
ligatures = {ligatures}
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
# A bell while the app is in the background sounds the alert and bounces
# the Dock; off, the card only flashes.
bell_alert = {bell_alert}
# \"program\" (the shell or editor decides), \"always\" or \"never\".
cursor_blink = {cursor_blink}
# \"program\" (the shell or editor decides), \"block\", \"bar\" or \"underline\".
cursor_style = {cursor_style}
# A paste with a newline into a shell that did not ask for bracketed paste
# (so it would run) waits for a confirmation.
paste_protection = {paste_protection}
# Paint bold text in ANSI colours 0-7 with the bright 8-15.
bold_is_bright = {bold_is_bright}
# Hide the pointer while typing into a terminal, until it moves.
hide_pointer_while_typing = {hide_pointer_while_typing}
# Grid lines per wheel or trackpad line (0.1 to 10).
scroll_multiplier = {scroll_multiplier}
# Option as Alt: false types the layout's symbol (⌥b is ∫); true sends the
# escape prefix readline's ⌥b/⌥f want; \"left\" or \"right\" keep one side each.
option_as_alt = {option_as_alt}
# Closing a terminal whose command is still running asks first.
confirm_close = {confirm_close}
# ⌘← ⌘→ ⌘⌫ ⌥← ⌥→ ⌥⌫ edit the shell's line as the Mac's text fields do.
natural_editing = {natural_editing}

[remote]
# Frames per second a remote window or display is captured at (15 to 120).
fps = {fps}
# Ceiling for one stream in megabits per second (1 to 200); the host grows
# towards it as the link allows.
max_bitrate_mbps = {max_bitrate_mbps}
# 10-bit HEVC, for an HDR source.
hdr = {hdr}
# Open a stream with its audio silenced here; the title-bar pill still toggles it.
muted = {muted}

[colors]
# Terminal colours as \"#rrggbb\"; \"\" keeps the theme's own. They apply in
# both appearances.
foreground = \"\"
background = \"\"
cursor = \"\"
# Text under the cursor block; black or white against the cursor when unset.
cursor_text = \"\"
selection = \"\"
# ANSI 0-15 in order: black, red, green, yellow, blue, magenta, cyan, white,
# then their bright forms. Fewer than 16 keep the rest.
ansi = []

[host]
# Who may connect to the host daemon on this Mac, as address ranges
# (\"100.64.0.0/10\", \"fd00::/8\", \"192.168.1.20\"). Empty admits the tailnet
# (100.64.0.0/10, fd7a:115c:a1e0::/48) and private LANs (10/8, 172.16/12,
# 192.168/16, fc00::/7, link-local); a list replaces those. Loopback always
# connects. Traffic is not encrypted by Slopty: the VPN or tailnet is the
# boundary. Read when slopty-hostd starts.
allow = []
",
            mono_family = toml_string(&d.font.mono_family),
            mono_size = toml_float(d.font.mono_size),
            mono_line_height = toml_float(d.font.mono_line_height),
            ligatures = d.font.ligatures,
            ui_size = toml_float(d.font.ui_size),
            appearance = toml_string(appearance_name(d.theme.appearance)),
            minimum_contrast = toml_float(d.terminal.minimum_contrast),
            copy_on_select = d.terminal.copy_on_select,
            bell_alert = d.terminal.bell_alert,
            cursor_blink = toml_string(cursor_blink_name(d.terminal.cursor_blink)),
            cursor_style = toml_string(cursor_style_name(d.terminal.cursor_style)),
            paste_protection = d.terminal.paste_protection,
            bold_is_bright = d.terminal.bold_is_bright,
            hide_pointer_while_typing = d.terminal.hide_pointer_while_typing,
            scroll_multiplier = toml_float(d.terminal.scroll_multiplier),
            option_as_alt = toml_string(option_as_alt_name(d.terminal.option_as_alt)),
            confirm_close = d.terminal.confirm_close,
            natural_editing = d.terminal.natural_editing,
            fps = d.remote.fps,
            max_bitrate_mbps = d.remote.max_bitrate_mbps,
            hdr = d.remote.hdr,
            muted = d.remote.muted,
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

const fn cursor_style_name(c: CursorStyle) -> &'static str {
    match c {
        CursorStyle::Program => "program",
        CursorStyle::Block => "block",
        CursorStyle::Bar => "bar",
        CursorStyle::Underline => "underline",
    }
}

const fn option_as_alt_name(o: OptionAsAlt) -> &'static str {
    match o {
        OptionAsAlt::False => "false",
        OptionAsAlt::True => "true",
        OptionAsAlt::Left => "left",
        OptionAsAlt::Right => "right",
    }
}

const fn cursor_blink_name(c: CursorBlink) -> &'static str {
    match c {
        CursorBlink::Program => "program",
        CursorBlink::Always => "always",
        CursorBlink::Never => "never",
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
        assert_eq!(d.font.mono_line_height, 1.0, "the font's own");
        assert_eq!(d.font.ui_size, 13.0);
        assert_eq!(d.theme.appearance, Appearance::System);
        assert_eq!(d.terminal.minimum_contrast, 1.0, "off, as ghostty");
        assert!(!d.terminal.copy_on_select, "\u{2318}C copies, as on the Mac");
        assert!(d.terminal.bell_alert, "a bell in the background is heard");
        assert_eq!(d.terminal.cursor_blink, CursorBlink::Program, "DECSCUSR decides");
        assert_eq!(d.terminal.cursor_style, CursorStyle::Program, "and its shape");
        assert!(d.terminal.paste_protection, "a pasted newline asks first");
        assert!(!d.terminal.bold_is_bright, "bold is a weight, as in ghostty");
        assert!(d.terminal.hide_pointer_while_typing, "as Terminal.app");
        assert_eq!(d.terminal.scroll_multiplier, 1.0, "one for one");
        assert!(d.font.ligatures, "the font's own");
        assert_eq!((d.remote.fps, d.remote.max_bitrate_mbps, d.remote.hdr), (60, 30, false));
    }

    #[test]
    fn remote_keys() {
        let loaded =
            Settings::parse("[remote]\nfps = 30\nmax_bitrate_mbps = 8\nhdr = true\nmuted = true\n");
        assert!(loaded.error.is_none(), "{:?}", loaded.error);
        assert_eq!(
            loaded.settings.remote,
            RemoteSettings { fps: 30, max_bitrate_mbps: 8, hdr: true, muted: true }
        );
        assert!(!Settings::default().remote.muted, "sound on, as the host plays it");
    }

    #[test]
    fn colour_keys() {
        let loaded = Settings::parse(
            "[colors]\nforeground = \"#c0caf5\"\nbackground = \"1a1b26\"\ncursor = \"\"\nansi = [\"#15161e\", \"#f7768e\"]\n",
        );
        assert!(loaded.error.is_none(), "{:?}", loaded.error);
        let c = &loaded.settings.colors;
        assert_eq!(c.foreground, Color(Some([0xc0, 0xca, 0xf5])));
        assert_eq!(c.background, Color(Some([0x1a, 0x1b, 0x26])), "the # is optional");
        assert_eq!(c.cursor, Color(None), "empty: the theme's own");
        assert_eq!(c.ansi, [Color(Some([0x15, 0x16, 0x1e])), Color(Some([0xf7, 0x76, 0x8e]))]);
        assert_eq!(Settings::default().colors, ColorSettings::default(), "nothing set");
        for bad in ["#12345", "#gg0000", "red"] {
            let loaded = Settings::parse(&format!("[colors]\ncursor = \"{bad}\"\n"));
            assert!(
                loaded.error.as_ref().is_some_and(|e| e.to_string().contains("#rrggbb")),
                "{bad}: {:?}",
                loaded.error
            );
        }
        assert_eq!(
            toml::to_string(&ColorSettings {
                cursor: Color(Some([1, 0xab, 0xff])),
                ..ColorSettings::default()
            })
            .unwrap_or_default()
            .lines()
            .find(|l| l.starts_with("cursor ="))
            .unwrap_or_default(),
            "cursor = \"#01abff\"",
            "written back as #rrggbb"
        );
    }

    #[test]
    fn terminal_keys() {
        let loaded = Settings::parse(
            "[font]\nmono_line_height = 1.2\n[terminal]\nminimum_contrast = 3\ncopy_on_select = true\nbell_alert = false\ncursor_blink = \"never\"\npaste_protection = false\nbold_is_bright = true\nhide_pointer_while_typing = false\nscroll_multiplier = 3\nconfirm_close = false\nnatural_editing = false\n",
        );
        assert!(loaded.error.is_none(), "{:?}", loaded.error);
        assert!(!loaded.settings.terminal.confirm_close);
        assert!(!loaded.settings.terminal.natural_editing);
        assert_eq!(loaded.settings.font.mono_line_height, 1.2);
        assert_eq!(loaded.settings.terminal.minimum_contrast, 3.0);
        assert!(loaded.settings.terminal.copy_on_select);
        assert!(!loaded.settings.terminal.bell_alert);
        assert_eq!(loaded.settings.terminal.cursor_blink, CursorBlink::Never);
        assert!(!loaded.settings.terminal.paste_protection);
        assert!(loaded.settings.terminal.bold_is_bright);
        assert!(!loaded.settings.terminal.hide_pointer_while_typing);
        assert_eq!(loaded.settings.terminal.scroll_multiplier, 3.0);
        let loaded = Settings::parse("[font]\nligatures = false\n");
        assert!(!loaded.settings.font.ligatures);
        for (text, want) in [
            ("program", CursorStyle::Program),
            ("block", CursorStyle::Block),
            ("bar", CursorStyle::Bar),
            ("underline", CursorStyle::Underline),
        ] {
            let loaded = Settings::parse(&format!("[terminal]\ncursor_style = \"{text}\"\n"));
            assert_eq!(loaded.settings.terminal.cursor_style, want, "{text}");
        }
        for (text, want) in [
            ("false", OptionAsAlt::False),
            ("true", OptionAsAlt::True),
            ("left", OptionAsAlt::Left),
            ("right", OptionAsAlt::Right),
        ] {
            let loaded = Settings::parse(&format!("[terminal]\noption_as_alt = \"{text}\"\n"));
            assert_eq!(loaded.settings.terminal.option_as_alt, want, "{text}");
        }
        for (text, want) in [("program", CursorBlink::Program), ("always", CursorBlink::Always)] {
            let loaded = Settings::parse(&format!("[terminal]\ncursor_blink = \"{text}\"\n"));
            assert_eq!(loaded.settings.terminal.cursor_blink, want, "{text}");
        }
        assert!(
            Settings::parse("[terminal]\ncursor_blink = \"sometimes\"\n").error.is_some(),
            "an unknown variant is an error"
        );
        assert_eq!(loaded.settings.font.mono_family, Font::default().mono_family);
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
            "[font]\nmono_size = 11.0\nkerning = true\n[terminal]\nscrollback_lines = 1\n",
        );
        assert!(loaded.error.is_none(), "{:?}", loaded.error);
        assert_eq!(loaded.settings.font.mono_size, 11.0);
        assert_eq!(
            loaded.warnings,
            vec![
                "unknown key `font.kerning`".to_owned(),
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
        assert!(text.contains("max_bitrate_mbps = 30"), "{text}");
        assert!(text.contains("allow = []"), "{text}");
    }

    #[test]
    fn host_keys() {
        let loaded = Settings::parse("[host]\nallow = [\"100.64.0.3\", \"fd00::/8\"]\n");
        assert!(loaded.error.is_none() && loaded.warnings.is_empty(), "{loaded:?}");
        assert_eq!(loaded.settings.host.allow, ["100.64.0.3", "fd00::/8"]);
        assert!(Settings::default().host.allow.is_empty(), "the private ranges by default");
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
