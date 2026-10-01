//! The verbs as MCP tools: names, descriptions, hints, argument schemas, and a call run end to
//! end (arguments parsed, names resolved, the verb sent, the answer rendered as its view).
//!
//! Every MCP surface serves exactly this list and this [`call`], so a model sees the same tools
//! and gets the same JSON whether it reaches the server's endpoint or `slopty mcp`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use rmcp::ErrorData;
use rmcp::handler::server::common::{schema_for_empty_input, schema_for_type};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool, ToolAnnotations};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use slopty_core::{DisplayId, WindowId};
use slopty_proto::items::ItemKind;
use slopty_proto::orchestration::{ErrorCode, EventFilter, IdempotencyKey, Input, Size, WaitUntil};
use slopty_proto::project::{
    LimitsChange, Preference, Report, ReportKind, Runner, TaskChange, TaskId,
};
use slopty_proto::screen::CaptureTarget;
use slopty_proto::search::SearchQuery;

use crate::ops::{
    self, AgentSpec, DEFAULT_MAX_ENTRIES, DEFAULT_MAX_ENTRIES_PAGE, DEFAULT_MAX_LINES,
    DEFAULT_MAX_MATCHES, DEFAULT_WAIT_MS, LaunchSpec, NewTask, PlacementSpec, ProjectEdit,
    ProjectSpec, Spec,
};
use crate::resolve::Resolver;
use crate::view::{self, Encoding};
use crate::{Dispatch, ToolError, bulk};

/// What the model reads before any tool description.
pub const INSTRUCTIONS: &str = "\
Slopty runs terminals and coding agents on a fleet of machines (workers). Start with \
list_workers, then list_terminals. A terminal is named by its `term` (worker/session, as the \
lists print it); copy it verbatim into the terminal tools. A `worker` argument takes a worker's \
name or id. To run a command: send_input text \"cmd\\n\", then wait_for command_done, then \
read_output from the line you started at. Prefer wait_for (one terminal) and events (the whole \
fleet: agents needing you, terminals opening and closing, workers coming and going) to polling \
read_screen or read_output in a loop. To work with another coding agent, read_conversation; its \
permission prompts are the person's to answer, never an agent's, so never type its menu's digits. \
For a \
goal bigger than one agent, make a project (project_create) and split it into tasks \
(task_create): each owns the paths it writes (or only reads), may depend on others and nest to \
any depth the project allows, and says where it may run as CEL rules over the workers' facts \
(list_workers shows them; placement_suggest ranks the workers with reasons) or pins a worker \
outright. Start what runs for a task, Claude Code or any command, with task_spawn, and follow \
the tree with project_status (since and timeout_ms wait for news; bounds and live say how much \
room there is). Agents started for a task have these tools too, their project and task the \
defaults.";

/// How often a `wait_for` with a progress sink reports that it is still waiting.
pub const PROGRESS_EVERY: Duration = Duration::from_secs(10);

/// Told how long a `wait_for` has waited so far, every [`PROGRESS_EVERY`].
pub type Progress<'a> = &'a (dyn Fn(Duration) + Send + Sync);

/// A worker, or the only one online.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WorkerArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
}

/// `list_terminals`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListTerminalsArgs {
    /// Only this worker (name or id); every worker when omitted.
    worker: Option<String>,
}

/// `open_terminal`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpenTerminalArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
    /// Working directory, absolute or `~/…`; the worker's home when omitted.
    cwd: Option<String>,
    /// Program and arguments, e.g. `["npm", "run", "dev"]`; the login shell when omitted.
    #[serde(default)]
    command: Vec<String>,
    /// Extra environment variables.
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// A short name for the terminal's tile on the workspace.
    name: Option<String>,
    /// Columns (10-1000); with `rows`. 120x36 when omitted, until a client shows it.
    cols: Option<u16>,
    /// Rows (2-500); with `cols`.
    rows: Option<u16>,
    /// A name for this call's effect, such as a fresh UUID. Sent again with the same key (a
    /// retry after a timeout or a dropped connection), the call answers what the first did
    /// instead of doing it twice; the same key with other arguments is an error.
    idempotency_key: Option<String>,
}

/// `spawn_agent`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SpawnAgentArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
    /// Working directory, usually a repository root.
    cwd: String,
    /// The first prompt, typed once the agent is ready.
    prompt: Option<String>,
    /// Arguments for `claude`, e.g. `["--model", "opus"]`.
    #[serde(default)]
    args: Vec<String>,
    /// Extra environment variables.
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// Columns (10-1000); with `rows`. 120x36 when omitted, until a client shows it.
    cols: Option<u16>,
    /// Rows (2-500); with `cols`.
    rows: Option<u16>,
    /// Start it for a project's task, placed by the server and tracked in the project's tree:
    /// the project's name. The caller's own project when `task` or `parent` is given alone.
    project: Option<String>,
    /// With `project`: the task it works on. A new task (titled by the prompt's first line,
    /// briefed with the prompt) when omitted.
    task: Option<TaskArg>,
    /// With `project` and no `task`: the new task's parent; the caller's own task when omitted.
    parent: Option<TaskArg>,
    /// A name for this call's effect, such as a fresh UUID. Sent again with the same key (a
    /// retry after a timeout or a dropped connection), the call answers what the first did
    /// instead of doing it twice; the same key with other arguments is an error.
    idempotency_key: Option<String>,
}

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

/// A task's placement: where it may run and where it had better.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PlacementArgs {
    /// This worker (name or id) and no other, whatever the rules say.
    pin: Option<String>,
    /// CEL rules over a worker's facts that must all hold, such as `os == "linux" && cpus >=
    /// 16`, `has(probes.cuda)`, `"wasm32-unknown-unknown" in rust_targets`,
    /// `labels["fast-disk"]`. `list_workers` shows every worker's `facts`.
    #[serde(default)]
    require: Vec<String>,
    /// CEL rules that score a worker: each that holds adds its `weight` (1 when omitted); a
    /// rule giving a number adds weight times it (`{"expr": "-load", "weight": 5}`).
    #[serde(default)]
    prefer: Vec<PreferArgs>,
    /// Run beside these: tasks (`#3`) or workers (name or id).
    #[serde(default)]
    near: Vec<String>,
    /// Keep away from these: tasks (`#3`) or workers.
    #[serde(default)]
    avoid: Vec<String>,
}

/// One preference.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PreferArgs {
    /// A CEL rule over a worker's facts.
    expr: String,
    /// Points when it holds; negative steers away. 1 when omitted.
    weight: Option<i32>,
}

impl PlacementArgs {
    fn spec(self) -> PlacementSpec {
        let prefer = self
            .prefer
            .into_iter()
            .map(|p| Preference { expr: p.expr, weight: p.weight.unwrap_or(1) })
            .collect();
        PlacementSpec {
            pin: self.pin,
            require: self.require,
            prefer,
            near: self.near,
            avoid: self.avoid,
        }
    }
}

/// A project's limits, within the bounds the person set (`project_status` shows both).
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LimitsArgs {
    /// Most of its live agents on one worker.
    live_per_worker: Option<u16>,
    /// Most of its live agents in all.
    live_per_project: Option<u16>,
    /// How deep its tree of tasks may go.
    depth: Option<u16>,
    /// How many timeline entries it keeps.
    timeline_kept: Option<u32>,
}

impl From<LimitsArgs> for LimitsChange {
    fn from(a: LimitsArgs) -> Self {
        Self {
            live_per_worker: a.live_per_worker,
            live_per_project: a.live_per_project,
            depth: a.depth,
            timeline_kept: a.timeline_kept,
        }
    }
}

/// A JSON object as the text the server keeps.
fn metadata_text(doc: Option<Map<String, Value>>) -> Result<Option<String>, ToolError> {
    doc.map(|d| serde_json::to_string(&d).map_err(|e| ToolError::invalid(e.to_string())))
        .transpose()
}

fn task_numbers(tasks: &[TaskArg]) -> Result<Vec<TaskId>, ToolError> {
    tasks.iter().map(|t| ops::task_number(&t.text())).collect()
}

/// `project_create`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProjectCreateArgs {
    /// Its name: 1-40 lowercase letters, digits and dashes (`slopty`, `net-rewrite`).
    project: String,
    /// What it is for, in a line.
    title: String,
    /// The repository its tasks work in: a path on the workers, or a URL.
    repo: String,
    /// The branch finished work lands on; `main` when omitted.
    target: Option<String>,
    /// The command that says a task's work is right, such as `cargo gate`.
    verifier: Option<String>,
    /// The orchestrator's terminal (`worker/session`); yours when omitted and you run in one.
    orchestrator: Option<String>,
    /// Its limits over the defaults, within the person's bounds.
    #[serde(default)]
    limits: LimitsArgs,
    /// Anything to keep with it, as a JSON object.
    metadata: Option<Map<String, Value>>,
    /// A name for this call's effect, such as a fresh UUID; a repeat answers as the first did.
    idempotency_key: Option<String>,
}

/// `project_update`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProjectUpdateArgs {
    /// The project; yours when omitted.
    project: Option<String>,
    /// A new orchestrator terminal (`worker/session`).
    orchestrator: Option<String>,
    /// A new verifier command; empty for none.
    verifier: Option<String>,
    /// New limits, within the person's bounds.
    #[serde(default)]
    limits: LimitsArgs,
    /// New metadata, a JSON object in place of the old.
    metadata: Option<Map<String, Value>>,
    /// A name for this call's effect, such as a fresh UUID; a repeat answers as the first did.
    idempotency_key: Option<String>,
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

/// `task_create`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskCreateArgs {
    /// The project; yours when omitted.
    project: Option<String>,
    /// The task it is split from; your own task when omitted and you work on one there.
    parent: Option<TaskArg>,
    /// Tasks whose work it needs first. A dependency never leads back to it.
    #[serde(default)]
    depends_on: Vec<TaskArg>,
    /// What sort of work it is, in your words (`build`, `review`, `bench`).
    #[serde(default)]
    kind: String,
    /// What it is, in a line.
    title: String,
    /// What its agent is told to do: the goal, the constraints, how to know it is done.
    #[serde(default)]
    brief: String,
    /// Repository-relative paths it alone may write (`crates/slopty-server`, `docs/x.md`); a
    /// directory owns everything under it. Refused when one overlaps a live task's.
    #[serde(default)]
    owns: Vec<String>,
    /// It only reads: it owns nothing and overlaps nobody.
    #[serde(default)]
    read_only: bool,
    /// Where it may run; anywhere with room when omitted.
    placement: Option<PlacementArgs>,
    /// Its own verifier command, over the project's.
    verifier: Option<String>,
    /// Anything to keep with it, as a JSON object.
    metadata: Option<Map<String, Value>>,
    /// A name for this call's effect, such as a fresh UUID; a repeat answers as the first did.
    idempotency_key: Option<String>,
}

