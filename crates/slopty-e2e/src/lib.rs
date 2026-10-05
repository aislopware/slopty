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
//! [`harness`] (ptyd + worker + app in a temporary directory) and [`snapshot`] (compare a
//! rendered frame with a golden PNG and write the diff for a failed one).

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

use serde::{Deserialize, Serialize};

#[cfg(feature = "harness")]
pub mod cut;
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

/// Environment variable naming a file of Tailscale `Status` JSON for the e2e build's app.
///
/// The app reads it in place of this machine's tailnet, so its panel offers what the test put
/// there and never what the machine's real tailnet answers.
pub const TAILNET_STATUS_ENV: &str = "SLOPTY_TAILNET_STATUS";

/// Environment variable holding a worker's `doctor` report (`slopty_proto::ctl::Health` as
/// JSON) for the e2e build's app on a Mac.
///
/// With it set, "Use this Mac as a worker" runs against a stand-in that installs nothing,
/// answers with this report and adds nothing, so the checklist can be drawn in any state.
/// Without it the self-test offers no such entry.
pub const THIS_MAC_ENV: &str = "SLOPTY_THIS_MAC";

/// Environment variable naming the macOS permissions the e2e build's app takes every worker to
/// hold: grant names ([`SCREEN_RECORDING`], [`ACCESSIBILITY`]) joined by commas, none when empty.
///
/// A worker reports what this machine granted its binary, which differs from Mac to Mac and
/// from build to build, and the navigator says when a grant is missing. With this set, the app
/// sees the grants the test chose and a render is the same on every machine.
pub const WORKER_GRANTS_ENV: &str = "SLOPTY_WORKER_GRANTS";

/// Screen Recording, in [`WORKER_GRANTS_ENV`].
pub const SCREEN_RECORDING: &str = "screen-recording";

/// Accessibility, in [`WORKER_GRANTS_ENV`].
pub const ACCESSIBILITY: &str = "accessibility";

/// Whether a [`WORKER_GRANTS_ENV`] value names `grant`: a whole name, not a prefix of one.
#[must_use]
pub fn granted(grants: &str, grant: &str) -> bool {
    grants.split(',').any(|g| g.trim() == grant)
}

/// Environment variable that, set, has the e2e build's app hold every upload before its first
/// byte.
///
/// How far an upload got by a given frame is up to the machine's speed; held, a drawn upload
/// reads 0% on every machine. A cancel still stops a held upload.
pub const HOLD_UPLOADS_ENV: &str = "SLOPTY_HOLD_UPLOADS";

