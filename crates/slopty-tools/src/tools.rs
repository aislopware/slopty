//! The agents' verbs as MCP tools: names, descriptions, hints, argument schemas, and a call
//! run end to end (arguments parsed, names resolved, the verb sent, the answer rendered as its
//! view).
//!
//! Every MCP surface serves exactly this list and this [`call`], so a model sees the same tools
//! and gets the same JSON whether it reaches the server's endpoint or `slopty mcp`. They are
//! the primitives an agent at any depth uses to start, reach and read other agents across the
//! fleet; every other verb is the `slopty` command, which an agent runs through its shell like
//! any other.

use std::collections::HashMap;
use std::time::Duration;

use rmcp::ErrorData;
use rmcp::handler::server::common::schema_for_type;
use rmcp::model::{CallToolResult, ContentBlock, Tool, ToolAnnotations};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use slopty_core::WorkerId;
use slopty_proto::orchestration::{ErrorCode, IdempotencyKey, Outcome, ThreadView, Verb};

use crate::ops::{self, AgentWanted};
use crate::resolve::Resolver;
use crate::show::{self, ShowPageArgs, ShowVisualArgs};
use crate::{Dispatch, ToolError, ui, view};

/// What the model reads before any tool description.
pub const INSTRUCTIONS: &str = "\
Slopty runs agents (Claude Code, Codex, pi, ACP agents) on a fleet of machines. machines lists \
the machines and their facts. agent_start starts an agent on one, which records you as its \
parent; when it comes to rest or needs the person, you hear it through your hooks. agent_tell \
sends an agent words, agent_read reads its thread, agent_wait waits for agents to come to \
rest, agent_stop ends one with the agents under it, and agents lists every agent with its \
parent. branch_bring copies a branch from a clone on one machine to a clone on another. \
Requests an agent asks are the person's to answer. show_visual shows the person a native \
visual (a chart, table, dashboard, diagram, simulation or 3D view) inline, and show_page a web \
page, each above your reply. ui_state reads the person's Slopty window (its regions, tabs and \
the chat on screen); ui_terminal, ui_open, ui_region, ui_chat and ui_split change it, never \
taking the keyboard from the person. Everything else is the `slopty` command: `slopty --help` \
lists it.";

/// How often a long wait with a progress sink reports that it is still waiting.
pub const PROGRESS_EVERY: Duration = Duration::from_secs(10);

/// Told how long a long wait has waited so far, every [`PROGRESS_EVERY`].
pub type Progress<'a> = &'a (dyn Fn(Duration) + Send + Sync);

/// `agent_start`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AgentStartArgs {
    /// The machine, by name or id.
    machine: String,
    /// What the agent is told first.
    prompt: String,
    /// The folder it works in, absolute or `~/…`.
    folder: Option<String>,
    /// A remote repository's address: the machine's clone of it is the folder, made first
    /// when there is none.
    repo: Option<String>,
    /// A fresh git worktree of the folder's clone, by this name, on the branch
    /// `worktree-<name>`.
    worktree: Option<String>,
    /// `claude` (the default), `codex`, `pi`, or an ACP agent by its registry name.
    agent: Option<String>,
    /// The model, by the agent's own id.
    model: Option<String>,
    /// How hard it thinks, by the agent's own name.
    effort: Option<String>,
    /// A name for this call's effect, such as a fresh UUID; a repeat answers as the first did.
    idempotency_key: Option<String>,
}

/// `agent_tell`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AgentTellArgs {
    /// The agent's id.
    agent: String,
    /// The words.
    text: String,
    /// A name for this call's effect, such as a fresh UUID; a repeat answers as the first did.
    idempotency_key: Option<String>,
}

/// `agent_read`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AgentReadArgs {
    /// The agent's id, or a subagent's thread id.
    agent: String,
    /// `messages` (the default) for what was said, `activity` for that and every tool call
    /// with the end of its output.
    #[serde(default)]
    view: ViewArg,
    /// The last turn already read: the `next` of the previous read. From the first turn held
    /// when omitted.
    after: Option<u32>,
}