/// `task_claim`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskClaimArgs {
    /// The project; yours when omitted.
    project: Option<String>,
    /// The task; yours when omitted.
    task: Option<TaskArg>,
    /// Repository-relative paths to own besides what it owns.
    paths: Vec<String>,
    /// A name for this call's effect, such as a fresh UUID; a repeat answers as the first did.
    idempotency_key: Option<String>,
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
    /// A new placement, in place of the old.
    placement: Option<PlacementArgs>,
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
    /// `checkpoint` (progress; delivered with the next report), `needs_input` (you need an
    /// answer; delivered at once), `stuck` (you cannot go on; at once) or `done` (finished;
    /// delivered once it settles, a later report replacing it).
    kind: ReportKindArg,
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

/// `review_report`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReviewReportArgs {
    /// The project; the one you were started for when omitted.
    project: Option<String>,
    /// The task whose work you read; the one you were started for when omitted.
    task: Option<TaskArg>,
    /// Whether the work may merge: true unless a finding blocks.
    approved: bool,
    /// The review in a few lines.
    summary: String,
    /// What you found that matters, the most important first; at most 8 are kept.
    #[serde(default)]
    findings: Vec<FindingArg>,
    /// A name for this call's effect, such as a fresh UUID; a repeat answers as the first did.
    idempotency_key: Option<String>,
}

/// One finding of a review.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FindingArg {
    /// The file, relative to the repository's root.
    path: Option<String>,
    /// The line in the work's version of the file.
    line: Option<u32>,
    /// `blocker`, `should` or `nit`, or a word of your own.
    severity: String,
    /// Whether it keeps the work from merging: only a blocker does.
    #[serde(default)]
    blocking: bool,
    /// What is wrong and what to do, in a few sentences.
    body: String,
}

impl ReviewReportArgs {
    fn verdict(self) -> slopty_proto::project::ReviewVerdict {
        slopty_proto::project::ReviewVerdict {
            approved: self.approved,
            summary: self.summary,
            findings: self
                .findings
                .into_iter()
                .map(|f| slopty_proto::project::Finding {
                    path: f.path,
                    line: f.line,
                    severity: f.severity,
                    blocking: f.blocking,
                    body: f.body,
                })
                .collect(),
        }
    }
}

/// The kinds of report.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ReportKindArg {
    /// Progress worth knowing.
    Checkpoint,
    /// An answer is needed to go on.
    NeedsInput,
    /// It cannot go on.
    Stuck,
    /// It finished.
    Done,
}

impl TaskReportArgs {
    fn report(&self) -> Report {
        let kind = match self.kind {
            ReportKindArg::Checkpoint => ReportKind::Checkpoint,
            ReportKindArg::NeedsInput => ReportKind::NeedsInput,
            ReportKindArg::Stuck => ReportKind::Stuck,
            ReportKindArg::Done => ReportKind::Done,
        };
        Report {
            kind,
            note: self.note.clone(),
            artifacts: self.artifacts.clone(),
            branch: self.branch.clone(),
            pr: self.pr,
        }
    }
}

/// `task_assign`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskAssignArgs {
    /// The project; yours when omitted.
    project: Option<String>,
    /// The task.
    task: TaskArg,
    /// The terminal (`worker/session`); yours when omitted.
    term: Option<String>,
    /// A name for this call's effect, such as a fresh UUID; a repeat answers as the first did.
    idempotency_key: Option<String>,
}

/// `task_spawn`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskSpawnArgs {
    /// The project; yours when omitted.
    project: Option<String>,
    /// The task.
    task: TaskArg,
    /// This worker (name or id) over the task's placement; the server places it when omitted.
    worker: Option<String>,
    /// Working directory on the worker, usually the repository's root; home when omitted.
    cwd: Option<String>,
    /// Run this instead of Claude Code: any program and its arguments, such as another
    /// agent's CLI, a build or a benchmark; `[]` for the login shell.
    command: Option<Vec<String>>,
    /// Claude Code's first prompt, typed once it is ready; say where its brief is.
    prompt: Option<String>,
    /// Arguments for `claude`, e.g. `["--model", "opus"]`. Flags that loosen its permissions
    /// are refused unless the person allows them for the project.
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

/// `placement_suggest`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PlacementSuggestArgs {
    /// The project whose limits and tasks count; yours when omitted.
    project: Option<String>,
    /// The task whose placement to rank; yours when omitted and no `placement` is given.
    task: Option<TaskArg>,
    /// A placement to try instead of the task's.
    placement: Option<PlacementArgs>,
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
            placement: None,
            run_on: None,
            verifier: self.verifier.clone(),
            metadata: metadata_text(self.metadata.clone())?,
        })
    }
}

/// `resize_terminal`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResizeArgs {
    /// The terminal, as the lists print it (`worker/session`).
    term: String,
    /// Columns (10-1000).
    cols: u16,
    /// Rows (2-500).
    rows: u16,
    /// A name for this call's effect, such as a fresh UUID. Sent again with the same key (a
    /// retry after a timeout or a dropped connection), the call answers what the first did
    /// instead of doing it twice; the same key with other arguments is an error.
    idempotency_key: Option<String>,
}

/// `events`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EventsArgs {
    /// Cursor: the `next` of the previous call. Omitted means from now on; 0 means everything
    /// the server still holds.
    since: Option<u64>,
    /// Wait this long for a first event (default 60000; the server caps it at 240000). 0
    /// answers at once, with the cursor for now.
    timeout_ms: Option<u32>,
    /// Only agents that come to need a human or go idle, on any worker.
    #[serde(default)]
    agent_input: bool,
}

/// `send_input`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendInputArgs {
    /// The terminal, as the lists print it (`worker/session`).
    term: String,
    /// Text typed as-is; `\n` presses Enter.
    text: Option<String>,
    /// Text delivered as one paste (bracketed when the program asked for it).
    paste: Option<String>,
    /// Named keys pressed in order, each `[mods+]key`.
    keys: Option<Vec<String>>,
    /// A name for this call's effect, such as a fresh UUID. Sent again with the same key (a
    /// retry after a timeout or a dropped connection), the call answers what the first did
    /// instead of doing it twice; the same key with other arguments is an error.
    idempotency_key: Option<String>,
}

/// A terminal alone.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TermArgs {
    /// The terminal, as the lists print it (`worker/session`).
    term: String,
}

/// `close_terminal`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    /// The terminal, as the lists print it (`worker/session`).
    term: String,
    /// A name for this call's effect, such as a fresh UUID. Sent again with the same key (a
    /// retry after a timeout or a dropped connection), the call answers what the first did
    /// instead of doing it twice; the same key with other arguments is an error.
    idempotency_key: Option<String>,
}

/// `read_output`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadOutputArgs {
    /// The terminal, as the lists print it (`worker/session`).
    term: String,
    /// First absolute line index wanted: the `next` of the previous call, or a command's
    /// `output_start`. The oldest retained line when omitted.
    since: Option<u64>,
    /// At most this many lines (default 200).
    max_lines: Option<u32>,
}

/// `list_commands`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListCommandsArgs {
    /// The terminal, as the lists print it (`worker/session`).
    term: String,
    /// Only commands whose prompt is at or after this absolute line.
    since: Option<u64>,
}

/// `wait_for`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WaitForArgs {
    /// The terminal, as the lists print it (`worker/session`).
    term: String,
    /// Wait for a new line of output matching this regular expression.
    output: Option<String>,
    /// Wait for this many milliseconds without output.
    quiet_ms: Option<u32>,
    /// Wait for the running shell command to finish (or the next one, if none runs).
    #[serde(default)]
    command_done: bool,
    /// Wait for the terminal's program to exit.
    #[serde(default)]
    exit: bool,
    /// Wait for the agent in the terminal to need a human or go idle.
    #[serde(default)]
    agent_input: bool,
    /// Give up after this many milliseconds (default 60000; the server caps it at 240000).
    timeout_ms: Option<u32>,
    /// A name for this call's effect, such as a fresh UUID. Sent again with the same key (a
    /// retry after a timeout or a dropped connection), the call answers what the first did
    /// instead of doing it twice; the same key with other arguments is an error.
    idempotency_key: Option<String>,
}

/// `read_file`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
    /// Absolute path, or `~/…`.
    path: String,
    /// First byte to read (default 0).
    #[serde(default)]
    offset: u64,
    /// At most this many bytes (8 MiB at most); the rest of the file when omitted.
    length: Option<u64>,
}

/// `list_dir`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListDirArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
    /// Absolute path, or `~/…`.
    path: String,
    /// At most this many entries (default 1000, at most 10000).
    max: Option<u32>,
}

/// `stat`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
    /// Absolute path, or `~/…`.
    path: String,
}

/// `search_files`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchFilesArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
    /// The directory to search, absolute or `~/…`.
    root: String,
    /// What to look for: the text as it is, unless `regex`.
    pattern: String,
    /// The pattern is a regular expression (Rust regex syntax, as ripgrep takes it).
    #[serde(default)]
    regex: bool,
    /// Upper and lower case differ; by default they match each other.
    #[serde(default)]
    match_case: bool,
    /// A match must stand as a whole word.
    #[serde(default)]
    whole_word: bool,
    /// Which files, as ripgrep's `--glob` takes them (`*.rs`, `!tests/**`).
    #[serde(default)]
    globs: Vec<String>,
    /// Lines of context before and after each match (0-5, default 0).
    #[serde(default)]
    context: u32,
    /// At most this many matching lines (default 200, at most 2000).
    max_lines: Option<u32>,
}

impl SearchFilesArgs {
    fn query(&self) -> SearchQuery {
        SearchQuery {
            pattern: self.pattern.clone(),
            regex: self.regex,
            match_case: self.match_case,
            whole_word: self.whole_word,
            globs: self.globs.clone(),
            context: self.context,
        }
    }
}

/// `forget_worker`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ForgetWorkerArgs {
    /// Worker name or id.
    worker: String,
    /// A name for this call's effect, such as a fresh UUID. Sent again with the same key (a
    /// retry after a timeout or a dropped connection), the call answers what the first did
    /// instead of doing it twice; the same key with other arguments is an error.
    idempotency_key: Option<String>,
}

/// `wake_worker`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WakeWorkerArgs {
    /// Worker name or id.
    worker: String,
}

/// `write_file`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WriteFileArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
    /// Absolute path, or `~/…`.
    path: String,
    /// The whole new contents, spelled as `encoding` says.
    content: String,
    /// `utf8` (the default) for text as it is, `base64` for binary contents.
    #[serde(default)]
    encoding: Encoding,
    /// A name for this call's effect, such as a fresh UUID. Sent again with the same key (a
    /// retry after a timeout or a dropped connection), the call answers what the first did
    /// instead of doing it twice; the same key with other arguments is an error.
    idempotency_key: Option<String>,
}