/// The round trip the app under test shows in its readouts, whatever its link measures.
///
/// A local link's, under the figure a readout names (the navigator, the status bar, the
/// palette), so a golden never depends on how busy the machine was. The dump's `rtt_us` stays
/// the live one.
pub const SHOWN_RTT: std::time::Duration = std::time::Duration::from_millis(1);

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
    /// Hand the app a link the system opened it with, where the iOS scene's delegate hands it
    /// (`slopty_app::open_link`). `simctl openurl` would come through an "Open in Slopty?"
    /// confirmation that only a person may answer. On a device, the person's tap on the
    /// Camera's banner is that answer.
    OpenLink {
        /// The link.
        url: String,
    },
    /// Hand the app a resume, as the system's watch would (`slopty_platform::resume`):
    /// `woke`, `screens-woke`, `session-active`, `unlocked`, `foreground` or `path-changed`.
    /// Nothing sleeps and no network changes.
    Resume {
        /// The resume's name.
        what: String,
    },
    /// Derive the chrome as the system's Increase Contrast would have it, on or off. The app's
    /// own setting is stood in for; this Mac's is never read or changed.
    Contrast {
        /// Increase Contrast is on.
        increased: bool,
    },
    /// Ask the server to forget a worker it lists as not online, as the hosts popover's Forget
    /// does; it leaves the app when the directory unlists it.
    ForgetWorker {
        /// Its id, as the worker writes it.
        id: String,
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
    /// Put a picture on the app's clipboard through GPUI, as a screenshot would be. The
    /// simulator only, whose pasteboard is its own; the Mac app answers an error, since its
    /// clipboard is a named pasteboard the test reads and writes itself (`SLOPTY_PASTEBOARD`).
    Clipboard {
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
    /// drag moves an item, a corner drag resizes one, a ⌘-drag on a terminal path drags the
    /// file out.
    Drag {
        /// Where the button goes down, x in points.
        x: f32,
        /// Where the button goes down, y in points.
        y: f32,
        /// Where it comes up, x in points.
        to_x: f32,
        /// Where it comes up, y in points.
        to_y: f32,
        /// ⌘ is held throughout.
        command: bool,
    },
    /// Keep the file promises of the last drag out of the app, as a drop in the directory
    /// `into` would: each promise's own keeper writes its file there. Under the self-test a
    /// drag out parks its promises here instead of starting a system drag. macOS only.
    KeepDragged {
        /// An existing directory on this machine.
        into: String,
    },
    /// Drop files on a window point, as Finder's drag would end there: GPUI's own file-drop
    /// events (entered, over, dropped) through `Window::dispatch_event`, no system drag.
    DropFiles {
        /// Absolute paths on this machine.
        paths: Vec<String>,
        /// Window x in points.
        x: f32,
        /// Window y in points.
        y: f32,
    },
    /// Carry a drag of files and texts from this machine to a window point, as the platform's
    /// drag destination hands the workspace each step of a system drag (`DropSink::over`): they
    /// sit on a pasteboard of the command's own, and no system drag runs. Over a remote tile
    /// the drag goes on to the worker. The first step carries them; while the drag stays on the
    /// same tile they are not read again. Answers [`Reply::Over`]. macOS only.
    DragOver {
        /// Absolute paths on this machine, an item each.
        paths: Vec<String>,
        /// Texts, an item of plain text each, after the files.
        texts: Vec<String>,
        /// Window x in points.
        x: f32,
        /// Window y in points.
        y: f32,
    },
    /// Let go of the drag [`Command::DragOver`] carries at a window point, as the platform's
    /// `performDragOperation:` would (`DropSink::dropped`): answers [`Reply::Taken`]. macOS
    /// only.
    DragDrop {
        /// Window x in points.
        x: f32,
        /// Window y in points.
        y: f32,
    },
    /// The drag [`Command::DragOver`] carries leaves the window (`DropSink::left`). macOS only.
    DragLeave,
    /// Move the pointer to a window point (no button).
    Move {
        /// Window x in points.
        x: f32,
        /// Window y in points.
        y: f32,
    },
    /// Draw every time readout (a turn's elapsed time, a record's stamp, an author's age) as at
    /// `at_ms`, Unix milliseconds, from now on, or by the system's clock again for `None`: a
    /// golden that shows a time then holds the same one whenever it is taken.
    PinClock {
        /// The moment, or `None` to let the clock run.
        at_ms: Option<u64>,
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
    /// Open `count` sessions running `command` (the login shell when empty) in the active
    /// workspace, as ⌘N does. Load for the frame-time scenarios without
    /// typing anything into a shell.
    Open {
        /// Program and arguments.
        #[serde(default)]
        command: Vec<String>,
        /// How many.
        #[serde(default = "one")]
        count: u32,
    },
    /// Open a file tile for `path` on the worker, as "view" on a tool call does, landing on
    /// `line` when given.
    OpenFile {
        /// Absolute path on the worker.
        path: String,
        /// 1-based line to land on.
        #[serde(default)]
        line: Option<u32>,
    },
    /// Open a browser tile for `url` on the context worker, as the palette's "Open URL…"
    /// does.
    OpenUrl {
        /// An http or https address.
        url: String,
    },
    /// Deliver `keys` (a chord such as `cmd-a` or `cmd-shift-z`) to the window of the page of
    /// the browser tile for `url`, while the page holds the keyboard, as AppKit delivers a key
    /// press: through the application's key equivalents and menu bar when the window is key,
    /// else to the window itself. Replies with an error when the page does not hold the
    /// keyboard (macOS).
    PageKeys {
        /// The tile's address, as its item names it.
        url: String,
        /// The chord.
        keys: String,
    },
    /// Start a fresh frame-time measurement window ([`FrameInfo`] in the next dumps).
    FramesReset,
    /// Bring a session's terminal into view, make it active and give it the keyboard, as
    /// tapping a waiting badge does, so the soft keyboard can route to it.
    Reveal {
        /// The session id (`terminal:<id>` without the prefix), as the dump reports it.
        session: String,
    },
    /// Add the worker's first display to the active workspace, as picking it would (the worker
    /// needs Screen Recording permission; the stream opens when the item lands).
    AddDisplay,
    /// Put a window of the worker in the strip, as picking it in the ⌘O picker does. The id
    /// need not name a live window: one that never sends a frame is how a remote tile's
    /// placeholder is reached on a machine that has granted no screen capture.
    PickWindow {
        /// The worker's window id.
        window: u32,
        /// The title the tile shows.
        title: String,
    },
    /// Drive the app's system-notification response path with `tag` (a session UUID),
    /// exactly as `cx.on_system_notification_response` would when the user activates an
    /// agent banner: reveal the session in whichever workspace holds its tile. System
    /// notifications are disabled outside a bundle, so this is the only way to test the path.
    NotificationResponse {
        /// The banner's tag, which is the session UUID.
        tag: String,
    },
    /// Keep the app's main thread busy for `ms` milliseconds, as a hang does: nothing is drawn
    /// and no input is answered meanwhile. The hang monitor's case.
    HoldMain {
        /// How long, in milliseconds.
        ms: u64,
    },
    /// Resize the window's content area. iOS cannot resize its window from inside the app, so
    /// there the app lays itself out in this size at the window's top left: the stand-in for
    /// Split View and Stage Manager (the full size ends it).
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
    /// Everything the chrome and the workspace know, as data.
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
                '1'..='9' => 0x1e_u32.saturating_add(u32::from(c).saturating_sub(u32::from('1'))),
                '0' => 0x27,
                ' ' => 0x2c,
                '-' => 0x2d,
                '=' => 0x2e,
                '[' => 0x2f,
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
            "backspace" => 0x2a,
            "tab" => 0x2b,
            "space" => 0x2c,
            "capslock" => 0x39,
            "f1" => 0x3a,
            "f2" => 0x3b,
            "f3" => 0x3c,
            "f4" => 0x3d,
            "f5" => 0x3e,
            "f6" => 0x3f,
            "f7" => 0x40,
            "f8" => 0x41,
            "f9" => 0x42,
            "f10" => 0x43,
            "f11" => 0x44,
            "f12" => 0x45,
            "insert" => 0x49,
            "home" => 0x4a,
            "pageup" => 0x4b,
            "delete" => 0x4c,
            "end" => 0x4d,
            "pagedown" => 0x4e,
            "right" => 0x4f,
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
    /// A [`Command::Render`]: the image written, in device pixels, and what the same frame
    /// says in words: its accessibility tree, read from a frame of the same state drawn right
    /// after it.
    Rendered {
        /// Width.
        width: u32,
        /// Height.
        height: u32,
        /// The frame's accessibility tree, in reading order.
        a11y: Vec<A11yNode>,
        /// Device pixels per point.
        scale: f32,
    },
    /// A [`Command::DragOver`]: what the drag is over.
    Over {
        /// `local` over the app's own (GPUI takes it); over a remote tile, what a drop there
        /// would do as the worker last said: `none`, `copy`, `link` or `move`.
        op: String,
        /// The drag the window carries to the worker, if it is.
        drag: Option<String>,
    },
    /// A [`Command::DragDrop`]: whether the drop was taken (a refused one slides back).
    Taken {
        /// Taken.
        taken: bool,
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
    /// Every added worker.
    pub workers: Vec<WorkerInfo>,
    /// The add-worker panel is showing.
    pub adding: bool,
    /// `connected` while every worker is, else the first other worker's state; `no workers`
    /// with none.
    pub status: String,
    /// The toast over the workspace, if any.
    pub notice: Option<String>,
    /// Which kind of tile holds the keyboard (the workspace's own notion).
    pub focus: Option<String>,
    /// Who has the keyboard: `workspace`, `terminal:<session>`, `thread:<session>` (its
    /// agent's thread, shown in place of the TUI), `screen:<stream>`, `file:<item>` (its
    /// editor), `browser:<item>` (the page itself), `project:<name>` (a board), `other`, or
    /// `none`.
    pub focused: String,
    /// The active workspace's name.
    #[serde(default)]
    pub workspace: String,
    /// The overview is open.
    #[serde(default)]
    pub overview: bool,
    /// The theme is the dark variant (`[theme] appearance`, or the system's under `system`).
    pub dark: bool,
    /// Every tile, workspace by workspace, column by column, top to bottom.
    pub items: Vec<ItemInfo>,
    /// Terminals with a view.
    pub terminals: Vec<TerminalInfo>,
    /// Remote windows and displays with a stream.
    pub screens: Vec<ScreenInfo>,
    /// The accessibility tree GPUI built for the last frame, depth first in reading order,
    /// trimmed to what a screen reader reads. Empty when the app was built without `e2e`.
    #[serde(default)]
    pub a11y: Vec<A11yNode>,
    /// The UI's frame times since the last [`Command::FramesReset`].
    pub frames: FrameInfo,
    /// Where the frame the app drew for this dump differs from the same state drawn from
    /// scratch: a view showing an old state. [`crate::Driver::dump`] fails on it.
    #[serde(default)]
    pub stale: Option<String>,
    /// This app's client id on the wire (what the worker's `screens` listing names).
    #[serde(default)]
    pub client: String,
    /// The server's projects as the app mirrors them, by name.
    #[serde(default)]
    pub projects: Vec<ProjectInfo>,
}

/// The UI frame-time probe (`slopty_ui::frames`), in microseconds.
///
/// How long the window took to draw each frame and how evenly frames came, over the last 1024
/// frames; the counters run since the last reset. `dropped` counts display slots lost to draws
/// that ran past the period (a 30 ms draw at 60 Hz loses one); an idle app drops nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
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

/// What a frame says in words: a line per node of its accessibility tree, in reading order.
///
/// Each line is the role, the label and the value, and a mark on the node with the keyboard.
/// Where a node sits is the picture's business, so no bounds: a golden's text changes only
/// when a word does.
#[must_use]
pub fn frame_text(a11y: &[A11yNode]) -> String {
    let mut text = String::new();
    for node in a11y {
        text.push_str(&node.role);
        for part in [&node.label, &node.value].into_iter().flatten().filter(|p| !p.is_empty()) {
            text.push_str(" \u{2502} ");
            text.push_str(&part.replace('\n', "\u{23ce}"));
        }
        if node.focused {
            text.push_str(" (focused)");
        }
        text.push('\n');
    }
    text
}

/// The window.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize, Default)]
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

/// One added worker.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct WorkerInfo {
    /// Display name.
    pub name: String,
    /// `connected`, `connecting…`, …
    pub status: String,
    /// Agents on this worker waiting on the human; the pill and the Dock badge sum this over
    /// every worker.
    #[serde(default)]
    pub needs_you: usize,
    /// Link round trip in microseconds, sampled once a second while connected (the bar's
    /// readout); `None` before the first sample.
    #[serde(default)]
    pub rtt_us: Option<u64>,
    /// This client asked the worker for its clipboard's changes (its tile has the keyboard
    /// and the app is frontmost); the worker may not have heard it yet.
    pub clipboard_watched: bool,
    /// How many links have come up to it since the app started: a relink shows here.
    #[serde(default)]
    pub links: u64,
}

/// One project as the app mirrors it, and its board.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct ProjectInfo {
    /// Its name.
    pub id: String,
    /// Its title.
    pub title: String,
    /// Its orchestrator's session, when it has one.
    pub orchestrator: Option<String>,
    /// Each lane that holds a task, left to right, with the tasks' numbers.
    pub lanes: Vec<(String, Vec<u32>)>,
    /// Each task's number and state, as the server said.
    pub tasks: Vec<(u32, String)>,
    /// How many timeline entries the app holds.
    pub timeline: usize,
    /// Its orchestrator's tile shows the board.
    pub shown: bool,
    /// The node the board's keyboard stands on: a task's number, or `orchestrator`.
    pub picked: Option<String>,
}

/// One tile.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, Default)]
pub struct ItemInfo {
    /// Item id.
    pub id: String,
    /// `terminal`, `window`, `display`, `note`, `file`, `browser`.
    pub kind: String,
    /// The worker holding it, by name.
    #[serde(default)]
    pub worker: String,
    /// Session id for terminals.
    pub session: Option<String>,
    /// Its place: workspace, column, tile in the column.
    #[serde(default)]
    pub pos: [usize; 3],
    /// Window rect: x, y, w, h in points (where to click); zero when it is not drawn.
    pub bounds: [f32; 4],
    /// The focused tile.
    pub active: bool,
    /// A file tile's path and what it shows.
    #[serde(default)]
    pub file: Option<FileItemInfo>,
    /// A browser tile's page, read from the web view itself.
    pub browser: Option<BrowserItemInfo>,
}

/// A browser tile.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct BrowserItemInfo {
    /// The address the item names, as the worker sees it.
    pub url: String,
    /// Where this client loads it: the worker's loopback port moved to the one served here.
    pub local_url: Option<String>,
    /// The page's address now, as the web view reports it, put back on the worker's port.
    pub page_url: String,
    /// The page's title, as the web view reports it.
    pub title: String,
    /// A navigation is under way.
    pub loading: bool,
    /// Why the page failed, if it did.
    pub failed: Option<String>,
    /// The web view is on screen, as the platform shows it.
    pub shown: bool,
    /// A picture of the page is ready for where the tile is drawn without it (the overview,
    /// a render).
    pub snapshot: bool,
}

