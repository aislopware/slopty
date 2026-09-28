//! User settings: `<data dir>/settings.toml`.
//!
//! Every field has a default and the file may be absent. Unknown keys are reported as
//! warnings, never errors. A file that does not parse yields the defaults plus the error, so
//! the app never fails to start over a typo; the caller shows the error and the next save
//! heals it. Watching the file for changes is the app's job (`slopty-app`).

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
pub use slopty_net::HostAddr;

pub mod edit;
pub mod schema;

/// What the app takes of each number, outside of which a value is a typo and its default
/// applies. The schema's `range` reads these, so the form's steppers and the app's check agree.
pub mod bounds {
    use std::ops::RangeInclusive;

    /// Terminal font size, in points.
    pub const MONO_SIZE: RangeInclusive<f32> = 6.0..=72.0;
    /// Chrome font size, in points.
    pub const UI_SIZE: RangeInclusive<f32> = 8.0..=32.0;
    /// Half the font's line height packs rows past reading; twice it is a list, not a grid.
    pub const LINE_HEIGHT: RangeInclusive<f32> = 0.5..=2.0;
    /// A tenth of a line per wheel line is glacial; ten is a page.
    pub const SCROLL: RangeInclusive<f32> = 0.1..=10.0;
    /// WCAG ratios run from 1 (the same colour) to 21 (black on white).
    pub const CONTRAST: RangeInclusive<f32> = 1.0..=21.0;
    /// Below 15 the stream is a slideshow; above 120 no display here refreshes.
    pub const FPS: RangeInclusive<u16> = 15..=120;
    /// Under a megabit nothing decodes; 200 Mbit/s is past what one stream ever grows to.
    pub const MBPS: RangeInclusive<u16> = 1..=200;
}

/// File name inside the data directory.
pub const FILE_NAME: &str = "settings.toml";

/// `settings.toml` in the platform's data directory (`slopty_platform::dirs::data_dir`).
#[must_use]
pub fn path() -> PathBuf {
    path_in(&slopty_platform::dirs::data_dir())
}

/// `settings.toml` inside `data_dir`.
#[must_use]
pub fn path_in(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// Which theme variant to use.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Follow the window's appearance (System Settings ▸ Appearance).
    #[default]
    #[schemars(title = "System")]
    System,
    /// Always light.
    #[schemars(title = "Light")]
    Light,
    /// Always dark.
    #[schemars(title = "Dark")]
    Dark,
}

/// Whether the terminal cursor blinks.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CursorBlink {
    /// The program decides (DECSCUSR): shells are steady, editors often blink.
    #[default]
    #[schemars(title = "Auto")]
    Program,
    /// Always.
    #[schemars(title = "Always")]
    Always,
    /// Never.
    #[schemars(title = "Never")]
    Never,
}

/// The terminal cursor's shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    /// The program decides (DECSCUSR).
    #[default]
    #[schemars(title = "Auto")]
    Program,
    /// A filled block.
    #[schemars(title = "Block")]
    Block,
    /// A bar at the left edge.
    #[schemars(title = "Bar")]
    Bar,
    /// An underline.
    #[schemars(title = "Underline")]
    Underline,
}

/// Whether ⌥ is Alt (ghostty's `macos-option-as-alt`): a modifier that sends an escape
/// prefix, or the layout's key that types the symbol.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OptionAsAlt {
    /// The layout's key (`⌥b` types `∫`).
    #[default]
    #[schemars(title = "Off")]
    False,
    /// Alt on both sides.
    #[schemars(title = "Both")]
    True,
    /// Only the left key is Alt.
    #[schemars(title = "Left")]
    Left,
    /// Only the right key is Alt.
    #[schemars(title = "Right")]
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

impl JsonSchema for Color {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Color".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "format": schema::COLOR,
            "pattern": "^#?([0-9a-fA-F]{6})?$",
        })
    }
}