/// `open_item`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpenItemArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
    /// A web page, `http` or `https`; a localhost address is the worker's own.
    url: Option<String>,
    /// A text file on the worker to edit, absolute.
    file: Option<String>,
    /// A note's Markdown.
    note: Option<String>,
    /// A window to stream live, by its id from `list_windows`.
    window: Option<u32>,
    /// A whole display to stream live, by its id from `list_windows`.
    display: Option<u32>,
    /// A short name for the tile.
    name: Option<String>,
    /// A name for this call's effect, such as a fresh UUID. Sent again with the same key (a
    /// retry after a timeout or a dropped connection), the call answers what the first did
    /// instead of doing it twice; the same key with other arguments is an error.
    idempotency_key: Option<String>,
}

/// An item alone.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ItemArgs {
    /// The item, as `list_items` prints it (`worker/item`).
    item: String,
    /// A name for this call's effect, such as a fresh UUID. Sent again with the same key (a
    /// retry after a timeout or a dropped connection), the call answers what the first did
    /// instead of doing it twice; the same key with other arguments is an error.
    idempotency_key: Option<String>,
}

/// `rename_item`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RenameItemArgs {
    /// The item, as `list_items` prints it (`worker/item`).
    item: String,
    /// The new name; omitted takes the name away, and the tile says what it shows.
    name: Option<String>,
    /// A name for this call's effect, such as a fresh UUID. Sent again with the same key (a
    /// retry after a timeout or a dropped connection), the call answers what the first did
    /// instead of doing it twice; the same key with other arguments is an error.
    idempotency_key: Option<String>,
}

/// `read_conversation`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadConversationArgs {
    /// The terminal the agent runs in, as the lists print it (`worker/session`).
    term: String,
    /// `main` (the default) for the agent's own conversation, or a subagent's id from
    /// `threads`.
    thread: Option<String>,
    /// First entry wanted, by its place in the thread: the `next` of the previous call. The
    /// last `max` entries when omitted.
    since: Option<u32>,
    /// At most this many entries (default 50, at most 500).
    max: Option<u32>,
}

/// `capture_still`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CaptureStillArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
    /// A window, by its id from `list_windows`.
    window: Option<u32>,
    /// A whole display, by its id from `list_windows`.
    display: Option<u32>,
}

impl CaptureStillArgs {
    fn target(&self) -> Result<CaptureTarget, ToolError> {
        match (self.window, self.display) {
            (Some(id), None) => Ok(CaptureTarget::Window(WindowId(id))),
            (None, Some(id)) => Ok(CaptureTarget::Display(DisplayId(id))),
            _ => Err(ToolError::invalid("give exactly one of window, display")),
        }
    }
}

/// `upload_file`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UploadArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
    /// The file on the machine this tool runs on, absolute.
    local: String,
    /// Where it goes on the worker: absolute, or `~/…`.
    path: String,
    /// A name for this call's effect, such as a fresh UUID. Sent again with the same key (a
    /// retry after a timeout or a dropped connection), the call answers what the first did
    /// instead of doing it twice; the same key with other arguments is an error.
    idempotency_key: Option<String>,
}

/// `download_file`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DownloadArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
    /// The file on the worker: absolute, or `~/…`.
    path: String,
    /// Where it goes on the machine this tool runs on, absolute; a directory takes the file
    /// under its own name.
    local: String,
}

