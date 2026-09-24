//! Each verb once: resolve its handles, send it, and take apart the one answer it expects. The
//! CLI and `slopty mcp` both call these, so a verb means the same on either surface.

use std::collections::HashMap;

use anyhow::Result;
use slopty_core::WorkerId;
use slopty_proto::agent::{AgentKind, AgentStatus};
use slopty_proto::orchestration::{
    Command, Input, Line, Outcome, Port, Screen, TermRef, Verb, WaitUntil, Waited,
};
use slopty_proto::server::{Liveness, WorkerInfo};
use slopty_proto::terminal::SessionSummary;
use tokio::task::JoinSet;

use crate::link::Link;
use crate::resolve::{Resolver, unexpected};
use crate::view::Overview;

/// The directory, every terminal, and the status of each agent on a worker that is online.
/// The two lists go out together, then one status request per terminal, all in flight at once.
pub async fn overview(link: &Link) -> Result<Overview> {
    let (workers, terminals) =
        tokio::join!(link.call(Verb::ListWorkers), link.call(Verb::ListTerminals { worker: None }));
    let workers = match workers? {
        Outcome::Workers(list) => list,
        other => return Err(unexpected(other)),
    };
    let terminals = match terminals? {
        Outcome::Terminals(list) => list,
        other => return Err(unexpected(other)),
    };
    let online = |w: &WorkerId| {
        workers.iter().any(|info| info.worker == *w && info.liveness == Liveness::Online)
    };
    let mut asks = JoinSet::new();
    for (worker, s) in terminals.iter().filter(|(w, _)| online(w)) {
        let term = TermRef { worker: *worker, session: s.id };
        let link = link.clone();
        asks.spawn(async move { (term, link.call(Verb::AgentStatus { term }).await) });
    }
    let mut agents = HashMap::new();
    while let Some(asked) = asks.join_next().await {
        match asked? {
            (term, Ok(Outcome::Agent(Some(agent)))) => {
                agents.insert(term, agent);
            }
            (_, Ok(Outcome::Agent(None))) => {}
            (term, Ok(other)) => tracing::debug!(?term, ?other, "agent status"),
            (term, Err(e)) => tracing::debug!(?term, error = %e, "agent status"),
        }
    }
    Ok(Overview { workers, terminals, agents })
}

/// Terminals on one worker or all, with the directory to name their workers.
pub async fn terminals(
    res: &mut Resolver<'_>,
    worker: Option<&str>,
) -> Result<(Vec<WorkerInfo>, Vec<(WorkerId, SessionSummary)>)> {
    let worker = match worker {
        Some(w) => Some(res.worker(Some(w)).await?),
        None => None,
    };
    let link = res.link();
    let (workers, terminals) =
        tokio::join!(res.workers(), link.call(Verb::ListTerminals { worker }));
    let terminals = match terminals? {
        Outcome::Terminals(list) => list,
        other => return Err(unexpected(other)),
    };
    Ok((workers?.to_vec(), terminals))
}

async fn opened(link: &Link, verb: Verb) -> Result<TermRef> {
    match link.call(verb).await? {
        Outcome::Opened(term) => Ok(term),
        other => Err(unexpected(other)),
    }
}

