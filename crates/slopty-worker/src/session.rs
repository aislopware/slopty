//! The session actor.

use std::collections::VecDeque;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use bytes::{Buf as _, Bytes, BytesMut};
use slopty_core::{ClientId, SessionId};
use slopty_engine::boundary::Boundary;
use slopty_engine::ghostty::{CommandBlock, Position, ScreenText, TextLines, TextSince};
use slopty_engine::{EngineConfig, EngineEvent, GhosttyEngine, ImageUpload};
use slopty_proto::codec;
use slopty_proto::terminal::{
    ColorOverrides, FRAMES_UNREACHED_BYTES, Frame, MAX_FETCH_LINES, MAX_OSC52_BYTES, TermColors,
    TermEvent, TermRequest, TermSize,
};
use slopty_pty::PtyMaster;
use slopty_pty::protocol::OutputFrame;
use tokio::sync::{Notify, mpsc, oneshot, watch};

use crate::WorkerError;

/// While output keeps flowing, frames go out no closer together than this: 125 frames per
/// second per session is more than any display shows (120 Hz `ProMotion`), and without the
/// cap a `yes`-like flood sent five hundred frames a second per session, which twenty
/// sessions turned into a client that could not keep up (MEASUREMENTS, 2026-09-05 "frame
/// budget"). Output after a quiet spell is framed at once: there is nothing to coalesce it
/// with yet, and a fixed window (2 ms until 2026-09-06) was the largest fixed cost on a
/// keystroke's echo (MEASUREMENTS, 2026-09-06 "leading-edge frame"). Bytes that are already
/// readable when the timer fires still join the same frame, because the read arm of the
/// actor's select comes first.
const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(8);
/// After a viewer's input reaches the PTY, this many frames go out as their output is read
/// instead of at the pace: the echo, a second write that repaints it (a highlighter recolouring
/// the line), or the echo behind a read of other output that took the acknowledgement. Beside
/// a spinner or a redrawing TUI the pace held every echo for up to 8 ms (MEASUREMENTS
/// 2026-09-25, "echo pacing and frames in flight"). A budget per input, so a flood beside the
/// typing stays paced.
const ECHO_FRAMES: u8 = 2;
/// Output read this long after the input no longer counts as its echo.
const ECHO_WINDOW: Duration = Duration::from_millis(50);
/// Frames a viewer may have on their way at once: one its connection is writing, one ready
/// behind it. The next is built only when one of them is done, so a link slower than the
/// program is sent the newest screen rather than a queue of old ones; the engine keeps what
/// changed meanwhile and the next diff carries all of it.
const FRAMES_IN_FLIGHT: usize = 2;
/// A viewer is sent a `TermEvent::Marker` after this many bytes of frames, so its answers
/// confirm the frames it has while more are on their way.
const MARKER_EVERY_BYTES: usize = FRAMES_UNREACHED_BYTES / 4;
/// Unanswered markers remembered per viewer. One that answers has about five out at a time;
/// one that never answers would otherwise collect them for as long as it watches.
const MARKERS_KEPT: usize = 16;
/// Events a viewer's sink had no room for wait in order, up to this many bytes. Past it they
/// are dropped and the viewer is told everything again once its sink drains, as a joiner is.
const QUEUED_MAX_BYTES: usize = 16 << 20;
/// PTY read buffer.
const READ_BUF: usize = 64 << 10;
/// Search hits sent back at most; the count still covers every hit.
const MAX_SEARCH_MATCHES: u32 = 5_000;

/// Where a client's events go.
///
/// A frame is done with when the connection drops its [`Outbound`], and a viewer has at most
/// two frames not done with: so a connection holds each frame until the link has taken it. Every
/// other event goes in order and is never skipped; one the sink has no room for waits in the actor,
/// and a viewer that misses a diff is sent every row next.
pub type ClientSink = mpsc::Sender<Outbound>;

/// One event for the viewers, encoded once however many it goes to.
#[derive(Clone, Debug)]
pub struct Outbound {
    wire: Bytes,
    frame: bool,
    /// A diff that answers a viewer's input: a datagram may carry a copy of it.
    echo: bool,
    /// A later frame may depend on it having arrived (the pixels it places, the colours or
    /// size it paints with, lines it shows), so no copy of that frame may overtake it.
    ahead: bool,
    /// A frame's place among its viewer's [`FRAMES_IN_FLIGHT`], given back when it is dropped:
    /// held only for its `Drop`.
    _credit: Option<Arc<Credit>>,
}

impl Outbound {
    fn encode(ev: &TermEvent) -> Result<Self, codec::CodecError> {
        Ok(Self {
            wire: codec::encode(ev)?,
            frame: matches!(ev, TermEvent::Frame(_)),
            echo: false,
            ahead: matches!(
                ev,
                TermEvent::Image { .. }
                    | TermEvent::Colors(_)
                    | TermEvent::Resized { .. }
                    | TermEvent::Lines { .. }
            ),
            _credit: None,
        })
    }

    /// The same bytes, holding a place in `viewer`'s frames in flight until dropped.
    fn claimed(&self, viewer: &Viewer, room: &Arc<Notify>) -> Self {
        viewer.in_flight.fetch_add(1, Ordering::AcqRel);
        let credit = Credit { in_flight: Arc::clone(&viewer.in_flight), room: Arc::clone(room) };
        Self { wire: self.wire.clone(), _credit: Some(Arc::new(credit)), ..*self }
    }

    /// The event as the session stream carries it (length prefix, then postcard): write it
    /// with `FramedSend::send_raw`.
    #[must_use]
    pub fn wire(&self) -> &[u8] {
        &self.wire
    }

    /// Whether it carries a [`TermEvent::Frame`].
    #[must_use]
    pub const fn is_frame(&self) -> bool {
        self.frame
    }

    /// Whether it is a diff built soon after a viewer's input reached the PTY: an echo, which
    /// a datagram may copy (`slopty_proto::datagram::TermDatagram`).
    #[must_use]
    pub const fn is_echo(&self) -> bool {
        self.echo
    }

    /// Whether the next frame depends on it, so a copy of that frame must not overtake it.
    #[must_use]
    pub const fn goes_ahead_of_frames(&self) -> bool {
        self.ahead
    }
}

/// A frame on its way to one viewer. Dropping it, which the connection does once the frame is
/// written, frees the place and wakes the actor to build that viewer's next frame.
#[derive(Debug)]
struct Credit {
    in_flight: Arc<AtomicUsize>,
    room: Arc<Notify>,
}

impl Drop for Credit {
    fn drop(&mut self) {
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
        self.room.notify_one();
    }
}

/// Commands into the actor.
enum Cmd {
    Attach { client: ClientId, size: TermSize, sink: ClientSink },
    Detach { client: ClientId, sink: Option<ClientSink> },
    Reserve { client: ClientId },
    Request { client: ClientId, req: TermRequest, at: tokio::time::Instant },
    Snapshot { reply: oneshot::Sender<Snapshot> },
    ResizeUnviewed { size: TermSize, reply: oneshot::Sender<u16> },
    Probe { reply: oneshot::Sender<Probe> },
    Read { read: Read, reply: oneshot::Sender<Result<Text, WorkerError>> },
    Exited { status: i32 },
    Close,
}

/// What `Worker` reports about a session.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Snapshot {
    /// Title from OSC 0/2, if any.
    pub title: Option<String>,
    /// Cwd from OSC 7, if any.
    pub cwd: Option<String>,
    /// The repository that cwd is in, resolved when it last changed.
    pub repo: Option<String>,
    /// The terminal's size, which the driver sets.
    pub size: TermSize,
    /// Clients attached to it.
    pub viewers: u16,
    /// Exit status if the child is gone.
    pub exited: Option<i32>,
}

/// What the worker can see of a session without asking anything running in it: the program in
/// the foreground of its tty, the title it painted, and where it runs.
///
/// This is what `slopty_agent` attributes hand-started Claude Code sessions from. It is taken
/// on the actor's own thread, which is the only one holding the PTY master, and only on the
/// daemon's agent tick — never per frame.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Probe {
    /// The foreground process of the session's tty.
    pub foreground: Option<slopty_pty::process::Foreground>,
    /// Title from OSC 0/2, if any.
    pub title: Option<String>,
    /// Cwd from OSC 7, if any.
    pub cwd: Option<String>,
}

/// What has happened in a session, for waiters: counters that only grow, published on a
/// [`watch`] channel so a waiter wakes on a change instead of asking. The channel closes when
/// the actor stops.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Activity {
    /// PTY reads consumed.
    pub output: u64,
    /// Commands ended (OSC 133 `D`), as [`GhosttyEngine::commands_ended`] counts them.
    pub commands_ended: u64,
    /// The child is gone: its exit was reported or the master closed.
    pub exited: bool,
}

