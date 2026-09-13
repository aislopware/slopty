//! Terminal sessions: lifecycle, input, frames.

use serde::{Deserialize, Serialize};
use slopty_core::SessionId;
use slopty_grid::{Cursor, Line, LineIndex, RowUpdate, TermModes};

use crate::input::{CellMetrics, KeyEvent, MouseEvent};

/// Terminal size in cells.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TermSize {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
    /// Client cell pixel metrics.
    pub metrics: CellMetrics,
}

impl Default for TermSize {
    fn default() -> Self {
        Self { cols: 80, rows: 24, metrics: CellMetrics { cell_width: 8, cell_height: 16 } }
    }
}

impl TermSize {
    /// Width in pixels.
    #[must_use]
    pub const fn width_px(self) -> u32 {
        (self.cols as u32).saturating_mul(self.metrics.cell_width as u32)
    }

    /// Height in pixels.
    #[must_use]
    pub const fn height_px(self) -> u32 {
        (self.rows as u32).saturating_mul(self.metrics.cell_height as u32)
    }
}

/// Request to create a session.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OpenSession {
    /// Initial size.
    pub size: TermSize,
    /// Working directory; the host's default when `None`.
    pub cwd: Option<String>,
    /// Program and arguments; the user's login shell when empty.
    pub command: Vec<String>,
    /// Extra environment.
    pub env: Vec<(String, String)>,
    /// Display name.
    pub title: Option<String>,
    /// Attach immediately on the same connection.
    pub attach: bool,
}

/// Lifecycle state of a session.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SessionState {
    /// The child is alive.
    Running,
    /// The child exited; the last screen is retained until closed.
    Exited {
        /// Exit status, or the signal number negated.
        status: i32,
    },
}

/// A session as listed by the host.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SessionSummary {
    /// Identity.
    pub id: SessionId,
    /// What runs in it: a shell in a PTY, or an agent the host drives (protocol 15).
    pub kind: SessionKind,
    /// Title (OSC 0/2, else the command).
    pub title: String,
    /// Current working directory if known (OSC 7).
    pub cwd: Option<String>,
    /// The repository [`Self::cwd`] is in, if any: the directory holding its `.git` entry.
    /// Only the host can resolve it, and a client that groups by repository must not guess.
    pub repo: Option<String>,
    /// Current size.
    pub cols: u16,
    /// Current size.
    pub rows: u16,
    /// State.
    pub state: SessionState,
    /// Number of attached clients.
    pub viewers: u16,
    /// Command line the session was started with.
    pub command: Vec<String>,
}

/// What a session is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum SessionKind {
    /// A program in a PTY: the grid, keys, the whole terminal.
    #[default]
    Terminal,
    /// A coding agent the host drives over its structured protocol: a conversation, no grid.
    /// Prompts go through `ClientMsg::AgentSay`, permissions through `AgentAnswer`, Esc
    /// through `AgentInterrupt`; `TermRequest::Close` still closes it.
    Agent,
}

/// Why a session closed.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CloseReason {
    /// A client asked.
    Requested,
    /// The child exited and the session was not retained.
    Exited,
    /// The host is shutting down.
    HostShutdown,
    /// A driven agent died on its own: nobody asked it to close and it left a non-zero
    /// status. The client shows the reason so a lost conversation is not a mystery.
    Failed {
        /// The process's exit status (`-1` when killed by a signal).
        status: i32,
        /// Its last line of stderr ("Not logged in"), empty when it said nothing.
        detail: String,
    },
}

/// Client → host, scoped to one session.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TermRequest {
    /// Attach: the host opens a session stream and sends a full frame.
    Attach {
        /// The client's size; becomes the PTY size if this client is the driver.
        size: TermSize,
    },
    /// Stop receiving frames; the session lives on.
    Detach,
    /// Terminate the child and drop the session.
    Close,
    /// Client size changed.
    Resize(TermSize),
    /// Claim or release the right to drive the PTY size.
    Drive {
        /// True to claim.
        drive: bool,
    },
    /// Key event.
    Key(KeyEvent),
    /// Pointer event.
    Mouse(MouseEvent),
    /// Paste text (host applies bracketed paste if the mode is on).
    Paste(String),
    /// Raw bytes to the PTY (tooling, tests).
    Raw(Vec<u8>),
    /// ⌘K: drop the history and repaint the prompt at the top. The host erases the
    /// scrollback as if the program had asked (`CSI 3 J`, so a replay agrees) and sends the
    /// shell ⌃L for the screen.
    Clear,
    /// Focus changed (DEC 1004).
    Focus {
        /// True when focused.
        focused: bool,
    },
    /// Ask for scrollback lines `[start, start + count)`.
    FetchLines {
        /// First absolute line.
        start: LineIndex,
        /// How many.
        count: u32,
    },
    /// Find `needle` in the whole retained history plus the screen. Case-insensitive unless
    /// the needle has an upper-case letter. Answered with `TermEvent::Matches`, or
    /// `TermEvent::SearchInvalid` when a regex does not compile.
    Search {
        /// Text (or pattern) to find; empty clears.
        needle: String,
        /// At most this many matches come back (the newest ones).
        max: u32,
        /// `needle` is a regular expression (Rust `regex` syntax, no look-around).
        regex: bool,
    },
    /// The colours this client draws the terminal with. The driver's become the terminal's
    /// defaults, so a program asking OSC 10/11/12 `?` or OSC 4 hears the colours it is
    /// actually shown in. Sent after `Attach` and whenever the theme changes.
    Colors(TermColors),
}

