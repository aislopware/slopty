//! Each verb once: resolve its handles, send it, and take apart the one answer it expects. The
//! CLI and both MCP surfaces call these, so a verb means the same on every one of them.
//!
//! A verb that changes something takes the caller's [`IdempotencyKey`], if it gave one.

use slopty_core::WorkerId;
use slopty_proto::agent::{AgentEntry, AgentStart, Worktree};
use slopty_proto::folder::FsOp;
use slopty_proto::orchestration::{
    Command, DirEntry, EventFilter, FileStat, HubEvent, IdempotencyKey, Input, Line, Outcome, Port,
    Screen, Size, TermAgent, TermRef, ThreadOf, ThreadRead, ThreadView, Verb, WaitUntil, Waited,
};
use slopty_proto::screen::{CaptureTarget, DisplayInfo, WindowInfo};
use slopty_proto::search::{FileHits, SearchQuery, SearchSummary};
use slopty_proto::server::WorkerInfo;
use slopty_proto::terminal::SessionSummary;
use slopty_proto::thread::{AgentId, AskId, ThreadId, TurnId};

use crate::resolve::Resolver;
use crate::view::Overview;
use crate::{Dispatch, ToolError};

/// How long a wait waits when the caller names no timeout. The server caps what it is given.
pub const DEFAULT_WAIT_MS: u32 = 60_000;
/// How many lines a read of the output returns when the caller names no limit.
pub const DEFAULT_MAX_LINES: u32 = 200;
/// How many entries a directory listing returns when the caller names no limit.
pub const DEFAULT_MAX_ENTRIES: u32 = 1_000;
/// How many matching lines a search in files returns when the caller names no limit: enough
/// to see what a query is about, few enough for a model to read.
pub const DEFAULT_MAX_MATCHES: u32 = 200;

/// The directory and every terminal, each with its agent as the server last heard it: the
/// two lists go out together.
pub async fn overview<D: Dispatch>(dispatch: &D) -> Result<Overview, ToolError> {
    let (workers, terminals, facts) = tokio::join!(
        dispatch.call(Verb::ListWorkers),
        dispatch.call(Verb::ListTerminals { worker: None }),
        dispatch.call(Verb::WorkerFacts { worker: None })
    );
    let workers = match workers {
        Outcome::Workers(list) => list,
        other => return Err(ToolError::unexpected(other)),
    };
    let (terminals, agents) = match terminals {
        Outcome::Terminals { terminals, agents } => (terminals, agents),
        other => return Err(ToolError::unexpected(other)),
    };
    // A worker reached on its own has no server to know the facts.
    let facts = match facts {
        Outcome::Facts(facts) => facts,
        _ => Vec::new(),
    };
    Ok(Overview { workers, terminals, agents, facts })
}

/// Terminals as [`terminals`] lists them.
#[derive(Debug)]
pub struct Listing {
    /// The directory, to name the terminals' workers.
    pub workers: Vec<WorkerInfo>,
    /// The terminals, by worker.
    pub terminals: Vec<(WorkerId, SessionSummary)>,
    /// The agent at work in each terminal that has one.
    pub agents: Vec<(TermRef, TermAgent)>,
}

/// Terminals on one worker or all, the agent at work in each that has one, and the directory
/// to name their workers.
pub async fn terminals<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
) -> Result<Listing, ToolError> {
    let worker = res.some_worker(worker).await?;
    let dispatch = res.dispatch();
    let (workers, terminals) =
        tokio::join!(res.workers(), dispatch.call(Verb::ListTerminals { worker }));
    let (terminals, agents) = match terminals {
        Outcome::Terminals { terminals, agents } => (terminals, agents),
        other => return Err(ToolError::unexpected(other)),
    };
    Ok(Listing { workers: workers?.to_vec(), terminals, agents })
}

async fn opened<D: Dispatch>(
    dispatch: &D,
    key: Option<IdempotencyKey>,
    verb: Verb,
) -> Result<TermRef, ToolError> {
    match dispatch.send(key, verb).await {
        Outcome::Opened(term) | Outcome::OpenedIn { term, .. } => Ok(term),
        other => Err(ToolError::unexpected(other)),
    }
}

