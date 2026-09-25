//! The orchestration verbs, as a worker answers them (`docs/decisions/topology.md`).
//!
//! The server forwards every [`Verb`] naming this worker down the worker's link, and
//! [`Orchestrator::serve`] answers it. Terminals opened here go through the same path a
//! client's `OpenSession` takes and are announced on the same broadcast, so every attached
//! client sees them like any other. Reads come from the session's engine on its own thread
//! ([`SessionHandle::read`]); waits sleep on the session's [`Activity`](crate::session::Activity)
//! and the agent events, never on a timer that asks again.
//!
//! The per-session functions ([`send_input`], [`read_screen`], [`read_output`],
//! [`list_commands`], [`wait_for`]) take a [`SessionHandle`] alone, so they work on a session
//! actor without ptyd behind it.

pub mod keys;
mod wait;

use std::collections::BinaryHeap;
use std::ffi::OsString;
use std::io::{Read as _, Seek as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use slopty_core::{ClientId, SessionId, WorkerId};
use slopty_engine::ghostty::Position;
use slopty_proto::WorkerMsg;
use slopty_proto::agent::{AgentKind, AgentSource, AgentStatus, BlockReason, SessionAgent};
use slopty_proto::orchestration::{
    Command, DirEntry, ErrorCode, FileKind, FileStat, Input, Line, Outcome, Screen, Size, TermRef,
    Verb, WaitUntil,
};
use slopty_proto::terminal::{CloseReason, OpenSession, SessionSummary, TermRequest, TermSize};
use tokio::sync::broadcast;
pub use wait::{AgentFeed, wait_for};

use crate::session::{Read, SessionHandle, Text};
use crate::{ItemStore, Worker, WorkerError};

/// Who orchestration acts as, for the session actor (its errors go nowhere: the verb's own
/// outcome reports them) and for the item deltas it causes (nobody's echo).
const ORCHESTRATOR: ClientId = ClientId::nil();

/// Lines one `ReadOutput` returns at most, whatever it asks: a reply is one control-stream
/// frame, and a caller pages on with `next`.
pub const MAX_OUTPUT_LINES: u32 = 10_000;

/// Most bytes one `ReadFile` returns. A reply is one frame on the server's control stream,
/// and this leaves half of it for the envelope; a caller pages through a larger file with
/// `offset`.
pub const MAX_FILE_BYTES: u64 = 8 << 20;
const _: () = assert!(
    MAX_FILE_BYTES.saturating_mul(2) <= slopty_proto::codec::MAX_FRAME_BYTES as u64,
    "a whole read and its envelope fit in one frame"
);

/// Most entries one `ListDir` returns: names of up to 255 bytes each keep the reply a few
/// megabytes, well inside a frame.
pub const MAX_DIR_ENTRIES: u32 = 10_000;

/// The grid a verb may ask for, inclusive: as small as a status line, as large as a wall of
/// displays in a small font.
const MIN_SIZE: Size = Size { cols: 10, rows: 2 };
const MAX_SIZE: Size = Size { cols: 1000, rows: 500 };

/// How long a spawned agent's first prompt waits for the agent to say it is at its prompt
/// before it is typed anyway.
const PROMPT_FALLBACK: Duration = Duration::from_secs(20);

/// Between a pasted prompt and the Enter that submits it. Ink-based TUIs (Claude Code) read
/// a paste and an Enter that arrive in one read as one paste, and the Enter is swallowed.
const SUBMIT_PAUSE: Duration = Duration::from_millis(200);

/// Size of a terminal opened by a verb, until a client attaches and drives it.
const ORCHESTRATED_SIZE: TermSize = TermSize {
    cols: 120,
    rows: 36,
    metrics: slopty_proto::input::CellMetrics { cell_width: 8, cell_height: 16 },
};

/// Coding-agent status as the daemon tracks it (`slopty_agent::AgentTable` behind a lock).
pub trait Agents: Send + Sync {
    /// The agent in `session` now, if one runs.
    fn status(&self, session: SessionId) -> Option<SessionAgent>;
    /// `session` is gone; drop what was known about it.
    fn forget(&self, session: SessionId);
}

/// A verb that failed: the code and the words for whoever asked.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Failure {
    /// Why, as a code.
    pub code: ErrorCode,
    /// Why, in words.
    pub message: String,
}

