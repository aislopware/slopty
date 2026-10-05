//! User settings: `<data dir>/settings.toml`.
//!
//! Every field has a default and the file may be absent. Unknown keys are reported as
//! warnings, never errors. A file that does not parse yields the defaults plus the error, so
//! the app never fails to start over a typo; the caller shows the error and the next save
//! heals it. Every reader follows the file for changes itself ([`follow`]).

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
pub use slopty_net::HostAddr;

pub mod edit;
pub mod follow;
pub mod schema;

/// What the app takes of each number, outside of which a value is a typo and its default
/// applies. The schema's `range` reads these, so the form's steppers and the app's check agree.
pub mod bounds {
    use std::ops::RangeInclusive;

    /// Terminal font size, in points.
    pub const MONO_SIZE: RangeInclusive<f32> = 6.0..=72.0;
    /// Chrome font size, in points.
    pub const UI_SIZE: RangeInclusive<f32> = 8.0..=32.0;
    /// What is read at length, in points: under 10 an answer is a footnote, past 32 a poster.
    pub const PROSE_SIZE: RangeInclusive<f32> = 10.0..=32.0;
    /// Half the font's line height packs rows past reading; twice it is a list, not a grid.
    pub const LINE_HEIGHT: RangeInclusive<f32> = 0.5..=2.0;
    /// A tenth of a line per wheel line is glacial; ten is a page.
    pub const SCROLL: RangeInclusive<f32> = 0.1..=10.0;
    /// WCAG ratios run from 1 (the same colour) to 21 (black on white).
    pub const CONTRAST: RangeInclusive<f32> = 1.0..=21.0;
    /// Under a megabit nothing decodes; 200 Mbit/s is past what one stream ever grows to.
    pub const MBPS: RangeInclusive<u16> = 1..=200;
    /// Minutes a display made for a client waits for it: none, up to a day.
    pub const DISPLAY_LINGER_MINS: RangeInclusive<u16> = 0..=1440;
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

/// When a terminal's bell, or an agent that needs the person, sounds the alert and bounces
/// the Dock.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Alert {
    /// Never: the tile flashes, and the banner, the inbox and the badge say it.
    #[schemars(title = "Never")]
    Never,
    /// Only while no Slopty window is in front: in front, the tile says it.
    #[default]
    #[schemars(title = "When hidden")]
    Hidden,
    /// Always, in front of the window too.
    #[schemars(title = "Always")]
    Always,
}