/// A file tile.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct FileItemInfo {
    /// Absolute path on the worker.
    pub path: String,
    /// The tile's one-line summary ("12 lines", "missing: …", "reading…").
    pub summary: String,
    /// Lines drawn.
    pub lines: usize,
    /// The caret's line, 1-based.
    #[serde(default)]
    pub line: Option<u32>,
    /// The editor holds an edit not yet on disk (or on its way).
    pub edited: bool,
    /// What stops a save: `conflict`, or `failed: <why>`.
    pub trouble: Option<String>,
    /// Why the text cannot be edited, when it cannot.
    pub read_only: Option<String>,
    /// The editor's text, when it is short ([`FILE_TEXT_SHOWN`] bytes or fewer).
    #[serde(default)]
    pub text: Option<String>,
    /// A Markdown file shows its preview, not its source.
    #[serde(default)]
    pub previewing: bool,
}

/// The longest file text a dump carries: enough for a test's file, and no dump of a large one.
pub const FILE_TEXT_SHOWN: usize = 4096;

/// One terminal.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, Default)]
pub struct TerminalInfo {
    /// Session id.
    pub session: String,
    /// Always `terminal`.
    pub kind: String,
    /// What the title bar says: the program's title, else the session's, else "shell".
    pub title: Option<String>,
    /// Columns × rows.
    pub size: [u16; 2],
    /// Cursor column and row.
    pub cursor: [u16; 2],
    /// The cursor's shape as the program last set it (DECSCUSR), by its variant's name (`Bar`,
    /// `Block`, …): what a focused caret draws unless the theme fixes one.
    pub cursor_shape: String,
    /// The cursor stands on a prompt row (OSC 133;A) and no command runs: the shell waits for
    /// a line.
    pub at_prompt: bool,
    /// The visible rows, top to bottom, trailing spaces trimmed.
    pub rows: Vec<String>,
    /// The line-numbering epoch of the latest frame (a reflow, reset or alt-screen switch
    /// starts a new one); `None` before the first frame.
    pub epoch: Option<u32>,
    /// The coding agent's state as the worker reports it: `idle`, `working`, `tool:<name>`,
    /// `blocked:permission:<tool>`, `blocked:question`, `blocked:elicitation`,
    /// `blocked:idle`, `done`; `None` without an agent.
    pub agent: Option<String>,
    /// What the worker says the agent is doing ("thinking…", "calling Write…", a prompt's
    /// first line, a permission's summary); `None` without one.
    #[serde(default)]
    pub agent_detail: Option<String>,
    /// Which signal the worker read the agent's state from: `process`, `title`, `transcript`
    /// or `hook`; `None` without an agent.
    pub agent_source: Option<String>,
    /// Keystroke → paint, for keys typed into this terminal.
    pub latency: LatencyInfo,
    /// What the terminal font said about itself, once the grid has been laid out.
    pub face: Option<FaceInfo>,
    /// This client drives the PTY size (the other clients wear the "take" pill).
    #[serde(default)]
    pub driving: bool,
    /// Images placed on the visible grid (kitty graphics).
    #[serde(default)]
    pub images: usize,
    /// The shell's listening ports on the worker and where each is served here: `[worker
    /// port, local port]`.
    #[serde(default)]
    pub ports: Vec<[u16; 2]>,
    /// An upload dropped on the tile, as the tile shows it (`↑ 42%`); `None` without one.
    #[serde(default)]
    pub upload: Option<String>,
    /// The grid in window points, once laid out: its origin x and y, the cell width and the
    /// row height. Where a test points at a cell.
    #[serde(default)]
    pub grid: Option<[f32; 4]>,
}

