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
    /// Put a picture on the app's clipboard through GPUI, as a screenshot would be. For the
    /// simulator, whose pasteboard is its own: the Mac's is shared with every other app, so
    /// the Mac scenario uses `Attach` instead.
    Clipboard {
        /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
        media_type: String,
        /// The encoded picture, base64.
        data: String,
    },
    /// Attach a picture to the active driven conversation's next prompt, as pasting one into
    /// its composer would (the paste itself reads the system clipboard, which a test must
    /// not touch; the headless layer covers that read).
    Attach {
        /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
        media_type: String,
        /// The encoded picture, base64.
        data: String,
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
    /// Press the primary button at a window point, move to another and release: a title-bar
    /// drag moves an item, a corner drag resizes one.
    Drag {
        /// Where the button goes down, x in points.
        x: f32,
        /// Where the button goes down, y in points.
        y: f32,
        /// Where it comes up, x in points.
        to_x: f32,
        /// Where it comes up, y in points.
        to_y: f32,
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
        /// Hold ⌘ (the platform modifier): on the canvas that zooms instead of panning.
        #[serde(default)]
        zoom: bool,
    },
    /// Open `count` sessions running `command` (the login shell when empty) on the active
    /// canvas, as ⌘N does; the host places them. Load for the frame-time scenarios without
    /// typing anything into a shell.
    Open {
        /// Program and arguments.
        #[serde(default)]
        command: Vec<String>,
        /// How many.
        #[serde(default = "one")]
        count: u32,
    },
    /// Open a Claude Code agent the host drives over its structured protocol (a conversation
    /// card, `kind: agent`), as ⌘⌥T does, in `cwd` or the host's default.
    OpenAgent {
        /// Working directory.
        #[serde(default)]
        cwd: Option<String>,
        /// A Claude Code session id to resume, as the resume picker would.
        #[serde(default)]
        resume: Option<String>,
    },
    /// Start a fresh frame-time measurement window ([`FrameInfo`] in the next dumps).
    FramesReset,
    /// Bring a session's terminal into view, make it active and give it the keyboard, as
    /// tapping a waiting badge does. On a phone this zooms the card up to a live grid (clamped
    /// to `CARD_ZOOM`), which a plain click cannot, so the soft keyboard can route to it.
    Reveal {
        /// The session id (`terminal:<id>` without the prefix), as the dump reports it.
        session: String,
    },
    /// Add the host's first display to the active canvas, as picking it would (the host
    /// needs Screen Recording permission; the stream opens when the item lands).
    AddDisplay,
    /// Drive the app's system-notification response path with `tag` (a session UUID) and an
    /// optional `action`, exactly as `cx.on_system_notification_response` would when the user
    /// activates an agent banner: find the host whose canvas holds the session, switch to it,
    /// then reveal the session (no action) or answer its prompt (`allow` / `deny`). System
    /// notifications are disabled outside a bundle, so this is the only way to test the path.
    NotificationResponse {
        /// The banner's tag, which is the session UUID.
        tag: String,
        /// The button pressed: `allow`, `deny`, or none to reveal (the banner body).
        #[serde(default)]
        action: Option<String>,
    },
    /// Resize the window's content area.
    Resize {
        /// Width in points.
        width: f32,
        /// Height in points.
        height: f32,
    },
    /// A hardware key press delivered at the UIKit boundary (iOS only): what the metal
    /// view's `pressesBegan:` / `pressesEnded:` reads out of the `UIPress`, run through the
    /// same delivery; a plain key while a text input is up is typed through `insertText:`
    /// the way UIKit's text system would. `usage` is the USB HID usage (`hid`), `modifiers`
    /// the chord in GPUI's syntax (`cmd-shift`, empty for none). macOS answers an error.
    UiKeyPress {
        /// `UIKey.keyCode`.
        usage: u32,
        /// `cmd`, `ctrl`, `alt`, `shift`, `capslock`, joined by `-`.
        #[serde(default)]
        modifiers: String,
        /// Which callback.
        phase: UiPressPhase,
    },
    /// One `touches…:withEvent:` set delivered at the UIKit boundary (iOS only): every
    /// touch in it with the same phase, as UIKit reports a set. Ids identify fingers across
    /// phases. GPUI's own recognizer turns them into taps, pans, long presses and drags.
    UiTouch {
        /// The fingers.
        touches: Vec<UiTouchPoint>,
        /// `UITouch.phase` of every touch in the set.
        phase: UiTouchPhase,
    },
    /// One report of the metal view's `UIPinchGestureRecognizer` (iOS only): `scale` is the
    /// change since the previous report (the view resets the recognizer to 1 after each), so
    /// a whole pinch is the product of its steps.
    UiPinch {
        /// `recognizer.scale`.
        scale: f32,
        /// `locationInView:` x in points.
        x: f32,
        /// `locationInView:` y in points.
        y: f32,
        /// `recognizer.state`.
        phase: UiGesturePhase,
    },
    /// The text system's `insertText:` on the text input view (iOS only): what the soft
    /// keyboard delivers, IME included.
    UiInsertText {
        /// The text.
        text: String,
    },
    /// The text system's `deleteBackward` on the text input view (iOS only).
    UiDeleteBackward,
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

/// Which `presses…:withEvent:` callback a described key press enters.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiPressPhase {
    /// `pressesBegan:`.
    Began,
    /// `pressesEnded:`.
    Ended,
    /// `pressesCancelled:`.
    Cancelled,
}