impl Failure {
    /// A failure.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    fn outcome(self) -> Outcome {
        Outcome::Error { code: self.code, message: self.message }
    }
}

impl From<WorkerError> for Failure {
    fn from(e: WorkerError) -> Self {
        let code = match e {
            WorkerError::NoSuchSession | WorkerError::SessionClosed => ErrorCode::UnknownTerminal,
            _ => ErrorCode::Failed,
        };
        Self::new(code, e.to_string())
    }
}

/// Answers the verbs the server forwards to this worker. Cheap to clone.
#[derive(Clone)]
pub struct Orchestrator {
    inner: Arc<Inner>,
}

struct Inner {
    id: WorkerId,
    worker: Worker,
    items: ItemStore,
    events: broadcast::Sender<WorkerMsg>,
}

impl std::fmt::Debug for Orchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Orchestrator").field("worker", &self.inner.id).finish_non_exhaustive()
    }
}

impl Orchestrator {
    /// An orchestrator for worker `id`, acting on its sessions, agent table, item registry and
    /// client broadcast.
    #[must_use]
    pub fn new(
        id: WorkerId,
        worker: Worker,
        items: ItemStore,
        events: broadcast::Sender<WorkerMsg>,
    ) -> Self {
        Self { inner: Arc::new(Inner { id, worker, items, events }) }
    }

    /// Answer one verb. Every failure is an [`Outcome::Error`].
    pub async fn serve(&self, verb: Verb) -> Outcome {
        self.dispatch(verb).await.unwrap_or_else(Failure::outcome)
    }

    async fn dispatch(&self, verb: Verb) -> Result<Outcome, Failure> {
        let inner = &self.inner;
        match verb {
            Verb::ListWorkers
            | Verb::ListTerminals { .. }
            | Verb::Events { .. }
            | Verb::ForgetWorker { .. } => {
                Err(Failure::new(ErrorCode::Invalid, "the server answers this, not a worker"))
            }
            Verb::OpenTerminal { worker, cwd, command, env, name, size } => {
                self.mine(worker)?;
                let req = OpenSession {
                    size: term_size(size)?,
                    cwd,
                    command,
                    env,
                    title: name,
                    attach: false,
                };
                let handle = self.open(&req, ORCHESTRATOR).await?;
                Ok(Outcome::Opened(TermRef { worker, session: handle.id() }))
            }
            Verb::SpawnAgent { worker, agent, cwd, prompt, args, env, size } => {
                self.mine(worker)?;
                let spawn = Spawn { cwd, args, env, size: term_size(size)? };
                self.spawn_agent(agent, spawn, prompt).await
            }
            Verb::SendInput { term, input } => {
                send_input(&self.session(term)?, &input).await?;
                Ok(Outcome::Done)
            }
            Verb::ReadScreen { term } => {
                Ok(Outcome::Screen(read_screen(&self.session(term)?).await?))
            }
            Verb::ReadOutput { term, since, max_lines } => {
                let (lines, next) = read_output(&self.session(term)?, since, max_lines).await?;
                Ok(Outcome::Output { lines, next })
            }
            Verb::ListCommands { term, since } => {
                Ok(Outcome::Commands(list_commands(&self.session(term)?, since).await?))
            }
            Verb::WaitFor { term, until, timeout_ms } => {
                let handle = self.session(term)?;
                let feed = matches!(until, WaitUntil::AgentNeedsInput).then(|| AgentFeed {
                    events: inner.events.subscribe(),
                    now: inner.worker.agents().status(term.session).map(|a| a.status),
                });
                let timeout = Duration::from_millis(u64::from(timeout_ms));
                Ok(Outcome::Waited(wait_for(&handle, &until, timeout, feed).await?))
            }
            Verb::AgentStatus { term } => {
                self.session(term)?;
                Ok(Outcome::Agent(inner.worker.agents().status(term.session)))
            }
            Verb::Close { term } => {
                self.mine(term.worker)?;
                self.close(term.session).await?;
                Ok(Outcome::Done)
            }
            Verb::ReadFile { worker, path, offset, length } => {
                self.mine(worker)?;
                let path = crate::file::expand_home(Path::new(&path));
                let (bytes, size) = blocking(move || read_file(&path, offset, length)).await?;
                Ok(Outcome::File { bytes, offset, size })
            }
            Verb::WriteFile { worker, path, bytes } => {
                self.mine(worker)?;
                let path = crate::file::expand_home(Path::new(&path));
                blocking(move || write_file(&path, &bytes)).await.map(|()| Outcome::Done)
            }
            Verb::ListPorts { worker } => {
                self.mine(worker)?;
                let roots = inner.worker.pids().await?;
                let ports = blocking(move || Ok(crate::ports::listening(&roots))).await?;
                Ok(Outcome::Ports(ports))
            }
            Verb::ResizeTerminal { term, size } => {
                let size = term_size(Some(size))?;
                resize(&self.session(term)?, size).await?;
                Ok(Outcome::Done)
            }
            Verb::ListDir { worker, path, max } => {
                self.mine(worker)?;
                let path = crate::file::expand_home(Path::new(&path));
                let (entries, total) = blocking(move || list_dir(&path, max)).await?;
                Ok(Outcome::Dir { entries, total })
            }
            Verb::Stat { worker, path } => {
                self.mine(worker)?;
                let path = crate::file::expand_home(Path::new(&path));
                blocking(move || stat(&path)).await.map(Outcome::Stat)
            }
        }
    }