/// A terminal palette on the wire: what a client paints default text, the background, the
/// cursor and ANSI 0–15 with, as `[r, g, b]`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TermColors {
    /// Default text.
    pub fg: [u8; 3],
    /// Default background.
    pub bg: [u8; 3],
    /// Cursor.
    pub cursor: [u8; 3],
    /// ANSI 0–15.
    pub ansi: [[u8; 3]; 16],
}

/// One search hit, in cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SearchMatch {
    /// Absolute line.
    pub line: LineIndex,
    /// First cell.
    pub col: u16,
    /// Cells covered.
    pub len: u16,
}

/// One frame: the changed rows since the previous frame (or every row when `full`).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Frame {
    /// Monotonic per session stream. A gap means the client must request a resync.
    pub seq: u64,
    /// True when `updates` holds every row (attach, resize, resync).
    pub full: bool,
    /// Bumped whenever absolute line numbering was invalidated (reflow on resize, RIS, alternate
    /// screen switch). The client drops its line cache when this changes.
    pub epoch: u32,
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
    /// Cursor.
    pub cursor: Cursor,
    /// Modes.
    pub modes: TermModes,
    /// Oldest scrollback line still retrievable.
    pub oldest_line: LineIndex,
    /// Absolute index of the first visible row: `total_lines - rows` when at the bottom.
    pub first_visible_line: LineIndex,
    /// Total lines (history + screen).
    pub total_lines: u64,
    /// Highest key `seq` whose bytes reached the PTY before this frame was captured.
    pub input_ack: u64,
    /// Changed rows.
    pub updates: Vec<RowUpdate>,
    /// Every image placed on the visible screen (kitty graphics), in paint order.
    pub images: Vec<Placement>,
}

/// A rectangle of an image, in its pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct PixelRect {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

/// One image the program placed on the grid (kitty graphics).
///
/// Positions are cells of the viewport, sizes the cell pixels of `TermSize::metrics`, as the
/// host laid it out.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Placement {
    /// The image, as `TermEvent::Image` carried it.
    pub image: u32,
    /// The pixels' generation: a re-sent image with the same id has a newer one.
    pub generation: u64,
    /// Top-left cell column; negative when the placement starts left of the viewport.
    pub col: i32,
    /// Top-left cell row; negative when it has scrolled partly above the viewport.
    pub row: i32,
    /// Cells covered.
    pub cols: u32,
    /// Rows covered.
    pub rows: u32,
    /// Pixel offset from the cell's left edge.
    pub x_offset: u32,
    /// Pixel offset from the cell's top edge.
    pub y_offset: u32,
    /// Painted width, in cell pixels.
    pub width: u32,
    /// Painted height, in cell pixels.
    pub height: u32,
    /// The part of the image shown, in the pixels `TermEvent::Image` carried.
    pub source: PixelRect,
    /// Z order: negative sits under the text, `≥ 0` over it.
    pub z: i32,
}

/// Bytes of image pixels a client keeps for placements, and the host assumes it keeps.
///
/// The least recently placed image goes first once the budget is over. The host re-sends an
/// image it dropped from its own ledger when a placement needs it again.
pub const IMAGE_CACHE_BYTES: usize = 48 * 1024 * 1024;

/// Host → client on the session stream.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TermEvent {
    /// Grid changed.
    Frame(Frame),
    /// Scrollback lines in reply to `FetchLines` (may be shorter than asked if evicted).
    Lines {
        /// First absolute line.
        start: LineIndex,
        /// The lines.
        lines: Vec<Line>,
    },
    /// Title changed.
    Title(String),
    /// Working directory changed.
    Cwd {
        /// The new directory.
        path: String,
        /// The repository it is in, resolved by the host. `None` outside a repository.
        repo: Option<String>,
    },
    /// BEL.
    Bell,
    /// The program wrote to the system clipboard (OSC 52 / OSC 1337 Copy). Text only, at
    /// most `MAX_CLIPBOARD_BYTES`; every attached client puts it on its own clipboard.
    /// There is no read counterpart: an OSC 52 `?` is dropped on the host, by design.
    ClipboardWrite {
        /// Contents.
        text: String,
    },
    /// Child exited.
    Exited {
        /// Status.
        status: i32,
    },
    /// The PTY size changed (another client drives it).
    Resized {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// Driver changed.
    Driver {
        /// True when this client now drives the size.
        you: bool,
    },
    /// Something went wrong with a request.
    Error(String),
    /// Reply to `TermRequest::Search`.
    Matches {
        /// The needle these are for (replies can cross in flight).
        needle: String,
        /// Every hit in the retained history and screen, even those not listed.
        total: u32,
        /// The newest hits, oldest first.
        matches: Vec<SearchMatch>,
    },
    /// `TermRequest::Search` with `regex` asked for a pattern that does not compile.
    SearchInvalid {
        /// The needle in question.
        needle: String,
        /// The compiler's complaint.
        message: String,
    },
    /// The program asked for a desktop notification (OSC 9, OSC 777 `notify`, OSC 99):
    /// a banner and the dock bounce when the human is not looking, as an agent's would be.
    Notification {
        /// Its title; empty when the protocol carries none.
        title: String,
        /// Its body.
        body: String,
    },
    /// The pixels of an image a `Frame` places (kitty graphics).
    ///
    /// Sent before the first frame that places it and again after a `full` frame. RGBA,
    /// row-major, no padding.
    Image {
        /// The image id programs and placements use.
        id: u32,
        /// Its generation: a re-transmission under the same id carries a newer one.
        generation: u64,
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// `width * height * 4` bytes.
        rgba: Vec<u8>,
    },
}
