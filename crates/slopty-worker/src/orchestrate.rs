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

use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use slopty_core::{ClientId, SessionId, WorkerId};
use slopty_engine::ghostty::Position;
use slopty_proto::WorkerMsg;
use slopty_proto::agent::{AgentKind, AgentSource, AgentStatus, BlockReason};
use slopty_proto::orchestration::{
    Command, ErrorCode, Input, Line, Outcome, Screen, TermRef, Verb, WaitUntil,
};
use slopty_proto::terminal::{CloseReason, OpenSession, TermRequest, TermSize};
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

/// Largest file `ReadFile` returns; a reply is one frame on the server's control stream.
pub const MAX_FILE_BYTES: u64 = 16 << 20;

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
    fn status(&self, session: SessionId) -> Option<(AgentKind, AgentStatus)>;
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
            Verb::ListWorkers | Verb::ListTerminals { .. } => {
                Err(Failure::new(ErrorCode::Invalid, "the server answers this, not a worker"))
            }
            Verb::OpenTerminal { worker, cwd, command, env, name } => {
                self.mine(worker)?;
                let req = OpenSession {
                    size: ORCHESTRATED_SIZE,
                    cwd,
                    command,
                    env,
                    title: name,
                    attach: false,
                };
                let handle = self.open(&req, ORCHESTRATOR).await?;
                Ok(Outcome::Opened(TermRef { worker, session: handle.id() }))
            }
            Verb::SpawnAgent { worker, agent, cwd, prompt } => {
                self.mine(worker)?;
                self.spawn_agent(agent, cwd, prompt).await
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
                    now: inner.worker.agents().status(term.session).map(|(_kind, status)| status),
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
            Verb::ReadFile { worker, path } => {
                self.mine(worker)?;
                let path = crate::file::expand_home(Path::new(&path));
                blocking(move || read_file(&path)).await.map(Outcome::File)
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
        }
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

    /// Start the agent's TUI in `cwd`; with a prompt, type it once the agent is at its prompt.
    async fn spawn_agent(
        &self,
        agent: AgentKind,
        cwd: String,
        prompt: Option<String>,
    ) -> Result<Outcome, Failure> {
        let program = match agent {
            AgentKind::ClaudeCode => "claude",
        };
        // Subscribed before the spawn: the agent may report itself ready before the open
        // returns.
        let events = self.inner.events.subscribe();
        let req = OpenSession {
            size: ORCHESTRATED_SIZE,
            cwd: Some(cwd),
            command: vec![program.to_owned()],
            env: Vec::new(),
            title: None,
            attach: false,
        };
        let handle = self.open(&req, ORCHESTRATOR).await?;
        let session = handle.id();
        if let Some(prompt) = prompt {
            let now = self.inner.worker.agents().status(session).map(|(_kind, status)| status);
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

/// A whole file, [`MAX_FILE_BYTES`] at most.
fn read_file(path: &Path) -> Result<Vec<u8>, Failure> {
    let mut file = std::fs::File::open(path).map_err(|e| io_failure(path, &e))?;
    let meta = file.metadata().map_err(|e| io_failure(path, &e))?;
    if meta.is_dir() {
        return Err(Failure::new(ErrorCode::Failed, format!("{} is a directory", path.display())));
    }
    let too_big = |size: u64| {
        Failure::new(
            ErrorCode::Failed,
            format!(
                "{} is {size} bytes; read_file returns at most {MAX_FILE_BYTES} (16 MiB)",
                path.display()
            ),
        )
    };
    if meta.len() > MAX_FILE_BYTES {
        return Err(too_big(meta.len()));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    // One byte past the cap tells a file that grew since the stat from one that fits.
    (&mut file)
        .take(MAX_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| io_failure(path, &e))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(too_big(bytes.len() as u64));
    }
    Ok(bytes)
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

    #[test]
    fn files_are_read_capped_and_written_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        write_file(&path, b"one").unwrap();
        assert_eq!(read_file(&path).unwrap(), b"one");
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        write_file(&path, b"two").unwrap();
        assert_eq!(read_file(&path).unwrap(), b"two");
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&path).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o755, "the mode carries over");
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "no temporary file is left behind");

        let big = dir.path().join("big.bin");
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        let err = read_file(&big).unwrap_err();
        assert!(err.message.contains("16 MiB"), "{err:?}");
        assert_eq!(read_file(dir.path()).unwrap_err().code, ErrorCode::Failed);
        let missing = write_file(&dir.path().join("no/such/dir/x"), b"").unwrap_err();
        assert_eq!(missing.code, ErrorCode::Failed);
    }
}