impl Alert {
    /// Whether it sounds, with or without a Slopty window in front.
    #[must_use]
    pub const fn sounds(self, window_active: bool) -> bool {
        match self {
            Self::Never => false,
            Self::Hidden => !window_active,
            Self::Always => true,
        }
    }
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

/// When typing into Slopty is kept from other programs on this Mac (macOS secure event input,
/// Terminal's "Secure Keyboard Entry").
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SecureEntry {
    /// While a terminal waits for a password, or a remote window's password field has the
    /// keyboard.
    #[default]
    #[schemars(title = "At passwords")]
    Passwords,
    /// Whenever a Slopty window is in front.
    #[schemars(title = "Always")]
    Always,
    /// Never.
    #[schemars(title = "Never")]
    Never,
}

/// What keeps a worker Mac out of idle sleep. A display that streams is kept on unless it is
/// [`Self::Never`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum KeepAwake {
    /// A client attached, or an agent at work with nobody attached.
    #[default]
    #[schemars(title = "While working")]
    Working,
    /// Only a client attached: an agent left working sleeps with the Mac.
    #[schemars(title = "While attached")]
    Attached,
    /// Nothing: the Mac sleeps as its own settings say, a stream's display too.
    #[schemars(title = "Never")]
    Never,
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

/// `[colors]`: the terminal palette for each appearance, `[colors.light]` and
/// `[colors.dark]`. A colour picked for one is rarely right on the other's ground, so neither
/// reaches the other.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(title = "Terminal colours")]
pub struct ColorSettings {
    /// The palette in the light appearance.
    #[schemars(title = "Light terminal colours")]
    pub light: Palette,
    /// The palette in the dark appearance.
    #[schemars(title = "Dark terminal colours")]
    pub dark: Palette,
}

impl ColorSettings {
    /// The palette for the dark appearance, or the light one.
    #[must_use]
    pub const fn for_dark(&self, dark: bool) -> &Palette {
        if dark { &self.dark } else { &self.light }
    }
}

/// One appearance's terminal palette, each entry the theme's own unless set.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Palette {
    /// Empty keeps the theme's own.
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
    /// Any monospace; `JetBrains Mono` is built in.
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
    /// A multiple of the font's own; 1.0 keeps it.
    ///
    /// Terminal line height as a multiple of the font's own (ghostty's `adjust-cell-height`):
    /// `1.0` is the font's, `1.2` airier, `0.9` tighter.
    #[schemars(
        title = "Line height",
        range(min = *bounds::LINE_HEIGHT.start(), max = *bounds::LINE_HEIGHT.end()),
        extend("x-step" = 0.1, "x-unit" = "\u{d7}")
    )]
    pub mono_line_height: f32,
    /// Draw => and != as one glyph where the font can.
    ///
    /// Whether the terminal font's ligatures are shaped.
    #[schemars(title = "Ligatures")]
    pub ligatures: bool,
    /// Bars, lists and dialogs; every label scales with it.
    ///
    /// Chrome (top bar, pills, picker) font size in points.
    #[schemars(title = "Text size", range(min = *bounds::UI_SIZE.start(), max = *bounds::UI_SIZE.end()), extend("x-step" = 1.0, "x-unit" = "pt"))]
    pub ui_size: f32,
    /// Agents' answers and your messages; the bars keep their size.
    ///
    /// What is read at length, in points: an agent's answers, the messages sent to it and the
    /// field they are written in. The chrome keeps the text size.
    #[schemars(title = "Reading size", range(min = *bounds::PROSE_SIZE.start(), max = *bounds::PROSE_SIZE.end()), extend("x-step" = 1.0, "x-unit" = "pt"))]
    pub prose_size: f32,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            mono_family: "JetBrains Mono".to_owned(),
            mono_size: 13.0,
            mono_line_height: 1.0,
            ligatures: true,
            ui_size: 13.0,
            prose_size: 15.0,
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
    /// When hidden sounds it only while Slopty is behind other windows.
    ///
    /// When a terminal's bell, or an agent that needs you (an approval, a question, a
    /// finished turn), plays the alert sound and bounces the Dock, as a Mac app's alert does:
    /// never (the tile flashes, the banner and the inbox say it), only while no Slopty window
    /// is in front (in front, the tile says it), or always.
    #[schemars(title = "Alert")]
    pub alert: Alert,
    /// Auto blinks when the shell or the editor asks.
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
    /// Edit the line with ⌘ and ⌥ and the arrows, as text fields do.
    ///
    /// The Mac's line-editing keys in a shell (ghostty's macOS "natural text editing"
    /// keybinds): ⌘← ⌘→ ⌘⌫ ⌥← ⌥→ ⌥⌫ sent as readline's bytes.
    #[schemars(title = "Natural text editing")]
    pub natural_editing: bool,
    /// Keeps passwords typed here from other apps, as Terminal does.
    ///
    /// When macOS keeps what is typed into Slopty from other programs on this Mac (secure
    /// event input): while a terminal waits for a password or a remote window's password
    /// field has the keyboard, whenever a Slopty window is in front, or never. While it holds,
    /// other apps' shortcuts and "Send system shortcuts" do not see the keys.
    #[schemars(title = "Secure keyboard entry")]
    pub secure_keyboard_entry: SecureEntry,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            minimum_contrast: 3.0,
            copy_on_select: false,
            alert: Alert::Hidden,
            cursor_blink: CursorBlink::Program,
            cursor_style: CursorStyle::Program,
            paste_protection: true,
            bold_is_bright: false,
            hide_pointer_while_typing: true,
            scroll_multiplier: 1.0,
            option_as_alt: OptionAsAlt::False,
            confirm_close: true,
            natural_editing: true,
            secure_keyboard_entry: SecureEntry::Passwords,
        }
    }
}

/// `[remote]`: what a remote window or display stream asks the worker for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(title = "Remote windows and desktops")]
pub struct RemoteSettings {
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
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self { max_bitrate_mbps: 30 }
    }
}

/// `[clipboard]`: the clipboard shared with the machines.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(title = "Clipboard")]
pub struct ClipboardSettings {
    /// Copy on a machine, paste here, and back; off, each keeps its own.
    ///
    /// Shares the clipboard with the machines: what is copied on one is offered to this Mac
    /// and to the others, and what is copied here is offered to the machine in front. Off, nothing
    /// crosses either way; a terminal's own copy and paste still work.
    #[schemars(title = "Share the clipboard")]
    pub sync: bool,
    /// Each machine named here keeps its own setting.
    ///
    /// A Mac others use can be kept out of it.
    #[schemars(title = "By machine")]
    pub workers: BTreeMap<String, bool>,
}

impl Default for ClipboardSettings {
    fn default() -> Self {
        Self { sync: true, workers: BTreeMap::new() }
    }
}

impl ClipboardSettings {
    /// Whether the clipboard is shared with the worker called `name`.
    #[must_use]
    pub fn shared_with(&self, name: &str) -> bool {
        self.workers.get(name).copied().unwrap_or(self.sync)
    }
}

/// `[keys]`: the app's key bindings the file changes.
///
/// A table per context (`[keys.workspace]`, `[keys.terminal]`) of action names and their
/// chords. An action the file does not name keeps its default; which names and contexts exist
/// is the app's keymap's (`slopty_ui::keymap`), which says what it does not know.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(title = "Keys")]
pub struct KeySettings(pub BTreeMap<String, BTreeMap<String, Chords>>);

impl KeySettings {
    /// The chords the file gives `action` in `context`, if it names it.
    #[must_use]
    pub fn get(&self, context: &str, action: &str) -> Option<&Chords> {
        self.0.get(context)?.get(action)
    }
}

/// An action's chords as the file sets them: one (`"cmd-t"`), several (`["cmd-t", "cmd-n"]`),
/// or none (`""`, `"none"` or `[]`), which unbinds it. Each is in the palette's key syntax,
/// read by the keymap.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Chords(pub Vec<String>);

