//! A project's verbs as MCP tools: names, descriptions, hints, argument schemas, and a call run
//! end to end (arguments parsed, names resolved, the verb sent, the answer rendered as its view).
//!
//! Every MCP surface serves exactly this list and this [`call`], so a model sees the same tools
//! and gets the same JSON whether it reaches the server's endpoint or `slopty mcp`. They are
//! only what a project's orchestrator and its tasks' agents need; every other verb is the
//! `slopty` command, which an agent runs through its shell like any other.

use std::collections::BTreeMap;
use std::time::Duration;

use rmcp::ErrorData;
use rmcp::handler::server::common::schema_for_type;
use rmcp::model::{CallToolResult, ContentBlock, Tool, ToolAnnotations};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use slopty_proto::orchestration::{ErrorCode, IdempotencyKey, Size, ThreadView};
use slopty_proto::project::{Report, Runner, TaskChange, TaskId};

use crate::ops::{self, LaunchSpec, NewTask, Which};
use crate::resolve::Resolver;
use crate::{Dispatch, ToolError, view};

/// What the model reads before any tool description.
pub const INSTRUCTIONS: &str = "\
These tools are a Slopty project's: one goal that agents work on side by side across a fleet \
of machines (workers). project_status shows the whole project, its tasks and its timeline, and \
with since and timeout_ms waits for the next change; task_get shows one task in full. The \
orchestrator starts work with task_start: each task is one agent (Claude Code, Codex, pi or an \
ACP agent) or a command, working from its brief in a worktree of its own on the worker named \
or one with room. Tasks sit side by side under the project and do not nest. task_tell says \
more to a task's agent, task_wait waits for tasks' news, and task_update changes a task. A \
task's agent moves its own task with task_update and reports it with task_report; its project \
and task are the defaults. read_thread reads another agent's thread; the requests on it are \
the person's to answer, never an agent's. Everything else, the workers and their facts, \
terminals, files and the workspace, is the `slopty` command in your shell: `slopty --help` \
lists it, and `slopty --json …` prints what a script reads.";

/// How often a long wait with a progress sink reports that it is still waiting.
pub const PROGRESS_EVERY: Duration = Duration::from_secs(10);

/// Told how long a long wait has waited so far, every [`PROGRESS_EVERY`].
pub type Progress<'a> = &'a (dyn Fn(Duration) + Send + Sync);

/// A task by its number, `3` or `"#3"`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
enum TaskArg {
    /// Its number.
    Number(u32),
    /// Its number as text.
    Text(String),
}

impl TaskArg {
    fn text(&self) -> String {
        match self {
            Self::Number(n) => n.to_string(),
            Self::Text(t) => t.clone(),
        }
    }
}

fn task_text(task: Option<&TaskArg>) -> Option<String> {
    task.map(TaskArg::text)
}

/// A JSON object as the text the server keeps.
fn metadata_text(doc: Option<Map<String, Value>>) -> Result<Option<String>, ToolError> {
    doc.map(|d| serde_json::to_string(&d).map_err(|e| ToolError::invalid(e.to_string())))
        .transpose()
}

fn task_numbers(tasks: &[TaskArg]) -> Result<Vec<TaskId>, ToolError> {
    tasks.iter().map(|t| ops::task_number(&t.text())).collect()
}

/// `project_status`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProjectStatusArgs {
    /// The project; yours when omitted.
    project: Option<String>,
    /// Timeline cursor: the `next` of the previous call. The latest entries when omitted; 0
    /// for all the server keeps.
    since: Option<u64>,
    /// Wait this long for a timeline entry past `since`; 0 (the default) answers at once. The
    /// server caps it at 240000.
    timeout_ms: Option<u32>,
}

/// `task_get`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskGetArgs {
    /// The project; yours when omitted.
    project: Option<String>,
    /// The task; yours when omitted. `orchestrator` for the orchestrator's own node.
    task: Option<TaskArg>,
}

/// `task_start`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskStartArgs {
    /// The project; yours when omitted.
    project: Option<String>,
    /// A task the project has, to start: one made before and refused its start, one stopped or
    /// given back. A new task is made from `title` and the fields after it when omitted.
    task: Option<TaskArg>,
    /// A new task: what it is, in a line.
    title: Option<String>,
    /// A new task: what its agent is told to do (the goal, the constraints, how to know it is
    /// done). Its first prompt, unless `prompt` says otherwise.
    #[serde(default)]
    brief: String,
    /// A new task: what sort of work it is, in your words (`build`, `review`, `bench`).
    #[serde(default)]
    kind: String,
    /// A new task: tasks whose work it needs first; it starts once they are done unless
    /// `ignore_dependencies`. A dependency never leads back to it.
    #[serde(default)]
    depends_on: Vec<TaskArg>,
    /// A new task: it only reads.
    #[serde(default)]
    read_only: bool,
    /// A new task: anything to keep with it, as a JSON object.
    metadata: Option<Map<String, Value>>,
    /// This worker (name or id); one with room when omitted. `slopty --json workers`
    /// shows each worker's facts, and work that needs no Apple platform belongs on Linux.
    worker: Option<String>,
    /// Working directory on the worker; beside a clone of the project's repository, in a git
    /// worktree of the task's own when it writes, when omitted.
    cwd: Option<String>,
    /// Run this instead of an agent: any program and its arguments, such as a build or a
    /// benchmark; `[]` for the login shell.
    command: Option<Vec<String>>,
    /// Which agent: `claude` (the default), `codex`, `pi`, or an ACP agent by the registry's
    /// name (`gemini`, `acp:gemini`). Each gets Slopty's tools and its role, and goes only to
    /// a worker with it installed.
    agent: Option<String>,
    /// The agent's first prompt; a new task's brief when omitted.
    prompt: Option<String>,
    /// The model, by the agent's own id.
    model: Option<String>,
    /// Arguments for the agent, e.g. `["--effort", "high"]`. Flags that loosen Claude Code's
    /// permissions are refused unless the person allows them for the project.
    #[serde(default)]
    args: Vec<String>,
    /// Extra environment variables.
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// Columns (10-1000); with `rows`.
    cols: Option<u16>,
    /// Rows (2-500); with `cols`.
    rows: Option<u16>,
    /// Start it though a task it depends on is not done yet.
    #[serde(default)]
    ignore_dependencies: bool,
    /// A name for this call's effect, such as a fresh UUID; a repeat answers as the first did.
    idempotency_key: Option<String>,
}