impl TerminalInfo {
    /// The shell waits at its prompt with the bar zle sets while it reads a line.
    ///
    /// The prompt and that bar reach the app as separate writes (PS1, then `zle-line-init`),
    /// after the block the command ran under, so a frame taken on the prompt alone can still
    /// hold the block. A picture of a prompt waits for both.
    #[must_use]
    pub fn reads_a_line(&self) -> bool {
        self.at_prompt && self.cursor_shape == "Bar"
    }

    /// The window point at the middle of column `col` of visible row `row`, once laid out.
    #[must_use]
    pub fn cell_center(&self, col: usize, row: usize) -> Option<(f32, f32)> {
        let [x, y, width, height] = self.grid?;
        #[expect(clippy::cast_precision_loss, reason = "a column and a row of a terminal")]
        let (col, row) = (col as f32, row as f32);
        Some((width.mul_add(col + 0.5, x), height.mul_add(row + 0.5, y)))
    }
}

/// Keystroke → paint (`slopty_ui::terminal::latency`), microseconds, over the last 256 keys.
///
/// `echo_*` runs from the key to the paint of the first frame the worker produced after applying
/// it; `predicted_*` from the key to the paint that showed the local-echo guess, counted only
/// while the predictor was drawing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct LatencyInfo {
    /// Keys echoed.
    pub echoed: u64,
    /// Key → echo paint, median.
    pub echo_p50_us: u64,
    /// Same, 95th percentile.
    pub echo_p95_us: u64,
    /// Same, 99th percentile.
    pub echo_p99_us: u64,
    /// Same, worst.
    pub echo_max_us: u64,
    /// The echo's hops, `[p50, p95]` each, in the order of [`ECHO_HOPS`].
    pub echo_hops_us: [[u64; 2]; 5],
    /// Keys predicted.
    pub predicted: u64,
    /// Keys guessed at whose echo was on the first frame painted after them, ahead of the guess.
    pub echo_first: u64,
    /// Keys guessed at that a frame painted after them showed neither guessed nor echoed.
    pub guess_late: u64,
    /// Key → predicted paint, median.
    pub predicted_p50_us: u64,
    /// Same, 95th percentile.
    pub predicted_p95_us: u64,
    /// Same, 99th percentile.
    pub predicted_p99_us: u64,
    /// Same, worst.
    pub predicted_max_us: u64,
    /// The prediction's hops, `[p50, p95]` each, in the order of [`PREDICTED_HOPS`].
    pub predicted_hops_us: [[u64; 2]; 3],
}

