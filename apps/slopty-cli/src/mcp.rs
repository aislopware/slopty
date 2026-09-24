//! `slopty mcp`: the verbs as an MCP server on stdio, for an AI agent such as Claude Code.
//!
//! Each tool call becomes one verb (after any name lookups) on a link to the server held for
//! the process lifetime as `Role::Agent`, redialled when it drops. Answers are the same JSON
//! as `slopty … --json`. An agent that comes to need a human anywhere on the fleet is announced
//! as a `notifications/message`, so the orchestrating session hears of it without polling.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use rmcp::handler::server::common::{schema_for_empty_input, schema_for_type};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ProgressNotificationParam, ProtocolVersion,
    ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::schemars::JsonSchema;
use rmcp::service::{MaybeSendFuture, RequestContext};
use rmcp::{ErrorData, Peer, RoleServer, ServerHandler, ServiceExt as _};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use slopty_core::WorkerId;
use slopty_net::client::bind_client;
use slopty_proto::agent::{AgentEvent, AgentStatus, BlockReason};
use slopty_proto::orchestration::{Input, TermRef, WaitUntil};
use slopty_proto::server::{Event, FromServer, Role};
use tokio::sync::broadcast;

use crate::link::{self, Link};
use crate::ops::{self, Spec};
use crate::resolve::Resolver;
use crate::view;

/// `slopty mcp --help` epilogue.
pub const REGISTER_HELP: &str = "\
Register Slopty with Claude Code:

  claude mcp add slopty -- slopty mcp

The server is --server, else $SLOPTY_SERVER, else `server` under [client] in settings.toml. \
To pin one: claude mcp add slopty -- slopty mcp --server studio";

/// The protocol revision this shim speaks; older ones are still negotiated for clients that
/// ask for them in `initialize`.
const REVISION: ProtocolVersion = ProtocolVersion::V_2026_07_28;
/// `wait_for` without a `timeout_ms`.
const DEFAULT_WAIT_MS: u32 = 60_000;
/// `read_output` without a `max_lines`.
const DEFAULT_MAX_LINES: u32 = 200;
/// How often a `wait_for` that carries a progress token reports that it is still waiting.
const PROGRESS_EVERY: Duration = Duration::from_secs(10);

/// What the model reads before any tool description.
const INSTRUCTIONS: &str = "\
Slopty runs terminals and coding agents on a fleet of machines (workers). Start with \
list_workers, then list_terminals. A terminal is named by its `term` (worker/session, as the \
lists print it); copy it verbatim into the terminal tools. A `worker` argument takes a worker's \
name or id. To run a command: send_input text \"cmd\\n\", then wait_for command_done, then \
read_output from the line you started at. Prefer wait_for to polling read_screen or read_output \
in a loop.";

/// Serve MCP on stdio until the client hangs up.
pub async fn run(server: Option<&str>, data_dir: &Path) -> Result<()> {
    let address = link::locate(server, data_dir)?;
    let endpoint = bind_client()?;
    let role = Role::Agent { name: format!("slopty mcp @ {}", crate::verbs::machine_name()) };
    let (link, events) = Link::persistent(endpoint.clone(), address, role);
    let running = Slopty { link }
        .serve(rmcp::transport::stdio())
        .await
        .context("the MCP client did not open a session")?;
    let forward = tokio::spawn(forward_needs(events, running.peer().clone()));
    let quit = running.waiting().await;
    forward.abort();
    crate::client::close_endpoint(&endpoint).await;
    quit.context("MCP session")?;
    Ok(())
}

/// The MCP server: tools that forward to the link.
#[derive(Debug, Clone)]
struct Slopty {
    link: Link,
}

/// A worker, or the only one online.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct WorkerArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
}

/// `list_terminals`.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ListTerminalsArgs {
    /// Only this worker (name or id); every worker when omitted.
    worker: Option<String>,
}

/// `open_terminal`.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
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
    /// A short name for the terminal's tile on the canvas.
    name: Option<String>,
}