impl Chords {
    /// The chords as written, trimmed, the words that mean none dropped.
    fn of(texts: impl IntoIterator<Item = String>) -> Self {
        Self(
            texts
                .into_iter()
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty() && !t.eq_ignore_ascii_case("none"))
                .collect(),
        )
    }
}

impl Serialize for Chords {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [] => serializer.serialize_str(""),
            [one] => serializer.serialize_str(one),
            many => many.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Chords {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Chords;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a chord (\"cmd-t\"), a list of chords, or \"\" for none")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Chords, E> {
                Ok(Chords::of([v.to_owned()]))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Chords, A::Error> {
                let mut texts = Vec::new();
                while let Some(text) = seq.next_element::<String>()? {
                    texts.push(text);
                }
                Ok(Chords::of(texts))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

impl JsonSchema for Chords {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Chords".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "anyOf": [
                { "type": "string" },
                { "type": "array", "items": { "type": "string" } },
            ],
        })
    }
}

/// `[worker]`: what `slopty-worker` reads from the same file.
///
/// It follows the file, applying every key as it changes.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(title = "Share this Mac's shells and windows")]
pub struct WorkerSettings {
    /// Address ranges let in besides loopback and the tailnet.
    ///
    /// Address ranges (`10.8.0.0/24`, `fd00::/8`, a bare address) whose peers may connect: a
    /// VPN or LAN Tailscale does not vouch for (`slopty_net::admission`). Applied to the next
    /// peer as soon as the file changes.
    #[schemars(title = "Allowed addresses", example = ["100.64.0.0/10", "fd00::/8"])]
    pub allow: Vec<String>,
    /// The server it lists itself with; installing it sets one.
    ///
    /// The server to register with, `host[:port]` with
    /// [`SERVER_PORT`](slopty_net::endpoint::SERVER_PORT) when the port is absent; `None` (an
    /// empty string in the file) leaves it registered nowhere, serving only the clients that reach
    /// it already, which no installer leaves it doing. `--server` and `SLOPTY_SERVER`
    /// take precedence. A change registers it with the new one at once.
    #[serde(with = "server_address")]
    #[schemars(title = "Register with", with = "String", example = "studio.local")]
    pub server: Option<HostAddr>,
    /// ACP agents beside the ones Slopty knows; an empty command hides one.
    ///
    /// A name and the command line that serves the Agent Client Protocol on stdio
    /// (`mine = ["/opt/mine/bin/agent", "--acp"]`). A name Slopty already knows (`gemini`,
    /// `opencode`, the ACP registry's) is started this way instead, and an empty list takes it
    /// away. Its threads' agent is `acp:<name>`. A change is probed again at once.
    #[schemars(
        title = "ACP agents",
        example = serde_json::json!({ "mine": ["/opt/mine/bin/agent", "--acp"], "goose": [] })
    )]
    pub acp: BTreeMap<String, Vec<String>>,
    /// Use the client's input source there; off, the client composes.
    ///
    /// While a remote window has the keyboard, the worker selects the client's keyboard input
    /// source, so keys go by their place and the remote app's own input methods compose. Off,
    /// the worker keeps its own source for the person at it, and each client composes on its
    /// side and sends the text. Turned off, the worker's own source comes back at once.
    #[schemars(title = "Follow the client's input source")]
    pub input_source_sync: bool,
    /// While working also counts an agent at work with nobody attached.
    ///
    /// What keeps this Mac out of idle sleep: a client attached or an agent at work, a client
    /// attached only, or nothing (the Mac sleeps as its own settings say, a streamed display
    /// too). A server on this Mac follows it too, counting every client linked to it and every
    /// worker's agents. Applied at once.
    #[schemars(title = "Keep awake")]
    pub keep_awake: KeepAwake,
    /// How long a client's display waits for it to return; 0 ends it.
    ///
    /// A display the worker made in a client's shape stays this many minutes after its last
    /// stream ends, windows in place, for the same client to take back. A change counts from
    /// the next display let go.
    #[schemars(
        title = "Keep a client's display",
        range(min = *bounds::DISPLAY_LINGER_MINS.start(), max = *bounds::DISPLAY_LINGER_MINS.end()),
        extend("x-step" = 5, "x-unit" = "min")
    )]
    pub display_linger_mins: u16,
}

impl Default for WorkerSettings {
    fn default() -> Self {
        Self {
            allow: Vec::new(),
            server: None,
            acp: BTreeMap::new(),
            input_source_sync: true,
            keep_awake: KeepAwake::Working,
            display_linger_mins: 10,
        }
    }
}

impl WorkerSettings {
    /// How long a client's display waits for it: [`Self::display_linger_mins`], or its
    /// default outside [`bounds::DISPLAY_LINGER_MINS`].
    #[must_use]
    pub fn display_linger(&self) -> std::time::Duration {
        let mins = if bounds::DISPLAY_LINGER_MINS.contains(&self.display_linger_mins) {
            self.display_linger_mins
        } else {
            Self::default().display_linger_mins
        };
        std::time::Duration::from_secs(u64::from(mins) * 60)
    }
}

