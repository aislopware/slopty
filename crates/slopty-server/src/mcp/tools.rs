//! The tools: their argument schemas and descriptions, the [`Verb`] each call becomes, and the
//! compact JSON its [`Outcome`] renders as.

use std::borrow::Cow;
use std::collections::BTreeMap;

use rmcp::handler::server::common::{schema_for_empty_input, schema_for_type};
use rmcp::model::{Tool, ToolAnnotations};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use slopty_core::{SessionId, WorkerId};
use slopty_proto::agent::AgentKind;
use slopty_proto::orchestration::{Input, Line, Outcome, TermRef, Verb, WaitUntil, Waited};
use uuid::Uuid;

use crate::hub::WAIT_CAP_MS;

/// Lines [`Verb::ReadOutput`] returns when the call names no limit.
const DEFAULT_MAX_LINES: u32 = 200;
/// How long `wait_for` waits when the call names no timeout.
const DEFAULT_WAIT_MS: u32 = 60_000;

/// Why a call became no verb.
#[derive(Debug)]
pub enum BadCall {
    /// No tool has the name.
    UnknownTool,
    /// The arguments do not fit the tool.
    Arguments(String),
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListTerminalsArgs {
    /// Only this worker's terminals (a worker id from `list_workers`); all workers' when absent.
    worker: Option<Uuid>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpenTerminalArgs {
    /// The worker to run it on.
    worker: Uuid,
    /// Working directory, absolute or `~/…`; the worker's home when absent.
    cwd: Option<String>,
    /// Program and arguments (`["npm", "run", "dev"]`); the user's login shell when absent.
    command: Option<Vec<String>>,
    /// Extra environment variables.
    env: Option<BTreeMap<String, String>>,
    /// A name for the terminal's tile on the user's canvas.
    name: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum AgentArg {
    /// Claude Code.
    ClaudeCode,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SpawnAgentArgs {
    /// The worker to run it on.
    worker: Uuid,
    /// Working directory, usually a repository root.
    cwd: String,
    /// The first prompt, typed once the agent is ready.
    prompt: Option<String>,
    /// Which agent; `claude_code` when absent.
    agent: Option<AgentArg>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TermArgs {
    /// The worker the terminal runs on.
    worker: Uuid,
    /// The terminal's session id.
    session: Uuid,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendInputArgs {
    /// The worker the terminal runs on.
    worker: Uuid,
    /// The terminal's session id.
    session: Uuid,
    /// Text typed as-is; a newline presses Enter.
    text: Option<String>,
    /// Text delivered as one paste (bracketed when the program asked for it).
    paste: Option<String>,
    /// Named keys pressed in order, each `[mods+]key`: "enter", "ctrl+c", "up", "shift+tab",
    /// "escape"; mods are ctrl, alt, shift, cmd.
    keys: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadOutputArgs {
    /// The worker the terminal runs on.
    worker: Uuid,
    /// The terminal's session id.
    session: Uuid,
    /// First absolute line wanted (a previous call's `next`); the oldest retained when absent.
    since: Option<u64>,
    /// At most this many lines (default 200).
    max_lines: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListCommandsArgs {
    /// The worker the terminal runs on.
    worker: Uuid,
    /// The terminal's session id.
    session: Uuid,
    /// Only commands whose prompt is at or after this absolute line.
    since: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum UntilArg {
    /// A line printed after the call started matches `pattern`.
    Output,
    /// No output for `quiet_ms`.
    Quiet,
    /// The running command finishes, or the next one if none runs.
    CommandDone,
    /// The terminal's program exits.
    Exit,
    /// The agent in the terminal is idle or blocked on a human.
    AgentNeedsInput,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WaitForArgs {
    /// The worker the terminal runs on.
    worker: Uuid,
    /// The terminal's session id.
    session: Uuid,
    /// What to wait for.
    until: UntilArg,
    /// For `until: output`: a regular expression one line must match.
    pattern: Option<String>,
    /// For `until: quiet`: milliseconds without output.
    quiet_ms: Option<u32>,
    /// Give up after this long (default 60000, at most 240000).
    timeout_ms: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WorkerArgs {
    /// The worker.
    worker: Uuid,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    /// The worker.
    worker: Uuid,
    /// Absolute path, or `~/…`.
    path: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WriteFileArgs {
    /// The worker.
    worker: Uuid,
    /// Absolute path, or `~/…`.
    path: String,
    /// The new contents as text.
    text: Option<String>,
    /// The new contents as base64, for binary files.
    base64: Option<String>,
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

fn tool(name: &'static str, description: &'static str, kind: Kind) -> Tool {
    tool_with(name, description, kind, schema_for_empty_input())
}

fn tool_of<T: JsonSchema + 'static>(
    name: &'static str,
    description: &'static str,
    kind: Kind,
) -> Tool {
    tool_with(name, description, kind, schema_for_type::<T>())
}

fn tool_with(
    name: &'static str,
    description: &'static str,
    kind: Kind,
    schema: std::sync::Arc<rmcp::model::JsonObject>,
) -> Tool {
    let hints = ToolAnnotations::new().open_world(false);
    let hints = match kind {
        Kind::Read => hints.read_only(true),
        Kind::Write => hints.read_only(false).destructive(false),
        Kind::Destroy => hints.read_only(false).destructive(true),
    };
    Tool::new(Cow::Borrowed(name), Cow::Borrowed(description), schema).with_annotations(hints)
}

/// Every tool.
pub fn all() -> Vec<Tool> {
    vec![
        tool(
            "list_workers",
            "Every machine (worker) the server knows: id, name, address, liveness (Online, \
             Unreachable, Gone) and capabilities (OS, CPUs, memory, displays, installed agents, \
             load). Start here: the `worker` id every other tool takes comes from this list, and \
             only Online workers accept commands.",
            Kind::Read,
        ),
        tool_of::<ListTerminalsArgs>(
            "list_terminals",
            "The terminals on one worker, or on every worker: each with its `worker`, its `id` \
             (pass it as `session`), title, cwd, repository, size and state. A worker that is \
             not Online lists its last known terminals.",
            Kind::Read,
        ),
        tool_of::<OpenTerminalArgs>(
            "open_terminal",
            "Start a terminal on a worker; returns its handle {worker, session}. It runs the \
             user's login shell in `cwd` (home by default) unless `command` names a program. \
             It also appears on the user's canvas. Next: send_input, then wait_for, then \
             read_output.",
            Kind::Write,
        ),
        tool_of::<SpawnAgentArgs>(
            "spawn_agent",
            "Start a coding agent's TUI (Claude Code) in a new terminal in `cwd`, usually a \
             repository, and type `prompt` once it is ready. Returns {worker, session}. Follow \
             it with wait_for until=agent_needs_input, then read_screen or agent_status.",
            Kind::Write,
        ),
        tool_of::<SendInputArgs>(
            "send_input",
            "Type into a terminal. Give exactly one of `text` (typed as-is; end a shell command \
             with \\n to run it), `paste` (one paste, for multi-line text into editors and \
             agents) or `keys` (named keys: [\"enter\"], [\"ctrl+c\"], [\"up\", \"enter\"], \
             [\"escape\"]). Returns once delivered; wait_for shows the effect.",
            Kind::Write,
        ),
        tool_of::<TermArgs>(
            "read_screen",
            "The terminal's screen as drawn now: `lines` top to bottom with `first` (the \
             absolute index of the top row), `cursor` [row, col], title, cwd, and `alternate` \
             (a full-screen program such as an editor or an agent runs). Use it for TUIs; for a \
             command's output prefer read_output, and to wait prefer wait_for over polling this.",
            Kind::Read,
        ),
        tool_of::<ReadOutputArgs>(
            "read_output",
            "Scrollback and screen lines from absolute line `since` on (the oldest retained \
             when absent), at most `max_lines` (default 200). Returns `lines`, `from` (the index \
             of the first) and `next`: pass `next` as `since` to page on without gaps or \
             repeats. list_commands gives the lines of one command's output.",
            Kind::Read,
        ),
        tool_of::<ListCommandsArgs>(
            "list_commands",
            "The shell commands a terminal ran, oldest first (OSC 133 marks): each command \
             line, its prompt line, the absolute lines of its output [start, end) for \
             read_output, and its exit code (null while it runs). Shells without the marks \
             report none.",
            Kind::Read,
        ),
        tool_of::<WaitForArgs>(
            "wait_for",
            "Block until something happens in a terminal, instead of polling. `until`: output \
             (a new line matches the regex `pattern`), quiet (no output for `quiet_ms`), \
             command_done (the running command finishes), exit (the program exits) or \
             agent_needs_input (the agent is idle or needs a human). `timeout_ms` defaults to \
             60000 and is capped at 240000. Answers {\"result\": \"met\"} with the matching \
             line, \"timed_out\" (call again to keep waiting) or \"closed\".",
            Kind::Read,
        ),
        tool_of::<TermArgs>(
            "agent_status",
            "The coding agent in a terminal and what it is doing: Idle, Working, Tool (running \
             one), Blocked (needs a human: a permission or a question) or Done. `agent` is null \
             when none runs.",
            Kind::Read,
        ),
        tool_of::<TermArgs>(
            "close_terminal",
            "Close a terminal, hanging up its program. Its handle is dead afterwards.",
            Kind::Destroy,
        ),
        tool_of::<ReadFileArgs>(
            "read_file",
            "Read a file on a worker. UTF-8 files come back as `text`, others as `base64`.",
            Kind::Read,
        ),
        tool_of::<WriteFileArgs>(
            "write_file",
            "Create or replace a file on a worker with `text`, or `base64` for binary \
             contents; give exactly one.",
            Kind::Destroy,
        ),
        tool_of::<WorkerArgs>(
            "list_ports",
            "TCP ports listening in the process trees of a worker's terminals: number, pid, \
             process and the `session` that owns it; how to find the dev server a terminal \
             started.",
            Kind::Read,
        ),
    ]
}

fn args<T: DeserializeOwned>(arguments: Map<String, Value>) -> Result<T, BadCall> {
    serde_json::from_value(Value::Object(arguments)).map_err(|e| BadCall::Arguments(e.to_string()))
}

const fn term(worker: Uuid, session: Uuid) -> TermRef {
    TermRef { worker: WorkerId::from_uuid(worker), session: SessionId::from_uuid(session) }
}

fn invalid(why: &str) -> BadCall {
    BadCall::Arguments(why.to_owned())
}

/// The verb a call to `name` with `arguments` stands for.
pub fn verb(name: &str, arguments: Map<String, Value>) -> Result<Verb, BadCall> {
    Ok(match name {
        "list_workers" => Verb::ListWorkers,
        "list_terminals" => {
            let a: ListTerminalsArgs = args(arguments)?;
            Verb::ListTerminals { worker: a.worker.map(WorkerId::from_uuid) }
        }
        "open_terminal" => {
            let a: OpenTerminalArgs = args(arguments)?;
            Verb::OpenTerminal {
                worker: WorkerId::from_uuid(a.worker),
                cwd: a.cwd,
                command: a.command.unwrap_or_default(),
                env: a.env.unwrap_or_default().into_iter().collect(),
                name: a.name,
            }
        }
        "spawn_agent" => {
            let a: SpawnAgentArgs = args(arguments)?;
            let agent = match a.agent {
                None | Some(AgentArg::ClaudeCode) => AgentKind::ClaudeCode,
            };
            Verb::SpawnAgent {
                worker: WorkerId::from_uuid(a.worker),
                agent,
                cwd: a.cwd,
                prompt: a.prompt,
            }
        }
        "send_input" => {
            let a: SendInputArgs = args(arguments)?;
            let input = match (a.text, a.paste, a.keys) {
                (Some(text), None, None) => Input::Text(text),
                (None, Some(paste), None) => Input::Paste(paste),
                (None, None, Some(keys)) => Input::Keys(keys),
                _ => return Err(invalid("give exactly one of text, paste or keys")),
            };
            Verb::SendInput { term: term(a.worker, a.session), input }
        }
        "read_screen" => {
            let a: TermArgs = args(arguments)?;
            Verb::ReadScreen { term: term(a.worker, a.session) }
        }
        "read_output" => {
            let a: ReadOutputArgs = args(arguments)?;
            Verb::ReadOutput {
                term: term(a.worker, a.session),
                since: a.since,
                max_lines: a.max_lines.unwrap_or(DEFAULT_MAX_LINES),
            }
        }
        "list_commands" => {
            let a: ListCommandsArgs = args(arguments)?;
            Verb::ListCommands { term: term(a.worker, a.session), since: a.since }
        }
        "wait_for" => {
            let a: WaitForArgs = args(arguments)?;
            let until = match (a.until, a.pattern, a.quiet_ms) {
                (UntilArg::Output, Some(pattern), None) => WaitUntil::Output(pattern),
                (UntilArg::Quiet, None, Some(ms)) => WaitUntil::Quiet { ms },
                (UntilArg::CommandDone, None, None) => WaitUntil::CommandDone,
                (UntilArg::Exit, None, None) => WaitUntil::Exit,
                (UntilArg::AgentNeedsInput, None, None) => WaitUntil::AgentNeedsInput,
                (UntilArg::Output, ..) => {
                    return Err(invalid("until output takes `pattern` alone"));
                }
                (UntilArg::Quiet, ..) => return Err(invalid("until quiet takes `quiet_ms` alone")),
                _ => {
                    return Err(invalid(
                        "`pattern` goes with until output and `quiet_ms` with until quiet",
                    ));
                }
            };
            Verb::WaitFor {
                term: term(a.worker, a.session),
                until,
                timeout_ms: a.timeout_ms.unwrap_or(DEFAULT_WAIT_MS).min(WAIT_CAP_MS),
            }
        }
        "agent_status" => {
            let a: TermArgs = args(arguments)?;
            Verb::AgentStatus { term: term(a.worker, a.session) }
        }
        "close_terminal" => {
            let a: TermArgs = args(arguments)?;
            Verb::Close { term: term(a.worker, a.session) }
        }
        "read_file" => {
            let a: ReadFileArgs = args(arguments)?;
            Verb::ReadFile { worker: WorkerId::from_uuid(a.worker), path: a.path }
        }
        "write_file" => {
            let a: WriteFileArgs = args(arguments)?;
            let bytes = match (a.text, a.base64) {
                (Some(text), None) => text.into_bytes(),
                (None, Some(encoded)) => data_encoding::BASE64
                    .decode(encoded.as_bytes())
                    .map_err(|e| BadCall::Arguments(format!("base64: {e}")))?,
                _ => return Err(invalid("give exactly one of text or base64")),
            };
            Verb::WriteFile { worker: WorkerId::from_uuid(a.worker), path: a.path, bytes }
        }
        "list_ports" => {
            let a: WorkerArgs = args(arguments)?;
            Verb::ListPorts { worker: WorkerId::from_uuid(a.worker) }
        }
        _ => return Err(BadCall::UnknownTool),
    })
}

/// An outcome as compact JSON text, and whether it is an error.
pub fn render(outcome: Outcome) -> (String, bool) {
    let failed = matches!(outcome, Outcome::Error { .. });
    let value = match outcome {
        Outcome::Workers(workers) => json!(workers),
        Outcome::Terminals(terminals) => Value::Array(
            terminals
                .into_iter()
                .map(|(worker, summary)| {
                    let mut entry = json!(summary);
                    if let Value::Object(fields) = &mut entry {
                        fields.insert("worker".to_owned(), json!(worker));
                    }
                    entry
                })
                .collect(),
        ),
        Outcome::Opened(TermRef { worker, session }) => {
            json!({ "worker": worker, "session": session })
        }
        Outcome::Screen(screen) => json!({
            "first": screen.lines.first().map(|l| l.index),
            "lines": texts(screen.lines),
            "cursor": [screen.cursor.0, screen.cursor.1],
            "title": screen.title,
            "cwd": screen.cwd,
            "alternate": screen.alternate,
        }),
        Outcome::Output { lines, next } => json!({
            "from": lines.first().map(|l| l.index),
            "lines": texts(lines),
            "next": next,
        }),
        Outcome::Commands(commands) => json!(commands),
        Outcome::Waited(Waited::Met { line }) => json!({ "result": "met", "line": line }),
        Outcome::Waited(Waited::TimedOut) => json!({ "result": "timed_out" }),
        Outcome::Waited(Waited::Closed) => json!({ "result": "closed" }),
        Outcome::Agent(None) => json!({ "agent": null }),
        Outcome::Agent(Some((agent, status))) => json!({ "agent": agent, "status": status }),
        Outcome::File(bytes) => match String::from_utf8(bytes) {
            Ok(text) => json!({ "text": text }),
            Err(e) => json!({ "base64": data_encoding::BASE64.encode(e.as_bytes()) }),
        },
        Outcome::Ports(ports) => json!(ports),
        Outcome::Done => json!({ "ok": true }),
        Outcome::Error { code, message } => json!({ "error": code, "message": message }),
    };
    (value.to_string(), failed)
}

fn texts(lines: Vec<Line>) -> Vec<String> {
    lines.into_iter().map(|l| l.text).collect()
}

#[cfg(test)]
mod tests {
    use slopty_proto::orchestration::ErrorCode;

    use super::*;

    fn call(name: &str, arguments: Value) -> Result<Verb, BadCall> {
        let Value::Object(arguments) = arguments else { panic!("an object") };
        verb(name, arguments)
    }

    #[test]
    fn every_tool_has_a_verb_and_a_schema() {
        let tools = all();
        assert_eq!(tools.len(), 14, "one per verb");
        for tool in &tools {
            assert_eq!(tool.input_schema.get("type"), Some(&json!("object")), "{}", tool.name);
            let parsed = verb(&tool.name, Map::new());
            assert!(!matches!(parsed, Err(BadCall::UnknownTool)), "{} has a verb", tool.name);
        }
        assert!(matches!(verb("nope", Map::new()), Err(BadCall::UnknownTool)));
    }

    #[test]
    fn arguments_become_verbs() {
        let (w, s) = (Uuid::new_v4(), Uuid::new_v4());
        let t = term(w, s);
        let keys = call("send_input", json!({ "worker": w, "session": s, "keys": ["enter"] }));
        assert_eq!(
            keys.unwrap(),
            Verb::SendInput { term: t, input: Input::Keys(vec!["enter".to_owned()]) }
        );
        let both =
            call("send_input", json!({ "worker": w, "session": s, "text": "a", "keys": [] }));
        assert!(matches!(both, Err(BadCall::Arguments(_))));
        let wait = call(
            "wait_for",
            json!({ "worker": w, "session": s, "until": "output", "pattern": "ready", "timeout_ms": 999_999 }),
        );
        assert_eq!(
            wait.unwrap(),
            Verb::WaitFor {
                term: t,
                until: WaitUntil::Output("ready".to_owned()),
                timeout_ms: WAIT_CAP_MS
            }
        );
        let quiet = call("wait_for", json!({ "worker": w, "session": s, "until": "quiet" }));
        assert!(matches!(quiet, Err(BadCall::Arguments(_))), "quiet needs quiet_ms");
        let done = call("wait_for", json!({ "worker": w, "session": s, "until": "command_done" }));
        assert!(matches!(
            done.unwrap(),
            Verb::WaitFor { until: WaitUntil::CommandDone, timeout_ms: DEFAULT_WAIT_MS, .. }
        ));
        let open = call("open_terminal", json!({ "worker": w, "env": { "A": "1" } })).unwrap();
        assert!(
            matches!(open, Verb::OpenTerminal { env, command, .. } if env == vec![("A".to_owned(), "1".to_owned())] && command.is_empty())
        );
        let bad = call("read_screen", json!({ "worker": "not-a-uuid", "session": s }));
        assert!(matches!(bad, Err(BadCall::Arguments(_))));
        let extra = call("read_screen", json!({ "worker": w, "session": s, "colour": 1 }));
        assert!(matches!(extra, Err(BadCall::Arguments(_))), "unknown fields are refused");
        let file = call("write_file", json!({ "worker": w, "path": "/tmp/x", "base64": "AAE=" }));
        assert!(matches!(file.unwrap(), Verb::WriteFile { bytes, .. } if bytes == [0, 1]));
    }

    #[test]
    fn outcomes_render_compactly() {
        let lines =
            vec![Line { index: 7, text: "a".to_owned() }, Line { index: 8, text: "b".to_owned() }];
        let (text, failed) = render(Outcome::Output { lines, next: 9 });
        assert_eq!(text, r#"{"from":7,"lines":["a","b"],"next":9}"#);
        assert!(!failed);
        let (text, failed) =
            render(Outcome::Error { code: ErrorCode::WorkerUnreachable, message: "m".to_owned() });
        assert_eq!(text, r#"{"error":"WorkerUnreachable","message":"m"}"#);
        assert!(failed);
        let (w, s) = (Uuid::new_v4(), Uuid::new_v4());
        let (text, _) = render(Outcome::Opened(term(w, s)));
        assert_eq!(
            serde_json::from_str::<Value>(&text).ok(),
            Some(serde_json::json!({ "worker": w.to_string(), "session": s.to_string() })),
            "plain UUID handles"
        );
        let (text, _) = render(Outcome::File(vec![0xff, 0]));
        assert_eq!(text, r#"{"base64":"/wA="}"#);
    }
}