/// An echoed key's hops: key → its echo's batch left the link (the worker's round trip and
/// the hand-over to the UI thread), → applied to the grid, → painted, → submitted to the GPU,
/// → on the glass.
pub const ECHO_HOPS: [&str; 5] =
    ["key→arrived", "arrived→applied", "applied→painted", "painted→submitted", "submitted→glass"];

/// A predicted key's hops: key → the guess painted, → submitted, → on the glass.
pub const PREDICTED_HOPS: [&str; 3] = ["key→painted", "painted→submitted", "submitted→glass"];

impl LatencyInfo {
    /// One table row.
    #[must_use]
    pub fn row(&self) -> String {
        let ms = FrameInfo::ms;
        format!(
            "echo {:.1} / {:.1} / {:.1} / {:.1} ms ({} keys) · predicted {:.1} / {:.1} / {:.1} / {:.1} ms ({} keys, {} echoed first, {} late)",
            ms(self.echo_p50_us),
            ms(self.echo_p95_us),
            ms(self.echo_p99_us),
            ms(self.echo_max_us),
            self.echoed,
            ms(self.predicted_p50_us),
            ms(self.predicted_p95_us),
            ms(self.predicted_p99_us),
            ms(self.predicted_max_us),
            self.predicted,
            self.echo_first,
            self.guess_late,
        )
    }

