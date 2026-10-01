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
use slopty_proto::project::{
    BadProjectId, LimitsChange, NodeDetail, Peer, Placement, Preference, Project, ProjectId,
    ProjectStatus, Report, Runner, Suggestion, Task, TaskChange, TaskId, TaskLaunch, TaskSpec,
    WorkerFacts,
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
    let (workers, terminals, facts) = tokio::join!(
        dispatch.call(Verb::ListWorkers),
        dispatch.call(Verb::ListTerminals { worker: None }),
        dispatch.call(Verb::WorkerFacts { worker: None })
    );
    let workers = match workers {
        Outcome::Workers(list) => list,
        other => return Err(ToolError::unexpected(other)),
    };
    let terminals = match terminals {
        Outcome::Terminals(list) => list,
        other => return Err(ToolError::unexpected(other)),
    };
    // A worker reached on its own has no server to know the facts.
    let facts = match facts {
        Outcome::Facts(facts) => facts,
        _ => Vec::new(),
    };
    Ok(Overview { workers, terminals, facts })
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
    let verb = Verb::OpenTerminal { worker, cwd, command, env, name, size, session: None };
    opened(res.dispatch(), key, verb).await
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
    // The server sets `permission_flags` from the person's policy, whatever is asked here.
    let verb = Verb::SpawnAgent {
        worker,
        agent,
        cwd,
        prompt,
        args,
        env,
        size,
        session: None,
        permission_flags: false,
    };
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
/// `thread`'s entries from `since`, or its last `max`. Read by the person (the CLI outside an
/// agent's terminal), the session's permission prompts wait for [`answer_permission`] from then
/// on; read by an agent, nothing changes.
pub async fn read_conversation<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    term: &str,
    thread: ThreadId,
    since: Option<u32>,
    max: u32,
) -> Result<(TermRef, ConversationPage), ToolError> {
    let term = res.term(term).await?;
    // The server says whether the read holds prompts: the person's alone.
    let verb = Verb::ReadConversation { term, thread, since, max, hold: false };
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

/// The project `given` names, else the caller's own ([`Own::project`]).
///
/// # Errors
/// Neither names one, or the name is not a project name.
pub fn project_named(given: Option<&str>, own: &Own) -> Result<ProjectId, ToolError> {
    match given.map(str::trim).filter(|g| !g.is_empty()) {
        Some(name) => name.parse().map_err(|e: BadProjectId| ToolError::invalid(e.to_string())),
        None => own
            .project
            .clone()
            .ok_or_else(|| ToolError::invalid("name the project: this caller works for none")),
    }
}

/// What the caller works on.
#[derive(Clone, Debug, Default)]
pub struct Own {
    /// Its project.
    pub project: Option<ProjectId>,
    /// Its task in that project; none for the project's orchestrator.
    pub task: Option<TaskId>,
}

/// What the caller works on: the server's record of its own terminal first.
///
/// That is in any project; its environment ([`crate::Scope`]) counts only when the server has
/// it on nothing. An environment can be stale or inherited from another agent; the server's
/// record cannot.
///
/// # Errors
/// The server could not be asked.
pub async fn own<D: Dispatch>(dispatch: &D) -> Result<Own, ToolError> {
    let scope = dispatch.scope();
    if let Some(session) = scope.session {
        match dispatch.call(Verb::WorkingOn { session }).await {
            Outcome::WorkingOn(Some((project, task))) => {
                return Ok(Own { project: Some(project), task });
            }
            Outcome::WorkingOn(None) => {}
            other => return Err(ToolError::unexpected(other)),
        }
    }
    Ok(Own { project: scope.project, task: scope.task })
}

/// A task's number, `3` or `#3`.
///
/// # Errors
/// It is not one.
pub fn task_number(given: &str) -> Result<TaskId, ToolError> {
    let given = given.trim();
    given.parse().map_err(|e| ToolError::invalid(format!("{given:?} is not a task number: {e}")))
}

/// The task the caller works on in `project` ([`own`]); none in a project not its own.
async fn own_task<D: Dispatch>(
    dispatch: &D,
    project: &ProjectId,
) -> Result<Option<TaskId>, ToolError> {
    let own = own(dispatch).await?;
    Ok(own.task.filter(|_| own.project.as_ref() == Some(project)))
}

/// The project and the task named, the caller's own filling in what is not.
///
/// # Errors
/// No task is named and the caller has none in that project.
pub async fn project_task<D: Dispatch>(
    dispatch: &D,
    project: Option<&str>,
    task: Option<&str>,
) -> Result<(ProjectId, TaskId), ToolError> {
    let own = own(dispatch).await?;
    let project = project_named(project, &own)?;
    if let Some(task) = task.map(str::trim).filter(|t| !t.is_empty()) {
        return Ok((project, task_number(task)?));
    }
    match own.task.filter(|_| own.project.as_ref() == Some(&project)) {
        Some(task) => Ok((project, task)),
        None => Err(ToolError::invalid(format!(
            "name the task: this caller works on none in project {project}"
        ))),
    }
}

/// The terminal `given` names, else the caller's own ([`crate::Scope::session`]); none when
/// neither does and `required` is false.
async fn term_or_own<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    given: Option<&str>,
    required: bool,
) -> Result<Option<TermRef>, ToolError> {
    if let Some(term) = given.map(str::trim).filter(|g| !g.is_empty()) {
        return res.term(term).await.map(Some);
    }
    match res.dispatch().scope().session {
        Some(session) => res.term(&session.to_string()).await.map(Some),
        None if required => Err(ToolError::invalid(
            "name the terminal: this caller runs in no Slopty terminal (no SLOPTY_SESSION)",
        )),
        None => Ok(None),
    }
}