/// A text view of the session's terminal ([`SessionHandle::read`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Read {
    /// The screen.
    Screen,
    /// Retained lines ([`GhosttyEngine::text_lines`]).
    Output {
        /// First line wanted.
        since: Option<u64>,
        /// At most this many.
        max: u32,
    },
    /// OSC 133 command blocks ([`GhosttyEngine::commands`]).
    Commands {
        /// Blocks whose prompt is at or after this line.
        since: Option<u64>,
    },
    /// Where the cursor is.
    Position,
    /// Text written since a position ([`GhosttyEngine::text_since`]).
    Since(Position),
    /// The first command that ended at or after a position ([`GhosttyEngine::ended_after`]).
    EndedAfter(Position),
}

/// The answer to a [`Read`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Text {
    /// For [`Read::Screen`]: the screen with the title and directory the actor holds.
    Screen {
        /// Rows and cursor.
        screen: ScreenText,
        /// Title from OSC 0/2.
        title: Option<String>,
        /// Directory from OSC 7.
        cwd: Option<String>,
    },
    /// For [`Read::Output`].
    Output(TextLines),
    /// For [`Read::Commands`].
    Commands(Vec<CommandBlock>),
    /// For [`Read::Position`].
    Position(Position),
    /// For [`Read::Since`].
    Since(TextSince),
    /// For [`Read::EndedAfter`]: where that command's `133;D` was written.
    Ended(Option<Position>),
}

/// Cloneable handle to a running session actor.
#[derive(Clone, Debug)]
pub struct SessionHandle {
    id: SessionId,
    tx: mpsc::UnboundedSender<Cmd>,
    activity: watch::Receiver<Activity>,
    /// Where orchestration's waits have read up to (see [`Self::mark`]).
    marks: Arc<parking_lot::Mutex<Marks>>,
}

/// Orchestration's read positions in a session: output matched up to `output`, commands
/// reported up to `command`.
#[derive(Clone, Copy, Debug, Default)]
struct Marks {
    output: Option<Position>,
    command: Option<Position>,
}

/// `at` is past `mark`, or in another line numbering (which replaces it).
fn ahead(mark: Option<Position>, at: Position) -> bool {
    let place = |p: Position| (p.line, p.col);
    mark.is_none_or(|m| m.epoch != at.epoch || place(m) < place(at))
}

impl std::fmt::Debug for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Attach { .. } => "Attach",
            Self::Detach { .. } => "Detach",
            Self::Reserve { .. } => "Reserve",
            Self::Request { .. } => "Request",
            Self::Snapshot { .. } => "Snapshot",
            Self::ResizeUnviewed { .. } => "ResizeUnviewed",
            Self::Probe { .. } => "Probe",
            Self::Read { .. } => "Read",
            Self::Exited { .. } => "Exited",
            Self::Close => "Close",
        };
        f.write_str(name)
    }
}

impl SessionHandle {
    /// The session this handle reaches.
    #[must_use]
    pub const fn id(&self) -> SessionId {
        self.id
    }

    /// Attach a client; it receives a full frame first.
    pub fn attach(
        &self,
        client: ClientId,
        size: TermSize,
        sink: ClientSink,
    ) -> Result<(), WorkerError> {
        self.send(Cmd::Attach { client, size, sink })
    }

    /// Detach a client, whatever sink it is attached through.
    pub fn detach(&self, client: ClientId) -> Result<(), WorkerError> {
        self.send(Cmd::Detach { client, sink: None })
    }

    /// Detach a client only if it is still attached through `sink`.
    ///
    /// A connection that dies after the same client reconnected (a relaunched app whose old
    /// QUIC connection idles out) must not evict the new connection's viewer.
    pub fn detach_sink(&self, client: ClientId, sink: &ClientSink) -> Result<(), WorkerError> {
        self.send(Cmd::Detach { client, sink: Some(sink.clone()) })
    }

    /// Make `client` the driver before anyone attaches: the client that opened a session
    /// sizes it, even if another client's attach (it sees the session first via the
    /// broadcast) lands earlier.
    pub fn reserve_driver(&self, client: ClientId) -> Result<(), WorkerError> {
        self.send(Cmd::Reserve { client })
    }

    /// Forward a terminal request from a client.
    pub fn request(&self, client: ClientId, req: TermRequest) -> Result<(), WorkerError> {
        self.send(Cmd::Request { client, req, at: tokio::time::Instant::now() })
    }

    /// The title, directory, size, viewers and exit as the actor knows them now.
    pub async fn snapshot(&self) -> Result<Snapshot, WorkerError> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Snapshot { reply })?;
        rx.await.map_err(|_gone| WorkerError::SessionClosed)
    }

    /// Resize the session if no client shows it, in one step on the actor, so a client that
    /// attaches meanwhile cannot lose its seat to the resize. Returns the number of clients
    /// showing it: zero means the size was applied.
    pub async fn resize_unviewed(&self, size: TermSize) -> Result<u16, WorkerError> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::ResizeUnviewed { size, reply })?;
        rx.await.map_err(|_gone| WorkerError::SessionClosed)
    }

    /// What can be seen of the session from outside it ([`Probe`]).
    pub async fn probe(&self) -> Result<Probe, WorkerError> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Probe { reply })?;
        rx.await.map_err(|_gone| WorkerError::SessionClosed)
    }

    /// Read the terminal as text, on the actor's thread.
    pub async fn read(&self, read: Read) -> Result<Text, WorkerError> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Read { read, reply })?;
        rx.await.map_err(|_gone| WorkerError::SessionClosed)?
    }

    /// The session's [`Activity`], marked seen as of now: `changed()` wakes on what happens
    /// next, and errors once the actor has stopped.
    #[must_use]
    pub fn activity(&self) -> watch::Receiver<Activity> {
        let mut rx = self.activity.clone();
        rx.mark_unchanged();
        rx
    }

    /// Where orchestration's output matching reads from next, as `expect` keeps its buffer:
    /// set where orchestration first touched the session (opened it, typed into it, waited on
    /// it) and moved past each match. `None` until then.
    #[must_use]
    pub fn mark(&self) -> Option<Position> {
        self.marks.lock().output
    }

    /// Set the mark unless it is set already; the mark in force either way.
    pub fn mark_if_unset(&self, at: Position) -> Position {
        *self.marks.lock().output.get_or_insert(at)
    }

    /// Move the mark to `at`, never back within one line numbering.
    pub fn advance_mark(&self, at: Position) {
        let mut marks = self.marks.lock();
        if ahead(marks.output, at) {
            marks.output = Some(at);
        }
    }

    /// Where the last command a `CommandDone` wait reported ended; `None` before one did.
    #[must_use]
    pub fn command_mark(&self) -> Option<Position> {
        self.marks.lock().command
    }

    /// A `CommandDone` wait reported the command that ended at `at`; the next one waits for a
    /// later command.
    pub fn advance_command_mark(&self, at: Position) {
        let mut marks = self.marks.lock();
        if ahead(marks.command, at) {
            marks.command = Some(at);
        }
    }

    /// The child exited with `status` (ptyd reaped it): the viewers are told.
    pub fn exited(&self, status: i32) {
        let _ignored = self.tx.send(Cmd::Exited { status });
    }

    /// Stop the actor (the PTY master closes; ptyd decides the child's fate).
    pub fn close(&self) {
        let _ignored = self.tx.send(Cmd::Close);
    }

    fn send(&self, cmd: Cmd) -> Result<(), WorkerError> {
        self.tx.send(cmd).map_err(|_gone| WorkerError::SessionClosed)
    }
}

/// What the actor hands ptyd so the session outlives this worker: a copy of every byte read from
/// the master, now and then the engine's whole state (see `Actor::checkpoint`), and the size.
#[derive(Debug)]
pub enum Tap {
    /// Output just read from the master, framed for ptyd where it was read: one copy of the
    /// bytes between the read and the socket.
    Output(OutputFrame),
    /// The terminal state, replacing everything tapped before it.
    Checkpoint {
        /// The session it is of.
        id: SessionId,
        /// VT bytes from [`GhosttyEngine::checkpoint`].
        state: Vec<u8>,
    },
    /// The terminal was resized: the size the next worker starts at.
    Resize {
        /// The session it is of.
        id: SessionId,
        /// The new size.
        size: TermSize,
    },
}

/// What the actor needs to start.
#[derive(Debug)]
pub struct SessionStart {
    /// The session this actor runs.
    pub id: SessionId,
    /// The PTY master from ptyd.
    pub master: OwnedFd,
    /// The last worker's terminal state (empty if none), replayed before `backlog`.
    pub checkpoint: Vec<u8>,
    /// Output produced since then (replayed through the engine, never sent raw).
    pub backlog: Vec<u8>,
    /// Where copies of output and checkpoints go (a task feeding ptyd).
    pub tap: mpsc::Sender<Tap>,
    /// Size of record.
    pub size: TermSize,
    /// Scrollback lines to retain.
    pub scrollback_lines: u32,
    /// The child's exit status, when ptyd reaped it before this worker adopted the session.
    pub exited: Option<i32>,
    /// Where the session says its output named a local server, so its ports get scanned.
    pub port_hints: Option<mpsc::UnboundedSender<SessionId>>,
}