/// `[colors]`: the terminal palette, each entry the theme's own unless set. They apply
/// to both appearances.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(title = "Terminal colours")]
pub struct ColorSettings {
    /// Empty keeps the theme's own, in both appearances.
    ///
    /// Default text.
    #[schemars(title = "Text")]
    pub foreground: Color,
    /// The chrome's surfaces follow it.
    ///
    /// Default background.
    #[schemars(title = "Background")]
    pub background: Color,
    /// The block, bar or underline.
    #[schemars(title = "Cursor")]
    pub cursor: Color,
    /// Black or white against the cursor when empty.
    ///
    /// Text under the cursor block.
    #[schemars(title = "Text under the cursor")]
    pub cursor_text: Color,
    /// Behind selected text.
    #[schemars(title = "Selection")]
    pub selection: Color,
    /// Black to white, then their bright forms; fewer than 16 keep the rest.
    ///
    /// ANSI 0–15 in order; fewer than 16 leave the rest to the theme.
    #[schemars(title = "ANSI colours", example = ["#15161e", "#f7768e"])]
    pub ansi: Vec<Color>,
}

/// `[font]`.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(title = "Font")]
pub struct Font {
    /// `JetBrains Mono` ships with the app; any installed monospace works.
    ///
    /// Monospace family for terminals. The bundled face is `JetBrains Mono`; any installed
    /// family works, with `SF Mono` and `Menlo` as fallbacks when it is missing.
    #[schemars(title = "Family", extend("format" = schema::FONT_FAMILY))]
    pub mono_family: String,
    /// Zooming a tile changes it for that tile only.
    ///
    /// Terminal font size in points.
    #[schemars(title = "Size", range(min = *bounds::MONO_SIZE.start(), max = *bounds::MONO_SIZE.end()), extend("x-step" = 1.0, "x-unit" = "pt"))]
    pub mono_size: f32,
    /// A multiple of the font's own; 1.0 is what it asks for.
    ///
    /// Terminal line height as a multiple of the font's own (ghostty's `adjust-cell-height`):
    /// `1.0` is the font's, `1.2` airier, `0.9` tighter.
    #[schemars(
        title = "Line height",
        range(min = *bounds::LINE_HEIGHT.start(), max = *bounds::LINE_HEIGHT.end()),
        extend("x-step" = 0.1, "x-unit" = "\u{d7}")
    )]
    pub mono_line_height: f32,
    /// Draw => and != as one glyph in fonts that have them.
    ///
    /// Whether the terminal font's ligatures are shaped.
    #[schemars(title = "Ligatures")]
    pub ligatures: bool,
    /// Bars, lists and dialogs; every label scales with it.
    ///
    /// Chrome (top bar, pills, picker) font size in points.
    #[schemars(title = "Text size", range(min = *bounds::UI_SIZE.start(), max = *bounds::UI_SIZE.end()), extend("x-step" = 1.0, "x-unit" = "pt"))]
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
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(title = "Theme")]
pub struct ThemeSettings {
    /// Follow the system, or stay light or dark.
    #[schemars(title = "Theme")]
    pub appearance: Appearance,
}