impl TaskStartArgs {
    /// Which task it starts, and how.
    fn start(self) -> Result<(Option<String>, Which, LaunchSpec, Option<String>), ToolError> {
        let run = match (self.command, self.agent) {
            (Some(argv), None)
                if self.prompt.is_none() && self.model.is_none() && self.args.is_empty() =>
            {
                Runner::Command { argv }
            }
            (Some(_), _) => {
                return Err(ToolError::invalid(
                    "prompt, model, args and agent are an agent's; a command takes its own \
                     arguments",
                ));
            }
            (None, agent) => {
                ops::agent_runner(agent.as_deref(), self.prompt, self.model, self.args)
            }
        };
        let launch = LaunchSpec {
            pin: self.worker,
            cwd: self.cwd.unwrap_or_default(),
            run,
            env: self.env.into_iter().collect(),
            size: size(self.cols, self.rows)?,
            ignore_dependencies: self.ignore_dependencies,
        };
        let new_fields = self.title.is_some()
            || !self.brief.is_empty()
            || !self.kind.is_empty()
            || !self.depends_on.is_empty()
            || self.read_only
            || self.metadata.is_some();
        let which = match (self.task, self.title) {
            (Some(task), None) if !new_fields => Which::Made(task.text()),
            (Some(_), _) => {
                return Err(ToolError::invalid(
                    "`task` starts one the project has; a new task's fields (title, brief, kind, \
                     depends_on, read_only, metadata) go without it",
                ));
            }
            (None, Some(title)) => Which::New(Box::new(NewTask {
                depends_on: self.depends_on.iter().map(TaskArg::text).collect(),
                kind: self.kind,
                title,
                brief: self.brief,
                read_only: self.read_only,
                verifier: None,
                metadata: metadata_text(self.metadata)?,
            })),
            (None, None) => {
                return Err(ToolError::invalid(
                    "give a new task's `title`, or the `task` to start",
                ));
            }
        };
        Ok((self.project, which, launch, self.idempotency_key))
    }
}

/// `task_update`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskUpdateArgs {
    /// The project; yours when omitted.
    project: Option<String>,
    /// The task; yours when omitted.
    task: Option<TaskArg>,
    /// A new state: planned, running, waiting, blocked, verifying, done, merged, failed. A
    /// merged task is final; only a done or verifying task merges.
    state: Option<String>,
    /// What you are doing, in your own words, for the tree; empty clears it.
    status: Option<String>,
    /// The branch its work is on.
    branch: Option<String>,
    /// The commit its work starts from, in hex.
    base: Option<String>,
    /// Words for the project's timeline.
    note: Option<String>,
    /// New dependencies, in place of the old.
    depends_on: Option<Vec<TaskArg>>,
    /// Its own verifier command; empty for the project's.
    verifier: Option<String>,
    /// New metadata, a JSON object in place of the old.
    metadata: Option<Map<String, Value>>,
    /// A name for this call's effect, such as a fresh UUID; a repeat answers as the first did.
    idempotency_key: Option<String>,
}

/// `task_report`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskReportArgs {
    /// The project; yours when omitted.
    project: Option<String>,
    /// The task; yours when omitted.
    task: Option<TaskArg>,
    /// What you have to say, in a few lines.
    #[serde(default)]
    note: String,
    /// What you made: paths, commits, links.
    #[serde(default)]
    artifacts: Vec<String>,
    /// The branch your work is on.
    branch: Option<String>,
    /// The pull request you opened, by number.
    pr: Option<u32>,
    /// A name for this call's effect, such as a fresh UUID; a repeat answers as the first did.
    idempotency_key: Option<String>,
}

/// `task_tell`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskTellArgs {
    /// The project; yours when omitted.
    project: Option<String>,
    /// The task to tell: any of the project you orchestrate.
    task: TaskArg,
    /// What to say, in a few lines.
    text: String,
    /// A name for this call's effect, such as a fresh UUID; a repeat answers as the first did.
    idempotency_key: Option<String>,
}

/// `task_wait`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskWaitArgs {
    /// The project; yours when omitted.
    project: Option<String>,
    /// The tasks to wait for, at most 64.
    tasks: Vec<TaskArg>,
    /// `any` (the default): answer at the first news of any of them; `all`: once each has
    /// news.
    until: Option<UntilArg>,
    /// Timeline cursor: the `next` of the previous `task_wait` or `project_status`, so nothing in
    /// between is missed. From now when omitted.
    since: Option<u64>,
    /// How long to wait, at most 1800000; 50000 when omitted. Running out cancels nothing.
    timeout_ms: Option<u32>,
}

/// What `task_wait` waits for.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum UntilArg {
    /// The first news of any task named.
    #[default]
    Any,
    /// News of every task named.
    All,
}

impl TaskReportArgs {
    fn report(&self) -> Report {
        Report {
            note: self.note.clone(),
            artifacts: self.artifacts.clone(),
            branch: self.branch.clone(),
            pr: self.pr,
        }
    }
}

impl TaskUpdateArgs {
    fn change(&self) -> Result<TaskChange, ToolError> {
        let state = match self.state.as_deref() {
            Some(word) => Some(view::projects::state_named(word).ok_or_else(|| {
                ToolError::invalid(format!(
                    "state is planned, running, waiting, blocked, verifying, done, merged or \
                     failed, not {word:?}"
                ))
            })?),
            None => None,
        };
        let depends_on = self.depends_on.as_deref().map(task_numbers).transpose()?;
        Ok(TaskChange {
            state,
            status: self.status.clone(),
            branch: self.branch.clone(),
            verified: None,
            base: self.base.clone(),
            note: self.note.clone(),
            depends_on,
            run_on: None,
            verifier: self.verifier.clone(),
            metadata: metadata_text(self.metadata.clone())?,
        })
    }
}