async fn project_answer<D: Dispatch>(
    dispatch: &D,
    key: Option<IdempotencyKey>,
    verb: Verb,
) -> Result<ProjectStatus, ToolError> {
    match dispatch.send(key, verb).await {
        Outcome::Project(status) => Ok(*status),
        other => Err(ToolError::unexpected(other)),
    }
}

async fn task_answer<D: Dispatch>(
    dispatch: &D,
    key: Option<IdempotencyKey>,
    verb: Verb,
) -> Result<Task, ToolError> {
    match dispatch.send(key, verb).await {
        Outcome::Task(task) => Ok(*task),
        other => Err(ToolError::unexpected(other)),
    }
}

/// A new project's fields, names unresolved.
#[derive(Debug, Default)]
pub struct ProjectSpec {
    /// Its name.
    pub project: String,
    /// What it is for.
    pub title: String,
    /// Its repository.
    pub repo: String,
    /// The branch work lands on; `main` when absent.
    pub target: Option<String>,
    /// The verifier command.
    pub verifier: Option<String>,
    /// Push the target to `origin` after each merge; the person's to turn on.
    pub push: bool,
    /// The orchestrator's terminal; the caller's own when absent and it runs in one.
    pub orchestrator: Option<String>,
    /// Its limits over the defaults.
    pub limits: LimitsChange,
    /// Anything kept with it: the text of a JSON object.
    pub metadata: Option<String>,
}

/// Make a project.
pub async fn project_create<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    spec: ProjectSpec,
    key: Option<IdempotencyKey>,
) -> Result<ProjectStatus, ToolError> {
    let project = project_named(Some(&spec.project), &Own::default())?;
    let orchestrator = term_or_own(res, spec.orchestrator.as_deref(), false).await?;
    let verb = Verb::ProjectCreate {
        project,
        title: spec.title,
        repo: spec.repo,
        target: spec.target.unwrap_or_else(|| "main".to_owned()),
        verifier: spec.verifier,
        push: spec.push,
        orchestrator,
        limits: spec.limits,
        metadata: spec.metadata,
    };
    project_answer(res.dispatch(), key, verb).await
}

/// A change to a project, names unresolved.
#[derive(Debug, Default)]
pub struct ProjectEdit {
    /// A new orchestrator terminal.
    pub orchestrator: Option<String>,
    /// A new verifier; empty for none.
    pub verifier: Option<String>,
    /// Push the target after each merge, or stop.
    pub push: Option<bool>,
    /// New limits.
    pub limits: LimitsChange,
    /// New metadata.
    pub metadata: Option<String>,
}

/// Change a project's orchestrator, verifier, limits or metadata.
pub async fn project_set<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    project: Option<&str>,
    edit: ProjectEdit,
    key: Option<IdempotencyKey>,
) -> Result<ProjectStatus, ToolError> {
    let project = project_named(project, &own(res.dispatch()).await?)?;
    let orchestrator = match edit.orchestrator.as_deref() {
        Some(term) => Some(res.term(term).await?),
        None => None,
    };
    let ProjectEdit { verifier, push, limits, metadata, .. } = edit;
    let verb = Verb::ProjectSet { project, orchestrator, verifier, push, limits, metadata };
    project_answer(res.dispatch(), key, verb).await
}

/// Every project.
pub async fn projects<D: Dispatch>(dispatch: &D) -> Result<Vec<Project>, ToolError> {
    match dispatch.call(Verb::ProjectList).await {
        Outcome::Projects(list) => Ok(list),
        other => Err(ToolError::unexpected(other)),
    }
}

/// A project whole, its timeline from `since`, waiting up to `timeout_ms` for news past it.
pub async fn project_status<D: Dispatch>(
    dispatch: &D,
    project: Option<&str>,
    since: Option<u64>,
    timeout_ms: u32,
) -> Result<ProjectStatus, ToolError> {
    let project = project_named(project, &own(dispatch).await?)?;
    project_answer(dispatch, None, Verb::ProjectStatus { project, since, timeout_ms }).await
}