/// `[terminal]`.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(title = "Terminal")]
pub struct TerminalSettings {
    /// Text under this ratio moves toward black or white; 1 keeps every colour.
    ///
    /// The least WCAG contrast ratio (1–21) between a cell's text and its background; text
    /// under it is moved toward black or white, whichever reads, only as far as the ratio
    /// needs, keeping its hue. `1.0` leaves every colour as the program set it. On by
    /// default, unlike ghostty's `minimum-contrast`: a prompt's own 24-bit colours chosen for
    /// a dark terminal (a pale mint at 1.2:1) are unreadable on a light one.
    #[schemars(
        title = "Minimum contrast",
        range(min = *bounds::CONTRAST.start(), max = *bounds::CONTRAST.end()),
        extend("x-step" = 0.5, "x-unit" = ":1")
    )]
    pub minimum_contrast: f32,
    /// A selection goes to the clipboard as soon as it is made.
    ///
    /// Ghostty's `copy-on-select = clipboard`, iTerm2's default.
    #[schemars(title = "Copy on select")]
    pub copy_on_select: bool,
    /// Sound the alert and bounce the Dock; off, the tile only flashes.
    ///
    /// A bell while the app is in the background plays the alert sound and bounces the Dock;
    /// off, it only flashes the tile.
    #[schemars(title = "Bell in the background")]
    pub bell_alert: bool,
    /// Auto lets the shell or the editor choose.
    ///
    /// Whether the cursor blinks (ghostty's `cursor-style-blink`): the program's choice, or
    /// always, or never.
    #[schemars(title = "Blink")]
    pub cursor_blink: CursorBlink,
    /// Auto lets the shell or the editor choose.
    ///
    /// The cursor's shape (ghostty's `cursor-style`): the program's choice, or block, bar
    /// or underline.
    #[schemars(title = "Shape")]
    pub cursor_style: CursorStyle,
    /// A paste that would run a command waits for a confirmation.
    ///
    /// A paste that could run commands (a newline into a shell that did not ask for
    /// bracketed paste) waits for a confirmation (ghostty's `clipboard-paste-protection`).
    #[schemars(title = "Paste protection")]
    pub paste_protection: bool,
    /// Bold text in the first eight colours takes their bright forms.
    ///
    /// Bold text in ANSI 0–7 is painted in ANSI 8–15 (ghostty's `bold-is-bright`).
    #[schemars(title = "Bold is bright")]
    pub bold_is_bright: bool,
    /// It comes back when it moves.
    ///
    /// The pointer hides while typing into a terminal, until it moves.
    #[schemars(title = "Hide pointer while typing")]
    pub hide_pointer_while_typing: bool,
    /// Grid lines per wheel or trackpad line.
    ///
    /// What a wheel or trackpad line scrolls, in grid lines (ghostty's
    /// `mouse-scroll-multiplier`): `1.0` one for one, `3.0` fast.
    #[schemars(
        title = "Scroll speed",
        range(min = *bounds::SCROLL.start(), max = *bounds::SCROLL.end()),
        extend("x-step" = 0.5, "x-unit" = "\u{d7}")
    )]
    pub scroll_multiplier: f32,
    /// Send the escape prefix readline wants instead of the layout's symbol.
    ///
    /// ⌥ as Alt (ghostty's `macos-option-as-alt`): `false` types the layout's symbol,
    /// `true` sends an escape prefix for readline's ⌥b/⌥f, `left`/`right` one side each.
    #[schemars(title = "Option as Alt")]
    pub option_as_alt: OptionAsAlt,
    /// Closing a terminal whose command still runs asks first.
    ///
    /// Ghostty's `confirm-close-surface`.
    #[schemars(title = "Confirm close")]
    pub confirm_close: bool,
    /// Command and Option with the arrows and delete edit the line as text fields do.
    ///
    /// The Mac's line-editing keys in a shell (ghostty's macOS "natural text editing"
    /// keybinds): ⌘← ⌘→ ⌘⌫ ⌥← ⌥→ ⌥⌫ sent as readline's bytes.
    #[schemars(title = "Natural text editing")]
    pub natural_editing: bool,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            minimum_contrast: 3.0,
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