/// `spawn_agent`.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct SpawnAgentArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
    /// Working directory, usually a repository root.
    cwd: String,
    /// The first prompt, typed once the agent is ready.
    prompt: Option<String>,
}

/// `send_input`.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
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
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct TermArgs {
    /// The terminal, as the lists print it (`worker/session`).
    term: String,
}

/// `read_output`.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
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
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ListCommandsArgs {
    /// The terminal, as the lists print it (`worker/session`).
    term: String,
    /// Only commands whose prompt is at or after this absolute line.
    since: Option<u64>,
}

/// `wait_for`.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
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
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
    /// Absolute path, or `~/…`.
    path: String,
}

/// `write_file`.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(deny_unknown_fields)]
struct WriteFileArgs {
    /// Worker name or id; the only worker online when omitted.
    worker: Option<String>,
    /// Absolute path, or `~/…`.
    path: String,
    /// The whole new contents, as text.
    content: String,
}

impl SendInputArgs {
    fn input(self) -> Result<Input> {
        match (self.text, self.paste, self.keys) {
            (Some(text), None, None) => Ok(Input::Text(text)),
            (None, Some(paste), None) => Ok(Input::Paste(paste)),
            (None, None, Some(keys)) => Ok(Input::Keys(keys)),
            _ => bail!("give exactly one of text, paste, keys"),
        }
    }
}

impl WaitForArgs {
    fn until(&self) -> Result<WaitUntil> {
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
            _ => bail!("give exactly one of output, quiet_ms, command_done, exit, agent_input"),
        }
    }
}

fn read_only() -> ToolAnnotations {
    ToolAnnotations::new().read_only(true)
}

fn changes() -> ToolAnnotations {
    let mut a = ToolAnnotations::new().read_only(false);
    a.destructive_hint = Some(false);
    a
}

fn destroys() -> ToolAnnotations {
    let mut a = ToolAnnotations::new().read_only(false);
    a.destructive_hint = Some(true);
    a
}

fn tool<T: JsonSchema + 'static>(
    name: &'static str,
    description: &'static str,
    annotations: ToolAnnotations,
) -> Tool {
    Tool::new(name, description, schema_for_type::<T>()).annotate(annotations)
}

/// Every tool, in the order a model meets them.
fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "list_workers",
            "Every machine (worker) the Slopty server knows: name, liveness (online, \
             unreachable, gone), address, OS, how many terminals it runs, and `waiting`: the \
             agents on it that need a human, each with its `term`. Start here.",
            schema_for_empty_input(),
        )
        .annotate(read_only()),
        tool::<ListTerminalsArgs>(
            "list_terminals",
            "Terminals on one worker or on all: `term` (the handle every terminal tool takes, \
             copy it verbatim), title, working directory, repository, size, whether the program \
             still runs, and its command line.",
            read_only(),
        ),
        tool::<OpenTerminalArgs>(
            "open_terminal",
            "Start a terminal on a worker, running the login shell or `command`, and return its \
             `term`. Then send_input, wait_for and read_output.",
            changes(),
        ),
        tool::<SpawnAgentArgs>(
            "spawn_agent",
            "Start Claude Code in a new terminal in `cwd`, optionally typing `prompt` once it is \
             ready, and return its `term`. Follow it with wait_for agent_input to learn when it \
             needs you, agent_status for what it is doing, and read_screen to see it.",
            changes(),
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
            changes(),
        ),
        tool::<TermArgs>(
            "read_screen",
            "The screen as drawn now: rows with their absolute line index, cursor, title, \
             working directory, and `alternate` (a full-screen program such as an editor or an \
             agent's TUI is showing). Best for TUIs and prompts; for what a command printed, \
             read_output pages through all of it.",
            read_only(),
        ),
        tool::<ReadOutputArgs>(
            "read_output",
            "Scrollback and screen as lines with absolute indexes, which stay put as old lines \
             are evicted. Returns `lines` and `next`: pass `next` back as `since` to read only \
             what came after, and page through long output that way instead of re-reading it. \
             A command's output starts at its `output_start` from list_commands.",
            read_only(),
        ),
        tool::<ListCommandsArgs>(
            "list_commands",
            "Commands run at the shell prompt (OSC 133 blocks), oldest first: the command line, \
             `exit` (null while it runs) and the output's line range [output_start, output_end) \
             for read_output. Empty when the shell has no prompt integration.",
            read_only(),
        ),
        tool::<WaitForArgs>(
            "wait_for",
            "Block until something happens in a terminal instead of polling read_screen or \
             read_output. Give exactly one condition: `output` (a regex matched by a new line), \
             `quiet_ms` (that long with no output), `command_done` (the running command ends), \
             `exit` (the program exits), `agent_input` (the agent needs a human or went idle). \
             Returns `result`: met (with the matching `line` for `output`), timed_out, or \
             closed. On timed_out, look with read_screen, then wait again.",
            read_only(),
        ),
        tool::<TermArgs>(
            "agent_status",
            "The coding agent in a terminal, if one runs, and its status: idle, working, tool \
             (running `tool`), blocked (needs a human, with `reason` and for a permission the \
             `tool`), or done.",
            read_only(),
        ),
        tool::<TermArgs>(
            "close_terminal",
            "Hang up a terminal's program and remove the terminal.",
            destroys(),
        ),
        tool::<ReadFileArgs>(
            "read_file",
            "Read a UTF-8 text file on a worker; returns `text` and its `size` in bytes.",
            read_only(),
        ),
        tool::<WriteFileArgs>(
            "write_file",
            "Create or replace a text file on a worker with `content`.",
            destroys(),
        ),
        tool::<WorkerArgs>(
            "list_ports",
            "TCP ports listening in a worker's terminals' process trees, with the process and \
             the `term` it runs in: how to find the dev server a terminal started.",
            read_only(),
        ),
    ]
}