/// `UITouchPhase`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiTouchPhase {
    /// `touchesBegan:`.
    Began,
    /// `touchesMoved:`.
    Moved,
    /// A touch in a `touchesMoved:` set that did not move itself.
    Stationary,
    /// `touchesEnded:`.
    Ended,
    /// `touchesCancelled:`.
    Cancelled,
}

/// `UIGestureRecognizerState` while a continuous recognizer reports to its target.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiGesturePhase {
    /// `UIGestureRecognizerStateBegan`.
    Began,
    /// `UIGestureRecognizerStateChanged`.
    Changed,
    /// `UIGestureRecognizerStateEnded`.
    Ended,
    /// `UIGestureRecognizerStateCancelled`.
    Cancelled,
}

/// One finger of a [`Command::UiTouch`] set.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct UiTouchPoint {
    /// The finger, stable from its `began` to its `ended`.
    pub id: u64,
    /// `locationInView:` x in points.
    pub x: f32,
    /// `locationInView:` y in points.
    pub y: f32,
}

/// USB HID usages of the keyboard page (`UIKeyboardHIDUsage`), as a test names keys.
pub mod hid {
    /// The usage for a key in GPUI's name (`a`, `1`, `up`, `enter`, `escape`, `-`, `/`…), or
    /// `None` for a key a US keyboard does not have a usage for.
    #[must_use]
    pub fn usage(key: &str) -> Option<u32> {
        let mut chars = key.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return Some(match c {
                'a'..='z' => 0x04_u32.saturating_add(u32::from(c).saturating_sub(u32::from('a'))),
                '1'..='9' => 0x1E_u32.saturating_add(u32::from(c).saturating_sub(u32::from('1'))),
                '0' => 0x27,
                ' ' => 0x2C,
                '-' => 0x2D,
                '=' => 0x2E,
                '[' => 0x2F,
                ']' => 0x30,
                '\\' => 0x31,
                ';' => 0x33,
                '\'' => 0x34,
                '`' => 0x35,
                ',' => 0x36,
                '.' => 0x37,
                '/' => 0x38,
                _ => return None,
            });
        }
        Some(match key {
            "enter" => 0x28,
            "escape" => 0x29,
            "backspace" => 0x2A,
            "tab" => 0x2B,
            "space" => 0x2C,
            "capslock" => 0x39,
            "f1" => 0x3A,
            "f2" => 0x3B,
            "f3" => 0x3C,
            "f4" => 0x3D,
            "f5" => 0x3E,
            "f6" => 0x3F,
            "f7" => 0x40,
            "f8" => 0x41,
            "f9" => 0x42,
            "f10" => 0x43,
            "f11" => 0x44,
            "f12" => 0x45,
            "insert" => 0x49,
            "home" => 0x4A,
            "pageup" => 0x4B,
            "delete" => 0x4C,
            "end" => 0x4D,
            "pagedown" => 0x4E,
            "right" => 0x4F,
            "left" => 0x50,
            "down" => 0x51,
            "up" => 0x52,
            _ => return None,
        })
    }

    /// A keystroke in GPUI's binding syntax (`cmd-shift-l`, `up`, `ctrl-c`) split into the
    /// key's usage and its modifiers (`cmd-shift`), or `None` when the key has no usage.
    #[must_use]
    pub fn chord(keystroke: &str) -> Option<(u32, String)> {
        let (modifiers, key) = keystroke.rsplit_once('-').unwrap_or(("", keystroke));
        // A bare `-` is the minus key ("cmd--" is command + minus).
        let (modifiers, key) =
            if key.is_empty() { (modifiers.trim_end_matches('-'), "-") } else { (modifiers, key) };
        usage(key).map(|usage| (usage, modifiers.to_owned()))
    }
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
    /// The UI's frame times since the last [`Command::FramesReset`].
    pub frames: FrameInfo,
    /// This app's client id on the wire (what the host's `screens` listing names).
    #[serde(default)]
    pub client: String,
}