/// `read_thread`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadThreadArgs {
    /// The task whose agent's thread to read; with `project`, yours when omitted.
    task: Option<TaskArg>,
    /// The task's project; yours when omitted.
    project: Option<String>,
    /// A thread's id instead: a subagent's `child` from an earlier read, say.
    thread: Option<String>,
    /// The terminal an agent runs in instead, as the lists print it (`worker/session`).
    term: Option<String>,
    /// `messages` (the default) for what was said, `activity` for that and every tool call
    /// with the end of its output.
    #[serde(default)]
    view: ViewArg,
    /// The last turn already read: the `next` of the previous read. From the first turn held
    /// when omitted.
    after: Option<u32>,
}

/// How much of a thread a read gives.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ViewArg {
    /// What was said.
    #[default]
    Messages,
    /// What was said and done.
    Activity,
}

impl ViewArg {
    const fn view(self) -> ThreadView {
        match self {
            Self::Messages => ThreadView::Messages,
            Self::Activity => ThreadView::Activity,
        }
    }
}

/// The key a call gave, checked.
fn checked_key(given: Option<String>) -> Result<Option<IdempotencyKey>, ToolError> {
    given.map(IdempotencyKey::new).transpose().map_err(|e| ToolError::invalid(e.to_string()))
}

/// `cols` and `rows` together, or neither.
fn size(cols: Option<u16>, rows: Option<u16>) -> Result<Option<Size>, ToolError> {
    match (cols, rows) {
        (Some(cols), Some(rows)) => Ok(Some(Size { cols, rows })),
        (None, None) => Ok(None),
        _ => Err(ToolError::invalid("give cols and rows together")),
    }
}

/// How a tool behaves, for the client's hints.
#[derive(Clone, Copy)]
enum Kind {
    /// Reads only.
    Read,
    /// Changes something, nothing lost.
    Write,
}

fn tool<T: JsonSchema + 'static>(
    name: &'static str,
    description: &'static str,
    kind: Kind,
) -> Tool {
    let hints = ToolAnnotations::new().open_world(false);
    let hints = match kind {
        Kind::Read => hints.read_only(true),
        Kind::Write => hints.read_only(false).destructive(false),
    };
    Tool::new(name, description, schema_for_type::<T>()).annotate(hints)
}

/// Every tool, in the order a model meets them.
#[must_use]
pub fn list() -> Vec<Tool> {
    vec![
        tool::<ProjectStatusArgs>(
            "project_status",
            "A project whole: its `tasks` (each with `state`, its agent's own `status`, \
             `depends_on`, `kind`, the worker it is pinned to, its terminal's `term`, \
             `branch`, `pr`, `verified`), `natives` (Claude Code's own subagents and to-dos in \
             each node's session), `bounds` and `limits` with what runs now (`live`), and its \
             `timeline` from `since`, with `next` to read on from. With `timeout_ms` it waits \
             for the next change: how to follow the tree without polling.",
            Kind::Read,
        ),
        tool::<TaskGetArgs>(
            "task_get",
            "One node of a project's tree in full: the task's brief, pin, verifier, base \
             commit and metadata, and the subagents and to-dos Claude Code keeps \
             in it. project_status shows the tree; this shows a node.",
            Kind::Read,
        ),
        tool::<TaskStartArgs>(
            "task_start",
            "Start a task, the one way work starts: a new one made from a `title`, a `brief` \
             its agent works from (its first prompt), a `kind`, `depends_on` or `read_only`; or \
             a `task` the project has. It runs Claude Code, another `agent` or a `command` on \
             the `worker` you name (`slopty --json workers` shows each one's facts; work \
             that needs no Apple platform belongs on Linux) or one with room, beside a clone of \
             the project's repository in a worktree of its own unless you name a `cwd`. Start \
             only work that runs in parallel with yours and needs no context you hold: do \
             sequential or small work yourself. A start is refused while as many tasks wait on \
             the person as the project's review limit, saying how many and where. A new task \
             refused its start is kept: start it later by its number. Returns the task with its \
             terminal's `term`.",
            Kind::Write,
        ),
        tool::<TaskUpdateArgs>(
            "task_update",
            "Change a task: its `state` (verifying, done, failed, planned again; merging is the \
             person's), your own `status` text, `depends_on`, `metadata`; record its \
             `branch` or the `base` commit its work starts from, or put a `note` on the \
             timeline. Its agent's own status moves it among running, waiting and blocked.",
            Kind::Write,
        ),
        tool::<TaskReportArgs>(
            "task_report",
            "Report your task done to the project's orchestrator, with a `note`, what you made \
             (`artifacts`), your `branch` and `pr`. It reaches the orchestrator through its own \
             hooks once your task settles, never typed into its terminal, and stays on the \
             timeline. Done work is checked, then waits for the person to merge it. A question \
             or a block needs no report: the orchestrator hears your turn end.",
            Kind::Write,
        ),
        tool::<TaskTellArgs>(
            "task_tell",
            "As the orchestrator, tell a task's agent something. It reaches that agent through \
             its hooks at once, after anything the person said and never in its place, marked \
             as your words, not the person's; your latest replaces one still unread. It never \
             answers a permission or a question the person was asked: a task waiting on the \
             person is refused until it moves on. A task's agent reports with task_report.",
            Kind::Write,
        ),
        tool::<TaskWaitArgs>(
            "task_wait",
            "Wait for news of tasks: a report, a move of state (its agent ending a turn without \
             a report is one, waiting on the person another), its terminal gone, its verifier \
             or checks, a step that ended. `until` any (default) or all. A task merged or \
             failed is ready at once. Returns each task's card with its `news` and \
             `last_report`, `ready`, `timed_out`, and `next` to wait on from. Running out of time \
             cancels nothing. For agents without hooks; hooks bring the same news unasked.",
            Kind::Read,
        ),
        tool::<ReadThreadArgs>(
            "read_thread",
            "What another agent's thread did, whatever its agent (Claude Code, Codex, pi, ACP): \
             a task's (by `task`), a subagent's (by `thread`) or a terminal's (by `term`). \
             Returns whole `turns` after `after`, each with its `entries` (`user` and `agent` \
             messages; with view `activity` also each `tool` call with its `state`, the end of \
             its `output` and the `child` thread it started, and the agent's `notice`s), its \
             `phase` and what it `wait`s on, and the `requests` open on it, which are the \
             person's to answer. Bounded: `truncated` says a text was cut or turns were left \
             out. Pass `next` back as `after` to read on; a turn under way is read again until \
             it ends. Reading changes nothing.",
            Kind::Read,
        ),
    ]
}