    /// The session's summary as a client's list shows it, if the session runs.
    pub async fn summary(&self, session: SessionId) -> Option<SessionSummary> {
        self.inner.worker.summaries().await.into_iter().find(|s| s.id == session)
    }

    /// Open a session and announce it the way a client's open is announced: the summary to
    /// every client, and a terminal item made `by` the given client.
    ///
    /// # Errors
    ///
    /// ptyd refusing the spawn, or the session failing to start.
    pub async fn open(
        &self,
        req: &OpenSession,
        by: ClientId,
    ) -> Result<SessionHandle, WorkerError> {
        let inner = &self.inner;
        let handle = inner.worker.open(req).await?;
        // A fresh terminal: output matching starts at its first byte, so a program's banner
        // printed before the first wait still counts.
        handle.mark_if_unset(Position { line: 0, col: 0, epoch: 0 });
        let session = handle.id();
        let summaries = inner.worker.summaries().await;
        if let Some(summary) = summaries.into_iter().find(|s| s.id == session) {
            let _sent = inner.events.send(WorkerMsg::SessionOpened(summary));
        }
        if let Some(delta) = inner.items.ensure_terminal(session, by) {
            let _sent = inner.events.send(WorkerMsg::Items(delta));
        }
        Ok(handle)
    }

    /// Close a session and announce it the way a client's close is announced.
    async fn close(&self, session: SessionId) -> Result<(), WorkerError> {
        let inner = &self.inner;
        inner.worker.close(session).await?;
        inner.worker.agents().forget(session);
        let reason = CloseReason::Requested;
        let _sent = inner.events.send(WorkerMsg::SessionClosed { session, reason });
        for delta in inner.items.remove_session(session, ORCHESTRATOR) {
            let _sent = inner.events.send(WorkerMsg::Items(delta));
        }
        Ok(())
    }