    /// The hops' medians and 95th percentiles, one line.
    #[must_use]
    pub fn hops(&self) -> String {
        let ms = FrameInfo::ms;
        let line = |names: &[&str], hops: &[[u64; 2]]| {
            names
                .iter()
                .zip(hops)
                .map(|(name, [p50, p95])| format!("{name} {:.1} / {:.1}", ms(*p50), ms(*p95)))
                .collect::<Vec<_>>()
                .join(" · ")
        };
        format!(
            "echo: {} | predicted: {}",
            line(&ECHO_HOPS, &self.echo_hops_us),
            line(&PREDICTED_HOPS, &self.predicted_hops_us)
        )
    }
}

/// The face the grid was derived from, in device pixels at `size` pixels per em: the font's
/// own numbers where it has them, `None` where ghostty's estimate stood in.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize, Default)]
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
/// completed the frame and ends when the window server reported the stream's layer showing it,
/// `interval_*` is the spacing of those reports, and `skipped` / `repeats` are the two cadence
/// faults (a frame the display never saw, a present that showed the picture already up).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct ScreenInfo {
    /// Item id.
    pub item: String,
    /// Stream id.
    pub stream: u32,
    /// Stream pixel size.
    pub size: [u32; 2],
    /// Pictures put up on the stream's layer.
    pub frames: u64,
    /// Pictures the layer reported shown.
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
    /// What the worker last said about the capture target: `live` (it is drawing, or has drawn)
    /// or `idle` (it has produced no frame at all, so no refresh can help).
    #[serde(default)]
    pub source: String,
    /// Loss-recovery counters from the client's `ScreenStats`, for the injected-loss table.
    #[serde(default)]
    pub recovery: RecoveryInfo,
    /// The part of the stream's layer that shows, as the window last presented it, in whole
    /// window points `[x, y, width, height]`; `None` when that frame placed it nowhere.
    #[serde(default)]
    pub layer: Option<[i32; 4]>,
}