/// `agent_wait`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AgentWaitArgs {
    /// The agents' ids, at most 64.
    agents: Vec<String>,
    /// Wait for every one; the first one when false (the default).
    #[serde(default)]
    all: bool,
    /// How long to wait, in milliseconds, at most 240000; 60000 when omitted.
    timeout_ms: Option<u32>,
}

/// `agent_stop`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AgentStopArgs {
    /// The agent's id.
    agent: String,
    /// A name for this call's effect, such as a fresh UUID; a repeat answers as the first did.
    idempotency_key: Option<String>,
}

/// `branch_bring`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BranchBringArgs {
    /// The machine the branch is on, by name or id.
    from: String,
    /// The clone or worktree it is in there.
    repo: String,
    /// The branch.
    branch: String,
    /// The machine it goes to, by name or id.
    to: String,
    /// The clone it lands in there; that machine's clone of the same repository when omitted.
    into: Option<String>,
    /// The name it lands as; its own when omitted.
    name: Option<String>,
    /// A name for this call's effect, such as a fresh UUID; a repeat answers as the first did.
    idempotency_key: Option<String>,
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

/// How a tool behaves, for the client's hints.
#[derive(Clone, Copy)]
enum Kind {
    /// Reads only.
    Read,
    /// Changes something, nothing lost.
    Write,
    /// Ends something.
    Ends,
}

fn tool<T: JsonSchema + 'static>(
    name: &'static str,
    description: &'static str,
    kind: Kind,
) -> Tool {
    annotated(Tool::new(name, description, schema_for_type::<T>()), kind)
}

/// A tool that takes no arguments: an object with none.
fn bare(name: &'static str, description: &'static str, kind: Kind) -> Tool {
    let mut schema = Map::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("properties".to_owned(), json!({}));
    schema.insert("additionalProperties".to_owned(), json!(false));
    annotated(Tool::new(name, description, std::sync::Arc::new(schema)), kind)
}

fn annotated(tool: Tool, kind: Kind) -> Tool {
    let hints = ToolAnnotations::new().open_world(false);
    let hints = match kind {
        Kind::Read => hints.read_only(true),
        Kind::Write => hints.read_only(false).destructive(false),
        Kind::Ends => hints.read_only(false).destructive(true),
    };
    tool.annotate(hints)
}

/// No arguments, as a tool that takes none was called.
fn none(arguments: &Map<String, Value>) -> Result<(), ToolError> {
    match arguments.keys().next() {
        Some(key) => Err(ToolError::invalid(format!("bad arguments: unknown field `{key}`"))),
        None => Ok(()),
    }
}