    /// Start the agent's TUI; with a prompt, type it once the agent is at its prompt.
    async fn spawn_agent(
        &self,
        agent: AgentKind,
        spawn: Spawn,
        prompt: Option<String>,
    ) -> Result<Outcome, Failure> {
        let program = match agent {
            AgentKind::ClaudeCode => "claude",
        };
        // Subscribed before the spawn: the agent may report itself ready before the open
        // returns.
        let events = self.inner.events.subscribe();
        let Spawn { cwd, args, env, size } = spawn;
        let req = OpenSession {
            size,
            cwd: Some(cwd),
            command: std::iter::once(program.to_owned()).chain(args).collect(),
            env,
            title: None,
            attach: false,
        };
        let handle = self.open(&req, ORCHESTRATOR).await?;
        let session = handle.id();
        if let Some(prompt) = prompt {
            let now = self.inner.worker.agents().status(session).map(|a| a.status);
            tokio::spawn(type_when_ready(handle, prompt, AgentFeed { events, now }));
        }
        Ok(Outcome::Opened(TermRef { worker: self.inner.id, session }))
    }

    /// `worker` is this one.
    fn mine(&self, worker: WorkerId) -> Result<(), Failure> {
        if worker == self.inner.id {
            Ok(())
        } else {
            Err(Failure::new(
                ErrorCode::UnknownWorker,
                format!("this is worker {}, not {worker}", self.inner.id),
            ))
        }
    }

    /// The live session `term` names, on this worker.
    fn session(&self, term: TermRef) -> Result<SessionHandle, Failure> {
        self.mine(term.worker)?;
        self.inner.worker.get(term.session).map_err(|_gone| {
            Failure::new(
                ErrorCode::UnknownTerminal,
                format!("no terminal {} on this worker", term.session),
            )
        })
    }
}

/// Where and how an agent starts.
struct Spawn {
    cwd: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    size: TermSize,
}

/// The session size a verb asks for, [`ORCHESTRATED_SIZE`] when it names none.
fn term_size(size: Option<Size>) -> Result<TermSize, Failure> {
    let Some(size) = size else { return Ok(ORCHESTRATED_SIZE) };
    let fits = (MIN_SIZE.cols..=MAX_SIZE.cols).contains(&size.cols)
        && (MIN_SIZE.rows..=MAX_SIZE.rows).contains(&size.rows);
    if !fits {
        return Err(Failure::new(
            ErrorCode::Invalid,
            format!(
                "{}x{} is not a terminal size; cols {}..={}, rows {}..={}",
                size.cols, size.rows, MIN_SIZE.cols, MAX_SIZE.cols, MIN_SIZE.rows, MAX_SIZE.rows
            ),
        ));
    }
    Ok(TermSize { cols: size.cols, rows: size.rows, ..ORCHESTRATED_SIZE })
}

/// Resize a session no client shows.
///
/// A session's size is its driver's, and a client showing it drives it to fit its window, so a
/// session with viewers is refused rather than fought over. The check and the resize are one
/// step on the session's actor, so a client attaching meanwhile keeps its seat.
///
/// # Errors
///
/// [`ErrorCode::Failed`] while a client shows the session; [`ErrorCode::UnknownTerminal`] when
/// it is gone.
pub async fn resize(handle: &SessionHandle, size: TermSize) -> Result<(), Failure> {
    let viewers = handle.resize_unviewed(size).await?;
    if viewers > 0 {
        return Err(Failure::new(
            ErrorCode::Failed,
            format!(
                "{viewers} client(s) show this terminal and size it to their window; only a \
                 terminal no client shows can be resized"
            ),
        ));
    }
    Ok(())
}