/// Spawn the actor thread.
pub fn spawn(start: SessionStart) -> Result<SessionHandle, WorkerError> {
    let (tx, rx) = mpsc::unbounded_channel();
    let id = start.id;
    let (activity_tx, activity) =
        watch::channel(Activity { exited: start.exited.is_some(), ..Activity::default() });
    let handle = SessionHandle { id, tx, activity, marks: Arc::default() };
    thread::Builder::new()
        .name(format!("session-{id}"))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!(session = %id, error = %e, "runtime");
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                match Actor::new(start, rx, activity_tx) {
                    Ok(actor) => actor.run().await,
                    Err(e) => tracing::error!(session = %id, error = %e, "session start failed"),
                }
            });
        })
        .map_err(|e| {
            WorkerError::Pty(slopty_pty::PtyError::Os {
                context: "spawn session thread",
                source: e,
            })
        })?;
    Ok(handle)
}

struct Viewer {
    client: ClientId,
    sink: ClientSink,
    size: TermSize,
    /// The colours this client paints with, once it said (the driver's reach the engine).
    colors: Option<TermColors>,
    /// Its frames not done with yet (see [`Credit`]).
    in_flight: Arc<AtomicUsize>,
    /// It was not sent the last diff, or has had no frame yet: its next frame is every row.
    stale: bool,
    /// Events its sink had no room for, oldest first, and their bytes.
    queued: VecDeque<Outbound>,
    queued_bytes: usize,
    /// A task waits for room in its sink.
    waiting: bool,
    /// Its queue passed [`QUEUED_MAX_BYTES`] and was dropped: it is sent nothing until its
    /// sink has room, then told everything again.
    lost: bool,
    /// Where its frames stand against the markers it answers ([`Reach`]).
    reach: Reach,
}

/// How far a viewer's frames have got, by the markers it answers: a written frame may still
/// wait in the transport's stream buffer (up to its 1.25 MB window, five seconds at 250 kB/s),
/// and only the client can say it arrived. A viewer that answers is held to
/// [`FRAMES_UNREACHED_BYTES`] of frames it has not confirmed; one that never answers (a tool
/// reading the stream raw) is sent frames as its connection writes them.
#[derive(Debug, Default)]
struct Reach {
    /// Bytes of frames sent to it.
    sent: usize,
    /// `sent` when it was last sent a marker.
    marked: usize,
    /// Markers not answered yet, with `sent` at each.
    markers: VecDeque<(u64, usize)>,
    /// `sent` at the newest marker it answered, once it has answered one.
    reached: Option<usize>,
}

impl Reach {
    /// It may be sent another frame.
    fn open(&self) -> bool {
        self.reached.is_none_or(|at| self.sent.saturating_sub(at) < FRAMES_UNREACHED_BYTES)
    }

    /// A frame of `bytes` went to it; `true` when a marker should follow it.
    const fn frame(&mut self, bytes: usize) -> bool {
        self.sent = self.sent.saturating_add(bytes);
        self.sent.saturating_sub(self.marked) >= MARKER_EVERY_BYTES
    }

    fn marker(&mut self, id: u64) {
        self.marked = self.sent;
        if self.markers.len() == MARKERS_KEPT {
            self.markers.pop_front();
        }
        self.markers.push_back((id, self.sent));
    }

    /// It answered marker `id`; an id it was never sent (a late answer from the stream a
    /// re-attach replaced) changes nothing.
    fn answered(&mut self, id: u64) {
        let Some(n) = self.markers.iter().position(|&(m, _)| m == id) else { return };
        if let Some((_, at)) = self.markers.drain(..=n).next_back() {
            self.reached = Some(at);
        }
    }
}

impl Viewer {
    fn new(client: ClientId, sink: ClientSink, size: TermSize, colors: Option<TermColors>) -> Self {
        Self {
            client,
            sink,
            size,
            colors,
            in_flight: Arc::default(),
            stale: true,
            queued: VecDeque::new(),
            queued_bytes: 0,
            waiting: false,
            lost: false,
            reach: Reach::default(),
        }
    }

    /// It has room for another frame: fewer than [`FRAMES_IN_FLIGHT`] with its connection, and
    /// the ones it confirmed not too far behind.
    fn takes_frame(&self) -> bool {
        !self.lost && self.in_flight.load(Ordering::Acquire) < FRAMES_IN_FLIGHT && self.reach.open()
    }
}

/// How the input on its way to the PTY came: a viewer typed, clicked or pasted it (with the
/// key sequence number it carried, if any), or the engine answered a query.
#[derive(Clone, Copy, Debug)]
enum Origin {
    Viewer { key: Option<u64> },
    Engine,
}

/// The frames a viewer's input buys ahead of the pace ([`ECHO_FRAMES`]).
#[derive(Clone, Copy, Debug, Default)]
struct EchoBurst {
    until: Option<tokio::time::Instant>,
    frames: u8,
}

impl EchoBurst {
    fn arm(&mut self, now: tokio::time::Instant) {
        *self = Self { until: now.checked_add(ECHO_WINDOW), frames: ECHO_FRAMES };
    }

    /// Whether output read at `now` still counts as the echo of the last input.
    fn open(&self, now: tokio::time::Instant) -> bool {
        self.until.is_some_and(|until| now <= until)
    }

    /// Whether output read at `now` is framed at once rather than paced, spending a frame.
    fn spend(&mut self, now: tokio::time::Instant) -> bool {
        if self.frames == 0 || self.until.is_none_or(|until| now > until) {
            return false;
        }
        self.frames = self.frames.saturating_sub(1);
        true
    }
}

/// Input on its way to the PTY. The tty takes what fits in its input queue; the rest waits
/// here while the actor keeps reading, because a program that echoes its input as it reads
/// (a shell, `cat`) stops reading when its output is not read, and a writer that waited for
/// the whole paste to go in would never read that output.
#[derive(Debug, Default)]
struct Input {
    pending: BytesMut,
    /// Bytes queued and bytes written since the session started.
    queued: u64,
    written: u64,
    /// Where each request's bytes end in the stream, and where they came from.
    ends: VecDeque<(u64, Origin)>,
}

/// Input waiting for the PTY is held to this: a program that stops reading (a stopped job, a
/// hung TUI) would otherwise have a looping paste or a script's input grow the queue without
/// bound. Several full pastes of a large file fit.
const INPUT_MAX_BYTES: usize = 16 << 20;

/// Input refused because [`INPUT_MAX_BYTES`] already wait for the program.
#[derive(Debug, thiserror::Error)]
#[error("the terminal's program is not reading its input; {} MiB are waiting already", INPUT_MAX_BYTES >> 20)]
struct InputFull;

/// Where one keystroke is on its way through the actor, for the trace that takes the echo
/// round trip apart (MEASUREMENTS.md, "the keystroke path, stage by stage"). Three stamps at
/// trace level and nothing else: the cost when tracing is off is two `Option` writes.
#[derive(Debug, Default)]
struct EchoTrace {
    /// Input bytes were written to the master, and no output has been read since.
    input_at: Option<tokio::time::Instant>,
    /// The first read after that input returned, and no frame has gone out since.
    read_at: Option<tokio::time::Instant>,
}