/// The tool called `name`.
#[must_use]
pub fn get(name: &str) -> Option<Tool> {
    list().into_iter().find(|t| t.name == name)
}

/// Run the tool `name` on `dispatch`, its answer as compact JSON text.
///
/// A failure of the tool is an `isError` result for the model to read; a long wait tells
/// `progress`, when given, how long it has waited every [`PROGRESS_EVERY`].
///
/// # Errors
/// Invalid params when no tool has the name.
pub async fn call<D: Dispatch>(
    dispatch: &D,
    name: &str,
    arguments: Map<String, Value>,
    progress: Option<Progress<'_>>,
) -> Result<CallToolResult, ErrorData> {
    if get(name).is_none() {
        return Err(ErrorData::invalid_params(format!("no tool is called {name}"), None));
    }
    Ok(match run(dispatch, name, arguments, progress).await {
        Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
        Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]),
    })
}

fn args<T: DeserializeOwned>(arguments: Map<String, Value>) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(arguments))
        .map_err(|e| ToolError::invalid(format!("bad arguments: {e}")))
}

fn json(value: &impl serde::Serialize) -> Result<Value, ToolError> {
    serde_json::to_value(value).map_err(|e| ToolError::new(ErrorCode::Failed, e.to_string()))
}

async fn run<D: Dispatch>(
    dispatch: &D,
    name: &str,
    arguments: Map<String, Value>,
    progress: Option<Progress<'_>>,
) -> Result<Value, ToolError> {
    let mut res = Resolver::new(dispatch);
    match name {
        "project_status" => {
            let a: ProjectStatusArgs = args(arguments)?;
            let timeout = a.timeout_ms.unwrap_or(0);
            let status = ops::project_status(dispatch, a.project.as_deref(), a.since, timeout);
            json(&view::projects::status(&with_progress(status, progress).await?))
        }
        "task_get" => {
            let a: TaskGetArgs = args(arguments)?;
            let task = task_text(a.task.as_ref());
            let node = ops::task_get(dispatch, a.project.as_deref(), task.as_deref()).await?;
            json(&view::projects::node(&node))
        }
        "task_start" => {
            let a: TaskStartArgs = args(arguments)?;
            let (project, which, launch, key) = a.start()?;
            let key = checked_key(key)?;
            let started = ops::task_start(&mut res, project.as_deref(), which, launch, key);
            json(&view::projects::task(&started.await?))
        }
        "task_update" => {
            let a: TaskUpdateArgs = args(arguments)?;
            let change = a.change()?;
            let key = checked_key(a.idempotency_key)?;
            let task = task_text(a.task.as_ref());
            let (project, task) = (a.project.as_deref(), task.as_deref());
            let updated = ops::task_update(&res, project, task, change, key);
            json(&view::projects::task(&updated.await?))
        }
        "task_report" => {
            let a: TaskReportArgs = args(arguments)?;
            let key = checked_key(a.idempotency_key.clone())?;
            let task = task_text(a.task.as_ref());
            let reported =
                ops::task_report(dispatch, a.project.as_deref(), task.as_deref(), a.report(), key);
            json(&view::projects::task(&reported.await?))
        }
        "task_tell" => {
            let a: TaskTellArgs = args(arguments)?;
            let key = checked_key(a.idempotency_key)?;
            let task = a.task.text();
            ops::task_tell(dispatch, a.project.as_deref(), Some(&task), a.text, key).await?;
            json(&view::DONE)
        }
        "task_wait" => {
            let a: TaskWaitArgs = args(arguments)?;
            let tasks: Vec<String> = a.tasks.iter().map(TaskArg::text).collect();
            let all = matches!(a.until.unwrap_or_default(), UntilArg::All);
            let timeout = a.timeout_ms.unwrap_or(ops::DEFAULT_TASK_WAIT_MS);
            let waited =
                ops::task_wait(dispatch, a.project.as_deref(), (&tasks, all), a.since, timeout);
            json(&view::projects::task_wait(&with_progress(waited, progress).await?))
        }
        "read_thread" => {
            let a: ReadThreadArgs = args(arguments)?;
            let task = task_text(a.task.as_ref());
            let of = ops::ThreadArg {
                thread: a.thread.as_deref(),
                term: a.term.as_deref(),
                project: a.project.as_deref(),
                task: task.as_deref(),
            };
            let read = ops::read_thread(&mut res, of, a.view.view(), a.after).await?;
            json(&view::thread_read(&read))
        }
        other => Err(ToolError::invalid(format!("no tool is called {other}"))),
    }
}