/// Type a spawned agent's first prompt once it says it is at its prompt, from a signal that
/// only comes once its TUI is drawn (the title, the transcript, a hook), or after
/// [`PROMPT_FALLBACK`] without one.
async fn type_when_ready(handle: SessionHandle, prompt: String, mut feed: AgentFeed) {
    let session = handle.id();
    let ready = |status: &AgentStatus, source: AgentSource| {
        source != AgentSource::Process
            && matches!(status, AgentStatus::Idle | AgentStatus::Blocked(BlockReason::IdlePrompt))
    };
    let deadline = tokio::time::Instant::now().checked_add(PROMPT_FALLBACK);
    let wait = async {
        loop {
            match feed.events.recv().await {
                Ok(WorkerMsg::Agent(ev))
                    if ev.session == session && ready(&ev.status, ev.source) =>
                {
                    return;
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    };
    let waited = match deadline {
        Some(at) => tokio::time::timeout_at(at, wait).await.is_ok(),
        None => false,
    };
    tracing::info!(%session, ready = waited, "typing the agent's first prompt");
    if let Err(e) = handle.request(ORCHESTRATOR, TermRequest::Paste(prompt)) {
        tracing::warn!(%session, error = %e, "first prompt not typed");
        return;
    }
    tokio::time::sleep(SUBMIT_PAUSE).await;
    if let Err(e) = send_input(&handle, &Input::Keys(vec!["enter".to_owned()])).await {
        tracing::warn!(%session, error = %e.message, "first prompt not submitted");
    }
}

/// Type into a session the way a client's input path does.
///
/// Text goes as the committed text a keyboard's input method sends (raw bytes) with each newline an
/// Enter press through the key encoder; a paste through the paste encoder (bracketed when the
/// program asked); named keys through the key encoder, all of them checked before any is sent. The
/// session's mark is set where the cursor stands before the input, if orchestration never set it,
/// so a wait for the input's output finds it however soon it came.
///
/// # Errors
///
/// [`ErrorCode::Invalid`] for a key name that does not parse; [`ErrorCode::UnknownTerminal`]
/// when the session is gone.
pub async fn send_input(handle: &SessionHandle, input: &Input) -> Result<(), Failure> {
    let requests: Vec<TermRequest> = match input {
        Input::Text(text) => text_requests(text),
        Input::Paste(text) => vec![TermRequest::Paste(text.clone())],
        Input::Keys(names) => names
            .iter()
            .map(|name| keys::parse(name, 0).map(TermRequest::Key))
            .collect::<Result<_, _>>()
            .map_err(|message| Failure::new(ErrorCode::Invalid, message))?,
    };
    if handle.mark().is_none() {
        handle.mark_if_unset(position(handle).await?);
    }
    for req in requests {
        handle.request(ORCHESTRATOR, req)?;
    }
    Ok(())
}

/// Where the session's cursor is.
async fn position(handle: &SessionHandle) -> Result<Position, Failure> {
    match handle.read(Read::Position).await? {
        Text::Position(at) => Ok(at),
        _other => Err(unexpected()),
    }
}

/// Text as raw runs and Enter presses: `\n`, `\r` and `\r\n` are each one Enter.
fn text_requests(text: &str) -> Vec<TermRequest> {
    let enter = || keys::parse("enter", 0).map(TermRequest::Key).ok();
    let mut out = Vec::new();
    let mut run = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' || c == '\n' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            if !run.is_empty() {
                out.push(TermRequest::Raw(std::mem::take(&mut run).into_bytes()));
            }
            out.extend(enter());
        } else {
            run.push(c);
        }
    }
    if !run.is_empty() {
        out.push(TermRequest::Raw(run.into_bytes()));
    }
    out
}

/// The screen as drawn now.
///
/// # Errors
///
/// [`ErrorCode::UnknownTerminal`] when the session is gone; [`ErrorCode::Failed`] when the
/// engine fails.
pub async fn read_screen(handle: &SessionHandle) -> Result<Screen, Failure> {
    let Text::Screen { screen, title, cwd } = handle.read(Read::Screen).await? else {
        return Err(unexpected());
    };
    let lines =
        (screen.first..).zip(screen.rows).map(|(index, text)| Line { index, text }).collect();
    Ok(Screen {
        lines,
        cursor: screen.cursor,
        title: title.unwrap_or_default(),
        cwd,
        alternate: screen.alternate,
    })
}

/// At most `max_lines` (and [`MAX_OUTPUT_LINES`]) lines from `since` on, and where to ask
/// from next.
///
/// # Errors
///
/// As [`read_screen`].
pub async fn read_output(
    handle: &SessionHandle,
    since: Option<u64>,
    max_lines: u32,
) -> Result<(Vec<Line>, u64), Failure> {
    let read = Read::Output { since, max: max_lines.min(MAX_OUTPUT_LINES) };
    let Text::Output(text) = handle.read(read).await? else { return Err(unexpected()) };
    let next = text.next();
    let lines = (text.first..).zip(text.lines).map(|(index, text)| Line { index, text }).collect();
    Ok((lines, next))
}

