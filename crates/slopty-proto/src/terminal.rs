//! Terminal sessions: lifecycle, input, frames.

use serde::{Deserialize, Deserializer, Serialize};
use slopty_core::{SessionId, WallMs};
use slopty_grid::{Cursor, Line, LineIndex, MAX_COLS, MAX_ROWS, RowUpdate, TermModes};

use crate::drag::{DragId, DragItem};
use crate::input::{CellMetrics, KeyEvent, MouseEvent};
use crate::transfer::{ClipFormat, ClipType};

/// Terminal size in cells.
///
/// A size decodes clamped to [`MAX_COLS`] × [`MAX_ROWS`]: a window wider than any terminal gets
/// the widest one, and the PTY and the engine are given the same size.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TermSize {
    /// Columns.
    #[serde(deserialize_with = "clamped::<_, MAX_COLS>")]
    pub cols: u16,
    /// Rows.
    #[serde(deserialize_with = "clamped::<_, MAX_ROWS>")]
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

/// A size a peer asked for, clamped to `MAX`.
fn clamped<'de, D: Deserializer<'de>, const MAX: u16>(d: D) -> Result<u16, D::Error> {
    u16::deserialize(d).map(|n| n.min(MAX))
}

/// A size a peer reports, at most `MAX`: a frame's screen, which the lines in it must fit.
fn bounded<'de, D: Deserializer<'de>, const MAX: u16>(d: D) -> Result<u16, D::Error> {
    let n = u16::deserialize(d)?;
    if n > MAX {
        return Err(serde::de::Error::invalid_value(
            serde::de::Unexpected::Unsigned(u64::from(n)),
            &"a size within `MAX_COLS` × `MAX_ROWS`",
        ));
    }
    Ok(n)
}

/// Request to create a session.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OpenSession {
    /// Initial size.
    pub size: TermSize,
    /// Working directory; the worker's default when `None`.
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

/// How the tty's line discipline treats input right now, as the worker reads it from `termios`:
/// what decides whether a client may draw a keystroke it has not seen echoed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LineDiscipline {
    /// `ECHO`: the tty echoes what is typed. Off at a password prompt.
    pub echo: bool,
    /// `ICANON`: input is edited a line at a time (a shell's `read`, `cat`), not handed to the
    /// program key by key.
    pub canonical: bool,
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

/// A session as listed by the worker.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SessionSummary {
    /// Identity.
    pub id: SessionId,
    /// Title (OSC 0/2, else the command).
    pub title: String,
    /// Current working directory if known (OSC 7).
    pub cwd: Option<String>,
    /// The repository [`Self::cwd`] is in, if any: the directory holding its `.git` entry.
    /// Only the worker can resolve it, and a client that groups by repository must not guess.
    pub repo: Option<String>,
    /// The branch [`Self::repo`] has checked out, or its commit abbreviated to seven hex
    /// digits when `HEAD` is detached. Read again when the directory changes and when a
    /// command ends, so a checkout shows by the next prompt.
    pub branch: Option<String>,
    /// Which repository [`Self::repo`] is, the same on every machine that has a clone of it
    /// ([`RepoId`]): what a client groups clones on several workers by.
    pub repo_id: Option<RepoId>,
    /// What [`Self::repo`]'s working tree has changed against `HEAD`, counted by the worker
    /// in the background after the same moments the branch is read; `None` outside a
    /// repository, before the first count, or when git could not say.
    pub changes: Option<RepoChanges>,
    /// When the session's program was spawned. Wall clock, because the summary is held and
    /// relayed (the server hands it to clients that join later) and an age measured at sending
    /// would be wrong by the time it is read.
    pub started_ms: WallMs,
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
    /// The coding agent running in it and what it is doing, when one is.
    pub agent: Option<crate::agent::SessionAgent>,
    /// The program's progress report (`OSC 9;4`) while one stands, as
    /// [`TermEvent::Progress`] says it to the viewers: a client that does not view the session
    /// still shows it. `None` once the report is removed or the program has exited.
    pub progress: Option<Progress>,
    /// The session was reopened after its shell was lost, as [`TermEvent::Restored`] says it.
    pub restored: Option<Restored>,
}