/// `[remote]`: what a remote window or display stream asks the worker for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(title = "Remote windows and desktops")]
pub struct RemoteSettings {
    /// What the worker captures and encodes at.
    ///
    /// Frames per second the worker captures and encodes at.
    #[schemars(title = "Frame rate", range(min = *bounds::FPS.start(), max = *bounds::FPS.end()), extend("x-step" = 15, "x-unit" = "fps"))]
    pub fps: u16,
    /// The most one stream may take; it grows toward it as the link allows.
    ///
    /// The most the worker may send per stream, in megabits per second: the ceiling its
    /// bitrate controller grows towards, never the rate it starts at.
    #[schemars(
        title = "Bitrate ceiling",
        range(min = *bounds::MBPS.start(), max = *bounds::MBPS.end()),
        extend("x-step" = 5, "x-unit" = "Mb/s")
    )]
    pub max_bitrate_mbps: u16,
    /// A stream opens silent here; its pill still turns the sound on.
    ///
    /// A stream opens silenced on this client; the title-bar pill still toggles it.
    #[schemars(title = "Start muted")]
    pub muted: bool,
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self { fps: 60, max_bitrate_mbps: 30, muted: false }
    }
}

/// `[worker]`: what `slopty-worker` reads from the same file when it starts.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(title = "This Mac as a worker")]
pub struct WorkerSettings {
    /// Ranges admitted besides loopback and the tailnet; empty admits only those two.
    ///
    /// Address ranges (`10.8.0.0/24`, `fd00::/8`, a bare address) whose peers may connect: a
    /// VPN or LAN Tailscale does not vouch for (`slopty_net::admission`).
    #[schemars(title = "Allowed addresses", example = ["100.64.0.0/10", "fd00::/8"])]
    pub allow: Vec<String>,
    /// Read when the worker starts; empty runs it on its own.
    ///
    /// The server to register with, `host[:port]` with
    /// [`SERVER_PORT`](slopty_net::endpoint::SERVER_PORT) when the port is absent; `None` (an
    /// empty string in the file) runs the worker on its own. `--server` and `SLOPTY_SERVER`
    /// take precedence.
    #[serde(with = "server_address")]
    #[schemars(title = "Register with", with = "String", example = "studio.local")]
    pub server: Option<HostAddr>,
}

/// `[server]`: what `slopty-server` reads from the file of the data directory its own lives in.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(title = "This Mac as a server")]
pub struct ServerSettings {
    /// Beside loopback and the tailnet, as the worker's.
    ///
    /// Address ranges whose peers may connect besides loopback and the tailnet, as
    /// [`WorkerSettings::allow`].
    #[schemars(title = "Allowed addresses", example = ["10.8.0.0/24"])]
    pub allow: Vec<String>,
}

/// `[client]`: how the app and the `slopty` CLI find the workers.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(title = "This app")]
pub struct ClientSettings {
    /// Whose directory lists the workers; empty reaches those added by address.
    ///
    /// The server whose directory lists the workers, `host[:port]` with
    /// [`SERVER_PORT`](slopty_net::endpoint::SERVER_PORT) when the port is absent; `None` (an
    /// empty string in the file) reaches only the workers added by address.
    #[serde(with = "server_address")]
    #[schemars(title = "Server", with = "String", example = "studio.local")]
    pub server: Option<HostAddr>,
}

/// `Option<HostAddr>` as the string a person types, `""` for none, the port defaulting to
/// [`SERVER_PORT`](slopty_net::endpoint::SERVER_PORT).
mod server_address {
    use serde::{Deserialize as _, Deserializer, Serializer};
    use slopty_net::HostAddr;
    use slopty_net::endpoint::SERVER_PORT;

    #[expect(clippy::ref_option, reason = "serde's `with` hands the field over by reference")]
    pub fn serialize<S: Serializer>(addr: &Option<HostAddr>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&addr.as_ref().map(ToString::to_string).unwrap_or_default())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<HostAddr>, D::Error> {
        let text = String::deserialize(d)?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        HostAddr::parse_with_port(&text, SERVER_PORT).map(Some).map_err(serde::de::Error::custom)
    }
}