/// Every tool, in the order a model meets them.
#[must_use]
pub fn list() -> Vec<Tool> {
    vec![
        bare(
            "machines",
            "Every machine on the fleet: its `name`, `liveness`, `os`, and its `facts` (the \
             agents installed, the repositories it has clones of under `repos`, cpus, memory, \
             load, the agents live there).",
            Kind::Read,
        ),
        tool::<AgentStartArgs>(
            "agent_start",
            "Start an agent on a machine, in a `folder` or the machine's clone of a `repo`, \
             optionally in a fresh `worktree`, told `prompt` first. It acts as freely as the \
             person's autonomy setting allows, and has these tools too. You are recorded as its \
             parent: its rest, or its need of the person, reaches you through your hooks. \
             Returns its `agent` id and the `worktree` it works in.",
            Kind::Write,
        ),
        tool::<AgentTellArgs>(
            "agent_tell",
            "Send an agent of your own tree words, marked as yours and not the person's. They \
             reach it through its hooks, waking it when it is idle. Returns once they are queued.",
            Kind::Write,
        ),
        tool::<AgentReadArgs>(
            "agent_read",
            "An agent's thread: whole `turns` after `after`, each with its `entries` (`user` and \
             `agent` messages; with view `activity` also each `tool` call with its `state`, the \
             end of its `output` and the `child` thread it started), its `phase`, what it \
             `wait`s on, and the `requests` open on it. `truncated` says something was cut; \
             `next` is the `after` to read on from.",
            Kind::Read,
        ),
        tool::<AgentWaitArgs>(
            "agent_wait",
            "Wait until one of `agents` (or each, with `all`) has come to rest, needs the \
             person or ended. Returns each as it stands (`phase`, `asks`, `last` words, \
             `rests`) and `timed_out` when time ran out first.",
            Kind::Read,
        ),
        tool::<AgentStopArgs>(
            "agent_stop",
            "End an agent and every agent under it, their conversations kept.",
            Kind::Ends,
        ),
        bare(
            "agents",
            "Every agent on the fleet that runs or was started by another: its `id`, `parent`, \
             `worker`, `agent`, `title`, `folder`, `branch`, `phase`, what it `asks`, its \
             `last` words.",
            Kind::Read,
        ),
        tool::<BranchBringArgs>(
            "branch_bring",
            "Copy `branch` from the clone `repo` on machine `from` into a clone on machine \
             `to`, as `name`. Returns the `branch` it landed as and its `head` commit.",
            Kind::Write,
        ),
        tool::<ShowVisualArgs>("show_visual", show::SHOW_VISUAL_DESCRIPTION, Kind::Read),
        tool::<ShowPageArgs>("show_page", show::SHOW_PAGE_DESCRIPTION, Kind::Read),
        bare("ui_state", ui::STATE_DESCRIPTION, Kind::Read),
        tool::<ui::TerminalArgs>("ui_terminal", ui::TERMINAL_DESCRIPTION, Kind::Write),
        tool::<ui::OpenArgs>("ui_open", ui::OPEN_DESCRIPTION, Kind::Write),
        tool::<ui::RegionArgs>("ui_region", ui::REGION_DESCRIPTION, Kind::Write),
        tool::<ui::ChatArgs>("ui_chat", ui::CHAT_DESCRIPTION, Kind::Write),
        tool::<ui::OpenArgs>("ui_split", ui::SPLIT_DESCRIPTION, Kind::Write),
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
    let Some(tool) = get(name) else {
        return Err(ErrorData::invalid_params(format!("no tool is called {name}"), None));
    };
    // The window's answer is its own JSON, a failure's with its code.
    if ui::NAMES.contains(&name) {
        let text = |value: Value| vec![ContentBlock::text(value.to_string())];
        let ran = match here() {
            Ok(cwd) => ui::call(dispatch, name, arguments, &cwd).await,
            Err(e) => Err(e),
        };
        return Ok(match ran {
            Ok(Ok(done)) => CallToolResult::success(text(done)),
            Ok(Err(failure)) => CallToolResult::error(text(failure)),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]),
        });
    }
    // A change waits out a server it cannot reach, its lookups too; a read answers at once.
    let changes = tool.annotations.as_ref().and_then(|a| a.read_only_hint) == Some(false);
    let ran = if changes {
        run(&crate::Waiting(dispatch), name, arguments, progress).await
    } else {
        run(dispatch, name, arguments, progress).await
    };
    Ok(match ran {
        Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
        Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]),
    })
}

fn args<T: DeserializeOwned>(arguments: Map<String, Value>) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(arguments))
        .map_err(|e| ToolError::invalid(format!("bad arguments: {e}")))
}

fn to_json(value: &impl serde::Serialize) -> Result<Value, ToolError> {
    serde_json::to_value(value).map_err(|e| ToolError::new(ErrorCode::Failed, e.to_string()))
}

/// Every machine's name, by its id; none when the server cannot say.
async fn names<D: Dispatch>(dispatch: &D) -> HashMap<WorkerId, String> {
    match dispatch.call(Verb::ListWorkers).await {
        Outcome::Workers(list) => list.into_iter().map(|w| (w.worker, w.name)).collect(),
        _ => HashMap::new(),
    }
}