/// `[server]`: what `slopty-server` reads from the file of the data directory its own lives in.
///
/// It follows the file: each key applies as the file changes.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(title = "This Mac as a server")]
pub struct ServerSettings {
    /// Address ranges let in besides loopback and the tailnet.
    ///
    /// Address ranges whose peers may connect besides loopback and the tailnet, as
    /// [`WorkerSettings::allow`].
    #[schemars(title = "Allowed addresses", example = ["10.8.0.0/24"])]
    pub allow: Vec<String>,
    /// What every project's own limits stay under.
    pub projects: ProjectBounds,
}

/// `[server.projects]`: what the person allows projects across the fleet
/// (`docs/decisions/projects.md`). No agent raises them.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(title = "Projects")]
pub struct ProjectBounds {
    /// Agents running at once across the fleet, in a project or not.
    ///
    /// The most live agents the server lets run across every worker at once.
    #[schemars(title = "Live agents", example = 24)]
    pub live_agents: u16,
    /// Projects whose agents may be started with looser permissions.
    ///
    /// The names of the projects whose spawned agents may be given flags that loosen Claude
    /// Code's permissions (`--dangerously-skip-permissions`, `--permission-mode`). Empty
    /// allows none.
    #[schemars(title = "Looser permissions for", example = ["nightly-refactor"])]
    pub permission_flags: Vec<String>,
}

impl Default for ProjectBounds {
    fn default() -> Self {
        Self { live_agents: 24, permission_flags: Vec::new() }
    }
}

/// `[client]`: how the app and the `slopty` CLI find the machines.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(title = "This app")]
pub struct ClientSettings {
    /// Lists your machines; empty until this app is set up.
    ///
    /// The server whose directory lists the workers, `host[:port]` with
    /// [`SERVER_PORT`](slopty_net::endpoint::SERVER_PORT) when the port is absent; `None` (an
    /// empty string in the file) is an app not set up yet, which opens on its first run.
    #[serde(with = "server_address")]
    #[schemars(title = "Server", with = "String", example = "studio.local")]
    pub server: Option<HostAddr>,
    /// Opens files and folders in your own editor; empty uses the system's.
    ///
    /// A link holding `{path}` (the file or folder on its machine), and optionally `{host}`
    /// (the machine's name) and `{line}` (the line in view, else 1), which any editor with a
    /// link scheme opens, such as `zed://ssh/{host}{path}` or
    /// `vscode://vscode-remote/ssh-remote+{host}{path}`. Empty opens the file with the
    /// system's handler for its type.
    #[schemars(title = "Editor", with = "String", example = "zed://ssh/{host}{path}")]
    pub editor: EditorLink,
}

/// `[client] editor`: the link that opens a file in the person's editor, `""` for the system's.
///
/// It is a link rather than a command because every editor with remote editing publishes one,
/// and a phone can open a link but run no command.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize)]
#[serde(transparent)]
pub struct EditorLink(String);

impl EditorLink {
    /// Whether none is set: the system's handler opens files.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The editor its scheme names, for the editors that publish one: `zed://` is Zed.
    #[must_use]
    pub fn editor_name(&self) -> Option<&'static str> {
        let (scheme, _) = self.0.split_once(':')?;
        Some(match scheme.to_ascii_lowercase().as_str() {
            "zed" => "Zed",
            "vscode" => "VS Code",
            "vscode-insiders" => "VS Code Insiders",
            "vscodium" => "VSCodium",
            "cursor" => "Cursor",
            "windsurf" => "Windsurf",
            _ => return None,
        })
    }

    /// The link that opens `path` on `host` at `line`, or `None` for the system's handler.
    #[must_use]
    #[expect(
        clippy::literal_string_with_formatting_args,
        reason = "the placeholders a person writes in the link, not Rust's"
    )]
    pub fn open(&self, host: &str, path: &str, line: Option<u32>) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }
        Some(
            self.0
                .replace("{host}", host)
                .replace("{line}", &line.unwrap_or(1).to_string())
                .replace("{path}", &link_path(path)),
        )
    }
}

impl<'de> Deserialize<'de> for EditorLink {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?.trim().to_owned();
        let scheme = text.split_once(':').is_some_and(|(scheme, _)| {
            scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        });
        if text.is_empty() || (scheme && text.contains("{path}")) {
            Ok(Self(text))
        } else {
            Err(serde::de::Error::custom(
                "an editor link starts with its scheme and holds {path}, as zed://ssh/{host}{path}",
            ))
        }
    }
}

/// `path` as a link's path: every byte but the unreserved ones and `/` percent-encoded, so a
/// space, `#` or `?` in a name stays part of it.
#[must_use]
pub fn link_path(path: &str) -> String {
    use std::fmt::Write as _;
    path.bytes().fold(String::with_capacity(path.len()), |mut out, b| {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            let _infallible = write!(out, "%{b:02X}");
        }
        out
    })
}

/// `Option<HostAddr>` as the string a person types, `""` for none, the port defaulting to
/// [`SERVER_PORT`](slopty_net::endpoint::SERVER_PORT).
mod server_address {
    use serde::{Deserialize as _, Deserializer, Serializer};
    use slopty_net::HostAddr;
    use slopty_net::endpoint::SERVER_PORT;