/// Which repository a checkout is, the same on every machine that has a clone of it.
///
/// Two ways to know, since neither always answers: the address it was cloned from, as a forge
/// names it, and its first commit, which every clone, mirror and worktree of it shares whatever
/// its remotes. A client merges two checkouts when either matches; placement matches workers
/// on the same ([`crate::project`]'s `repos` fact).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct RepoId {
    /// Its `origin` remote (else its first), normalized to `host/path`: the host lowercased,
    /// no scheme, user, port, trailing slash or `.git` (`github.com/aislopware/slopty` for
    /// `git@github.com:aislopware/slopty.git` and `https://github.com/aislopware/slopty`).
    /// `None` with no remote, or one on this machine's own disk, which names nothing elsewhere.
    pub origin: Option<String>,
    /// Its first commit (the oldest root reachable from `HEAD`), in full hex. `None` before the
    /// worker has read it, or in a repository with no commit yet.
    pub root: Option<String>,
    /// The address to clone it from, as its config spells [`Self::origin`], with any
    /// credentials in it left out: a worker with no clone makes one from it with its own git
    /// credentials. Not part of its identity.
    pub url: Option<String>,
}

impl RepoId {
    /// Whether `self` and `other` name one repository: the same origin or the same first
    /// commit.
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        let both = |a: &Option<String>, b: &Option<String>| a.is_some() && a == b;
        both(&self.origin, &other.origin) || both(&self.root, &other.root)
    }

    /// The keys it is known by, the origin first: what the `repos` placement fact lists.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.origin.iter().chain(&self.root).map(String::as_str)
    }
}

/// A working tree's changes against `HEAD`: what `git diff --numstat HEAD` counts, plus the
/// untracked files `.gitignore` does not hide.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct RepoChanges {
    /// Files changed, added, removed or untracked.
    pub files: u32,
    /// Lines added in tracked text files.
    pub added: u32,
    /// Lines removed in tracked text files.
    pub removed: u32,
}

/// Why a session closed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CloseReason {
    /// A client asked.
    Requested,
    /// The child exited and the session was not retained.
    Exited,
}

/// Client → worker, scoped to one session.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TermRequest {
    /// Attach: the worker opens a session stream and sends a full frame.
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
    /// Paste text (worker applies bracketed paste if the mode is on).
    Paste {
        /// What to paste.
        text: String,
        /// The person has seen it and wants it pasted as it is. Unconfirmed, the worker writes
        /// it only when it cannot run anything under the program's mode as it is now (no line
        /// break outside bracketed paste, no end of the bracket inside it); otherwise it sends
        /// it back as [`TermEvent::PasteHeld`] for the client to ask about.
        confirmed: bool,
    },
    /// Raw bytes to the PTY (tooling, tests).
    Raw(#[serde(with = "serde_bytes")] Vec<u8>),
    /// ⌘K: drop the history and repaint the prompt at the top. The worker erases the
    /// scrollback as if the program had asked (`CSI 3 J`, so a replay agrees) and sends the
    /// shell ⌃L for the screen.
    Clear,
    /// Focus changed (DEC 1004).
    Focus {
        /// True when focused.
        focused: bool,
    },
    /// Ask for scrollback lines `[start, start + count)`. The worker serves at most
    /// [`MAX_FETCH_LINES`] in one answer.
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
    /// Everything the session stream carried up to `TermEvent::Marker { id }` has been
    /// applied. A frame the connection has written may still wait in the transport, and this
    /// is how the worker learns it arrived: it holds back frames once [`FRAMES_UNREACHED_BYTES`]
    /// of them are unconfirmed, so a slow link shows a screen a fraction of a second old
    /// rather than the tail of a queue.
    Reached {
        /// The marker's id.
        marker: u64,
    },
    /// ⌘V or ⌃V while this client's clipboard holds a picture and no text. The worker puts
    /// this client's clipboard offer on its pasteboard, then applies the chord, so a program
    /// that reads the pasteboard on it (Claude Code) finds the picture. The offer rode ahead
    /// of this on the control stream as `ClipMsg::Offer` when the worker had not heard it.
    /// The session's input behind this waits, in order, until the pasteboard holds the
    /// offer, as a window's input waits behind its ⌘V. It has no datagram copy, which could
    /// overtake the offer.
    PastePicture(PasteChord),
    /// This client's drag of `items` entered the tile, while the program asks for drops
    /// ([`TermEvent::DropTarget`]). The items carry no bytes: the program is offered their
    /// types ([`drop_offer`]), and what it accepts the client pushes up as the program answers
    /// ([`TermEvent::DropAccepted`]), the rest only when the program asks for it on the drop
    /// ([`crate::transfer::ClipMsg::Fetch`] under [`crate::transfer::Source::Drag`]). One drag
    /// per client is over a tile at a time; a new one replaces it.
    DragEnter {
        /// The drag, as its representations are named in fetches.
        drag: DragId,
        /// What it carries: its files, the files it promises, and its data's types.
        items: Vec<DragItem>,
    },
    /// The drag that entered is over the cell at `at`. Answered with
    /// [`TermEvent::DropAccepted`].
    DragOver {
        /// Where.
        at: DropPoint,
    },
    /// This client's drag left the tile without dropping.
    DragLeave,
    /// This client dropped its drag on the tile at `at`. The program reads what it wants of
    /// it, then concludes ([`TermEvent::DropConcluded`]); a program that never accepted the
    /// drag is not given it, and the drop concludes as nothing at once.
    Drop {
        /// Where.
        at: DropPoint,
    },
    /// The files of this client's drag `drag`, its own and those its promises wrote, have
    /// landed at `landed` on the worker, which gives the program their `file://` URLs as
    /// `text/uri-list`. `None` when they will not come. They go up from the moment the program
    /// accepts `text/uri-list` (or at the drop, for promised files), so they may land before
    /// the drop or after it, or after a new drag has entered: they name their drag.
    DropFiles {
        /// Whose.
        drag: DragId,
        /// Where they landed, in item order.
        landed: Option<Vec<String>>,
    },
}