async fn done<D: Dispatch>(
    dispatch: &D,
    key: Option<IdempotencyKey>,
    verb: Verb,
) -> Result<(), ToolError> {
    match dispatch.send(key, verb).await {
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
    key: Option<IdempotencyKey>,
) -> Result<TermRef, ToolError> {
    let worker = res.worker(worker).await?;
    let Spec { cwd, command, env, name, size } = spec;
    let verb =
        Verb::OpenTerminal { worker, cwd, command, env, name, size, session: None, worktree: None };
    opened(res.dispatch(), key, verb).await
}

/// Resize a terminal no client shows.
pub async fn resize<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    term: &str,
    size: Size,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    let term = res.term(term).await?;
    done(res.dispatch(), key, Verb::ResizeTerminal { term, size }).await
}

/// Type into a terminal.
pub async fn send<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    term: &str,
    input: Input,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    let term = res.term(term).await?;
    done(res.dispatch(), key, Verb::SendInput { term, input }).await
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
    key: Option<IdempotencyKey>,
) -> Result<Waited, ToolError> {
    match dispatch.send(key, Verb::WaitFor { term, until, timeout_ms }).await {
        Outcome::Waited(waited) => Ok(waited),
        other => Err(ToolError::unexpected(other)),
    }
}

/// The agent at work in a terminal, as its thread's row says.
pub async fn agent_status<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    term: &str,
) -> Result<Option<TermAgent>, ToolError> {
    let term = res.term(term).await?;
    match res.dispatch().call(Verb::AgentStatus { term }).await {
        Outcome::Agent(agent) => Ok(agent.map(|a| *a)),
        other => Err(ToolError::unexpected(other)),
    }
}

/// Close a terminal.
pub async fn close<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    term: &str,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    let term = res.term(term).await?;
    done(res.dispatch(), key, Verb::Close { term }).await
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

/// The folder `path` would be made in, and its name: `~/a/b` is `b` in `~/a`.
///
/// # Errors
///
/// When `path` names no folder below another, such as `/` or `~`.
pub fn parent_and_name(path: &str) -> Result<(String, String), ToolError> {
    let path = if path.len() > 1 { path.trim_end_matches('/') } else { path };
    match path.rsplit_once('/') {
        Some((parent, name)) if !name.is_empty() => {
            let parent = if parent.is_empty() { "/" } else { parent };
            Ok((parent.to_owned(), name.to_owned()))
        }
        _ => Err(ToolError::invalid(format!(
            "{path} names no folder to make: give its whole path, such as ~/src/new"
        ))),
    }
}

/// Make a folder, move or rename an entry, or trash one ([`FsOp`]), on a worker: where the
/// entry now is. A refusal comes back as the worker said it, plainly.
pub async fn fs_change<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    op: FsOp,
    key: Option<IdempotencyKey>,
) -> Result<String, ToolError> {
    let worker = res.worker(worker).await?;
    match res.dispatch().send(key, Verb::FsChange { worker, op }).await {
        Outcome::FsDone { path } => Ok(path),
        other => Err(ToolError::unexpected(other)),
    }
}

/// The lines matching `query` in the files under `root`, in path order, `max_lines` of them at
/// most, and how the search ended.
pub async fn search<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    root: String,
    query: SearchQuery,
    max_lines: u32,
) -> Result<(Vec<FileHits>, SearchSummary), ToolError> {
    let worker = res.worker(worker).await?;
    match res.dispatch().call(Verb::Search { worker, root, query, max_lines }).await {
        Outcome::Search { files, summary } => Ok((files, summary)),
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
    key: Option<IdempotencyKey>,
) -> Result<WorkerId, ToolError> {
    let worker = res.worker(Some(worker)).await?;
    done(res.dispatch(), key, Verb::ForgetWorker { worker }).await?;
    Ok(worker)
}

/// A magic packet sent to a sleeping worker.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Woken {
    /// The worker it was sent for.
    pub worker: WorkerId,
    /// The machine that sent it: the server, or a worker on the same LAN.
    pub by: String,
    /// The sleeping worker's interfaces it went to.
    pub to: Vec<String>,
    /// The worker said it sleeps through a magic packet, so it may not wake.
    pub wake_on_lan_off: bool,
}