struct Actor {
    id: SessionId,
    engine: GhosttyEngine,
    master: Arc<PtyMaster>,
    rx: mpsc::UnboundedReceiver<Cmd>,
    /// A viewer's sink has room again for what waits for it (sent by the task waiting on it).
    drained_tx: mpsc::UnboundedSender<(ClientId, ClientSink)>,
    drained_rx: mpsc::UnboundedReceiver<(ClientId, ClientSink)>,
    /// Woken when a viewer's frame is done with ([`Credit`]).
    room: Arc<Notify>,
    /// A diff is waiting for a viewer that follows the diffs to have room for it.
    owed: bool,
    input: Input,
    echo: EchoTrace,
    burst: EchoBurst,
    viewers: Vec<Viewer>,
    driver: Option<ClientId>,
    /// The driver's sink closed and no detach has said why yet: the seat is kept for it, with
    /// the sink, until its connection detaches that sink (then it passes on) or the client
    /// attaches again (then it keeps it). A closed sink alone is also what a re-attach looks
    /// like while the new attach is still on its way, and handing the size to another viewer
    /// then took it from a client that never left.
    orphan: Option<(ClientId, ClientSink)>,
    /// The next `TermEvent::Marker` id.
    next_marker: u64,
    title: Option<String>,
    cwd: Option<String>,
    /// The program's colour changes (OSC 4/10/11/12): broadcast, and sent to a late attach.
    program_colors: ColorOverrides,
    /// [`crate::repo::root_of`] of `cwd`, resolved once per change rather than per summary.
    repo: Option<String>,
    exited: Option<i32>,
    /// The master read EOF or failed: the child is gone, whatever its status turns out to be.
    pty_closed: bool,
    /// Highest key seq written to the PTY.
    written_seq: u64,
    /// `written_seq` as of the most recent PTY read: what the next frame may acknowledge.
    ack_seq: u64,
    frame_due: Option<tokio::time::Instant>,
    /// When the last frame left, for [`MIN_FRAME_INTERVAL`].
    last_frame: Option<tokio::time::Instant>,
    /// When the program's render hold (synchronized output) times out: the frame is taken then
    /// even if the program never writes again.
    hold_due: Option<tokio::time::Instant>,
    /// Copies of output and checkpoints, for ptyd.
    tap: mpsc::Sender<Tap>,
    /// Output has arrived since the last checkpoint.
    dirty_since_checkpoint: bool,
    /// Bytes tapped since the last checkpoint (a checkpoint empties ptyd's ring).
    tapped_since_checkpoint: usize,
    /// A tap did not fit the channel: ptyd's ring has a hole until the next checkpoint.
    tap_lost: bool,
    /// A resize ptyd has not been told of, because the tap channel was full.
    size_untold: Option<TermSize>,
    /// When the next checkpoint is due (`CHECKPOINT_AFTER` past the last output).
    checkpoint_due: Option<tokio::time::Instant>,
    /// Where the output stands in VT syntax, so a quiet-spell checkpoint never cuts a sequence.
    boundary: Boundary,
    /// What waiters watch.
    activity: watch::Sender<Activity>,
    /// See [`SessionStart::port_hints`].
    port_hints: Option<mpsc::UnboundedSender<SessionId>>,
    /// No port hint is sent before this, so a flood of addresses is one hint a second.
    next_hint: Option<tokio::time::Instant>,
}

/// A session's output is looked at for a local server's address at most this often once it
/// named one.
const HINT_EVERY: Duration = Duration::from_secs(1);

/// A checkpoint follows this much quiet after output. Shorter means a crashed worker loses less
/// of what a fresh one cannot replay from the ring; longer means fewer formatter runs.
const CHECKPOINT_AFTER: Duration = Duration::from_millis(500);
/// A checkpoint is also taken once this many bytes were tapped since the last one, so ptyd's
/// ring (4 MiB by default) never overflows under a flood and the replay stays bounded.
const CHECKPOINT_EVERY_BYTES: usize = 1 << 20;
/// Larger states are not sent: ptyd hands the state and its ring over in one frame, and a
/// state this size means a history far past any configured scrollback.
const CHECKPOINT_MAX_BYTES: usize = slopty_pty::protocol::MAX_CHECKPOINT_BYTES;

/// When the frame for output that arrived at `now` should go out: now, or at the end of the
/// previous frame's `MIN_FRAME_INTERVAL` if that is later.
fn frame_due_after(
    now: tokio::time::Instant,
    last_frame: Option<tokio::time::Instant>,
) -> tokio::time::Instant {
    match last_frame.and_then(|at| at.checked_add(MIN_FRAME_INTERVAL)) {
        Some(paced) if paced > now => paced,
        _ => now,
    }
}

impl Actor {
    fn new(
        start: SessionStart,
        rx: mpsc::UnboundedReceiver<Cmd>,
        activity: watch::Sender<Activity>,
    ) -> Result<Self, WorkerError> {
        let mut engine = GhosttyEngine::new(EngineConfig {
            size: start.size,
            scrollback_lines: start.scrollback_lines,
        })?;
        if !start.checkpoint.is_empty() {
            engine.write(&start.checkpoint);
        }
        if !start.backlog.is_empty() {
            engine.write(&start.backlog);
        }
        // What the replay said is state, not news: the last worker delivered the bells, the
        // notifications, the clipboard writes and the query answers already (an answer written
        // now would land in a shell that is not asking). The title, the directory and the
        // program's colours are what the first attach is told.
        let (mut title, mut cwd, mut program_colors) = (None, None, ColorOverrides::default());
        for ev in engine.drain_events() {
            match ev {
                EngineEvent::Title(t) => title = Some(t),
                EngineEvent::Cwd(c) => cwd = Some(c),
                EngineEvent::Colors(c) => program_colors = c,
                EngineEvent::PtyWrite(_)
                | EngineEvent::Bell
                | EngineEvent::Notification { .. }
                | EngineEvent::ClipboardWrite { .. } => {}
            }
        }
        let repo = cwd.as_deref().and_then(crate::repo::root_of_str);
        let master = Arc::new(PtyMaster::new(start.master)?);
        let (drained_tx, drained_rx) = mpsc::unbounded_channel();
        Ok(Self {
            id: start.id,
            engine,
            master,
            rx,
            drained_tx,
            drained_rx,
            room: Arc::default(),
            owed: false,
            burst: EchoBurst::default(),
            input: Input::default(),
            viewers: Vec::new(),
            driver: None,
            orphan: None,
            next_marker: 0,
            title,
            cwd,
            program_colors,
            repo,
            exited: start.exited,
            pty_closed: start.exited.is_some(),
            written_seq: 0,
            ack_seq: 0,
            frame_due: None,
            last_frame: None,
            hold_due: None,
            echo: EchoTrace::default(),
            tap: start.tap,
            // Whatever we replayed is in ptyd's ring or checkpoint already; the first checkpoint
            // of this worker folds it and anything the replay answered into one state.
            dirty_since_checkpoint: true,
            tapped_since_checkpoint: 0,
            tap_lost: false,
            size_untold: None,
            checkpoint_due: None,
            boundary: Boundary::default(),
            activity,
            port_hints: start.port_hints,
            next_hint: None,
        })
    }