/// The UI frame-time probe (`slopty_ui::frames`), in microseconds.
///
/// How long the window took to draw each frame and how evenly frames came, over the last 1024
/// frames; the counters run since the last reset. `dropped` counts display slots lost to draws
/// that ran past the period (a 30 ms draw at 60 Hz loses one); an idle app drops nothing.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct FrameInfo {
    /// Frames drawn.
    pub frames: u64,
    /// Frames whose draw took longer than the display period.
    pub over_budget: u64,
    /// Display slots lost to long draws.
    pub dropped: u64,
    /// Draw, median.
    pub draw_p50_us: u64,
    /// Draw, 95th percentile.
    pub draw_p95_us: u64,
    /// Draw, 99th percentile.
    pub draw_p99_us: u64,
    /// Worst draw in the ring.
    pub draw_max_us: u64,
    /// Interval between frames, median.
    pub interval_p50_us: u64,
    /// Interval, 95th percentile.
    pub interval_p95_us: u64,
    /// Interval, 99th percentile.
    pub interval_p99_us: u64,
    /// The display period the counters are measured against.
    pub nominal_us: u64,
}

impl FrameInfo {
    /// A duration in milliseconds, for the tables.
    #[must_use]
    pub fn ms(us: u64) -> f64 {
        #[expect(clippy::cast_precision_loss, reason = "microseconds well below 2^53")]
        let out = us as f64 / 1e3;
        out
    }

    /// One table row: `p50 / p95 / p99 / max ms · every p50 / p95 ms · n frames, over, dropped`.
    #[must_use]
    pub fn row(&self) -> String {
        format!(
            "draw {:.1} / {:.1} / {:.1} / {:.1} ms · every {:.1} / {:.1} ms · {} frames, {} over {:.1} ms, {} dropped",
            Self::ms(self.draw_p50_us),
            Self::ms(self.draw_p95_us),
            Self::ms(self.draw_p99_us),
            Self::ms(self.draw_max_us),
            Self::ms(self.interval_p50_us),
            Self::ms(self.interval_p95_us),
            self.frames,
            self.over_budget,
            Self::ms(self.nominal_us),
            self.dropped,
        )
    }
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
    /// Agents on this host waiting on the human; the pill and the Dock badge sum this over
    /// every host.
    #[serde(default)]
    pub needs_you: usize,
    /// Link round trip in microseconds, sampled once a second while connected (the bar's
    /// readout); `None` before the first sample.
    #[serde(default)]
    pub rtt_us: Option<u64>,
    /// Whether this link's path goes through a relay rather than direct; `None` before the
    /// first sample.
    #[serde(default)]
    pub relayed: Option<bool>,
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
    /// A note's text, as the document holds it.
    #[serde(default)]
    pub note: Option<String>,
}