/// A task or a worker to run beside or away from: `#3` or `3` is a task, anything else a
/// worker's name or id.
pub async fn peer<D: Dispatch>(res: &mut Resolver<'_, D>, given: &str) -> Result<Peer, ToolError> {
    let given = given.trim();
    let digits = given.trim_start_matches('#');
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        return task_number(given).map(Peer::Task);
    }
    res.worker(Some(given)).await.map(Peer::Worker)
}

/// A placement, names unresolved.
#[derive(Clone, Debug, Default)]
pub struct PlacementSpec {
    /// A worker by name or id: this one and no other.
    pub pin: Option<String>,
    /// CEL rules each worker must meet.
    pub require: Vec<String>,
    /// CEL rules that score a worker, with their weights.
    pub prefer: Vec<Preference>,
    /// Tasks (`#3`) or workers to run beside.
    pub near: Vec<String>,
    /// Tasks (`#3`) or workers to keep away from.
    pub avoid: Vec<String>,
}

/// `spec` with its names resolved.
///
/// # Errors
/// A worker it names is not one.
pub async fn placement<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    spec: PlacementSpec,
) -> Result<Placement, ToolError> {
    let pin = res.some_worker(spec.pin.as_deref()).await?;
    let mut near = Vec::with_capacity(spec.near.len());
    for p in &spec.near {
        near.push(peer(res, p).await?);
    }
    let mut avoid = Vec::with_capacity(spec.avoid.len());
    for p in &spec.avoid {
        avoid.push(peer(res, p).await?);
    }
    Ok(Placement { pin, require: spec.require, prefer: spec.prefer, near, avoid })
}

/// A new task's fields, names unresolved.
#[derive(Debug, Default)]
pub struct NewTask {
    /// The task it was split from; the caller's own task when absent and it works on one in
    /// the same project.
    pub parent: Option<String>,
    /// Tasks it needs first.
    pub depends_on: Vec<String>,
    /// What sort of work it is.
    pub kind: String,
    /// What it is.
    pub title: String,
    /// What its agent is told.
    pub brief: String,
    /// The paths it alone may write.
    pub owns: Vec<String>,
    /// It only reads.
    pub read_only: bool,
    /// Where it may run.
    pub placement: PlacementSpec,
    /// Its own verifier.
    pub verifier: Option<String>,
    /// Anything kept with it: the text of a JSON object.
    pub metadata: Option<String>,
}

/// Make a task.
pub async fn task_create<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    project: Option<&str>,
    new: NewTask,
    key: Option<IdempotencyKey>,
) -> Result<Task, ToolError> {
    let project = project_named(project, &own(res.dispatch()).await?)?;
    let parent = match new.parent.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(parent) => Some(task_number(parent)?),
        None => own_task(res.dispatch(), &project).await?,
    };
    let depends_on = new.depends_on.iter().map(|d| task_number(d)).collect::<Result<_, _>>()?;
    let placement = placement(res, new.placement).await?;
    let spec = TaskSpec {
        parent,
        depends_on,
        kind: new.kind,
        title: new.title,
        brief: new.brief,
        owns: new.owns,
        read_only: new.read_only,
        placement,
        verifier: new.verifier,
        metadata: new.metadata,
    };
    task_answer(res.dispatch(), key, Verb::TaskCreate { project, spec: Box::new(spec) }).await
}

/// Take more paths for a task to own.
pub async fn task_claim<D: Dispatch>(
    dispatch: &D,
    project: Option<&str>,
    task: Option<&str>,
    paths: Vec<String>,
    key: Option<IdempotencyKey>,
) -> Result<Task, ToolError> {
    let (project, task) = project_task(dispatch, project, task).await?;
    task_answer(dispatch, key, Verb::TaskClaim { project, task, paths }).await
}

/// Change a task: move it, set its status, dependencies, placement, verifier or metadata,
/// record its branch or verifier's word, note something. `placement`, when given, replaces
/// the task's.
pub async fn task_update<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    project: Option<&str>,
    task: Option<&str>,
    mut change: TaskChange,
    placement_spec: Option<PlacementSpec>,
    key: Option<IdempotencyKey>,
) -> Result<Task, ToolError> {
    let (project, task) = project_task(res.dispatch(), project, task).await?;
    if let Some(spec) = placement_spec {
        change.placement = Some(placement(res, spec).await?);
    }
    task_answer(res.dispatch(), key, Verb::TaskUpdate { project, task, change: Box::new(change) })
        .await
}

