//! Each verb once: resolve its handles, send it, and take apart the one answer it expects. The
//! CLI and both MCP surfaces call these, so a verb means the same on every one of them.
//!
//! A verb that changes something takes the caller's [`IdempotencyKey`], if it gave one.

use slopty_core::WorkerId;
use slopty_proto::agent::{AgentKind, SessionAgent};
use slopty_proto::folder::FsOp;
use slopty_proto::git::{GitDone, GitOp};
use slopty_proto::items::{Item, ItemKind};
use slopty_proto::orchestration::{
    Command, DirEntry, ErrorCode, EventFilter, FileStat, HubEvent, IdempotencyKey, Input, ItemRef,
    Line, Outcome, Port, Screen, Size, TermRef, ThreadOf, ThreadRead, ThreadView, Verb, WaitUntil,
    Waited,
};
use slopty_proto::project::{
    BadProjectId, LimitsChange, Moment, NodeDetail, Peer, Placement, Preference, Project,
    ProjectId, ProjectStatus, Report, Runner, ScheduleSpec, StepState, Suggestion, Task,
    TaskChange, TaskId, TaskLaunch, TaskSpec, TimelineEntry, WorkerFacts,
};
use slopty_proto::screen::{CaptureTarget, DisplayInfo, WindowInfo};
use slopty_proto::search::{FileHits, SearchQuery, SearchSummary};
use slopty_proto::server::WorkerInfo;
use slopty_proto::terminal::SessionSummary;
use slopty_proto::thread::{AskId, TurnId};

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