/// The MIME type a drop of files is offered as.
pub const URI_LIST: &str = "text/uri-list";

/// The MIME type plain text is also offered as: kitty's clients ask for it rather than for
/// [`ClipFormat::Text`]'s `text/plain;charset=utf-8`.
pub const PLAIN_TEXT: &str = "text/plain";

/// Where the bytes of a type a drop offers come from.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum DropFrom {
    /// The `file://` URLs of the drag's files once they landed ([`TermRequest::DropFiles`]).
    Files,
    /// Representation `kind` of item `item` of the drag.
    Rep {
        /// Which item, counted from 0.
        item: u16,
        /// Which representation.
        kind: ClipType,
    },
}

/// The MIME types a drag of `items` offers a program, in order, and where each one's bytes
/// come from.
///
/// [`URI_LIST`] comes first when the drag carries or promises files, then each other
/// representation in item order, plain text under both its names. A type without a MIME name
/// (an Apple type only) is not offered, and a type two items carry is offered for the first.
/// Both ends work it out alike, so a program's request by index names the same bytes on each.
#[must_use]
pub fn drop_offer(items: &[DragItem]) -> Vec<(String, DropFrom)> {
    let mut offer: Vec<(String, DropFrom)> = Vec::new();
    if items.iter().any(|i| i.file.is_some() || i.promised.is_some()) {
        offer.push((URI_LIST.to_owned(), DropFrom::Files));
    }
    for (item, entry) in (0_u16..).zip(items) {
        for rep in &entry.reps {
            let Some(format) = rep.kind.format() else { continue };
            let names: &[&str] = match format {
                ClipFormat::FileUrls => continue,
                ClipFormat::Text => &[ClipFormat::Text.mime(), PLAIN_TEXT],
                other => &[other.mime()],
            };
            for name in names {
                if offer.iter().all(|(mime, _)| mime != name) {
                    let from = DropFrom::Rep { item, kind: rep.kind.clone() };
                    offer.push(((*name).to_owned(), from));
                }
            }
        }
    }
    offer
}

/// Where a drag is over a terminal, and what it allows (Kitty drag and drop, OSC 72).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct DropPoint {
    /// Cell column, from 0 at the left.
    pub col: u16,
    /// Cell row, from 0 at the top of the screen.
    pub row: u16,
    /// Pixels from the left of the grid.
    pub x: i32,
    /// Pixels from the top of the grid.
    pub y: i32,
    /// The drag may copy.
    pub copy: bool,
    /// The drag may move.
    pub moves: bool,
}

