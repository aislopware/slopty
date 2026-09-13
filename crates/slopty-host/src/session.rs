//! The session actor.

use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use slopty_core::{ClientId, SessionId};
use slopty_engine::boundary::Boundary;
use slopty_engine::{EngineConfig, EngineEvent, GhosttyEngine, VtEngine as _};
use slopty_proto::screen::MAX_CLIPBOARD_BYTES;
use slopty_proto::terminal::{ColorOverrides, TermColors, TermEvent, TermRequest, TermSize};
use slopty_pty::PtyMaster;
use tokio::sync::{mpsc, oneshot};

use crate::HostError;

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
/// PTY read buffer.
const READ_BUF: usize = 64 << 10;
/// Search hits sent back at most; the count still covers every hit.
const MAX_SEARCH_MATCHES: u32 = 5_000;

/// Where a client's events go. Bounded: a client that cannot keep up gets dropped rather than
/// stalling the session (it can reattach and receive a full frame).
pub type ClientSink = mpsc::Sender<TermEvent>;

/// Commands into the actor.
enum Cmd {
    Attach { client: ClientId, size: TermSize, sink: ClientSink },
    Detach { client: ClientId, sink: Option<ClientSink> },
    Reserve { client: ClientId },
    Request { client: ClientId, req: TermRequest, at: tokio::time::Instant },
    Snapshot { reply: oneshot::Sender<Snapshot> },
    Probe { reply: oneshot::Sender<Probe> },
    Close,
}

/// What `Host` reports about a session.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Snapshot {
    /// Title from OSC 0/2, if any.
    pub title: Option<String>,
    /// Cwd from OSC 7, if any.
    pub cwd: Option<String>,
    /// The repository that cwd is in, resolved when it last changed.
    pub repo: Option<String>,
    /// Size.
    pub size: TermSize,
    /// Attached clients.
    pub viewers: u16,
    /// Exit status if the child is gone.
    pub exited: Option<i32>,
}

/// What the host can see of a session without asking anything running in it: the program in
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

/// Cloneable handle to a running session actor.
#[derive(Clone, Debug)]
pub struct SessionHandle {
    id: SessionId,
    tx: mpsc::UnboundedSender<Cmd>,
}

impl std::fmt::Debug for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Attach { .. } => "Attach",
            Self::Detach { .. } => "Detach",
            Self::Reserve { .. } => "Reserve",
            Self::Request { .. } => "Request",
            Self::Snapshot { .. } => "Snapshot",
            Self::Probe { .. } => "Probe",
            Self::Close => "Close",
        };
        f.write_str(name)
    }
}

impl SessionHandle {
    /// Session id.
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
    ) -> Result<(), HostError> {
        self.send(Cmd::Attach { client, size, sink })
    }

    /// Detach a client, whatever sink it is attached through.
    pub fn detach(&self, client: ClientId) -> Result<(), HostError> {
        self.send(Cmd::Detach { client, sink: None })
    }

    /// Detach a client only if it is still attached through `sink`.
    ///
    /// A connection that dies after the same client reconnected (a relaunched app whose old
    /// QUIC connection idles out) must not evict the new connection's viewer.
    pub fn detach_sink(&self, client: ClientId, sink: &ClientSink) -> Result<(), HostError> {
        self.send(Cmd::Detach { client, sink: Some(sink.clone()) })
    }

    /// Make `client` the driver before anyone attaches: the client that opened a session
    /// sizes it, even if another client's attach (it sees the session first via the
    /// broadcast) lands earlier.
    pub fn reserve_driver(&self, client: ClientId) -> Result<(), HostError> {
        self.send(Cmd::Reserve { client })
    }

    /// Forward a terminal request from a client.
    pub fn request(&self, client: ClientId, req: TermRequest) -> Result<(), HostError> {
        self.send(Cmd::Request { client, req, at: tokio::time::Instant::now() })
    }

    /// Current state.
    pub async fn snapshot(&self) -> Result<Snapshot, HostError> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Snapshot { reply })?;
        rx.await.map_err(|_gone| HostError::SessionClosed)
    }

    /// What can be seen of the session from outside it ([`Probe`]).
    pub async fn probe(&self) -> Result<Probe, HostError> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Probe { reply })?;
        rx.await.map_err(|_gone| HostError::SessionClosed)
    }

    /// Stop the actor (the PTY master closes; ptyd decides the child's fate).
    pub fn close(&self) {
        let _ignored = self.tx.send(Cmd::Close);
    }

    fn send(&self, cmd: Cmd) -> Result<(), HostError> {
        self.tx.send(cmd).map_err(|_gone| HostError::SessionClosed)
    }
}