    #[expect(clippy::ref_option, reason = "serde's `with` hands the field over by reference")]
    pub(crate) fn serialize<S: Serializer>(
        addr: &Option<HostAddr>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_str(&addr.as_ref().map(ToString::to_string).unwrap_or_default())
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<HostAddr>, D::Error> {
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
    /// The clipboard shared with the workers.
    pub clipboard: ClipboardSettings,
    /// Terminal colours, per appearance.
    pub colors: ColorSettings,
    /// The app's key bindings the file changes.
    pub keys: KeySettings,
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
# Agents' answers and your messages, in points (10 to 32); the chrome keeps its size.
prose_size = {prose_size}

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
# When a terminal's bell, or an agent that needs you, sounds the alert and
# bounces the Dock: \"never\" (the tile flashes), \"hidden\" (only while no
# Slopty window is in front) or \"always\".
alert = {alert}
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
# Ceiling for one stream in megabits per second (1 to 200); the machine grows
# towards it as the link allows.
max_bitrate_mbps = {max_bitrate_mbps}

[clipboard]
# Share the clipboard with the machines: copy on one, paste here or on another.
# Off, nothing crosses either way; a terminal's own copy and paste still work.
sync = {clipboard_sync}
# Machines by name, each shared with or not whatever sync says.
#
# [clipboard.workers]
# shared-mac = false

[colors.light]
# Terminal colours as \"#rrggbb\" in the light appearance; \"\" keeps the
# theme's own. [colors.dark] below holds the dark one's.
foreground = \"\"
background = \"\"
cursor = \"\"
# Text under the cursor block; black or white against the cursor when unset.
cursor_text = \"\"
selection = \"\"
# ANSI 0-15 in order: black, red, green, yellow, blue, magenta, cyan, white,
# then their bright forms. Fewer than 16 keep the rest.
ansi = []

[colors.dark]
foreground = \"\"
background = \"\"
cursor = \"\"
cursor_text = \"\"
selection = \"\"
ansi = []

[keys]
# The app's keys, a table per context: app, workspace, terminal, file,
# conversation, folder, search and page. An action takes a chord in the
# palette's syntax (\"cmd-shift-t\"), a list of them, or \"\" to unbind it; one
# the file does not name keeps its default. Settings > Keyboard lists every
# action by the name it has here, and records a new chord for it.
#
# [keys.workspace]
# new_terminal = [\"cmd-t\", \"cmd-n\"]

[worker]
# allow and server apply as this file changes; the rest when slopty-worker
# starts again.
#
# Who may connect to the worker daemon on this Mac, as address ranges
# (\"10.8.0.0/24\", \"fd00::/8\", \"192.168.1.20\"), besides loopback and the
# tailnet, which always connect. A private LAN or a plain VPN is not admitted
# until it is listed here. Traffic is not encrypted by Slopty: the VPN or
# tailnet is the boundary.
allow = []
# The server this Mac registers with as a worker, \"host\" or \"host:port\"
# (port 45560 when absent). Installing the worker sets it. --server and
# SLOPTY_SERVER override it.
server = \"\"
# Agents that speak ACP on stdio, by name and command line, beside the ones
# Slopty knows; a known name is started this way instead, and [] hides it.
#
# [worker.acp]
# mine = [\"/opt/mine/bin/agent\", \"--acp\"]

[client]
# The server whose directory lists the machines this app and the slopty CLI
# reach, \"host\" or \"host:port\" (port 45560 when absent). Empty until the
# app is set up.
server = \"\"
# Opens files and folders in your own editor: a link holding {{path}}, and
# optionally {{host}} and {{line}}, such as \"zed://ssh/{{host}}{{path}}\" or
# \"vscode://vscode-remote/ssh-remote+{{host}}{{path}}\". Empty uses the system's
# handler for the file's type.
editor = \"\"
",
            mono_family = toml_string(&d.font.mono_family),
            mono_size = toml_float(d.font.mono_size),
            mono_line_height = toml_float(d.font.mono_line_height),
            ligatures = d.font.ligatures,
            ui_size = toml_float(d.font.ui_size),
            prose_size = toml_float(d.font.prose_size),
            appearance = toml_string(appearance_name(d.theme.appearance)),
            minimum_contrast = toml_float(d.terminal.minimum_contrast),
            copy_on_select = d.terminal.copy_on_select,
            alert = toml_string(alert_name(d.terminal.alert)),
            clipboard_sync = d.clipboard.sync,
            cursor_blink = toml_string(cursor_blink_name(d.terminal.cursor_blink)),
            cursor_style = toml_string(cursor_style_name(d.terminal.cursor_style)),
            paste_protection = d.terminal.paste_protection,
            bold_is_bright = d.terminal.bold_is_bright,
            hide_pointer_while_typing = d.terminal.hide_pointer_while_typing,
            scroll_multiplier = toml_float(d.terminal.scroll_multiplier),
            option_as_alt = toml_string(option_as_alt_name(d.terminal.option_as_alt)),
            confirm_close = d.terminal.confirm_close,
            natural_editing = d.terminal.natural_editing,
            max_bitrate_mbps = d.remote.max_bitrate_mbps,
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

/// Have the worker under `data_dir` register with `server`, unless it names one of its own;
/// the server it registers with now.
///
/// # Errors
///
/// As [`save_server`].
pub fn join_server(data_dir: &Path, server: &HostAddr) -> Result<HostAddr, String> {
    let settings = Settings::load(&path_in(data_dir)).settings;
    if let Some(own) = settings.worker.server {
        return Ok(own);
    }
    save_server(data_dir, ServerOf::Worker, Some(server))?;
    Ok(server.clone())
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

/// Tables whose keys the person names. `[keys]` names actions the keymap knows, not this
/// crate, and the keymap says which it does not; a worker's ACP agents are anything.
const OPEN_TABLES: [&str; 3] = ["keys", "clipboard.workers", "worker.acp"];

/// Keys in `given` with no counterpart in `known`, recursively through tables, except the
/// [`OPEN_TABLES`].
fn unknown_keys(given: &toml::Table, known: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (key, value) in given {
        let full = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
        if OPEN_TABLES.contains(&full.as_str()) {
            continue;
        }
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

const fn alert_name(a: Alert) -> &'static str {
    match a {
        Alert::Never => "never",
        Alert::Hidden => "hidden",
        Alert::Always => "always",
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
        assert_eq!(d.terminal.alert, Alert::Hidden, "heard while Slopty is hidden");
        assert_eq!(d.terminal.cursor_blink, CursorBlink::Program, "DECSCUSR decides");
        assert_eq!(d.terminal.cursor_style, CursorStyle::Program, "and its shape");
        assert!(d.terminal.paste_protection, "a pasted newline asks first");
        assert!(!d.terminal.bold_is_bright, "bold is a weight, as in ghostty");
        assert!(d.terminal.hide_pointer_while_typing, "as Terminal.app");
        assert_eq!(d.terminal.scroll_multiplier, 1.0, "one for one");
        assert!(d.font.ligatures, "the font's own");
        assert_eq!(d.remote.max_bitrate_mbps, 30);
    }

    #[test]
    fn remote_keys() {
        let loaded = Settings::parse("[remote]\nmax_bitrate_mbps = 8\n");
        assert!(loaded.error.is_none(), "{:?}", loaded.error);
        assert_eq!(loaded.settings.remote, RemoteSettings { max_bitrate_mbps: 8 });
        let gone = Settings::parse("[remote]\nmuted = true\nsharp_text = true\n");
        assert_eq!(
            gone.warnings,
            ["unknown key `remote.muted`", "unknown key `remote.sharp_text`"],
            "the pill mutes and the rate picks the chroma"
        );
    }

    /// `[worker]`'s own choices: syncing the input source, what keeps the Mac awake, how long a
    /// client's display waits. A linger outside its bounds is the default's, and 0 is at once.
    #[test]
    fn worker_choices() {
        let d = WorkerSettings::default();
        assert!(d.input_source_sync, "on: keys go by their place");
        assert_eq!(d.keep_awake, KeepAwake::Working);
        assert_eq!(d.display_linger(), std::time::Duration::from_mins(10));
        assert_eq!(Settings::default().terminal.secure_keyboard_entry, SecureEntry::Passwords);

        let loaded = Settings::parse(
            "[worker]\ninput_source_sync = false\nkeep_awake = \"never\"\n\
             display_linger_mins = 0\n[terminal]\nsecure_keyboard_entry = \"always\"\n",
        );
        assert!(loaded.error.is_none() && loaded.warnings.is_empty(), "{loaded:?}");
        let worker = &loaded.settings.worker;
        assert!(!worker.input_source_sync);
        assert_eq!(worker.keep_awake, KeepAwake::Never);
        assert_eq!(worker.display_linger(), std::time::Duration::ZERO, "let go at once");
        assert_eq!(loaded.settings.terminal.secure_keyboard_entry, SecureEntry::Always);

        let far = WorkerSettings { display_linger_mins: 5000, ..WorkerSettings::default() };
        assert_eq!(far.display_linger(), std::time::Duration::from_mins(10), "out of bounds");
    }

    /// `[keys]` holds a table per context of action names and their chords: one, a list, or
    /// none by `""`, `"none"` or `[]`. The names are the keymap's to check, so none is an
    /// unknown key here; a value that is no chord at all is an error like any wrong type.
    #[test]
    fn key_bindings_by_context() {
        let loaded = Settings::parse(
            "[keys.workspace]\nnew_terminal = \" cmd-alt-t \"\nfont_larger = [\"cmd-=\", \"cmd-+\"]\nclose_tile = \"\"\n[keys.terminal]\ncopy = \"none\"\npaste = []\n",
        );
        assert!(loaded.error.is_none() && loaded.warnings.is_empty(), "{loaded:?}");
        let keys = &loaded.settings.keys;
        let chords = |context, action| keys.get(context, action).map(|c| c.0.clone());
        assert_eq!(chords("workspace", "new_terminal"), Some(vec!["cmd-alt-t".to_owned()]));
        assert_eq!(chords("workspace", "font_larger").map(|c| c.len()), Some(2));
        for (context, action) in [("workspace", "close_tile"), ("terminal", "copy")] {
            assert_eq!(chords(context, action), Some(Vec::new()), "{action} unbound");
        }
        assert_eq!(chords("terminal", "paste"), Some(Vec::new()));
        assert_eq!(chords("terminal", "find"), None, "not named: the default");
        assert_eq!(Settings::default().keys, KeySettings::default(), "every default");

        let wrong = Settings::parse("[keys.workspace]\nnew_terminal = 3\n");
        let error = wrong.error.map(|e| e.to_string()).unwrap_or_default();
        assert!(error.contains("a chord"), "{error}");

        let back = toml::to_string(&loaded.settings).unwrap_or_default();
        assert_eq!(Settings::parse(&back).settings.keys, *keys, "written back as read:\n{back}");
    }

    /// The clipboard is shared with every worker unless `sync` says not; a worker named under
    /// `[clipboard.workers]` is shared with or not whatever `sync` says. Both default to
    /// sharing, and the alert to sounding while Slopty is hidden.
    #[test]
    fn the_clipboard_is_shared_by_default_and_per_worker_by_name() {
        let d = Settings::default();
        assert!(d.clipboard.sync && d.clipboard.shared_with("studio"));
        let loaded = Settings::parse(
            "[terminal]\nalert = \"never\"\n[clipboard]\nsync = false\n[clipboard.workers]\nstudio = true\n",
        );
        assert!(loaded.error.is_none() && loaded.warnings.is_empty(), "{loaded:?}");
        let s = &loaded.settings;
        assert_eq!(s.terminal.alert, Alert::Never);
        let ways = [Alert::Never, Alert::Hidden, Alert::Always];
        let heard = ways.map(|a| (a.sounds(false), a.sounds(true)));
        assert_eq!(heard, [(false, false), (true, false), (true, true)], "hidden, then in front");
        assert!(Settings::parse("[terminal]\nalert = true\n").error.is_some(), "no bool");
        assert!(s.clipboard.shared_with("studio"), "named: shared");
        assert!(!s.clipboard.shared_with("shared-mac"), "the rest follow sync");
        let shared = Settings::parse("[clipboard.workers]\nshared-mac = false\n").settings;
        assert!(!shared.clipboard.shared_with("shared-mac") && shared.clipboard.shared_with("x"));
    }

    #[test]
    fn colour_keys() {
        let loaded = Settings::parse(
            "[colors.dark]\nforeground = \"#c0caf5\"\nbackground = \"1a1b26\"\ncursor = \"\"\nansi = [\"#15161e\", \"#f7768e\"]\n",
        );
        assert!(loaded.error.is_none() && loaded.warnings.is_empty(), "{loaded:?}");
        let c = &loaded.settings.colors.dark;
        assert_eq!(c.foreground, Color(Some([0xc0, 0xca, 0xf5])));
        assert_eq!(c.background, Color(Some([0x1a, 0x1b, 0x26])), "the # is optional");
        assert_eq!(c.cursor, Color(None), "empty: the theme's own");
        assert_eq!(c.ansi, [Color(Some([0x15, 0x16, 0x1e])), Color(Some([0xf7, 0x76, 0x8e]))]);
        let colors = &loaded.settings.colors;
        assert_eq!(colors.light, Palette::default(), "the dark palette leaves light alone");
        assert_eq!(colors.for_dark(true), &colors.dark);
        assert_eq!(colors.for_dark(false), &colors.light);
        assert_eq!(Settings::default().colors, ColorSettings::default(), "nothing set");
        // One table for both appearances is gone, not read as either.
        let flat = Settings::parse("[colors]\nforeground = \"#c0caf5\"\n");
        assert_eq!(flat.warnings, ["unknown key `colors.foreground`"]);
        assert_eq!(flat.settings.colors, ColorSettings::default());
        for bad in ["#12345", "#gg0000", "red"] {
            let loaded = Settings::parse(&format!("[colors.light]\ncursor = \"{bad}\"\n"));
            assert!(
                loaded.error.as_ref().is_some_and(|e| e.to_string().contains("#rrggbb")),
                "{bad}: {:?}",
                loaded.error
            );
        }
        assert_eq!(
            toml::to_string(&Palette {
                cursor: Color(Some([1, 0xab, 0xff])),
                ..Palette::default()
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
            "[font]\nmono_line_height = 1.2\n[terminal]\nminimum_contrast = 3\ncopy_on_select = true\nalert = \"always\"\ncursor_blink = \"never\"\npaste_protection = false\nbold_is_bright = true\nhide_pointer_while_typing = false\nscroll_multiplier = 3\nconfirm_close = false\nnatural_editing = false\n",
        );
        assert!(loaded.error.is_none(), "{:?}", loaded.error);
        assert!(!loaded.settings.terminal.confirm_close);
        assert!(!loaded.settings.terminal.natural_editing);
        assert_eq!(loaded.settings.font.mono_line_height, 1.2);
        assert_eq!(loaded.settings.terminal.minimum_contrast, 3.0);
        assert!(loaded.settings.terminal.copy_on_select);
        assert_eq!(loaded.settings.terminal.alert, Alert::Always);
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
        let loaded = Settings::parse("[font]\nmono_size = 14\nui_size = 12\nprose_size = 18\n");
        assert!(loaded.error.is_none(), "{:?}", loaded.error);
        assert_eq!(loaded.settings.font.mono_size, 14.0);
        assert_eq!(loaded.settings.font.ui_size, 12.0);
        assert_eq!(loaded.settings.font.prose_size, 18.0, "reading has its own size");
        assert_eq!(Settings::default().font.prose_size, 15.0, "two over the chrome's");
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

    /// A worker made from the app joins the server it is given, and keeps a server of its own.
    #[test]
    fn a_worker_joins_the_server_it_is_given_unless_it_has_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let studio: HostAddr = "studio:7000".parse().unwrap();
        assert_eq!(join_server(dir.path(), &studio), Ok(studio.clone()));
        let saved = Settings::load(&path_in(dir.path())).settings;
        assert_eq!(saved.worker.server, Some(studio.clone()));
        assert_eq!(saved.client.server, None, "the app's own key is the app's to write");

        let own: HostAddr = "100.64.0.9:7000".parse().unwrap();
        save_server(dir.path(), ServerOf::Worker, Some(&own)).unwrap();
        assert_eq!(join_server(dir.path(), &studio), Ok(own), "its own wins");
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
        assert!(text.contains("editor = \"\""), "{text}");
        assert!(!text.contains("[quick_terminal]"), "{text}");
        assert!(text.contains("[keys]\n# The app's keys"), "{text}");
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

    /// The editor link fills in the machine, the line and the path, the path percent-encoded so
    /// a name with a space or `#` stays whole; empty is the system's handler, and a link that
    /// cannot name the file is refused.
    #[test]
    fn the_editor_link_opens_the_file_on_its_machine() {
        assert_eq!(Settings::default().client.editor.open("studio", "/a", None), None);
        let zed = Settings::parse("[client]\neditor = \"zed://ssh/{host}{path}:{line}\"\n");
        assert!(zed.error.is_none() && zed.warnings.is_empty(), "{zed:?}");
        assert_eq!(
            zed.settings.client.editor.open("me@studio", "/src/a b#1.rs", Some(42)).as_deref(),
            Some("zed://ssh/me@studio/src/a%20b%231.rs:42")
        );
        assert_eq!(
            zed.settings.client.editor.open("studio", "/src", None).as_deref(),
            Some("zed://ssh/studio/src:1"),
            "no line in view is the first"
        );
        for refused in ["zed://ssh/{host}", "/usr/local/bin/zed {path}", "1x://{path}"] {
            let bad = Settings::parse(&format!("[client]\neditor = \"{refused}\"\n"));
            assert!(bad.error.is_some_and(|e| e.to_string().contains("{path}")), "{refused}");
        }
        let blank = Settings::parse("[client]\neditor = \" \"\n");
        assert_eq!(blank.settings.client.editor.open("studio", "/a", None), None);
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

    /// `[worker.labels]` and `[worker.probes]` fed the cut placement rules and are gone: a file
    /// that still has them loads, warning of each.
    #[test]
    fn worker_labels_and_probes_are_unknown() {
        let loaded =
            Settings::parse("[worker.labels]\nrack = \"b2\"\n[worker.probes]\ncuda = \"x\"\n");
        assert!(loaded.error.is_none(), "{loaded:?}");
        assert_eq!(
            loaded.warnings,
            ["unknown key `worker.labels`", "unknown key `worker.probes`"],
            "{loaded:?}"
        );
    }

    /// A worker's own ACP agents are any name and a command line, and an empty one hides it.
    #[test]
    fn worker_acp_agents() {
        let read = Settings::parse(
            "[worker.acp]\nmine = [\"/opt/mine/bin/agent\", \"--acp\"]\ngoose = []\n",
        );
        assert!(read.error.is_none() && read.warnings.is_empty(), "{read:?}");
        let acp = &read.settings.worker.acp;
        assert_eq!(acp["mine"], ["/opt/mine/bin/agent", "--acp"]);
        assert_eq!(acp["goose"], Vec::<String>::new());
        assert!(Settings::default().worker.acp.is_empty());
        assert!(Settings::parse("[worker.acp]\nmine = \"agent\"\n").error.is_some(), "a list");
        let back = toml::to_string(&read.settings).unwrap_or_default();
        assert_eq!(Settings::parse(&back).settings.worker.acp, *acp, "written back:\n{back}");
    }

    /// The fleet's bounds on projects, each key with its default when the file leaves it out.
    #[test]
    fn server_project_bounds() {
        let d = Settings::default().server.projects;
        assert_eq!(d.live_agents, 24);
        assert!(d.permission_flags.is_empty(), "no project loosens permissions by default");
        let loaded = Settings::parse(
            "[server.projects]\nlive_agents = 40\npermission_flags = [\"nightly\"]\n",
        );
        assert!(loaded.error.is_none() && loaded.warnings.is_empty(), "{loaded:?}");
        assert_eq!(
            loaded.settings.server.projects,
            ProjectBounds { live_agents: 40, permission_flags: vec!["nightly".to_owned()] }
        );
        let gone = Settings::parse("[server.projects]\ntimeline_kept = 4096\n");
        assert_eq!(gone.warnings, ["unknown key `server.projects.timeline_kept`"]);
        let typo = Settings::parse("[server.projects]\nlive_agent = 40\n");
        assert_eq!(typo.warnings, ["unknown key `server.projects.live_agent`"]);
        assert!(Settings::parse("[server.projects]\nlive_agents = -1\n").error.is_some());
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