/// What a drop does with its data, as the program says.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum DropOperation {
    /// Nothing: not accepted, or canceled.
    None,
    /// The data is copied.
    Copy,
    /// The data is moved.
    Move,
}

/// What a [`TermRequest::PastePicture`] applies once the worker's pasteboard holds the
/// client's picture.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PasteChord {
    /// ⌘V: an empty paste, bracketed when the program asked for bracketed paste. Claude Code
    /// reads the pasteboard's picture on an empty paste.
    Command,
    /// ⌃V as typed, Claude Code's own picture-paste key.
    Control(KeyEvent),
}

impl PasteChord {
    /// The input the chord is once the pasteboard is set.
    #[must_use]
    pub fn into_request(self) -> TermRequest {
        match self {
            Self::Command => TermRequest::Paste { text: String::new(), confirmed: true },
            Self::Control(key) => TermRequest::Key(key),
        }
    }
}

impl TermRequest {
    /// Whether the request writes to the PTY. These are numbered per session on a connection's
    /// control stream, from 1 in the order it carries them, by both ends alike: a datagram copy
    /// names its request by that number (`datagram::ClientDatagram::Input`).
    #[must_use]
    pub const fn is_input(&self) -> bool {
        match self {
            Self::Key(_)
            | Self::Mouse(_)
            | Self::Paste { .. }
            | Self::Raw(_)
            | Self::Clear
            | Self::Focus { .. }
            | Self::PastePicture(_) => true,
            Self::Attach { .. }
            | Self::Detach
            | Self::Close
            | Self::Resize(_)
            | Self::Drive { .. }
            | Self::FetchLines { .. }
            | Self::Search { .. }
            | Self::Colors(_)
            | Self::Reached { .. }
            | Self::DragEnter { .. }
            | Self::DragOver { .. }
            | Self::DragLeave
            | Self::Drop { .. }
            | Self::DropFiles { .. } => false,
        }
    }
}

/// Most lines one [`TermRequest::FetchLines`] is answered with; a client asks in chunks of it.
pub const MAX_FETCH_LINES: u32 = 4096;

/// Bytes of frames a viewer that answers markers may have on their way unconfirmed. At
/// 250 kB/s a quarter of a second; the transport's own stream window (1.25 MB) was five.
pub const FRAMES_UNREACHED_BYTES: usize = 64 * 1024;

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

/// The colours a program changed over the client's own (OSC 4, 10, 11, 12), as `[r, g, b]`.
///
/// `None` and an absent index mean the client's colour; an OSC 104/110/111/112 reset or a
/// full reset sends the event again with the entry gone. The whole set is carried each time,
/// so a client attaching later paints the same colours as one that watched every change.
#[derive(Clone, Default, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ColorOverrides {
    /// Default text (OSC 10).
    pub fg: Option<[u8; 3]>,
    /// Default background (OSC 11).
    pub bg: Option<[u8; 3]>,
    /// Cursor (OSC 12).
    pub cursor: Option<[u8; 3]>,
    /// Palette entries the program set (OSC 4), by index, ascending.
    pub palette: Vec<(u8, [u8; 3])>,
}

/// One search hit, in cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SearchMatch {
    /// Absolute line.
    pub line: LineIndex,
    /// First cell.
    pub col: u16,
    /// Cells covered, in reading order. A hit over a soft wrap runs past the end of its row
    /// onto the rows its line wraps into: on a grid `cols` wide it covers cells `col..col + len`
    /// of row `line` counted on, row after row, at `cols` cells each.
    pub len: u16,
}

