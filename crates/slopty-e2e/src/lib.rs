//! The app's self-test surface.
//!
//! When `SLOPTY_TEST_SOCKET=<path>` is set, the Slopty app listens on that Unix socket and
//! answers [`Command`]s with [`Reply`]s, one JSON object per line each way. Keys and pointer
//! events go through GPUI's own dispatch (`Window::dispatch_keystroke`, `dispatch_event`),
//! pictures come from GPUI's own Metal renderer (`Window::render_to_image`), and state comes
//! back as a [`Dump`]. Nothing posts a system event and nothing captures the screen: the app
//! is driven and observed from inside, so the whole client can be tested unattended on a
//! machine that has granted no permission at all.
//!
//! The `harness` feature adds the [`Driver`] (the client side of the socket), the
//! [`harness`] (ptyd + hostd + app in a temporary directory) and [`snapshot`] (compare a
//! rendered frame with a golden PNG and write the diff for a failed one).

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

#[cfg(feature = "harness")]
pub mod driver;
#[cfg(feature = "harness")]
pub mod harness;
#[cfg(feature = "harness")]
pub mod snapshot;

#[cfg(feature = "harness")]
pub use driver::Driver;
#[cfg(feature = "harness")]
pub use harness::Stack;

/// Environment variable naming the socket the app should listen on.
pub const SOCKET_ENV: &str = "SLOPTY_TEST_SOCKET";

/// A pointer button, as the driver names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    /// Primary.
    #[default]
    Left,
    /// Secondary.
    Right,
    /// Wheel.
    Middle,
}

/// What the driver asks the app to do.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Liveness.
    Ping,
    /// Redeem a pairing ticket and connect, as the pairing panel would.
    Pair {
        /// `sloptypair…` from `slopty host ticket` or `slopty-hostd --print-ticket`.
        ticket: String,
    },
    /// Dispatch keystrokes in GPUI's binding syntax, space separated (`cmd-n`, `enter`,
    /// `ctrl-c`, `a`).
    Keys {
        /// The keystrokes.
        keys: String,
    },
    /// Type text: one keystroke per character, carrying the character as the typed text.
    Type {
        /// What to type.
        text: String,
    },
    /// Press and release a pointer button at a window point.
    Click {
        /// Window x in points.
        x: f32,
        /// Window y in points.
        y: f32,
        /// Button.
        #[serde(default)]
        button: Button,
        /// Click count (2 for a double click).
        #[serde(default = "one")]
        count: u32,
    },
    /// Move the pointer to a window point (no button).
    Move {
        /// Window x in points.
        x: f32,
        /// Window y in points.
        y: f32,
    },
    /// Scroll at a window point, in lines.
    Scroll {
        /// Window x in points.
        x: f32,
        /// Window y in points.
        y: f32,
        /// Horizontal lines.
        dx: f32,
        /// Vertical lines (positive scrolls the content down, GPUI convention).
        dy: f32,
    },
    /// Resize the window's content area.
    Resize {
        /// Width in points.
        width: f32,
        /// Height in points.
        height: f32,
    },
    /// Everything the chrome and the canvas know, as data.
    Dump,
    /// Render the current frame with the app's own renderer to a PNG at `path`.
    Render {
        /// Where to write the PNG (absolute).
        path: String,
    },
    /// Quit the app.
    Quit,
}

const fn one() -> u32 {
    1
}

/// What the app answers.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    /// Done.
    Ok,
    /// A [`Command::Dump`].
    Dump(Box<Dump>),
    /// A [`Command::Render`]: the image written, in device pixels.
    Rendered {
        /// Width.
        width: u32,
        /// Height.
        height: u32,
    },
    /// The command failed.
    Error {
        /// Why.
        message: String,
    },
}