/// Wake a sleeping worker. The worker coming online is the directory's news, not this answer's.
pub async fn wake<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: &str,
) -> Result<Woken, ToolError> {
    let worker = res.worker(Some(worker)).await?;
    let wake_on_lan_off = res
        .workers()
        .await?
        .iter()
        .any(|w| w.worker == worker && w.caps.wake_on_lan == Some(false));
    match res.dispatch().call(Verb::Wake { worker }).await {
        Outcome::WakeSent { by, to } => Ok(Woken { worker, by, to, wake_on_lan_off }),
        other => Err(ToolError::unexpected(other)),
    }
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

/// The windows and displays a worker can stream, and the worker.
pub async fn windows<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
) -> Result<(WorkerId, Vec<WindowInfo>, Vec<DisplayInfo>), ToolError> {
    let worker = res.worker(worker).await?;
    match res.dispatch().call(Verb::ListWindows { worker }).await {
        Outcome::Screens { windows, displays } => Ok((worker, windows, displays)),
        other => Err(ToolError::unexpected(other)),
    }
}

/// Which thread a read or an answer is about, as its caller names it.
///
/// A thread's id (an agent's or a subagent's), or the terminal an agent runs in. A thread
/// wins over a terminal.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadArg<'a> {
    /// A thread's id.
    pub thread: Option<&'a str>,
    /// A terminal, as the lists print it.
    pub term: Option<&'a str>,
}

/// The thread `arg` names, for the server to find.
async fn thread_of<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    arg: ThreadArg<'_>,
) -> Result<ThreadOf, ToolError> {
    if let Some(thread) = arg.thread {
        let id = thread.trim().parse().map_err(|e| {
            ToolError::invalid(format!(
                "{thread:?} is not a thread id ({e}): a uuid, as reads print it"
            ))
        })?;
        return Ok(ThreadOf::Thread(id));
    }
    match arg.term {
        Some(term) => Ok(ThreadOf::Term(res.term(term).await?)),
        None => Err(ToolError::invalid("name a thread or a terminal")),
    }
}

/// What a thread did after turn `after`, whatever its agent: whole turns, bounded.
///
/// The requests open on it come too. Read by the person (the CLI outside an agent's terminal),
/// a Claude Code TUI's prompts wait for [`answer_request`] from then on; read by an agent,
/// nothing changes.
pub async fn read_thread<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    arg: ThreadArg<'_>,
    view: ThreadView,
    after: Option<u32>,
) -> Result<ThreadRead, ToolError> {
    let of = thread_of(res, arg).await?;
    // The server says whether the read holds prompts: the person's alone.
    let verb = Verb::ReadThread { of, view, after: after.map(TurnId), hold: false };
    match res.dispatch().call(verb).await {
        Outcome::Thread(read) => Ok(*read),
        other => Err(ToolError::unexpected(other)),
    }
}

/// Answer a request open on a thread by one of its choices: the person's alone.
pub async fn answer_request<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    arg: ThreadArg<'_>,
    (ask, choice, message): (String, String, Option<String>),
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    let of = thread_of(res, arg).await?;
    let verb = Verb::AnswerRequest { of, ask: AskId(ask), choice, message };
    done(res.dispatch(), key, verb).await
}

/// A still picture, as PNG.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Still {
    /// The PNG file's bytes.
    pub png: Vec<u8>,
    /// Pixels across.
    pub width: u32,
    /// Pixels down.
    pub height: u32,
}

/// One still picture of a window or a display on a worker.
pub async fn capture_still<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    target: CaptureTarget,
) -> Result<Still, ToolError> {
    let worker = res.worker(worker).await?;
    match res.dispatch().call(Verb::CaptureStill { worker, target }).await {
        Outcome::Still { png, width, height } => Ok(Still { png, width, height }),
        other => Err(ToolError::unexpected(other)),
    }
}

/// The agent a name means: `claude` or Claude Code's id, `codex`, `pi`, an ACP agent by the
/// registry's name or `acp:<name>`.
#[must_use]
pub fn agent_id(name: &str) -> AgentId {
    match name.trim() {
        "claude" | AgentId::CLAUDE_CODE => AgentId::named(AgentId::CLAUDE_CODE),
        name @ (AgentId::CODEX | AgentId::PI) => AgentId::named(name),
        name if name.starts_with(AgentId::ACP_PREFIX) => AgentId::named(name),
        name => AgentId::acp(name),
    }
}