/// One frame: the changed rows since the previous frame (or every row when `full`).
///
/// A frame that is not full may move `first_visible_line`, when output scrolled the screen:
/// a row it does not carry shows the line the client already holds at that absolute index,
/// so a line scrolling up is never sent again.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Frame {
    /// Monotonic per session. A gap means the client must request a resync. A frame that is
    /// not after the last one applied came on a stream a re-attach replaced, and is dropped,
    /// unless it is full at the same number: a joiner's frame carries the others' number.
    pub seq: u64,
    /// True when `updates` holds every row (attach, resize, resync, a new numbering).
    pub full: bool,
    /// Names a numbering of the absolute lines. A new one comes with a reflow on resize, a
    /// reset, or the alternate screen, and the client puts its line cache aside. Returning
    /// from the alternate screen brings the primary's back, and with it the cache.
    pub epoch: u32,
    /// Columns, at most [`MAX_COLS`].
    #[serde(deserialize_with = "bounded::<_, MAX_COLS>")]
    pub cols: u16,
    /// Rows, at most [`MAX_ROWS`].
    #[serde(deserialize_with = "bounded::<_, MAX_ROWS>")]
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
    /// Every image placed wholly above the screen, in the history (at most [`MAX_ABOVE`] of
    /// them, the nearest the screen), replacing what the client holds. A frame every viewer
    /// takes whole carries it, and one after the images changed; otherwise it is `None`, and a
    /// client moves a placement up itself once a frame's screen starts below it, and forgets
    /// one whose lines left the history.
    pub above: Option<Vec<Placement>>,
    /// The command blocks, when there is news of them: on the frame they belong to, so a
    /// viewer that is behind takes them in the same event as its rows.
    pub blocks: Option<Blocks>,
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

/// The most command blocks [`Blocks`] lists whole, the newest: a scrollbar a few
/// hundred points tall shows no more distinct marks, and each is a few bytes on the wire.
pub const MAX_BLOCKS: usize = 4096;

/// A command block as the shell integration marked it (`OSC 133`): a prompt where a command
/// was typed and its output started. What the scrollbar marks.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BlockMark {
    /// The absolute line its prompt starts on.
    pub prompt: LineIndex,
    /// The status its `133;D` carried; `None` while it runs, or when the shell gave none.
    pub exit: Option<u8>,
}

/// The command blocks a frame carries ([`Frame::blocks`]). A client forgets a block whose
/// prompt left the history, and every block on a frame in another numbering.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Blocks {
    /// Every block the history holds (at most [`MAX_BLOCKS`], the newest), replacing what the
    /// client holds: on a frame every viewer takes whole. Otherwise only the blocks that
    /// started or ended since the last frame, each replacing the one at its prompt.
    pub whole: bool,
    /// Oldest first.
    pub marks: Vec<BlockMark>,
}

/// The most placements [`Frame::above`] carries, the nearest the screen: each is a
/// few dozen bytes on the wire, and a program can place thousands of small images.
pub const MAX_ABOVE: usize = 1024;

/// One image the program placed on the grid (kitty graphics).
///
/// Its row is an absolute line, so it stays with the text it was placed among as that
/// scrolls into the history; its column a cell, and its sizes the cell pixels of
/// `TermSize::metrics`, as the worker laid it out.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Placement {
    /// The image, as `TermEvent::Image` carried it.
    pub image: u32,
    /// The pixels' generation: a re-sent image with the same id has a newer one.
    pub generation: u64,
    /// Top-left cell column; negative when the placement starts left of the viewport.
    pub col: i32,
    /// The absolute line of its top row: before the screen's first line when it has scrolled
    /// partly or wholly into the history.
    pub line: LineIndex,
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

/// A [`TermEvent::Frame`]'s number and whether it is `full`, read from its postcard body.
///
/// Only the front is read, not the rows; `None` for any other event. What lets a client drop
/// the second copy of an echo (stream and datagram) before decoding it.
#[must_use]
pub fn frame_head(body: &[u8]) -> Option<(u64, bool)> {
    // Postcard writes an enum's variant index as a varint, then the struct's fields in order;
    // `Frame` is the first variant, and `seq` and `full` its first fields.
    let (variant, fields) = postcard::take_from_bytes::<u32>(body).ok()?;
    if variant != 0 {
        return None;
    }
    postcard::take_from_bytes::<(u64, bool)>(fields).ok().map(|(head, _rows)| head)
}

/// Largest text a program can put on the clipboard through OSC 52; a pasteboard can hold a
/// whole file, and pushing that to every viewer would starve the rows behind it.
pub const MAX_OSC52_BYTES: usize = 256 * 1024;

/// Bytes of image pixels a client keeps for placements, and the worker assumes it keeps.
///
/// The least recently placed image goes first once the budget is over. The worker re-sends an
/// image it dropped from its own ledger when a placement needs it again.
pub const IMAGE_CACHE_BYTES: usize = 48 * 1024 * 1024;