/// A snapshot of the app's state.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, Default)]
pub struct Dump {
    /// The window.
    pub window: WindowInfo,
    /// Every paired host, in switcher order.
    pub hosts: Vec<HostInfo>,
    /// The pairing panel is showing.
    pub pairing: bool,
    /// The status text in the top bar.
    pub status: String,
    /// The transient notice in the top bar, if any.
    pub notice: Option<String>,
    /// Which kind of item holds the keyboard on the active canvas (the canvas's own notion).
    pub focus: Option<String>,
    /// GPUI's focused element: `canvas`, `terminal:<session>`, `other`, or `none`.
    pub focused: String,
    /// Camera zoom of the active canvas.
    pub zoom: f32,
    /// Items on the active canvas, by z (bottom first).
    pub items: Vec<ItemInfo>,
    /// Terminals on the active canvas.
    pub terminals: Vec<TerminalInfo>,
    /// Remote windows and displays on the active canvas.
    pub screens: Vec<ScreenInfo>,
    /// The accessibility tree GPUI built for the last frame, depth first in reading order,
    /// trimmed to what a screen reader reads. Empty when the app was built without `e2e`.
    #[serde(default)]
    pub a11y: Vec<A11yNode>,
    /// `slopty hook install` has already been offered on the active canvas.
    pub hooks_offered: bool,
}

/// One node of the accessibility tree.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, Default)]
pub struct A11yNode {
    /// The accesskit role as `Debug` prints it: `Button`, `Terminal`, `Heading`, …
    pub role: String,
    /// The label, if any.
    pub label: Option<String>,
    /// The value, if any (a terminal's cursor row, a text field's text).
    pub value: Option<String>,
    /// The node holds the keyboard focus.
    pub focused: bool,
    /// Window rect in points: x, y, w, h.
    pub bounds: [f32; 4],
}

/// The window.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, Default)]
pub struct WindowInfo {
    /// Content width in points.
    pub width: f32,
    /// Content height in points.
    pub height: f32,
    /// Device pixels per point.
    pub scale: f32,
    /// The window is the key window.
    pub active: bool,
}

/// One paired host.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct HostInfo {
    /// Display name.
    pub name: String,
    /// `connected`, `connecting…`, …
    pub status: String,
    /// Its canvas is the one shown.
    pub active: bool,
}

/// One canvas item.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, Default)]
pub struct ItemInfo {
    /// Item id.
    pub id: String,
    /// `terminal`, `window`, `display`, `note`.
    pub kind: String,
    /// Session id for terminals.
    pub session: Option<String>,
    /// Canvas rect: x, y, w, h in canvas units.
    pub rect: [f32; 4],
    /// Window rect: x, y, w, h in points (where to click).
    pub bounds: [f32; 4],
    /// The active item.
    pub active: bool,
    /// Sleeping.
    pub sleeping: bool,
}

/// One terminal.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct TerminalInfo {
    /// Session id.
    pub session: String,
    /// Title from the program, if any.
    pub title: Option<String>,
    /// Columns × rows.
    pub size: [u16; 2],
    /// Cursor column and row.
    pub cursor: [u16; 2],
    /// The visible rows, top to bottom, trailing spaces trimmed.
    pub rows: Vec<String>,
    /// The coding agent's state as the host reports it: `idle`, `working`, `tool:<name>`,
    /// `blocked:permission:<tool>`, `blocked:question`, `blocked:elicitation`,
    /// `blocked:idle`, `done`; `None` without an agent.
    pub agent: Option<String>,
    /// Which signal the host read the agent's state from: `process`, `title`, `transcript`
    /// or `hook`; `None` without an agent.
    pub agent_source: Option<String>,
    /// The agent's conversation, while it is shown in place of the grid.
    pub conversation: Option<ConversationInfo>,
}

