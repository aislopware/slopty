//! Each verb once: resolve its handles, send it, and take apart the one answer it expects. The
//! CLI and both MCP surfaces call these, so a verb means the same on every one of them.

use slopty_core::WorkerId;
use slopty_proto::agent::{AgentKind, AgentStatus};
use slopty_proto::orchestration::{
    Command, DirEntry, EventFilter, FileStat, HubEvent, Input, Line, Outcome, Port, Screen, Size,
    TermRef, Verb, WaitUntil, Waited,
};
use slopty_proto::server::WorkerInfo;
use slopty_proto::terminal::SessionSummary;

use crate::resolve::Resolver;
use crate::view::Overview;
use crate::{Dispatch, ToolError};

/// How long a wait waits when the caller names no timeout. The server caps what it is given.
pub const DEFAULT_WAIT_MS: u32 = 60_000;
/// How many lines a read of the output returns when the caller names no limit.
pub const DEFAULT_MAX_LINES: u32 = 200;
/// How many entries a directory listing returns when the caller names no limit.
pub const DEFAULT_MAX_ENTRIES: u32 = 1_000;

/// The directory and every terminal, each with its agent as the server last heard it: the
/// two lists go out together.
pub async fn overview<D: Dispatch>(dispatch: &D) -> Result<Overview, ToolError> {
    let (workers, terminals) = tokio::join!(
        dispatch.call(Verb::ListWorkers),
        dispatch.call(Verb::ListTerminals { worker: None })
    );
    let workers = match workers {
        Outcome::Workers(list) => list,
        other => return Err(ToolError::unexpected(other)),
    };
    let terminals = match terminals {
        Outcome::Terminals(list) => list,
        other => return Err(ToolError::unexpected(other)),
    };
    Ok(Overview { workers, terminals })
}

/// Terminals on one worker or all, with the directory to name their workers.
pub async fn terminals<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
) -> Result<(Vec<WorkerInfo>, Vec<(WorkerId, SessionSummary)>), ToolError> {
    let worker = res.some_worker(worker).await?;
    let dispatch = res.dispatch();
    let (workers, terminals) =
        tokio::join!(res.workers(), dispatch.call(Verb::ListTerminals { worker }));
    let terminals = match terminals {
        Outcome::Terminals(list) => list,
        other => return Err(ToolError::unexpected(other)),
    };
    Ok((workers?.to_vec(), terminals))
}

async fn opened<D: Dispatch>(dispatch: &D, verb: Verb) -> Result<TermRef, ToolError> {
    match dispatch.call(verb).await {
        Outcome::Opened(term) => Ok(term),
        other => Err(ToolError::unexpected(other)),
    }
}

async fn done<D: Dispatch>(dispatch: &D, verb: Verb) -> Result<(), ToolError> {
    match dispatch.call(verb).await {
        Outcome::Done => Ok(()),
        other => Err(ToolError::unexpected(other)),
    }
}

/// What to start in a new terminal.
#[derive(Debug, Default)]
pub struct Spec {
    /// Working directory.
    pub cwd: Option<String>,
    /// Program and arguments; the login shell when empty.
    pub command: Vec<String>,
    /// Extra environment.
    pub env: Vec<(String, String)>,
    /// A name for its tile.
    pub name: Option<String>,
    /// Its grid until a client shows it.
    pub size: Option<Size>,
}

/// Start a terminal.
pub async fn open<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    spec: Spec,
) -> Result<TermRef, ToolError> {
    let worker = res.worker(worker).await?;
    let Spec { cwd, command, env, name, size } = spec;
    opened(res.dispatch(), Verb::OpenTerminal { worker, cwd, command, env, name, size }).await
}

/// How to start an agent.
#[derive(Debug, Default)]
pub struct AgentSpec {
    /// Working directory, usually a repository.
    pub cwd: String,
    /// The first prompt, typed once the agent is ready.
    pub prompt: Option<String>,
    /// Arguments after the agent's program.
    pub args: Vec<String>,
    /// Extra environment.
    pub env: Vec<(String, String)>,
    /// Its grid until a client shows it.
    pub size: Option<Size>,
}

/// Start Claude Code in a new terminal.
pub async fn spawn_agent<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    spec: AgentSpec,
) -> Result<TermRef, ToolError> {
    let worker = res.worker(worker).await?;
    let AgentSpec { cwd, prompt, args, env, size } = spec;
    let agent = AgentKind::ClaudeCode;
    let verb = Verb::SpawnAgent { worker, agent, cwd, prompt, args, env, size };
    opened(res.dispatch(), verb).await
}

/// Resize a terminal no client shows.
pub async fn resize<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    term: &str,
    size: Size,
) -> Result<(), ToolError> {
    let term = res.term(term).await?;
    done(res.dispatch(), Verb::ResizeTerminal { term, size }).await
}

/// Type into a terminal.
pub async fn send<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    term: &str,
    input: Input,
) -> Result<(), ToolError> {
    let term = res.term(term).await?;
    done(res.dispatch(), Verb::SendInput { term, input }).await
}

/// The screen now.
pub async fn screen<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    term: &str,
) -> Result<Screen, ToolError> {
    let term = res.term(term).await?;
    match res.dispatch().call(Verb::ReadScreen { term }).await {
        Outcome::Screen(screen) => Ok(screen),
        other => Err(ToolError::unexpected(other)),
    }
}