/// Why a request about a session failed.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, thiserror::Error)]
pub enum TermError {
    /// The program is not reading its input, and the worker's queue of it is full: what was
    /// typed was refused rather than held without bound.
    #[error("The program is not reading its input; what was typed was dropped")]
    InputFull,
    /// The worker has no such session: it ended.
    #[error("The terminal has ended")]
    NoSuchSession,
    /// Writing to the terminal failed.
    #[error("Writing to the terminal failed: {0}")]
    Write(String),
    /// The terminal engine could not do it: a frame, lines, a search, a key to encode.
    #[error("The terminal could not do it: {0}")]
    Engine(String),
    /// The session stream to this client could not be opened.
    #[error("The terminal's stream did not open: {0}")]
    Stream(String),
}

/// Worker → client on the session stream.
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
    /// Working directory changed, or the branch checked out there did.
    Cwd {
        /// The new directory.
        path: String,
        /// The repository it is in, resolved by the worker. `None` outside a repository.
        repo: Option<String>,
        /// That repository's branch, as [`SessionSummary::branch`].
        branch: Option<String>,
    },
    /// BEL.
    Bell,
    /// The program wrote to the system clipboard (OSC 52 / OSC 1337 Copy). Text only, at
    /// most [`MAX_OSC52_BYTES`]; every attached client puts it on its own clipboard.
    /// An OSC 52 `?` never travels: the worker answers it while a client shares its clipboard.
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
        /// Columns, at most [`MAX_COLS`].
        #[serde(deserialize_with = "bounded::<_, MAX_COLS>")]
        cols: u16,
        /// Rows, at most [`MAX_ROWS`].
        #[serde(deserialize_with = "bounded::<_, MAX_ROWS>")]
        rows: u16,
    },
    /// Driver changed.
    Driver {
        /// True when this client now drives the size.
        you: bool,
    },
    /// A request about the session failed, or the session could not go on serving it.
    Error(TermError),
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
    /// Sent before the first frame that places it and again after a `full` frame. BGRA with
    /// the alpha premultiplied, row-major, no padding: the texture format, so a client uploads
    /// the bytes as they arrive.
    Image {
        /// The image id programs and placements use.
        id: u32,
        /// Its generation: a re-transmission under the same id carries a newer one.
        generation: u64,
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// `width * height * 4` bytes, written as one byte string (not a sequence of `u8`s):
        /// the same wire bytes, a copy instead of a call per byte.
        #[serde(with = "serde_bytes")]
        bgra: Vec<u8>,
    },
    /// The program changed (or reset) the terminal's colours; the whole current set.
    Colors(ColorOverrides),
    /// Answer with `TermRequest::Reached { marker: id }` once every event before this one is
    /// applied. Sent after every quarter of [`FRAMES_UNREACHED_BYTES`] of frames.
    Marker {
        /// Unique within the session.
        id: u64,
    },
    /// The program's progress changed (`OSC 9;4`), or its report was dropped. Sent on attach
    /// while one stands, like the title.
    Progress(Progress),
    /// The session was reopened after its shell was lost. Sent on attach for the session's
    /// whole life.
    Restored(Restored),
    /// The program asked for another pointer shape over the grid (`OSC 22`). Sent on attach
    /// while it is not the I-beam, like the title.
    Pointer(PointerShape),
    /// An unconfirmed `TermRequest::Paste` that would run something under the program's mode
    /// as it stands, sent back unwritten to the client that pasted it, which asks the person.
    PasteHeld {
        /// The text, as it was pasted.
        text: String,
    },
    /// Whether the program asks for drops (Kitty drag and drop, OSC 72): while it does, a
    /// drag over the tile goes to it as [`TermRequest::DragOver`] and [`TermRequest::Drop`]
    /// instead of typing paths. Sent on attach while it does.
    DropTarget {
        /// It asks.
        accepts: bool,
    },
    /// The program answered the drag of the client it goes to: what a drop would do, and the
    /// MIME types it wants, most wanted first.
    DropAccepted {
        /// What a drop would do.
        operation: DropOperation,
        /// The types it wants; empty when it did not say.
        mimes: Vec<String>,
    },
    /// The program is done with the drop of the client it goes to, having done `operation`.
    DropConcluded {
        /// What it did.
        operation: DropOperation,
    },
}