fn args<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| anyhow!("bad arguments: {e}"))
}

fn json(value: &impl serde::Serialize) -> Result<Value> {
    Ok(serde_json::to_value(value)?)
}

impl Slopty {
    /// Run one tool; its error is the model's to read.
    async fn dispatch(
        &self,
        name: &str,
        arguments: Value,
        context: &RequestContext<RoleServer>,
    ) -> Result<Value> {
        let mut res = Resolver::new(&self.link);
        match name {
            "list_workers" => {
                if arguments.as_object().is_some_and(|a| !a.is_empty()) {
                    bail!("list_workers takes no arguments");
                }
                json(&ops::overview(&self.link).await?.json())
            }
            "list_terminals" => {
                let a: ListTerminalsArgs = args(arguments)?;
                let (workers, terminals) = ops::terminals(&mut res, a.worker.as_deref()).await?;
                json(&view::terminals_json(&workers, &terminals))
            }
            "open_terminal" => {
                let a: OpenTerminalArgs = args(arguments)?;
                let env = a.env.into_iter().collect();
                let spec = Spec { cwd: a.cwd, command: a.command, env, name: a.name };
                json(&view::opened(ops::open(&mut res, a.worker.as_deref(), spec).await?))
            }
            "spawn_agent" => {
                let a: SpawnAgentArgs = args(arguments)?;
                let term = ops::spawn_agent(&mut res, a.worker.as_deref(), a.cwd, a.prompt).await?;
                json(&view::opened(term))
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
                let term = ops::term(&mut res, &a.term).await?;
                let timeout = a.timeout_ms.unwrap_or(DEFAULT_WAIT_MS);
                let wait = ops::wait(&self.link, term, until, timeout);
                json(&view::waited(&with_progress(wait, context).await?))
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
                let bytes = ops::read_file(&mut res, a.worker.as_deref(), a.path.clone()).await?;
                let text = std::str::from_utf8(&bytes).map_err(|_binary| {
                    anyhow!("{} is not UTF-8 text ({} bytes)", a.path, bytes.len())
                })?;
                json(&view::file(&a.path, text))
            }
            "write_file" => {
                let a: WriteFileArgs = args(arguments)?;
                let bytes = a.content.into_bytes();
                ops::write_file(&mut res, a.worker.as_deref(), a.path, bytes).await?;
                json(&view::DONE)
            }
            "list_ports" => {
                let a: WorkerArgs = args(arguments)?;
                let (worker, ports) = ops::ports(&mut res, a.worker.as_deref()).await?;
                json(&view::ports(worker, &ports))
            }
            other => bail!("no tool is called {other}"),
        }
    }
}