/// What the actor hands ptyd so the session outlives this host: a copy of every byte read from
/// the master, and now and then the engine's whole state (see `Actor::checkpoint`).
#[derive(Debug)]
pub enum Tap {
    /// Output just read from the master.
    Output {
        /// Session.
        id: SessionId,
        /// The bytes.
        bytes: Vec<u8>,
    },
    /// The terminal state, replacing everything tapped before it.
    Checkpoint {
        /// Session.
        id: SessionId,
        /// VT bytes from [`GhosttyEngine::checkpoint`].
        state: Vec<u8>,
    },
}

/// What the actor needs to start.
#[derive(Debug)]
pub struct SessionStart {
    /// Id.
    pub id: SessionId,
    /// The PTY master from ptyd.
    pub master: OwnedFd,
    /// The last host's terminal state (empty if none), replayed before `backlog`.
    pub checkpoint: Vec<u8>,
    /// Output produced since then (replayed through the engine, never sent raw).
    pub backlog: Vec<u8>,
    /// Where copies of output and checkpoints go (a task feeding ptyd).
    pub tap: mpsc::Sender<Tap>,
    /// Size of record.
    pub size: TermSize,
    /// Scrollback lines to retain.
    pub scrollback_lines: u32,
}

/// Spawn the actor thread.
pub fn spawn(start: SessionStart) -> Result<SessionHandle, HostError> {
    let (tx, rx) = mpsc::unbounded_channel();
    let id = start.id;
    let handle = SessionHandle { id, tx };
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
                match Actor::new(start, rx) {
                    Ok(actor) => actor.run().await,
                    Err(e) => tracing::error!(session = %id, error = %e, "session start failed"),
                }
            });
        })
        .map_err(|e| {
            HostError::Pty(slopty_pty::PtyError::Os { context: "spawn session thread", source: e })
        })?;
    Ok(handle)
}

struct Viewer {
    client: ClientId,
    sink: ClientSink,
    size: TermSize,
    /// The colours this client paints with, once it said (the driver's reach the engine).
    colors: Option<TermColors>,
}

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
    echo: EchoTrace,
    viewers: Vec<Viewer>,
    driver: Option<ClientId>,
    title: Option<String>,
    cwd: Option<String>,
    /// The program's colour changes (OSC 4/10/11/12): broadcast, and sent to a late attach.
    program_colors: ColorOverrides,
    /// [`crate::repo::root_of`] of `cwd`, resolved once per change rather than per summary.
    repo: Option<String>,
    exited: Option<i32>,
    /// Highest key seq written to the PTY.
    written_seq: u64,
    /// `written_seq` as of the most recent PTY read: what the next frame may acknowledge.
    ack_seq: u64,
    frame_due: Option<tokio::time::Instant>,
    /// When the last frame left, for [`MIN_FRAME_INTERVAL`].
    last_frame: Option<tokio::time::Instant>,
    /// Copies of output and checkpoints, for ptyd.
    tap: mpsc::Sender<Tap>,
    /// Output has arrived since the last checkpoint.
    dirty_since_checkpoint: bool,
    /// Bytes tapped since the last checkpoint (a checkpoint empties ptyd's ring).
    tapped_since_checkpoint: usize,
    /// A tap did not fit the channel: ptyd's ring has a hole until the next checkpoint.
    tap_lost: bool,
    /// When the next checkpoint is due (`CHECKPOINT_AFTER` past the last output).
    checkpoint_due: Option<tokio::time::Instant>,
    /// Where the output stands in VT syntax, so a quiet-spell checkpoint never cuts a sequence.
    boundary: Boundary,
}