/// One remote window or display, and how its pictures reach the screen.
///
/// The timings are microseconds so a test can assert on them without floating point. They cover
/// the client's own presentation path: `latency_*` starts at the arrival of the datagram that
/// completed the frame and ends at the paint that showed it, `interval_*` is the spacing of
/// those paints, and `skipped` / `repeats` are the two cadence faults (a frame the display never
/// saw, a paint that showed the picture already up).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct ScreenInfo {
    /// Canvas item id.
    pub item: String,
    /// Stream id.
    pub stream: u32,
    /// Stream pixel size.
    pub size: [u32; 2],
    /// Frames painted.
    pub frames: u64,
    /// Frames the pacer put on screen.
    pub presented: u64,
    /// Frames replaced before they were ever painted.
    pub skipped: u64,
    /// Paints that showed the picture already up.
    pub repeats: u64,
    /// Frames dropped as not newer than what was up.
    pub late: u64,
    /// Median arrival → present, microseconds.
    pub latency_p50_us: u64,
    /// 95th percentile of the same.
    pub latency_p95_us: u64,
    /// Worst in the window.
    pub latency_max_us: u64,
    /// Median arrival → decoded, microseconds.
    pub decode_p50_us: u64,
    /// Median gap between presented frames, microseconds.
    pub interval_p50_us: u64,
    /// Mean absolute deviation of that gap, microseconds.
    pub interval_jitter_us: u64,
    /// Frames the pacer's ring holds.
    pub window: usize,
}

/// A terminal's conversation view.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct ConversationInfo {
    /// One line per entry, oldest first: `user: …`, `assistant: …`, `thinking`,
    /// `tool <name>: <summary>`, `result <name>: <first line>` (`failed` after the name of a
    /// failed one).
    pub entries: Vec<String>,
    /// What is written in the composer.
    pub composer: String,
    /// The composer holds the keyboard.
    pub composer_focused: bool,
    /// The list follows new entries (the reader has not scrolled up).
    pub pinned: bool,
    /// The row above the composer: `permission:<tool>`, `allowed`, `denied`, `prompt`.
    pub attention: Option<String>,
}

impl Dump {
    /// Every visible terminal row that contains `needle`.
    #[must_use]
    pub fn rows_containing(&self, needle: &str) -> Vec<&str> {
        self.terminals
            .iter()
            .flat_map(|t| t.rows.iter())
            .filter(|r| r.contains(needle))
            .map(String::as_str)
            .collect()
    }

    /// The first item of `kind`.
    #[must_use]
    pub fn item(&self, kind: &str) -> Option<&ItemInfo> {
        self.items.iter().find(|i| i.kind == kind)
    }

    /// The first accessibility node with `role` and, when given, `label`.
    #[must_use]
    pub fn a11y_node(&self, role: &str, label: Option<&str>) -> Option<&A11yNode> {
        self.a11y
            .iter()
            .find(|n| n.role == role && label.is_none_or(|l| n.label.as_deref() == Some(l)))
    }
}

impl ItemInfo {
    /// The window point at the middle of the item.
    #[must_use]
    pub const fn center(&self) -> (f32, f32) {
        let [x, y, w, h] = self.bounds;
        (x + w / 2.0, y + h / 2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip_as_tagged_json() {
        let cmd = Command::Click { x: 1.5, y: 2.0, button: Button::Right, count: 2 };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"cmd\":\"click\""), "{json}");
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
        // Defaults keep the driver's JSON short.
        let short: Command = serde_json::from_str(r#"{"cmd":"click","x":1,"y":2}"#).unwrap();
        assert_eq!(short, Command::Click { x: 1.0, y: 2.0, button: Button::Left, count: 1 });
    }

    #[test]
    fn dump_helpers_find_rows_and_items() {
        let dump = Dump {
            items: vec![ItemInfo {
                kind: "terminal".into(),
                bounds: [10.0, 20.0, 100.0, 50.0],
                ..ItemInfo::default()
            }],
            terminals: vec![TerminalInfo {
                rows: vec!["$ echo hi".into(), "hi".into(), String::new()],
                ..TerminalInfo::default()
            }],
            ..Dump::default()
        };
        assert_eq!(dump.rows_containing("hi"), ["$ echo hi", "hi"]);
        assert_eq!(dump.item("terminal").unwrap().center(), (60.0, 45.0));
        assert!(dump.item("note").is_none());
    }
}