/// Run `wait`, telling the client every [`PROGRESS_EVERY`] that it still waits when the call
/// asked for progress, so a client that counts silence as a hang does not give up on it.
async fn with_progress<T>(
    wait: impl Future<Output = Result<T>>,
    context: &RequestContext<RoleServer>,
) -> Result<T> {
    let Some(token) = context.meta.get_progress_token() else { return wait.await };
    let started = Instant::now();
    let mut ticks = tokio::time::interval(PROGRESS_EVERY);
    ticks.tick().await;
    tokio::pin!(wait);
    loop {
        tokio::select! {
            done = &mut wait => return done,
            _ = ticks.tick() => {
                let waited = started.elapsed().as_secs_f64();
                let note = ProgressNotificationParam::new(token.clone(), waited)
                    .with_message(format!("waited {waited:.0} s"));
                if let Err(e) = context.peer.notify_progress(note).await {
                    tracing::debug!(error = %e, "progress");
                }
            }
        }
    }
}

impl ServerHandler for Slopty {
    fn get_info(&self) -> ServerConfig {
        #[expect(
            deprecated,
            reason = "SEP-2577 deprecates logging, but a `notifications/message` is the one \
                      message a stdio client shows unprompted; Claude Code's channels are a \
                      preview on an older revision"
        )]
        let capabilities = ServerCapabilities::builder().enable_logging().enable_tools().build();
        ServerConfig::new(capabilities)
            .with_protocol_version(REVISION)
            .with_server_info(Implementation::new("slopty", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::known_up_to(&REVISION))
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(tools())))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|t| t.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if self.get_tool(&request.name).is_none() {
            return Err(ErrorData::invalid_params(format!("no tool {}", request.name), None));
        }
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let result = match self.dispatch(&request.name, arguments, &context).await {
            Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))]),
        };
        Ok(result.into())
    }
}

/// Announce each agent that comes to need a human, once per episode, until the client leaves.
async fn forward_needs(mut pushed: broadcast::Receiver<FromServer>, peer: Peer<RoleServer>) {
    let mut names: HashMap<WorkerId, String> = HashMap::new();
    let mut last: HashMap<TermRef, AgentStatus> = HashMap::new();
    loop {
        let msg = match pushed.recv().await {
            Ok(msg) => msg,
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                tracing::warn!(missed, "server events dropped");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        match msg {
            FromServer::Directory(list) => {
                names = list.into_iter().map(|w| (w.worker, w.name)).collect();
            }
            FromServer::Worker(w) => {
                names.insert(w.worker, w.name);
            }
            FromServer::Event(Event::SessionClosed { worker, session }) => {
                last.remove(&TermRef { worker, session });
            }
            FromServer::Event(Event::Agent { worker, event }) => {
                let term = TermRef { worker, session: event.session };
                let before = last.insert(term, event.status.clone());
                let name = names.get(&worker).map(String::as_str);
                if let Some(note) = needs_human(term, name, &event, before.as_ref())
                    && let Err(e) = notify(&peer, note).await
                {
                    tracing::debug!(error = %e, "the MCP client is gone");
                    return;
                }
            }
            _other => {}
        }
    }
}

#[expect(deprecated, reason = "see `get_info`: logging is how a stdio client hears of it")]
async fn notify(peer: &Peer<RoleServer>, (level, data): (Level, Value)) -> Result<()> {
    use rmcp::model::{LoggingLevel, LoggingMessageNotificationParam};
    let level = match level {
        Level::Warning => LoggingLevel::Warning,
        Level::Notice => LoggingLevel::Notice,
    };
    let note = LoggingMessageNotificationParam::new(level, data).with_logger("slopty");
    peer.notify_logging_message(note).await?;
    Ok(())
}

/// How loudly to say it: a question or a permission blocks work, an idle prompt only waits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Level {
    Warning,
    Notice,
}