/// One terminal.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, Default)]
pub struct TerminalInfo {
    /// Session id.
    pub session: String,
    /// `terminal` (a grid), or `agent` (a session the host drives: the conversation is the
    /// whole card).
    pub kind: String,
    /// What the title bar says: the program's title, else the session's, else "shell".
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
    /// What the host says the agent is doing ("thinking…", "calling Write…", a prompt's
    /// first line, a permission's summary); `None` without one.
    #[serde(default)]
    pub agent_detail: Option<String>,
    /// Which signal the host read the agent's state from: `process`, `title`, `transcript`
    /// or `hook`; `None` without an agent.
    pub agent_source: Option<String>,
    /// The agent's conversation, while it is shown in place of the grid.
    pub conversation: Option<ConversationInfo>,
    /// Keystroke → paint, for keys typed into this terminal.
    pub latency: LatencyInfo,
    /// What the terminal font said about itself, once the grid has been laid out.
    pub face: Option<FaceInfo>,
    /// This client drives the PTY size (the other clients wear the "take" pill).
    #[serde(default)]
    pub driving: bool,
}

/// Keystroke → paint (`slopty_ui::terminal::latency`), microseconds, over the last 256 keys.
///
/// `echo_*` runs from the key to the paint of the first frame the host produced after applying
/// it; `predicted_*` from the key to the paint that showed the local-echo guess, counted only
/// while the predictor was drawing.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct LatencyInfo {
    /// Keys echoed.
    pub echoed: u64,
    /// Key → echo paint, median.
    pub echo_p50_us: u64,
    /// Same, 95th percentile.
    pub echo_p95_us: u64,
    /// Same, worst.
    pub echo_max_us: u64,
    /// Keys predicted.
    pub predicted: u64,
    /// Key → predicted paint, median.
    pub predicted_p50_us: u64,
    /// Same, 95th percentile.
    pub predicted_p95_us: u64,
    /// Same, worst.
    pub predicted_max_us: u64,
}

impl LatencyInfo {
    /// One table row.
    #[must_use]
    pub fn row(&self) -> String {
        let ms = FrameInfo::ms;
        format!(
            "echo {:.1} / {:.1} / {:.1} ms ({} keys) · predicted {:.1} / {:.1} / {:.1} ms ({} keys)",
            ms(self.echo_p50_us),
            ms(self.echo_p95_us),
            ms(self.echo_max_us),
            self.echoed,
            ms(self.predicted_p50_us),
            ms(self.predicted_p95_us),
            ms(self.predicted_max_us),
            self.predicted,
        )
    }
}

/// The face the grid was derived from, in device pixels at `size` pixels per em: the font's
/// own numbers where it has them, `None` where ghostty's estimate stood in.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, Default)]
pub struct FaceInfo {
    /// Device pixels per em the face was measured at (font size × display scale).
    pub size: f32,
    /// Widest printable-ASCII advance.
    pub cell_width: f32,
    /// Typographic ascent (positive) and descent (negative).
    pub ascent: f32,
    /// Typographic ascent (positive) and descent (negative).
    pub descent: f32,
    /// Line gap (`hhea` leading), 0 when the font has none.
    pub line_gap: f32,
    /// Top of the underline stroke relative to the baseline (negative below), from `post`.
    pub underline_position: Option<f32>,
    /// Underline thickness, from `post`.
    pub underline_thickness: Option<f32>,
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
    /// What the host last said about the capture target: `live` (it is drawing, or has drawn)
    /// or `idle` (it has produced no frame at all, so no refresh can help).
    #[serde(default)]
    pub source: String,
    /// Loss-recovery counters from the client's `ScreenStats`, for the injected-loss table.
    #[serde(default)]
    pub recovery: RecoveryInfo,
}