/// Run `wait`, telling `progress` every [`PROGRESS_EVERY`] how long it has waited, so a client
/// that counts silence as a hang does not give up on it.
async fn with_progress<T>(wait: impl Future<Output = T>, progress: Option<Progress<'_>>) -> T {
    let Some(report) = progress else { return wait.await };
    let started = tokio::time::Instant::now();
    let mut ticks = tokio::time::interval(PROGRESS_EVERY);
    ticks.tick().await;
    tokio::pin!(wait);
    loop {
        tokio::select! {
            done = &mut wait => return done,
            _ = ticks.tick() => report(started.elapsed()),
        }
    }
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;
    use serde_json::json;
    use slopty_core::{SessionId, WallMs, WorkerId};
    use slopty_proto::orchestration::{Outcome, TermRef, ThreadOf, ThreadRead, Verb};
    use slopty_proto::project::{
        Assignment, Bounds, Limits, Live, NativeCounts, Natives, Project, ProjectId, ProjectStatus,
        Runner, Task, TaskId, TaskState, TimelineEntry,
    };
    use slopty_proto::server::{Liveness, Os, WorkerCaps, WorkerInfo};
    use slopty_proto::terminal::{SessionState, SessionSummary};
    use slopty_proto::thread::AgentId;

    use super::*;

    fn studio() -> WorkerId {
        "0199a000-0000-7000-8000-000000000001".parse().unwrap()
    }

    fn shell() -> SessionId {
        "0199a1b1-c3d4-7000-8000-00000000abcd".parse().unwrap()
    }

    /// One online worker with one shell; records every verb and the keys that came with them;
    /// a project read that may wait takes 25 s.
    #[derive(Default)]
    struct Fake {
        verbs: Mutex<Vec<Verb>>,
        keys: Mutex<Vec<Option<IdempotencyKey>>>,
        /// The caller's own project and task.
        scope: crate::Scope,
    }

    /// Task `id` as the fake's server makes it, its agent in the fake's shell once started.
    fn made_task(id: u32, title: &str, started: bool) -> Task {
        Task {
            checks: None,
            spent: slopty_proto::project::Spent::default(),
            id: TaskId(id),
            depends_on: Vec::new(),
            kind: String::new(),
            title: title.to_owned(),
            brief: String::new(),
            read_only: false,
            pin: None,
            verifier: None,
            metadata: None,
            state: if started { TaskState::Running } else { TaskState::Planned },
            status: None,
            assignment: started.then(|| Assignment {
                term: TermRef { worker: studio(), session: shell() },
                thread: None,
                since_ms: WallMs::ZERO,
                ended_ms: None,
                conversation: None,
                spawned: true,
            }),
            branch: None,
            worktree: None,
            base: None,
            pr: None,
            verified: None,
            merge: None,
            created_ms: WallMs::ZERO,
            updated_ms: WallMs::ZERO,
            step: None,
            give_backs: slopty_proto::project::GiveBacks::default(),
            tests: None,
        }
    }

    /// The fake's project: task 3 planned, task 5 with its agent in the fake's shell.
    fn project_status(id: ProjectId) -> ProjectStatus {
        ProjectStatus {
            project: Project {
                scripts: Vec::new(),
                orchestrator_spent: slopty_proto::project::Spent::default(),
                id,
                title: "Slopty".to_owned(),
                repo: "~/slopty".to_owned(),
                repo_id: None,
                target: "main".to_owned(),
                verifier: None,
                push: false,
                orchestrator: None,
                limits: Limits::default(),
                metadata: None,
                created_ms: WallMs::ZERO,
                members: Vec::new(),
            },
            tasks: [made_task(3, "Plan", false), made_task(5, "Review", true)]
                .iter()
                .map(|t| t.card(&Natives::default()))
                .collect(),
            orchestrator_natives: NativeCounts::default(),
            timeline: Vec::new(),
            next: 0,
            bounds: Bounds::default(),
            live: Live::default(),
        }
    }

    impl Fake {
        fn verbs(&self) -> Vec<Verb> {
            self.verbs.lock().clone()
        }
    }

    impl Dispatch for Fake {
        async fn send(&self, key: Option<IdempotencyKey>, verb: Verb) -> Outcome {
            self.verbs.lock().push(verb.clone());
            self.keys.lock().push(key);
            match verb {
                Verb::ListWorkers => Outcome::Workers(vec![WorkerInfo {
                    worker: studio(),
                    name: "mac-studio".to_owned(),
                    address: "127.0.0.1:45550".to_owned(),
                    liveness: Liveness::Online,
                    caps: WorkerCaps {
                        os: Os::MacOs,
                        os_version: "26.5".to_owned(),
                        arch: "aarch64".to_owned(),
                        form: slopty_proto::server::Form::Desktop,
                        cpus: 8,
                        memory: 1,
                        encoders: Vec::new(),
                        displays: Vec::new(),
                        agents: Vec::new(),
                        can_capture: false,
                        can_inject: false,
                        virtual_displays: false,
                        version: "0".to_owned(),
                        lan: Vec::new(),
                        wake_on_lan: None,
                        writes_failing: None,
                        stops_at_logout: None,
                    },
                    load: 0.0,
                    last_seen_ms: WallMs::ZERO,
                }]),
                Verb::ListTerminals { .. } => Outcome::Terminals {
                    agents: Vec::new(),
                    terminals: vec![(
                        studio(),
                        SessionSummary {
                            id: shell(),
                            title: "zsh".to_owned(),
                            cwd: None,
                            repo: None,
                            branch: None,
                            changes: None,
                            started_ms: WallMs::ZERO,
                            cols: 80,
                            rows: 24,
                            state: SessionState::Running,
                            viewers: 0,
                            command: Vec::new(),
                            progress: None,
                            restored: None,
                            repo_id: None,
                        },
                    )],
                },
                Verb::ReadThread { of, after, .. } => {
                    let thread = match of {
                        ThreadOf::Thread(thread) => thread,
                        _ => slopty_proto::thread::ThreadId::derived(&["task"]),
                    };
                    Outcome::Thread(Box::new(ThreadRead {
                        worker: studio(),
                        thread,
                        agent: AgentId::named(AgentId::PI),
                        title: "Fix the build".to_owned(),
                        parent: None,
                        phase: slopty_proto::thread::Phase::Working,
                        wait: None,
                        turns: Vec::new(),
                        requests: Vec::new(),
                        next: after.unwrap_or(slopty_proto::thread::TurnId(0)),
                        truncated: false,
                        skipped: false,
                    }))
                }
                Verb::TaskCreate { spec, .. } => {
                    Outcome::Task(Box::new(made_task(7, &spec.title, false)))
                }
                Verb::TaskUpdate { task, .. } => {
                    Outcome::Task(Box::new(made_task(task.0, "Review", true)))
                }
                Verb::ProjectStatus { project, timeout_ms, .. } => {
                    if timeout_ms > 0 {
                        tokio::time::sleep(Duration::from_secs(25)).await;
                    }
                    Outcome::Project(Box::new(project_status(project)))
                }
                Verb::TaskTell { task: Some(TaskId(9)), .. } => Outcome::Error {
                    code: ErrorCode::UnknownTask,
                    message: "no task #9".to_owned(),
                },
                Verb::TaskSpawn { task, .. } => {
                    Outcome::Task(Box::new(made_task(task.0, "Fix the hub", true)))
                }
                // The server's record of the terminal, over what its environment says.
                Verb::WorkingOn { .. } => {
                    Outcome::WorkingOn(Some(("slopty".parse().expect("a name"), Some(TaskId(5)))))
                }
                _ => Outcome::Done,
            }
        }

        fn scope(&self) -> crate::Scope {
            self.scope.clone()
        }
    }

    /// The orchestrator's own project is what the project tools default to: `task_start` makes a
    /// task with the kind, dependencies and metadata it gave and starts it in one call on the
    /// worker named, its brief the agent's first prompt; and it starts a task made before with
    /// any agent or a command. A caller that runs for no project is told to name one.
    #[tokio::test]
    async fn task_start_makes_and_starts_a_task_in_the_caller_s_own_project() {
        let scope =
            crate::Scope { session: None, project: Some("slopty".parse().unwrap()), task: None };
        let fake = Fake { scope, ..Fake::default() };
        let made = json!({
            "title": "Hub",
            "brief": "Fix the hub\nThen test it",
            "kind": "build",
            "depends_on": [2],
            "metadata": { "ticket": 12 },
            "worker": studio().to_string(),
            "cwd": "~/w",
        });
        let (failed, text) = call_json(&fake, "task_start", made).await;
        assert!(!failed, "{text}");
        let verbs = fake.verbs();
        let [Verb::TaskCreate { project, spec }, Verb::TaskSpawn { task, launch, .. }] =
            verbs.as_slice()
        else {
            panic!("{verbs:?}")
        };
        assert_eq!(project.as_str(), "slopty");
        assert_eq!((spec.kind.as_str(), spec.depends_on.as_slice()), ("build", &[TaskId(2)][..]));
        assert_eq!(spec.metadata.as_deref(), Some(r#"{"ticket":12}"#));
        assert_eq!((*task, launch.pin, launch.cwd.as_str()), (TaskId(7), Some(studio()), "~/w"));
        let brief = Some("Fix the hub\nThen test it".to_owned());
        assert_eq!(launch.run, Runner::Claude { prompt: brief, args: Vec::new() }, "its brief");

        let command = json!({"task": 7, "command": ["cargo", "test"]});
        let (failed, text) = call_json(&fake, "task_start", command).await;
        assert!(!failed, "{text}");
        let Some(Verb::TaskSpawn { launch, .. }) = fake.verbs().pop() else { panic!() };
        let argv = vec!["cargo".to_owned(), "test".to_owned()];
        assert_eq!(launch.run, Runner::Command { argv });

        let codex = json!({"task": 7, "agent": "codex", "prompt": "Go", "args": ["-m", "o3"]});
        let (failed, text) = call_json(&fake, "task_start", codex).await;
        assert!(!failed, "{text}");
        let Some(Verb::TaskSpawn { launch, .. }) = fake.verbs().pop() else { panic!() };
        let args = vec!["-m".to_owned(), "o3".to_owned()];
        assert_eq!(launch.run, Runner::Codex { prompt: Some("Go".to_owned()), args });
        let mixed = json!({"task": 7, "agent": "codex", "command": ["codex"]});
        let (failed, text) = call_json(&fake, "task_start", mixed).await;
        assert!(failed && text.contains("an agent's"), "{text}");
        for (named, agent) in [
            ("pi", AgentId::named(AgentId::PI)),
            ("acp:gemini", AgentId::acp("gemini")),
            ("gemini", AgentId::acp("gemini")),
        ] {
            let thread = json!({"task": 7, "agent": named, "prompt": "Go", "model": "flash"});
            let (failed, text) = call_json(&fake, "task_start", thread).await;
            assert!(!failed, "{text}");
            let Some(Verb::TaskSpawn { launch, .. }) = fake.verbs().pop() else { panic!() };
            let run = Runner::Agent {
                agent,
                prompt: Some("Go".to_owned()),
                model: Some("flash".to_owned()),
                args: Vec::new(),
            };
            assert_eq!(launch.run, run, "{named}");
        }
        let claude = json!({"task": 7, "prompt": "Go", "model": "opus"});
        let (failed, text) = call_json(&fake, "task_start", claude).await;
        assert!(!failed, "{text}");
        let Some(Verb::TaskSpawn { launch, .. }) = fake.verbs().pop() else { panic!() };
        let args = vec!["--model".to_owned(), "opus".to_owned()];
        assert_eq!(launch.run, Runner::Claude { prompt: Some("Go".to_owned()), args });

        let both = json!({"task": 7, "title": "Again"});
        let (failed, text) = call_json(&fake, "task_start", both).await;
        assert!(failed, "a task or a new one, not both: {text}");
        let (failed, text) = call_json(&fake, "task_start", json!({})).await;
        assert!(failed, "a task or a new one: {text}");

        let bad = call_json(&fake, "task_start", json!({"title": "x", "os": "linux"})).await;
        assert!(bad.0 && bad.1.contains("unknown field `os`"), "{bad:?}");
        let (failed, text) = call_json(&Fake::default(), "task_start", json!({"title": "x"})).await;
        assert!(failed && text.contains("name the project"), "{text}");
    }

    /// The server's record of the caller's terminal says which task is its own, over the
    /// `SLOPTY_TASK` it was started with: an agent put on another task acts on that one.
    #[tokio::test]
    async fn the_server_s_record_of_the_caller_s_terminal_names_its_task() {
        let scope = crate::Scope {
            session: Some(shell()),
            project: Some("slopty".parse().unwrap()),
            task: Some(TaskId(3)),
        };
        let fake = Fake { scope, ..Fake::default() };
        let (failed, text) = call_json(&fake, "task_update", json!({"status": "reading"})).await;
        assert!(!failed, "{text}");
        let Some(Verb::TaskUpdate { task, change, .. }) = fake.verbs().pop() else {
            panic!("{:?}", fake.verbs())
        };
        assert_eq!((task, change.status.as_deref()), (TaskId(5), Some("reading")));

        let (failed, text) =
            call_json(&fake, "task_update", json!({"project": "other", "status": "x"})).await;
        assert!(failed && text.contains("task"), "another project has no own task: {text}");
    }

    async fn call_json(fake: &Fake, name: &str, arguments: Value) -> (bool, String) {
        let Value::Object(arguments) = arguments else { panic!("an object") };
        let result = call(fake, name, arguments, None).await.unwrap();
        let text = result.content[0].as_text().unwrap().text.clone();
        (result.is_error == Some(true), text)
    }

    /// The tools are the project's eight and nothing else: the rest is the `slopty` command.
    #[test]
    fn every_tool_has_a_schema_a_description_and_a_hint() {
        let tools = list();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(
            names,
            [
                "project_status",
                "task_get",
                "task_start",
                "task_update",
                "task_report",
                "task_tell",
                "task_wait",
                "read_thread",
            ]
        );
        for t in &tools {
            assert_eq!(t.input_schema.get("type"), Some(&json!("object")), "{}", t.name);
            assert!(t.description.as_ref().is_some_and(|d| d.len() > 40), "{}", t.name);
            assert!(t.annotations.as_ref().is_some_and(|a| a.read_only_hint.is_some()));
        }
        let tell = get("task_tell").unwrap();
        assert_eq!(tell.input_schema["required"], json!(["task", "text"]));
        for name in names {
            assert!(INSTRUCTIONS.contains(name), "the instructions name {name}");
        }
    }

    #[tokio::test]
    async fn an_unknown_tool_is_invalid_params_and_a_failure_is_the_models_to_read() {
        let scope =
            crate::Scope { project: Some("slopty".parse().unwrap()), ..crate::Scope::default() };
        let fake = Fake { scope, ..Fake::default() };
        let err = call(&fake, "list_workers", Map::new(), None).await.unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS, "the CLI's now");

        let told = json!({ "task": 9, "text": "Hello." });
        let (failed, text) = call_json(&fake, "task_tell", told).await;
        assert!(failed);
        assert_eq!(text, "no task #9 (UnknownTask)");

        let (failed, text) = call_json(&fake, "task_get", json!({ "task": 3, "y": 1 })).await;
        assert!(failed);
        assert!(text.starts_with("bad arguments: unknown field `y`"), "{text}");
    }

    /// A key goes with the verb it names, so a retried call is done once; a key that is no key
    /// is the model's to fix.
    #[tokio::test]
    async fn an_idempotency_key_goes_with_its_verb() {
        let scope = crate::Scope {
            project: Some("slopty".parse().unwrap()),
            task: Some(TaskId(5)),
            ..crate::Scope::default()
        };
        let fake = Fake { scope, ..Fake::default() };
        let args = json!({ "state": "done", "idempotency_key": "step-3" });
        let (failed, text) = call_json(&fake, "task_update", args).await;
        assert!(!failed, "{text}");
        let sent = fake.keys.lock().last().cloned().flatten();
        assert_eq!(sent, Some(IdempotencyKey::new("step-3").unwrap()));
        let args = json!({ "note": "x", "idempotency_key": "two words" });
        let (failed, text) = call_json(&fake, "task_report", args).await;
        assert!(failed && text.contains("idempotency key"), "{text}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_long_wait_reports_progress_every_ten_seconds() {
        let fake = Fake::default();
        let reported = Mutex::new(Vec::new());
        let report = |waited: Duration| reported.lock().push(waited);
        let Value::Object(args) = json!({ "project": "slopty", "timeout_ms": 30_000 }) else {
            panic!()
        };
        let result = call(&fake, "project_status", args, Some(&report)).await.unwrap();
        assert_ne!(result.is_error, Some(true), "{result:?}");
        let every = PROGRESS_EVERY;
        assert_eq!(*reported.lock(), [every, every.saturating_mul(2)], "at 10 s and 20 s of 25");
        let Some(Verb::ProjectStatus { timeout_ms, .. }) = fake.verbs().pop() else { panic!() };
        assert_eq!(timeout_ms, 30_000, "passed through; the server caps it");
    }

    /// A thread is read by a task, a thread's id or a terminal, in the view asked and from
    /// the turn asked, and the server finds where it is; nothing names none. Answering a
    /// request is the person's, so no tool does it.
    #[tokio::test]
    async fn a_thread_is_read_by_task_thread_or_term_and_answering_is_no_tool() {
        let fake = Fake::default();
        let child = slopty_proto::thread::ThreadId::derived(&["child"]);
        let read = json!({ "thread": child.to_string(), "view": "activity", "after": 4 });
        let (failed, text) = call_json(&fake, "read_thread", read).await;
        assert!(!failed, "{text}");
        let page: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            (page["thread"].clone(), page["next"].clone()),
            (json!(child.to_string()), json!(4))
        );
        let asked = Verb::ReadThread {
            of: ThreadOf::Thread(child),
            view: ThreadView::Activity,
            after: Some(slopty_proto::thread::TurnId(4)),
            hold: false,
        };
        assert_eq!(fake.verbs().pop(), Some(asked));

        let term = format!("{}/{}", studio(), shell());
        call_json(&fake, "read_thread", json!({ "term": term })).await;
        let t = TermRef { worker: studio(), session: shell() };
        let Some(Verb::ReadThread { of, view, after: None, .. }) = fake.verbs().pop() else {
            panic!()
        };
        assert_eq!((of, view), (ThreadOf::Term(t), ThreadView::Messages));
        call_json(&fake, "read_thread", json!({ "project": "slopty", "task": 3 })).await;
        let Some(Verb::ReadThread { of: ThreadOf::Task { task, .. }, .. }) = fake.verbs().pop()
        else {
            panic!()
        };
        assert_eq!(task, TaskId(3));
        let (failed, text) = call_json(&fake, "read_thread", json!({})).await;
        assert!(failed && text.contains("name a task, a thread or a terminal"), "{text}");
        let (failed, text) = call_json(&fake, "read_thread", json!({ "thread": "t1" })).await;
        assert!(failed && text.contains("not a thread id"), "{text}");

        let Value::Object(args) = json!({ "thread": child.to_string() }) else { panic!() };
        let unknown = call(&fake, "answer_request", args, None).await;
        assert!(unknown.is_err(), "the person answers requests, never an agent");
    }

    /// An agent tells a task by its number, its words going as they are; a task must be named,
    /// since an agent never tells itself.
    #[tokio::test]
    async fn task_tell_names_its_task_and_carries_the_words() {
        let scope =
            crate::Scope { project: Some("slopty".parse().unwrap()), ..crate::Scope::default() };
        let fake = Fake { scope, ..Fake::default() };
        let (failed, text) =
            call_json(&fake, "task_tell", json!({"task": "#7", "text": "Cover the iPad."})).await;
        assert!(!failed, "{text}");
        let told = Verb::TaskTell {
            project: "slopty".parse().unwrap(),
            task: Some(TaskId(7)),
            text: "Cover the iPad.".to_owned(),
        };
        assert_eq!(fake.verbs().pop(), Some(told));
        let (failed, text) = call_json(&fake, "task_tell", json!({"text": "Hello."})).await;
        assert!(failed && text.contains("task"), "a task is named: {text}");
    }

    /// A project's timeline as a server answers it, read after read; with nothing left it
    /// waits out the read's time and answers nothing new, as the server does.
    struct Timeline {
        reads: Mutex<std::collections::VecDeque<ProjectStatus>>,
        last: Mutex<Option<ProjectStatus>>,
        asked: Mutex<Vec<Verb>>,
    }

    impl Timeline {
        fn new(reads: Vec<ProjectStatus>) -> Self {
            Self {
                reads: Mutex::new(reads.into()),
                last: Mutex::new(None),
                asked: Mutex::default(),
            }
        }
    }

    impl Dispatch for Timeline {
        async fn send(&self, _key: Option<IdempotencyKey>, verb: Verb) -> Outcome {
            self.asked.lock().push(verb.clone());
            let Verb::ProjectStatus { timeout_ms, .. } = verb else { return Outcome::Done };
            let next = self.reads.lock().pop_front();
            let status = if let Some(status) = next {
                status
            } else {
                tokio::time::sleep(Duration::from_millis(u64::from(timeout_ms))).await;
                let last = self.last.lock().clone().expect("read once");
                ProjectStatus { timeline: Vec::new(), ..last }
            };
            *self.last.lock() = Some(status.clone());
            Outcome::Project(Box::new(status))
        }

        fn scope(&self) -> crate::Scope {
            crate::Scope { project: Some("slopty".parse().unwrap()), ..crate::Scope::default() }
        }
    }

    fn at(seq: u64, task: u32, what: slopty_proto::project::Moment) -> TimelineEntry {
        TimelineEntry { seq, at_ms: WallMs::ZERO, task: Some(TaskId(task)), what }
    }

    fn read(next: u64, timeline: Vec<TimelineEntry>) -> ProjectStatus {
        ProjectStatus { timeline, next, ..project_status("slopty".parse().unwrap()) }
    }

    /// `task_wait` follows the timeline from now with the server's own long wait: what came
    /// before is the past (only its latest report is kept), a delivery or a note is no news, and
    /// a turn ended or a report is. With `all` it waits for each; out of time it says so, asks
    /// nothing to stop, and gives the cursor to go on from. A task merged is ready at once.
    #[tokio::test(start_paused = true)]
    async fn task_wait_waits_for_news_and_cancels_nothing() {
        use slopty_proto::project::{Moment, Report};
        let report = |note: &str| Moment::Reported {
            report: Report { note: note.to_owned(), artifacts: Vec::new(), branch: None, pr: None },
        };
        let rested = Moment::State { from: TaskState::Running, to: TaskState::Waiting };
        let quiet = Moment::Note { text: "noted".to_owned() };
        let timeline = Timeline::new(vec![
            read(10, vec![at(9, 5, report("half way"))]),
            read(12, vec![at(10, 5, quiet.clone()), at(11, 3, quiet)]),
            read(13, vec![at(12, 5, rested.clone())]),
        ]);
        let tasks = ["5".to_owned(), "#3".to_owned()];
        let waited = ops::task_wait(&timeline, None, (&tasks, false), None, 60_000).await.unwrap();
        assert_eq!(
            (waited.ready.as_slice(), waited.timed_out, waited.next),
            (&[TaskId(5)][..], false, 13)
        );
        assert_eq!(waited.news, [at(12, 5, rested)]);
        let view = serde_json::to_value(view::projects::task_wait(&waited)).unwrap();
        assert_eq!(view["tasks"][0]["last_report"]["text"], json!("done: half way"), "{view}");
        let sinces: Vec<Option<u64>> = timeline
            .asked
            .lock()
            .iter()
            .map(|v| match v {
                Verb::ProjectStatus { since, .. } => *since,
                other => panic!("only reads: {other:?}"),
            })
            .collect();
        assert_eq!(sinces, [None, Some(10), Some(12)]);

        let timeline = Timeline::new(vec![read(13, Vec::new())]);
        let started = tokio::time::Instant::now();
        let waited =
            ops::task_wait(&timeline, None, (&tasks, true), Some(13), 90_000).await.unwrap();
        assert!(waited.timed_out && waited.ready.is_empty() && waited.next == 13);
        assert_eq!(started.elapsed(), Duration::from_secs(90), "the whole wait, no more");
        assert!(
            timeline.asked.lock().iter().all(|v| matches!(v, Verb::ProjectStatus { .. })),
            "nothing is stopped or cancelled"
        );

        let mut merged = read(20, Vec::new());
        if let Some(card) = merged.tasks.iter_mut().find(|c| c.id == TaskId(5)) {
            card.state = TaskState::Merged;
        }
        let timeline = Timeline::new(vec![merged]);
        let waited =
            ops::task_wait(&timeline, None, (&tasks[..1], true), None, 60_000).await.unwrap();
        assert_eq!((waited.ready.as_slice(), waited.timed_out), (&[TaskId(5)][..], false));
        let unknown = ["9".to_owned()];
        let timeline = Timeline::new(vec![read(20, Vec::new())]);
        let refused =
            ops::task_wait(&timeline, None, (&unknown, false), None, 0).await.unwrap_err();
        assert_eq!(refused.code, ErrorCode::UnknownTask);
    }
}