/// The note for an agent status that newly needs a human; `None` for one that does not, or
/// that already did the same way before.
fn needs_human(
    term: TermRef,
    worker_name: Option<&str>,
    event: &AgentEvent,
    before: Option<&AgentStatus>,
) -> Option<(Level, Value)> {
    let reason = view::blocked(&event.status)?;
    if before == Some(&event.status) {
        return None;
    }
    let level = match reason {
        BlockReason::IdlePrompt => Level::Notice,
        BlockReason::Permission { .. } | BlockReason::Question | BlockReason::Elicitation => {
            Level::Warning
        }
    };
    let agent = view::agent(Some(&(event.kind, event.status.clone())));
    let where_ = worker_name.map_or_else(|| term.worker.to_string(), str::to_owned);
    let what = view::agent_text(Some(&(event.kind, event.status.clone())));
    let message = event.detail.as_ref().map_or_else(
        || format!("{what} (on {where_})"),
        |detail| format!("{what} (on {where_}): {detail}"),
    );
    Some((
        level,
        json!({
            "message": message,
            "term": view::term_string(term),
            "worker_name": worker_name,
            "agent": agent,
            "detail": event.detail,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use slopty_core::SessionId;
    use slopty_proto::agent::{AgentKind, AgentSource};

    use super::*;

    fn event(status: AgentStatus) -> AgentEvent {
        AgentEvent {
            session: SessionId::nil(),
            kind: AgentKind::ClaudeCode,
            status,
            agent_session: None,
            detail: Some("Waiting for permission: Bash".to_owned()),
            attention: true,
            source: AgentSource::Hook,
        }
    }

    #[test]
    fn a_blocked_agent_is_announced_once() {
        let term = TermRef { worker: WorkerId::nil(), session: SessionId::nil() };
        let blocked = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() });
        let (level, data) =
            needs_human(term, Some("mac-studio"), &event(blocked.clone()), None).unwrap();
        assert_eq!(level, Level::Warning);
        assert_eq!(data["agent"]["reason"], "permission");
        assert_eq!(data["agent"]["tool"], "Bash");
        assert_eq!(data["worker_name"], "mac-studio");
        let message = data["message"].as_str().unwrap();
        assert!(
            message.contains("permission for Bash") && message.contains("mac-studio"),
            "{message}"
        );
        assert_eq!(needs_human(term, None, &event(blocked.clone()), Some(&blocked)), None);
        assert_eq!(needs_human(term, None, &event(AgentStatus::Working), None), None);
        let idle = AgentStatus::Blocked(BlockReason::IdlePrompt);
        let (level, _) = needs_human(term, None, &event(idle), Some(&blocked)).unwrap();
        assert_eq!(level, Level::Notice, "an idle prompt waits, it does not block");
    }

    #[test]
    fn every_tool_has_a_schema_and_a_description() {
        let tools = tools();
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
                "agent_status",
                "close_terminal",
                "read_file",
                "write_file",
                "list_ports",
            ]
        );
        for t in &tools {
            assert_eq!(t.input_schema.get("type"), Some(&json!("object")), "{}", t.name);
            assert!(t.description.as_ref().is_some_and(|d| d.len() > 40), "{}", t.name);
        }
        let send = tools.iter().find(|t| t.name == "send_input").unwrap();
        assert!(send.input_schema["properties"]["keys"].is_object(), "{:?}", send.input_schema);
    }

    #[test]
    fn wait_for_takes_exactly_one_condition() {
        let parse = |v: Value| args::<WaitForArgs>(v).and_then(|a| a.until());
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
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn send_input_takes_exactly_one_form() {
        let parse = |v: Value| args::<SendInputArgs>(v).and_then(SendInputArgs::input);
        assert_eq!(
            parse(json!({"term": "t", "keys": ["ctrl+c"]})).unwrap(),
            Input::Keys(vec!["ctrl+c".to_owned()])
        );
        parse(json!({"term": "t", "text": "a", "paste": "b"})).unwrap_err();
        parse(json!({"term": "t"})).unwrap_err();
    }
}