/// The client's loss-recovery counters for one stream.
///
/// A subset of `slopty_client`'s `ScreenStats`, so an app self-test can build the injected-loss
/// table the in-process worker test does. All are cumulative over the stream's life.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
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
    /// Data fragments the worker cut the frames into.
    pub data_shards: u64,
    /// Parity fragments the worker added.
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
    /// Silences the worker's own quiet explained, outright or within the stamp's slack: not
    /// stalls.
    pub silences_worker: u64,
    /// Silences the receiver's own loop slept through: not stalls.
    pub silences_dozed: u64,
    /// Stalls the send stamps prove were spent in flight.
    pub stalls_in_flight: u64,
    /// Stalls charged for want of a stamp to read (wrapped, overtaken or absent).
    pub stalls_unread: u64,
    /// Longest silence, milliseconds.
    pub gap_ms_max: u64,
    /// Longest stretch of a silence the receiver's loop slept through, milliseconds.
    pub dozed_ms_max: u64,
    /// Opus packets played.
    pub audio_packets: u64,
    /// Opus packets missing from the sequence.
    pub audio_lost: u64,
    /// Lost packets papered over with the previous one fading out.
    pub audio_concealed: u64,
}

impl Dump {
    /// Every connected worker has had its round trip sampled, so the status bar's readout has
    /// landed: a golden taken before it holds a frame the next run may not.
    #[must_use]
    pub fn rtt_sampled(&self) -> bool {
        self.workers.iter().filter(|w| w.status == "connected").all(|w| w.rtt_us.is_some())
    }