impl OpenItemArgs {
    fn kind(self) -> Result<(ItemKind, Option<String>), ToolError> {
        let kinds = [
            self.url.map(|url| ItemKind::Browser { url }),
            self.file.map(|path| ItemKind::File { path }),
            self.note.map(|text| ItemKind::Note { text }),
            self.window.map(|id| ItemKind::Window { window: WindowId(id) }),
            self.display.map(|display| ItemKind::Display { display: DisplayId(display) }),
        ];
        let mut given = kinds.into_iter().flatten();
        match (given.next(), given.next()) {
            (Some(kind), None) => Ok((kind, self.name)),
            _ => Err(ToolError::invalid("give exactly one of url, file, note, window, display")),
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

impl SendInputArgs {
    fn input(self) -> Result<Input, ToolError> {
        match (self.text, self.paste, self.keys) {
            (Some(text), None, None) => Ok(Input::Text(text)),
            (None, Some(paste), None) => Ok(Input::Paste(paste)),
            (None, None, Some(keys)) => Ok(Input::Keys(keys)),
            _ => Err(ToolError::invalid("give exactly one of text, paste, keys")),
        }
    }
}

impl WaitForArgs {
    fn until(&self) -> Result<WaitUntil, ToolError> {
        let conditions = [
            self.output.as_ref().map(|p| WaitUntil::Output(p.clone())),
            self.quiet_ms.map(|ms| WaitUntil::Quiet { ms }),
            self.command_done.then_some(WaitUntil::CommandDone),
            self.exit.then_some(WaitUntil::Exit),
            self.agent_input.then_some(WaitUntil::AgentNeedsInput),
        ];
        let mut given = conditions.into_iter().flatten();
        match (given.next(), given.next()) {
            (Some(until), None) => Ok(until),
            _ => Err(ToolError::invalid(
                "give exactly one of output, quiet_ms, command_done, exit, agent_input",
            )),
        }
    }
}

/// How a tool behaves, for the client's hints.
#[derive(Clone, Copy)]
enum Kind {
    /// Reads only.
    Read,
    /// Changes something, nothing lost.
    Write,
    /// Ends or replaces something.
    Destroy,
}

fn tool_with(
    name: &'static str,
    description: &'static str,
    kind: Kind,
    schema: Arc<JsonObject>,
) -> Tool {
    let hints = ToolAnnotations::new().open_world(false);
    let hints = match kind {
        Kind::Read => hints.read_only(true),
        Kind::Write => hints.read_only(false).destructive(false),
        Kind::Destroy => hints.read_only(false).destructive(true),
    };
    Tool::new(name, description, schema).annotate(hints)
}

fn tool<T: JsonSchema + 'static>(
    name: &'static str,
    description: &'static str,
    kind: Kind,
) -> Tool {
    tool_with(name, description, kind, schema_for_type::<T>())
}

/// Every tool, in the order a model meets them.
#[must_use]
pub fn list() -> Vec<Tool> {
    vec![
        tool_with(
            "list_workers",
            "Every machine (worker) the Slopty server knows: name, liveness (online, \
             unreachable, gone), address, OS, how many terminals it runs, `waiting` (the \
             agents on it that need a human, each with its `term`), and `facts`: everything it \
             is and has (toolchains, agent CLIs, GPUs, its person's labels and probes, load, \
             live agents), by the names placement rules read. Start here.",
            Kind::Read,
            schema_for_empty_input(),
        ),
        tool::<ListTerminalsArgs>(
            "list_terminals",
            "Terminals on one worker or on all: `term` (the handle every terminal tool takes, \
             copy it verbatim), title, working directory, repository, size, whether the program \
             still runs, its command line, and the coding agent in it if one runs (as \
             `agent_status` gives it).",
            Kind::Read,
        ),
        tool::<OpenTerminalArgs>(
            "open_terminal",
            "Start a terminal on a worker, running the login shell or `command`, and return its \
             `term`. Then send_input, wait_for and read_output. `cols`/`rows` set its size; \
             a client that shows it later sizes it to its window.",
            Kind::Write,
        ),
        tool::<SpawnAgentArgs>(
            "spawn_agent",
            "Start Claude Code (`claude` plus `args`) in a new terminal in `cwd`, optionally \
             typing `prompt` once it is ready, and return its `term`. It gets Slopty's tools \
             too. With `project`, it works on a task of that project (`task`, else a new one \
             under `parent`), the server places it where the task's needs are met and the tree \
             shows it. Follow it with wait_for agent_input (this agent) or events agent_input \
             (any agent) to learn when it needs you, agent_status for what it is doing, and \
             read_screen to see it.",
            Kind::Write,
        ),
        tool::<SendInputArgs>(
            "send_input",
            "Type into a terminal; give exactly one of: `text`, typed as-is with `\\n` pressing \
             Enter (\"cargo test\\n\" runs a command); `paste`, delivered as one bracketed paste, \
             best for multi-line text into an editor, a REPL or an agent's prompt; `keys`, keys \
             pressed in order, each `[mods+]key` with mods ctrl, alt (or opt), shift, cmd and the \
             key a single character, a name (enter, tab, space, escape, backspace, delete, up, \
             down, left, right, home, end, pageup, pagedown, insert, f1…f25) or a W3C \
             KeyboardEvent.code (ArrowUp, Digit1, NumpadEnter). Examples: [\"ctrl+c\"], \
             [\"escape\", \":\", \"w\", \"q\", \"enter\"], [\"shift+tab\"], [\"up\", \"enter\"].",
            Kind::Write,
        ),
        tool::<TermArgs>(
            "read_screen",
            "The screen as drawn now: rows with their absolute line index, cursor, title, \
             working directory, and `alternate` (a full-screen program such as an editor or an \
             agent's TUI is showing). Best for TUIs and prompts; for what a command printed, \
             read_output pages through all of it.",
            Kind::Read,
        ),
        tool::<ReadOutputArgs>(
            "read_output",
            "Scrollback and screen as lines with absolute indexes, which stay put as old lines \
             are evicted. Returns `lines` and `next`: pass `next` back as `since` to read only \
             what came after, and page through long output that way instead of re-reading it. \
             A command's output starts at its `output_start` from list_commands.",
            Kind::Read,
        ),
        tool::<ListCommandsArgs>(
            "list_commands",
            "Commands run at the shell prompt (OSC 133 blocks), oldest first: the command line, \
             `exit` (null while it runs) and the output's line range [output_start, output_end) \
             for read_output. Empty when the shell has no prompt integration.",
            Kind::Read,
        ),
        tool::<WaitForArgs>(
            "wait_for",
            "Block until something happens in a terminal instead of polling read_screen or \
             read_output. Give exactly one condition: `output` (a regex matched by a new line), \
             `quiet_ms` (that long with no output), `command_done` (the running command ends), \
             `exit` (the program exits), `agent_input` (the agent needs a human or went idle). \
             `timeout_ms` defaults to 60000 and the server caps it at 240000. Returns `result`: \
             met (with the matching `line` for `output`), timed_out, or closed. On timed_out, \
             look with read_screen, then wait again.",
            Kind::Read,
        ),
        tool::<EventsArgs>(
            "events",
            "What happened across every worker, oldest first: agent status changes (`agent`, \
             with `term` and `needs_human`), terminals opened, exited (with `exit_status`) \
             and closed, workers online, \
             unreachable, gone or removed. Blocks until at least one event or `timeout_ms`. \
             Returns `events`, `next` and `missed` (events dropped from the server's log). Pass \
             `next` back as `since` to go on without gaps. With `agent_input` it waits for the \
             first agent anywhere that needs a human or goes idle. To see nothing twice, take \
             the cursor first (timeout_ms 0), then read state (list_workers), then wait from \
             the cursor.",
            Kind::Read,
        ),
        tool::<TermArgs>(
            "agent_status",
            "The coding agent in a terminal, if one runs, and its status: idle, working, tool \
             (running `tool`), blocked (needs a human, with `reason` and for a permission the \
             `tool`), or done. `source` says what the status was read from: `hook`, or \
             `transcript`, `title` or `process` when the agent's hooks are not installed.",
            Kind::Read,
        ),
        tool::<ResizeArgs>(
            "resize_terminal",
            "Resize a terminal to `cols` x `rows`, for a program that lays out to the width. \
             Fails while a client shows the terminal, since its window sets the size then.",
            Kind::Write,
        ),
        tool::<CloseArgs>(
            "close_terminal",
            "Hang up a terminal's program and remove the terminal.",
            Kind::Destroy,
        ),
        tool::<ReadFileArgs>(
            "read_file",
            "Read a file on a worker, whole or from `offset` for `length` bytes (8 MiB per \
             read). Returns `content`, its `encoding` (utf8 when the bytes are UTF-8, else \
             base64), the file's `size`, `offset`, `length` read and `more` (the file goes on: \
             read again from offset + length).",
            Kind::Read,
        ),
        tool::<WriteFileArgs>(
            "write_file",
            "Create or replace a file on a worker with `content`: text as it is, or binary \
             contents in base64 with `encoding` base64.",
            Kind::Destroy,
        ),
        tool::<ListDirArgs>(
            "list_dir",
            "A directory's entries on a worker, by name: `name`, `kind` (file, dir, symlink, \
             other), `size` and `modified_ms`, with `total` and `truncated` when there are more \
             than `max`.",
            Kind::Read,
        ),
        tool::<PathArgs>(
            "stat",
            "What is at a path on a worker, following links: `exists`, then `kind`, `size`, \
             `modified_ms` and `mode` (octal).",
            Kind::Read,
        ),
        tool::<SearchFilesArgs>(
            "search_files",
            "Search the files under a directory on a worker, as ripgrep does: .gitignore \
             honoured, binary files skipped. Returns `files` in path order, each with its \
             `lines` (`line`, `text` without indentation, `matches` as byte ranges, `context` \
             on a line round a match), then `lines`, `searched` and `capped` (there were more: \
             narrow with `globs` or a sharper pattern).",
            Kind::Read,
        ),
        tool::<WorkerArgs>(
            "list_ports",
            "TCP ports listening in a worker's terminals' process trees, with the process and \
             the `term` it runs in: how to find the dev server a terminal started.",
            Kind::Read,
        ),
        tool::<WorkerArgs>(
            "list_items",
            "The items on a worker's workspace, which every person's Slopty shows as tiles: \
             `item` (the handle the item tools take, copy it verbatim), `kind` (terminal, \
             window, display, note, file, browser), `name` when one was given, `sleeping`, and \
             what it shows (`term`, `window`, `display`, `text`, `path` or `url`).",
            Kind::Read,
        ),
        tool::<OpenItemArgs>(
            "open_item",
            "Put a tile on a worker's workspace for every person to see, and return its `item`. \
             Give exactly one of: `url`, a web page (a localhost address is the worker's own, \
             so the dev server list_ports finds opens as the worker serves it); `file`, a text \
             file to edit; `note`, Markdown; `window` or `display`, streamed live, with ids \
             from list_windows. A terminal comes with open_terminal instead.",
            Kind::Write,
        ),
        tool::<RenameItemArgs>(
            "rename_item",
            "Give an item's tile a name, or take it away (omit `name`) so the tile says what \
             it shows.",
            Kind::Write,
        ),
        tool::<ItemArgs>(
            "remove_item",
            "Take an item off its workspace. A terminal's item goes with close_terminal.",
            Kind::Destroy,
        ),
        tool::<ItemArgs>(
            "point_at",
            "Point every person looking at the workspace at an item: each Slopty offers a jump \
             to its tile. For a result someone should look at now.",
            Kind::Write,
        ),
        tool::<WorkerArgs>(
            "list_windows",
            "The windows and displays a worker can stream, for open_item: `windows` (`window` \
             id, app, title, display, on_screen, width and height in points) and `displays` \
             (`display` id, width, height, scale, hz).",
            Kind::Read,
        ),
        tool::<ReadConversationArgs>(
            "read_conversation",
            "The conversation of the coding agent in a terminal, as Slopty's conversation face \
             shows it: `entries` (prompts, answers, thinking, tool calls with their results) \
             from `start` to `next` of `total`, the other `threads` (subagents), `tasks`, \
             `meters`, and `held`: the permission prompts the person holds, each with its `ask`, \
             `tool` and what the call would do. Reading changes nothing: every prompt stays in \
             the agent's terminal for the person. Pass `next` back as `since` to read on.",
            Kind::Read,
        ),
        tool::<CaptureStillArgs>(
            "capture_still",
            "One still picture of a window or a whole display on a worker, by its id from \
             list_windows, as a PNG image (halved until it fits a reply), with its `width` and \
             `height`. A worker that may not record its screen answers Unsupported.",
            Kind::Read,
        ),
        tool::<UploadArgs>(
            "upload_file",
            "Send a file of any size from the machine this tool runs on (`local`) to `path` on \
             a worker, in parts, replacing what is there only once every part has arrived and \
             adds up. A new file keeps the local file's mode. Only where the tool runs on your \
             machine (`slopty mcp`); the server's endpoint answers Unsupported.",
            Kind::Destroy,
        ),
        tool::<DownloadArgs>(
            "download_file",
            "Bring a file of any size from `path` on a worker to `local` on the machine this \
             tool runs on, in parts; refused when the file changes while it is read. Only where \
             the tool runs on your machine (`slopty mcp`); the server's endpoint answers \
             Unsupported.",
            Kind::Destroy,
        ),
        tool::<ForgetWorkerArgs>(
            "forget_worker",
            "Remove a worker that is not online (unreachable or gone) from the server's list, \
             for a machine retired or set up again elsewhere. An online worker is refused.",
            Kind::Destroy,
        ),
        tool::<ProjectCreateArgs>(
            "project_create",
            "Make a project: one goal many agents work on across the workers, which every \
             person's Slopty shows as a tree. Its orchestrator is you unless you name another \
             terminal; its `limits` (agents per worker and in all, tree depth, timeline size) \
             stay within the person's `bounds`. Returns the project with `tasks`, `timeline`, \
             `bounds` and `live`.",
            Kind::Write,
        ),
        tool::<ProjectUpdateArgs>(
            "project_update",
            "Change a project's orchestrator terminal, verifier, limits or metadata. Limits \
             stay within the person's bounds, which no tool raises.",
            Kind::Write,
        ),
        tool_with(
            "project_list",
            "Every project: name, title, repository, target branch, verifier, orchestrator, \
             limits.",
            Kind::Read,
            schema_for_empty_input(),
        ),
        tool::<ProjectStatusArgs>(
            "project_status",
            "A project whole: its `tasks` (each with `state`, its agent's own `status`, \
             `depends_on`, `kind`, the paths it `owns`, its `placement`, its terminal's `term`, \
             `branch`, `pr`, `verified`), `natives` (Claude Code's own subagents and to-dos in \
             each node's session), `bounds` and `limits` with what runs now (`live`), and its \
             `timeline` from `since`, with `next` to read on from. With `timeout_ms` it waits \
             for the next change: how to follow the tree without polling.",
            Kind::Read,
        ),
        tool::<TaskGetArgs>(
            "task_get",
            "One node of a project's tree in full: the task's brief, owned paths, placement, \
             verifier, base commit and metadata, and the subagents and to-dos Claude Code keeps \
             in it. project_status shows the tree; this shows a node.",
            Kind::Read,
        ),
        tool::<TaskCreateArgs>(
            "task_create",
            "Make a task in a project: a `title`, a `brief` its agent works from, a `kind` in \
             your words, `depends_on` other tasks, the paths it alone may write (`owns`; \
             refused when one overlaps another live task's) or `read_only`, and where it may \
             run (`placement`: CEL rules over the workers' facts, or a pinned worker). Nest it \
             under a `parent` to any depth the project allows. Returns the task with its \
             number.",
            Kind::Write,
        ),
        tool::<TaskClaimArgs>(
            "task_claim",
            "Take more paths for a task to own. Refused, with the task that holds it, when a \
             path overlaps one another live task owns; a task merged or failed lets its go.",
            Kind::Write,
        ),
        tool::<TaskUpdateArgs>(
            "task_update",
            "Change a task: its `state` (verifying, done, failed, planned again; merging is the \
             person's or the merge queue's), your own `status` text, `depends_on`, \
             `placement`, `verifier`, `metadata`; record its `branch` or the `base` commit its \
             work starts from, or put a `note` on the timeline. Its agent's own status moves \
             it among running, waiting and blocked.",
            Kind::Write,
        ),
        tool::<TaskReportArgs>(
            "task_report",
            "Report on your task's work to whoever split it off (its parent task's agent, or \
             the orchestrator): `checkpoint`, `needs_input`, `stuck` or `done`, with a `note`, \
             what you made (`artifacts`), your `branch` and `pr`. It reaches that agent through \
             its own hooks when the kind says, never typed into its terminal, and stays on the \
             timeline.",
            Kind::Write,
        ),
        tool::<ReviewReportArgs>(
            "review_report",
            "As the reviewer the server started for a task, say whether its work may merge: \
             `approved`, a `summary`, and the `findings` that matter (path, line, severity, \
             whether each blocks). An approval puts it in the merge queue; changes asked go \
             back to its agent with the findings. Only that reviewer, or the person, may.",
            Kind::Write,
        ),
        tool::<TaskAssignArgs>(
            "task_assign",
            "Put the terminal named (yours when omitted) on a task, for one the server did not \
             start. A task has one terminal, a terminal one task.",
            Kind::Write,
        ),
        tool::<TaskSpawnArgs>(
            "task_spawn",
            "Start what runs for a task, Claude Code (with Slopty's tools) or any `command`, \
             on the worker you name or the best its placement ranks, with the project and \
             the task in its environment. A task whose `depends_on` are not done waits, unless \
             you say `ignore_dependencies`. Every start counts against the project's limits \
             and the person's bounds. Refused with each worker's reason when none fits. \
             Returns the task with its terminal's `term`. When the person asks to start each \
             task themselves, it is `proposed` instead: it starts once they say so, and you \
             hear of it as for any start.",
            Kind::Write,
        ),
        tool::<PlacementSuggestArgs>(
            "placement_suggest",
            "Rank every worker for a task's placement, or for a `placement` you try: best \
             first, each with `fits`, its `score` and the `reasons` (each rule, whether it held, \
             its points, and why not). Changes nothing; pin the one you like with task_spawn's \
             `worker`.",
            Kind::Read,
        ),
        tool::<WakeWorkerArgs>(
            "wake_worker",
            "Wake a worker that sleeps: the server, or an online worker on the same LAN, sends \
             it a magic packet. Returns `by` (the machine that sent it), `to` (the sleeping \
             worker's interfaces) and `wake_on_lan_off` (it said it sleeps through one). It \
             shows online in list_workers once it is up; wait for that with events.",
            Kind::Write,
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
/// A failure of the tool is an `isError` result for the model to read; a `wait_for` tells
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
    let answered = if name == "capture_still" {
        capture(dispatch, arguments).await
    } else {
        run(dispatch, name, arguments, progress)
            .await
            .map(|value| vec![ContentBlock::text(value.to_string())])
    };
    Ok(match answered {
        Ok(content) => CallToolResult::success(content),
        Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]),
    })
}

/// `capture_still`: the picture as an image, after its size as JSON.
async fn capture<D: Dispatch>(
    dispatch: &D,
    arguments: Map<String, Value>,
) -> Result<Vec<ContentBlock>, ToolError> {
    let a: CaptureStillArgs = args(arguments)?;
    let target = a.target()?;
    let still =
        ops::capture_still(&mut Resolver::new(dispatch), a.worker.as_deref(), target).await?;
    let size = json(&view::still(None, still.width, still.height, still.png.len()))?;
    let image = data_encoding::BASE64.encode(&still.png);
    Ok(vec![ContentBlock::text(size.to_string()), ContentBlock::image(image, "image/png")])
}

/// Refused where the tool does not run on the caller's machine.
fn here<D: Dispatch>(dispatch: &D) -> Result<(), ToolError> {
    if dispatch.local_files() {
        Ok(())
    } else {
        Err(ToolError::new(
            ErrorCode::Unsupported,
            "this endpoint runs on the server, not on your machine, so it cannot reach your \
             files; run `slopty mcp` where they are, or `slopty push` and `slopty pull`",
        ))
    }
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
        "list_workers" => {
            if !arguments.is_empty() {
                return Err(ToolError::invalid("list_workers takes no arguments"));
            }
            json(&ops::overview(dispatch).await?.json())
        }
        "list_terminals" => {
            let a: ListTerminalsArgs = args(arguments)?;
            let (workers, terminals) = ops::terminals(&mut res, a.worker.as_deref()).await?;
            json(&view::terminals_json(&workers, &terminals))
        }
        "open_terminal" => {
            let a: OpenTerminalArgs = args(arguments)?;
            let env = a.env.into_iter().collect();
            let size = size(a.cols, a.rows)?;
            let spec = Spec { cwd: a.cwd, command: a.command, env, name: a.name, size };
            let key = checked_key(a.idempotency_key)?;
            json(&view::opened(ops::open(&mut res, a.worker.as_deref(), spec, key).await?))
        }
        "spawn_agent" => {
            let a: SpawnAgentArgs = args(arguments)?;
            let spec = AgentSpec {
                cwd: a.cwd,
                prompt: a.prompt,
                args: a.args,
                env: a.env.into_iter().collect(),
                size: size(a.cols, a.rows)?,
            };
            let key = checked_key(a.idempotency_key)?;
            if a.project.is_none() && a.task.is_none() && a.parent.is_none() {
                let term = ops::spawn_agent(&mut res, a.worker.as_deref(), spec, key).await?;
                return json(&view::opened(term));
            }
            let project = a.project.as_deref();
            let task = if let Some(task) = task_text(a.task.as_ref()) {
                task
            } else {
                let brief = spec.prompt.clone().unwrap_or_default();
                let title = brief.lines().map(str::trim).find(|l| !l.is_empty());
                let made = NewTask {
                    parent: task_text(a.parent.as_ref()),
                    title: title.unwrap_or("Agent").chars().take(120).collect(),
                    brief,
                    ..NewTask::default()
                };
                let made_key = key.as_ref().map(|k| k.part("task_create"));
                ops::task_create(&mut res, project, made, made_key).await?.id.to_string()
            };
            let AgentSpec { cwd, prompt, args, env, size } = spec;
            let run = Runner::Claude { prompt, args };
            let launch =
                LaunchSpec { pin: a.worker, cwd, run, env, size, ignore_dependencies: false };
            let started = ops::task_spawn(&mut res, project, Some(&task), launch, key).await?;
            let mut answer = json(&view::projects::task(&started))?;
            if let (Some(term), Some(fields)) =
                (started.assignment.map(|a| a.term), answer.as_object_mut())
            {
                fields.insert("worker".to_owned(), json(&term.worker)?);
                fields.insert("session".to_owned(), json(&term.session)?);
            }
            Ok(answer)
        }
        "project_create" => {
            let a: ProjectCreateArgs = args(arguments)?;
            let key = checked_key(a.idempotency_key)?;
            let spec = ProjectSpec {
                project: a.project,
                title: a.title,
                repo: a.repo,
                target: a.target,
                verifier: a.verifier,
                review: None,
                push: false,
                ask_to_start: false,
                orchestrator: a.orchestrator,
                limits: a.limits.into(),
                metadata: metadata_text(a.metadata)?,
            };
            json(&view::projects::status(&ops::project_create(&mut res, spec, key).await?))
        }
        "project_update" => {
            let a: ProjectUpdateArgs = args(arguments)?;
            let key = checked_key(a.idempotency_key)?;
            let edit = ProjectEdit {
                orchestrator: a.orchestrator,
                verifier: a.verifier,
                review: None,
                push: None,
                ask_to_start: None,
                limits: a.limits.into(),
                metadata: metadata_text(a.metadata)?,
            };
            let set = ops::project_set(&mut res, a.project.as_deref(), edit, key);
            json(&view::projects::status(&set.await?))
        }
        "project_list" => {
            if !arguments.is_empty() {
                return Err(ToolError::invalid("project_list takes no arguments"));
            }
            json(&view::projects::projects(&ops::projects(dispatch).await?))
        }
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
        "task_create" => {
            let a: TaskCreateArgs = args(arguments)?;
            let key = checked_key(a.idempotency_key)?;
            let new = NewTask {
                parent: task_text(a.parent.as_ref()),
                depends_on: a.depends_on.iter().map(TaskArg::text).collect(),
                kind: a.kind,
                title: a.title,
                brief: a.brief,
                owns: a.owns,
                read_only: a.read_only,
                placement: a.placement.map(PlacementArgs::spec).unwrap_or_default(),
                verifier: a.verifier,
                metadata: metadata_text(a.metadata)?,
            };
            let task = ops::task_create(&mut res, a.project.as_deref(), new, key).await?;
            json(&view::projects::task(&task))
        }
        "task_claim" => {
            let a: TaskClaimArgs = args(arguments)?;
            let key = checked_key(a.idempotency_key)?;
            let task = task_text(a.task.as_ref());
            let claimed =
                ops::task_claim(dispatch, a.project.as_deref(), task.as_deref(), a.paths, key);
            json(&view::projects::task(&claimed.await?))
        }
        "task_update" => {
            let a: TaskUpdateArgs = args(arguments)?;
            let change = a.change()?;
            let key = checked_key(a.idempotency_key)?;
            let task = task_text(a.task.as_ref());
            let placement = a.placement.map(PlacementArgs::spec);
            let (project, task) = (a.project.as_deref(), task.as_deref());
            let updated = ops::task_update(&mut res, project, task, change, placement, key);
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
        "review_report" => {
            let a: ReviewReportArgs = args(arguments)?;
            let key = checked_key(a.idempotency_key.clone())?;
            let task = task_text(a.task.as_ref());
            let project = a.project.clone();
            let reviewed =
                ops::task_review(dispatch, project.as_deref(), task.as_deref(), a.verdict(), key);
            json(&view::projects::task(&reviewed.await?))
        }
        "task_assign" => {
            let a: TaskAssignArgs = args(arguments)?;
            let key = checked_key(a.idempotency_key)?;
            let task = a.task.text();
            let (project, term) = (a.project.as_deref(), a.term.as_deref());
            let assigned = ops::task_assign(&mut res, project, Some(&task), term, key).await?;
            json(&view::projects::task(&assigned))
        }
        "task_spawn" => {
            let a: TaskSpawnArgs = args(arguments)?;
            let key = checked_key(a.idempotency_key)?;
            let run = match a.command {
                Some(argv) if a.prompt.is_none() && a.args.is_empty() => Runner::Command { argv },
                Some(_) => {
                    return Err(ToolError::invalid(
                        "prompt and args are Claude Code's; a command takes its own arguments",
                    ));
                }
                None => Runner::Claude { prompt: a.prompt, args: a.args },
            };
            let launch = LaunchSpec {
                pin: a.worker,
                cwd: a.cwd.unwrap_or_default(),
                run,
                env: a.env.into_iter().collect(),
                size: size(a.cols, a.rows)?,
                ignore_dependencies: a.ignore_dependencies,
            };
            let task = a.task.text();
            let started =
                ops::task_spawn(&mut res, a.project.as_deref(), Some(&task), launch, key).await?;
            json(&view::projects::task(&started))
        }
        "placement_suggest" => {
            let a: PlacementSuggestArgs = args(arguments)?;
            let task = task_text(a.task.as_ref());
            let spec = a.placement.map(PlacementArgs::spec);
            let ranked =
                ops::placement_suggest(&mut res, a.project.as_deref(), task.as_deref(), spec);
            json(&view::projects::suggestions(&ranked.await?))
        }
        "resize_terminal" => {
            let a: ResizeArgs = args(arguments)?;
            let size = Size { cols: a.cols, rows: a.rows };
            ops::resize(&mut res, &a.term, size, checked_key(a.idempotency_key)?).await?;
            json(&view::DONE)
        }
        "events" => {
            let a: EventsArgs = args(arguments)?;
            let filter =
                if a.agent_input { EventFilter::AgentNeedsInput } else { EventFilter::All };
            let timeout = a.timeout_ms.unwrap_or(DEFAULT_WAIT_MS);
            let page = with_progress(ops::events(dispatch, a.since, timeout, filter), progress);
            json(&view::events(&page.await?))
        }
        "send_input" => {
            let mut a: SendInputArgs = args(arguments)?;
            let (term, key) = (a.term.clone(), checked_key(a.idempotency_key.take())?);
            ops::send(&mut res, &term, a.input()?, key).await?;
            json(&view::DONE)
        }
        "read_screen" => {
            let a: TermArgs = args(arguments)?;
            json(&view::screen(&ops::screen(&mut res, &a.term).await?))
        }
        "read_output" => {
            let a: ReadOutputArgs = args(arguments)?;
            let max = a.max_lines.unwrap_or(DEFAULT_MAX_LINES);
            let (lines, next) = ops::output(&mut res, &a.term, a.since, max).await?;
            json(&view::output(&lines, next))
        }
        "list_commands" => {
            let a: ListCommandsArgs = args(arguments)?;
            json(&view::commands(&ops::commands(&mut res, &a.term, a.since).await?))
        }
        "wait_for" => {
            let a: WaitForArgs = args(arguments)?;
            let until = a.until()?;
            let term = res.term(&a.term).await?;
            let timeout = a.timeout_ms.unwrap_or(DEFAULT_WAIT_MS);
            let wait = ops::wait(dispatch, term, until, timeout, checked_key(a.idempotency_key)?);
            json(&view::waited(&with_progress(wait, progress).await?))
        }
        "agent_status" => {
            let a: TermArgs = args(arguments)?;
            json(&view::agent(ops::agent_status(&mut res, &a.term).await?.as_ref()))
        }
        "close_terminal" => {
            let a: CloseArgs = args(arguments)?;
            ops::close(&mut res, &a.term, checked_key(a.idempotency_key)?).await?;
            json(&view::DONE)
        }
        "read_file" => {
            let a: ReadFileArgs = args(arguments)?;
            let worker = res.worker(a.worker.as_deref()).await?;
            let path = a.path.clone();
            let chunk = ops::read_file(dispatch, worker, path, a.offset, a.length).await?;
            json(&view::file(&a.path, &chunk))
        }
        "write_file" => {
            let a: WriteFileArgs = args(arguments)?;
            let bytes = a
                .encoding
                .decode(a.content)
                .map_err(|e| ToolError::invalid(format!("content is not base64: {e}")))?;
            let key = checked_key(a.idempotency_key)?;
            ops::write_file(&mut res, a.worker.as_deref(), a.path, bytes, key).await?;
            json(&view::DONE)
        }
        "list_ports" => {
            let a: WorkerArgs = args(arguments)?;
            let (worker, ports) = ops::ports(&mut res, a.worker.as_deref()).await?;
            json(&view::ports(worker, &ports))
        }
        "list_dir" => {
            let a: ListDirArgs = args(arguments)?;
            let max = a.max.unwrap_or(DEFAULT_MAX_ENTRIES);
            let path = a.path.clone();
            let (entries, total) = ops::list_dir(&mut res, a.worker.as_deref(), path, max).await?;
            json(&view::dir(&a.path, &entries, total))
        }
        "stat" => {
            let a: PathArgs = args(arguments)?;
            let found = ops::stat(&mut res, a.worker.as_deref(), a.path.clone()).await?;
            json(&view::stat(&a.path, found.as_ref()))
        }
        "search_files" => {
            let a: SearchFilesArgs = args(arguments)?;
            let max = a.max_lines.unwrap_or(DEFAULT_MAX_MATCHES);
            let (worker, root) = (a.worker.as_deref(), a.root.clone());
            let (files, summary) = ops::search(&mut res, worker, root, a.query(), max).await?;
            json(&view::search(&a.root, &files, &summary))
        }
        "forget_worker" => {
            let a: ForgetWorkerArgs = args(arguments)?;
            ops::forget_worker(&mut res, &a.worker, checked_key(a.idempotency_key)?).await?;
            json(&view::DONE)
        }
        "wake_worker" => {
            let a: WakeWorkerArgs = args(arguments)?;
            json(&view::woken(&ops::wake(&mut res, &a.worker).await?))
        }
        "list_items" => {
            let a: WorkerArgs = args(arguments)?;
            let (worker, items) = ops::items(&mut res, a.worker.as_deref()).await?;
            json(&view::items(worker, &items))
        }
        "open_item" => {
            let mut a: OpenItemArgs = args(arguments)?;
            let (worker, key) = (a.worker.clone(), checked_key(a.idempotency_key.take())?);
            let (kind, name) = a.kind()?;
            let item = ops::open_item(&mut res, worker.as_deref(), kind, name, key).await?;
            json(&view::opened_item(item))
        }
        "rename_item" => {
            let a: RenameItemArgs = args(arguments)?;
            ops::rename_item(&mut res, &a.item, a.name, checked_key(a.idempotency_key)?).await?;
            json(&view::DONE)
        }
        "remove_item" => {
            let a: ItemArgs = args(arguments)?;
            ops::remove_item(&mut res, &a.item, checked_key(a.idempotency_key)?).await?;
            json(&view::DONE)
        }
        "point_at" => {
            let a: ItemArgs = args(arguments)?;
            ops::point_at(&mut res, &a.item, checked_key(a.idempotency_key)?).await?;
            json(&view::DONE)
        }
        "list_windows" => {
            let a: WorkerArgs = args(arguments)?;
            let (_worker, windows, displays) = ops::windows(&mut res, a.worker.as_deref()).await?;
            json(&view::screens(&windows, &displays))
        }
        "read_conversation" => {
            let a: ReadConversationArgs = args(arguments)?;
            let thread = view::thread_named(a.thread.as_deref());
            let max = a.max.unwrap_or(DEFAULT_MAX_ENTRIES_PAGE);
            let (term, page) =
                ops::read_conversation(&mut res, &a.term, thread, a.since, max).await?;
            json(&view::conversation(term, &page))
        }
        "upload_file" => {
            here(dispatch)?;
            let a: UploadArgs = args(arguments)?;
            let key = checked_key(a.idempotency_key)?;
            let local = std::path::Path::new(&a.local);
            let moved = bulk::upload(&mut res, a.worker.as_deref(), local, a.path, key).await?;
            json(&view::moved(&moved))
        }
        "download_file" => {
            here(dispatch)?;
            let a: DownloadArgs = args(arguments)?;
            let local = std::path::Path::new(&a.local);
            let moved = bulk::download(&mut res, a.worker.as_deref(), a.path, local).await?;
            json(&view::moved(&moved))
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
    use slopty_core::{ItemId, SessionId, WallMs, WorkerId};
    use slopty_proto::conversation::{Clipped, PermissionPrompt, ThreadId, ToolDetail};
    use slopty_proto::items::Item;
    use slopty_proto::orchestration::{
        ConversationPage, ItemRef, Outcome, TermRef, ThreadInfo, Verb, Waited,
    };
    use slopty_proto::project::{
        Assignment, Bounds, Fact, Limits, Live, NativeCounts, Natives, Peer, Placement, Preference,
        Project, ProjectId, ProjectStatus, Runner, Task, TaskId, TaskState, WorkerFacts,
    };
    use slopty_proto::server::{Liveness, Os, WorkerCaps, WorkerInfo};
    use slopty_proto::terminal::{SessionState, SessionSummary};

    use super::*;

    fn studio() -> WorkerId {
        "0199a000-0000-7000-8000-000000000001".parse().unwrap()
    }

    fn page() -> ItemId {
        "0199a1b1-c3d4-7000-8000-0000000017e5".parse().unwrap()
    }

    fn shell() -> SessionId {
        "0199a1b1-c3d4-7000-8000-00000000abcd".parse().unwrap()
    }

    /// One online worker with one shell; records every verb and the keys that came with them;
    /// a wait takes 25 s.
    #[derive(Default)]
    struct Fake {
        verbs: Mutex<Vec<Verb>>,
        keys: Mutex<Vec<Option<IdempotencyKey>>>,
        /// Runs on another machine than its caller, as the server's endpoint does.
        elsewhere: bool,
        /// The caller's own project and task.
        scope: crate::Scope,
    }

    /// Task `id` as the fake's server makes it, its agent in the fake's shell once started.
    fn made_task(id: u32, parent: Option<TaskId>, title: &str, started: bool) -> Task {
        Task {
            checks: None,
            spent: slopty_proto::project::Spent::default(),
            id: TaskId(id),
            parent,
            depends_on: Vec::new(),
            kind: String::new(),
            title: title.to_owned(),
            brief: String::new(),
            owns: Vec::new(),
            read_only: false,
            placement: Placement::default(),
            verifier: None,
            metadata: None,
            state: if started { TaskState::Running } else { TaskState::Planned },
            status: None,
            assignment: started.then(|| Assignment {
                term: TermRef { worker: studio(), session: shell() },
                since_ms: WallMs::ZERO,
                ended_ms: None,
                conversation: None,
                placed: None,
            }),
            branch: None,
            worktree: None,
            base: None,
            pr: None,
            reviewed: None,
            verified: None,
            merge: None,
            created_ms: WallMs::ZERO,
            updated_ms: WallMs::ZERO,
            step: None,
            proposal: None,
        }
    }

    /// The fake's project: task 3 planned, task 5 with its agent in the fake's shell.
    fn project_status(id: ProjectId) -> ProjectStatus {
        ProjectStatus {
            project: Project {
                orchestrator_spent: slopty_proto::project::Spent::default(),
                id,
                title: "Slopty".to_owned(),
                repo: "~/slopty".to_owned(),
                repo_id: None,
                target: "main".to_owned(),
                review: None,
                verifier: None,
                push: false,
                ask_to_start: false,
                orchestrator: None,
                limits: Limits::default(),
                metadata: None,
                created_ms: WallMs::ZERO,
            },
            tasks: [made_task(3, None, "Plan", false), made_task(5, None, "Review", true)]
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
                    },
                    load: 0.0,
                    last_seen_ms: WallMs::ZERO,
                }]),
                Verb::ListTerminals { .. } => Outcome::Terminals(vec![(
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
                        agent: None,
                        progress: None,
                        restored: None,
                        repo_id: None,
                    },
                )]),
                Verb::WaitFor { .. } => {
                    tokio::time::sleep(Duration::from_secs(25)).await;
                    Outcome::Waited(Waited::TimedOut)
                }
                Verb::ReadFile { offset, .. } => {
                    Outcome::File { bytes: vec![0xff, 0], offset, size: offset.saturating_add(2) }
                }
                Verb::Events { since, .. } => {
                    Outcome::Events { events: Vec::new(), next: since.unwrap_or(41), missed: 0 }
                }
                Verb::Close { .. } => Outcome::Error {
                    code: ErrorCode::UnknownTerminal,
                    message: "no such terminal".to_owned(),
                },
                Verb::OpenItem { worker, .. } => Outcome::Item(ItemRef { worker, item: page() }),
                Verb::ListItems { .. } => Outcome::Items(vec![Item {
                    id: page(),
                    kind: ItemKind::Browser { url: "http://localhost:5173/".to_owned() },
                    sleeping: false,
                    name: None,
                }]),
                Verb::ReadConversation { thread, .. } => {
                    let text = Clipped { text: "{}".to_owned(), lines: 1, chars: 2, full: None };
                    Outcome::Conversation(Box::new(ConversationPage {
                        threads: vec![ThreadInfo { id: ThreadId::Main, origin: None, entries: 0 }],
                        thread,
                        entries: Vec::new(),
                        start: 0,
                        next: 0,
                        total: 0,
                        tasks: Vec::new(),
                        meters: None,
                        held: vec![PermissionPrompt {
                            session: shell(),
                            ask: 3,
                            tool: "Bash".to_owned(),
                            detail: ToolDetail::Other { input: text },
                            suggestions: Vec::new(),
                            mode: None,
                            asked_ms: WallMs::ZERO,
                            until_ms: WallMs::ZERO,
                        }],
                    }))
                }
                Verb::CaptureStill { .. } => {
                    Outcome::Still { png: b"\x89PNG".to_vec(), width: 640, height: 400 }
                }
                Verb::Wake { .. } => {
                    Outcome::WakeSent { by: "server".to_owned(), to: vec!["en0".to_owned()] }
                }
                Verb::TaskCreate { spec, .. } => {
                    Outcome::Task(Box::new(made_task(7, spec.parent, &spec.title, false)))
                }
                Verb::TaskUpdate { task, .. } => {
                    Outcome::Task(Box::new(made_task(task.0, None, "Review", true)))
                }
                Verb::ProjectStatus { project, .. } => {
                    Outcome::Project(Box::new(project_status(project)))
                }
                Verb::WorkerFacts { .. } => {
                    let labels = BTreeMap::from([("rack".to_owned(), Fact::Text("b2".to_owned()))]);
                    let facts = BTreeMap::from([("labels".to_owned(), Fact::Map(labels))]);
                    Outcome::Facts(vec![WorkerFacts { worker: studio(), facts }])
                }
                Verb::TaskSpawn { task, .. } => {
                    Outcome::Task(Box::new(made_task(task.0, None, "Fix the hub", true)))
                }
                // The server's record of the terminal, over what its environment says.
                Verb::WorkingOn { .. } => {
                    Outcome::WorkingOn(Some(("slopty".parse().expect("a name"), Some(TaskId(5)))))
                }
                _ => Outcome::Done,
            }
        }

        fn local_files(&self) -> bool {
            !self.elsewhere
        }

        fn scope(&self) -> crate::Scope {
            self.scope.clone()
        }
    }

    /// An agent's own project and task are what the project tools default to: a task it makes
    /// is its own task's child, with the placement, kind, dependencies and metadata it gave, and
    /// `spawn_agent` with a project makes a task of the prompt and starts it through the
    /// server's placement. A caller that runs for no project is told to name one.
    #[tokio::test]
    async fn the_project_tools_default_to_the_caller_s_own_project_and_task() {
        let scope = crate::Scope {
            session: None,
            project: Some("slopty".parse().unwrap()),
            task: Some(TaskId(3)),
        };
        let fake = Fake { scope, ..Fake::default() };
        let made = json!({
            "title": "Hub",
            "owns": ["crates/slopty-server"],
            "kind": "build",
            "depends_on": [2],
            "placement": {
                "require": ["os == \"linux\""],
                "prefer": [{ "expr": "cpus", "weight": 2 }],
                "near": ["#2"],
            },
            "metadata": { "ticket": 12 },
        });
        let (failed, text) = call_json(&fake, "task_create", made).await;
        assert!(!failed, "{text}");
        let Some(Verb::TaskCreate { project, spec }) = fake.verbs().pop() else {
            panic!("{:?}", fake.verbs())
        };
        assert_eq!((project.as_str(), spec.parent), ("slopty", Some(TaskId(3))));
        assert_eq!((spec.kind.as_str(), spec.depends_on.as_slice()), ("build", &[TaskId(2)][..]));
        assert_eq!(spec.owns, ["crates/slopty-server"]);
        assert_eq!(spec.placement.require, ["os == \"linux\""]);
        assert_eq!(spec.placement.prefer, [Preference { expr: "cpus".to_owned(), weight: 2 }]);
        assert_eq!(spec.placement.near, [Peer::Task(TaskId(2))]);
        assert_eq!(spec.metadata.as_deref(), Some(r#"{"ticket":12}"#));

        let spawn =
            json!({"cwd": "~/w", "prompt": "Fix the hub\nThen test it", "project": "slopty"});
        let (failed, text) = call_json(&fake, "spawn_agent", spawn).await;
        assert!(!failed, "{text}");
        let verbs = fake.verbs();
        let [.., Verb::TaskCreate { spec, .. }, Verb::TaskSpawn { task, launch, .. }] =
            verbs.as_slice()
        else {
            panic!("{verbs:?}")
        };
        assert_eq!((spec.title.as_str(), spec.parent), ("Fix the hub", Some(TaskId(3))));
        assert_eq!(spec.brief, "Fix the hub\nThen test it");
        assert_eq!((*task, launch.pin, launch.cwd.as_str()), (TaskId(7), None, "~/w"));
        assert!(matches!(launch.run, Runner::Claude { .. }), "{launch:?}");
        let answer: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(answer["term"], format!("{}/{}", studio(), shell()));
        assert_eq!(answer["session"], shell().to_string());

        let command = json!({"task": 7, "command": ["cargo", "test"]});
        let (failed, text) = call_json(&fake, "task_spawn", command).await;
        assert!(!failed, "{text}");
        let Some(Verb::TaskSpawn { launch, .. }) = fake.verbs().pop() else { panic!() };
        let argv = vec!["cargo".to_owned(), "test".to_owned()];
        assert_eq!(launch.run, Runner::Command { argv });

        let bad = call_json(&fake, "task_create", json!({"title": "x", "os": "linux"})).await;
        assert!(bad.0 && bad.1.contains("unknown field `os`"), "{bad:?}");
        let (failed, text) =
            call_json(&Fake::default(), "task_create", json!({"title": "x"})).await;
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

    /// `list_workers` shows what each worker reports of itself beside its capabilities.
    #[tokio::test]
    async fn list_workers_shows_each_worker_s_facts() {
        let (failed, text) = call_json(&Fake::default(), "list_workers", json!({})).await;
        assert!(!failed, "{text}");
        let listed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(listed[0]["facts"]["labels"]["rack"], "b2", "{listed}");
    }

    async fn call_json(fake: &Fake, name: &str, arguments: Value) -> (bool, String) {
        let Value::Object(arguments) = arguments else { panic!("an object") };
        let result = call(fake, name, arguments, None).await.unwrap();
        let text = result.content[0].as_text().unwrap().text.clone();
        (result.is_error == Some(true), text)
    }

    #[test]
    fn every_tool_has_a_schema_a_description_and_a_hint() {
        let tools = list();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(
            names,
            [
                "list_workers",
                "list_terminals",
                "open_terminal",
                "spawn_agent",
                "send_input",
                "read_screen",
                "read_output",
                "list_commands",
                "wait_for",
                "events",
                "agent_status",
                "resize_terminal",
                "close_terminal",
                "read_file",
                "write_file",
                "list_dir",
                "stat",
                "search_files",
                "list_ports",
                "list_items",
                "open_item",
                "rename_item",
                "remove_item",
                "point_at",
                "list_windows",
                "read_conversation",
                "capture_still",
                "upload_file",
                "download_file",
                "forget_worker",
                "project_create",
                "project_update",
                "project_list",
                "project_status",
                "task_get",
                "task_create",
                "task_claim",
                "task_update",
                "task_report",
                "review_report",
                "task_assign",
                "task_spawn",
                "placement_suggest",
                "wake_worker",
            ]
        );
        for t in &tools {
            assert_eq!(t.input_schema.get("type"), Some(&json!("object")), "{}", t.name);
            assert!(t.description.as_ref().is_some_and(|d| d.len() > 40), "{}", t.name);
            assert!(t.annotations.as_ref().is_some_and(|a| a.read_only_hint.is_some()));
        }
        let send = get("send_input").unwrap();
        assert!(send.input_schema["properties"]["keys"].is_object(), "{:?}", send.input_schema);
        let write = get("write_file").unwrap();
        assert_eq!(write.input_schema["required"], json!(["path", "content"]));
    }

    #[test]
    fn wait_for_takes_exactly_one_condition() {
        let parse = |v: Value| {
            let Value::Object(m) = v else { panic!() };
            args::<WaitForArgs>(m).and_then(|a| a.until())
        };
        assert_eq!(
            parse(json!({"term": "t", "output": "ok$"})).unwrap(),
            WaitUntil::Output("ok$".to_owned())
        );
        assert_eq!(
            parse(json!({"term": "t", "quiet_ms": 300})).unwrap(),
            WaitUntil::Quiet { ms: 300 }
        );
        assert_eq!(
            parse(json!({"term": "t", "agent_input": true})).unwrap(),
            WaitUntil::AgentNeedsInput
        );
        parse(json!({"term": "t"})).unwrap_err();
        parse(json!({"term": "t", "exit": true, "command_done": true})).unwrap_err();
        let err = parse(json!({"term": "t", "exit": true, "session": "x"})).unwrap_err();
        assert!(err.message.contains("unknown field"), "{err}");
    }

    #[test]
    fn send_input_takes_exactly_one_form() {
        let parse = |v: Value| {
            let Value::Object(m) = v else { panic!() };
            args::<SendInputArgs>(m).and_then(SendInputArgs::input)
        };
        assert_eq!(
            parse(json!({"term": "t", "keys": ["ctrl+c"]})).unwrap(),
            Input::Keys(vec!["ctrl+c".to_owned()])
        );
        parse(json!({"term": "t", "text": "a", "paste": "b"})).unwrap_err();
        parse(json!({"term": "t"})).unwrap_err();
    }

    #[tokio::test]
    async fn an_unknown_tool_is_invalid_params_and_a_failure_is_the_models_to_read() {
        let fake = Fake::default();
        let err = call(&fake, "rm_rf", Map::new(), None).await.unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        let term = format!("{}/{}", studio(), shell());
        let (failed, text) = call_json(&fake, "close_terminal", json!({ "term": term })).await;
        assert!(failed);
        assert_eq!(text, "no such terminal (UnknownTerminal)");
        assert_eq!(
            fake.verbs(),
            [Verb::Close { term: TermRef { worker: studio(), session: shell() } }],
            "full ids cost no round trip"
        );

        let (failed, text) = call_json(&fake, "read_screen", json!({ "term": "x", "y": 1 })).await;
        assert!(failed);
        assert!(text.starts_with("bad arguments: unknown field `y`"), "{text}");
        let (failed, text) = call_json(&fake, "list_workers", json!({ "worker": "a" })).await;
        assert!(failed, "{text}");
    }

    #[tokio::test]
    async fn names_resolve_and_answers_render_as_views() {
        let fake = Fake::default();
        let args = json!({ "term": "mac-studio/0199a1b1", "keys": ["enter"] });
        let (failed, text) = call_json(&fake, "send_input", args).await;
        assert!(!failed, "{text}");
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), json!({ "ok": true }));
        assert!(!text.contains('\n'), "compact: {text}");
        let term = TermRef { worker: studio(), session: shell() };
        assert_eq!(
            fake.verbs(),
            [
                Verb::ListWorkers,
                Verb::ListTerminals { worker: Some(studio()) },
                Verb::SendInput { term, input: Input::Keys(vec!["enter".to_owned()]) },
            ]
        );

        let (_, text) = call_json(&fake, "read_file", json!({ "path": "/bin/x" })).await;
        let file: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            file,
            json!({
                "path": "/bin/x", "size": 2, "offset": 0, "length": 2, "more": false,
                "encoding": "base64", "content": "/wA="
            })
        );

        let args = json!({ "worker": studio().to_string(), "path": "/b", "content": "AAE=", "encoding": "base64" });
        let (failed, text) = call_json(&fake, "write_file", args).await;
        assert!(!failed, "{text}");
        let wrote = fake.verbs().pop().unwrap();
        assert_eq!(
            wrote,
            Verb::WriteFile { worker: studio(), path: "/b".to_owned(), bytes: vec![0, 1] }
        );
        let args = json!({ "path": "/b", "content": "héllo" });
        call_json(&fake, "write_file", args).await;
        let Some(Verb::WriteFile { bytes, .. }) = fake.verbs().pop() else { panic!() };
        assert_eq!(bytes, "héllo".as_bytes(), "text by default");
        let args = json!({ "path": "/b", "content": "!!", "encoding": "base64" });
        let (failed, text) = call_json(&fake, "write_file", args).await;
        assert!(failed && text.contains("not base64"), "{text}");
    }

    /// A key given to a tool that changes something goes with its verb; a malformed one is the
    /// model's to fix.
    #[tokio::test]
    async fn an_idempotency_key_goes_with_its_verb() {
        let fake = Fake::default();
        let term = format!("{}/{}", studio(), shell());
        let args = json!({ "term": term, "text": "make\n", "idempotency_key": "step-3" });
        let (failed, text) = call_json(&fake, "send_input", args).await;
        assert!(!failed, "{text}");
        let sent = fake.keys.lock().last().cloned().flatten();
        assert_eq!(sent, Some(IdempotencyKey::new("step-3").unwrap()));
        let args = json!({ "term": term, "idempotency_key": "two words" });
        let (failed, text) = call_json(&fake, "close_terminal", args).await;
        assert!(failed && text.contains("idempotency key"), "{text}");
    }

    /// The new verbs' arguments reach the wire as they were given: a size together or not at
    /// all, a range, the agent filter, the agent's arguments.
    #[tokio::test]
    async fn sizes_ranges_and_filters_reach_the_verb() {
        let fake = Fake::default();
        let term = format!("{}/{}", studio(), shell());
        let t = TermRef { worker: studio(), session: shell() };
        let (failed, text) =
            call_json(&fake, "resize_terminal", json!({ "term": term, "cols": 200, "rows": 50 }))
                .await;
        assert!(!failed, "{text}");
        let size = Size { cols: 200, rows: 50 };
        assert_eq!(fake.verbs().pop(), Some(Verb::ResizeTerminal { term: t, size }));

        let (failed, text) = call_json(&fake, "open_terminal", json!({ "cols": 90 })).await;
        assert!(failed && text.contains("together"), "{text}");
        let spawn = json!({
            "worker": studio().to_string(), "cwd": "~/src", "args": ["--model", "opus"],
            "env": { "A": "1" }, "cols": 100, "rows": 30,
        });
        call_json(&fake, "spawn_agent", spawn).await;
        let Some(Verb::SpawnAgent { args, env, size, prompt, .. }) = fake.verbs().pop() else {
            panic!("spawn")
        };
        assert_eq!(args, ["--model", "opus"]);
        assert_eq!(env, [("A".to_owned(), "1".to_owned())]);
        assert_eq!((size, prompt), (Some(Size { cols: 100, rows: 30 }), None));

        let read =
            json!({ "worker": studio().to_string(), "path": "/f", "offset": 10, "length": 2 });
        let (_, text) = call_json(&fake, "read_file", read).await;
        let file: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((file["offset"].as_u64(), file["more"].as_bool()), (Some(10), Some(false)));
        let Some(Verb::ReadFile { offset, length, .. }) = fake.verbs().pop() else { panic!() };
        assert_eq!((offset, length), (10, Some(2)));

        let (_, text) =
            call_json(&fake, "events", json!({ "agent_input": true, "timeout_ms": 0 })).await;
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({
                "events": [], "next": 41, "missed": 0
            })
        );
        let filter = EventFilter::AgentNeedsInput;
        assert_eq!(
            fake.verbs().pop(),
            Some(Verb::Events { since: None, timeout_ms: 0, filter }),
            "the hub answers it: no name is resolved first"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_long_wait_reports_progress_every_ten_seconds() {
        let fake = Fake::default();
        let reported = Mutex::new(Vec::new());
        let report = |waited: Duration| reported.lock().push(waited);
        let term = format!("{}/{}", studio(), shell());
        let Value::Object(args) = json!({ "term": term, "exit": true, "timeout_ms": 30_000 })
        else {
            panic!()
        };
        let result = call(&fake, "wait_for", args, Some(&report)).await.unwrap();
        let text = &result.content[0].as_text().unwrap().text;
        let waited: Value = serde_json::from_str(text).unwrap();
        assert_eq!(waited, json!({ "result": "timed_out", "line": null }));
        let every = PROGRESS_EVERY;
        assert_eq!(*reported.lock(), [every, every.saturating_mul(2)], "at 10 s and 20 s of 25");
        let Some(Verb::WaitFor { timeout_ms, .. }) = fake.verbs().pop() else { panic!() };
        assert_eq!(timeout_ms, 30_000, "passed through; the server caps it");
    }

    /// An item is opened from exactly one of its kinds and named by a prefix of its id, which
    /// resolves against the worker's items before the verb goes.
    #[tokio::test]
    async fn an_item_opens_from_one_kind_and_answers_to_a_prefix() {
        let fake = Fake::default();
        let (failed, text) =
            call_json(&fake, "open_item", json!({ "url": "http://a/", "note": "x" })).await;
        assert!(failed && text.contains("exactly one"), "{text}");
        let open = json!({ "url": "http://localhost:5173/", "name": "app" });
        let (failed, text) = call_json(&fake, "open_item", open).await;
        assert!(!failed, "{text}");
        let handle = format!("{}/{}", studio(), page());
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), json!({ "item": handle }));
        let kind = ItemKind::Browser { url: "http://localhost:5173/".to_owned() };
        let name = Some("app".to_owned());
        assert_eq!(fake.verbs().pop(), Some(Verb::OpenItem { worker: studio(), kind, name }));

        let (failed, text) =
            call_json(&fake, "point_at", json!({ "item": "mac-studio/0199a1b1" })).await;
        assert!(!failed, "{text}");
        let item = ItemRef { worker: studio(), item: page() };
        assert_eq!(fake.verbs().pop(), Some(Verb::PointAt { item }));
        let (failed, text) = call_json(&fake, "rename_item", json!({ "item": "0199a1b1" })).await;
        assert!(!failed, "{text}");
        assert_eq!(fake.verbs().pop(), Some(Verb::RenameItem { item, name: None }));
    }

    /// A conversation is read from a thread and page, and its waiting prompt answered by its
    /// `ask` under a key; a verdict's message goes with deny only.
    #[tokio::test]
    async fn a_conversation_is_read_and_its_prompts_are_not_an_agents_to_answer() {
        let fake = Fake::default();
        let term = format!("{}/{}", studio(), shell());
        let t = TermRef { worker: studio(), session: shell() };
        let read = json!({ "term": term, "thread": "a1", "since": 4 });
        let (failed, text) = call_json(&fake, "read_conversation", read).await;
        assert!(!failed, "{text}");
        let page: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            (page["thread"].clone(), page["held"][0]["ask"].clone()),
            (json!("a1"), json!(3))
        );
        let thread = ThreadId::Agent("a1".to_owned());
        let asked =
            Verb::ReadConversation { term: t, thread, since: Some(4), max: 50, hold: false };
        assert_eq!(fake.verbs().pop(), Some(asked));

        let Value::Object(args) = json!({ "term": term }) else { panic!() };
        let unknown = call(&fake, "answer_permission", args, None).await;
        assert!(unknown.is_err(), "the person answers permissions, never an agent");
    }

    /// A still comes back as an image after its size; the endpoint that runs elsewhere than
    /// its caller refuses to move the caller's files.
    #[tokio::test]
    async fn a_still_is_an_image_and_files_move_only_where_they_are() {
        let fake = Fake::default();
        let Value::Object(args) = json!({ "window": 42 }) else { panic!() };
        let result = call(&fake, "capture_still", args, None).await.unwrap();
        assert_ne!(result.is_error, Some(true), "{result:?}");
        let size: Value = serde_json::from_str(&result.content[0].as_text().unwrap().text).unwrap();
        assert_eq!(size, json!({ "width": 640, "height": 400, "bytes": 4, "format": "png" }));
        let image = result.content[1].as_image().unwrap();
        assert_eq!((image.mime_type.as_str(), image.data.as_str()), ("image/png", "iVBORw=="));
        let target = CaptureTarget::Window(WindowId(42));
        assert_eq!(fake.verbs().pop(), Some(Verb::CaptureStill { worker: studio(), target }));
        let (failed, text) = call_json(&fake, "capture_still", json!({})).await;
        assert!(failed && text.contains("exactly one"), "{text}");

        let server = Fake { elsewhere: true, ..Fake::default() };
        let up = json!({ "local": "/tmp/x", "path": "/w/x" });
        let (failed, text) = call_json(&server, "upload_file", up).await;
        assert!(failed && text.contains("(Unsupported)"), "{text}");
        let down = json!({ "path": "/w/x", "local": "/tmp/x" });
        let (failed, text) = call_json(&server, "download_file", down).await;
        assert!(failed && text.contains("(Unsupported)"), "{text}");
        assert!(server.verbs().is_empty(), "nothing was sent");
    }

    /// A worker is woken by its name, and the answer says who sent the packet and where.
    #[tokio::test]
    async fn a_worker_is_woken_by_name() {
        let fake = Fake::default();
        let (failed, text) =
            call_json(&fake, "wake_worker", json!({ "worker": "mac-studio" })).await;
        assert!(!failed, "{text}");
        let woken: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            woken,
            json!({
                "worker": studio().to_string(),
                "by": "server",
                "to": ["en0"],
                "wake_on_lan_off": false,
            })
        );
        assert_eq!(fake.verbs().pop(), Some(Verb::Wake { worker: studio() }));
        let (failed, text) = call_json(&fake, "wake_worker", json!({})).await;
        assert!(failed && text.contains("worker"), "a worker is named: {text}");
    }
}
