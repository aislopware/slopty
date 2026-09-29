//! Each verb once: resolve its handles, send it, and take apart the one answer it expects. The
//! CLI and both MCP surfaces call these, so a verb means the same on every one of them.
//!
//! A verb that changes something takes the caller's [`IdempotencyKey`], if it gave one.

use slopty_core::WorkerId;
use slopty_proto::agent::{AgentKind, SessionAgent};
use slopty_proto::conversation::{ThreadId, Verdict};
use slopty_proto::items::{Item, ItemKind};
use slopty_proto::orchestration::{
    Command, ConversationPage, DirEntry, EventFilter, FileStat, HubEvent, IdempotencyKey, Input,
    ItemRef, Line, Outcome, Port, Screen, Size, TermRef, Verb, WaitUntil, Waited,
};
use slopty_proto::screen::{CaptureTarget, DisplayInfo, WindowInfo};
use slopty_proto::search::{FileHits, SearchQuery, SearchSummary};
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
/// How many matching lines a search in files returns when the caller names no limit: enough
/// to see what a query is about, few enough for a model to read.
pub const DEFAULT_MAX_MATCHES: u32 = 200;

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

async fn opened<D: Dispatch>(
    dispatch: &D,
    key: Option<IdempotencyKey>,
    verb: Verb,
) -> Result<TermRef, ToolError> {
    match dispatch.send(key, verb).await {
        Outcome::Opened(term) => Ok(term),
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
    opened(res.dispatch(), key, Verb::OpenTerminal { worker, cwd, command, env, name, size }).await
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
    key: Option<IdempotencyKey>,
) -> Result<TermRef, ToolError> {
    let worker = res.worker(worker).await?;
    let AgentSpec { cwd, prompt, args, env, size } = spec;
    let agent = AgentKind::ClaudeCode;
    let verb = Verb::SpawnAgent { worker, agent, cwd, prompt, args, env, size };
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

/// The agent in a terminal and its status.
pub async fn agent_status<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    term: &str,
) -> Result<Option<SessionAgent>, ToolError> {
    let term = res.term(term).await?;
    match res.dispatch().call(Verb::AgentStatus { term }).await {
        Outcome::Agent(agent) => Ok(agent),
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

/// Replace a file.
pub async fn write_file<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    path: String,
    bytes: Vec<u8>,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    let worker = res.worker(worker).await?;
    done(res.dispatch(), key, Verb::WriteFile { worker, path, bytes }).await
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

/// The items on a worker's workspace, and the worker.
pub async fn items<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
) -> Result<(WorkerId, Vec<Item>), ToolError> {
    let worker = res.worker(worker).await?;
    match res.dispatch().call(Verb::ListItems { worker }).await {
        Outcome::Items(list) => Ok((worker, list)),
        other => Err(ToolError::unexpected(other)),
    }
}

/// Put an item on a worker's workspace.
pub async fn open_item<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    kind: ItemKind,
    name: Option<String>,
    key: Option<IdempotencyKey>,
) -> Result<ItemRef, ToolError> {
    let worker = res.worker(worker).await?;
    match res.dispatch().send(key, Verb::OpenItem { worker, kind, name }).await {
        Outcome::Item(item) => Ok(item),
        other => Err(ToolError::unexpected(other)),
    }
}

/// Name an item, or take its name away.
pub async fn rename_item<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    item: &str,
    name: Option<String>,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    let item = res.item(item).await?;
    done(res.dispatch(), key, Verb::RenameItem { item, name }).await
}

/// Take an item off its workspace.
pub async fn remove_item<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    item: &str,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    let item = res.item(item).await?;
    done(res.dispatch(), key, Verb::RemoveItem { item }).await
}

/// Point every client at an item.
pub async fn point_at<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    item: &str,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    let item = res.item(item).await?;
    done(res.dispatch(), key, Verb::PointAt { item }).await
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

/// Entries a page of a conversation returns when the caller names no limit.
pub const DEFAULT_MAX_ENTRIES_PAGE: u32 = 50;

/// A page of the conversation of the agent in a terminal.
///
/// `thread`'s entries from `since`, or its last `max`. Orchestration follows the session from
/// then on, and its permission prompts wait for [`answer_permission`].
pub async fn read_conversation<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    term: &str,
    thread: ThreadId,
    since: Option<u32>,
    max: u32,
) -> Result<(TermRef, ConversationPage), ToolError> {
    let term = res.term(term).await?;
    let verb = Verb::ReadConversation { term, thread, since, max };
    match res.dispatch().call(verb).await {
        Outcome::Conversation(page) => Ok((term, *page)),
        other => Err(ToolError::unexpected(other)),
    }
}

/// Answer a permission prompt held for orchestration in a terminal.
pub async fn answer_permission<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    term: &str,
    ask: u64,
    verdict: Verdict,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    let term = res.term(term).await?;
    done(res.dispatch(), key, Verb::AnswerPermission { term, ask, verdict }).await
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