/// The whole file.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize, JsonSchema)]
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
    /// The worker daemon: who may connect, and the server it registers with.
    pub worker: WorkerSettings,
    /// The server daemon: who may connect.
    pub server: ServerSettings,
    /// The app and the CLI as clients of a server.
    pub client: ClientSettings,
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
            Err(e) => return Loaded::broken(reason(&e, text)),
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
# Least contrast ratio (1.0 to 21.0) between text and its background; text
# under it moves toward black or white, only as far as needed, keeping its hue.
# 1.0 keeps every colour as the program set it.
minimum_contrast = {minimum_contrast}
# Copy a selection to the clipboard as soon as it is made.
copy_on_select = {copy_on_select}
# A bell while the app is in the background sounds the alert and bounces
# the Dock; off, the tile only flashes.
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
# Ceiling for one stream in megabits per second (1 to 200); the worker grows
# towards it as the link allows.
max_bitrate_mbps = {max_bitrate_mbps}
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

[worker]
# Read when slopty-worker starts.
#
# Who may connect to the worker daemon on this Mac, as address ranges
# (\"10.8.0.0/24\", \"fd00::/8\", \"192.168.1.20\"), besides loopback and the
# tailnet, which always connect. A private LAN or a plain VPN is not admitted
# until it is listed here. Traffic is not encrypted by Slopty: the VPN or
# tailnet is the boundary.
allow = []
# The server this Mac registers with as a worker, \"host\" or \"host:port\"
# (port 45560 when absent). Empty runs it on its own. --server and
# SLOPTY_SERVER override it.
server = \"\"

[client]
# The server whose directory lists the workers this app and the slopty CLI
# reach, \"host\" or \"host:port\" (port 45560 when absent). Empty reaches only
# the workers added by address.
server = \"\"
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
        slopty_platform::fs::replace(path, Self::default_file().as_bytes())?;
        Ok(true)
    }
}

/// Which table's `server` key an edit sets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ServerOf {
    /// `[client] server`: the directory the app and the CLI read.
    Client,
    /// `[worker] server`: the server this Mac registers with as a worker.
    Worker,
}

impl ServerOf {
    const fn table(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Worker => "worker",
        }
    }

    const fn of(self, settings: &Settings) -> Option<&HostAddr> {
        match self {
            Self::Client => settings.client.server.as_ref(),
            Self::Worker => settings.worker.server.as_ref(),
        }
    }
}

/// `text` (a `settings.toml`, the commented defaults when there is none) with `of`'s `server`
/// set to `server`, every other line kept as it was, comments included.
///
/// # Errors
///
/// When `text` does not parse, since editing a broken file would bury the mistake.
pub fn with_server(text: &str, of: ServerOf, server: Option<&HostAddr>) -> Result<String, String> {
    if let Some(error) = Settings::parse(text).error {
        return Err(error.to_string());
    }
    let table = of.table();
    let value = toml_string(&server.map(ToString::to_string).unwrap_or_default());
    let out = edit::write(text, table, "server", &value);
    match Settings::parse(&out) {
        Loaded { error: None, settings, .. } if of.of(&settings) == server => Ok(out),
        _ => Err(format!("could not set [{table}] server in settings.toml")),
    }
}

/// Set `of`'s `server` in the `settings.toml` under `data_dir` (starting from the commented
/// defaults when there is none), keeping every other line.
///
/// The file is replaced whole, so the app watching it never reads half of one.
///
/// # Errors
///
/// When the file cannot be read or written, or does not parse.
pub fn save_server(data_dir: &Path, of: ServerOf, server: Option<&HostAddr>) -> Result<(), String> {
    let path = path_in(data_dir);
    let shown = |e: std::io::Error| format!("{}: {e}", path.display());
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default_file(),
        Err(e) => return Err(shown(e)),
    };
    let text = with_server(&text, of, server).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::create_dir_all(data_dir).map_err(shown)?;
    slopty_platform::fs::replace(&path, text.as_bytes()).map_err(shown)
}