/// The OSC 133 command blocks from `since` on.
///
/// # Errors
///
/// As [`read_screen`].
pub async fn list_commands(
    handle: &SessionHandle,
    since: Option<u64>,
) -> Result<Vec<Command>, Failure> {
    let Text::Commands(blocks) = handle.read(Read::Commands { since }).await? else {
        return Err(unexpected());
    };
    Ok(blocks
        .into_iter()
        .map(|b| Command {
            line: b.command,
            prompt_line: b.prompt_line,
            output: b.output,
            exit: b.exit.filter(|_| b.finished).map(i32::from),
        })
        .collect())
}

fn unexpected() -> Failure {
    Failure::new(ErrorCode::Failed, "the session answered a different read")
}

/// Run file work on the blocking pool.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Failure::new(ErrorCode::Failed, e.to_string()))?
}

fn io_failure(path: &Path, e: &std::io::Error) -> Failure {
    Failure::new(ErrorCode::Failed, format!("{}: {e}", path.display()))
}

/// The bytes of a file from `offset` on, `length` of them or the rest, [`MAX_FILE_BYTES`] at
/// most; and the file's size.
fn read_file(path: &Path, offset: u64, length: Option<u64>) -> Result<(Vec<u8>, u64), Failure> {
    // Non-blocking, so opening a named pipe answers at once instead of waiting for a writer;
    // what was opened is then looked at, not the path, which could change in between.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| io_failure(path, &e))?;
    let meta = file.metadata().map_err(|e| io_failure(path, &e))?;
    if meta.is_dir() {
        return Err(Failure::new(ErrorCode::Failed, format!("{} is a directory", path.display())));
    }
    if !meta.is_file() {
        return Err(Failure::new(
            ErrorCode::Failed,
            format!("{} is not a regular file", path.display()),
        ));
    }
    let size = meta.len();
    let want = if let Some(length) = length {
        length.min(MAX_FILE_BYTES)
    } else {
        let rest = size.saturating_sub(offset);
        if rest > MAX_FILE_BYTES {
            return Err(Failure::new(
                ErrorCode::Failed,
                format!(
                    "{} is {size} bytes, and one read returns at most {MAX_FILE_BYTES} (8 MiB); \
                     read it in parts with offset and length",
                    path.display()
                ),
            ));
        }
        rest
    };
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset)).map_err(|e| io_failure(path, &e))?;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(want.min(size)).unwrap_or(0));
    // Capped here too: the file may have grown since the stat.
    (&mut file).take(want).read_to_end(&mut bytes).map_err(|e| io_failure(path, &e))?;
    Ok((bytes, size))
}

/// The first `max` entries of a directory by name ([`MAX_DIR_ENTRIES`] at most), and how many
/// it holds.
fn list_dir(path: &Path, max: u32) -> Result<(Vec<DirEntry>, u32), Failure> {
    first_entries(path, max, |entry| std::fs::symlink_metadata(entry))
}