/// The pointer a program asks for over the grid with `OSC 22`, by its W3C cursor name.
///
/// A clickable span in a TUI, a splitter it resizes. The terminal starts at [`Self::Text`]. A
/// client draws the nearest shape its platform has; hovering a link keeps its own pointer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[expect(missing_docs, reason = "each variant is its W3C cursor name")]
pub enum PointerShape {
    #[default]
    Text,
    Default,
    ContextMenu,
    Help,
    Pointer,
    Progress,
    Wait,
    Cell,
    Crosshair,
    VerticalText,
    Alias,
    Copy,
    Move,
    NoDrop,
    NotAllowed,
    Grab,
    Grabbing,
    AllScroll,
    ColResize,
    RowResize,
    NResize,
    EResize,
    SResize,
    WResize,
    NeResize,
    NwResize,
    SeResize,
    SwResize,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
    ZoomIn,
    ZoomOut,
}

/// What a program reports of its progress with `OSC 9;4`, a sequence from the `ConEmu`
/// terminal: build tools, package managers and Claude Code's turn bar
/// (`terminalProgressBarEnabled`) emit it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    /// What to show.
    pub state: ProgressState,
    /// How far along, 0 to 100, when the program said. An error or a pause that names no
    /// value keeps the last one, as `ConEmu` and Windows Terminal do.
    pub percent: Option<u8>,
}

/// The state an `OSC 9;4` report names.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ProgressState {
    /// Nothing to show: never reported, removed (`9;4;0`), or its program gave the prompt back.
    #[default]
    None,
    /// Determinate, at [`Progress::percent`] (`9;4;1`).
    Set,
    /// Failed (`9;4;2`).
    Error,
    /// Working, with no measure of how far along (`9;4;3`).
    Indeterminate,
    /// Paused (`9;4;4`).
    Paused,
}

/// A session reopened after its shell was lost to a reboot or to ptyd ending.
///
/// It runs a new login shell in the directory the old one was in, with the old screen and
/// scrollback above a divider. Nothing the old shell was running is started again.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Restored {
    /// When the scrollback above the divider was saved: output after it was lost.
    pub saved_ms: WallMs,
    /// What the session was opened to run; empty for the login shell. A client may offer it
    /// again, never run it unasked.
    pub command: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drag::FileMeta;
    use crate::transfer::Rep;

    fn rep(kind: ClipType) -> Rep {
        Rep { kind, size: None, hash: None, inline: None }
    }

    /// Files first as one list, then each type once, plain text under both its names; an
    /// Apple-only type and a second item's same type are not offered.
    #[test]
    fn a_drop_offers_each_type_once_files_first() {
        let file = FileMeta {
            name: "a".to_owned(),
            size: 1,
            folder: false,
            mode: 0o644,
            mtime_ms: WallMs::ZERO,
            path: None,
        };
        let format = |f| rep(ClipType::Format(f));
        let items = vec![
            DragItem {
                file: None,
                promised: None,
                reps: vec![format(ClipFormat::Html), format(ClipFormat::Text)],
            },
            DragItem { file: Some(file), promised: None, reps: Vec::new() },
            DragItem {
                file: None,
                promised: None,
                reps: vec![
                    rep(ClipType::Apple("com.adobe.pdf".to_owned())),
                    format(ClipFormat::Html),
                ],
            },
        ];
        let offer = drop_offer(&items);
        let mimes: Vec<&str> = offer.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(mimes, [URI_LIST, "text/html", "text/plain;charset=utf-8", PLAIN_TEXT]);
        let text = DropFrom::Rep { item: 0, kind: ClipType::Format(ClipFormat::Text) };
        assert_eq!(offer[0].1, DropFrom::Files);
        assert_eq!((&offer[2].1, &offer[3].1), (&text, &text), "one rep, two names");
        let promised =
            DragItem { file: None, promised: Some("public.png".to_owned()), reps: Vec::new() };
        assert_eq!(drop_offer(&[promised]), [(URI_LIST.to_owned(), DropFrom::Files)]);
        assert_eq!(drop_offer(&[]), []);
    }
}