    async fn run(mut self) {
        let mut buf = vec![0_u8; READ_BUF];
        let pty = Arc::clone(&self.master);
        let room = Arc::clone(&self.room);
        // The replay answered nothing yet: take its title and cwd, then checkpoint at once.
        // ptyd handed us its ring with the master, so until this lands another restart would
        // have nothing but the previous checkpoint.
        self.read_line_discipline();
        self.after_output();
        self.checkpoint(true);
        loop {
            let writing = !self.input.pending.is_empty() && !self.pty_closed;
            tokio::select! {
                biased;
                cmd = self.rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    if !self.handle(cmd) {
                        break;
                    }
                }
                Some((client, sink)) = self.drained_rx.recv() => self.sink_has_room(client, &sink),
                () = room.notified() => self.frame_room(),
                writable = pty.writable(), if writing => match writable {
                    Ok(()) => self.write_input(),
                    Err(e) => self.input_failed(&e),
                },
                read = pty.read(&mut buf), if !self.pty_closed => match read {
                    Ok(0) => {
                        tracing::info!(session = %self.id, "pty closed");
                        self.pty_closed = true;
                        self.flush_frame();
                        self.activity.send_if_modified(|a| !std::mem::replace(&mut a.exited, true));
                    }
                    Ok(n) => self.on_output(buf.get(..n).unwrap_or_default()),
                    Err(e) => {
                        tracing::warn!(session = %self.id, error = %e, "pty read failed");
                        self.pty_closed = true;
                        self.activity.send_if_modified(|a| !std::mem::replace(&mut a.exited, true));
                    }
                },
                () = sleep_until_due(self.frame_due) => {
                    self.frame_due = None;
                    self.flush_frame();
                }
                () = sleep_until_due(self.hold_due) => {
                    self.flush_frame();
                    self.arm_hold();
                }
                () = sleep_until_due(self.checkpoint_due) => {
                    self.checkpoint_due = None;
                    self.checkpoint(false);
                }
            }
        }
        tracing::debug!(session = %self.id, "actor stopped");
    }

    /// Bytes just read from the master.
    fn on_output(&mut self, bytes: &[u8]) {
        self.ack_seq = self.written_seq;
        if let Some(input_at) = self.echo.input_at
            && self.echo.read_at.is_none()
        {
            let now = tokio::time::Instant::now();
            self.echo.read_at = Some(now);
            tracing::trace!(
                session = %self.id,
                echo_us = now.saturating_duration_since(input_at).as_micros(),
                bytes = bytes.len(),
                "echo read"
            );
        }
        if tracing::enabled!(tracing::Level::TRACE) {
            let shown = String::from_utf8_lossy(bytes);
            tracing::trace!(session = %self.id, n = bytes.len(), bytes = %shown.escape_debug(), "pty read");
        }
        self.engine.write(bytes);
        self.read_line_discipline();
        self.hint_ports(bytes);
        self.tap_output(bytes);
        self.after_output();
        self.arm_hold();
        let ended = self.engine.commands_ended();
        self.activity.send_modify(|a| {
            a.output = a.output.wrapping_add(1);
            a.commands_ended = ended;
        });
        let now = tokio::time::Instant::now();
        // Frame right here when nothing paces it, or when it may be a viewer's echo. A timer
        // set to "now" is not now: tokio rounds a deadline up to its next millisecond tick and
        // the driver parks until then, which put 1.4 ms between a keystroke's echo and its
        // frame (MEASUREMENTS.md, "the keystroke path, stage by stage"). The timer is for the
        // flood, where the next frame is owed later.
        let due = frame_due_after(now, self.last_frame);
        if due <= now || self.burst.spend(now) {
            self.frame_due = None;
            self.flush_frame();
        } else if self.frame_due.is_none() {
            self.frame_due = Some(due);
        }
    }

    /// Tell the engine whether the tty echoes and edits lines now, so the next frame's modes
    /// say so and a client never draws a guess at a password. A program changes them (`stty
    /// -echo`) before it prints the prompt they are for, so reading them after each read of its
    /// output is in time: one `tcgetattr`, about a microsecond (MEASUREMENTS.md, "the line
    /// discipline per read").
    fn read_line_discipline(&mut self) {
        match self.master.line_discipline() {
            Ok(d) => self.engine.set_line_discipline(slopty_engine::LineDiscipline {
                echo: d.echo,
                canonical: d.canonical,
            }),
            Err(e) => tracing::debug!(session = %self.id, error = %e, "line discipline unread"),
        }
    }

    /// A viewer's frame was done with: build what is owed now, or when the pace allows.
    fn frame_room(&mut self) {
        let wanted = self.viewers.iter().any(|v| v.takes_frame() && (v.stale || self.owed));
        if !wanted {
            return;
        }
        let now = tokio::time::Instant::now();
        let due = frame_due_after(now, self.last_frame);
        if due <= now {
            self.frame_due = None;
            self.flush_frame();
        } else if self.frame_due.is_none() {
            self.frame_due = Some(due);
        }
    }

    /// Tell the daemon when output names a local server, so it scans for the listener.
    fn hint_ports(&mut self, bytes: &[u8]) {
        let Some(hints) = &self.port_hints else { return };
        let now = tokio::time::Instant::now();
        if self.next_hint.is_some_and(|at| now < at) || !crate::ports::mentions_local_server(bytes)
        {
            return;
        }
        self.next_hint = now.checked_add(HINT_EVERY);
        let _sent = hints.send(self.id);
    }

    /// Wake when the program's render hold times out. One that already has is ended by the
    /// next frame built, so it needs no timer (and with nobody watching, none is built).
    fn arm_hold(&mut self) {
        let now = tokio::time::Instant::now();
        self.hold_due = self
            .engine
            .hold_remaining()
            .filter(|left| !left.is_zero())
            .map(|left| now.checked_add(left).unwrap_or(now));
    }

    /// Copy output to ptyd's ring and schedule the checkpoint that will fold it away.
    fn tap_output(&mut self, bytes: &[u8]) {
        self.dirty_since_checkpoint = true;
        self.boundary.feed(bytes);
        if !self.tap_lost {
            self.tapped_since_checkpoint = self.tapped_since_checkpoint.saturating_add(bytes.len());
            let sent = match OutputFrame::new(self.id, bytes) {
                Ok(frame) => self.tap.try_send(Tap::Output(frame)).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            if let Err(e) = sent {
                // Full or gone: the ring has a hole. The next checkpoint replaces the ring, so
                // pull it forward rather than leaving a replay that would misparse mid-sequence.
                tracing::warn!(session = %self.id, error = %e, "output tap dropped; checkpointing early");
                self.tap_lost = true;
            }
        }
        if self.tap_lost || self.tapped_since_checkpoint >= CHECKPOINT_EVERY_BYTES {
            // Right here rather than through the timer: the select prefers the master, and a
            // flood that keeps it readable would starve a timer indefinitely.
            self.checkpoint(true);
        } else {
            self.checkpoint_after_quiet();
        }
    }

    fn checkpoint_after_quiet(&mut self) {
        let now = tokio::time::Instant::now();
        self.checkpoint_due = Some(now.checked_add(CHECKPOINT_AFTER).unwrap_or(now));
    }

    /// Hand ptyd the engine's whole state, so a worker that replaces this one starts from it.
    /// Unless `force`d, a checkpoint waits while the output stands inside an escape sequence
    /// or a character: it replaces the bytes before it, and the rest of that sequence would
    /// print as text after a restart.
    fn checkpoint(&mut self, force: bool) {
        if !self.dirty_since_checkpoint {
            return;
        }
        // A state the queue has no room for would be formatted for nothing; while the ring has
        // a hole that is every read of a flood.
        if self.tap.capacity() == 0 || (!force && !self.boundary.is_ground()) {
            self.checkpoint_after_quiet();
            return;
        }
        if let Some(size) = self.size_untold.take() {
            self.tell_size(size);
        }
        let mut state = Vec::new();
        if let Some(t) = &self.title {
            // Not part of what the formatter emits; the next worker learns it the way this one did.
            state.extend_from_slice(b"\x1b]0;");
            state.extend_from_slice(t.as_bytes());
            state.extend_from_slice(b"\x1b\\");
        }
        if let Err(e) = self.engine.checkpoint(&mut state) {
            tracing::warn!(session = %self.id, error = %e, "checkpoint failed");
            return;
        }
        if state.len() > CHECKPOINT_MAX_BYTES {
            // ptyd would refuse the frame; keep the ring instead (it holds the output since
            // the last checkpoint that did fit) and try again at the next quiet spell.
            tracing::warn!(session = %self.id, bytes = state.len(), "checkpoint too large; skipped");
            self.tapped_since_checkpoint = 0;
            return;
        }
        match self.tap.try_send(Tap::Checkpoint { id: self.id, state }) {
            Ok(()) => {
                self.dirty_since_checkpoint = false;
                self.tapped_since_checkpoint = 0;
                self.tap_lost = false;
            }
            Err(e) => {
                // Try again after the next quiet spell; the ring keeps growing meanwhile.
                tracing::warn!(session = %self.id, error = %e, "checkpoint not sent");
                self.checkpoint_after_quiet();
            }
        }
    }

    /// Tell ptyd the size, so a worker that replaces this one starts the replay at it.
    fn tell_size(&mut self, size: TermSize) {
        if self.tap.try_send(Tap::Resize { id: self.id, size }).is_err() {
            self.size_untold = Some(size);
        }
    }

    /// Side effects of the bytes just consumed.
    fn after_output(&mut self) {
        for ev in self.engine.drain_events() {
            match ev {
                // An answer to a query the program will not read is lost with nobody to tell.
                EngineEvent::PtyWrite(bytes) => {
                    let _refused = self.queue_input(&bytes, Origin::Engine);
                }
                EngineEvent::Bell => self.broadcast(&TermEvent::Bell),
                EngineEvent::Colors(colors) => {
                    self.program_colors = colors.clone();
                    self.broadcast(&TermEvent::Colors(colors));
                }
                EngineEvent::Notification { title, body } => {
                    self.broadcast(&TermEvent::Notification { title, body });
                }
                EngineEvent::Title(t) => {
                    self.title = Some(t.clone());
                    self.broadcast(&TermEvent::Title(t));
                }
                EngineEvent::Cwd(c) => {
                    // A handful of `stat` calls, only when the shell says it moved. This is
                    // the actor's own thread, which holds the PTY master and nothing else.
                    self.repo = crate::repo::root_of_str(&c);
                    self.cwd = Some(c.clone());
                    self.broadcast(&TermEvent::Cwd { path: c, repo: self.repo.clone() });
                }
                EngineEvent::ClipboardWrite { text } => {
                    // Same ceiling as pasteboard sync: a program can OSC 52 a whole file, and
                    // that would sit ahead of every frame on the session stream.
                    if text.len() > MAX_OSC52_BYTES {
                        tracing::warn!(session = %self.id, bytes = text.len(), "OSC 52 write too large; dropped");
                    } else {
                        self.broadcast(&TermEvent::ClipboardWrite { text });
                    }
                }
            }
        }
    }

    /// Queue bytes for the PTY behind whatever is still waiting, and write what fits now. A
    /// key sequence number they carry is acknowledged once they are all written. Bytes that
    /// would take the queue past [`INPUT_MAX_BYTES`] are refused whole.
    fn queue_input(&mut self, bytes: &[u8], origin: Origin) -> Result<(), InputFull> {
        if bytes.is_empty() || self.pty_closed {
            return Ok(());
        }
        if self.input.pending.len().saturating_add(bytes.len()) > INPUT_MAX_BYTES {
            tracing::warn!(
                session = %self.id,
                bytes = bytes.len(),
                queued = self.input.pending.len(),
                "input refused: the program is not reading"
            );
            return Err(InputFull);
        }
        self.input.pending.extend_from_slice(bytes);
        self.input.queued = self.input.queued.saturating_add(bytes.len() as u64);
        self.input.ends.push_back((self.input.queued, origin));
        self.write_input();
        Ok(())
    }

    /// Write as much of the queued input as the tty takes without waiting.
    fn write_input(&mut self) {
        let from = tokio::time::Instant::now();
        while !self.input.pending.is_empty() {
            match self.master.try_write(&self.input.pending) {
                Ok(0) => break,
                Ok(n) => {
                    self.input.pending.advance(n);
                    self.input.written = self.input.written.saturating_add(n as u64);
                }
                Err(e) => return self.input_failed(&e),
            }
        }
        let (mut done, mut typed) = (false, false);
        while let Some(&(end, origin)) = self.input.ends.front() {
            if end > self.input.written {
                break;
            }
            self.input.ends.pop_front();
            if let Origin::Viewer { key } = origin {
                typed = true;
                if let Some(seq) = key {
                    self.written_seq = self.written_seq.max(seq);
                }
            }
            done = true;
        }
        if typed {
            self.burst.arm(tokio::time::Instant::now());
        }
        if done {
            let now = tokio::time::Instant::now();
            // The stamp the echo trace measures from; a keystroke that lands while the
            // previous one is still in flight restarts the trace, which is what a bench that
            // waits for each frame never does.
            self.echo = EchoTrace { input_at: Some(now), read_at: None };
            tracing::trace!(
                session = %self.id,
                write_us = now.saturating_duration_since(from).as_micros(),
                queued = self.input.pending.len(),
                "pty input written"
            );
        }
    }

    fn input_failed(&mut self, e: &slopty_pty::PtyError) {
        tracing::warn!(session = %self.id, error = %e, "pty write failed");
        self.input.pending.clear();
        self.input.ends.clear();
        self.input.written = self.input.queued;
        self.broadcast(&TermEvent::Error(e.to_string()));
    }

    /// The next diff for the viewers that follow the diffs and have room for it, and every row
    /// for those that missed one. With nobody to diff for, what changed is dropped: whoever
    /// comes next is sent every row. With nobody who has room, it waits in the engine, which
    /// keeps collecting what changes, until a frame is done with ([`Self::frame_room`]).
    fn flush_frame(&mut self) {
        if self.viewers.iter().all(|v| v.stale) {
            self.owed = false;
            self.engine.discard_frame();
        } else if self.viewers.iter().any(|v| !v.stale && v.takes_frame()) {
            self.owed = false;
            match self.engine.take_frame(self.ack_seq) {
                Ok(Some(frame)) => {
                    let images = self.engine.drain_images();
                    self.send_frame(frame, images);
                    self.frame_sent();
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(session = %self.id, error = %e, "frame build failed");
                    self.broadcast(&TermEvent::Error(e.to_string()));
                }
            }
        } else {
            self.owed = true;
        }
        self.send_whole_frames();
    }

    /// A frame left: the pace counts from here, and the echo trace ends.
    fn frame_sent(&mut self) {
        let now = tokio::time::Instant::now();
        self.last_frame = Some(now);
        if let (Some(input_at), Some(read_at)) =
            (self.echo.input_at.take(), self.echo.read_at.take())
        {
            tracing::trace!(
                session = %self.id,
                read_to_frame_us = now.saturating_duration_since(read_at).as_micros(),
                input_to_frame_us = now.saturating_duration_since(input_at).as_micros(),
                "frame flushed"
            );
        }
    }

    /// Hand a frame everyone takes (a diff, or every row after a resize) to each viewer that
    /// follows the diffs, the images it places first. A viewer with no room for it misses it,
    /// and is sent every row once it has.
    fn send_frame(&mut self, frame: Frame, images: Vec<ImageUpload>) {
        let images: Vec<Outbound> =
            images.into_iter().filter_map(|u| self.encode(&image_event(u))).collect();
        let echo = !frame.full && self.burst.open(tokio::time::Instant::now());
        let Some(mut frame) = self.encode(&TermEvent::Frame(frame)) else { return };
        frame.echo = echo;
        let mut gone = Vec::new();
        for i in 0..self.viewers.len() {
            let Some(v) = self.viewers.get_mut(i) else { continue };
            if v.stale {
                continue;
            }
            if !v.takes_frame() {
                v.stale = true;
                continue;
            }
            let open = images.iter().all(|image| self.deliver(i, image.clone()))
                && self.deliver_frame(i, &frame);
            if !open {
                gone.push(i);
            }
        }
        self.remove(gone);
    }

    /// Hand viewer `i` a frame, holding a place among its frames in flight, and a marker after
    /// it when the frames since the last one call for it. `false` when the viewer is gone.
    fn deliver_frame(&mut self, i: usize, frame: &Outbound) -> bool {
        let Some(v) = self.viewers.get_mut(i) else { return true };
        if v.lost {
            // An image ahead of it overflowed the queue: the frame would be dropped, and
            // counting it would hold the viewer to frames it can never confirm.
            return true;
        }
        let claimed = frame.claimed(v, &self.room);
        let marker_due = v.reach.frame(frame.wire.len());
        if !self.deliver(i, claimed) {
            return false;
        }
        // The frame itself may have overflowed the queue, which starts its count again.
        if !marker_due || self.viewers.get(i).is_none_or(|v| v.lost) {
            return true;
        }
        let id = self.next_marker;
        self.next_marker = self.next_marker.wrapping_add(1);
        let Some(marker) = self.encode(&TermEvent::Marker { id }) else { return true };
        if let Some(v) = self.viewers.get_mut(i) {
            v.reach.marker(id);
        }
        self.deliver(i, marker)
    }

    /// Every viewer that missed a diff and has room is sent every row, at the others'
    /// sequence number.
    fn send_whole_frames(&mut self) {
        let mut gone = Vec::new();
        for i in 0..self.viewers.len() {
            if self.viewers.get(i).is_some_and(|v| v.stale && v.takes_frame())
                && !self.send_whole(i)
            {
                gone.push(i);
            }
        }
        self.remove(gone);
    }

    /// Every row for viewer `i`, with the images it needs. `false` when the viewer is gone.
    fn send_whole(&mut self, i: usize) -> bool {
        let (frame, images) = match self.engine.join_frame(self.ack_seq) {
            Ok(joined) => joined,
            Err(e) => {
                tracing::error!(session = %self.id, error = %e, "whole frame failed");
                return self
                    .encode(&TermEvent::Error(e.to_string()))
                    .is_none_or(|out| self.deliver(i, out));
            }
        };
        let Some(frame) = self.encode(&TermEvent::Frame(frame)) else { return true };
        let Some(v) = self.viewers.get_mut(i) else { return true };
        v.stale = false;
        images
            .into_iter()
            .all(|u| self.encode(&image_event(u)).is_none_or(|image| self.deliver(i, image)))
            && self.deliver_frame(i, &frame)
    }

    /// Drop the viewers at `gone` (ascending indices), whose sinks closed. A closed sink does
    /// not say whether its client left or is attaching again, so the driver's seat is kept
    /// for it until a detach or an attach does ([`Actor::orphan`]).
    fn remove(&mut self, gone: Vec<usize>) {
        for i in gone.into_iter().rev() {
            if i < self.viewers.len() {
                let v = self.viewers.swap_remove(i);
                if self.driver == Some(v.client) {
                    self.orphan = Some((v.client, v.sink));
                }
            }
        }
    }

    /// Encode `ev` once and hand it to every viewer.
    fn broadcast(&mut self, ev: &TermEvent) {
        if self.viewers.is_empty() {
            return;
        }
        let Some(out) = self.encode(ev) else { return };
        let mut gone = Vec::new();
        for i in 0..self.viewers.len() {
            if !self.deliver(i, out.clone()) {
                gone.push(i);
            }
        }
        self.remove(gone);
    }

    fn send_to(&mut self, client: ClientId, ev: &TermEvent) {
        let Some(i) = self.viewers.iter().position(|v| v.client == client) else {
            return;
        };
        if let Some(out) = self.encode(ev) {
            // A closed sink is its connection's to detach.
            let _open = self.deliver(i, out);
        }
    }

    fn encode(&self, ev: &TermEvent) -> Option<Outbound> {
        Outbound::encode(ev)
            .inspect_err(|e| tracing::error!(session = %self.id, error = %e, "event not encoded"))
            .ok()
    }

    /// Hand `out` to viewer `i` behind whatever waits for it; if its sink is full it waits in
    /// order for room. `false` when the viewer is gone.
    fn deliver(&mut self, i: usize, out: Outbound) -> bool {
        let Some(v) = self.viewers.get_mut(i) else { return true };
        if v.lost {
            return true;
        }
        let out = if v.queued.is_empty() {
            match v.sink.try_send(out) {
                Ok(()) => return true,
                Err(mpsc::error::TrySendError::Closed(_)) => return false,
                Err(mpsc::error::TrySendError::Full(out)) => out,
            }
        } else {
            out
        };
        v.queued_bytes = v.queued_bytes.saturating_add(out.wire.len());
        v.queued.push_back(out);
        if v.queued_bytes > QUEUED_MAX_BYTES {
            tracing::warn!(session = %self.id, client = %v.client, bytes = v.queued_bytes, "viewer not reading; told everything again once it drains");
            v.queued.clear();
            v.queued_bytes = 0;
            v.lost = true;
            v.stale = true;
            // The frames and markers dropped with the queue will never be confirmed.
            v.reach = Reach::default();
        }
        self.wait_for_room(i);
        true
    }

    /// Wake the actor when viewer `i`'s sink has room for what waits for it.
    fn wait_for_room(&mut self, i: usize) {
        let Some(v) = self.viewers.get_mut(i) else { return };
        if v.waiting {
            return;
        }
        v.waiting = true;
        let (client, sink, drained) = (v.client, v.sink.clone(), self.drained_tx.clone());
        let room = (sink.max_capacity() / 2).clamp(1, v.queued.len().max(1));
        tokio::task::spawn_local(async move {
            if sink.reserve_many(room).await.is_ok() {
                let _ignored = drained.send((client, sink));
            }
        });
    }

    /// A viewer's sink has room: what waited for it goes in, in order. One whose queue was
    /// dropped is told everything again, after the others take the diff they are owed.
    fn sink_has_room(&mut self, client: ClientId, sink: &ClientSink) {
        let Some(i) =
            self.viewers.iter().position(|v| v.client == client && v.sink.same_channel(sink))
        else {
            return;
        };
        let Some(v) = self.viewers.get_mut(i) else { return };
        v.waiting = false;
        if v.lost {
            self.flush_frame();
            if let Some(v) = self.viewers.get_mut(i) {
                v.lost = false;
            }
            tracing::info!(session = %self.id, %client, "viewer caught up");
            let driving = self.driver == Some(client);
            self.send_to(client, &TermEvent::Driver { you: driving });
            self.introduce(client);
            return;
        }
        while let Some(out) = v.queued.pop_front() {
            let len = out.wire.len();
            match v.sink.try_send(out) {
                Ok(()) => v.queued_bytes = v.queued_bytes.saturating_sub(len),
                Err(mpsc::error::TrySendError::Full(out)) => {
                    v.queued.push_front(out);
                    self.wait_for_room(i);
                    return;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => return self.remove(vec![i]),
            }
        }
        self.frame_room();
    }

    /// What a viewer joining is told: the title, the directory, the program's colours, every
    /// row with the images on them (now if it has room, else when it has), and the exit if the
    /// child is gone.
    fn introduce(&mut self, client: ClientId) {
        if let Some(t) = self.title.clone() {
            self.send_to(client, &TermEvent::Title(t));
        }
        if let Some(path) = self.cwd.clone() {
            self.send_to(client, &TermEvent::Cwd { path, repo: self.repo.clone() });
        }
        if self.program_colors != ColorOverrides::default() {
            self.send_to(client, &TermEvent::Colors(self.program_colors.clone()));
        }
        if let Some(i) = self.viewers.iter().position(|v| v.client == client) {
            if let Some(v) = self.viewers.get_mut(i) {
                v.stale = true;
            }
            if self.viewers.get(i).is_some_and(Viewer::takes_frame) && !self.send_whole(i) {
                self.remove(vec![i]);
            }
        }
        if let Some(status) = self.exited {
            self.send_to(client, &TermEvent::Exited { status });
        }
    }

    /// `client` detached: the size passes to another viewer if it drove and has no other
    /// view of the session.
    fn on_viewer_gone(&mut self, client: ClientId) {
        if self.driver == Some(client) && !self.viewers.iter().any(|v| v.client == client) {
            self.orphan = None;
            self.driver = None;
            // Hand the size to the first remaining viewer, so a phone that joined after the
            // laptop left still gets a fitting terminal.
            if let Some(next) = self.viewers.first().map(|v| (v.client, v.size)) {
                self.driver = Some(next.0);
                self.send_to(next.0, &TermEvent::Driver { you: true });
                self.apply_size(next.1);
            }
        }
    }

    /// The colours `client` paints with become the terminal's defaults (what colour queries
    /// answer), when it has said them; a driver that never did leaves the defaults alone.
    fn apply_colors_of(&mut self, client: ClientId) {
        let Some(colors) = self.viewers.iter().find(|v| v.client == client).and_then(|v| v.colors)
        else {
            return;
        };
        if let Err(e) = self.engine.set_colors(&colors) {
            tracing::error!(session = %self.id, error = %e, "engine colours failed");
        }
    }

    fn apply_size(&mut self, size: TermSize) {
        if size == self.engine.size() {
            return;
        }
        if let Err(e) = slopty_pty::pty::set_size(self.master.as_fd(), size) {
            tracing::warn!(session = %self.id, error = %e, "TIOCSWINSZ failed");
        }
        if let Err(e) = self.engine.resize(size) {
            tracing::error!(session = %self.id, error = %e, "engine resize failed");
            return;
        }
        self.tell_size(size);
        // The checkpoint ptyd holds was formatted at the old size; the next one is at this.
        self.dirty_since_checkpoint = true;
        self.checkpoint_after_quiet();
        self.broadcast(&TermEvent::Resized { cols: size.cols, rows: size.rows });
        match self.engine.full_frame(self.ack_seq) {
            Ok(frame) => {
                let images = self.engine.drain_images();
                self.send_frame(frame, images);
            }
            Err(e) => tracing::error!(session = %self.id, error = %e, "full frame failed"),
        }
    }

    /// Returns `false` when the actor should stop.
    fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Attach { client, size, sink } => {
                // The others take what they are owed before the joiner's frame is built at
                // their sequence number; built first, it would take their diff from them.
                self.flush_frame();
                // A re-attach (resync, reconnect) keeps the colours the client already said.
                let colors =
                    self.viewers.iter().find(|v| v.client == client).and_then(|v| v.colors);
                self.viewers.retain(|v| v.client != client);
                if self.orphan.as_ref().is_some_and(|(c, _)| *c == client) {
                    self.orphan = None;
                }
                self.viewers.push(Viewer::new(client, sink, size, colors));
                if self.driver.is_none() {
                    self.driver = Some(client);
                    self.send_to(client, &TermEvent::Driver { you: true });
                    self.apply_size(size);
                    self.apply_colors_of(client);
                } else if self.driver == Some(client) {
                    // A reconnecting driver (relaunched app) learns it still drives.
                    self.send_to(client, &TermEvent::Driver { you: true });
                    self.apply_size(size);
                    self.apply_colors_of(client);
                }
                self.introduce(client);
            }
            Cmd::Detach { client, sink } => {
                // Scoped to a sink, only that sink's viewer goes: the same client may be
                // attached again through another one, and that one stays.
                let through = |s: &ClientSink| sink.as_ref().is_none_or(|t| t.same_channel(s));
                let before = self.viewers.len();
                self.viewers.retain(|v| v.client != client || !through(&v.sink));
                let orphaned =
                    self.orphan.as_ref().is_some_and(|(c, s)| *c == client && through(s));
                if self.viewers.len() != before || orphaned {
                    self.on_viewer_gone(client);
                }
            }
            Cmd::Reserve { client } => {
                self.orphan = None;
                if let Some(old) = self.driver.replace(client)
                    && old != client
                {
                    self.send_to(old, &TermEvent::Driver { you: false });
                }
            }
            Cmd::Request { client, req, at } => self.request(client, req, at),
            Cmd::Snapshot { reply } => {
                let _ignored = reply.send(Snapshot {
                    title: self.title.clone(),
                    cwd: self.cwd.clone(),
                    repo: self.repo.clone(),
                    size: self.engine.size(),
                    viewers: u16::try_from(self.viewers.len()).unwrap_or(u16::MAX),
                    exited: self.exited,
                });
            }
            Cmd::ResizeUnviewed { size, reply } => {
                let viewers = u16::try_from(self.viewers.len()).unwrap_or(u16::MAX);
                if viewers == 0 {
                    self.apply_size(size);
                }
                let _ignored = reply.send(viewers);
            }
            Cmd::Probe { reply } => {
                // A child that already exited has no foreground process; asking would only
                // read whatever the kernel put in its place.
                let foreground = (!self.pty_closed)
                    .then(|| slopty_pty::process::foreground(self.master.as_fd()))
                    .flatten();
                let _ignored = reply.send(Probe {
                    foreground,
                    title: self.title.clone(),
                    cwd: self.cwd.clone(),
                });
            }
            Cmd::Exited { status } => {
                self.exited = Some(status);
                self.activity.send_if_modified(|a| !std::mem::replace(&mut a.exited, true));
                self.flush_frame();
                self.broadcast(&TermEvent::Exited { status });
            }
            Cmd::Read { read, reply } => {
                let _ignored = reply.send(self.read(read));
            }
            Cmd::Close => return false,
        }
        true
    }

    fn read(&self, read: Read) -> Result<Text, WorkerError> {
        let engine = &self.engine;
        Ok(match read {
            Read::Screen => Text::Screen {
                screen: engine.screen_text()?,
                title: self.title.clone(),
                cwd: self.cwd.clone(),
            },
            Read::Output { since, max } => Text::Output(engine.text_lines(since, max)?),
            Read::Commands { since } => Text::Commands(engine.commands(since)?),
            Read::Position => Text::Position(engine.position()?),
            Read::Since(from) => Text::Since(engine.text_since(from)?),
            Read::EndedAfter(from) => Text::Ended(engine.ended_after(from)),
        })
    }

    fn request(&mut self, client: ClientId, req: TermRequest, at: tokio::time::Instant) {
        tracing::trace!(session = %self.id, queued_us = at.elapsed().as_micros(), "request");
        let mut bytes = Vec::new();
        let mut key = None;
        let result = match req {
            TermRequest::Attach { .. } | TermRequest::Detach | TermRequest::Close => Ok(()),
            TermRequest::Resize(size) => {
                if let Some(v) = self.viewers.iter_mut().find(|v| v.client == client) {
                    v.size = size;
                }
                if self.driver == Some(client) {
                    self.apply_size(size);
                }
                Ok(())
            }
            TermRequest::Reached { marker } => {
                if let Some(v) = self.viewers.iter_mut().find(|v| v.client == client) {
                    v.reach.answered(marker);
                }
                self.frame_room();
                Ok(())
            }
            TermRequest::Colors(colors) => {
                if let Some(v) = self.viewers.iter_mut().find(|v| v.client == client) {
                    v.colors = Some(colors);
                }
                if self.driver == Some(client) {
                    self.apply_colors_of(client);
                }
                Ok(())
            }
            TermRequest::Drive { drive } => {
                if drive {
                    self.orphan = None;
                    if let Some(old) = self.driver.replace(client)
                        && old != client
                    {
                        self.send_to(old, &TermEvent::Driver { you: false });
                    }
                    self.send_to(client, &TermEvent::Driver { you: true });
                    if let Some(size) =
                        self.viewers.iter().find(|v| v.client == client).map(|v| v.size)
                    {
                        self.apply_size(size);
                    }
                    self.apply_colors_of(client);
                } else if self.driver == Some(client) {
                    self.driver = None;
                    self.send_to(client, &TermEvent::Driver { you: false });
                }
                Ok(())
            }
            TermRequest::Key(event) => {
                key = Some(event.seq);
                self.engine.encode_key(&event, &mut bytes)
            }
            TermRequest::Mouse(m) => self.engine.encode_mouse(&m, &mut bytes),
            TermRequest::Paste(text) => self.engine.encode_paste(&text, &mut bytes),
            TermRequest::Raw(raw) => {
                bytes = raw;
                Ok(())
            }
            TermRequest::Clear => {
                // The scrollback goes as if the program had printed the erase: through the
                // engine and the tap, so a checkpoint replay ends up in the same place. The
                // screen is the shell's: ⌃L repaints its prompt at the top.
                const ERASE_SCROLLBACK: &[u8] = b"\x1b[3J";
                self.engine.write(ERASE_SCROLLBACK);
                self.tap_output(ERASE_SCROLLBACK);
                self.after_output();
                bytes = vec![0x0c];
                Ok(())
            }
            TermRequest::Focus { focused } => self.engine.encode_focus(focused, &mut bytes),
            TermRequest::FetchLines { start, count } => {
                match self.engine.lines(start, count.min(MAX_FETCH_LINES)) {
                    Ok((start, lines)) => {
                        self.send_to(client, &TermEvent::Lines { start, lines });
                    }
                    Err(e) => self.send_to(client, &TermEvent::Error(e.to_string())),
                }
                Ok(())
            }
            TermRequest::Search { needle, max, regex } => {
                match self.engine.search(&needle, regex, max.min(MAX_SEARCH_MATCHES)) {
                    Ok(found) => self.send_to(
                        client,
                        &TermEvent::Matches { needle, total: found.total, matches: found.matches },
                    ),
                    Err(slopty_engine::EngineError::Pattern(message)) => {
                        self.send_to(client, &TermEvent::SearchInvalid { needle, message });
                    }
                    Err(e) => self.send_to(client, &TermEvent::Error(e.to_string())),
                }
                Ok(())
            }
        };
        let queued = result.map_err(|e| e.to_string()).and_then(|()| {
            self.queue_input(&bytes, Origin::Viewer { key }).map_err(|full| full.to_string())
        });
        if let Err(e) = queued {
            self.send_to(client, &TermEvent::Error(e));
        }
    }
}