/// [`list_dir`], with `look` reading an entry's metadata: only the names are gathered from the
/// whole directory, the first `max` of them kept, and only those looked at.
fn first_entries(
    path: &Path,
    max: u32,
    mut look: impl FnMut(&Path) -> std::io::Result<std::fs::Metadata>,
) -> Result<(Vec<DirEntry>, u32), Failure> {
    let keep = usize::try_from(max.min(MAX_DIR_ENTRIES)).unwrap_or(usize::MAX);
    // The greatest kept name on top, to be pushed out by a smaller one.
    let mut first: BinaryHeap<OsString> = BinaryHeap::with_capacity(keep.saturating_add(1));
    let mut total = 0_u32;
    for entry in std::fs::read_dir(path).map_err(|e| io_failure(path, &e))? {
        let name = entry.map_err(|e| io_failure(path, &e))?.file_name();
        total = total.saturating_add(1);
        if first.len() < keep {
            first.push(name);
        } else if first.peek().is_some_and(|last| name < *last) {
            first.pop();
            first.push(name);
        }
    }
    let mut entries = Vec::with_capacity(first.len());
    for name in first.into_sorted_vec() {
        // Gone between the listing and the look: it is not in the directory any more.
        let Ok(meta) = look(&path.join(&name)) else { continue };
        entries.push(DirEntry {
            name: name.to_string_lossy().into_owned(),
            kind: kind(meta.file_type()),
            size: meta.len(),
            modified_ms: modified_ms(&meta),
        });
    }
    Ok((entries, total))
}

/// What is at `path`, following a symbolic link; `None` when nothing is.
fn stat(path: &Path) -> Result<Option<FileStat>, Failure> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_failure(path, &e)),
    };
    Ok(Some(FileStat {
        kind: kind(meta.file_type()),
        size: meta.len(),
        modified_ms: modified_ms(&meta),
        mode: std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o7777,
    }))
}

fn kind(t: std::fs::FileType) -> FileKind {
    use std::os::unix::fs::FileTypeExt as _;
    if t.is_symlink() {
        FileKind::Symlink
    } else if t.is_dir() {
        FileKind::Dir
    } else if t.is_fifo() || t.is_socket() || t.is_block_device() || t.is_char_device() {
        FileKind::Other
    } else {
        FileKind::File
    }
}