/// The person's git op in the repository holding `repo` on a worker.
///
/// A status, a commit of the files they chose with their message, a push, or a pull request
/// through their gh. A refusal comes back in git's or gh's own words.
pub async fn git<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    worker: Option<&str>,
    repo: String,
    op: GitOp,
    key: Option<IdempotencyKey>,
) -> Result<GitDone, ToolError> {
    let worker = res.worker(worker).await?;
    match res.dispatch().send(key, Verb::Git { worker, repo, op }).await {
        Outcome::Git(done) => Ok(*done),
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

/// Which thread a read or an answer is about, as its caller names it.
///
/// A thread's id (a subagent's too), the terminal an agent runs in, or a task (the caller's own
/// project's when none is named). A thread wins over a terminal, a terminal over a task.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadArg<'a> {
    /// A thread's id.
    pub thread: Option<&'a str>,
    /// A terminal, as the lists print it.
    pub term: Option<&'a str>,
    /// The task's project.
    pub project: Option<&'a str>,
    /// A task.
    pub task: Option<&'a str>,
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
    if let Some(term) = arg.term {
        return Ok(ThreadOf::Term(res.term(term).await?));
    }
    if arg.task.is_none() {
        return Err(ToolError::invalid("name a task, a thread or a terminal"));
    }
    let (project, task) = project_task(res.dispatch(), arg.project, arg.task).await?;
    Ok(ThreadOf::Task { project, task })
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
    /// What a fresh-context reviewer looks for before each merge; the person's to ask for.
    pub review: Option<String>,
    /// Push the target to `origin` after each merge; the person's to turn on.
    pub push: bool,
    /// Hold each task's start for the person; the person's to set.
    pub ask_to_start: bool,
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
        review: spec.review,
        push: spec.push,
        ask_to_start: spec.ask_to_start,
        orchestrator,
        limits: spec.limits,
        metadata: spec.metadata,
        members: Vec::new(),
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
    /// A new reviewer's brief; empty for no reviewer.
    pub review: Option<String>,
    /// Push the target after each merge, or stop.
    pub push: Option<bool>,
    /// Hold each task's start for the person, or start them as their orchestrator asks.
    pub ask_to_start: Option<bool>,
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
    let ProjectEdit { verifier, review, push, ask_to_start, limits, metadata, .. } = edit;
    let verb = Verb::ProjectSet {
        project,
        orchestrator,
        verifier,
        review,
        push,
        ask_to_start,
        limits,
        metadata,
        members: None,
    };
    project_answer(res.dispatch(), key, verb).await
}

/// Say what each kind of a project's work needs of its machines, in place of what was said.
///
/// # Errors
/// The project is not named or known, a rule does not compile, or a name is given twice.
pub async fn project_needs<D: Dispatch>(
    dispatch: &D,
    project: Option<&str>,
    needs: Vec<slopty_proto::project::Need>,
    key: Option<IdempotencyKey>,
) -> Result<ProjectStatus, ToolError> {
    let project = project_named(project, &own(dispatch).await?)?;
    project_answer(dispatch, key, Verb::ProjectNeeds { project, needs }).await
}

/// When a schedule runs: its cron rule, the IANA time zone it is read in (the server's own
/// when empty), and whether it waits paused.
#[derive(Debug, Default)]
pub struct ScheduleWhen {
    /// Five cron fields, or `@daily` and the like.
    pub when: String,
    /// The time zone.
    pub zone: String,
    /// It runs only when the person says.
    pub paused: bool,
}

/// Set a schedule of a project, as the person: `number` anew, or a new one. Each run makes
/// `new` (which hangs from no task and depends on none) and starts `launch` for it.
///
/// # Errors
/// As [`task_spawn`] for the worker and [`task_create`] for the placement; refused for an
/// agent, or a spec the server refuses.
pub async fn schedule_set<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    project: Option<&str>,
    number: Option<u32>,
    (new, launch, when): (NewTask, LaunchSpec, ScheduleWhen),
    key: Option<IdempotencyKey>,
) -> Result<ProjectStatus, ToolError> {
    let project = project_named(project, &own(res.dispatch()).await?)?;
    let placement = placement(res, new.placement).await?;
    let task = TaskSpec {
        parent: None,
        depends_on: Vec::new(),
        kind: new.kind,
        title: new.title,
        brief: new.brief,
        owns: new.owns,
        read_only: new.read_only,
        placement,
        verifier: new.verifier,
        metadata: None,
    };
    let pin = res.some_worker(launch.pin.as_deref()).await?;
    let LaunchSpec { cwd, run, env, size, ignore_dependencies, .. } = launch;
    let launch = TaskLaunch { pin, cwd, run, env, size, ignore_dependencies };
    let ScheduleWhen { when, zone, paused } = when;
    let spec = ScheduleSpec { task, launch, when, zone, paused };
    let verb = Verb::ScheduleSet { project, schedule: number, spec: Box::new(spec) };
    project_answer(res.dispatch(), key, verb).await
}

/// Take a schedule of a project away, as the person; its tasks stay.
///
/// # Errors
/// Refused for an agent, or a schedule the project does not have.
pub async fn schedule_delete<D: Dispatch>(
    dispatch: &D,
    project: Option<&str>,
    number: u32,
    key: Option<IdempotencyKey>,
) -> Result<ProjectStatus, ToolError> {
    let project = project_named(project, &own(dispatch).await?)?;
    project_answer(dispatch, key, Verb::ScheduleDelete { project, schedule: number }).await
}