/// Lines from `since` on, and the index to ask from next.
pub async fn output<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    term: &str,
    since: Option<u64>,
    max_lines: u32,
) -> Result<(Vec<Line>, u64), ToolError> {
    let term = res.term(term).await?;
    match res.dispatch().call(Verb::ReadOutput { term, since, max_lines }).await {
        Outcome::Output { lines, next } => Ok((lines, next)),
        other => Err(ToolError::unexpected(other)),
    }
}

/// Command blocks.
pub async fn commands<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    term: &str,
    since: Option<u64>,
) -> Result<Vec<Command>, ToolError> {
    let term = res.term(term).await?;
    match res.dispatch().call(Verb::ListCommands { term, since }).await {
        Outcome::Commands(list) => Ok(list),
        other => Err(ToolError::unexpected(other)),
    }
}

/// Wait for a condition on a resolved terminal; resolve first, so the wait is one request.
pub async fn wait<D: Dispatch>(
    dispatch: &D,
    term: TermRef,
    until: WaitUntil,
    timeout_ms: u32,
) -> Result<Waited, ToolError> {
    match dispatch.call(Verb::WaitFor { term, until, timeout_ms }).await {
        Outcome::Waited(waited) => Ok(waited),
        other => Err(ToolError::unexpected(other)),
    }
}

/// The agent in a terminal and its status.
pub async fn agent_status<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    term: &str,
) -> Result<Option<(AgentKind, AgentStatus)>, ToolError> {
    let term = res.term(term).await?;
    match res.dispatch().call(Verb::AgentStatus { term }).await {
        Outcome::Agent(agent) => Ok(agent),
        other => Err(ToolError::unexpected(other)),
    }
}

/// Close a terminal.
pub async fn close<D: Dispatch>(res: &mut Resolver<'_, D>, term: &str) -> Result<(), ToolError> {
    let term = res.term(term).await?;
    done(res.dispatch(), Verb::Close { term }).await
}

/// Bytes of a file from `offset` on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Chunk {
    /// What was read.
    pub bytes: Vec<u8>,
    /// Where they start in the file.
    pub offset: u64,
    /// The whole file's size.
    pub size: u64,
}

/// A file's bytes from `offset` on, `length` of them or the rest; the worker caps one read.
pub async fn read_file<D: Dispatch>(
    dispatch: &D,
    worker: WorkerId,
    path: String,
    offset: u64,
    length: Option<u64>,
) -> Result<Chunk, ToolError> {
    match dispatch.call(Verb::ReadFile { worker, path, offset, length }).await {
        Outcome::File { bytes, offset, size } => Ok(Chunk { bytes, offset, size }),
        other => Err(ToolError::unexpected(other)),
    }
}

/// A directory's first `max` entries by name, and how many it holds.
pub async fn list_dir<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    path: String,
    max: u32,
) -> Result<(Vec<DirEntry>, u32), ToolError> {
    let worker = res.worker(worker).await?;
    match res.dispatch().call(Verb::ListDir { worker, path, max }).await {
        Outcome::Dir { entries, total } => Ok((entries, total)),
        other => Err(ToolError::unexpected(other)),
    }
}

/// What is at a path, if anything.
pub async fn stat<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    path: String,
) -> Result<Option<FileStat>, ToolError> {
    let worker = res.worker(worker).await?;
    match res.dispatch().call(Verb::Stat { worker, path }).await {
        Outcome::Stat(stat) => Ok(stat),
        other => Err(ToolError::unexpected(other)),
    }
}

/// A page of the server's events.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EventPage {
    /// Oldest first.
    pub events: Vec<HubEvent>,
    /// The cursor to ask from next.
    pub next: u64,
    /// Events after the cursor the server no longer holds.
    pub missed: u64,
}

/// The server's events from `since` (from now when absent) that pass `filter`, waiting up to
/// `timeout_ms` for a first one.
pub async fn events<D: Dispatch>(
    dispatch: &D,
    since: Option<u64>,
    timeout_ms: u32,
    filter: EventFilter,
) -> Result<EventPage, ToolError> {
    match dispatch.call(Verb::Events { since, timeout_ms, filter }).await {
        Outcome::Events { events, next, missed } => Ok(EventPage { events, next, missed }),
        other => Err(ToolError::unexpected(other)),
    }
}

/// Remove a worker that is not online from the server's registry; answers the id it removed.
pub async fn forget_worker<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: &str,
) -> Result<WorkerId, ToolError> {
    let worker = res.worker(Some(worker)).await?;
    done(res.dispatch(), Verb::ForgetWorker { worker }).await?;
    Ok(worker)
}

/// Replace a file.
pub async fn write_file<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    path: String,
    bytes: Vec<u8>,
) -> Result<(), ToolError> {
    let worker = res.worker(worker).await?;
    done(res.dispatch(), Verb::WriteFile { worker, path, bytes }).await
}

/// Listening ports, and the worker they are on.
pub async fn ports<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
) -> Result<(WorkerId, Vec<Port>), ToolError> {
    let worker = res.worker(worker).await?;
    match res.dispatch().call(Verb::ListPorts { worker }).await {
        Outcome::Ports(list) => Ok((worker, list)),
        other => Err(ToolError::unexpected(other)),
    }
}
