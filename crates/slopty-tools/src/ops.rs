//! Each verb once: resolve its handles, send it, and take apart the one answer it expects. The
//! CLI and both MCP surfaces call these, so a verb means the same on every one of them.

use slopty_core::WorkerId;
use slopty_proto::agent::{AgentKind, AgentStatus};
use slopty_proto::orchestration::{
    Command, Input, Line, Outcome, Port, Screen, TermRef, Verb, WaitUntil, Waited,
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
}

/// Start a terminal.
pub async fn open<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    spec: Spec,
) -> Result<TermRef, ToolError> {
    let worker = res.worker(worker).await?;
    let Spec { cwd, command, env, name } = spec;
    opened(res.dispatch(), Verb::OpenTerminal { worker, cwd, command, env, name }).await
}

/// Start Claude Code in a new terminal.
pub async fn spawn_agent<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    cwd: String,
    prompt: Option<String>,
) -> Result<TermRef, ToolError> {
    let worker = res.worker(worker).await?;
    let verb = Verb::SpawnAgent { worker, agent: AgentKind::ClaudeCode, cwd, prompt };
    opened(res.dispatch(), verb).await
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

/// A file's bytes.
pub async fn read_file<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    path: String,
) -> Result<Vec<u8>, ToolError> {
    let worker = res.worker(worker).await?;
    match res.dispatch().call(Verb::ReadFile { worker, path }).await {
        Outcome::File(bytes) => Ok(bytes),
        other => Err(ToolError::unexpected(other)),
    }
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