/// Run a schedule of a project now, as the person: its task made and started.
///
/// # Errors
/// Refused for an agent, while its last run's task is under way, or as a start is.
pub async fn schedule_run<D: Dispatch>(
    dispatch: &D,
    project: Option<&str>,
    number: u32,
    key: Option<IdempotencyKey>,
) -> Result<Task, ToolError> {
    let project = project_named(project, &own(dispatch).await?)?;
    task_answer(dispatch, key, Verb::ScheduleRun { project, schedule: number }).await
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

/// Let a project go, as the person: its tasks, queue and timeline. Its terminals run on.
///
/// # Errors
/// The project is not named or not known, or the caller is an agent.
pub async fn project_delete<D: Dispatch>(
    dispatch: &D,
    project: &str,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    let project = project_named(Some(project), &Own::default())?;
    match dispatch.send(key, Verb::ProjectDelete { project }).await {
        Outcome::Done => Ok(()),
        other => Err(ToolError::unexpected(other)),
    }
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

/// Start a task its orchestrator proposed, as the person: on `pin` when one is named.
///
/// # Errors
/// As [`task_report`]; an agent is refused.
pub async fn task_start<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    project: Option<&str>,
    task: Option<&str>,
    on: Option<&str>,
    key: Option<IdempotencyKey>,
) -> Result<Task, ToolError> {
    let (project, task) = project_task(res.dispatch(), project, task).await?;
    let pin = match on {
        Some(name) => Some(res.worker(Some(name)).await?),
        None => None,
    };
    task_answer(res.dispatch(), key, Verb::TaskStart { project, task, pin }).await
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

/// Tell a task's agent something, through its hooks as a report goes.
///
/// The words are the person's, or an agent's to a task under it (the orchestrator to any of its
/// project's, a task's agent to those split from its own), marked as that agent's. Nothing is
/// typed into its terminal.
///
/// # Errors
/// As [`task_report`]; a task with no agent running is refused, and so is an agent's word to
/// a task not under it or one that waits on the person.
pub async fn task_tell<D: Dispatch>(
    dispatch: &D,
    project: Option<&str>,
    task: Option<&str>,
    text: String,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    let (project, task) = project_task(dispatch, project, task).await?;
    match dispatch.send(key, Verb::TaskTell { project, task: Some(task), text }).await {
        Outcome::Done => Ok(()),
        other => Err(ToolError::unexpected(other)),
    }
}

/// Tell a project's orchestrator something, as the person: their words reach it through its
/// hooks, and its inbox wakes it when it is idle.
///
/// # Errors
/// The project is not named or not known, it has no orchestrator running, or the caller is an
/// agent.
pub async fn orchestrator_tell<D: Dispatch>(
    dispatch: &D,
    project: &str,
    text: String,
    key: Option<IdempotencyKey>,
) -> Result<(), ToolError> {
    let project = project_named(Some(project), &Own::default())?;
    match dispatch.send(key, Verb::TaskTell { project, task: None, text }).await {
        Outcome::Done => Ok(()),
        other => Err(ToolError::unexpected(other)),
    }
}

/// How long [`task_wait`] waits when the caller names no timeout: under the minute many MCP
/// clients give a tool call.
pub const DEFAULT_TASK_WAIT_MS: u32 = 50_000;
/// The longest [`task_wait`] waits.
pub const TASK_WAIT_MAX_MS: u32 = 30 * 60_000;
/// The most tasks one [`task_wait`] follows.
pub const TASK_WAIT_MOST: usize = 64;
/// The longest one read of the project waits; the server caps it there too.
const READ_WAIT_MS: u32 = 240_000;

/// What [`task_wait`] saw.
#[derive(Clone, Debug)]
pub struct TaskWait {
    /// The project as last read.
    pub status: ProjectStatus,
    /// The tasks followed, in the order named.
    pub tasks: Vec<TaskId>,
    /// What each of them did since the wait began, oldest first.
    pub news: Vec<TimelineEntry>,
    /// The latest report of each, from the news or what the project's timeline still held.
    pub reports: Vec<TimelineEntry>,
    /// Those with news, or merged or given up when the wait began.
    pub ready: Vec<TaskId>,
    /// The time ran out first. Nothing was stopped or cancelled for it.
    pub timed_out: bool,
    /// The cursor to wait on from, as `since`, so nothing between two waits is missed.
    pub next: u64,
}

/// Whether a timeline entry is news of its task for [`task_wait`].
///
/// News is a report, a move of its state (its agent ending a turn without a report is one),
/// its terminal gone, a verdict, its checks, or a step that ended.
#[must_use]
pub const fn news(what: &Moment) -> bool {
    match what {
        Moment::Reported { .. }
        | Moment::State { .. }
        | Moment::AgentGone { .. }
        | Moment::Verified(_)
        | Moment::Reviewed(_)
        | Moment::Checks(_) => true,
        Moment::Step(step) => !matches!(step.state, StepState::Running { .. }),
        _ => false,
    }
}

/// Wait for the next news of `tasks` in a project, any of them or `all` of them.
///
/// It waits from `since`, or from now, for up to `timeout_ms`. A task merged or given up when
/// the wait begins is ready at once, since no news may come of it. Running out of time cancels
/// nothing: the answer says so, and `next` picks the wait up where it stopped.
///
/// It follows the project's timeline with the server's own long wait, so it costs a read per
/// change of the project, not a poll.
///
/// # Errors
/// No task, too many, a number that is not one, a task not in the project, or the server
/// could not be asked.
pub async fn task_wait<D: Dispatch>(
    dispatch: &D,
    project: Option<&str>,
    (tasks, all): (&[String], bool),
    since: Option<u64>,
    timeout_ms: u32,
) -> Result<TaskWait, ToolError> {
    let project = project_named(project, &own(dispatch).await?)?;
    let mut followed: Vec<TaskId> = Vec::new();
    for given in tasks {
        let task = task_number(given)?;
        if !followed.contains(&task) {
            followed.push(task);
        }
    }
    if followed.is_empty() || followed.len() > TASK_WAIT_MOST {
        return Err(ToolError::invalid(format!(
            "name between 1 and {TASK_WAIT_MOST} tasks to wait for"
        )));
    }
    let wait = std::time::Duration::from_millis(u64::from(timeout_ms.min(TASK_WAIT_MAX_MS)));
    let started = tokio::time::Instant::now();
    let read =
        |since, timeout_ms| Verb::ProjectStatus { project: project.clone(), since, timeout_ms };
    let mut status = project_answer(dispatch, None, read(since, 0)).await?;
    if let Some(missing) = followed.iter().find(|t| !status.tasks.iter().any(|card| card.id == **t))
    {
        return Err(ToolError::new(
            ErrorCode::UnknownTask,
            format!("no task {missing} in project {project}"),
        ));
    }
    let mut reports: Vec<TimelineEntry> = Vec::new();
    let mut news_of = Vec::new();
    let take = |status: &ProjectStatus, reports: &mut Vec<TimelineEntry>, news_of: &mut Vec<_>| {
        for e in status.timeline.iter().filter(|e| e.task.is_some_and(|t| followed.contains(&t))) {
            if matches!(e.what, Moment::Reported { .. }) {
                reports.retain(|r: &TimelineEntry| r.task != e.task);
                reports.push(e.clone());
            }
        }
        news_of.extend(
            status
                .timeline
                .iter()
                .filter(|e| e.task.is_some_and(|t| followed.contains(&t)) && news(&e.what))
                .cloned(),
        );
    };
    let mut discard = Vec::new();
    // With no cursor, what the timeline holds is the past: its reports only.
    take(&status, &mut reports, if since.is_some() { &mut news_of } else { &mut discard });
    let final_at_start: Vec<TaskId> = status
        .tasks
        .iter()
        .filter(|c| followed.contains(&c.id) && !c.state.holds_paths())
        .map(|c| c.id)
        .collect();
    let ready_of = |news_of: &[TimelineEntry]| -> Vec<TaskId> {
        followed
            .iter()
            .copied()
            .filter(|t| final_at_start.contains(t) || news_of.iter().any(|e| e.task == Some(*t)))
            .collect()
    };
    let mut cursor = status.next;
    let timed_out = loop {
        let ready = ready_of(&news_of);
        if if all { ready.len() == followed.len() } else { !ready.is_empty() } {
            break false;
        }
        let left = wait.saturating_sub(started.elapsed());
        if left.is_zero() {
            break true;
        }
        let left_ms = u32::try_from(left.as_millis()).unwrap_or(u32::MAX).clamp(1, READ_WAIT_MS);
        status = project_answer(dispatch, None, read(Some(cursor), left_ms)).await?;
        let left = wait.saturating_sub(started.elapsed());
        if status.next == cursor && !left.is_zero() {
            // Answered with nothing new before its time was up: never spin on such a server.
            tokio::time::sleep(std::time::Duration::from_millis(250).min(left)).await;
        }
        take(&status, &mut reports, &mut news_of);
        cursor = status.next;
    };
    Ok(TaskWait {
        ready: ready_of(&news_of),
        status,
        tasks: followed,
        news: news_of,
        reports,
        timed_out,
        next: cursor,
    })
}

/// Say whether a task's work may merge, as the reviewer the server started for it or as the
/// person: an approval queues it, changes asked give it back to its agent with the findings.
///
/// # Errors
/// As [`task_report`]; any other agent is refused.
pub async fn task_review<D: Dispatch>(
    dispatch: &D,
    project: Option<&str>,
    task: Option<&str>,
    verdict: slopty_proto::project::ReviewVerdict,
    key: Option<IdempotencyKey>,
) -> Result<Task, ToolError> {
    let (project, task) = project_task(dispatch, project, task).await?;
    task_answer(dispatch, key, Verb::TaskReview { project, task, verdict }).await
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

/// What runs for a task whose agent is named `agent`.
///
/// Claude Code (`claude`, the default) and Codex (`codex`) run in a terminal with their own
/// TUI; pi (`pi`), an ACP agent (`acp:<name>`, or the registry's bare name) and any other run
/// as a thread of the worker's thread host ([`Runner::Agent`]). `model`, by the agent's own id,
/// goes as Claude Code's and Codex's `--model` argument.
#[must_use]
pub fn agent_runner(
    agent: Option<&str>,
    prompt: Option<String>,
    mut model: Option<String>,
    mut args: Vec<String>,
) -> Runner {
    use slopty_proto::thread::AgentId;
    let mut with_model = |args: &mut Vec<String>| {
        if let Some(model) = model.take() {
            args.splice(0..0, ["--model".to_owned(), model]);
        }
    };
    match agent.map(str::trim).filter(|a| !a.is_empty()) {
        None | Some("claude" | AgentId::CLAUDE_CODE) => {
            with_model(&mut args);
            Runner::Claude { prompt, args }
        }
        Some(AgentId::CODEX) => {
            with_model(&mut args);
            Runner::Codex { prompt, args }
        }
        Some(name) => {
            let agent = if name == AgentId::PI || name.starts_with(AgentId::ACP_PREFIX) {
                AgentId::named(name)
            } else {
                AgentId::acp(name)
            };
            Runner::Agent { agent, prompt, model, args }
        }
    }
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

/// Have an agent try a task for each of `specs` at once, each an attempt of its own on the
/// worker it names or, where several fit, one no other attempt took.
///
/// # Errors
/// As [`task_spawn`]; refused when the task cannot be tried, or no attempt started.
pub async fn task_attempts<D: Dispatch>(
    res: &mut Resolver<'_, D>,
    project: Option<&str>,
    task: Option<&str>,
    specs: Vec<LaunchSpec>,
    key: Option<IdempotencyKey>,
) -> Result<Task, ToolError> {
    let (project, task) = project_task(res.dispatch(), project, task).await?;
    let mut launches = Vec::with_capacity(specs.len());
    for spec in specs {
        let pin = res.some_worker(spec.pin.as_deref()).await?;
        let LaunchSpec { cwd, run, env, size, ignore_dependencies, .. } = spec;
        launches.push(TaskLaunch { pin, cwd, run, env, size, ignore_dependencies });
    }
    task_answer(res.dispatch(), key, Verb::TaskAttempts { project, task, launches }).await
}

/// Pick the attempt that lands: every other attempt at its task stops, its agent closed and
/// its worktree freed. Answers the task tried.
///
/// # Errors
/// As [`task_report`]; refused for a task that is no attempt, or once another is picked.
pub async fn task_pick<D: Dispatch>(
    dispatch: &D,
    project: Option<&str>,
    attempt: &str,
    key: Option<IdempotencyKey>,
) -> Result<Task, ToolError> {
    let (project, attempt) = project_task(dispatch, project, Some(attempt)).await?;
    task_answer(dispatch, key, Verb::TaskPick { project, attempt }).await
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