async fn run<D: Dispatch>(
    dispatch: &D,
    name: &str,
    arguments: Map<String, Value>,
    progress: Option<Progress<'_>>,
) -> Result<Value, ToolError> {
    let mut res = Resolver::new(dispatch);
    match name {
        "machines" => {
            none(&arguments)?;
            let overview = ops::overview(dispatch).await?;
            to_json(&overview.json())
        }
        "agent_start" => {
            let a: AgentStartArgs = args(arguments)?;
            let key = checked_key(a.idempotency_key)?;
            let wanted = AgentWanted {
                worker: a.machine,
                folder: a.folder,
                prompt: a.prompt,
                agent: a.agent,
                model: a.model,
                effort: a.effort,
                worktree: a.worktree,
                repo: a.repo,
            };
            let (agent, worktree) = ops::agent_start(&mut res, wanted, key).await?;
            Ok(json!({ "agent": agent.to_string(), "worktree": worktree }))
        }
        "agent_tell" => {
            let a: AgentTellArgs = args(arguments)?;
            let key = checked_key(a.idempotency_key)?;
            ops::agent_tell(dispatch, ops::agent_named(&a.agent)?, a.text, key).await?;
            to_json(&view::DONE)
        }
        "agent_read" => {
            let a: AgentReadArgs = args(arguments)?;
            let id = ops::agent_named(&a.agent)?.to_string();
            let of = ops::ThreadArg { thread: Some(&id), term: None };
            let read = ops::read_thread(&mut res, of, a.view.view(), a.after).await?;
            to_json(&view::thread_read(&read))
        }
        "agent_wait" => {
            let a: AgentWaitArgs = args(arguments)?;
            let agents =
                a.agents.iter().map(|a| ops::agent_named(a)).collect::<Result<Vec<_>, _>>()?;
            let timeout = a.timeout_ms.unwrap_or(ops::DEFAULT_WAIT_MS);
            let waited = ops::agent_wait(dispatch, agents, a.all, timeout);
            let (agents, timed_out) = with_progress(waited, progress).await?;
            to_json(&view::agents_tree::agents(&agents, timed_out, &names(dispatch).await))
        }
        "agent_stop" => {
            let a: AgentStopArgs = args(arguments)?;
            let key = checked_key(a.idempotency_key)?;
            ops::agent_stop(dispatch, ops::agent_named(&a.agent)?, key).await?;
            to_json(&view::DONE)
        }
        "agents" => {
            none(&arguments)?;
            let agents = ops::agents(dispatch).await?;
            to_json(&view::agents_tree::agents(&agents, false, &names(dispatch).await))
        }
        "branch_bring" => {
            let a: BranchBringArgs = args(arguments)?;
            let key = checked_key(a.idempotency_key)?;
            let from = (a.from.as_str(), a.repo, a.branch);
            let to = (a.to.as_str(), a.into, a.name);
            let (branch, head) = ops::branch_bring(&mut res, from, to, key).await?;
            Ok(json!({ "branch": branch, "head": head }))
        }
        "show_visual" => show::visual(args(arguments)?, &here()?),
        "show_page" => show::page(args(arguments)?, &here()?),
        other => Err(ToolError::invalid(format!("no tool is called {other}"))),
    }
}