async fn done(link: &Link, verb: Verb) -> Result<()> {
    match link.call(verb).await? {
        Outcome::Done => Ok(()),
        other => Err(unexpected(other)),
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
pub async fn open(res: &mut Resolver<'_>, worker: Option<&str>, spec: Spec) -> Result<TermRef> {
    let worker = res.worker(worker).await?;
    let Spec { cwd, command, env, name } = spec;
    opened(res.link(), Verb::OpenTerminal { worker, cwd, command, env, name }).await
}

/// Start Claude Code in a new terminal.
pub async fn spawn_agent(
    res: &mut Resolver<'_>,
    worker: Option<&str>,
    cwd: String,
    prompt: Option<String>,
) -> Result<TermRef> {
    let worker = res.worker(worker).await?;
    let verb = Verb::SpawnAgent { worker, agent: AgentKind::ClaudeCode, cwd, prompt };
    opened(res.link(), verb).await
}

/// Type into a terminal.
pub async fn send(res: &mut Resolver<'_>, term: &str, input: Input) -> Result<()> {
    let term = res.term(term).await?;
    done(res.link(), Verb::SendInput { term, input }).await
}

/// The screen now.
pub async fn screen(res: &mut Resolver<'_>, term: &str) -> Result<Screen> {
    let term = res.term(term).await?;
    match res.link().call(Verb::ReadScreen { term }).await? {
        Outcome::Screen(screen) => Ok(screen),
        other => Err(unexpected(other)),
    }
}

/// Lines from `since` on, and the index to ask from next.
pub async fn output(
    res: &mut Resolver<'_>,
    term: &str,
    since: Option<u64>,
    max_lines: u32,
) -> Result<(Vec<Line>, u64)> {
    let term = res.term(term).await?;
    match res.link().call(Verb::ReadOutput { term, since, max_lines }).await? {
        Outcome::Output { lines, next } => Ok((lines, next)),
        other => Err(unexpected(other)),
    }
}

/// Command blocks.
pub async fn commands(
    res: &mut Resolver<'_>,
    term: &str,
    since: Option<u64>,
) -> Result<Vec<Command>> {
    let term = res.term(term).await?;
    match res.link().call(Verb::ListCommands { term, since }).await? {
        Outcome::Commands(list) => Ok(list),
        other => Err(unexpected(other)),
    }
}

/// Resolve a terminal ahead of a wait, so the wait itself is one request.
pub async fn term(res: &mut Resolver<'_>, term: &str) -> Result<TermRef> {
    res.term(term).await
}

/// Wait for a condition on a resolved terminal.
pub async fn wait(link: &Link, term: TermRef, until: WaitUntil, timeout_ms: u32) -> Result<Waited> {
    match link.call(Verb::WaitFor { term, until, timeout_ms }).await? {
        Outcome::Waited(waited) => Ok(waited),
        other => Err(unexpected(other)),
    }
}

/// The agent in a terminal and its status.
pub async fn agent_status(
    res: &mut Resolver<'_>,
    term: &str,
) -> Result<Option<(AgentKind, AgentStatus)>> {
    let term = res.term(term).await?;
    match res.link().call(Verb::AgentStatus { term }).await? {
        Outcome::Agent(agent) => Ok(agent),
        other => Err(unexpected(other)),
    }
}

/// Close a terminal.
pub async fn close(res: &mut Resolver<'_>, term: &str) -> Result<()> {
    let term = res.term(term).await?;
    done(res.link(), Verb::Close { term }).await
}

/// A file's bytes.
pub async fn read_file(
    res: &mut Resolver<'_>,
    worker: Option<&str>,
    path: String,
) -> Result<Vec<u8>> {
    let worker = res.worker(worker).await?;
    match res.link().call(Verb::ReadFile { worker, path }).await? {
        Outcome::File(bytes) => Ok(bytes),
        other => Err(unexpected(other)),
    }
}

/// Replace a file.
pub async fn write_file(
    res: &mut Resolver<'_>,
    worker: Option<&str>,
    path: String,
    bytes: Vec<u8>,
) -> Result<()> {
    let worker = res.worker(worker).await?;
    done(res.link(), Verb::WriteFile { worker, path, bytes }).await
}

/// Listening ports, and the worker they are on.
pub async fn ports(res: &mut Resolver<'_>, worker: Option<&str>) -> Result<(WorkerId, Vec<Port>)> {
    let worker = res.worker(worker).await?;
    match res.link().call(Verb::ListPorts { worker }).await? {
        Outcome::Ports(list) => Ok((worker, list)),
        other => Err(unexpected(other)),
    }
}