/// Report on a task's work (the caller's own when none is named) to whoever split it off.
pub async fn task_report<D: Dispatch>(
    dispatch: &D,
    project: Option<&str>,
    task: Option<&str>,
    report: Report,
    key: Option<IdempotencyKey>,
) -> Result<Task, ToolError> {
    let (project, task) = project_task(dispatch, project, task).await?;
    task_answer(dispatch, key, Verb::TaskReport { project, task, report }).await
}

/// Put a task in its project's merge queue, as the person: its verifier runs first when one
/// applies.
///
/// # Errors
/// As [`task_report`]; an agent is refused.
pub async fn task_merge<D: Dispatch>(
    dispatch: &D,
    project: Option<&str>,
    task: Option<&str>,
    key: Option<IdempotencyKey>,
) -> Result<Task, ToolError> {
    let (project, task) = project_task(dispatch, project, task).await?;
    task_answer(dispatch, key, Verb::TaskMerge { project, task }).await
}

/// One node of a project's tree in full: the task named (the caller's own when none is), or
/// the orchestrator's node for `orchestrator`.
pub async fn task_get<D: Dispatch>(
    dispatch: &D,
    project: Option<&str>,
    task: Option<&str>,
) -> Result<NodeDetail, ToolError> {
    let (project, task) = if task.map(str::trim) == Some("orchestrator") {
        (project_named(project, &own(dispatch).await?)?, None)
    } else {
        let (project, task) = project_task(dispatch, project, task).await?;
        (project, Some(task))
    };
    match dispatch.call(Verb::TaskGet { project, task }).await {
        Outcome::Node(node) => Ok(*node),
        other => Err(ToolError::unexpected(other)),
    }
}

/// Put the terminal named (the caller's own when none is) on a task.
pub async fn task_assign<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    project: Option<&str>,
    task: Option<&str>,
    term: Option<&str>,
    key: Option<IdempotencyKey>,
) -> Result<Task, ToolError> {
    let (project, task) = project_task(res.dispatch(), project, task).await?;
    let Some(term) = term_or_own(res, term, true).await? else {
        return Err(ToolError::invalid("name the terminal"));
    };
    task_answer(res.dispatch(), key, Verb::TaskAssign { project, task, term }).await
}

/// How to start what runs for a task, the worker unresolved.
#[derive(Debug)]
pub struct LaunchSpec {
    /// A worker by name or id, over the task's placement; the server places it when absent.
    pub pin: Option<String>,
    /// Working directory on the worker; the worker's home when empty.
    pub cwd: String,
    /// What runs.
    pub run: Runner,
    /// Extra environment.
    pub env: Vec<(String, String)>,
    /// Its grid until a client shows it.
    pub size: Option<Size>,
    /// Start it though a task it depends on is not done.
    pub ignore_dependencies: bool,
}

/// Start what runs for a task where the server places it, or on the worker named.
pub async fn task_spawn<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    project: Option<&str>,
    task: Option<&str>,
    spec: LaunchSpec,
    key: Option<IdempotencyKey>,
) -> Result<Task, ToolError> {
    let (project, task) = project_task(res.dispatch(), project, task).await?;
    let pin = res.some_worker(spec.pin.as_deref()).await?;
    let LaunchSpec { cwd, run, env, size, ignore_dependencies, .. } = spec;
    let launch = TaskLaunch { pin, cwd, run, env, size, ignore_dependencies };
    task_answer(res.dispatch(), key, Verb::TaskSpawn { project, task, launch }).await
}

/// Every worker ranked for a placement: a task's own, `spec` in its stead, or `spec` alone.
pub async fn placement_suggest<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    project: Option<&str>,
    task: Option<&str>,
    spec: Option<PlacementSpec>,
) -> Result<Vec<Suggestion>, ToolError> {
    let own = own(res.dispatch()).await?;
    let named = project.map(str::trim).filter(|p| !p.is_empty());
    let project = match named {
        Some(p) => Some(project_named(Some(p), &own)?),
        None => own.project.clone(),
    };
    let task = match (task.map(str::trim).filter(|t| !t.is_empty()), &project) {
        (Some(t), _) => Some(task_number(t)?),
        (None, Some(p)) if spec.is_none() => own_task(res.dispatch(), p).await?,
        (None, _) => None,
    };
    let placement = match spec {
        Some(spec) => Some(placement(res, spec).await?),
        None => None,
    };
    let verb = Verb::PlacementSuggest { project, task, placement };
    match res.dispatch().call(verb).await {
        Outcome::Suggestions(ranked) => Ok(ranked),
        other => Err(ToolError::unexpected(other)),
    }
}

/// What the workers are and have, or one worker.
pub async fn worker_facts<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
) -> Result<Vec<WorkerFacts>, ToolError> {
    let worker = res.some_worker(worker).await?;
    match res.dispatch().call(Verb::WorkerFacts { worker }).await {
        Outcome::Facts(facts) => Ok(facts),
        other => Err(ToolError::unexpected(other)),
    }
}