/// The client's loss-recovery counters for one stream.
///
/// A subset of `slopty_client`'s `ScreenStats`, so an app self-test can build the injected-loss
/// table the in-process host test does. All are cumulative over the stream's life.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct RecoveryInfo {
    /// Frames delivered to the decoder.
    pub frames: u64,
    /// Frames recovered by parity (FEC).
    pub frames_fec: u64,
    /// Frames that needed a retransmission (NACK).
    pub frames_retransmit: u64,
    /// Frames given up on.
    pub frames_lost: u64,
    /// Data fragments that never arrived.
    pub datagrams_lost: u64,
    /// Data fragments the host cut the frames into.
    pub data_shards: u64,
    /// Parity fragments the host added.
    pub parity_shards: u64,
    /// Parity as observed on the wire, thousandths of the data fragments.
    pub parity_permille: u16,
    /// NACKs sent.
    pub nacks: u64,
    /// Refresh requests sent.
    pub refreshes: u64,
    /// Datagrams seen.
    pub datagrams: u64,
    /// Bytes received in datagrams (video, cursor, audio, parity).
    pub bytes: u64,
    /// Stalls that released (silence past the stall gap, then datagrams again).
    pub stalls: u64,
    /// Time spent stalled, milliseconds.
    pub stalled_ms: u64,
    /// Opus packets played.
    pub audio_packets: u64,
    /// Opus packets missing from the sequence.
    pub audio_lost: u64,
    /// Lost packets papered over with the previous one fading out.
    pub audio_concealed: u64,
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
    /// The row above the composer: `permission:<tool>`, `allowed`, `denied`, `question`,
    /// `answered`, `prompt`.
    pub attention: Option<String>,
    /// What a driven agent is writing now (empty otherwise).
    pub partial: String,
    /// The permission a driven agent waits on, `<tool>:<summary>`.
    pub permission: Option<String>,
    /// The model the driven agent named (`AgentInfo::model`).
    #[serde(default)]
    pub model: Option<String>,
    /// The permission mode the driven agent named.
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// Claude Code's own session id, once the agent said it.
    #[serde(default)]
    pub agent_session: Option<String>,
    /// The slash commands the driven agent announced.
    #[serde(default)]
    pub slash_commands: Vec<String>,
    /// Turns so far.
    #[serde(default)]
    pub turns: u32,
    /// The slash commands completing the composer's text right now, as listed above it.
    #[serde(default)]
    pub completions: Vec<String>,
    /// The model menu is open under the header.
    #[serde(default)]
    pub model_menu: bool,
    /// The option labels of a pending question, every question's in order.
    #[serde(default)]
    pub question_options: Vec<String>,
    /// What the pending permission's "Always" would do, when the agent suggested one.
    #[serde(default)]
    pub always: Option<String>,
    /// The subagents the agent spawned, `<call>:<description>:<tool uses>:<running|done>`.
    #[serde(default)]
    pub tasks: Vec<String>,
    /// The subscription's windows as the header shows them ("5h 23% · 7d 74%").
    #[serde(default)]
    pub usage: Option<String>,
    /// The pictures waiting to go with the next prompt, as their chips name them
    /// ("PNG · 70 B").
    #[serde(default)]
    pub attachments: Vec<String>,
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

    /// The terminal showing `session`.
    #[must_use]
    pub fn terminal(&self, session: &str) -> Option<&TerminalInfo> {
        self.terminals.iter().find(|t| t.session == session)
    }

    /// The item showing `session`.
    #[must_use]
    pub fn item_for_session(&self, session: &str) -> Option<&ItemInfo> {
        self.items.iter().find(|i| i.session.as_deref() == Some(session))
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
    fn hid_usages_follow_the_keyboard_page() {
        assert_eq!(hid::usage("a"), Some(0x04));
        assert_eq!(hid::usage("l"), Some(0x0F));
        assert_eq!(hid::usage("z"), Some(0x1D));
        assert_eq!(hid::usage("1"), Some(0x1E));
        assert_eq!(hid::usage("0"), Some(0x27));
        assert_eq!(hid::usage("up"), Some(0x52));
        assert_eq!(hid::usage("enter"), Some(0x28));
        assert_eq!(hid::usage("-"), Some(0x2D));
        assert_eq!(hid::usage("é"), None);
        assert_eq!(hid::usage("fn"), None);
        assert_eq!(hid::chord("cmd-shift-l"), Some((0x0F, "cmd-shift".to_owned())));
        assert_eq!(hid::chord("up"), Some((0x52, String::new())));
        assert_eq!(hid::chord("ctrl-c"), Some((0x06, "ctrl".to_owned())));
        assert_eq!(hid::chord("cmd--"), Some((0x2D, "cmd".to_owned())));
        assert_eq!(hid::chord("-"), Some((0x2D, String::new())));
        assert_eq!(hid::chord("cmd-fn"), None);
        let cmd = Command::UiKeyPress {
            usage: 0x0F,
            modifiers: "cmd-shift".into(),
            phase: UiPressPhase::Began,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"cmd\":\"ui_key_press\"") && json.contains("\"began\""), "{json}");
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
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