/// Where the agent works: the folder it started its MCP server in, which its paths are
/// relative to.
fn here() -> Result<std::path::PathBuf, ToolError> {
    std::env::current_dir()
        .map_err(|e| ToolError::new(ErrorCode::Failed, format!("no working folder: {e}")))
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
    use slopty_core::WallMs;
    use slopty_proto::agent::AgentEntry;
    use slopty_proto::orchestration::{ThreadOf, ThreadRead};
    use slopty_proto::server::{Liveness, Os, WorkerCaps, WorkerInfo};
    use slopty_proto::thread::{AgentId, Phase, ThreadId, TurnId};

    use super::*;

    fn studio() -> WorkerId {
        "0199a000-0000-7000-8000-000000000001".parse().unwrap()
    }

    fn child() -> ThreadId {
        ThreadId::derived(&["child"])
    }

    /// One online worker, `mac-studio`; records every verb and the key that came with it. A
    /// wait takes 25 s.
    #[derive(Default)]
    struct Fake {
        verbs: Mutex<Vec<Verb>>,
        keys: Mutex<Vec<Option<IdempotencyKey>>>,
        /// What the person's window was asked.
        windows: Mutex<Vec<slopty_proto::handoff::UiAct>>,
    }

    impl Fake {
        fn last(&self) -> Option<Verb> {
            self.verbs.lock().iter().rev().find(|v| !matches!(v, Verb::ListWorkers)).cloned()
        }
    }

    fn entry(id: ThreadId) -> AgentEntry {
        AgentEntry {
            id,
            parent: Some(ThreadId::derived(&["parent"])),
            worker: studio(),
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            title: "Fix the login".to_owned(),
            folder: Some("/src/app".to_owned()),
            branch: Some("worktree-login".to_owned()),
            phase: Phase::Idle,
            wait: None,
            ended: false,
            asks: None,
            last: Some("Done.".to_owned()),
            since_ms: WallMs::ZERO,
            rests: true,
        }
    }

    impl Dispatch for Fake {
        fn window(
            &self,
            act: slopty_proto::handoff::UiAct,
        ) -> impl Future<
            Output = Result<slopty_proto::handoff::UiDone, slopty_proto::handoff::UiFailure>,
        > + Send {
            self.windows.lock().push(act);
            std::future::ready(Ok(slopty_proto::handoff::UiDone {
                did: Some("showed the sidebar".to_owned()),
                ..slopty_proto::handoff::UiDone::default()
            }))
        }

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
                        curtain: false,
                        build: "0".to_owned(),
                        lan: Vec::new(),
                        wake_on_lan: None,
                        writes_failing: None,
                        stops_at_logout: None,
                    },
                    load: 0.0,
                    last_seen_ms: WallMs::ZERO,
                }]),
                Verb::ListTerminals { .. } => {
                    Outcome::Terminals { agents: Vec::new(), terminals: Vec::new() }
                }
                Verb::WorkerFacts { .. } => Outcome::Facts(Vec::new()),
                Verb::AgentStart(_) => Outcome::ThreadStarted { thread: child(), worktree: None },
                Verb::AgentWait { agents, .. } => {
                    tokio::time::sleep(Duration::from_secs(25)).await;
                    let agents = agents.into_iter().map(entry).collect();
                    Outcome::Agents { agents, timed_out: false }
                }
                Verb::Agents => Outcome::Agents { agents: vec![entry(child())], timed_out: false },
                Verb::AgentTell { agent, .. } if agent == ThreadId::derived(&["stranger"]) => {
                    Outcome::Error {
                        code: ErrorCode::Forbidden,
                        message: "not of this tree".to_owned(),
                    }
                }
                Verb::ReadThread { of, after, .. } => {
                    let ThreadOf::Thread(thread) = of else { panic!("{of:?}") };
                    Outcome::Thread(Box::new(ThreadRead {
                        worker: studio(),
                        thread,
                        agent: AgentId::named(AgentId::PI),
                        title: "Fix the build".to_owned(),
                        parent: None,
                        phase: Phase::Working,
                        wait: None,
                        turns: Vec::new(),
                        requests: Vec::new(),
                        next: after.unwrap_or(TurnId(0)),
                        truncated: false,
                        skipped: false,
                    }))
                }
                Verb::BranchBring { branch, .. } => {
                    Outcome::Fetched { branch, head: "abc1234".to_owned() }
                }
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

    /// The tools are the eight primitives, the two that show the person something and the six
    /// that read and drive their window, and nothing else: the rest is the `slopty` command.
    #[test]
    fn every_tool_has_a_schema_a_description_and_a_hint() {
        let tools = list();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(
            names,
            [
                "machines",
                "agent_start",
                "agent_tell",
                "agent_read",
                "agent_wait",
                "agent_stop",
                "agents",
                "branch_bring",
                "show_visual",
                "show_page",
                "ui_state",
                "ui_terminal",
                "ui_open",
                "ui_region",
                "ui_chat",
                "ui_split",
            ]
        );
        for t in &tools {
            assert_eq!(t.input_schema.get("type"), Some(&json!("object")), "{}", t.name);
            assert!(t.description.as_ref().is_some_and(|d| d.len() > 40), "{}", t.name);
            assert!(t.annotations.as_ref().is_some_and(|a| a.read_only_hint.is_some()));
        }
        let start = get("agent_start").unwrap();
        assert_eq!(start.input_schema["required"], json!(["machine", "prompt"]));
        for name in names {
            assert!(INSTRUCTIONS.contains(name), "the instructions name {name}");
        }
    }

    /// The window's tools reach the person's window through the carrier, each answer the
    /// window's own JSON; a carrier with no window, as the server's endpoint, says so by its
    /// code, and arguments that name nothing are the model's to fix before anything is asked.
    #[tokio::test]
    async fn the_window_tools_reach_the_window_through_their_carrier() {
        use slopty_proto::handoff::{UiAct, UiChange};
        struct Server;
        impl Dispatch for Server {
            fn send(
                &self,
                _key: Option<IdempotencyKey>,
                _verb: Verb,
            ) -> impl Future<Output = Outcome> + Send {
                std::future::ready(Outcome::Done)
            }
        }

        let fake = Fake::default();
        let shown = json!({ "region": "sidebar", "change": "show" });
        let (failed, text) = call_json(&fake, "ui_region", shown).await;
        assert!(!failed, "{text}");
        let answer: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(answer["did"], "showed the sidebar");
        let asked = UiAct::Region { region: "sidebar".to_owned(), change: UiChange::Show };
        assert_eq!(fake.windows.lock().as_slice(), [asked]);
        let (failed, text) = call_json(&fake, "ui_open", json!({})).await;
        let answer: Value = serde_json::from_str(&text).unwrap();
        assert!(failed && answer["error"]["code"] == "unknown", "{text}");
        assert_eq!(fake.windows.lock().len(), 1, "nothing asked of the window");

        let result = call(&Server, "ui_state", Map::new(), None).await.unwrap();
        let text = result.content[0].as_text().unwrap().text.clone();
        let answer: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(answer["error"]["code"], "no_window", "{text}");
    }

    /// What a model is handed is a golden: each tool's name and argument schema. A changed
    /// snapshot is a changed tool surface, read as one.
    #[test]
    fn the_tool_schemas_are_golden() {
        let tools: Vec<Value> = list()
            .iter()
            .map(|t| json!({ "name": t.name, "input_schema": *t.input_schema }))
            .collect();
        // Sorted, so the snapshot does not hang on whether some build unified serde_json's
        // `preserve_order` (CI's per-shard builds and a local one differ).
        insta::with_settings!({ sort_maps => true }, {
            insta::assert_json_snapshot!(tools);
        });
    }

    /// An agent's start names its machine by name and carries no harness arguments; its key
    /// goes with it, and the answer is the agent's id.
    #[tokio::test]
    async fn agent_start_resolves_the_machine_and_carries_its_key() {
        let fake = Fake::default();
        let start = json!({
            "machine": "mac-studio",
            "prompt": "Fix the login",
            "folder": "~/src/app",
            "worktree": "login",
            "agent": "codex",
            "idempotency_key": "start-1",
        });
        let (failed, text) = call_json(&fake, "agent_start", start).await;
        assert!(!failed, "{text}");
        let answer: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(answer["agent"], json!(child().to_string()));
        let Some(Verb::AgentStart(start)) = fake.last() else { panic!() };
        assert_eq!(start.worker, studio());
        assert_eq!(start.agent, Some(AgentId::named(AgentId::CODEX)));
        assert_eq!(start.worktree.as_deref(), Some("login"));
        let key = fake.keys.lock().last().cloned().flatten();
        assert_eq!(key, Some(IdempotencyKey::new("start-1").unwrap()));

        let args = json!({ "machine": "mac-studio", "prompt": "x", "args": ["--yolo"] });
        let (failed, text) = call_json(&fake, "agent_start", args).await;
        assert!(failed && text.contains("unknown field `args`"), "no harness arguments: {text}");
        let nowhere = json!({ "machine": "nowhere", "prompt": "x" });
        let (failed, _) = call_json(&fake, "agent_start", nowhere).await;
        assert!(failed, "a machine that does not resolve");
    }

    /// An agent is read by its id, in the view asked and from the turn asked; a refusal is the
    /// model's to read; an unknown tool is invalid params.
    #[tokio::test]
    async fn an_agent_is_read_by_its_id_and_a_refusal_is_the_model_s_to_read() {
        let fake = Fake::default();
        let read = json!({ "agent": child().to_string(), "view": "activity", "after": 4 });
        let (failed, text) = call_json(&fake, "agent_read", read).await;
        assert!(!failed, "{text}");
        let asked = Verb::ReadThread {
            of: ThreadOf::Thread(child()),
            view: ThreadView::Activity,
            after: Some(TurnId(4)),
        };
        assert_eq!(fake.last(), Some(asked));
        let (failed, text) = call_json(&fake, "agent_read", json!({ "agent": "t1" })).await;
        assert!(failed && text.contains("not an agent's id"), "{text}");

        let stranger = ThreadId::derived(&["stranger"]).to_string();
        let told = json!({ "agent": stranger, "text": "Hello." });
        let (failed, text) = call_json(&fake, "agent_tell", told).await;
        assert!(failed);
        assert_eq!(text, "not of this tree (Forbidden)");

        let err = call(&fake, "task_start", Map::new(), None).await.unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    /// A long wait reports progress every ten seconds, and answers with each agent by its
    /// machine's name.
    #[tokio::test(start_paused = true)]
    async fn a_long_wait_reports_progress_and_names_the_machine() {
        let fake = Fake::default();
        let reported = Mutex::new(Vec::new());
        let report = |waited: Duration| reported.lock().push(waited);
        let wait = json!({ "agents": [child().to_string()], "all": true, "timeout_ms": 30_000 });
        let Value::Object(args) = wait else { panic!() };
        let result = call(&fake, "agent_wait", args, Some(&report)).await.unwrap();
        assert_ne!(result.is_error, Some(true), "{result:?}");
        let every = PROGRESS_EVERY;
        assert_eq!(*reported.lock(), [every, every.saturating_mul(2)], "at 10 s and 20 s of 25");
        let text = result.content[0].as_text().unwrap().text.clone();
        let answer: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(answer["agents"][0]["worker"], json!("mac-studio"));
        assert_eq!(answer["agents"][0]["rests"], json!(true));
        let Some(Verb::AgentWait { all, timeout_ms, .. }) = fake.last() else { panic!() };
        assert_eq!((all, timeout_ms), (true, 30_000), "passed through; the server caps it");
    }

    /// A branch goes from one machine to another by their names.
    #[tokio::test]
    async fn a_branch_is_brought_between_machines() {
        let fake = Fake::default();
        let bring = json!({
            "from": "mac-studio", "repo": "~/src/app", "branch": "worktree-login",
            "to": studio().to_string(),
        });
        let (failed, text) = call_json(&fake, "branch_bring", bring).await;
        assert!(!failed, "{text}");
        assert_eq!(text, r#"{"branch":"worktree-login","head":"abc1234"}"#);
        let Some(Verb::BranchBring { from, to, into, name, .. }) = fake.last() else { panic!() };
        assert_eq!((from, to, into, name), (studio(), studio(), None, None));
        let (failed, text) = call_json(&fake, "agents", json!({})).await;
        assert!(!failed && text.contains("worktree-login"), "{text}");
    }
}