/// Have the worker under `data_dir` register with its client's server, unless it names one.
///
/// The Mac an app makes a worker joins the directory that app reads. Returns the server it now
/// registers with.
///
/// # Errors
///
/// As [`save_server`].
pub fn join_clients_server(data_dir: &Path) -> Result<Option<HostAddr>, String> {
    let settings = Settings::load(&path_in(data_dir)).settings;
    match (settings.worker.server, settings.client.server) {
        (Some(own), _) => Ok(Some(own)),
        (None, Some(client)) => {
            save_server(data_dir, ServerOf::Worker, Some(&client))?;
            Ok(Some(client))
        }
        (None, None) => Ok(None),
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

/// The parser's reason on one line, after the line it is about: the error's own first line is
/// only where it is (`TOML parse error at line 2, column 6`), and the why comes lines later.
fn reason(e: &toml::de::Error, text: &str) -> String {
    let message = first_line(e.message());
    let Some(span) = e.span() else { return message };
    let line = text.get(..span.start).unwrap_or_default().split('\n').count();
    format!("line {line}: {message}")
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
        assert_eq!(d.terminal.minimum_contrast, 3.0, "on: a dark prompt reads on light");
        assert!(!d.terminal.copy_on_select, "\u{2318}C copies, as on the Mac");
        assert!(d.terminal.bell_alert, "a bell in the background is heard");
        assert_eq!(d.terminal.cursor_blink, CursorBlink::Program, "DECSCUSR decides");
        assert_eq!(d.terminal.cursor_style, CursorStyle::Program, "and its shape");
        assert!(d.terminal.paste_protection, "a pasted newline asks first");
        assert!(!d.terminal.bold_is_bright, "bold is a weight, as in ghostty");
        assert!(d.terminal.hide_pointer_while_typing, "as Terminal.app");
        assert_eq!(d.terminal.scroll_multiplier, 1.0, "one for one");
        assert!(d.font.ligatures, "the font's own");
        assert_eq!((d.remote.fps, d.remote.max_bitrate_mbps), (60, 30));
    }

    #[test]
    fn remote_keys() {
        let loaded = Settings::parse("[remote]\nfps = 30\nmax_bitrate_mbps = 8\nmuted = true\n");
        assert!(loaded.error.is_none(), "{:?}", loaded.error);
        assert_eq!(
            loaded.settings.remote,
            RemoteSettings { fps: 30, max_bitrate_mbps: 8, muted: true }
        );
        assert!(!Settings::default().remote.muted, "sound on, as the worker plays it");
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
        assert!(text.contains(": line 1: "), "the line and the reason: {text}");
        assert_eq!(text.lines().count(), 1, "one line for the status bar: {text}");
    }

    #[test]
    fn wrong_type_falls_back_with_error() {
        let loaded = Settings::parse("[font]\nmono_size = \"big\"\n");
        assert!(loaded.error.is_some(), "a type error is an error");
        assert_eq!(loaded.settings, Settings::default());
    }

    /// A worker made from the app joins the app's server, and keeps a server of its own.
    #[test]
    fn a_worker_joins_its_clients_server_unless_it_has_its_own() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(join_clients_server(dir.path()), Ok(None), "no server anywhere");
        assert!(!path_in(dir.path()).exists(), "and nothing written");

        let studio: HostAddr = "studio:7000".parse().unwrap();
        save_server(dir.path(), ServerOf::Client, Some(&studio)).unwrap();
        assert_eq!(join_clients_server(dir.path()), Ok(Some(studio.clone())));
        let saved = Settings::load(&path_in(dir.path())).settings;
        assert_eq!(saved.worker.server, Some(studio));

        let own: HostAddr = "100.64.0.9:7000".parse().unwrap();
        save_server(dir.path(), ServerOf::Worker, Some(&own)).unwrap();
        assert_eq!(join_clients_server(dir.path()), Ok(Some(own)), "its own wins");
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
        assert!(text.contains("minimum_contrast = 3.0"), "{text}");
        assert!(text.contains("copy_on_select = false"), "{text}");
        assert!(text.contains("max_bitrate_mbps = 30"), "{text}");
        assert!(text.contains("allow = []"), "{text}");
        assert!(text.contains("server = \"\""), "{text}");
    }

    #[test]
    fn client_keys() {
        let loaded = Settings::parse("[client]\nserver = \"studio.tail1234.ts.net\"\n");
        assert!(loaded.error.is_none() && loaded.warnings.is_empty(), "{loaded:?}");
        let server = loaded.settings.client.server.unwrap();
        assert_eq!((server.host(), server.port()), ("studio.tail1234.ts.net", 45560));
        let explicit = Settings::parse("[client]\nserver = \"[fd7a:115c:a1e0::1]:7\"\n");
        assert_eq!(explicit.settings.client.server.map(|s| s.port()), Some(7));
        assert_eq!(Settings::default().client.server, None, "workers by address only");
        let bad = Settings::parse("[client]\nserver = \"studio:x\"\n");
        assert!(bad.error.is_some_and(|e| e.to_string().contains("bad port")));
        let blank = Settings::parse("[client]\nserver = \"  \"\n");
        assert_eq!(blank.settings.client.server, None);
        assert!(blank.warnings.is_empty(), "the key is known even when empty");
    }

    #[test]
    fn setting_the_server_keeps_the_rest_of_the_file() {
        let studio = HostAddr::parse_with_port("studio", 45560).unwrap();
        let text = with_server(&Settings::default_file(), ServerOf::Client, Some(&studio)).unwrap();
        assert!(text.contains("server = \"studio:45560\""), "{text}");
        assert!(text.contains("# The server whose directory"), "comments stay");
        let loaded = Settings::parse(&text);
        assert_eq!(loaded.settings.client.server.as_ref(), Some(&studio));
        assert_eq!(loaded.settings.worker.server, None, "the worker's key is another table's");

        let custom = "[font]\nmono_size = 15.0 # mine\n";
        let set = with_server(custom, ServerOf::Client, Some(&studio)).unwrap();
        assert_eq!(set, "[font]\nmono_size = 15.0 # mine\n\n[client]\nserver = \"studio:45560\"\n");
        let cleared = with_server(&set, ServerOf::Client, None).unwrap();
        assert!(cleared.ends_with("[client]\nserver = \"\"\n"), "{cleared}");

        let keyless = "[client]\n[font]\nmono_size = 15.0\n";
        let set = with_server(keyless, ServerOf::Client, Some(&studio)).unwrap();
        assert!(set.starts_with("[client]\nserver = \"studio:45560\"\n[font]"), "{set}");

        with_server("[font\n", ServerOf::Client, Some(&studio)).unwrap_err();

        let worker =
            with_server(&Settings::default_file(), ServerOf::Worker, Some(&studio)).unwrap();
        let loaded = Settings::parse(&worker);
        assert_eq!(loaded.settings.worker.server.as_ref(), Some(&studio));
        assert_eq!(loaded.settings.client.server, None, "the client's key is another table's");
    }

    #[test]
    fn worker_keys() {
        let loaded = Settings::parse(
            "[worker]\nallow = [\"100.64.0.3\", \"fd00::/8\"]\nserver = \"studio.tail1234.ts.net\"\n",
        );
        assert!(loaded.error.is_none() && loaded.warnings.is_empty(), "{loaded:?}");
        assert_eq!(loaded.settings.worker.allow, ["100.64.0.3", "fd00::/8"]);
        let server = loaded.settings.worker.server.unwrap();
        assert_eq!((server.host(), server.port()), ("studio.tail1234.ts.net", 45560));
        assert!(Settings::default().worker.allow.is_empty(), "the private ranges by default");
        assert_eq!(Settings::default().worker.server, None, "on its own by default");
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