/// An agent by its id, as `agents` prints it.
///
/// # Errors
/// It is not one.
pub fn agent_named(given: &str) -> Result<ThreadId, ToolError> {
    given.trim().parse().map_err(|e| {
        ToolError::invalid(format!(
            "{given:?} is not an agent's id ({e}): a uuid, as agents prints it"
        ))
    })
}

/// What [`agent_start`] asks, with the machine by name.
#[derive(Clone, Debug, Default)]
pub struct AgentWanted {
    /// The machine, by name or id.
    pub worker: String,
    /// The folder it works in.
    pub folder: Option<String>,
    /// What it is told first.
    pub prompt: String,
    /// Which agent, by [`agent_id`]'s names.
    pub agent: Option<String>,
    /// The model, by the agent's own id.
    pub model: Option<String>,
    /// How hard it thinks.
    pub effort: Option<String>,
    /// A fresh worktree, by name.
    pub worktree: Option<String>,
    /// A repository to clone first when the machine has none.
    pub repo: Option<String>,
}

/// Start an agent ([`Verb::AgentStart`]): the thread it runs as, and the worktree it works in
/// when it was given one.
///
/// # Errors
/// The machine does not resolve, or the server refused the start.
pub async fn agent_start<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    wanted: AgentWanted,
    key: Option<IdempotencyKey>,
) -> Result<(ThreadId, Option<Worktree>), ToolError> {
    let AgentWanted { worker, folder, prompt, agent, model, effort, worktree, repo } = wanted;
    let worker = res.worker(Some(&worker)).await?;
    let start = AgentStart {
        worker,
        folder,
        prompt,
        agent: agent.as_deref().map(agent_id),
        model,
        effort,
        worktree,
        repo,
    };
    match res.dispatch().send(key, Verb::AgentStart(Box::new(start))).await {
        Outcome::ThreadStarted { thread, worktree } => Ok((thread, worktree.map(|w| *w))),
        other => Err(ToolError::unexpected(other)),
    }
}

/// Words for an agent ([`Verb::AgentTell`]).
///
/// # Errors
/// The server refused them.
pub async fn agent_tell<D: Dispatch>(
    dispatch: &D,
    agent: ThreadId,
    text: String,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    done(dispatch, key, Verb::AgentTell { agent, text }).await
}

/// Wait for agents to need someone or come to rest ([`Verb::AgentWait`]): each as it stands,
/// and whether the wait ran out first.
///
/// # Errors
/// An agent the server does not know, or a failure.
pub async fn agent_wait<D: Dispatch>(
    dispatch: &D,
    agents: Vec<ThreadId>,
    all: bool,
    timeout_ms: u32,
) -> Result<(Vec<AgentEntry>, bool), ToolError> {
    match dispatch.call(Verb::AgentWait { agents, all, timeout_ms }).await {
        Outcome::Agents { agents, timed_out } => Ok((agents, timed_out)),
        other => Err(ToolError::unexpected(other)),
    }
}

/// End an agent and every agent under it ([`Verb::AgentStop`]).
///
/// # Errors
/// The server refused it.
pub async fn agent_stop<D: Dispatch>(
    dispatch: &D,
    agent: ThreadId,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    done(dispatch, key, Verb::AgentStop { agent }).await
}

/// Every agent on the fleet, each with its parent ([`Verb::Agents`]).
///
/// # Errors
/// A failure.
pub async fn agents<D: Dispatch>(dispatch: &D) -> Result<Vec<AgentEntry>, ToolError> {
    match dispatch.call(Verb::Agents).await {
        Outcome::Agents { agents, .. } => Ok(agents),
        other => Err(ToolError::unexpected(other)),
    }
}

/// Bring a branch from a clone on one machine into a clone on another
/// ([`Verb::BranchBring`]): the branch it landed as, and its commit.
///
/// # Errors
/// A machine does not resolve, or the server could not carry it.
pub async fn branch_bring<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    (from, repo, branch): (&str, String, String),
    (to, into, name): (&str, Option<String>, Option<String>),
    key: Option<IdempotencyKey>,
) -> Result<(String, String), ToolError> {
    let from = res.worker(Some(from)).await?;
    let to = res.worker(Some(to)).await?;
    let verb = Verb::BranchBring { from, repo, branch, to, into, name };
    match res.dispatch().send(key, verb).await {
        Outcome::Fetched { branch, head } => Ok((branch, head)),
        other => Err(ToolError::unexpected(other)),
    }
}