    /// Every terminal whose cursor stands at a prompt has its caret settled there
    /// ([`TerminalInfo::reads_a_line`]): a golden of a prompt holds the caret the next run
    /// draws too, not whichever of the shell's writes had landed.
    #[must_use]
    pub fn prompts_settled(&self) -> bool {
        self.terminals.iter().all(|t| !t.at_prompt || t.reads_a_line())
    }

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

    /// The grants list names whole grants: none when empty, spaces allowed, no prefixes.
    #[test]
    fn a_worker_holds_only_the_grants_named() {
        assert!(!granted("", SCREEN_RECORDING));
        assert!(granted("screen-recording, accessibility", ACCESSIBILITY));
        assert!(granted("screen-recording", SCREEN_RECORDING));
        assert!(!granted("screen-recording", ACCESSIBILITY));
        assert!(!granted("screen", SCREEN_RECORDING));
    }

    #[test]
    fn hid_usages_follow_the_keyboard_page() {
        assert_eq!(hid::usage("a"), Some(0x04));
        assert_eq!(hid::usage("l"), Some(0x0f));
        assert_eq!(hid::usage("z"), Some(0x1d));
        assert_eq!(hid::usage("1"), Some(0x1e));
        assert_eq!(hid::usage("0"), Some(0x27));
        assert_eq!(hid::usage("up"), Some(0x52));
        assert_eq!(hid::usage("enter"), Some(0x28));
        assert_eq!(hid::usage("-"), Some(0x2d));
        assert_eq!(hid::usage("é"), None);
        assert_eq!(hid::usage("fn"), None);
        assert_eq!(hid::chord("cmd-shift-l"), Some((0x0f, "cmd-shift".to_owned())));
        assert_eq!(hid::chord("up"), Some((0x52, String::new())));
        assert_eq!(hid::chord("ctrl-c"), Some((0x06, "ctrl".to_owned())));
        assert_eq!(hid::chord("cmd--"), Some((0x2d, "cmd".to_owned())));
        assert_eq!(hid::chord("-"), Some((0x2d, String::new())));
        assert_eq!(hid::chord("cmd-fn"), None);
        let cmd = Command::UiKeyPress {
            usage: 0x0f,
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