async fn sleep_until_due(due: Option<tokio::time::Instant>) {
    match due {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending::<()>().await,
    }
}

/// The pixels of an image the viewers do not hold yet, sent ahead of the frame placing it.
fn image_event(u: ImageUpload) -> TermEvent {
    TermEvent::Image {
        id: u.id,
        generation: u.generation,
        width: u.width,
        height: u.height,
        rgba: u.rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_after_a_quiet_spell_is_framed_at_once() {
        let now = tokio::time::Instant::now();
        assert_eq!(frame_due_after(now, None), now);
        let long_ago = now.checked_sub(Duration::from_secs(1)).unwrap();
        assert_eq!(frame_due_after(now, Some(long_ago)), now);
        let at_boundary = now.checked_sub(MIN_FRAME_INTERVAL).unwrap();
        assert_eq!(frame_due_after(now, Some(at_boundary)), now);
    }

    /// Input buys at most [`ECHO_FRAMES`] frames ahead of the pace, and only while its output
    /// could still be the echo: a flood beside the typing stays paced.
    #[test]
    fn input_buys_a_few_unpaced_frames_and_only_soon_after() {
        let now = tokio::time::Instant::now();
        let mut burst = EchoBurst::default();
        assert!(!burst.spend(now), "no input, no burst");
        burst.arm(now);
        let soon = now + Duration::from_millis(3);
        assert!(burst.open(soon));
        assert!(burst.spend(soon) && burst.spend(soon), "the echo and its repaint");
        assert!(burst.open(soon), "spent, but still the echo's window");
        assert!(!burst.open(now + ECHO_WINDOW + Duration::from_millis(1)));
        assert!(!burst.spend(soon), "then the pace again");
        burst.arm(now);
        assert!(
            !burst.spend(now + ECHO_WINDOW + Duration::from_millis(1)),
            "too late to be the echo"
        );
    }

    /// A frame's datagram copy must not overtake what the frame depends on: the pixels it
    /// places, the colours and size it paints with, the lines it shows.
    #[test]
    fn the_events_a_frame_depends_on_hold_its_copy_back() {
        let ahead = |ev: TermEvent| Outbound::encode(&ev).unwrap().goes_ahead_of_frames();
        let image =
            TermEvent::Image { id: 1, generation: 1, width: 1, height: 1, rgba: vec![0; 4] };
        assert!(ahead(image));
        assert!(ahead(TermEvent::Colors(ColorOverrides::default())));
        assert!(ahead(TermEvent::Resized { cols: 80, rows: 24 }));
        assert!(ahead(TermEvent::Lines { start: slopty_grid::LineIndex(0), lines: Vec::new() }));
        assert!(!ahead(TermEvent::Marker { id: 1 }));
        assert!(!ahead(TermEvent::Title("t".to_owned())));
        assert!(!ahead(TermEvent::Bell));
    }

    /// A viewer that never answers a marker is sent frames as its connection writes them and
    /// keeps a bounded list of markers; one that answers is held to the unconfirmed budget,
    /// opened again by its answers, and a late answer from a replaced stream changes nothing.
    #[test]
    fn a_viewer_is_held_to_the_frames_it_confirmed_once_it_answers() {
        let mut reach = Reach::default();
        let mut next = 0_u64;
        let mut send = |reach: &mut Reach, bytes: usize| {
            if reach.frame(bytes) {
                reach.marker(next);
                next += 1;
            }
        };
        for _ in 0..200 {
            send(&mut reach, 8_000);
        }
        assert!(reach.open(), "never answered: never held");
        assert_eq!(reach.markers.len(), MARKERS_KEPT);
        let newest = reach.markers.back().map(|&(id, _)| id).unwrap();
        reach.answered(newest);
        assert!(reach.markers.is_empty() && reach.open());
        while reach.open() {
            send(&mut reach, 8_000);
        }
        assert!(reach.sent - reach.reached.unwrap() >= FRAMES_UNREACHED_BYTES);
        reach.answered(newest);
        assert!(!reach.open(), "an answer it was already given opens nothing");
        let oldest = reach.markers.front().map(|&(id, _)| id).unwrap();
        reach.answered(oldest);
        assert!(reach.open(), "the first marker after the answer opens it");
    }

    #[test]
    fn a_flood_is_paced_to_the_minimum_interval() {
        let now = tokio::time::Instant::now();
        let just_sent = now.checked_sub(Duration::from_millis(1)).unwrap();
        assert_eq!(frame_due_after(now, Some(just_sent)), just_sent + MIN_FRAME_INTERVAL);
        let almost = now.checked_sub(Duration::from_millis(7)).unwrap();
        assert_eq!(frame_due_after(now, Some(almost)), now + Duration::from_millis(1));
    }
}
