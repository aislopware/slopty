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
use slopty_proto::orchestration::{ErrorCode, EventFilter, Input, Size, WaitUntil};

use crate::ops::{self, AgentSpec, DEFAULT_MAX_ENTRIES, DEFAULT_MAX_LINES, DEFAULT_WAIT_MS, Spec};
use crate::resolve::Resolver;
use crate::view::{self, Encoding};
use crate::{Dispatch, ToolError};

/// What the model reads before any tool description.
pub const INSTRUCTIONS: &str = "\
Slopty runs terminals and coding agents on a fleet of machines (workers). Start with \
list_workers, then list_terminals. A terminal is named by its `term` (worker/session, as the \
lists print it); copy it verbatim into the terminal tools. A `worker` argument takes a worker's \
name or id. To run a command: send_input text \"cmd\\n\", then wait_for command_done, then \
read_output from the line you started at. Prefer wait_for (one terminal) and events (the whole \
fleet: agents needing you, terminals opening and closing, workers coming and going) to polling \
read_screen or read_output in a loop.";

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
}

/// A terminal alone.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TermArgs {
    /// The terminal, as the lists print it (`worker/session`).
    term: String,
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

/// `forget_worker`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ForgetWorkerArgs {
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
             unreachable, gone), address, OS, how many terminals it runs, and `waiting`: the \
             agents on it that need a human, each with its `term`. Start here.",
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
             typing `prompt` once it is ready, and return its `term`. Follow it with wait_for \
             agent_input (this agent) or events agent_input (any agent) to learn when it needs \
             you, agent_status for what it is doing, and read_screen to see it.",
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
             with `term` and `needs_human`), terminals opened and closed, workers online, \
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
        tool::<TermArgs>(
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
        tool::<WorkerArgs>(
            "list_ports",
            "TCP ports listening in a worker's terminals' process trees, with the process and \
             the `term` it runs in: how to find the dev server a terminal started.",
            Kind::Read,
        ),
        tool::<ForgetWorkerArgs>(
            "forget_worker",
            "Remove a worker that is not online (unreachable or gone) from the server's list, \
             for a machine retired or set up again elsewhere. An online worker is refused.",
            Kind::Destroy,
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
            json(&view::opened(ops::open(&mut res, a.worker.as_deref(), spec).await?))
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
            json(&view::opened(ops::spawn_agent(&mut res, a.worker.as_deref(), spec).await?))
        }
        "resize_terminal" => {
            let a: ResizeArgs = args(arguments)?;
            ops::resize(&mut res, &a.term, Size { cols: a.cols, rows: a.rows }).await?;
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
            let a: SendInputArgs = args(arguments)?;
            let term = a.term.clone();
            ops::send(&mut res, &term, a.input()?).await?;
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
            let wait = ops::wait(dispatch, term, until, timeout);
            json(&view::waited(&with_progress(wait, progress).await?))
        }
        "agent_status" => {
            let a: TermArgs = args(arguments)?;
            json(&view::agent(ops::agent_status(&mut res, &a.term).await?.as_ref()))
        }
        "close_terminal" => {
            let a: TermArgs = args(arguments)?;
            ops::close(&mut res, &a.term).await?;
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
            ops::write_file(&mut res, a.worker.as_deref(), a.path, bytes).await?;
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
        "forget_worker" => {
            let a: ForgetWorkerArgs = args(arguments)?;
            ops::forget_worker(&mut res, &a.worker).await?;
            json(&view::DONE)
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
    use slopty_core::{SessionId, WorkerId};
    use slopty_proto::orchestration::{Outcome, TermRef, Verb, Waited};
    use slopty_proto::server::{Liveness, Os, WorkerCaps, WorkerInfo};
    use slopty_proto::terminal::{SessionState, SessionSummary};

    use super::*;

    fn studio() -> WorkerId {
        "0199a000-0000-7000-8000-000000000001".parse().unwrap()
    }

    fn shell() -> SessionId {
        "0199a1b1-c3d4-7000-8000-00000000abcd".parse().unwrap()
    }

    /// One online worker with one shell; records every verb; a wait takes 25 s.
    #[derive(Default)]
    struct Fake {
        verbs: Mutex<Vec<Verb>>,
    }

    impl Fake {
        fn verbs(&self) -> Vec<Verb> {
            self.verbs.lock().clone()
        }
    }

    impl Dispatch for Fake {
        async fn call(&self, verb: Verb) -> Outcome {
            self.verbs.lock().push(verb.clone());
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
                        load: 0.0,
                        version: "0".to_owned(),
                    },
                    last_seen_ms: 0,
                }]),
                Verb::ListTerminals { .. } => Outcome::Terminals(vec![(
                    studio(),
                    SessionSummary {
                        id: shell(),
                        title: "zsh".to_owned(),
                        cwd: None,
                        repo: None,
                        cols: 80,
                        rows: 24,
                        state: SessionState::Running,
                        viewers: 0,
                        command: Vec::new(),
                        agent: None,
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
                _ => Outcome::Done,
            }
        }
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
                "list_ports",
                "forget_worker",
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
}