fn modified_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Replace a file whole ([`crate::file::replace`]).
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    crate::file::replace(path, bytes).map(drop).map_err(|e| io_failure(path, &e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_raw_runs_and_enter_presses() {
        let reqs = text_requests("echo hi\nls\r\n\rx");
        let shape: Vec<String> = reqs
            .iter()
            .map(|r| match r {
                TermRequest::Raw(b) => String::from_utf8_lossy(b).into_owned(),
                TermRequest::Key(k) => format!("<{:?}>", k.code),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(shape, ["echo hi", "<Enter>", "ls", "<Enter>", "<Enter>", "x"]);
    }

    fn whole(path: &Path) -> Result<Vec<u8>, Failure> {
        read_file(path, 0, None).map(|(bytes, _size)| bytes)
    }

    #[test]
    fn files_are_read_capped_and_written_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        write_file(&path, b"one").unwrap();
        assert_eq!(whole(&path).unwrap(), b"one");
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        write_file(&path, b"two").unwrap();
        assert_eq!(whole(&path).unwrap(), b"two");
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&path).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o755, "the mode carries over");
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "no temporary file is left behind");

        let big = dir.path().join("big.bin");
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        let err = whole(&big).unwrap_err();
        assert!(err.message.contains("offset and length"), "{err:?}");
        assert_eq!(whole(dir.path()).unwrap_err().code, ErrorCode::Failed);
        let missing = write_file(&dir.path().join("no/such/dir/x"), b"").unwrap_err();
        assert_eq!(missing.code, ErrorCode::Failed);
    }

    /// A range reads from its offset, a length past the end stops at the end, and every read
    /// reports the whole file's size; a file over the cap is read in parts, each at most the
    /// cap.
    #[test]
    fn a_file_is_read_in_ranges_with_its_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("digits");
        std::fs::write(&path, b"0123456789").unwrap();
        assert_eq!(read_file(&path, 3, Some(4)).unwrap(), (b"3456".to_vec(), 10));
        assert_eq!(read_file(&path, 8, None).unwrap(), (b"89".to_vec(), 10));
        assert_eq!(read_file(&path, 8, Some(100)).unwrap(), (b"89".to_vec(), 10));
        assert_eq!(read_file(&path, 20, Some(5)).unwrap(), (Vec::new(), 10), "past the end");

        let big = dir.path().join("big.bin");
        std::fs::File::create(&big).unwrap().set_len(MAX_FILE_BYTES + 3).unwrap();
        let (first, size) = read_file(&big, 0, Some(u64::MAX)).unwrap();
        assert_eq!((first.len() as u64, size), (MAX_FILE_BYTES, MAX_FILE_BYTES + 3));
        let (rest, _size) = read_file(&big, MAX_FILE_BYTES, None).unwrap();
        assert_eq!(rest.len(), 3, "the rest fits");
    }

    /// Entries come by name with their kind, size and time, up to `max`, with the count of all
    /// of them; a missing path is an error, a missing stat is `None`.
    #[test]
    fn a_directory_lists_by_name_and_a_path_stats() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), b"hello").unwrap();
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::os::unix::fs::symlink("b.txt", dir.path().join("c")).unwrap();
        let (entries, total) = list_dir(dir.path(), 10).unwrap();
        let shape: Vec<(&str, FileKind)> =
            entries.iter().map(|e| (e.name.as_str(), e.kind)).collect();
        assert_eq!(
            shape,
            [("a", FileKind::Dir), ("b.txt", FileKind::File), ("c", FileKind::Symlink)]
        );
        assert_eq!(total, 3);
        assert_eq!(entries[1].size, 5);
        assert!(entries[1].modified_ms > 1_700_000_000_000, "{:?}", entries[1]);
        let (first, total) = list_dir(dir.path(), 1).unwrap();
        assert_eq!((first.len(), total), (1, 3), "bounded, with the whole count");
        assert_eq!(list_dir(&dir.path().join("nope"), 10).unwrap_err().code, ErrorCode::Failed);

        let linked = stat(&dir.path().join("c")).unwrap().unwrap();
        assert_eq!((linked.kind, linked.size), (FileKind::File, 5), "a link is followed");
        std::fs::set_permissions(
            dir.path().join("a"),
            std::os::unix::fs::PermissionsExt::from_mode(0o750),
        )
        .unwrap();
        let a = stat(&dir.path().join("a")).unwrap().unwrap();
        assert_eq!((a.kind, a.mode), (FileKind::Dir, 0o750));
        assert_eq!(stat(&dir.path().join("nope")).unwrap(), None);
    }

    /// A named pipe (or any other file that is not a regular one) is refused at once: opening
    /// one for reading waits for a writer, and would hold a blocking thread for good.
    #[test]
    fn a_named_pipe_is_refused_not_waited_on() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        assert!(made.is_ok_and(|s| s.success()), "mkfifo");
        let (done, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || done.send(read_file(&fifo, 0, None)));
        let read = answer.recv_timeout(Duration::from_secs(5)).expect("answered, not blocked");
        let refused = read.unwrap_err();
        assert!(refused.message.contains("not a regular file"), "{refused:?}");
    }

    /// Only the entries answered are looked at: a huge directory costs its names, not a stat
    /// of each, and its count is still whole.
    #[test]
    fn a_directory_is_stat_only_for_the_entries_it_answers() {
        let dir = tempfile::tempdir().unwrap();
        for i in (0..50).rev() {
            std::fs::write(dir.path().join(format!("f{i:02}")), b"").unwrap();
        }
        let looked = std::cell::Cell::new(0);
        let (entries, total) = first_entries(dir.path(), 3, |p| {
            looked.set(looked.get() + 1);
            std::fs::symlink_metadata(p)
        })
        .unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!((names, total), (vec!["f00", "f01", "f02"], 50));
        assert_eq!(looked.get(), 3, "a stat for each entry answered, none for the rest");
    }

    #[test]
    fn a_size_is_checked_and_defaults() {
        assert_eq!(term_size(None).unwrap(), ORCHESTRATED_SIZE);
        let wide = term_size(Some(Size { cols: 200, rows: 50 })).unwrap();
        assert_eq!((wide.cols, wide.rows, wide.metrics), (200, 50, ORCHESTRATED_SIZE.metrics));
        let err = term_size(Some(Size { cols: 0, rows: 50 })).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        term_size(Some(Size { cols: 80, rows: 5000 })).unwrap_err();
    }
}