/// A checkpoint follows this much quiet after output. Shorter means a crashed host loses less
/// of what a fresh one cannot replay from the ring; longer means fewer formatter runs.
const CHECKPOINT_AFTER: Duration = Duration::from_millis(500);
/// A checkpoint is also taken once this many bytes were tapped since the last one, so ptyd's
/// ring (4 MiB by default) never overflows under a flood and the replay stays bounded.
const CHECKPOINT_EVERY_BYTES: usize = 1 << 20;
/// Larger states are not sent: the ptyd frame codec caps a frame, and a state this size means
/// a history far past any configured scrollback.
const CHECKPOINT_MAX_BYTES: usize = 12 << 20;

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
    fn new(start: SessionStart, rx: mpsc::UnboundedReceiver<Cmd>) -> Result<Self, HostError> {
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
        // What the replay said is state, not news: the last host delivered the bells, the
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
        Ok(Self {
            id: start.id,
            engine,
            master,
            rx,
            viewers: Vec::new(),
            driver: None,
            title,
            cwd,
            program_colors,
            repo,
            exited: None,
            written_seq: 0,
            ack_seq: 0,
            frame_due: None,
            last_frame: None,
            echo: EchoTrace::default(),
            tap: start.tap,
            // Whatever we replayed is in ptyd's ring or checkpoint already; the first checkpoint
            // of this host folds it and anything the replay answered into one state.
            dirty_since_checkpoint: true,
            tapped_since_checkpoint: 0,
            tap_lost: false,
            checkpoint_due: None,
            boundary: Boundary::default(),
        })
    }

    async fn run(mut self) {
        let mut buf = vec![0_u8; READ_BUF];
        let reader = Arc::clone(&self.master);
        // The replay answered nothing yet: take its title and cwd, then checkpoint at once.
        // ptyd handed us its ring with the master, so until this lands another restart would
        // have nothing but the previous checkpoint.
        self.after_output().await;
        self.checkpoint(true);
        loop {
            let frame_timer = async {
                match self.frame_due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            };
            let checkpoint_timer = async {
                match self.checkpoint_due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                biased;
                cmd = self.rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    if !self.handle(cmd).await {
                        break;
                    }
                }
                read = reader.read(&mut buf), if self.exited.is_none() => match read {
                    Ok(0) => {
                        tracing::info!(session = %self.id, "pty closed");
                        self.flush_frame();
                        self.exited = Some(self.exited.unwrap_or(0));
                        self.broadcast(&TermEvent::Exited { status: 0 });
                    }
                    Ok(n) => {
                        self.ack_seq = self.written_seq;
                        if let Some(input_at) = self.echo.input_at
                            && self.echo.read_at.is_none()
                        {
                            let now = tokio::time::Instant::now();
                            self.echo.read_at = Some(now);
                            tracing::trace!(
                                session = %self.id,
                                echo_us = now.saturating_duration_since(input_at).as_micros(),
                                bytes = n,
                                "echo read"
                            );
                        }
                        let bytes = buf.get(..n).unwrap_or_default();
                        let shown = String::from_utf8_lossy(bytes);
                        tracing::trace!(session = %self.id, n, bytes = %shown.escape_debug(), "pty read");
                        self.engine.write(bytes);
                        self.tap_output(bytes);
                        self.after_output().await;
                        // Frame right here when nothing paces it. A timer set to "now" is not
                        // now: tokio rounds a deadline up to its next millisecond tick and the
                        // driver parks until then, which put 1.4 ms between a keystroke's echo
                        // and its frame (MEASUREMENTS.md, "the keystroke path, stage by stage").
                        // The timer is for the flood, where the next frame is owed later.
                        let now = tokio::time::Instant::now();
                        let due = frame_due_after(now, self.last_frame);
                        if due <= now {
                            self.frame_due = None;
                            self.flush_frame();
                        } else if self.frame_due.is_none() {
                            self.frame_due = Some(due);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(session = %self.id, error = %e, "pty read failed");
                        self.exited = Some(-1);
                        self.broadcast(&TermEvent::Exited { status: -1 });
                    }
                },
                () = frame_timer => {
                    self.frame_due = None;
                    self.flush_frame();
                }
                () = checkpoint_timer => {
                    self.checkpoint_due = None;
                    self.checkpoint(false);
                }
            }
        }
        tracing::debug!(session = %self.id, "actor stopped");
    }

    /// Copy output to ptyd's ring and schedule the checkpoint that will fold it away.
    fn tap_output(&mut self, bytes: &[u8]) {
        self.dirty_since_checkpoint = true;
        self.boundary.feed(bytes);
        self.tapped_since_checkpoint = self.tapped_since_checkpoint.saturating_add(bytes.len());
        if let Err(e) = self.tap.try_send(Tap::Output { id: self.id, bytes: bytes.to_vec() }) {
            // Full or gone: the ring has a hole. The next checkpoint replaces the ring, so
            // pull it forward rather than leaving a replay that would misparse mid-sequence.
            if !self.tap_lost {
                tracing::warn!(session = %self.id, error = %e, "output tap dropped; checkpointing early");
            }
            self.tap_lost = true;
        }
        if self.tap_lost || self.tapped_since_checkpoint >= CHECKPOINT_EVERY_BYTES {
            // Right here rather than through the timer: the select prefers the master, and a
            // flood that keeps it readable would starve a timer indefinitely.
            self.checkpoint(true);
        } else {
            let now = tokio::time::Instant::now();
            self.checkpoint_due = Some(now.checked_add(CHECKPOINT_AFTER).unwrap_or(now));
        }
    }

    /// Hand ptyd the engine's whole state, so a host that replaces this one starts from it.
    /// Unless `force`d, a checkpoint waits while the output stands inside an escape sequence
    /// or a character: it replaces the bytes before it, and the rest of that sequence would
    /// print as text after a restart.
    fn checkpoint(&mut self, force: bool) {
        if !self.dirty_since_checkpoint {
            return;
        }
        if !force && !self.boundary.is_ground() {
            let now = tokio::time::Instant::now();
            self.checkpoint_due = Some(now.checked_add(CHECKPOINT_AFTER).unwrap_or(now));
            return;
        }
        let mut state = Vec::new();
        if let Some(t) = &self.title {
            // Not part of what the formatter emits; the next host learns it the way this one did.
            state.extend_from_slice(b"\x1b]0;");
            state.extend_from_slice(t.as_bytes());
            state.extend_from_slice(b"\x1b\\");
        }
        match self.engine.checkpoint() {
            Ok(bytes) => state.extend_from_slice(&bytes),
            Err(e) => {
                tracing::warn!(session = %self.id, error = %e, "checkpoint failed");
                return;
            }
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
                let now = tokio::time::Instant::now();
                self.checkpoint_due = Some(now.checked_add(CHECKPOINT_AFTER).unwrap_or(now));
            }
        }
    }

    /// Side effects of the bytes just consumed.
    async fn after_output(&mut self) {
        for ev in self.engine.drain_events() {
            match ev {
                EngineEvent::PtyWrite(bytes) => {
                    if let Err(e) = self.master.write_all(&bytes).await {
                        tracing::warn!(session = %self.id, error = %e, "query response write failed");
                    }
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
                    if text.len() > MAX_CLIPBOARD_BYTES {
                        tracing::warn!(session = %self.id, bytes = text.len(), "OSC 52 write too large; dropped");
                    } else {
                        self.broadcast(&TermEvent::ClipboardWrite { text });
                    }
                }
            }
        }
    }

    fn flush_frame(&mut self) {
        if self.viewers.is_empty() {
            // Still consume dirty state so the next attach gets a clean full frame.
            let _consumed = self.engine.take_frame(self.ack_seq);
            let _unwatched = self.engine.drain_images();
            return;
        }
        match self.engine.take_frame(self.ack_seq) {
            Ok(Some(frame)) => {
                let now = tokio::time::Instant::now();
                self.last_frame = Some(now);
                for image in self.images() {
                    self.broadcast(&image);
                }
                self.broadcast(&TermEvent::Frame(frame));
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
            Ok(None) => {}
            Err(e) => {
                tracing::error!(session = %self.id, error = %e, "frame build failed");
                self.broadcast(&TermEvent::Error(e.to_string()));
            }
        }
    }

    fn broadcast(&mut self, ev: &TermEvent) {
        let mut dead = Vec::new();
        for (i, v) in self.viewers.iter().enumerate() {
            match v.sink.try_send(ev.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!(session = %self.id, client = %v.client, "client cannot keep up; detaching");
                    dead.push(i);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => dead.push(i),
            }
        }
        for i in dead.into_iter().rev() {
            let v = self.viewers.swap_remove(i);
            self.on_viewer_gone(v.client);
        }
    }

    fn send_to(&self, client: ClientId, ev: TermEvent) {
        if let Some(v) = self.viewers.iter().find(|v| v.client == client) {
            let _ignored = v.sink.try_send(ev);
        }
    }

    fn on_viewer_gone(&mut self, client: ClientId) {
        if self.driver == Some(client) {
            self.driver = None;
            // Hand the size to the first remaining viewer, so a phone that joined after the
            // laptop left still gets a fitting terminal.
            if let Some(next) = self.viewers.first().map(|v| (v.client, v.size)) {
                self.driver = Some(next.0);
                self.send_to(next.0, TermEvent::Driver { you: true });
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
        self.broadcast(&TermEvent::Resized { cols: size.cols, rows: size.rows });
        self.send_full_frame_to_all();
    }

    fn send_full_frame_to_all(&mut self) {
        match self.engine.full_frame(self.ack_seq) {
            Ok(frame) => {
                for image in self.images() {
                    self.broadcast(&image);
                }
                self.broadcast(&TermEvent::Frame(frame));
            }
            Err(e) => tracing::error!(session = %self.id, error = %e, "full frame failed"),
        }
    }

    /// The pixels the frame just taken places and the clients do not hold: sent ahead of it.
    fn images(&mut self) -> Vec<TermEvent> {
        self.engine
            .drain_images()
            .into_iter()
            .map(|u| TermEvent::Image {
                id: u.id,
                generation: u.generation,
                width: u.width,
                height: u.height,
                rgba: u.rgba,
            })
            .collect()
    }

    /// Returns `false` when the actor should stop.
    async fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Attach { client, size, sink } => {
                // A re-attach (resync, reconnect) keeps the colours the client already said.
                let colors =
                    self.viewers.iter().find(|v| v.client == client).and_then(|v| v.colors);
                self.viewers.retain(|v| v.client != client);
                self.viewers.push(Viewer { client, sink, size, colors });
                if self.driver.is_none() {
                    self.driver = Some(client);
                    self.send_to(client, TermEvent::Driver { you: true });
                    self.apply_size(size);
                    self.apply_colors_of(client);
                } else if self.driver == Some(client) {
                    // A reconnecting driver (relaunched app) learns it still drives.
                    self.send_to(client, TermEvent::Driver { you: true });
                    self.apply_size(size);
                    self.apply_colors_of(client);
                }
                if let Some(t) = &self.title {
                    self.send_to(client, TermEvent::Title(t.clone()));
                }
                if let Some(c) = &self.cwd {
                    self.send_to(
                        client,
                        TermEvent::Cwd { path: c.clone(), repo: self.repo.clone() },
                    );
                }
                if self.program_colors != ColorOverrides::default() {
                    self.send_to(client, TermEvent::Colors(self.program_colors.clone()));
                }
                match self.engine.full_frame(self.ack_seq) {
                    Ok(frame) => {
                        for image in self.images() {
                            self.send_to(client, image);
                        }
                        self.send_to(client, TermEvent::Frame(frame));
                    }
                    Err(e) => self.send_to(client, TermEvent::Error(e.to_string())),
                }
                if let Some(status) = self.exited {
                    self.send_to(client, TermEvent::Exited { status });
                }
            }
            Cmd::Detach { client, sink } => {
                let before = self.viewers.len();
                self.viewers.retain(|v| {
                    v.client != client || sink.as_ref().is_some_and(|s| !s.same_channel(&v.sink))
                });
                if self.viewers.len() != before {
                    self.on_viewer_gone(client);
                }
            }
            Cmd::Reserve { client } => {
                if let Some(old) = self.driver.replace(client)
                    && old != client
                {
                    self.send_to(old, TermEvent::Driver { you: false });
                }
            }
            Cmd::Request { client, req, at } => self.request(client, req, at).await,
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
            Cmd::Probe { reply } => {
                // A child that already exited has no foreground process; asking would only
                // read whatever the kernel put in its place.
                let foreground = self
                    .exited
                    .is_none()
                    .then(|| slopty_pty::process::foreground(self.master.as_fd()))
                    .flatten();
                let _ignored = reply.send(Probe {
                    foreground,
                    title: self.title.clone(),
                    cwd: self.cwd.clone(),
                });
            }
            Cmd::Close => return false,
        }
        true
    }

    async fn request(&mut self, client: ClientId, req: TermRequest, at: tokio::time::Instant) {
        let queued_us = at.elapsed().as_micros();
        let mut bytes = Vec::new();
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
                    if let Some(old) = self.driver.replace(client)
                        && old != client
                    {
                        self.send_to(old, TermEvent::Driver { you: false });
                    }
                    self.send_to(client, TermEvent::Driver { you: true });
                    if let Some(size) =
                        self.viewers.iter().find(|v| v.client == client).map(|v| v.size)
                    {
                        self.apply_size(size);
                    }
                    self.apply_colors_of(client);
                } else if self.driver == Some(client) {
                    self.driver = None;
                    self.send_to(client, TermEvent::Driver { you: false });
                }
                Ok(())
            }
            TermRequest::Key(key) => {
                let seq = key.seq;
                let r = self.engine.encode_key(&key, &mut bytes);
                if r.is_ok() && !bytes.is_empty() {
                    self.written_seq = self.written_seq.max(seq);
                }
                r
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
                self.after_output().await;
                bytes = vec![0x0c];
                Ok(())
            }
            TermRequest::Focus { focused } => self.engine.encode_focus(focused, &mut bytes),
            TermRequest::FetchLines { start, count } => {
                match self.engine.lines(start, count.min(4096)) {
                    Ok((start, lines)) => self.send_to(client, TermEvent::Lines { start, lines }),
                    Err(e) => self.send_to(client, TermEvent::Error(e.to_string())),
                }
                Ok(())
            }
            TermRequest::Search { needle, max, regex } => {
                match self.engine.search(&needle, regex, max.min(MAX_SEARCH_MATCHES)) {
                    Ok(found) => self.send_to(
                        client,
                        TermEvent::Matches { needle, total: found.total, matches: found.matches },
                    ),
                    Err(slopty_engine::EngineError::Pattern(message)) => {
                        self.send_to(client, TermEvent::SearchInvalid { needle, message });
                    }
                    Err(e) => self.send_to(client, TermEvent::Error(e.to_string())),
                }
                Ok(())
            }
        };
        if let Err(e) = result {
            self.send_to(client, TermEvent::Error(e.to_string()));
            return;
        }
        if bytes.is_empty() {
            return;
        }
        let write_from = tokio::time::Instant::now();
        if let Err(e) = self.master.write_all(&bytes).await {
            tracing::warn!(session = %self.id, error = %e, "pty write failed");
            self.send_to(client, TermEvent::Error(e.to_string()));
            return;
        }
        let now = tokio::time::Instant::now();
        // The stamp the echo trace measures from; a keystroke that lands while the previous
        // one is still in flight restarts the trace, which is what a bench that waits for each
        // frame never does.
        self.echo = EchoTrace { input_at: Some(now), read_at: None };
        tracing::trace!(
            session = %self.id,
            queued_us,
            write_us = now.saturating_duration_since(write_from).as_micros(),
            bytes = bytes.len(),
            "pty input written"
        );
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

    #[test]
    fn a_flood_is_paced_to_the_minimum_interval() {
        let now = tokio::time::Instant::now();
        let just_sent = now.checked_sub(Duration::from_millis(1)).unwrap();
        assert_eq!(frame_due_after(now, Some(just_sent)), just_sent + MIN_FRAME_INTERVAL);
        let almost = now.checked_sub(Duration::from_millis(7)).unwrap();
        assert_eq!(frame_due_after(now, Some(almost)), now + Duration::from_millis(1));
    }
}
